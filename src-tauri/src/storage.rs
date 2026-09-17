use std::{fs, path::PathBuf};

use chrono::{DateTime, Datelike, Utc};
use rusqlite::{params, Connection, OpenFlags};

use crate::models::{AllowanceMetric, Dashboard, ProviderAccount, ProviderSnapshot};

pub struct Storage {
    connection: Connection,
}

impl Storage {
    pub fn new(directory: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        fs::create_dir_all(&directory)?;
        let connection = Connection::open(directory.join("ai-allowance.db"))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS provider_accounts (
               id TEXT PRIMARY KEY,
               provider TEXT NOT NULL,
               label TEXT NOT NULL,
               scope_type TEXT NOT NULL,
               scope TEXT NOT NULL,
               enabled INTEGER NOT NULL DEFAULT 1,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS snapshots (
               account_id TEXT PRIMARY KEY,
               payload TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               FOREIGN KEY(account_id) REFERENCES provider_accounts(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS refresh_runs (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               account_id TEXT NOT NULL,
               status TEXT NOT NULL,
               message TEXT,
               created_at TEXT NOT NULL
             );",
        )?;
        Ok(Self { connection })
    }

    pub fn save_account(&self, account: &ProviderAccount) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        self.connection.execute(
            "INSERT INTO provider_accounts(id, provider, label, scope_type, scope, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(id) DO UPDATE SET label=excluded.label, scope=excluded.scope, updated_at=excluded.updated_at, enabled=1",
            params![account.id, account.provider, account.label, account.scope_type, account.scope, now],
        ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn accounts(&self) -> Result<Vec<ProviderAccount>, String> {
        let mut statement = self.connection.prepare(
            "SELECT id, provider, label, scope_type, scope FROM provider_accounts WHERE enabled=1 ORDER BY provider, label"
        ).map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let secret = keyring::Entry::new("ai-allowance", &id)
                    .ok()
                    .and_then(|entry| entry.get_password().ok())
                    .unwrap_or_default();
                Ok(ProviderAccount {
                    id,
                    provider: row.get(1)?,
                    label: row.get(2)?,
                    scope_type: row.get(3)?,
                    scope: row.get(4)?,
                    secret,
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    pub fn delete_account(&self, account_id: &str) -> Result<(), String> {
        self.connection
            .execute("DELETE FROM snapshots WHERE account_id=?1", [account_id])
            .map_err(|error| error.to_string())?;
        self.connection
            .execute("DELETE FROM provider_accounts WHERE id=?1", [account_id])
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn save_snapshots(&self, snapshots: &[ProviderSnapshot]) -> Result<(), String> {
        for snapshot in snapshots {
            let payload = serde_json::to_string(snapshot).map_err(|error| error.to_string())?;
            self.connection.execute(
                "INSERT INTO snapshots(account_id, payload, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(account_id) DO UPDATE SET payload=excluded.payload, updated_at=excluded.updated_at",
                params![snapshot.account_id, payload, snapshot.updated_at],
            ).map_err(|error| error.to_string())?;
            self.connection.execute(
                "INSERT INTO refresh_runs(account_id, status, message, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![snapshot.account_id, snapshot.freshness, snapshot.message, Utc::now().to_rfc3339()],
            ).map_err(|error| error.to_string())?;
        }
        self.connection
            .execute(
                "DELETE FROM refresh_runs WHERE created_at < datetime('now', '-365 days')",
                [],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn dashboard(&self) -> Result<Dashboard, String> {
        let mut statement = self
            .connection
            .prepare("SELECT payload FROM snapshots ORDER BY updated_at DESC")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let mut providers: Vec<ProviderSnapshot> = rows
            .filter_map(Result::ok)
            .filter_map(|payload| serde_json::from_str::<ProviderSnapshot>(&payload).ok())
            .filter(|provider| {
                !(provider.account_id.starts_with("github-local-")
                    && !provider.metrics.is_empty()
                    && provider
                        .metrics
                        .iter()
                        .all(|metric| metric.consumed.abs() < f64::EPSILON))
            })
            .collect();
        if let Some(local) = local_copilot_snapshot() {
            providers.retain(|provider| provider.account_id != local.account_id);
            providers.insert(0, local);
        }
        Ok(Dashboard {
            generated_at: Utc::now().to_rfc3339(),
            providers,
        })
    }
}

fn local_copilot_snapshot() -> Option<ProviderSnapshot> {
    let user_profile = std::env::var_os("USERPROFILE")?;
    let database_path = PathBuf::from(user_profile)
        .join(".copilot")
        .join("session-store.db");
    if !database_path.exists() {
        return None;
    }
    let connection = Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    local_copilot_snapshot_from_connection(&connection, Utc::now())
}

struct LocalCopilotUsage {
    session_count: i64,
    aic: f64,
    input: f64,
    output: f64,
    cached: f64,
    updated_at: String,
}

fn copilot_month_bounds(now: DateTime<Utc>) -> (String, String) {
    let start = format!("{:04}-{:02}-01T00:00:00Z", now.year(), now.month());
    let (next_year, next_month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    let end = format!("{next_year:04}-{next_month:02}-01T00:00:00Z");
    (start, end)
}

fn query_local_copilot_usage(
    connection: &Connection,
    period_start: &str,
    period_end: &str,
) -> Option<LocalCopilotUsage> {
    let (event_count, session_count, aic, input, output, cached, updated_at): (
        i64,
        i64,
        f64,
        f64,
        f64,
        f64,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT
               COUNT(*),
               COUNT(DISTINCT session_id),
               TOTAL(total_nano_aiu) / 1000000000.0,
               TOTAL(input_tokens),
               TOTAL(output_tokens),
               TOTAL(cache_read_tokens),
               strftime('%Y-%m-%dT%H:%M:%fZ', MAX(julianday(created_at)))
             FROM assistant_usage_events
             WHERE julianday(created_at) >= julianday(?1)
               AND julianday(created_at) < julianday(?2)",
            params![period_start, period_end],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .ok()?;
    if event_count == 0 {
        return None;
    }
    Some(LocalCopilotUsage {
        session_count,
        aic,
        input,
        output,
        cached,
        updated_at: updated_at?,
    })
}

fn local_copilot_snapshot_from_connection(
    connection: &Connection,
    now: DateTime<Utc>,
) -> Option<ProviderSnapshot> {
    let (period_start, period_end) = copilot_month_bounds(now);
    let usage = query_local_copilot_usage(connection, &period_start, &period_end)?;
    Some(ProviderSnapshot {
        account_id: "copilot-cli-local-usage".into(),
        provider: "github".into(),
        label: "Copilot CLI · local usage".into(),
        scope: format!(
            "All local Copilot CLI sessions · {} (UTC)",
            now.format("%B %Y")
        ),
        freshness: "fresh".into(),
        updated_at: usage.updated_at,
        reset_at: Some(period_end),
        metrics: vec![
            AllowanceMetric {
                kind: "credits".into(),
                label: "AI credits used".into(),
                unit: "AIC".into(),
                consumed: usage.aic,
                limit: None,
                remaining: None,
            },
            AllowanceMetric {
                kind: "tokens".into(),
                label: "Input tokens".into(),
                unit: "tokens".into(),
                consumed: usage.input,
                limit: None,
                remaining: None,
            },
            AllowanceMetric {
                kind: "tokens".into(),
                label: "Output tokens".into(),
                unit: "tokens".into(),
                consumed: usage.output,
                limit: None,
                remaining: None,
            },
            AllowanceMetric {
                kind: "tokens".into(),
                label: "Cached input".into(),
                unit: "tokens".into(),
                consumed: usage.cached,
                limit: None,
                remaining: None,
            },
        ],
        message: Some(format!(
            "Local Copilot CLI telemetry summed across {} session{} for the current UTC month. This is not the authoritative GitHub account allowance.",
            usage.session_count,
            if usage.session_count == 1 { "" } else { "s" }
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn usage_connection() -> Connection {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE assistant_usage_events (
                   session_id TEXT NOT NULL,
                   total_nano_aiu INTEGER,
                   input_tokens INTEGER,
                   output_tokens INTEGER,
                   cache_read_tokens INTEGER,
                   created_at TEXT
                 );",
            )
            .expect("usage table");
        connection
    }

    #[test]
    fn aggregates_all_sessions_in_current_utc_month() {
        let connection = usage_connection();
        let rows = [
            (
                "old",
                9_000_000_000_i64,
                900_i64,
                900_i64,
                900_i64,
                "2026-08-31T23:59:59Z",
            ),
            ("one", 1_500_000_000, 10, 20, 30, "2026-09-01 00:00:00"),
            ("two", 2_250_000_000, 100, 200, 300, "2026-09-15T12:30:00Z"),
            (
                "future",
                8_000_000_000,
                800,
                800,
                800,
                "2026-10-01T00:00:00Z",
            ),
        ];
        for row in rows {
            connection
                .execute(
                    "INSERT INTO assistant_usage_events
                     (session_id, total_nano_aiu, input_tokens, output_tokens, cache_read_tokens, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![row.0, row.1, row.2, row.3, row.4, row.5],
                )
                .expect("usage event");
        }

        let usage =
            query_local_copilot_usage(&connection, "2026-09-01T00:00:00Z", "2026-10-01T00:00:00Z")
                .expect("monthly usage");

        assert_eq!(usage.session_count, 2);
        assert!((usage.aic - 3.75).abs() < f64::EPSILON);
        assert_eq!(usage.input, 110.0);
        assert_eq!(usage.output, 220.0);
        assert_eq!(usage.cached, 330.0);
        assert_eq!(usage.updated_at, "2026-09-15T12:30:00.000Z");
    }

    #[test]
    fn maps_local_usage_to_an_unlimited_telemetry_card() {
        let connection = usage_connection();
        connection
            .execute(
                "INSERT INTO assistant_usage_events
                 (session_id, total_nano_aiu, input_tokens, output_tokens, cache_read_tokens, created_at)
                 VALUES ('one', 1250000000, 10, 20, 30, '2026-09-15T12:30:00Z')",
                [],
            )
            .expect("usage event");
        let now = Utc
            .with_ymd_and_hms(2026, 9, 15, 17, 0, 0)
            .single()
            .expect("valid date");

        let snapshot =
            local_copilot_snapshot_from_connection(&connection, now).expect("local snapshot");

        assert_eq!(snapshot.account_id, "copilot-cli-local-usage");
        assert_eq!(snapshot.label, "Copilot CLI · local usage");
        assert_eq!(
            snapshot.scope,
            "All local Copilot CLI sessions · September 2026 (UTC)"
        );
        assert_eq!(snapshot.reset_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert!(snapshot.message.as_deref().is_some_and(
            |message| message.contains("not the authoritative GitHub account allowance")
        ));
        assert!(snapshot
            .metrics
            .iter()
            .all(|metric| metric.limit.is_none() && metric.remaining.is_none()));
    }

    #[test]
    fn returns_none_for_missing_schema_or_empty_period() {
        let missing_table = Connection::open_in_memory().expect("in-memory database");
        assert!(query_local_copilot_usage(
            &missing_table,
            "2026-09-01T00:00:00Z",
            "2026-10-01T00:00:00Z"
        )
        .is_none());

        let empty_period = usage_connection();
        assert!(query_local_copilot_usage(
            &empty_period,
            "2026-09-01T00:00:00Z",
            "2026-10-01T00:00:00Z"
        )
        .is_none());
    }

    #[test]
    fn month_bounds_roll_over_at_year_end() {
        let now = Utc
            .with_ymd_and_hms(2026, 12, 31, 23, 59, 59)
            .single()
            .expect("valid date");

        assert_eq!(
            copilot_month_bounds(now),
            ("2026-12-01T00:00:00Z".into(), "2027-01-01T00:00:00Z".into())
        );
    }
}
