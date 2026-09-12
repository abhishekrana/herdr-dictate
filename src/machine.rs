//! The saved SSH machine the sidebar has selected.
//!
//! Exactly one function here touches the operating system: [`selected`].
//! Parsing is a pure function, so the shapes Herdr can report are testable
//! without a running server.

use std::process::Command;

use serde::Deserialize;

use crate::{Error, Result};

/// One entry of `herdr machine list --json`.
///
/// Unknown fields are tolerated: Herdr may add some, and a dictation must not
/// start failing because it did.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Machine {
    pub id: String,
    pub label: String,
    /// The ssh destination, as saved in the profile.
    pub target: String,
    /// Which Herdr session on that machine the profile addresses.
    pub session: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub selected: bool,
}

/// The local Herdr binary. Herdr injects its own path into a plugin process;
/// the configured value wins so a broken environment can be worked around.
pub fn herdr_bin(configured: &str) -> String {
    if !configured.is_empty() {
        return configured.to_string();
    }
    std::env::var("HERDR_BIN_PATH")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "herdr".into())
}

/// The machine the sidebar is pointing at, or `None` for the local server.
pub fn selected(herdr: &str) -> Result<Option<Machine>> {
    let output = Command::new(herdr)
        .args(["machine", "list", "--json"])
        .output()
        .map_err(|err| Error::Remote {
            label: "local".into(),
            message: format!("cannot run {herdr}: {err}"),
        })?;
    if !output.status.success() {
        return Err(Error::Remote {
            label: "local".into(),
            message: format!(
                "{herdr} machine list failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

/// The pure half of [`selected`].
pub fn parse(json: &str) -> Result<Option<Machine>> {
    let machines: Vec<Machine> = serde_json::from_str(json.trim())?;
    Ok(machines.into_iter().find(|m| m.selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = r#"[{"id":"7339","label":"desk","target":"desk",
        "session":"default","enabled":true,"selected":true}]"#;

    #[test]
    fn nothing_selected_means_the_local_server() {
        let json = ONE.replace("\"selected\":true", "\"selected\":false");
        assert_eq!(parse(&json).unwrap(), None);
    }

    #[test]
    fn an_empty_catalogue_means_the_local_server() {
        assert_eq!(parse("[]").unwrap(), None);
    }

    #[test]
    fn the_selected_machine_carries_its_target_and_session() {
        let machine = parse(ONE).unwrap().expect("selected");
        assert_eq!(machine.target, "desk");
        assert_eq!(machine.session, "default");
        assert_eq!(machine.id, "7339");
        assert!(machine.enabled);
    }

    #[test]
    fn a_disabled_machine_still_reports_as_selected() {
        // Whether to refuse it is the caller's call, not the parser's.
        let json = ONE.replace("\"enabled\":true", "\"enabled\":false");
        let machine = parse(&json).unwrap().expect("selected");
        assert!(!machine.enabled);
    }

    #[test]
    fn fields_herdr_adds_later_do_not_break_parsing() {
        let json = ONE.replace("\"selected\":true", "\"selected\":true,\"future\":42");
        assert!(parse(&json).unwrap().is_some());
    }

    #[test]
    fn the_first_selected_entry_wins() {
        let entry = &ONE[1..ONE.len() - 1];
        let first = entry.replace("7339", "a");
        let second = entry.replace("7339", "b");
        let json = format!("[{first},{second}]");
        assert_eq!(parse(&json).unwrap().unwrap().id, "a");
    }

    #[test]
    fn output_that_is_not_a_catalogue_is_an_error() {
        assert!(parse("not json").is_err());
    }

    #[test]
    fn the_configured_binary_wins_over_the_environment() {
        assert_eq!(herdr_bin("/opt/herdr"), "/opt/herdr");
    }
}
