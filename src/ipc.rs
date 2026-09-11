//! Client for the Herdr socket API.
//!
//! Requests are one JSON object per line over the Unix socket named by
//! `HERDR_SOCKET_PATH`. Talking the socket directly rather than spawning the
//! `herdr` binary keeps a dictation off the process-spawn path, which runs once
//! per delivery and once per state change.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Error, Result};

/// Requests are cheap and local; a hung server must not hold the microphone.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A reply is metadata, never terminal scrollback. Bounded so a malformed or
/// hostile response cannot exhaust memory.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Client {
    socket: PathBuf,
    timeout: Duration,
}

impl Client {
    /// The socket Herdr injected into this plugin process.
    pub fn from_env() -> Result<Self> {
        let socket = std::env::var_os("HERDR_SOCKET_PATH")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .ok_or(Error::NotInHerdr)?;
        Ok(Self::new(socket))
    }

    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            timeout: TIMEOUT,
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Send one request and return its `result`, or the server's own error.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;

        let request = json!({
            "id": format!("herdr-dictate:{method}"),
            "method": method,
            "params": params,
        });
        serde_json::to_writer(&mut stream, &request)?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut reader = BufReader::new(stream.take(MAX_RESPONSE_BYTES + 1));
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(Error::Rpc {
                method: method.into(),
                code: "empty_response".into(),
                message: "server closed the connection".into(),
            });
        }
        if line.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(Error::Rpc {
                method: method.into(),
                code: "response_too_large".into(),
                message: format!("over {MAX_RESPONSE_BYTES} bytes"),
            });
        }

        let mut response: Value = serde_json::from_slice(&line)?;
        if let Some(error) = response.get_mut("error") {
            // Herdr answers a missing pane with a machine-readable code, so a
            // failed delivery can name the pane and the reason without parsing prose.
            return Err(Error::Rpc {
                method: method.into(),
                code: error
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .into(),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
            });
        }
        Ok(response
            .get_mut("result")
            .map(Value::take)
            .unwrap_or(Value::Null))
    }

    /// Type literal text into a pane without submitting it.
    pub fn send_text(&self, pane: &str, text: &str) -> Result<()> {
        self.call("pane.send_text", json!({ "pane_id": pane, "text": text }))?;
        Ok(())
    }

    /// Press named keys in a pane.
    pub fn send_keys(&self, pane: &str, keys: &[&str]) -> Result<()> {
        self.call("pane.send_keys", json!({ "pane_id": pane, "keys": keys }))?;
        Ok(())
    }

    /// The pane Herdr considers focused. Used by entry points that arrive
    /// without an invocation context, such as a global hotkey.
    pub fn focused_pane(&self) -> Result<String> {
        let snapshot = self.call("session.snapshot", json!({}))?;
        snapshot
            .pointer("/snapshot/focused_pane_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or(Error::NoFocusedPane)
    }

    /// Re-read config.toml in the running server. Returns its diagnostics.
    pub fn reload_config(&self) -> Result<Vec<String>> {
        let result = self.call("server.reload_config", json!({}))?;
        Ok(result
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|d| {
                        d.as_str()
                            .map(ToOwned::to_owned)
                            .unwrap_or_else(|| d.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}
