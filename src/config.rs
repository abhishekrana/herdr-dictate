//! The user's Herdr config file and the keybindings this plugin needs in it.
//!
//! Herdr plugins cannot register keys; a plugin declares actions and the user
//! binds them. `setup` exists to make that one step easy.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::{PLUGIN_ID, Result};

/// A keybinding this plugin suggests.
pub struct Binding {
    pub key: &'static str,
    pub action: &'static str,
    pub description: &'static str,
}

/// The bindings `setup` offers. Single source of truth for suggested keys.
pub const BINDINGS: &[Binding] = &[
    Binding {
        key: "prefix+v",
        action: "toggle",
        description: "dictate: toggle",
    },
    Binding {
        key: "prefix+shift+v",
        action: "toggle-send",
        description: "dictate: toggle and submit",
    },
];

impl Binding {
    /// The name Herdr resolves: `plugin.id.action`.
    pub fn qualified(&self) -> String {
        format!("{PLUGIN_ID}.{}", self.action)
    }

    fn to_toml(&self) -> String {
        format!(
            "[[keys.command]]\nkey = \"{}\"\ntype = \"plugin_action\"\ncommand = \"{}\"\ndescription = \"{}\"\n",
            self.key,
            self.qualified(),
            self.description
        )
    }
}

/// The config file Herdr reads.
pub fn config_path() -> Result<PathBuf> {
    resolve_config_path(
        std::env::var_os("HERDR_CONFIG_PATH"),
        std::env::var_os("HOME"),
    )
}

/// Herdr documents one location for Linux and macOS alike, so no XDG lookup.
fn resolve_config_path(override_path: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    if let Some(path) = override_path.filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = home.filter(|v| !v.is_empty()).ok_or_else(|| {
        std::io::Error::other("HOME is unset, so the Herdr config cannot be located")
    })?;
    Ok(PathBuf::from(home).join(".config/herdr/config.toml"))
}

/// Wanted bindings this config does not already bind.
///
/// Matched by action, not key, so a user who rebound one is not offered a duplicate.
pub fn missing_bindings(config: &str) -> Vec<&'static Binding> {
    let bound = bound_actions(config);
    BINDINGS
        .iter()
        .filter(|b| !bound.contains(&b.qualified()))
        .collect()
}

fn bound_actions(config: &str) -> Vec<String> {
    let Ok(parsed) = config.parse::<toml::Table>() else {
        // Report everything bound so an unparseable config is never appended to.
        return BINDINGS.iter().map(Binding::qualified).collect();
    };
    parsed
        .get("keys")
        .and_then(|keys| keys.get("command"))
        .and_then(toml::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e.get("command").and_then(toml::Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn is_parseable(config: &str) -> bool {
    config.parse::<toml::Table>().is_ok()
}

/// The TOML block to append for these bindings.
pub fn block(bindings: &[&Binding]) -> String {
    let mut out = format!("\n# Added by {PLUGIN_ID} setup.\n");
    for (i, binding) in bindings.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = write!(out, "{}", binding.to_toml());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_config_is_missing_everything() {
        assert_eq!(missing_bindings("").len(), BINDINGS.len());
    }

    #[test]
    fn a_rebound_action_is_not_offered_again() {
        let config = format!(
            "[[keys.command]]\nkey = \"prefix+z\"\ntype = \"plugin_action\"\ncommand = \"{PLUGIN_ID}.toggle\"\n"
        );
        let missing = missing_bindings(&config);
        assert!(!missing.iter().any(|b| b.action == "toggle"));
        assert!(missing.iter().any(|b| b.action == "toggle-send"));
    }

    #[test]
    fn another_plugins_bindings_are_ignored() {
        let config = "[[keys.command]]\nkey = \"prefix+v\"\ntype = \"plugin_action\"\ncommand = \"someone.else.toggle\"\n";
        assert_eq!(missing_bindings(config).len(), BINDINGS.len());
    }

    #[test]
    fn a_broken_config_offers_nothing() {
        assert!(!is_parseable("[[keys.command]\nkey ="));
        assert!(missing_bindings("[[keys.command]\nkey =").is_empty());
    }

    #[test]
    fn the_generated_block_satisfies_what_it_claims() {
        let generated = block(&missing_bindings(""));
        assert!(is_parseable(&generated));
        assert!(missing_bindings(&generated).is_empty());
    }

    #[test]
    fn appending_to_an_existing_config_still_parses() {
        let existing = "onboarding = false\n\n[theme]\nname = \"solarized-light\"\n";
        let combined = format!("{existing}{}", block(&missing_bindings(existing)));
        assert!(is_parseable(&combined));
    }

    #[test]
    fn the_override_wins_over_home() {
        let path = resolve_config_path(Some("/tmp/elsewhere.toml".into()), Some("/home/u".into()))
            .unwrap();
        assert_eq!(path, PathBuf::from("/tmp/elsewhere.toml"));
    }

    #[test]
    fn without_an_override_it_is_under_home() {
        let path = resolve_config_path(None, Some("/home/u".into())).unwrap();
        assert_eq!(path, PathBuf::from("/home/u/.config/herdr/config.toml"));
    }

    #[test]
    fn an_empty_override_falls_back() {
        let path = resolve_config_path(Some("".into()), Some("/home/u".into())).unwrap();
        assert_eq!(path, PathBuf::from("/home/u/.config/herdr/config.toml"));
    }

    #[test]
    fn no_home_is_an_error() {
        assert!(resolve_config_path(None, None).is_err());
    }
}
