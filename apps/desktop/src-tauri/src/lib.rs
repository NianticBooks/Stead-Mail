//! Tauri desktop shell. Every command is a thin adapter over `mailcore::Api`, the same object
//! the CLI and the JSON-RPC daemon expose, so the UI never grows logic of its own.

use std::sync::Arc;

use mailcore::Api;
use serde::Serialize;
use tauri::State;

type Core = Arc<Api>;

#[derive(Serialize)]
struct Diag {
    os: &'static str,
    session_type: Option<String>,
    wayland_display: Option<String>,
    hyprland: bool,
    webkit_disable_dmabuf: Option<String>,
    nv_disable_explicit_sync: Option<String>,
    webkit_disable_compositing: Option<String>,
    core: Option<mailcore::api::PingInfo>,
    core_error: Option<String>,
}

#[tauri::command]
fn diag(core: State<'_, Core>) -> Diag {
    let (core_info, core_error) = match core.ping() {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Diag {
        os: std::env::consts::OS,
        session_type: std::env::var("XDG_SESSION_TYPE").ok(),
        wayland_display: std::env::var("WAYLAND_DISPLAY").ok(),
        hyprland: std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok(),
        webkit_disable_dmabuf: std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER").ok(),
        nv_disable_explicit_sync: std::env::var("__NV_DISABLE_EXPLICIT_SYNC").ok(),
        webkit_disable_compositing: std::env::var("WEBKIT_DISABLE_COMPOSITING_MODE").ok(),
        core: core_info,
        core_error,
    }
}

#[tauri::command]
fn list_accounts(core: State<'_, Core>) -> Result<Vec<mailcore::Account>, String> {
    core.list_accounts().map_err(|e| e.to_string())
}

#[tauri::command]
fn list_mailboxes(core: State<'_, Core>, account: String) -> Result<Vec<mailcore::store::MailboxRow>, String> {
    core.list_mailboxes(&account).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_messages(core: State<'_, Core>, account: String, mailbox: String, limit: usize) -> Result<Vec<mailcore::store::MessageRow>, String> {
    core.list_messages(&account, &mailbox, limit).map_err(|e| e.to_string())
}

#[tauri::command]
async fn sync_mailbox(core: State<'_, Core>, account: String, mailbox: String, limit: u32) -> Result<mailcore::api::SyncReport, String> {
    core.sync_mailbox(&account, &mailbox, limit).await.map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The core is opened lazily-but-once here. If the keyring or DB is unavailable we still
    // want the window to appear and report the error, so failures are deferred to `diag`.
    let core: Core = Arc::new(match Api::open_default() {
        Ok(api) => api,
        Err(e) => {
            eprintln!("mailcore: failed to open core: {e}");
            std::process::exit(2);
        }
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(core)
        .invoke_handler(tauri::generate_handler![diag, list_accounts, list_mailboxes, list_messages, sync_mailbox])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
