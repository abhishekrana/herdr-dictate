//! The invocation context Herdr hands every plugin command.
//!
//! `focused_pane_id` is the pane the user is looking at, decided by Herdr
//! itself. It is taken as given and never recomputed: Herdr knows the answer,
//! and deriving one from the agent list disagrees with it as soon as two agents
//! are live in the same place.

use serde::Deserialize;

use crate::{Error, Result};

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Context {
    pub workspace_id: Option<String>,
    pub workspace_label: Option<String>,
    pub tab_id: Option<String>,
    pub focused_pane_id: Option<String>,
    pub focused_pane_cwd: Option<String>,
    pub focused_pane_status: Option<String>,
    pub invocation_source: Option<String>,
}

impl Context {
    /// Parse `HERDR_PLUGIN_CONTEXT_JSON`. Absent or empty is not an error: a
    /// global hotkey reaches this plugin with no context at all.
    pub fn from_env() -> Result<Self> {
        match std::env::var("HERDR_PLUGIN_CONTEXT_JSON") {
            Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw).map_err(Error::Context),
            _ => Ok(Self::default()),
        }
    }

    /// The pane to dictate into: the one Herdr named, else `HERDR_PANE_ID`.
    /// A caller with neither asks the server instead.
    pub fn target_pane(&self) -> Option<String> {
        self.focused_pane_id
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("HERDR_PANE_ID")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_context_pane() {
        let ctx: Context = serde_json::from_str(r#"{"focused_pane_id":"w1:p2"}"#).unwrap();
        assert_eq!(ctx.target_pane().as_deref(), Some("w1:p2"));
    }

    #[test]
    fn an_empty_pane_id_is_no_pane() {
        let ctx: Context = serde_json::from_str(r#"{"focused_pane_id":""}"#).unwrap();
        // With no HERDR_PANE_ID set in the test environment this resolves to None.
        assert!(ctx.target_pane().is_none() || std::env::var("HERDR_PANE_ID").is_ok());
    }

    #[test]
    fn unknown_fields_do_not_break_parsing() {
        let ctx: Context =
            serde_json::from_str(r#"{"focused_pane_id":"w1:p1","future_field":42}"#).unwrap();
        assert_eq!(ctx.focused_pane_id.as_deref(), Some("w1:p1"));
    }
}
