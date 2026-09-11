//! Diagnostics: one named check per thing that can break, each with a remedy.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::audio::{SilenceConfig, rms};
use crate::context::Context;
use crate::ipc::Client;
use crate::{PLUGIN_ID, capture, config};

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
        config_file(),
        bindings(),
        plugin_dirs(),
    ]
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
