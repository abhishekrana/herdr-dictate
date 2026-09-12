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

use serde::Deserialize;

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
///
/// `kind` names the call in the log. Every crossing of the machine boundary is
/// traced with its destination, exit status and elapsed time - the script is
/// never logged, because it carries the transcript.
pub fn run(ssh: &Ssh, kind: &str, script: &str) -> Result<Output> {
    let started = std::time::Instant::now();
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

    let mut child = command.spawn().map_err(|err| {
        tracing::warn!(host = %ssh.destination, kind, %err, "ssh would not start");
        Error::Remote {
            label: ssh.destination.clone(),
            message: format!("cannot start {}: {err}", ssh.program),
        }
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
            tracing::warn!(
                host = %ssh.destination,
                kind,
                ms = started.elapsed().as_millis(),
                "ssh timed out"
            );
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

    let result = Output {
        code: output.status.code().unwrap_or(SSH_TRANSPORT_FAILURE),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    // Warm and cold differ by an order of magnitude, so the elapsed time is
    // what tells a slow link from a connection that is not being shared.
    tracing::debug!(
        host = %ssh.destination,
        kind,
        code = result.code,
        ms = started.elapsed().as_millis(),
        shared = ssh.control.is_some(),
        bytes = result.stdout.len(),
        "remote call"
    );
    if result.code != 0 {
        tracing::warn!(
            host = %ssh.destination,
            kind,
            code = result.code,
            transport = result.is_transport_failure(),
            stderr = %tail(&result.stderr),
            "remote call failed"
        );
    }
    Ok(result)
}

/// The end of a stderr stream, which is where the reason is. Bounded: ssh can
/// be verbose and a log line is not the place for all of it.
fn tail(text: &str) -> String {
    const MAX: usize = 300;
    let trimmed = text.trim();
    match trimmed.char_indices().nth_back(MAX) {
        Some((cut, _)) => format!("…{}", &trimmed[cut..]),
        None => trimmed.to_string(),
    }
}

/// The remote command typed the text but did not submit it, because the pane
/// no longer hosts an agent.
pub const AGENT_GONE: i32 = 3;

/// A machine resolved far enough to deliver to. Only `sink::selected` builds
/// one, so holding it is proof the session and binary were found.
#[derive(Clone, Debug)]
pub struct Target {
    pub ssh: Ssh,
    /// Absolute path to herdr on that machine.
    pub herdr: String,
    pub session: String,
    /// What the sidebar calls this machine, for messages.
    pub label: String,
}

/// Type the transcript into `pane`, submitting only when asked.
///
/// Verification rides in the same invocation as delivery: an agent that exits
/// leaves its pane at a shell prompt, and submitting there would run the
/// transcript as a command.
pub fn deliver_script(target: &Target, pane: &str, text: &str, submit: bool) -> String {
    let mut script = String::from("set -u\n");
    script.push_str(&format!("H={}\n", sq(&target.herdr)));
    script.push_str(&format!("S={}\n", sq(&target.session)));
    script.push_str(&format!("P={}\n", sq(pane)));
    script.push_str(&format!("T={}\n", sq(text)));
    if submit {
        script.push_str(
            "if \"$H\" --session \"$S\" agent list \
             | grep -q \"\\\"pane_id\\\"[[:space:]]*:[[:space:]]*\\\"$P\\\"\"; then\n\
             \x20   exec \"$H\" --session \"$S\" agent prompt \"$P\" \"$T\"\n\
             else\n\
             \x20   \"$H\" --session \"$S\" pane send-text \"$P\" \"$T\" || exit 1\n\
             \x20   exit 3\n\
             fi\n",
        );
    } else {
        script.push_str("exec \"$H\" --session \"$S\" pane send-text \"$P\" \"$T\"\n");
    }
    script
}

/// Set or clear the pane label that shows a dictation is running.
///
/// The user is looking at the remote pane, so the indicator has to live there.
pub fn label_script(target: &Target, pane: &str, label: Option<(&str, Duration)>) -> String {
    let mut script = String::from("set -u\n");
    script.push_str(&format!("H={}\n", sq(&target.herdr)));
    script.push_str(&format!("S={}\n", sq(&target.session)));
    script.push_str(&format!("P={}\n", sq(pane)));
    script.push_str(&format!("O={}\n", sq(crate::PLUGIN_ID)));
    match label {
        Some((text, ttl)) => {
            script.push_str(&format!("L={}\n", sq(text)));
            script.push_str(&format!("W={}\n", ttl.as_millis()));
            script.push_str(
                "exec \"$H\" --session \"$S\" pane report-metadata \"$P\" \
                 --source \"$O\" --display-agent \"$L\" --ttl-ms \"$W\"\n",
            );
        }
        None => script.push_str(
            "exec \"$H\" --session \"$S\" pane report-metadata \"$P\" \
             --source \"$O\" --clear-display-agent\n",
        ),
    }
    script
}

/// What one machine reports about itself, from a single round trip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The binary that answered, so `doctor` can tell the user what to pin.
    pub herdr: String,
    pub focused_pane: String,
    pub agents: Vec<Agent>,
}

/// One entry of the remote `agent list`. Detected agents carry no name, so the
/// pane is the only handle.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Agent {
    pub pane_id: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub agent_status: String,
}

/// What is living in the pane a transcript is about to go to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occupant {
    Agent,
    Other,
}

impl Probe {
    pub fn occupant(&self) -> Occupant {
        if self.agents.iter().any(|a| a.pane_id == self.focused_pane) {
            Occupant::Agent
        } else {
            Occupant::Other
        }
    }
}

/// Framing for one invocation, unforgeable by anything the remote prints.
pub fn nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("__hd{:x}{:x}__", std::process::id(), nanos)
}

/// Everything the latch needs, in one invocation.
///
/// Three separate calls measured 1550ms cold against 76ms batched and warm,
/// because each ssh exec channel costs round trips that the remote work does
/// not. Finding the binary rides along rather than costing a call of its own.
pub fn resolve_script(session: &str, configured_herdr: &str, nonce: &str) -> String {
    let mut script = String::from("set -u\n");
    script.push_str(&format!("M={}\n", sq(nonce)));
    script.push_str(&format!("C={}\n", sq(configured_herdr)));
    script.push_str(HERDR_LOOKUP);
    script.push_str("printf '%s herdr %s\\n' \"$M\" \"$H\"\n");
    script.push_str(&format!("S={}\n", sq(session)));
    script.push_str("printf '%s snapshot\\n' \"$M\"\n");
    script.push_str("\"$H\" --session \"$S\" api snapshot || exit 91\n");
    script.push_str("printf '%s agents\\n' \"$M\"\n");
    script.push_str("\"$H\" --session \"$S\" agent list || exit 91\n");
    script.push_str("printf '%s end\\n' \"$M\"\n");
    script
}

/// Herdr commonly lives under the user's home, which a non-interactive shell
/// does not have on PATH, so a bare `herdr` fails where an interactive one
/// works.
const HERDR_LOOKUP: &str = r#"H=''
for c in "$C" "$HOME/.local/bin/herdr" "$HOME/.cargo/bin/herdr" /usr/local/bin/herdr; do
    if [ -n "$c" ] && [ -x "$c" ]; then H="$c"; break; fi
done
[ -z "$H" ] && H="$(command -v herdr 2>/dev/null || true)"
if [ -z "$H" ]; then printf '%s herdr-missing\n' "$M"; exit 90; fi
"#;

/// Read back a [`Probe`] from framed output.
///
/// Everything before the first sentinel is discarded: a login shell's rc files
/// print to stdout on a non-interactive ssh, and that is the usual way a
/// remote-exec parser breaks.
pub fn parse_probe(stdout: &str, nonce: &str) -> Result<Probe> {
    let mut herdr = String::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    let mut ended = false;

    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix(nonce) else {
            if let Some((_, body)) = current.as_mut() {
                body.push_str(line);
                body.push('\n');
            }
            continue;
        };
        if let Some(section) = current.take() {
            sections.push(section);
        }
        let mut parts = rest.trim().splitn(2, ' ');
        match (parts.next().unwrap_or(""), parts.next()) {
            ("herdr", Some(path)) => herdr = path.trim().to_string(),
            ("herdr-missing", _) => {
                return Err(Error::Remote {
                    label: "remote".into(),
                    message: "no herdr binary found; set [remote.machines] herdr".into(),
                });
            }
            ("end", _) => ended = true,
            (kind, _) => current = Some((kind.to_string(), String::new())),
        }
    }
    if let Some(section) = current.take() {
        sections.push(section);
    }
    if !ended {
        return Err(Error::Remote {
            label: "remote".into(),
            message: "output ended early; nothing was latched".into(),
        });
    }

    let focused_pane = body(&sections, "snapshot")
        .and_then(|b| serde_json::from_str::<serde_json::Value>(b).ok())
        .and_then(|v| {
            [
                "/result/snapshot/focused_pane_id",
                "/result/focused_pane_id",
            ]
            .iter()
            .find_map(|p| v.pointer(p).and_then(|f| f.as_str()).map(String::from))
        })
        .ok_or(Error::NoFocusedPane)?;

    let agents = body(&sections, "agents")
        .and_then(|b| serde_json::from_str::<serde_json::Value>(b).ok())
        .and_then(|v| v.pointer("/result/agents").cloned())
        .and_then(|a| serde_json::from_value(a).ok())
        .unwrap_or_default();

    Ok(Probe {
        herdr,
        focused_pane,
        agents,
    })
}

fn body<'a>(sections: &'a [(String, String)], kind: &str) -> Option<&'a str> {
    sections
        .iter()
        .find(|(k, _)| k == kind)
        .map(|(_, b)| b.trim())
        .filter(|b| !b.is_empty())
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

    const N: &str = "__hdtest__";

    fn probe_output(snapshot: &str, agents: &str) -> String {
        format!(
            "{N} herdr /home/u/.local/bin/herdr\n\
             {N} snapshot\n{snapshot}\n\
             {N} agents\n{agents}\n\
             {N} end\n"
        )
    }

    const SNAPSHOT: &str = r#"{"id":"cli:api:snapshot","result":{"snapshot":
        {"focused_pane_id":"w1:p3","agents":[]}}}"#;
    const AGENTS: &str = r#"{"id":"cli:agent:list","result":{"agents":
        [{"pane_id":"w1:p3","agent":"claude","agent_status":"idle"}]}}"#;

    #[test]
    fn a_probe_reads_back_the_pane_and_its_occupant() {
        let probe = parse_probe(&probe_output(SNAPSHOT, AGENTS), N).unwrap();
        assert_eq!(probe.focused_pane, "w1:p3");
        assert_eq!(probe.herdr, "/home/u/.local/bin/herdr");
        assert_eq!(probe.occupant(), Occupant::Agent);
    }

    #[test]
    fn a_pane_with_no_agent_is_not_an_agent_pane() {
        let agents = r#"{"result":{"agents":[{"pane_id":"w1:p9","agent":"claude"}]}}"#;
        let probe = parse_probe(&probe_output(SNAPSHOT, agents), N).unwrap();
        assert_eq!(probe.occupant(), Occupant::Other);
    }

    #[test]
    fn an_empty_agent_list_is_not_an_error() {
        let probe = parse_probe(&probe_output(SNAPSHOT, r#"{"result":{"agents":[]}}"#), N).unwrap();
        assert_eq!(probe.occupant(), Occupant::Other);
    }

    #[test]
    fn login_shell_noise_before_the_first_sentinel_is_discarded() {
        let noisy = format!(
            "welcome to the box\nmotd line\n{}",
            probe_output(SNAPSHOT, AGENTS)
        );
        assert_eq!(parse_probe(&noisy, N).unwrap().focused_pane, "w1:p3");
    }

    #[test]
    fn truncated_output_latches_nothing() {
        let cut = format!("{N} herdr /h\n{N} snapshot\n{SNAPSHOT}\n");
        let err = parse_probe(&cut, N).unwrap_err();
        assert!(err.to_string().contains("ended early"));
    }

    #[test]
    fn a_sentinel_from_another_invocation_is_ignored() {
        let forged = probe_output(SNAPSHOT, AGENTS).replace(N, "__hdother__");
        assert!(parse_probe(&forged, N).is_err());
    }

    #[test]
    fn a_machine_without_herdr_says_so_rather_than_latching() {
        let err = parse_probe(&format!("{N} herdr-missing\n{N} end\n"), N).unwrap_err();
        assert!(err.to_string().contains("no herdr binary"));
    }

    #[test]
    fn a_snapshot_with_no_focused_pane_is_an_error() {
        let err =
            parse_probe(&probe_output(r#"{"result":{"snapshot":{}}}"#, AGENTS), N).unwrap_err();
        assert!(matches!(err, Error::NoFocusedPane));
    }

    #[test]
    fn the_resolve_script_quotes_a_hostile_session_name() {
        let script = resolve_script("it's; rm -rf /", "", N);
        assert!(script.contains(r#"S='it'\''s; rm -rf /'"#));
        assert!(!script.contains("\nrm -rf"));
    }

    #[test]
    fn a_nonce_is_not_shared_between_invocations() {
        assert_ne!(nonce(), nonce());
    }

    #[test]
    fn a_log_line_carries_the_end_of_stderr_not_all_of_it() {
        assert_eq!(tail("  boom  "), "boom");
        let long = "x".repeat(1_000);
        let cut = tail(&long);
        assert!(cut.len() < long.len());
        assert!(cut.starts_with('\u{2026}'));
        // The reason ssh gives is at the end, so that is the end kept.
        assert!(tail(&format!("{long}Permission denied")).ends_with("Permission denied"));
    }

    #[test]
    fn an_oversized_script_is_refused_before_it_is_sent() {
        let err = run(&ssh(), "test", &"x".repeat(MAX_SCRIPT_BYTES + 1)).unwrap_err();
        assert!(err.to_string().contains("over the limit"));
    }
}
