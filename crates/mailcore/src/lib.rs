//! mailcore: the platform-agnostic core of the AI-native mail + calendar client.
//!
//! Everything that is hard lives here and is exposed through [`api::Api`], which the
//! CLI, the JSON-RPC daemon (`mailcore serve`) and the Tauri desktop shell all call.
//! No UI toolkit types leak into this crate.

pub mod account;
pub mod ai;
pub mod api;
pub mod body;
pub mod caldav;
pub mod error;
pub mod google;
pub mod html;
pub mod imap;
pub mod oauth;
pub mod rpc;
pub mod secrets;
pub mod smtp;
pub mod sync;
pub mod store;

pub use account::{Account, AuthKind, Provider, ServerConfig};
pub use api::Api;
pub use error::{Error, Result};

/// Default path of the daemon's Unix socket: `$XDG_RUNTIME_DIR/mailcore.sock`, else in the data dir.
pub fn socket_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("MAILCORE_SOCKET") {
        return std::path::PathBuf::from(p);
    }
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir).join("mailcore.sock"),
        _ => data_dir().join("mailcore.sock"),
    }
}

/// Where mailcore keeps its database and fallback secret file.
pub fn data_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("MAILCORE_DATA_DIR") {
        return std::path::PathBuf::from(p);
    }
    directories::ProjectDirs::from("", "", "mailcore")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(".mailcore"))
}

/// The one HTTP client constructor. Installs rustls' process-wide crypto provider first, because
/// reqwest panics when it builds a TLS config without one.
pub fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("mailcore/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client")
}
