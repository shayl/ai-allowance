use chrono::{Datelike, TimeZone, Utc};
use reqwest::{Client, StatusCode};
use serde_json::Value;

use crate::models::{current_month_bounds, AllowanceMetric, ProviderAccount, ProviderSnapshot};

pub async fn refresh(account: &ProviderAccount) -> Result<ProviderSnapshot, String> {
    let mut resolved = account.clone();
    if account.scope_type == "local_env" {
        let variable = match account.provider.as_str() {
            "anthropic" => "ANTHROPIC_API_KEY",
            "openai" => "OPENAI_API_KEY",
            _ => return Err("Unsupported environment-backed provider.".into()),
        };
        resolved.secret = std::env::var(variable)
            .map_err(|_| format!("{variable} is no longer available to the application."))?;
    }
    if resolved.secret.is_empty() && resolved.scope_type != "local_cli" {
        return Err("Credential is missing from the OS vault.".into());
    }
    match resolved.provider.as_str() {
        "github" => github(&resolved).await,
        "anthropic" => anthropic(&resolved).await,
        "openai" => openai(&resolved).await,
        _ => Err("Unsupported provider.".into()),
    }
}

async fn checked_json(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED {
        return Err("Authentication failed. Check the stored credential.".into());
    }
    if status == StatusCode::FORBIDDEN {
        return Err(
            "The credential does not have the required billing or administration permission."
                .into(),
        );
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err("Provider rate limit reached. The previous snapshot remains available.".into());
    }
    if !status.is_success() {
        return Err(format!("Provider returned HTTP {status}."));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Invalid provider response: {error}"))
}

async fn github(account: &ProviderAccount) -> Result<ProviderSnapshot, String> {
    let now = Utc::now();
    if account.scope_type == "local_cli" {
        let output = std::process::Command::new("gh")
            .args([
                "api",
                "--method",
                "GET",
                "-H",
                "Accept: application/json",
                "/copilot_internal/user",
            ])
            .output()
            .map_err(|_| "GitHub CLI is no longer available.".to_string())?;
        if output.status.success() {
            let json: Value = serde_json::from_slice(&output.stdout)
                .map_err(|error| format!("Invalid Copilot quota response: {error}"))?;
            if let Some(snapshot) = github_copilot_quota_snapshot(account, &json) {
                return Ok(snapshot);
            }
        }
    }

    let scope = if account.scope_type == "organization" {
        format!(
            "organizations/{}/settings/billing/ai_credit/usage",
            account.scope
        )
    } else {
        if account.scope.is_empty() {
            return Err("GitHub personal reporting requires the account username.".into());
        }
        format!("users/{}/settings/billing/ai_credit/usage", account.scope)
    };
    let api_path = format!("/{scope}?year={}&month={}", now.year(), now.month());
    let json = if account.scope_type == "local_cli" {
        let output = std::process::Command::new("gh")
            .args([
                "api",
                "--method",
                "GET",
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "X-GitHub-Api-Version: 2026-03-10",
                &api_path,
            ])
            .output()
            .map_err(|_| "GitHub CLI is no longer available.".to_string())?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("needs the \"user\" scope") {
                return Err(format!(
                    "Connected as {}. The GitHub CLI token needs the `user` scope before it can read AI-credit usage. Run: gh auth refresh -h github.com -s user",
                    account.scope
                ));
            }
            return Err(format!(
                "Connected as {} through GitHub CLI, but GitHub did not return an AI-credit usage report for this account or plan.",
                account.scope
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("Invalid GitHub CLI response: {error}"))?
    } else {
        let url = format!("https://api.github.com{api_path}");
        checked_json(
            Client::new()
                .get(url)
                .header("Accept", "application/vnd.github+json")
                .header("Authorization", format!("Bearer {}", account.secret))
                .header("X-GitHub-Api-Version", "2026-03-10")
                .header("User-Agent", "ai-allowance")
                .send()
                .await
                .map_err(|error| error.to_string())?,
        )
        .await?
    };
    let items = json
        .get("usageItems")
        .or_else(|| json.get("usage_items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let credits = items
        .iter()
        .map(|item| number(item, &["netQuantity", "net_quantity", "quantity"]))
        .sum::<f64>();
    let cost = items
        .iter()
        .map(|item| {
            number(
                item,
                &["netAmount", "net_amount", "grossAmount", "gross_amount"],
            )
        })
        .sum::<f64>();
    Ok(snapshot(account, vec![
        AllowanceMetric { kind: "credits".into(), label: "AI credits used".into(), unit: "credits".into(), consumed: credits, limit: None, remaining: None },
        AllowanceMetric { kind: "currency".into(), label: "Net spend".into(), unit: "USD".into(), consumed: cost, limit: None, remaining: None },
    ], Some("GitHub's reporting API returns authoritative consumption; a remaining balance is shown only when the API provides a limit.".into())))
}

fn github_copilot_quota_snapshot(
    account: &ProviderAccount,
    json: &Value,
) -> Option<ProviderSnapshot> {
    let quota = json.get("quota_snapshots")?.get("premium_interactions")?;
    let entitlement = number(quota, &["entitlement"]);
    let remaining = number(quota, &["quota_remaining", "remaining"]);
    if entitlement <= 0.0 {
        return None;
    }

    let updated_at = quota
        .get("timestamp_utc")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let reset_at = json
        .get("quota_reset_date_utc")
        .or_else(|| json.get("quota_reset_date"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let plan = json
        .get("copilot_plan")
        .and_then(Value::as_str)
        .unwrap_or("Copilot");

    Some(ProviderSnapshot {
        account_id: account.id.clone(),
        provider: account.provider.clone(),
        label: account.label.clone(),
        scope: format!("{} · {} plan", account.scope, plan),
        freshness: "fresh".into(),
        updated_at,
        reset_at,
        metrics: vec![
            AllowanceMetric {
                kind: "currency".into(),
                label: "Microsoft allowance".into(),
                unit: "USD".into(),
                consumed: number(quota, &["credits_used"]) / 100.0,
                limit: Some(entitlement / 100.0),
                remaining: Some(remaining / 100.0),
            },
            AllowanceMetric {
                kind: "credits".into(),
                label: "GitHub credits reported used".into(),
                unit: "AIC".into(),
                consumed: number(quota, &["credits_used"]),
                limit: None,
                remaining: None,
            },
        ],
        message: Some(
            "Direct Copilot account quota. GitHub defines 1 AI credit as $0.01 USD. The used and remaining counters can refresh on different cadences."
                .into(),
        ),
    })
}

async fn anthropic(account: &ProviderAccount) -> Result<ProviderSnapshot, String> {
    if account.scope_type == "personal" {
        return Ok(unsupported(
            account,
            "Claude Pro/Max allowance is not exposed through a documented third-party API.",
        ));
    }
    let (start, end) = current_month_bounds();
    let client = Client::new();
    let cost_url = format!("https://api.anthropic.com/v1/organizations/cost_report?starting_at={start}&ending_at={end}&bucket_width=1d");
    let usage_url = format!("https://api.anthropic.com/v1/organizations/usage_report/messages?starting_at={start}&ending_at={end}&bucket_width=1d");
    let request = |url: String| {
        client
            .get(url)
            .header("x-api-key", &account.secret)
            .header("anthropic-version", "2023-06-01")
    };
    let (cost_json, usage_json) = tokio::try_join!(
        async {
            checked_json(
                request(cost_url)
                    .send()
                    .await
                    .map_err(|error| error.to_string())?,
            )
            .await
        },
        async {
            checked_json(
                request(usage_url)
                    .send()
                    .await
                    .map_err(|error| error.to_string())?,
            )
            .await
        }
    )?;
    let cost = recursive_sum(&cost_json, &["amount", "cost"]);
    let tokens = recursive_sum(
        &usage_json,
        &[
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ],
    );
    Ok(snapshot(account, vec![
        AllowanceMetric { kind: "currency".into(), label: "Current period".into(), unit: "USD".into(), consumed: cost, limit: None, remaining: None },
        AllowanceMetric { kind: "tokens".into(), label: "Tokens used".into(), unit: "tokens".into(), consumed: tokens, limit: None, remaining: None },
    ], Some("Anthropic Admin APIs report organization usage and cost, but do not imply a prepaid balance.".into())))
}

async fn openai(account: &ProviderAccount) -> Result<ProviderSnapshot, String> {
    if account.scope_type == "personal" {
        return Ok(unsupported(
            account,
            "Personal ChatGPT/Codex allowance is not exposed through a documented third-party API.",
        ));
    }
    let now = Utc::now();
    let start = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap()
        .timestamp();
    let client = Client::new();
    let costs = checked_json(
        client
            .get(format!(
                "https://api.openai.com/v1/organization/costs?start_time={start}&limit=31"
            ))
            .bearer_auth(&account.secret)
            .send()
            .await
            .map_err(|error| error.to_string())?,
    )
    .await?;
    let usage = checked_json(client.get(format!("https://api.openai.com/v1/organization/usage/completions?start_time={start}&bucket_width=1d&limit=31"))
        .bearer_auth(&account.secret).send().await.map_err(|error| error.to_string())?).await?;
    let cost = recursive_sum(&costs, &["value"]);
    let tokens = recursive_sum(
        &usage,
        &["input_tokens", "output_tokens", "input_cached_tokens"],
    );
    Ok(snapshot(account, vec![
        AllowanceMetric { kind: "currency".into(), label: "Current period".into(), unit: "USD".into(), consumed: cost, limit: None, remaining: None },
        AllowanceMetric { kind: "tokens".into(), label: "Tokens used".into(), unit: "tokens".into(), consumed: tokens, limit: None, remaining: None },
    ], Some("OpenAI organization APIs report API usage and costs separately from personal Codex plan limits.".into())))
}

fn snapshot(
    account: &ProviderAccount,
    metrics: Vec<AllowanceMetric>,
    message: Option<String>,
) -> ProviderSnapshot {
    ProviderSnapshot {
        account_id: account.id.clone(),
        provider: account.provider.clone(),
        label: account.label.clone(),
        scope: account.display_scope(),
        freshness: "fresh".into(),
        updated_at: Utc::now().to_rfc3339(),
        reset_at: None,
        metrics,
        message,
    }
}

fn unsupported(account: &ProviderAccount, message: &str) -> ProviderSnapshot {
    let mut result = snapshot(account, Vec::new(), Some(message.into()));
    result.freshness = "unsupported".into();
    result
}

fn number(value: &Value, keys: &[&str]) -> f64 {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(decimal)
        .unwrap_or(0.0)
}

fn decimal(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}

fn recursive_sum(value: &Value, keys: &[&str]) -> f64 {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| {
                let direct = if keys.contains(&key.as_str()) {
                    decimal(value).unwrap_or(0.0)
                } else {
                    0.0
                };
                direct + recursive_sum(value, keys)
            })
            .sum(),
        Value::Array(values) => values.iter().map(|value| recursive_sum(value, keys)).sum(),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_nested_provider_values() {
        let value: Value = serde_json::from_str(r#"{"data":[{"results":[{"input_tokens":10,"output_tokens":5},{"input_tokens":"3"}]}]}"#).unwrap();
        assert_eq!(
            recursive_sum(&value, &["input_tokens", "output_tokens"]),
            18.0
        );
    }

    #[test]
    fn reads_camel_and_snake_case_github_values() {
        let value: Value = serde_json::from_str(r#"{"netAmount":2.5,"net_quantity":12}"#).unwrap();
        assert_eq!(number(&value, &["netAmount", "net_amount"]), 2.5);
        assert_eq!(number(&value, &["netQuantity", "net_quantity"]), 12.0);
    }

    #[test]
    fn maps_copilot_account_allowance() {
        let account = ProviderAccount {
            id: "github-local-test".into(),
            provider: "github".into(),
            label: "GitHub Copilot".into(),
            scope_type: "local_cli".into(),
            scope: "developer".into(),
            secret: String::new(),
        };
        let value: Value = serde_json::from_str(
            r#"{
                "copilot_plan":"enterprise",
                "quota_reset_date_utc":"2026-10-01T00:00:00Z",
                "quota_snapshots":{"premium_interactions":{
                    "timestamp_utc":"2026-09-15T23:27:34.559Z",
                    "credits_used":9510,
                    "quota_remaining":190509.9,
                    "entitlement":200000
                }}
            }"#,
        )
        .unwrap();

        let snapshot = github_copilot_quota_snapshot(&account, &value).expect("quota snapshot");
        assert_eq!(snapshot.reset_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert_eq!(snapshot.metrics[0].consumed, 95.1);
        assert_eq!(snapshot.metrics[0].limit, Some(2000.0));
        assert_eq!(snapshot.metrics[0].remaining, Some(1905.099));
        assert_eq!(snapshot.metrics[1].consumed, 9510.0);
    }
}
