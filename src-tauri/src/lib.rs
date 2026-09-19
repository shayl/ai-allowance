mod models;
mod providers;
mod storage;

use std::sync::Mutex;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State,
};

use models::{AccountInput, Dashboard, LocalAccountCandidate, ProviderAccount};
use storage::Storage;

struct AppState {
    storage: Mutex<Storage>,
}

#[tauri::command]
fn get_dashboard(state: State<'_, AppState>) -> Result<Dashboard, String> {
    state
        .storage
        .lock()
        .map_err(|_| "Storage lock failed".to_string())?
        .dashboard()
}

#[tauri::command]
async fn refresh_dashboard(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Dashboard, String> {
    let accounts = state
        .storage
        .lock()
        .map_err(|_| "Storage lock failed".to_string())?
        .accounts()?;
    let mut snapshots = Vec::new();
    for account in accounts {
        match providers::refresh(&account).await {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(message) => snapshots.push(models::ProviderSnapshot::error(&account, message)),
        }
    }
    let storage = state
        .storage
        .lock()
        .map_err(|_| "Storage lock failed".to_string())?;
    storage.save_snapshots(&snapshots)?;
    let dashboard = storage.dashboard()?;
    if let Some(window) = app.get_webview_window("widget") {
        let _ = window.emit("dashboard-updated", &dashboard);
    }
    Ok(dashboard)
}

#[tauri::command]
fn save_account(input: AccountInput, state: State<'_, AppState>) -> Result<(), String> {
    if input.secret.trim().is_empty() {
        return Err("A provider token or admin API key is required.".to_string());
    }
    let account = input.into_account();
    let credential =
        keyring::Entry::new("ai-allowance", &account.id).map_err(|error| error.to_string())?;
    credential
        .set_password(&account.secret)
        .map_err(|error| format!("Credential vault error: {error}"))?;
    state
        .storage
        .lock()
        .map_err(|_| "Storage lock failed".to_string())?
        .save_account(&account)
}

#[tauri::command]
fn delete_account(account_id: String, state: State<'_, AppState>) -> Result<(), String> {
    if let Ok(entry) = keyring::Entry::new("ai-allowance", &account_id) {
        let _ = entry.delete_credential();
    }
    state
        .storage
        .lock()
        .map_err(|_| "Storage lock failed".to_string())?
        .delete_account(&account_id)
}

#[tauri::command]
fn discover_local_accounts() -> Vec<LocalAccountCandidate> {
    let mut candidates = Vec::new();
    if let Ok(login) = models::github_cli_login() {
        candidates.push(LocalAccountCandidate {
            id: format!("github-local-{login}"),
            provider: "github".into(),
            label: "GitHub Copilot".into(),
            account: login,
            source: "GitHub CLI".into(),
            can_connect: true,
            connected: false,
            message: "Found an authenticated GitHub CLI account. Review it and choose Connect to use it; the OAuth token is never copied into AI Allowance.".into(),
        });
    }

    for (variable, provider, label) in [
        ("ANTHROPIC_API_KEY", "anthropic", "Anthropic API"),
        ("OPENAI_API_KEY", "openai", "OpenAI API"),
    ] {
        if std::env::var_os(variable).is_some() {
            candidates.push(LocalAccountCandidate {
                id: format!("{provider}-env"),
                provider: provider.into(),
                label: label.into(),
                account: variable.into(),
                source: "Environment variable".into(),
                can_connect: true,
                connected: false,
                message: format!("Can use {variable} directly from the app environment after you choose Connect; the key is not copied into SQLite or the credential vault."),
            });
        }
    }

    if std::process::Command::new("codex")
        .arg("--version")
        .output()
        .is_ok()
    {
        candidates.push(LocalAccountCandidate {
            id: "openai-local".into(),
            provider: "openai".into(),
            label: "Codex CLI".into(),
            account: "Local CLI installation".into(),
            source: "Codex CLI".into(),
            can_connect: false,
            connected: false,
            message: "Installed, but its local consumer login is not imported because no documented third-party allowance API is available.".into(),
        });
    }
    candidates
}

fn local_account_scope_type(candidate: &LocalAccountCandidate) -> &'static str {
    if candidate.source.ends_with("CLI") {
        "local_cli"
    } else {
        "local_env"
    }
}

#[tauri::command]
fn connect_local_account(candidate_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let candidate = discover_local_accounts()
        .into_iter()
        .find(|candidate| candidate.id == candidate_id && candidate.can_connect)
        .ok_or_else(|| "The discovered account changed. Run discovery again.".to_string())?;
    let scope_type = local_account_scope_type(&candidate).to_string();
    state
        .storage
        .lock()
        .map_err(|_| "Storage lock failed".to_string())?
        .save_account(&ProviderAccount {
            id: candidate.id,
            provider: candidate.provider,
            label: candidate.label,
            scope_type,
            scope: candidate.account,
            secret: String::new(),
        })
}

#[tauri::command]
fn set_widget_visible(app: AppHandle, visible: bool) -> Result<(), String> {
    let window = ensure_widget(&app)?;
    if visible {
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
    } else {
        window.hide().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn ensure_widget(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    app.get_webview_window("widget")
        .ok_or_else(|| "The widget window was not initialized.".to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let storage = Storage::new(app.path().app_data_dir()?)?;
            app.manage(AppState {
                storage: Mutex::new(storage),
            });

            let show = MenuItem::with_id(app, "show", "Show AI Allowance", true, None::<&str>)?;
            let widget =
                MenuItem::with_id(app, "widget", "Toggle desktop widget", true, None::<&str>)?;
            let refresh = MenuItem::with_id(app, "refresh", "Refresh", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &widget, &refresh, &quit])?;

            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .expect("application icon"),
                )
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("AI Allowance")
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        ..
                    } = event
                    {
                        if let Some(window) = tray.app_handle().get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "widget" => {
                        if let Ok(window) = ensure_widget(app) {
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                            }
                        }
                    }
                    "refresh" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.emit("refresh-requested", ());
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" || window.label() == "widget" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_dashboard,
            refresh_dashboard,
            save_account,
            delete_account,
            discover_local_accounts,
            connect_local_account,
            set_widget_visible
        ])
        .run(tauri::generate_context!())
        .expect("error while running AI Allowance");
}
