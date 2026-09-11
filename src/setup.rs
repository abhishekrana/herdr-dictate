//! Show the keybindings this plugin needs, and offer to write them.

use std::io::{IsTerminal, Write};
use std::path::Path;

use crate::config;
use crate::ipc::Client;
use crate::{Error, PLUGIN_ID, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Ask before writing, when there is a terminal to ask at.
    Ask,
    Apply,
    /// Show the block, write nothing.
    Print,
}

pub fn run(mode: Mode, path: &Path, out: &mut impl Write) -> Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };

    writeln!(out, "config  {}", path.display())?;

    if !existing.is_empty() && !config::is_parseable(&existing) {
        writeln!(out, "\nNot valid TOML, so nothing was written.")?;
        writeln!(
            out,
            "Run `herdr config check`, fix it, then run setup again."
        )?;
        return Err(Error::ConfigUnparseable(path.to_path_buf()));
    }

    let missing = config::missing_bindings(&existing);
    if missing.is_empty() {
        writeln!(out, "\nEvery {PLUGIN_ID} action is already bound.")?;
        return Ok(());
    }

    let block = config::block(&missing);
    writeln!(out, "\nNot bound yet:\n")?;
    // The block leads with a blank line to separate it in the file, not on screen.
    for line in block.trim_start_matches('\n').lines() {
        writeln!(out, "  {line}")?;
    }

    if !should_write(mode, path, out)? {
        return Ok(());
    }

    append(path, &existing, &block)?;
    writeln!(out, "\nWrote {} binding(s):", missing.len())?;
    for binding in &missing {
        writeln!(out, "  {}  {}", binding.key, binding.description)?;
    }
    reload(out);
    Ok(())
}

fn should_write(mode: Mode, path: &Path, out: &mut impl Write) -> Result<bool> {
    match mode {
        Mode::Apply => Ok(true),
        Mode::Print => {
            writeln!(
                out,
                "Append them to {} yourself, or re-run with --apply.",
                path.display()
            )?;
            Ok(false)
        }
        Mode::Ask if !std::io::stdin().is_terminal() => {
            // A scripted run must not edit a config unasked.
            writeln!(out, "Not a terminal; nothing written. Re-run with --apply.")?;
            Ok(false)
        }
        Mode::Ask => {
            write!(out, "\nAppend them to {}? [y/N] ", path.display())?;
            out.flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            Ok(matches!(
                answer.trim().to_ascii_lowercase().as_str(),
                "y" | "yes"
            ))
        }
    }
}

/// Append the block, keeping a backup and replacing the file atomically.
///
/// Appends text rather than re-serialising: a round trip through a parsed
/// document drops the user's comments and ordering.
fn append(path: &Path, existing: &str, block: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !existing.is_empty() {
        std::fs::copy(path, path.with_extension("toml.bak"))?;
    }

    let mut updated = existing.to_owned();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(block);

    // A config Herdr cannot parse disables every setting in it.
    if !config::is_parseable(&updated) {
        return Err(Error::ConfigUnparseable(path.to_path_buf()));
    }

    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, updated.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Re-read the config so the keys work immediately. No server is not an error.
fn reload(out: &mut impl Write) {
    match Client::from_env().and_then(|c| c.reload_config()) {
        Ok(diagnostics) if diagnostics.is_empty() => {
            let _ = writeln!(out, "\nConfig reloaded.");
        }
        Ok(diagnostics) => {
            let _ = writeln!(out, "\nConfig reloaded, with diagnostics:");
            for d in diagnostics {
                let _ = writeln!(out, "  {d}");
            }
        }
        Err(err) => {
            let _ = writeln!(
                out,
                "\nNot reloaded ({err}). Run `herdr server reload-config`."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_into_a_missing_config_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        run(Mode::Apply, &path, &mut Vec::new()).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(config::is_parseable(&written));
        assert!(config::missing_bindings(&written).is_empty());

        let mut out = Vec::new();
        run(Mode::Apply, &path, &mut out).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), written);
        assert!(String::from_utf8_lossy(&out).contains("already bound"));
    }

    #[test]
    fn preserves_existing_content_and_leaves_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "# my notes\nonboarding = false\n\n[theme]\nname = \"solarized-light\"\n";
        std::fs::write(&path, original).unwrap();

        run(Mode::Apply, &path, &mut Vec::new()).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with(original));
        assert!(config::is_parseable(&written));
        assert_eq!(
            std::fs::read_to_string(path.with_extension("toml.bak")).unwrap(),
            original
        );
    }

    #[test]
    fn refuses_to_touch_an_unparseable_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let broken = "[[keys.command]\nkey =";
        std::fs::write(&path, broken).unwrap();

        assert!(run(Mode::Apply, &path, &mut Vec::new()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn print_mode_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut out = Vec::new();

        run(Mode::Print, &path, &mut out).unwrap();

        assert!(!path.exists());
        assert!(String::from_utf8_lossy(&out).contains("keys.command"));
    }

    #[test]
    fn a_config_without_a_trailing_newline_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "onboarding = false").unwrap();

        run(Mode::Apply, &path, &mut Vec::new()).unwrap();

        assert!(config::is_parseable(
            &std::fs::read_to_string(&path).unwrap()
        ));
    }
}
