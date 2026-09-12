//! Diagnostics: one named check per thing that can break, each with a remedy.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::audio::{SilenceConfig, rms};
use crate::context::Context;
use crate::ipc::Client;
use crate::settings::Settings;
use crate::{PLUGIN_ID, capture, config, server};

/// The entry to add to Herdr's own config, naming this binary by its real path:
/// the plugin is not on PATH.
fn chip_remedy() -> String {
    let exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "herdr-dictate".into());
    format!(
        r#"add under [ui] in ~/.config/herdr/config.toml: tab_bar_right = [{{ type = "command", command = "{exe} status", interval_seconds = 1 }}]"#
    )
}

/// How long the level check listens for.
const LEVEL_SAMPLE: Duration = Duration::from_millis(700);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// Works, but will probably misbehave.
    Warn,
    Fail,
}

#[derive(Debug)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    /// What to do about it. Empty when there is nothing to do.
    pub remedy: String,
}

fn ok(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Ok,
        detail: detail.into(),
        remedy: String::new(),
    }
}

fn warn(name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Warn,
        detail: detail.into(),
        remedy: remedy.into(),
    }
}

fn fail(name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Fail,
        detail: detail.into(),
        remedy: remedy.into(),
    }
}

/// Every check, in the order they are reported.
pub fn run() -> Vec<Check> {
    vec![
        herdr_socket(),
        focused_pane(),
        input_device(),
        input_level(),
        model_server(),
        config_file(),
        bindings(),
        registration(),
        status_chip(),
        plugin_dirs(),
        remote_machine(),
        remote_ssh(),
        remote_herdr(),
        remote_session(),
        remote_control(),
    ]
}

/// Everything the remote checks need, fetched once.
///
/// One round trip answers three questions; asking separately would be three
/// connections and, worse, three different answers.
fn probe() -> &'static Option<RemoteState> {
    static PROBE: std::sync::OnceLock<Option<RemoteState>> = std::sync::OnceLock::new();
    PROBE.get_or_init(remote_state)
}

struct RemoteState {
    machine: crate::machine::Machine,
    latch: Option<crate::sink::Latch>,
    error: Option<String>,
}

fn remote_state() -> Option<RemoteState> {
    let config = Settings::load().unwrap_or_default().remote;
    if !config.enabled {
        return None;
    }
    let machine = crate::machine::selected(&crate::machine::herdr_bin(&config.herdr))
        .ok()
        .flatten()?;
    let (latch, error) = match crate::sink::selected(&config) {
        Ok(latch) => (latch, None),
        Err(err) => (None, Some(err.to_string())),
    };
    Some(RemoteState {
        machine,
        latch,
        error,
    })
}

/// Which machine a dictation would go to. Nothing selected is the ordinary
/// case, not a problem.
fn remote_machine() -> Check {
    let name = "remote.machine";
    let Some(state) = probe() else {
        return ok(name, "none selected; delivering locally");
    };
    if !state.machine.enabled {
        return fail(
            name,
            format!("{} is selected but disabled", state.machine.label),
            "herdr machine enable <id>, or select another machine",
        );
    }
    ok(
        name,
        format!("{} ({})", state.machine.label, state.machine.target),
    )
}

/// Non-interactive key auth, which is a precondition rather than a nicety.
fn remote_ssh() -> Check {
    let name = "remote.ssh";
    let Some(state) = probe() else {
        return ok(name, "no machine selected");
    };
    match &state.error {
        None => ok(name, format!("reaches {}", state.machine.target)),
        Some(err) => fail(
            name,
            err.clone(),
            format!(
                "ssh {} true must succeed without a prompt; host keys are checked \
                 strictly, so connect once by hand first, and load a passphrased \
                 key with ssh-add",
                state.machine.target
            ),
        ),
    }
}

/// Herdr commonly lives under the user's home, which a non-interactive shell
/// does not have on PATH.
fn remote_herdr() -> Check {
    let name = "remote.herdr";
    let Some(state) = probe() else {
        return ok(name, "no machine selected");
    };
    match state.latch.as_ref().map(|l| l.sink.describe_binary()) {
        Some(path) if !path.is_empty() => ok(name, path.to_string()),
        _ => warn(
            name,
            "not resolved",
            format!(
                "set [remote.machines.\"{}\"] herdr to its absolute path",
                state.machine.id
            ),
        ),
    }
}

/// The pane a dictation would land in, and whether an agent is living there.
fn remote_session() -> Check {
    let name = "remote.session";
    let Some(state) = probe() else {
        return ok(name, "no machine selected");
    };
    let Some(latch) = state.latch.as_ref() else {
        return fail(
            name,
            format!("session {} did not answer", state.machine.session),
            "start that session on the machine, or fix the profile's session name",
        );
    };
    let occupant = match latch.occupant {
        crate::remote::Occupant::Agent => "an agent",
        crate::remote::Occupant::Other => "a shell, so nothing will be submitted",
    };
    ok(name, format!("{} hosts {occupant}", latch.pane))
}

/// Sharing one ssh connection is what keeps a dictation off a fresh handshake.
fn remote_control() -> Check {
    let name = "remote.control";
    let Some(state) = probe() else {
        return ok(name, "no machine selected");
    };
    match crate::remote::control_path(&state.machine.target) {
        Some(path) => ok(name, path.display().to_string()),
        None => warn(
            name,
            "the socket path is too long to share a connection",
            "every call reconnects, costing about half a second each",
        ),
    }
}

/// Whether Herdr runs this binary, or another build of the plugin.
fn registration() -> Check {
    let name = "plugin.registration";
    let Ok(client) = Client::from_env() else {
        return warn(name, "not running inside Herdr", "expected outside Herdr");
    };
    let root = match client.plugin_root() {
        Ok(Some(root)) => root,
        Ok(None) => {
            return fail(
                name,
                "not registered",
                "herdr plugin link <dir>, or herdr plugin install abhishekrana/herdr-dictate",
            );
        }
        Err(err) => return warn(name, err.to_string(), "check the Herdr socket"),
    };
    // The binary sits at <root>/target/release/, so its root is three up.
    let mine = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.ancestors().nth(3).map(Path::to_path_buf));
    match mine {
        Some(mine) if mine == Path::new(&root) => ok(name, root),
        Some(mine) => fail(
            name,
            format!("Herdr runs {root}, this binary is from {}", mine.display()),
            "scripts/deploy.sh, or herdr plugin link the directory you build in",
        ),
        None => warn(name, format!("registered at {root}"), ""),
    }
}

/// The tab-bar chip, which the user adds to Herdr's own config.
fn status_chip() -> Check {
    let name = "status.chip";
    let Ok(path) = config::config_path() else {
        return warn(name, "no Herdr config path", "set HOME");
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return warn(
            name,
            format!("{} is unreadable", path.display()),
            chip_remedy(),
        );
    };
    if text.contains("herdr-dictate status") {
        ok(name, "configured in tab_bar_right")
    } else {
        warn(name, "not in tab_bar_right", chip_remedy())
    }
}

/// The most severe status present.
pub fn worst(checks: &[Check]) -> Status {
    if checks.iter().any(|c| c.status == Status::Fail) {
        Status::Fail
    } else if checks.iter().any(|c| c.status == Status::Warn) {
        Status::Warn
    } else {
        Status::Ok
    }
}

pub fn render(checks: &[Check], out: &mut impl Write) -> std::io::Result<()> {
    writeln!(
        out,
        "herdr-dictate {} ({PLUGIN_ID})",
        env!("CARGO_PKG_VERSION")
    )?;
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for check in checks {
        let status = match check.status {
            Status::Ok => "ok  ",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        };
        writeln!(out, "{status}  {:width$}  {}", check.name, check.detail)?;
        if !check.remedy.is_empty() {
            writeln!(out, "      {:width$}  -> {}", "", check.remedy)?;
        }
    }
    Ok(())
}

fn herdr_socket() -> Check {
    match Client::from_env() {
        Ok(client) => match client.focused_pane() {
            Ok(_) => ok("herdr.socket", client.socket_path().display().to_string()),
            Err(err) => fail(
                "herdr.socket",
                err.to_string(),
                "Is the server running? Try `herdr status`.",
            ),
        },
        Err(err) => warn(
            "herdr.socket",
            err.to_string(),
            "Expected outside Herdr; delivery needs a running server.",
        ),
    }
}

fn focused_pane() -> Check {
    match Context::from_env().ok().and_then(|c| c.target_pane()) {
        Some(pane) => ok(
            "herdr.pane",
            format!("{pane} (from the invocation context)"),
        ),
        None => match Client::from_env().and_then(|c| c.focused_pane()) {
            Ok(pane) => ok("herdr.pane", format!("{pane} (asked the server)")),
            // Outside Herdr there is no pane to find, which is not a fault.
            Err(crate::Error::NotInHerdr) => {
                warn("herdr.pane", "no Herdr session", "Expected outside Herdr.")
            }
            Err(err) => fail(
                "herdr.pane",
                err.to_string(),
                "Focus a Herdr pane, or run this from inside one.",
            ),
        },
    }
}

fn input_device() -> Check {
    match capture::describe_input() {
        Ok(detail) => ok("audio.device", detail),
        Err(err) => fail(
            "audio.device",
            err.to_string(),
            "Check the microphone is connected and not muted.",
        ),
    }
}

/// Measure the room against the speech threshold.
///
/// Ambient level at or above it means trailing-silence auto-stop never fires.
fn input_level() -> Check {
    let threshold = SilenceConfig::default().threshold;
    let silence = SilenceConfig {
        max_duration: LEVEL_SAMPLE,
        ..SilenceConfig::default()
    };
    match capture::record(silence, Arc::new(AtomicBool::new(false))) {
        Err(err) => fail(
            "audio.level",
            err.to_string(),
            "The device opened but did not record.",
        ),
        Ok(recording) if recording.samples.is_empty() => fail(
            "audio.level",
            "no samples captured",
            "The stream opened but delivered nothing.",
        ),
        Ok(recording) => {
            let ambient = rms(&recording.samples);
            let detail = format!("ambient rms {ambient:.0}, speech threshold {threshold:.0}");
            if ambient >= threshold {
                warn(
                    "audio.level",
                    detail,
                    "Room tone counts as speech, so auto-stop may not fire. Raise the threshold.",
                )
            } else {
                ok("audio.level", detail)
            }
        }
    }
}

/// Whether the model is resident. Absent only means the next dictation pays
/// the load itself.
fn model_server() -> Check {
    let path = server::socket_path();
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        ok("model.server", format!("resident at {}", path.display()))
    } else {
        warn(
            "model.server",
            "not running",
            "Started after the next dictation; the one before it loads the model itself.",
        )
    }
}

fn config_file() -> Check {
    let path = match config::config_path() {
        Ok(path) => path,
        Err(err) => return fail("config.file", err.to_string(), "Set HERDR_CONFIG_PATH."),
    };
    match std::fs::read_to_string(&path) {
        Err(_) => warn(
            "config.file",
            format!("{} (absent)", path.display()),
            "Run `herdr-dictate setup`.",
        ),
        Ok(text) if !config::is_parseable(&text) => fail(
            "config.file",
            format!("{} is not valid TOML", path.display()),
            "Run `herdr config check`.",
        ),
        Ok(_) => ok("config.file", path.display().to_string()),
    }
}

fn bindings() -> Check {
    let text = config::config_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let missing = config::missing_bindings(&text);
    if missing.is_empty() {
        ok("config.bindings", "all bound")
    } else {
        let names: Vec<&str> = missing.iter().map(|b| b.action).collect();
        warn(
            "config.bindings",
            format!("unbound: {}", names.join(", ")),
            "Run `herdr-dictate setup`.",
        )
    }
}

fn plugin_dirs() -> Check {
    match std::env::var_os("HERDR_PLUGIN_STATE_DIR") {
        None => warn(
            "plugin.dirs",
            "HERDR_PLUGIN_STATE_DIR unset",
            "Set when Herdr runs the plugin; expected outside it.",
        ),
        Some(dir) => {
            let dir = Path::new(&dir);
            if writable(dir) {
                ok("plugin.dirs", dir.display().to_string())
            } else {
                fail(
                    "plugin.dirs",
                    format!("{} is not writable", dir.display()),
                    "Check its ownership.",
                )
            }
        }
    }
}

fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".doctor-write-probe");
    let wrote = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    wrote
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worst_status_wins() {
        assert_eq!(worst(&[]), Status::Ok);
        assert_eq!(worst(&[ok("a", "")]), Status::Ok);
        assert_eq!(worst(&[ok("a", ""), warn("b", "", "r")]), Status::Warn);
        assert_eq!(
            worst(&[warn("a", "", "r"), fail("b", "", "r")]),
            Status::Fail
        );
    }

    #[test]
    fn rendering_names_every_check_and_its_remedy() {
        let mut out = Vec::new();
        render(
            &[ok("a.one", "fine"), fail("b.two", "broken", "fix it")],
            &mut out,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("a.one"));
        assert!(text.contains("FAIL"));
        assert!(text.contains("-> fix it"));
    }

    #[test]
    fn a_passing_check_prints_no_remedy_arrow() {
        let mut out = Vec::new();
        render(&[ok("a.one", "fine")], &mut out).unwrap();
        assert!(!String::from_utf8(out).unwrap().contains("->"));
    }
}
