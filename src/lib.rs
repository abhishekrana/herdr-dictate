//! Local speech-to-text dictation into the focused Herdr pane.
//!
//! The binary is a thin shell over this library so every stage is testable
//! without a terminal, a microphone or a running Herdr server.

#![forbid(unsafe_code)]

pub mod context;
pub mod ipc;

/// Errors this plugin can report. Each variant is something a user can act on;
/// anything else surfaces as its underlying cause.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not running inside Herdr (HERDR_SOCKET_PATH is unset)")]
    NotInHerdr,

    #[error("Herdr reports no focused pane")]
    NoFocusedPane,

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
