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
    Ok(snapshot(
        account,
        github_billing_metrics(&items),
        Some(
            "GitHub billing reports use grossQuantity for total AIC consumed and netAmount for actual billed spend. This spend is separate from Copilot quota value and from Power BI cross-ecosystem spend, which is not currently imported."
                .into(),
        ),
    ))
}

fn github_copilot_quota_snapshot(
    account: &ProviderAccount,
    json: &Value,
) -> Option<ProviderSnapshot> {
    let quota = json.get("quota_snapshots")?.get("premium_interactions")?;
    let entitlement = optional_number(quota, &["entitlement"]);
    let remaining = optional_number(quota, &["quota_remaining", "remaining"]);
    let reported_used = optional_number(quota, &["credits_used"]);
    let mut metrics = Vec::new();
    let mut has_allowance = false;

    if let (Some(entitlement), Some(remaining)) = (entitlement, remaining) {
        if entitlement > 0.0 {
            let normalized_remaining = remaining.clamp(0.0, entitlement);
            let used = entitlement - normalized_remaining;
            metrics.push(AllowanceMetric {
                kind: "credits".into(),
                label: "Copilot allowance (AIC)".into(),
                unit: "AIC".into(),
                consumed: used,
                limit: Some(entitlement),
                remaining: Some(normalized_remaining),
            });
            metrics.push(AllowanceMetric {
                kind: "currency".into(),
                label: "Used quota value (USD equivalent)".into(),
                unit: "USD".into(),
                consumed: used / 100.0,
                limit: None,
                remaining: None,
            });
            has_allowance = true;
        }
    }

    if let Some(reported_used) = reported_used {
        metrics.push(AllowanceMetric {
            kind: "credits".into(),
            label: "GitHub-reported credits_used (AIC)".into(),
            unit: "AIC".into(),
            consumed: reported_used,
            limit: None,
            remaining: None,
        });
    }

    if metrics.is_empty() {
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
        metrics,
        message: Some(if has_allowance {
            "Copilot allowance consumption is derived from entitlement minus remaining. One AIC has $0.01 of quota value, but neither AIC nor its USD equivalent is actual billed or cross-ecosystem spend. Power BI spend is not currently imported. GitHub's reported counter is shown separately because it can refresh on a different cadence."
                .into()
        } else {
            "GitHub returned a finite credits_used counter, but the entitlement or remaining field was missing or malformed, so no allowance metric was created. Power BI spend is not currently imported."
                .into()
        }),
    })
}

fn github_billing_metrics(items: &[Value]) -> Vec<AllowanceMetric> {
    let mut metrics = Vec::new();
    if let Some(credits) = sum_numbers(items, &["grossQuantity", "gross_quantity"]) {
        metrics.push(AllowanceMetric {
            kind: "credits".into(),
            label: "AIC consumed".into(),
            unit: "AIC".into(),
            consumed: credits,
            limit: None,
            remaining: None,
        });
    }
    if let Some(cost) = sum_numbers(items, &["netAmount", "net_amount"]) {
        metrics.push(AllowanceMetric {
            kind: "currency".into(),
            label: "Actual billed spend".into(),
            unit: "USD".into(),
            consumed: cost,
            limit: None,
            remaining: None,
        });
    }
    metrics
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
    let cost_url = format!("https://api.anthropic.com/v1/organizations/cost_report?starting_at={start}&ending_at={end}&bucket_width=1d&limit=31");
    let usage_url = format!("https://api.anthropic.com/v1/organizations/usage_report/messages?starting_at={start}&ending_at={end}&bucket_width=1d&limit=31");
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
    let metrics = anthropic_metrics(&cost_json, &usage_json);
    let has_activity = metrics.iter().any(|metric| metric.consumed > 0.0);
    let message = if has_activity {
        "Anthropic reports this month's organization API usage and cost. These values do not include Claude web, Desktop, or personal Pro/Max activity, and the API does not provide a remaining balance."
    } else {
        "Connected successfully, but Anthropic reports no organization API activity for this month. Claude web, Desktop, and personal Pro/Max activity are not included in this API."
    };
    Ok(snapshot(account, metrics, Some(message.into())))
}

fn anthropic_metrics(cost_json: &Value, usage_json: &Value) -> Vec<AllowanceMetric> {
    let cost_usd = recursive_sum(cost_json, &["amount"]) / 100.0;
    let uncached_input = recursive_sum(usage_json, &["uncached_input_tokens"]);
    let cache_creation = recursive_sum(
        usage_json,
        &["ephemeral_1h_input_tokens", "ephemeral_5m_input_tokens"],
    );
    let cache_read = recursive_sum(usage_json, &["cache_read_input_tokens"]);
    let output = recursive_sum(usage_json, &["output_tokens"]);
    let total_tokens = uncached_input + cache_creation + cache_read + output;
    let web_searches = recursive_sum(usage_json, &["web_search_requests"]);

    let mut metrics = vec![
        AllowanceMetric {
            kind: "currency".into(),
            label: "API cost this month".into(),
            unit: "USD".into(),
            consumed: cost_usd,
            limit: None,
            remaining: None,
        },
        AllowanceMetric {
            kind: "tokens".into(),
            label: "Total API tokens".into(),
            unit: "tokens".into(),
            consumed: total_tokens,
            limit: None,
            remaining: None,
        },
        AllowanceMetric {
            kind: "tokens".into(),
            label: "Uncached input".into(),
            unit: "tokens".into(),
            consumed: uncached_input,
            limit: None,
            remaining: None,
        },
        AllowanceMetric {
            kind: "tokens".into(),
            label: "Output".into(),
            unit: "tokens".into(),
            consumed: output,
            limit: None,
            remaining: None,
        },
        AllowanceMetric {
            kind: "tokens".into(),
            label: "Cache creation".into(),
            unit: "tokens".into(),
            consumed: cache_creation,
            limit: None,
            remaining: None,
        },
        AllowanceMetric {
            kind: "tokens".into(),
            label: "Cache reads".into(),
            unit: "tokens".into(),
            consumed: cache_read,
            limit: None,
            remaining: None,
        },
    ];
    if web_searches > 0.0 {
        metrics.push(AllowanceMetric {
            kind: "requests".into(),
            label: "Web searches".into(),
            unit: "requests".into(),
            consumed: web_searches,
            limit: None,
            remaining: None,
        });
    }
    metrics
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

fn optional_number(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(decimal)
}

fn decimal(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|number| number.is_finite())
}

fn sum_numbers(values: &[Value], keys: &[&str]) -> Option<f64> {
    let numbers = values
        .iter()
        .filter_map(|value| optional_number(value, keys))
        .collect::<Vec<_>>();
    (!numbers.is_empty()).then(|| numbers.iter().sum())
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
    fn reads_only_finite_camel_and_snake_case_values() {
        let value: Value =
            serde_json::from_str(r#"{"netAmount":"not-a-number","net_amount":2.5}"#).unwrap();
        assert_eq!(
            optional_number(&value, &["netAmount", "net_amount"]),
            Some(2.5)
        );

        let non_finite: Value = serde_json::from_str(r#"{"credits_used":"NaN"}"#).unwrap();
        assert_eq!(optional_number(&non_finite, &["credits_used"]), None);
    }

    #[test]
    fn maps_anthropic_cost_cents_and_token_breakdown() {
        let cost: Value = serde_json::from_str(
            r#"{"data":[{"results":[{"amount":"123.45","currency":"USD"}]}]}"#,
        )
        .unwrap();
        let usage: Value = serde_json::from_str(
            r#"{"data":[{"results":[{
                "uncached_input_tokens":1500,
                "cache_creation":{"ephemeral_1h_input_tokens":10,"ephemeral_5m_input_tokens":20},
                "cache_read_input_tokens":200,
                "output_tokens":500,
                "server_tool_use":{"web_search_requests":3}
            }]}]}"#,
        )
        .unwrap();

        let metrics = anthropic_metrics(&cost, &usage);
        assert_eq!(metrics[0].consumed, 1.2345);
        assert_eq!(metrics[1].consumed, 2230.0);
        assert_eq!(metrics[2].consumed, 1500.0);
        assert_eq!(metrics[3].consumed, 500.0);
        assert_eq!(metrics[4].consumed, 30.0);
        assert_eq!(metrics[5].consumed, 200.0);
        assert_eq!(metrics[6].consumed, 3.0);
    }

    #[test]
    fn maps_copilot_account_allowance_with_remaining_precedence() {
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
                    "credits_used":501,
                    "quota_remaining":199498.6,
                    "entitlement":200000
                }}
            }"#,
        )
        .unwrap();

        let snapshot = github_copilot_quota_snapshot(&account, &value).expect("quota snapshot");
        assert_eq!(snapshot.reset_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert_eq!(snapshot.metrics[0].label, "Copilot allowance (AIC)");
        assert_eq!(snapshot.metrics[0].unit, "AIC");
        assert!((snapshot.metrics[0].consumed - 501.4).abs() < 1e-9);
        assert_eq!(snapshot.metrics[0].limit, Some(200000.0));
        assert_eq!(snapshot.metrics[0].remaining, Some(199498.6));
        assert_eq!(
            snapshot.metrics[1].label,
            "Used quota value (USD equivalent)"
        );
        assert!((snapshot.metrics[1].consumed - 5.014).abs() < 1e-9);
        assert_eq!(
            snapshot.metrics[2].label,
            "GitHub-reported credits_used (AIC)"
        );
        assert_eq!(snapshot.metrics[2].consumed, 501.0);
    }

    #[test]
    fn omits_incomplete_allowance_but_preserves_a_finite_reported_counter() {
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
                "quota_snapshots":{"premium_interactions":{
                    "credits_used":501,
                    "quota_remaining":"malformed",
                    "entitlement":200000
                }}
            }"#,
        )
        .unwrap();

        let snapshot = github_copilot_quota_snapshot(&account, &value).expect("counter snapshot");
        assert_eq!(snapshot.metrics.len(), 1);
        assert_eq!(
            snapshot.metrics[0].label,
            "GitHub-reported credits_used (AIC)"
        );
        assert_eq!(snapshot.metrics[0].consumed, 501.0);
        assert!(snapshot.metrics[0].limit.is_none());
        assert!(snapshot
            .message
            .as_deref()
            .is_some_and(|message| message.contains("missing or malformed")));
    }

    #[test]
    fn omits_a_non_finite_reported_counter() {
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
                "quota_snapshots":{"premium_interactions":{
                    "credits_used":"NaN",
                    "quota_remaining":199498.6,
                    "entitlement":200000
                }}
            }"#,
        )
        .unwrap();

        let snapshot = github_copilot_quota_snapshot(&account, &value).expect("allowance snapshot");
        assert_eq!(snapshot.metrics.len(), 2);
        assert!(snapshot
            .metrics
            .iter()
            .all(|metric| metric.label != "GitHub-reported credits_used (AIC)"));
    }

    #[test]
    fn maps_gross_aic_and_net_billed_spend_from_billing_reports() {
        let items: Vec<Value> = serde_json::from_str(
            r#"[
                {
                    "grossQuantity":12,
                    "netQuantity":3,
                    "grossAmount":9,
                    "netAmount":2.5
                },
                {
                    "gross_quantity":"8",
                    "net_quantity":1,
                    "net_amount":"1.25"
                }
            ]"#,
        )
        .unwrap();

        let metrics = github_billing_metrics(&items);
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].label, "AIC consumed");
        assert_eq!(metrics[0].unit, "AIC");
        assert_eq!(metrics[0].consumed, 20.0);
        assert_eq!(metrics[1].label, "Actual billed spend");
        assert_eq!(metrics[1].unit, "USD");
        assert_eq!(metrics[1].consumed, 3.75);
    }
}
