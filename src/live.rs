//! Live typing: a dictation typed chunk by chunk while the speaker carries on.
//!
//! The recorder feeds every sample to a [`Live`]; each chunk the [`Chunker`]
//! closes goes to one worker thread that transcribes it and types it. One
//! worker, fed in order, is what keeps the words in the order they were
//! spoken. When the recording ends only the tail is left to transcribe.
//!
//! Whether a dictation types live is the config's `[chunking] enabled`, unless
//! `toggle-chunking` has overridden it in the state directory.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

use crate::chunk::{ChunkConfig, Chunker};
use crate::{Error, Result};

/// What became of a dictation's words once every chunk was handled.
#[derive(Debug, Default)]
pub struct Typed {
    /// Chunks that produced words.
    pub chunks: usize,
    /// Characters that reached the pane.
    pub chars: usize,
    /// Chunks that could not be transcribed, whose words are gone.
    pub lost: usize,
    /// The first failure. Once one happens nothing more is typed, so the pane
    /// never holds words with a gap in them.
    pub error: Option<Error>,
    /// Words that did not reach the pane, in the order spoken.
    pub undelivered: String,
}

impl Typed {
    fn take(&mut self, text: String, deliver: &mut impl FnMut(&str) -> Result<()>) {
        // Each chunk's text ends with its own mark, so one space joins them.
        let joined = if self.chunks == 0 {
            text.clone()
        } else {
            format!(" {text}")
        };
        self.chunks += 1;
        if self.error.is_none() {
            match deliver(&joined) {
                Ok(()) => {
                    self.chars += joined.len();
                    return;
                }
                Err(err) => self.error = Some(err),
            }
        }
        if !self.undelivered.is_empty() {
            self.undelivered.push(' ');
        }
        self.undelivered.push_str(&text);
    }
}

/// Transcribes and types each chunk as the recording closes it.
pub struct Live {
    chunker: Chunker,
    /// Samples handed to the chunker so far.
    fed: usize,
    chunks: mpsc::Sender<Vec<i16>>,
    worker: JoinHandle<Typed>,
}

impl Live {
    /// `transcribe` turns a chunk into words, empty for silence or noise;
    /// `deliver` types words into the pane.
    pub fn start(
        config: ChunkConfig,
        mut transcribe: impl FnMut(&[i16]) -> Result<String> + Send + 'static,
        mut deliver: impl FnMut(&str) -> Result<()> + Send + 'static,
    ) -> Self {
        let (chunks, queue) = mpsc::channel::<Vec<i16>>();
        let worker = std::thread::spawn(move || {
            let mut typed = Typed::default();
            for (index, samples) in queue.into_iter().enumerate() {
                let started = std::time::Instant::now();
                let text = match transcribe(&samples) {
                    Ok(text) => text,
                    Err(err) => {
                        tracing::warn!(chunk = index, %err, "chunk not transcribed");
                        typed.lost += 1;
                        typed.error.get_or_insert(err);
                        continue;
                    }
                };
                tracing::info!(
                    chunk = index,
                    secs = samples.len() as f64 / crate::audio::TARGET_RATE as f64,
                    transcribe_ms = started.elapsed().as_millis(),
                    chars = text.len(),
                    "chunk"
                );
                if !text.is_empty() {
                    typed.take(text, &mut deliver);
                }
            }
            typed
        });
        Self {
            chunker: Chunker::new(config),
            fed: 0,
            chunks,
            worker,
        }
    }

    /// Everything recorded so far; sends each chunk it closes to be typed.
    pub fn hear(&mut self, recorded: &[i16]) {
        let fresh = &recorded[self.fed..];
        self.fed = recorded.len();
        for range in self.chunker.push(fresh) {
            send(&self.chunks, recorded, range);
        }
    }

    /// Type the tail of the finished recording and wait for every chunk.
    pub fn finish(mut self, recorded: &[i16]) -> Typed {
        self.hear(recorded);
        let Self {
            chunker,
            chunks,
            worker,
            ..
        } = self;
        if let Some(range) = chunker.finish() {
            send(&chunks, recorded, range);
        }
        drop(chunks);
        worker.join().unwrap_or_else(|_| Typed {
            error: Some(Error::Model("the typing thread panicked".into())),
            ..Typed::default()
        })
    }
}

/// The worker only stops by panicking, which `finish` reports.
fn send(chunks: &mpsc::Sender<Vec<i16>>, recorded: &[i16], range: Range<usize>) {
    let _ = chunks.send(recorded[range].to_vec());
}

/// The runtime override `toggle-chunking` writes, beside the recording state.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Override {
    enabled: bool,
}

fn override_path() -> Result<PathBuf> {
    Ok(crate::session::state_path()?.with_file_name("chunking.json"))
}

/// Whether a dictation starting now types live.
pub fn enabled(configured: bool) -> bool {
    match override_path() {
        Ok(path) => enabled_at(&path, configured),
        Err(_) => configured,
    }
}

/// Flip live typing and return whether it is now on. Flipping back to what
/// the config says removes the override, so the config rules again.
pub fn toggle(configured: bool) -> Result<bool> {
    toggle_at(&override_path()?, configured)
}

/// Whether the current setting comes from `toggle-chunking` rather than the config.
pub fn overridden() -> bool {
    override_path().is_ok_and(|path| read_override(&path).is_some())
}

fn enabled_at(path: &Path, configured: bool) -> bool {
    read_override(path).map_or(configured, |o| o.enabled)
}

fn toggle_at(path: &Path, configured: bool) -> Result<bool> {
    let now = !enabled_at(path, configured);
    if now == configured {
        match std::fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err.into()),
            _ => {}
        }
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string(&Override { enabled: now })?)?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(now)
}

/// An unreadable override is no override.
fn read_override(path: &Path) -> Option<Override> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use crate::audio::TARGET_RATE;

    /// Speech and room tone as square waves, as the chunker's tests build them.
    fn audio(spans: &[(f64, i16)]) -> Vec<i16> {
        spans
            .iter()
            .flat_map(|&(secs, amplitude)| {
                (0..(secs * TARGET_RATE as f64) as usize)
                    .map(move |i| if i % 2 == 0 { amplitude } else { -amplitude })
            })
            .collect()
    }

    /// Three spoken stretches with armed pauses between them: two chunks and a tail.
    fn three_chunks() -> Vec<i16> {
        audio(&[
            (6.0, 3_000),
            (1.5, 30),
            (6.0, 3_000),
            (1.5, 30),
            (2.0, 3_000),
        ])
    }

    type Pane = Arc<Mutex<Vec<String>>>;

    fn pane() -> (Pane, impl FnMut(&str) -> Result<()> + Send + 'static) {
        let pane = Pane::default();
        let typed = Arc::clone(&pane);
        (pane, move |text: &str| {
            typed.lock().unwrap().push(text.to_owned());
            Ok(())
        })
    }

    /// Names each chunk by its order, so the pane shows what arrived when.
    fn numbering() -> impl FnMut(&[i16]) -> Result<String> + Send + 'static {
        let mut n = 0;
        move |_| {
            n += 1;
            Ok(format!("Chunk {n}."))
        }
    }

    /// Feed a recording the way capture does, a device buffer at a time.
    fn dictate(live: &mut Live, samples: &[i16]) {
        for end in (1_024..samples.len()).step_by(1_024) {
            live.hear(&samples[..end]);
        }
    }

    #[test]
    fn chunks_are_typed_while_recording_and_the_tail_at_the_end() {
        let samples = three_chunks();
        let (pane, deliver) = pane();
        let mut live = Live::start(ChunkConfig::default(), numbering(), deliver);
        dictate(&mut live, &samples);
        // Both chunks closed before the recording ended.
        for _ in 0..100 {
            if pane.lock().unwrap().len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(*pane.lock().unwrap(), ["Chunk 1.", " Chunk 2."]);

        let typed = live.finish(&samples);
        assert_eq!(
            *pane.lock().unwrap(),
            ["Chunk 1.", " Chunk 2.", " Chunk 3."]
        );
        assert!(typed.error.is_none());
        assert_eq!(typed.chunks, 3);
    }

    #[test]
    fn a_slow_chunk_still_lands_before_the_next() {
        let samples = three_chunks();
        let (pane, deliver) = pane();
        let mut n = 0;
        let slow_first = move |_: &[i16]| {
            n += 1;
            if n == 1 {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Ok(format!("Chunk {n}."))
        };
        let mut live = Live::start(ChunkConfig::default(), slow_first, deliver);
        dictate(&mut live, &samples);
        live.finish(&samples);
        assert_eq!(
            *pane.lock().unwrap(),
            ["Chunk 1.", " Chunk 2.", " Chunk 3."]
        );
    }

    #[test]
    fn a_chunk_heard_as_nothing_types_nothing_and_adds_no_space() {
        let samples = three_chunks();
        let (pane, deliver) = pane();
        let mut n = 0;
        let silent_second = move |_: &[i16]| {
            n += 1;
            Ok(if n == 2 {
                String::new()
            } else {
                format!("Chunk {n}.")
            })
        };
        let mut live = Live::start(ChunkConfig::default(), silent_second, deliver);
        dictate(&mut live, &samples);
        live.finish(&samples);
        assert_eq!(*pane.lock().unwrap(), ["Chunk 1.", " Chunk 3."]);
    }

    #[test]
    fn after_a_failed_delivery_the_rest_is_kept_not_typed() {
        let samples = three_chunks();
        let pane = Pane::default();
        let typed = Arc::clone(&pane);
        let mut calls = 0;
        let fails_second = move |text: &str| {
            calls += 1;
            if calls == 2 {
                return Err(Error::NoFocusedPane);
            }
            typed.lock().unwrap().push(text.to_owned());
            Ok(())
        };
        let mut live = Live::start(ChunkConfig::default(), numbering(), fails_second);
        dictate(&mut live, &samples);
        let typed = live.finish(&samples);
        assert_eq!(*pane.lock().unwrap(), ["Chunk 1."]);
        assert_eq!(typed.undelivered, "Chunk 2. Chunk 3.");
        assert!(matches!(typed.error, Some(Error::NoFocusedPane)));
    }

    #[test]
    fn a_chunk_that_cannot_be_transcribed_stops_typing_and_is_reported() {
        let samples = three_chunks();
        let (pane, deliver) = pane();
        let mut n = 0;
        let fails_second = move |_: &[i16]| {
            n += 1;
            if n == 2 {
                return Err(Error::Model("gone".into()));
            }
            Ok(format!("Chunk {n}."))
        };
        let mut live = Live::start(ChunkConfig::default(), fails_second, deliver);
        dictate(&mut live, &samples);
        let typed = live.finish(&samples);
        assert_eq!(*pane.lock().unwrap(), ["Chunk 1."]);
        assert_eq!(typed.undelivered, "Chunk 3.");
        assert_eq!(typed.lost, 1, "the gap is counted, not hidden");
        assert!(matches!(typed.error, Some(Error::Model(_))));
    }

    #[test]
    fn a_short_dictation_is_one_pass_over_all_of_it() {
        let samples = audio(&[(3.0, 3_000), (0.5, 30)]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let lengths = Arc::clone(&seen);
        let transcribe = move |chunk: &[i16]| {
            lengths.lock().unwrap().push(chunk.len());
            Ok("Hello.".to_owned())
        };
        let (pane, deliver) = pane();
        let mut live = Live::start(ChunkConfig::default(), transcribe, deliver);
        dictate(&mut live, &samples);
        live.finish(&samples);
        assert_eq!(*seen.lock().unwrap(), [samples.len()]);
        assert_eq!(*pane.lock().unwrap(), ["Hello."]);
    }

    #[test]
    fn the_config_rules_until_toggled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunking.json");
        assert!(enabled_at(&path, true));
        assert!(!enabled_at(&path, false));
    }

    #[test]
    fn toggling_overrides_the_config_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/chunking.json");
        assert!(!toggle_at(&path, true).unwrap());
        assert!(!enabled_at(&path, true), "read back as a new process would");
        assert!(path.exists());
    }

    #[test]
    fn toggling_back_to_the_config_removes_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunking.json");
        toggle_at(&path, true).unwrap();
        assert!(toggle_at(&path, true).unwrap());
        assert!(!path.exists(), "an edit to the config takes effect again");
        // With the override gone, changing the config is what counts.
        assert!(!enabled_at(&path, false));
    }

    #[test]
    fn toggling_turns_on_what_the_config_turns_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunking.json");
        assert!(toggle_at(&path, false).unwrap());
        assert!(enabled_at(&path, false));
    }

    #[test]
    fn an_unreadable_override_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunking.json");
        std::fs::write(&path, "{\"enabled\":").unwrap();
        assert!(enabled_at(&path, true));
    }
}
