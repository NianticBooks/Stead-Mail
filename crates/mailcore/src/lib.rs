//! mailcore: the platform-agnostic core of the AI-native mail + calendar client.
//!
//! Everything that is hard lives here and is exposed through [`api::Api`], which the
//! CLI, the JSON-RPC daemon (`mailcore serve`) and the Tauri desktop shell all call.
//! No UI toolkit types leak into this crate.

pub mod account;
pub mod api;
pub mod error;
pub mod imap;
pub mod rpc;
pub mod secrets;
pub mod store;

pub use account::{Account, AuthKind, Provider, ServerConfig};
pub use api::Api;
pub use error::{Error, Result};

/// Where mailcore keeps its database and fallback secret file.
pub fn data_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("MAILCORE_DATA_DIR") {
        return std::path::PathBuf::from(p);
    }
    directories::ProjectDirs::from("", "", "mailcore")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(".mailcore"))
}
