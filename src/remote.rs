//! Reaching a Herdr server on a saved SSH machine.
//!
//! Exactly one function here touches the operating system: [`run`]. Everything
//! else is a pure function over strings, so quoting, argv and path derivation
//! are testable without a network or a remote host.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::{Error, PLUGIN_ID, Result};

/// `sun_path` bounds an ssh ControlPath; ssh refuses one at or above this.
const CONTROL_PATH_MAX: usize = 108;

/// A script is written to the child's stdin before its output is read, so it
/// must stay under the pipe buffer or the two would deadlock.
pub const MAX_SCRIPT_BYTES: usize = 32 * 1024;

/// ssh's own exit code for a transport failure, as opposed to one from the
/// command it ran.
pub const SSH_TRANSPORT_FAILURE: i32 = 255;

/// Quote a value for POSIX `sh`.
///
/// Inside single quotes no byte is special and `'` is the only terminator, so
/// closing, escaping and reopening is the whole rule. Correct only because the
/// interpreter is pinned to `/bin/sh`: `ssh host cmd args...` joins argv and
/// hands the string to the remote login shell, whose quoting differs.
pub fn sq(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// How to reach one machine.
#[derive(Clone, Debug)]
pub struct Ssh {
    pub program: String,
    pub destination: String,
    /// Socket for a shared connection, or `None` when one does not fit.
    pub control: Option<PathBuf>,
    pub connect_timeout: Duration,
    pub control_persist: Duration,
    pub timeout: Duration,
}

/// What one remote invocation produced.
#[derive(Clone, Debug)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// ssh could not reach the host, as opposed to the remote command failing.
    /// The one failure worth retrying: a shared connection can go stale.
    pub fn is_transport_failure(&self) -> bool {
        self.code == SSH_TRANSPORT_FAILURE
    }
}

/// The socket for a shared connection to `destination`, or `None` when the
/// path would exceed what ssh accepts.
///
/// Sharing is an optimisation: without it every call pays a full handshake,
/// but every call still works.
pub fn control_path(destination: &str) -> Option<PathBuf> {
    control_path_in(&runtime_dir(), destination)
}

/// The runtime directory Herdr's own socket would use.
fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(format!(
                "/tmp/{PLUGIN_ID}-{}",
                rustix::process::getuid().as_raw()
            ))
        })
}

/// The pure half, so the length bound is testable without touching the
/// environment.
pub fn control_path_in(dir: &std::path::Path, destination: &str) -> Option<PathBuf> {
    let path = dir.join(format!("hd-{}", digest(destination)));
    (path.as_os_str().len() < CONTROL_PATH_MAX).then_some(path)
}

/// Short, stable per-destination name. Stability is what lets a later
/// dictation find the connection an earlier one left warm.
fn digest(destination: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(destination.as_bytes());
    hash.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// The options Herdr itself uses for its remote work, so this plugin fails the
/// same way and for the same reasons.
pub fn ssh_args(ssh: &Ssh) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-T".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "NumberOfPasswordPrompts=0".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={}", ssh.connect_timeout.as_secs()),
        "-o".into(),
        "ConnectionAttempts=1".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=4".into(),
    ];
    if let Some(control) = &ssh.control {
        let persist = ssh.control_persist.as_secs();
        args.push("-S".into());
        args.push(control.display().to_string());
        args.push("-o".into());
        args.push("ControlMaster=auto".into());
        args.push("-o".into());
        args.push(if persist == 0 {
            "ControlPersist=no".into()
        } else {
            format!("ControlPersist={persist}")
        });
    }
    args
}

/// Run `script` on the machine, under `/bin/sh` reading it from stdin.
///
/// The only OS-touching function in this module. A non-zero exit is returned
/// rather than raised: callers distinguish a transport failure from a remote
/// refusal, and the two want different handling.
pub fn run(ssh: &Ssh, script: &str) -> Result<Output> {
    if script.len() > MAX_SCRIPT_BYTES {
        return Err(Error::Remote {
            label: ssh.destination.clone(),
            message: format!("script is {} bytes, over the limit", script.len()),
        });
    }

    let mut command = Command::new(&ssh.program);
    command.args(ssh_args(ssh));
    command.arg(&ssh.destination);
    // A fixed word, never the payload: the login shell only ever sees this.
    command.arg("/bin/sh -s");
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    if let Some(parent) = ssh.control.as_ref().and_then(|c| c.parent()) {
        // Losing the shared connection costs latency, never correctness.
        let _ = std::fs::create_dir_all(parent);
    }

    let mut child = command.spawn().map_err(|err| Error::Remote {
        label: ssh.destination.clone(),
        message: format!("cannot start {}: {err}", ssh.program),
    })?;

    let write = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(script.as_bytes()),
        None => Ok(()),
    };

    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    let finished = match rx.recv_timeout(ssh.timeout) {
        Ok(result) => result,
        Err(_) => {
            terminate(pid);
            // Reap, so the child cannot outlive this call.
            let _ = rx.recv();
            return Err(Error::Remote {
                label: ssh.destination.clone(),
                message: format!("no answer within {}s", ssh.timeout.as_secs()),
            });
        }
    };

    // A closed stdin means the far side died first; its output says why.
    drop(write);

    let output = finished.map_err(|err| Error::Remote {
        label: ssh.destination.clone(),
        message: err.to_string(),
    })?;

    Ok(Output {
        code: output.status.code().unwrap_or(SSH_TRANSPORT_FAILURE),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn terminate(pid: u32) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    let Some(pid) = rustix::process::Pid::from_raw(pid) else {
        return;
    };
    let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh() -> Ssh {
        Ssh {
            program: "ssh".into(),
            destination: "host".into(),
            control: Some(PathBuf::from("/run/user/1000/hd-abcdef")),
            connect_timeout: Duration::from_secs(10),
            control_persist: Duration::from_secs(300),
            timeout: Duration::from_secs(20),
        }
    }

    /// The quoting is only worth as much as the interpreter agrees with, so
    /// check it against the interpreter that will actually run it.
    fn round_trips(value: &str) {
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf %s {}", sq(value)))
            .output()
            .expect("/bin/sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout), value, "for {value:?}");
    }

    #[test]
    fn quoting_survives_everything_a_transcript_can_contain() {
        for value in [
            "plain",
            "it's",
            "\"quoted\"",
            "$(id)",
            "`id`",
            "a\\b",
            "; rm -rf /",
            "$HOME",
            "!!",
            "a\nb",
            "'''",
            "* ? [a-z]",
            "caf\u{e9} \u{1f600}",
            "--flag",
        ] {
            round_trips(value);
        }
    }

    #[test]
    fn quoting_survives_a_long_transcript() {
        round_trips(&"word it's ".repeat(1_000));
    }

    #[test]
    fn a_shared_connection_is_dropped_rather_than_truncated() {
        let short = PathBuf::from("/run/user/1000");
        assert!(control_path_in(&short, "host").is_some());

        let long = PathBuf::from("/x".repeat(80));
        assert!(control_path_in(&long, "host").is_none());
    }

    #[test]
    fn the_same_destination_keeps_the_same_socket() {
        let dir = PathBuf::from("/run/user/1000");
        assert_eq!(control_path_in(&dir, "host"), control_path_in(&dir, "host"));
        assert_ne!(
            control_path_in(&dir, "host"),
            control_path_in(&dir, "other")
        );
    }

    #[test]
    fn the_options_refuse_every_prompt() {
        let args = ssh_args(&ssh());
        for expected in [
            "BatchMode=yes",
            "NumberOfPasswordPrompts=0",
            "StrictHostKeyChecking=yes",
            "ConnectTimeout=10",
        ] {
            assert!(args.iter().any(|a| a == expected), "missing {expected}");
        }
    }

    #[test]
    fn sharing_is_absent_when_the_socket_does_not_fit() {
        let mut ssh = ssh();
        ssh.control = None;
        let args = ssh_args(&ssh);
        assert!(!args.iter().any(|a| a == "-S"));
        assert!(!args.iter().any(|a| a.starts_with("ControlMaster")));
    }

    #[test]
    fn zero_persistence_turns_sharing_off_rather_than_forever() {
        let mut ssh = ssh();
        ssh.control_persist = Duration::from_secs(0);
        assert!(ssh_args(&ssh).iter().any(|a| a == "ControlPersist=no"));
    }

    #[test]
    fn an_oversized_script_is_refused_before_it_is_sent() {
        let err = run(&ssh(), &"x".repeat(MAX_SCRIPT_BYTES + 1)).unwrap_err();
        assert!(err.to_string().contains("over the limit"));
    }
}
