//! whisper.cpp, compiled in.

use std::io::Write;
use std::sync::Once;

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
    convert_integer_to_float_audio,
};

use super::{Config, Engine};
use crate::{Error, Result, model};

/// whisper.cpp writes to stderr unless its log is redirected, and plugin stderr
/// is what Herdr shows the user.
static LOGGING: Once = Once::new();

pub struct Bundled {
    context: WhisperContext,
    model: String,
    language: String,
    prompt: Option<String>,
    threads: i32,
}

impl Bundled {
    pub fn load(config: &Config, progress: &mut impl Write) -> Result<Self> {
        LOGGING.call_once(whisper_rs::install_logging_hooks);

        let source = config.model.resolve(&model::cache_dir()?)?;
        let path = model::ensure(&source, progress)?;
        let context = WhisperContext::new_with_params(&path, WhisperContextParameters::default())
            .map_err(|e| Error::Model(format!("loading {}: {e}", path.display())))?;

        Ok(Self {
            context,
            model: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            language: config.language.clone(),
            prompt: config.prompt.clone(),
            threads: threads(config.threads),
        })
    }
}

fn threads(configured: u16) -> i32 {
    if configured > 0 {
        return configured.into();
    }
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
}

impl Engine for Bundled {
    fn transcribe(&mut self, samples: &[i16]) -> Result<String> {
        if samples.is_empty() {
            return Ok(String::new());
        }
        let mut audio = vec![0.0f32; samples.len()];
        convert_integer_to_float_audio(samples, &mut audio)
            .map_err(|e| Error::Model(e.to_string()))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(&self.language));
        params.set_translate(false);
        params.set_n_threads(self.threads);
        params.set_suppress_blank(true);
        // Each dictation stands alone; carrying context across them lets one
        // clip's words bias the next.
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(prompt) = &self.prompt {
            params.set_initial_prompt(prompt);
        }

        let mut state = self
            .context
            .create_state()
            .map_err(|e| Error::Model(e.to_string()))?;
        state
            .full(params, &audio)
            .map_err(|e| Error::Model(e.to_string()))?;

        let mut text = String::new();
        for i in 0..state.full_n_segments() {
            if let Some(segment) = state.get_segment(i) {
                if let Ok(chunk) = segment.to_str_lossy() {
                    text.push_str(&chunk);
                }
            }
        }
        Ok(super::strip_non_speech(&text))
    }

    fn describe(&self) -> String {
        format!(
            "bundled whisper.cpp, model {}, {} threads",
            self.model, self.threads
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_threads_means_one_per_core() {
        assert!(threads(0) >= 1);
        assert_eq!(threads(3), 3);
    }
}
