//! Local speech-to-text dictation into the focused Herdr pane.
//!
//! The binary is a thin shell over this library, so every stage is testable
//! without a terminal, a microphone or a running Herdr server.

#![forbid(unsafe_code)]

use std::path::PathBuf;

pub mod audio;
pub mod capture;
pub mod config;
pub mod context;
pub mod doctor;
pub mod ipc;
pub mod setup;

/// Namespace Herdr qualifies this plugin's actions and state directories with.
pub const PLUGIN_ID: &str = "abhishekrana.dictate";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not running inside Herdr (HERDR_SOCKET_PATH is unset)")]
    NotInHerdr,

    #[error("Herdr reports no focused pane")]
    NoFocusedPane,

    #[error("no default input device")]
    NoInputDevice,

    #[error("audio: {0}")]
    Audio(String),

    #[error("{0} is not valid TOML")]
    ConfigUnparseable(PathBuf),

    #[error("herdr rpc {method}: {code}{}", if .message.is_empty() { String::new() } else { format!(" - {}", .message) })]
    Rpc {
        method: String,
        code: String,
        message: String,
    },

    #[error("invocation context is not valid JSON")]
    Context(#[source] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
