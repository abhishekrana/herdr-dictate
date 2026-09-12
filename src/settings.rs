//! The plugin's own config file, in `HERDR_PLUGIN_CONFIG_DIR`.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::audio::SilenceConfig;
use crate::engine;
use crate::{Error, PLUGIN_ID, Result};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub engine: engine::Config,
    pub silence: Silence,
    pub server: Server,
    pub status: Status,
}

/// What the tab bar chip reads in each state, printed verbatim. A tab bar
/// segment carries no style, so the chip cannot be coloured.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Status {
    pub idle: String,
    pub recording: String,
    pub transcribing: String,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            idle: "\u{25cb} dictate".into(),
            recording: "\u{25cf} dictate".into(),
            transcribing: "\u{25cc} dictate".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    /// Keep the model resident between dictations.
    pub enabled: bool,
    /// Seconds of disuse before the server exits and frees the model. Zero
    /// keeps it forever.
    pub idle_secs: u64,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_secs: 300,
        }
    }
}

impl Server {
    pub fn idle(&self) -> Duration {
        Duration::from_secs(self.idle_secs)
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Silence {
    /// RMS above which a frame counts as speech.
    pub threshold: f64,
    /// Trailing silence that ends a recording. Zero disables auto-stop.
    pub trailing_secs: f64,
    pub max_secs: f64,
}

impl Default for Silence {
    fn default() -> Self {
        let base = SilenceConfig::default();
        Self {
            threshold: base.threshold,
            trailing_secs: base.trailing.as_secs_f64(),
            max_secs: base.max_duration.as_secs_f64(),
        }
    }
}

impl From<Silence> for SilenceConfig {
    fn from(value: Silence) -> Self {
        Self {
            threshold: value.threshold,
            trailing: Duration::from_secs_f64(value.trailing_secs.max(0.0)),
            max_duration: Duration::from_secs_f64(value.max_secs.max(1.0)),
        }
    }
}

/// Herdr sets the config directory only for a plugin process; the chip runs as
/// a plain command, so the fallback is the path Herdr would have given.
pub fn path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("config.toml"));
    }
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".config/herdr/plugins/config")
            .join(PLUGIN_ID)
            .join("config.toml"),
    )
}

impl Settings {
    /// Load the config file, or defaults when there is none.
    pub fn load() -> Result<Self> {
        let Some(path) = path() else {
            return Ok(Self::default());
        };
        match std::fs::read_to_string(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
            Ok(text) => {
                toml::from_str(&text).map_err(|e| Error::Model(format!("{}: {e}", path.display())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_all_defaults() {
        let settings: Settings = toml::from_str("").unwrap();
        assert_eq!(settings.engine.language, "en");
        assert_eq!(
            settings.silence.threshold,
            SilenceConfig::default().threshold
        );
    }

    #[test]
    fn status_text_defaults_to_a_monochrome_glyph_and_the_label() {
        let settings: Settings = toml::from_str("").unwrap();
        assert_eq!(settings.status.idle, "\u{25cb} dictate");
        assert_eq!(settings.status.recording, "\u{25cf} dictate");
        assert_eq!(settings.status.transcribing, "\u{25cc} dictate");
    }

    #[test]
    fn one_status_state_leaves_the_others_alone() {
        let settings: Settings = toml::from_str("[status]\nrecording = \"rec\"\n").unwrap();
        assert_eq!(settings.status.recording, "rec");
        assert_eq!(settings.status.idle, Status::default().idle);
        assert_eq!(settings.status.transcribing, Status::default().transcribing);
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let settings: Settings = toml::from_str("[silence]\nthreshold = 900.0\n").unwrap();
        assert_eq!(settings.silence.threshold, 900.0);
        assert_eq!(settings.silence.max_secs, Silence::default().max_secs);
        assert_eq!(settings.engine.language, "en");
    }

    #[test]
    fn a_misspelt_key_is_refused_rather_than_ignored() {
        assert!(toml::from_str::<Settings>("[silence]\nthreshhold = 900.0\n").is_err());
    }

    #[test]
    fn durations_convert_and_are_clamped_sane() {
        let config: SilenceConfig = Silence {
            threshold: 1.0,
            trailing_secs: -5.0,
            max_secs: 0.0,
        }
        .into();
        assert_eq!(
            config.trailing,
            Duration::ZERO,
            "negative trailing means no auto-stop"
        );
        assert!(
            config.max_duration >= Duration::from_secs(1),
            "a zero cap would end every recording at once"
        );
    }

    #[test]
    fn the_server_defaults_to_on_with_an_idle_timeout() {
        let settings = Settings::default();
        assert!(settings.server.enabled);
        assert_eq!(settings.server.idle(), Duration::from_secs(300));
    }

    #[test]
    fn the_server_can_be_turned_off() {
        let settings: Settings = toml::from_str("[server]\nenabled = false\n").unwrap();
        assert!(!settings.server.enabled);
        assert_eq!(settings.server.idle_secs, 300, "other defaults survive");
    }

    #[test]
    fn a_model_can_be_named_in_the_file() {
        let settings: Settings = toml::from_str("[engine.model]\nname = \"tiny.en\"\n").unwrap();
        assert_eq!(settings.engine.model.name.as_deref(), Some("tiny.en"));
    }
}
