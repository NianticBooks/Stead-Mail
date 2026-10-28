//! Tauri desktop shell. Mail logic lives in `mailcore`; the single `rpc` command passes the same
//! JSON-RPC methods the CLI daemon serves straight through, so the UI and every other client see
//! one API. Only what needs the desktop shell is handled here: opening the browser for sign-in,
//! opening links and files, and background IDLE watchers that push live updates to the window.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mailcore::api::SyncReport;
use mailcore::rpc::{self, Request};
use mailcore::sync::SyncOptions;
use mailcore::Api;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

struct Shared {
    api: Arc<Api>,
    watchers: Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>,
}

#[derive(Serialize)]
struct Diag {
    os: &'static str,
    session_type: Option<String>,
    wayland_display: Option<String>,
    hyprland: bool,
    nv_disable_explicit_sync: Option<String>,
    core: Option<mailcore::api::PingInfo>,
    core_error: Option<String>,
}

#[tauri::command]
fn diag(state: State<'_, Shared>) -> Diag {
    let (core, core_error) = match state.api.ping() {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Diag {
        os: std::env::consts::OS,
        session_type: std::env::var("XDG_SESSION_TYPE").ok(),
        wayland_display: std::env::var("WAYLAND_DISPLAY").ok(),
        hyprland: std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok(),
        nv_disable_explicit_sync: std::env::var("__NV_DISABLE_EXPLICIT_SYNC").ok(),
        core,
        core_error,
    }
}

/// Any JSON-RPC method of the core. The error is the core's message, ready to show to the user.
#[tauri::command]
async fn rpc(state: State<'_, Shared>, method: String, params: Option<Value>) -> Result<Value, String> {
    let req = Request { jsonrpc: Some("2.0".into()), id: Some(Value::from(1)), method, params: params.unwrap_or(Value::Null) };
    let resp = rpc::dispatch(&state.api, req).await;
    match (resp.result, resp.error) {
        (Some(v), _) => Ok(v),
        (None, Some(e)) => Err(e.message),
        (None, None) => Ok(Value::Null),
    }
}

/// Sign in to Google: the browser opens on the consent page and the core waits for the redirect.
#[tauri::command]
async fn google_login(app: AppHandle, state: State<'_, Shared>) -> Result<Value, String> {
    let opener = app.clone();
    let identity = state
        .api
        .google_login(move |url| {
            let _ = opener.opener().open_url(url, None::<&str>);
        })
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(identity).map_err(|e| e.to_string())
}

/// Open a link from a message in the user's browser or mail handler. Only plain web, mail and
/// phone links are allowed; anything else from untrusted mail is refused.
#[tauri::command]
fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    let lower = url.trim().to_ascii_lowercase();
    if !["https://", "http://", "mailto:", "tel:"].iter().any(|p| lower.starts_with(p)) {
        return Err("only http, https, mailto and tel links can be opened".into());
    }
    app.opener().open_url(url.trim(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_file(app: AppHandle, path: String) -> Result<(), String> {
    app.opener().reveal_item_in_dir(path).map_err(|e| e.to_string())
}

#[tauri::command]
fn download_dir(app: AppHandle) -> Result<String, String> {
    app.path().download_dir().map(|p| p.display().to_string()).map_err(|e| e.to_string())
}

#[derive(Serialize, Clone)]
struct WatchError {
    account: String,
    error: String,
}

/// One IDLE watcher per account on its inbox. It reconnects by itself; the window hears about
/// every pass through `mail://synced` and about fatal problems (a rejected login) through
/// `mail://watch-error`.
fn spawn_watcher(app: AppHandle, api: Arc<Api>, account_id: String) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let emitter = app.clone();
        let result = api
            .watch(
                &account_id,
                "INBOX",
                SyncOptions { initial_limit: 50, backfill: 0 },
                Duration::from_secs(25 * 60),
                move |report: &SyncReport| {
                    let _ = emitter.emit("mail://synced", report);
                },
                None,
            )
            .await;
        if let Err(e) = result {
            let _ = app.emit("mail://watch-error", WatchError { account: account_id, error: e.to_string() });
        }
    })
}

fn sync_watchers(app: &AppHandle, shared: &Shared) {
    let accounts = shared.api.list_accounts().unwrap_or_default();
    let mut map = shared.watchers.lock().unwrap_or_else(|p| p.into_inner());
    // Stop watchers of accounts that no longer exist or whose task has ended.
    map.retain(|id, h| {
        let keep = accounts.iter().any(|a| &a.id == id) && !h.inner().is_finished();
        if !keep {
            h.abort();
        }
        keep
    });
    for a in accounts {
        map.entry(a.id.clone()).or_insert_with(|| spawn_watcher(app.clone(), shared.api.clone(), a.id.clone()));
    }
}

/// Called by the UI after accounts are added or removed.
#[tauri::command]
fn restart_watchers(app: AppHandle, state: State<'_, Shared>) {
    sync_watchers(&app, &state);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let api = Arc::new(match Api::open_default() {
        Ok(api) => api,
        Err(e) => {
            eprintln!("mailcore: failed to open core: {e}");
            std::process::exit(2);
        }
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Shared { api: api.clone(), watchers: Mutex::new(HashMap::new()) })
        .setup(move |app| {
            let handle = app.handle().clone();
            sync_watchers(&handle, &app.state::<Shared>());
            // Queued mail is retried here too, so a message that failed while offline goes out
            // on its own once the network is back.
            let flusher_api = api.clone();
            let flusher_app = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    if let Ok(r) = flusher_api.outbox_flush().await {
                        if r.attempted > 0 {
                            let _ = flusher_app.emit("outbox://changed", r.sent);
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![diag, rpc, google_login, open_external, reveal_file, download_dir, restart_watchers])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
