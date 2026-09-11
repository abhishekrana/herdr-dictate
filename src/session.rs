//! The recording in progress.
//!
//! A dictation is two invocations of one command: the first records and waits,
//! the second finds it and asks it to stop. The state file is what the second
//! invocation finds, and it holds the pane the first one latched - the words
//! belong where you were looking when you spoke, not where you ended up.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, PLUGIN_ID, Result};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Session {
    pub pid: u32,
    pub pane: String,
    pub submit: bool,
}

pub fn state_path() -> Result<PathBuf> {
    let dir = match std::env::var_os("HERDR_PLUGIN_STATE_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => {
            let home = std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .ok_or_else(|| std::io::Error::other("HOME is unset"))?;
            PathBuf::from(home).join(".local/state").join(PLUGIN_ID)
        }
    };
    Ok(dir.join("recording.json"))
}

/// The recording in progress, if one is actually still running.
///
/// A state file whose process is gone is cleared rather than believed: a
/// recorder that crashed must not wedge every later press.
pub fn live() -> Result<Option<Session>> {
    let path = state_path()?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let Ok(session) = serde_json::from_str::<Session>(&text) else {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    };
    if is_running(session.pid) {
        Ok(Some(session))
    } else {
        let _ = std::fs::remove_file(&path);
        Ok(None)
    }
}

pub fn begin(session: &Session) -> Result<()> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(session)?;
    std::fs::write(&path, json)?;
    Ok(())
}

pub fn end() -> Result<()> {
    let path = state_path()?;
    match std::fs::remove_file(&path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}

/// Ask the running recorder to finish. It transcribes and delivers what it has.
pub fn stop(session: &Session) -> Result<()> {
    let pid = rustix::process::Pid::from_raw(session.pid as i32)
        .ok_or_else(|| Error::Audio(format!("invalid recorder pid {}", session.pid)))?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)
        .map_err(|e| Error::Audio(format!("stopping recorder {}: {e}", session.pid)))
}

/// Whether this pid is one of ours and still alive.
///
/// The command line is checked as well as the pid, so a reused pid belonging to
/// an unrelated process is never signalled.
fn is_running(pid: u32) -> bool {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(raw) => is_ours(&String::from_utf8_lossy(&raw)),
        Err(_) => false,
    }
}

fn is_ours(cmdline: &str) -> bool {
    cmdline.split('\0').next().is_some_and(|argv0| {
        std::path::Path::new(argv0)
            .file_name()
            .is_some_and(|name| name == "herdr-dictate")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_binary_counts_as_ours() {
        assert!(is_ours("/usr/local/bin/herdr-dictate\0toggle\0"));
        assert!(is_ours("herdr-dictate"));
        // The test binary and anything else sharing the directory name are not.
        assert!(!is_ours(
            "/home/u/herdr-dictate/target/debug/deps/herdr_dictate-a1b2"
        ));
        assert!(!is_ours("/usr/bin/python3\0script.py\0"));
        assert!(!is_ours(""));
    }

    #[test]
    fn an_impossible_pid_is_not_running() {
        assert!(!is_running(u32::MAX));
    }

    #[test]
    fn a_session_round_trips_through_json() {
        let session = Session {
            pid: 42,
            pane: "w1:p2".into(),
            submit: true,
        };
        let text = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&text).unwrap(), session);
    }
}
