//! Cutting a recording into chunks at the speaker's pauses, so each one can be
//! transcribed and typed while the speaker carries on.
//!
//! The rule is Myna's: once `arm` of audio is uncommitted, the next pause of
//! `pause` closes a chunk at the end of that pause. A chunk that reaches
//! `ceiling` without one is cut at the middle of the longest shorter pause it
//! holds, so no single pass grows long enough to lose accuracy. Pure over
//! samples; the microphone and the model are elsewhere.

use std::ops::Range;
use std::time::Duration;

use crate::audio::{TARGET_RATE, rms};

/// The voice detector judges 30 ms at a time.
const FRAME: usize = (TARGET_RATE as usize) * 3 / 100;
/// The shortest silence the ceiling will cut inside.
const CEILING_PAUSE: usize = TARGET_RATE as usize / 5;
/// A ceiling cut lands at least this far into the chunk.
const CEILING_MIN: usize = TARGET_RATE as usize;
/// A tail shorter than this holds no word worth a pass.
const MIN_TAIL: usize = TARGET_RATE as usize / 20;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkConfig {
    /// Uncommitted audio before a pause may close a chunk.
    pub arm: Duration,
    /// Silence that closes a chunk once armed.
    pub pause: Duration,
    /// The longest chunk.
    pub ceiling: Duration,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            arm: Duration::from_secs(5),
            pause: Duration::from_millis(800),
            ceiling: Duration::from_secs(25),
        }
    }
}

fn samples(duration: Duration) -> usize {
    (duration.as_secs_f64() * TARGET_RATE as f64).round() as usize
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Voice {
    /// Nothing loud enough to call speech yet.
    NotStarted,
    Silent,
    Active,
}

/// Myna's adaptive voice detector, a port of Murmure's: a noise floor that
/// follows quiet frames, and speech and silence thresholds set against it.
/// Levels are RMS around the frame's mean, in full scale.
#[derive(Debug)]
struct Vad {
    floor: f64,
    smoothed: f64,
    started: bool,
    speech: f64,
}

impl Vad {
    fn new() -> Self {
        Self {
            floor: 0.003,
            smoothed: 0.0,
            started: false,
            speech: 0.0,
        }
    }

    fn update(&mut self, level: f64) -> Voice {
        if level < self.floor {
            self.floor = 0.2 * level + 0.8 * self.floor;
        } else if level <= self.floor.max(0.004 / 5.0) * 10.0 {
            self.floor = 0.005 * level + 0.995 * self.floor;
        }
        self.smoothed = 0.3 * level + 0.7 * self.smoothed;
        if self.smoothed > (self.floor * 5.0).clamp(0.004, 0.08) {
            self.started = true;
        }
        if !self.started {
            return Voice::NotStarted;
        }
        self.speech = self.smoothed.max(self.speech * 0.9995);
        self.floor = self.floor.min(self.speech * 0.05);
        if self.smoothed < (self.floor * 3.0).clamp(0.0024, 0.048) {
            Voice::Silent
        } else {
            Voice::Active
        }
    }
}

/// A silence of at least [`CEILING_PAUSE`], where the ceiling may cut.
#[derive(Clone, Copy, Debug)]
struct Pause {
    len: usize,
    mid: usize,
}

/// Decides where chunks close, from the samples fed to it in order. Every
/// position is a sample index from the start of the recording.
#[derive(Debug)]
pub struct Chunker {
    arm: usize,
    pause: usize,
    ceiling: usize,
    vad: Vad,
    /// Samples short of a whole frame, judged once the frame fills.
    partial: Vec<i16>,
    /// Samples judged so far.
    judged: usize,
    /// Where the open chunk starts.
    front: usize,
    /// Silence counted towards `pause` since the chunk armed.
    silent_run: usize,
    /// Whether the open chunk holds speech.
    heard: bool,
    /// Where the current silence began.
    silent_from: Option<usize>,
    pauses: Vec<Pause>,
}

impl Chunker {
    pub fn new(config: ChunkConfig) -> Self {
        Self {
            arm: samples(config.arm),
            pause: samples(config.pause),
            ceiling: samples(config.ceiling),
            vad: Vad::new(),
            partial: Vec::with_capacity(FRAME),
            judged: 0,
            front: 0,
            silent_run: 0,
            heard: false,
            silent_from: None,
            pauses: Vec::new(),
        }
    }

    /// Feed the next samples; returns the chunks they closed that hold speech.
    /// A chunk that closes on silence alone is skipped, never returned.
    pub fn push(&mut self, mut samples: &[i16]) -> Vec<Range<usize>> {
        let mut closed = Vec::new();
        while !samples.is_empty() {
            let take = (FRAME - self.partial.len()).min(samples.len());
            self.partial.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.partial.len() == FRAME {
                let level = rms(&self.partial) / 32_768.0;
                self.partial.clear();
                closed.extend(self.judge(level));
            }
        }
        closed
    }

    /// The audio after the last chunk, once the recording has ended.
    pub fn finish(self) -> Option<Range<usize>> {
        let end = self.judged + self.partial.len();
        (end - self.front > MIN_TAIL).then_some(self.front..end)
    }

    fn judge(&mut self, level: f64) -> Option<Range<usize>> {
        let start = self.judged;
        self.judged += FRAME;
        let end = self.judged;
        let voice = self.vad.update(level);
        self.heard |= voice == Voice::Active;

        if voice == Voice::Silent {
            self.silent_from.get_or_insert(start);
        } else if let Some(from) = self.silent_from.take()
            && start - from >= CEILING_PAUSE
        {
            self.pauses.push(Pause {
                len: start - from,
                mid: (from + start) / 2,
            });
        }

        if end - self.front >= self.ceiling {
            let cut = self
                .pauses
                .iter()
                .filter(|p| p.mid >= self.front + CEILING_MIN)
                .max_by_key(|p| (p.len, p.mid))
                .map_or(end, |p| p.mid);
            let chunk = self.front..cut;
            self.front = cut;
            self.silent_run = 0;
            // The rest of the cut chunk was taken mid-speech.
            self.heard = true;
            self.pauses.retain(|p| p.mid > cut);
            return Some(chunk);
        }
        if end - self.front < self.arm {
            return None;
        }
        match voice {
            Voice::Silent => {
                self.silent_run += FRAME;
                if self.silent_run >= self.pause {
                    let chunk = self.front..end;
                    let heard = self.heard;
                    self.front = end;
                    self.silent_run = 0;
                    self.heard = false;
                    self.pauses.clear();
                    return heard.then_some(chunk);
                }
            }
            Voice::Active => self.silent_run = 0,
            Voice::NotStarted => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = TARGET_RATE as f64;

    /// Audio described as (seconds, amplitude) spans: speech at 3,000 and room
    /// tone at 30, each a square wave around `offset`.
    fn audio(spans: &[(f64, i16)], offset: i16) -> Vec<i16> {
        spans
            .iter()
            .flat_map(|&(secs, amplitude)| {
                (0..(secs * RATE) as usize).map(move |i| {
                    let swing = if i % 2 == 0 { amplitude } else { -amplitude };
                    offset + swing
                })
            })
            .collect()
    }

    const SPEECH: i16 = 3_000;
    const ROOM: i16 = 30;

    fn secs(range: &Range<usize>) -> (f64, f64) {
        (range.start as f64 / RATE, range.end as f64 / RATE)
    }

    fn chunks(samples: &[i16]) -> (Vec<Range<usize>>, Option<Range<usize>>) {
        let mut chunker = Chunker::new(ChunkConfig::default());
        // Uneven pushes, as a device delivers them.
        let closed = samples
            .chunks(1_000)
            .flat_map(|frame| chunker.push(frame))
            .collect();
        (closed, chunker.finish())
    }

    /// The detector calls silence a few hundred milliseconds after the voice
    /// stops, so a pause cut lands that much after the pause length.
    fn near(actual: f64, expected: f64) -> bool {
        (expected..expected + 0.5).contains(&actual)
    }

    #[test]
    fn a_pause_before_the_arm_does_not_cut() {
        let samples = audio(&[(3.0, SPEECH), (1.5, ROOM), (1.0, SPEECH)], 0);
        let (closed, tail) = chunks(&samples);
        assert!(
            closed.is_empty(),
            "cut at {:?}",
            closed.iter().map(secs).collect::<Vec<_>>()
        );
        assert_eq!(tail, Some(0..samples.len()));
    }

    #[test]
    fn once_armed_a_pause_closes_the_chunk_at_its_end() {
        let samples = audio(&[(6.0, SPEECH), (1.5, ROOM), (2.0, SPEECH)], 0);
        let (closed, tail) = chunks(&samples);
        assert_eq!(closed.len(), 1);
        let (start, end) = secs(&closed[0]);
        assert_eq!(start, 0.0);
        assert!(near(end, 6.8), "cut at {end}");
        assert_eq!(tail, Some(closed[0].end..samples.len()));
    }

    #[test]
    fn a_pause_shorter_than_the_setting_does_not_cut() {
        let samples = audio(&[(6.0, SPEECH), (0.6, ROOM), (2.0, SPEECH)], 0);
        assert!(chunks(&samples).0.is_empty());
    }

    #[test]
    fn unbroken_speech_is_cut_at_the_ceiling() {
        let samples = audio(&[(30.0, SPEECH)], 0);
        let (closed, tail) = chunks(&samples);
        assert_eq!(closed.len(), 1);
        let (start, end) = secs(&closed[0]);
        assert_eq!(start, 0.0);
        assert!(
            (25.0..25.03).contains(&end),
            "the first frame past 25 s: {end}"
        );
        assert_eq!(tail, Some(closed[0].end..samples.len()));
    }

    #[test]
    fn the_ceiling_cuts_in_the_middle_of_the_longest_short_pause() {
        let mut spans = Vec::new();
        for pause in [0.6, 0.75, 0.6, 0.6] {
            spans.extend([(4.5, SPEECH), (pause, ROOM)]);
        }
        spans.push((10.0, SPEECH));
        let (closed, _) = chunks(&audio(&spans, 0));
        assert_eq!(closed.len(), 1);
        let (start, end) = secs(&closed[0]);
        assert_eq!(start, 0.0);
        // The 0.75 s pause runs from 9.6 s to 10.35 s; the detector hears its
        // silence late, so the middle it finds sits in the pause's second half.
        assert!((9.9..10.35).contains(&end), "cut at {end}");
    }

    #[test]
    fn a_chunk_of_silence_is_skipped_and_the_next_starts_after_it() {
        let samples = audio(
            &[
                (6.0, SPEECH),
                (1.5, ROOM),
                (10.0, ROOM),
                (2.0, SPEECH),
                (0.5, ROOM),
            ],
            0,
        );
        let (closed, tail) = chunks(&samples);
        assert_eq!(closed.len(), 1, "only the spoken chunk: {closed:?}");
        let tail = tail.unwrap();
        let (tail_start, _) = secs(&tail);
        assert!(
            tail_start > 12.0 && tail_start < 15.5,
            "the silent stretch was skipped up to {tail_start}"
        );
        assert_eq!(tail.end, samples.len());
    }

    #[test]
    fn a_dc_offset_does_not_move_the_cuts() {
        let spans = [
            (6.0, SPEECH),
            (1.5, ROOM),
            (7.0, SPEECH),
            (1.2, ROOM),
            (1.0, SPEECH),
        ];
        let level = chunks(&audio(&spans, 0));
        let offset = chunks(&audio(&spans, 1_029));
        assert_eq!(level.0.len(), 2);
        assert_eq!(offset, level);
    }

    #[test]
    fn a_tail_too_short_to_hold_a_word_is_dropped() {
        let mut chunker = Chunker::new(ChunkConfig::default());
        chunker.push(&audio(&[(0.04, SPEECH)], 0));
        assert_eq!(chunker.finish(), None);
    }
}
