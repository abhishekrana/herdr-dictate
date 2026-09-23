//! Speech-to-text engines.
//!
//! One trait, so a backend other than the bundled one is a new implementation
//! rather than a change to anything that calls it.

pub mod bundled;

use std::io::Write;

use serde::Deserialize;

use crate::Result;
use crate::model::ModelRef;

pub trait Engine: Send {
    /// Transcribe 16 kHz mono samples.
    fn transcribe(&mut self, samples: &[i16]) -> Result<String>;

    /// One line naming the engine and model, for `doctor`.
    fn describe(&self) -> String;
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// whisper.cpp, compiled in.
    #[default]
    Bundled,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub kind: Kind,
    pub model: ModelRef,
    pub language: String,
    /// Text the model treats as preceding the clip, biasing its vocabulary.
    /// Listed terms come out right; terms absent from it are likelier to be
    /// mis-heard, so this cuts both ways and is best changed by measurement.
    pub prompt: Option<String>,
    /// Zero means one thread per core.
    pub threads: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            kind: Kind::default(),
            model: ModelRef::default(),
            language: "en".into(),
            prompt: None,
            threads: 0,
        }
    }
}

/// Build the configured engine, fetching the model if it is not cached.
pub fn build(config: &Config, progress: &mut impl Write) -> Result<Box<dyn Engine>> {
    match config.kind {
        Kind::Bundled => Ok(Box::new(bundled::Bundled::load(config, progress)?)),
    }
}

/// Drop what whisper narrates about silence rather than speech.
///
/// The set is open-ended (`[BLANK_AUDIO]`, `[SOUND]`, `[MUSIC]`), so the rule is
/// shape - square-bracketed, no lowercase - not a list of names. A clip with no
/// word outside brackets (`-`, `[typing]`, `(clicking)`) is noise and yields
/// nothing, and the dash whisper opens a speaker turn with is removed.
pub fn strip_non_speech(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let Some(len) = rest[start..].find(']') else {
            // An annotation cut off before its close, such as a lone `[`.
            if rest[start + 1..].chars().any(char::is_lowercase) {
                out.push_str(&rest[start..]);
            }
            rest = "";
            break;
        };
        let end = start + len + 1;
        let inner = &rest[start + 1..end - 1];
        let is_annotation = !inner.is_empty()
            && !inner.chars().any(char::is_lowercase)
            && inner.chars().any(char::is_alphabetic);
        if !is_annotation {
            out.push_str(&rest[start..end]);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    let text = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if !has_words(&text) {
        return String::new();
    }
    match text.split_once(' ') {
        Some((dash, words)) if !dash.is_empty() && dash.chars().all(|c| "-–—".contains(c)) => {
            words.to_owned()
        }
        _ => text,
    }
}

/// Whether any letter or digit sits outside brackets and parentheses.
fn has_words(text: &str) -> bool {
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            c if depth == 0 && c.is_alphanumeric() => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_are_dropped() {
        assert_eq!(strip_non_speech("[BLANK_AUDIO]"), "");
        assert_eq!(strip_non_speech("hello [SOUND] world"), "hello world");
        assert_eq!(strip_non_speech("[MUSIC PLAYING] go on"), "go on");
    }

    #[test]
    fn ordinary_bracketed_text_is_kept() {
        assert_eq!(strip_non_speech("see [the docs]"), "see [the docs]");
        assert_eq!(strip_non_speech("array[0] please"), "array[0] please");
    }

    #[test]
    fn whitespace_is_collapsed_not_mangled() {
        assert_eq!(strip_non_speech("  hello   world  "), "hello world");
        assert_eq!(strip_non_speech(""), "");
    }

    #[test]
    fn an_unclosed_bracket_is_left_alone() {
        assert_eq!(strip_non_speech("what [ is this"), "what [ is this");
    }

    #[test]
    fn a_cut_off_annotation_is_dropped() {
        assert_eq!(strip_non_speech(" ["), "");
        assert_eq!(strip_non_speech("go on [BLANK_AU"), "go on");
    }

    #[test]
    fn a_clip_with_no_words_is_noise() {
        for noise in ["-", " - ", "...", "[typing]", "(keyboard clicking)", "♪"] {
            assert_eq!(strip_non_speech(noise), "", "{noise:?}");
        }
    }

    #[test]
    fn the_speaker_turn_dash_is_removed() {
        assert_eq!(strip_non_speech(" - check the VMs"), "check the VMs");
        assert_eq!(strip_non_speech("– yes"), "yes");
    }

    #[test]
    fn a_dash_that_belongs_to_a_word_is_kept() {
        assert_eq!(strip_non_speech("--verbose please"), "--verbose please");
        assert_eq!(strip_non_speech("-5 degrees"), "-5 degrees");
        assert_eq!(strip_non_speech("well - maybe"), "well - maybe");
    }

    #[test]
    fn the_default_config_names_a_known_model() {
        let config = Config::default();
        assert_eq!(config.language, "en");
        assert!(config.model.resolve(std::path::Path::new("/cache")).is_ok());
    }

    #[test]
    fn config_parses_from_toml_and_rejects_typos() {
        let config: Config =
            toml::from_str("language = \"en\"\n[model]\nname = \"tiny.en\"\n").unwrap();
        assert_eq!(config.model.name.as_deref(), Some("tiny.en"));
        assert!(
            toml::from_str::<Config>("langauge = \"en\"").is_err(),
            "a misspelt key is refused"
        );
    }
}
