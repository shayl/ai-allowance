use chrono::{Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccount {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub scope_type: String,
    pub scope: String,
    #[serde(skip_serializing)]
    pub secret: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInput {
    pub provider: String,
    pub label: String,
    pub scope_type: String,
    pub scope: String,
    pub secret: String,
}

impl AccountInput {
    pub fn into_account(self) -> ProviderAccount {
        let scope = self.scope.trim().to_string();
        let id = format!(
            "{}-{}-{}",
            self.provider,
            self.scope_type,
            if scope.is_empty() { "default" } else { &scope }
        )
        .replace(
            |character: char| !character.is_ascii_alphanumeric() && character != '-',
            "-",
        );
        ProviderAccount {
            id,
            provider: self.provider,
            label: self.label,
            scope_type: self.scope_type,
            scope,
            secret: self.secret,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowanceMetric {
    pub kind: String,
    pub label: String,
    pub unit: String,
    pub consumed: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSnapshot {
    pub account_id: String,
    pub provider: String,
    pub label: String,
    pub scope: String,
    pub freshness: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
    pub metrics: Vec<AllowanceMetric>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ProviderSnapshot {
    pub fn error(account: &ProviderAccount, message: String) -> Self {
        Self {
            account_id: account.id.clone(),
            provider: account.provider.clone(),
            label: account.label.clone(),
            scope: account.display_scope(),
            freshness: "error".into(),
            updated_at: Utc::now().to_rfc3339(),
            reset_at: None,
            metrics: Vec::new(),
            message: Some(message),
        }
    }
}

impl ProviderAccount {
    pub fn display_scope(&self) -> String {
        if self.scope.is_empty() {
            if self.scope_type == "personal" {
                "Personal account".into()
            } else if self.scope_type == "local_env" {
                "Local environment".into()
            } else {
                "Organization".into()
            }
        } else {
            self.scope.clone()
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAccountCandidate {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub account: String,
    pub source: String,
    pub can_connect: bool,
    pub connected: bool,
    pub message: String,
}

pub fn github_cli_login() -> Result<String, String> {
    let output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .map_err(|_| "GitHub CLI is not installed or not available on PATH.".to_string())?;
    if !output.status.success() {
        return Err("GitHub CLI is installed but not authenticated.".to_string());
    }
    let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if login.is_empty() {
        Err("GitHub CLI did not return an authenticated account.".to_string())
    } else {
        Ok(login)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub generated_at: String,
    pub providers: Vec<ProviderSnapshot>,
}

pub fn current_month_bounds() -> (String, String) {
    let now = Utc::now();
    let start = format!("{:04}-{:02}-01T00:00:00Z", now.year(), now.month());
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    let end = format!("{year:04}-{month:02}-01T00:00:00Z");
    (start, end)
}
