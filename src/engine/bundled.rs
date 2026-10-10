//! transcribe.cpp, compiled in. The model file decides the family: Parakeet,
//! Whisper, or any other family transcribe.cpp runs.

use std::io::Write;
use std::sync::Once;

use transcribe_cpp::{Model, RunExtension, RunOptions, Session, SessionOptions, WhisperRunOptions};

use super::{Config, Engine};
use crate::{Error, Result, model};

/// Native and ggml diagnostics go through the `log` facade, which the binary's
/// subscriber filters, rather than straight to stderr.
static LOGGING: Once = Once::new();

/// Fixed so the sampled retry answers the same way for the same clip.
const RETRY_SEED: u32 = 1;

/// Whisper's no-speech probability above which a clip is noise.
const NO_SPEECH: f32 = 0.3;

pub struct Bundled {
    session: Session,
    family: Family,
    language: String,
    description: String,
}

/// What a model's family needs around a decode.
enum Family {
    /// A prompt, a no-speech gate, and a retry when a clip collapses to a
    /// wordless mark.
    Whisper(WhisperRunOptions),
    /// Parakeet and every other family: the text as decoded.
    Plain,
}

impl Bundled {
    pub fn load(config: &Config, progress: &mut impl Write) -> Result<Self> {
        LOGGING.call_once(transcribe_cpp::init_logging);

        let source = config.model.resolve(&model::cache_dir()?)?;
        let path = model::ensure(&source, progress)?;
        // The default backend request takes the best device present and falls
        // back to the CPU.
        let loaded = Model::load(&path)
            .map_err(|e| Error::Model(format!("loading {}: {e}", path.display())))?;
        let session = loaded
            .session_with(&SessionOptions {
                n_threads: config.threads.into(),
                ..SessionOptions::default()
            })
            .map_err(|e| Error::Model(e.to_string()))?;

        let arch = loaded.arch();
        let family = if arch == "whisper" {
            Family::Whisper(whisper_options(config.prompt.clone()))
        } else {
            if config.prompt.is_some() {
                tracing::warn!(arch, "the prompt is unused: only a whisper model takes one");
            }
            Family::Plain
        };
        let threads = match config.threads {
            0 => "default".to_owned(),
            n => n.to_string(),
        };
        let description = format!(
            "bundled transcribe.cpp, {arch} model {}, {} backend, {threads} threads",
            path.file_name().unwrap_or_default().to_string_lossy(),
            loaded.backend(),
        );

        Ok(Self {
            session,
            family,
            language: config.language.clone(),
            description,
        })
    }

    /// One pass over `audio`, returning its raw text.
    fn decode(&mut self, audio: &[f32], whisper: Option<WhisperRunOptions>) -> Result<String> {
        let options = RunOptions {
            language: Some(self.language.clone()),
            family: whisper.map(RunExtension::Whisper),
            ..RunOptions::default()
        };
        match self.session.run(audio, &options) {
            Ok(transcript) => Ok(transcript.text),
            // A decode cut short at its budget or by repetition keeps what it
            // heard before that, which beats losing the dictation.
            Err(err) if err.partial().is_some() => {
                tracing::warn!(%err, "decode stopped early; keeping its partial text");
                Ok(err.partial().map(|t| t.text.clone()).unwrap_or_default())
            }
            Err(err) => Err(Error::Model(err.to_string())),
        }
    }

    /// Whisper's pass, and its retry when the first pass claimed speech but
    /// carried no words.
    fn transcribe_whisper(&mut self, audio: &[f32], first: WhisperRunOptions) -> Result<String> {
        let text = self.decode(audio, Some(first.clone()))?;
        tracing::debug!(raw = ?text, "whisper");
        let words = super::strip_non_speech(&text);
        if !words.is_empty() {
            return Ok(words);
        }
        // Nothing in the first pass survived as words, so it is logged by
        // default; a retry is logged by length, since it may be speech.
        if !super::claims_speech(&text) {
            tracing::info!(raw = ?text, "whisper heard no words");
            return Ok(words);
        }

        // Greedy decoding can confidently collapse a clip of speech into a
        // lone `-`, past whisper's fallback thresholds; a seeded sampled pass
        // decodes it afresh. Only then, because on silence sampling invents
        // words.
        let retry = WhisperRunOptions {
            temperature: Some(0.2),
            seed: Some(RETRY_SEED),
            ..first
        };
        let retried = super::strip_non_speech(&self.decode(audio, Some(retry))?);
        tracing::info!(raw = ?text, chars = retried.len(), "whisper heard no words; retried");
        Ok(retried)
    }
}

/// Whisper's first pass.
fn whisper_options(prompt: Option<String>) -> WhisperRunOptions {
    WhisperRunOptions {
        initial_prompt: prompt,
        // Each dictation stands alone; carrying context across chunks lets
        // one stretch's words bias the next.
        condition_on_prev_tokens: Some(false),
        // Dictation wants the first answer, not a ladder of retries at rising
        // temperatures.
        temperature_inc: Some(0.0),
        // transcribe.cpp suppresses the labels whisper gives silence and
        // noise, so it answers them with stock phrases ("Thank you.") instead.
        // The no-speech probability alone gates them: measured noise scores
        // above 0.4 and short commands below 0.15. Every average
        // log-probability is below zero, so a zero threshold leaves the gate
        // to the no-speech probability.
        no_speech_thold: Some(NO_SPEECH),
        logprob_thold: Some(0.0),
        ..WhisperRunOptions::default()
    }
}

impl Engine for Bundled {
    fn transcribe(&mut self, samples: &[i16]) -> Result<String> {
        if samples.is_empty() {
            return Ok(String::new());
        }
        let audio: Vec<f32> = samples.iter().map(|&s| f32::from(s) / 32768.0).collect();

        let text = match &self.family {
            Family::Whisper(options) => {
                let options = options.clone();
                self.transcribe_whisper(&audio, options)?
            }
            // Collapsing whitespace also keeps a dictated newline from
            // submitting a prompt.
            Family::Plain => self
                .decode(&audio, None)?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        };
        Ok(text)
    }

    fn describe(&self) -> String {
        self.description.clone()
    }
}
