//! Sample conversion and silence detection.
//!
//! Pure functions over buffers: the microphone is a platform boundary and lives
//! in `capture`, so everything decided about the audio is testable here.

use std::time::Duration;

/// Whisper expects 16 kHz mono.
pub const TARGET_RATE: u32 = 16_000;

/// Average interleaved channels into mono `i16`.
pub fn to_mono_i16(samples: &[f32], channels: u16) -> Vec<i16> {
    let channels = channels.max(1) as usize;
    samples
        .chunks_exact(channels)
        .map(|frame| {
            let mean = frame.iter().sum::<f32>() / channels as f32;
            (mean.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
        })
        .collect()
}

/// Root mean square of a buffer, in `i16` units.
pub fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    (sum / samples.len() as f64).sqrt()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Continue,
    /// Trailing silence after speech.
    SilenceReached,
    /// The hard cap on one recording.
    MaxDuration,
}

#[derive(Clone, Copy, Debug)]
pub struct SilenceConfig {
    /// RMS above which a frame counts as speech. Tune per microphone.
    pub threshold: f64,
    /// Trailing silence that ends a recording. Zero disables auto-stop.
    pub trailing: Duration,
    pub max_duration: Duration,
}

impl Default for SilenceConfig {
    fn default() -> Self {
        Self {
            threshold: 300.0,
            trailing: Duration::from_secs_f64(2.0),
            max_duration: Duration::from_secs(120),
        }
    }
}

/// Decides when a recording has ended, from the frames fed to it.
///
/// Silence only counts once speech has been heard, so a slow start is not cut
/// off before the speaker begins.
#[derive(Debug)]
pub struct SilenceDetector {
    config: SilenceConfig,
    heard_speech: bool,
    silent: Duration,
    elapsed: Duration,
}

impl SilenceDetector {
    pub fn new(config: SilenceConfig) -> Self {
        Self {
            config,
            heard_speech: false,
            silent: Duration::ZERO,
            elapsed: Duration::ZERO,
        }
    }

    /// Feed one frame of mono samples and ask whether to keep recording.
    pub fn push(&mut self, frame: &[i16]) -> Decision {
        let duration = Duration::from_secs_f64(frame.len() as f64 / TARGET_RATE as f64);
        self.elapsed += duration;
        if self.elapsed >= self.config.max_duration {
            return Decision::MaxDuration;
        }

        if rms(frame) >= self.config.threshold {
            self.heard_speech = true;
            self.silent = Duration::ZERO;
        } else if self.heard_speech {
            self.silent += duration;
        }

        if !self.config.trailing.is_zero()
            && self.heard_speech
            && self.silent >= self.config.trailing
        {
            return Decision::SilenceReached;
        }
        Decision::Continue
    }

    pub fn heard_speech(&self) -> bool {
        self.heard_speech
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(amplitude: i16, samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|i| if i % 2 == 0 { amplitude } else { -amplitude })
            .collect()
    }

    /// One second of audio, as the detector counts it.
    fn second(amplitude: i16) -> Vec<i16> {
        frame(amplitude, TARGET_RATE as usize)
    }

    #[test]
    fn stereo_is_averaged_to_mono() {
        let mono = to_mono_i16(&[1.0, -1.0, 0.5, 0.5], 2);
        assert_eq!(mono.len(), 2);
        assert_eq!(mono[0], 0);
        assert!(mono[1] > 16_000);
    }

    #[test]
    fn samples_beyond_full_scale_are_clamped() {
        let mono = to_mono_i16(&[9.0, -9.0], 1);
        assert_eq!(mono, vec![i16::MAX, -i16::MAX]);
    }

    #[test]
    fn a_partial_frame_is_dropped_rather_than_skewing_the_mix() {
        assert_eq!(to_mono_i16(&[1.0], 2).len(), 0);
    }

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&[0; 128]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn rms_tracks_amplitude() {
        assert!((rms(&frame(1000, 128)) - 1000.0).abs() < 1.0);
    }

    #[test]
    fn silence_before_speech_never_stops_the_recording() {
        let mut detector = SilenceDetector::new(SilenceConfig::default());
        for _ in 0..10 {
            assert_eq!(detector.push(&second(0)), Decision::Continue);
        }
        assert!(!detector.heard_speech());
    }

    #[test]
    fn trailing_silence_after_speech_stops_it() {
        let mut detector = SilenceDetector::new(SilenceConfig::default());
        assert_eq!(detector.push(&second(5000)), Decision::Continue);
        assert_eq!(detector.push(&second(0)), Decision::Continue);
        assert_eq!(detector.push(&second(0)), Decision::SilenceReached);
    }

    #[test]
    fn speech_resets_the_silence_timer() {
        let mut detector = SilenceDetector::new(SilenceConfig::default());
        detector.push(&second(5000));
        detector.push(&second(0));
        detector.push(&second(5000));
        assert_eq!(detector.push(&second(0)), Decision::Continue);
    }

    #[test]
    fn a_zero_trailing_setting_disables_auto_stop() {
        let config = SilenceConfig {
            trailing: Duration::ZERO,
            ..SilenceConfig::default()
        };
        let mut detector = SilenceDetector::new(config);
        detector.push(&second(5000));
        for _ in 0..10 {
            assert_eq!(detector.push(&second(0)), Decision::Continue);
        }
    }

    #[test]
    fn the_max_duration_caps_a_recording_that_never_falls_silent() {
        let config = SilenceConfig {
            max_duration: Duration::from_secs(3),
            ..SilenceConfig::default()
        };
        let mut detector = SilenceDetector::new(config);
        assert_eq!(detector.push(&second(5000)), Decision::Continue);
        assert_eq!(detector.push(&second(5000)), Decision::Continue);
        assert_eq!(detector.push(&second(5000)), Decision::MaxDuration);
    }
}
