//! The recording in progress.
//!
//! A dictation is two invocations of one command: the first records and waits,
//! the second finds it and asks it to stop. The state file is what the second
//! invocation finds, and it holds the pane the first one latched - the words
//! belong where you were looking when you spoke, not where you ended up.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, PLUGIN_ID, Result};

/// What the recorder is doing now. A dictation outlives the microphone.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Recording,
    Transcribing,
}

impl Phase {
    /// What the pane shows during this phase.
    pub fn label(self) -> &'static str {
        match self {
            Self::Recording => "● dictating",
            Self::Transcribing => "◌ transcribing",
        }
    }

    /// The name the state files carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Transcribing => "transcribing",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Session {
    pub pid: u32,
    pub pane: String,
    pub submit: bool,
    /// Missing reads as recording.
    #[serde(default)]
    pub phase: Phase,
    /// Absent for the local server, which is every file written before remote
    /// delivery existed. A pane id only means something on one machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineRef>,
}

/// Which machine a pane id belongs to.
///
/// Id and label only: nothing re-resolves from this file, and an ssh
/// destination does not belong in it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineRef {
    pub id: String,
    pub label: String,
}

/// Where the recording state lives. Herdr names the directory for a plugin
/// process; anything else has to resolve the same path, so the fallback is the
/// directory Herdr would have given.
pub fn state_path() -> Result<PathBuf> {
    let dir = match std::env::var_os("HERDR_PLUGIN_STATE_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => state_home()?.join("herdr/plugins").join(PLUGIN_ID),
    };
    Ok(dir.join("recording.json"))
}

fn state_home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| std::io::Error::other("HOME is unset"))?;
    Ok(PathBuf::from(home).join(".local/state"))
}

/// The recording in progress, if one is actually still running.
///
/// A state file whose process is gone is cleared rather than believed: a
/// recorder that crashed must not wedge every later press.
pub fn live() -> Result<Option<Session>> {
    let path = state_path()?;
    let session = peek_at(&path)?;
    if session.is_none() && path.exists() {
        // Nothing alive is described here, so the next press starts clean.
        let _ = std::fs::remove_file(&path);
    }
    Ok(session.filter(|session| session.phase == Phase::Recording))
}

/// Whatever the state file says, if its process is alive. Never writes, so a
/// reader on a timer cannot destroy a running dictation.
pub fn peek() -> Result<Option<Session>> {
    peek_at(&state_path()?)
}

fn peek_at(path: &std::path::Path) -> Result<Option<Session>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let Ok(session) = serde_json::from_str::<Session>(&text) else {
        return Ok(None);
    };
    Ok(is_running(session.pid).then_some(session))
}

pub fn begin(session: &Session) -> Result<()> {
    write_state(session)
}

/// Write the state file whole, so a reader never sees half of one.
/// Keep a transcript that could not be delivered, and say where.
///
/// Only a lost transcript is an error, so words the sink refused are written
/// somewhere the user can read them rather than left in a log line.
pub fn keep_undelivered(text: &str) -> Result<PathBuf> {
    let path = state_path()?.with_file_name("undelivered.txt");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn write_state(session: &Session) -> Result<()> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string(session)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Clear the state file, but only while it still names this recorder: a press
/// during transcription writes a new session over the same file.
pub fn end(session: &Session) -> Result<()> {
    let path = state_path()?;
    match peek_at(&path)? {
        Some(current) if current.pid != session.pid => return Ok(()),
        _ => {}
    }
    match std::fs::remove_file(&path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}

/// Clears the state file however the press ends.
pub struct Active {
    session: Session,
}

impl Active {
    pub fn begin(session: Session) -> Result<Self> {
        write_state(&session)?;
        Ok(Self { session })
    }

    /// The microphone is closed; the model is still running.
    pub fn transcribing(&mut self) {
        self.session.phase = Phase::Transcribing;
        if let Err(err) = write_state(&self.session) {
            tracing::debug!(%err, "could not record the transcribing phase");
        }
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        let _ = end(&self.session);
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
            phase: Phase::Transcribing,
            machine: None,
        };
        let text = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&text).unwrap(), session);
    }

    #[test]
    fn a_file_from_before_remote_delivery_reads_as_local() {
        let text = r#"{"pid":42,"pane":"w1:p2","submit":false,"phase":"recording"}"#;
        assert_eq!(serde_json::from_str::<Session>(text).unwrap().machine, None);
    }

    #[test]
    fn a_remote_session_round_trips_with_its_machine() {
        let session = Session {
            pid: 42,
            pane: "w1:p2".into(),
            submit: true,
            phase: Phase::Recording,
            machine: Some(MachineRef {
                id: "7339".into(),
                label: "desk".into(),
            }),
        };
        let text = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&text).unwrap(), session);
    }

    #[test]
    fn a_file_without_a_phase_reads_as_recording() {
        let text = r#"{"pid":42,"pane":"w1:p2","submit":false}"#;
        let session = serde_json::from_str::<Session>(text).unwrap();
        assert_eq!(session.phase, Phase::Recording);
    }

    #[test]
    fn a_torn_file_is_reported_as_no_session_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recording.json");
        // Half a write, as a reader racing a non-atomic write would see.
        std::fs::write(&path, r#"{"pid":42,"pane":"w1"#).unwrap();

        assert!(peek_at(&path).unwrap().is_none());
        assert!(path.exists(), "a torn read must not delete the session");
    }

    #[test]
    fn a_session_whose_process_is_gone_is_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recording.json");
        let session = Session {
            pid: u32::MAX,
            pane: "w1:p2".into(),
            submit: false,
            phase: Phase::Recording,
            machine: None,
        };
        std::fs::write(&path, serde_json::to_string(&session).unwrap()).unwrap();

        assert!(peek_at(&path).unwrap().is_none());
        assert!(path.exists(), "peek never writes; live() is what heals");
    }
}
