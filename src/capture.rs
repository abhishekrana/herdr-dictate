//! Microphone capture: the one platform boundary in the plugin.
//!
//! The device is asked for 16 kHz mono directly, so there is no resampling.
//! ALSA's plug layer converts, which covers Linux; a device that cannot offer
//! the rate is reported rather than silently recorded at the wrong one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::audio::{Decision, SilenceConfig, SilenceDetector, TARGET_RATE, to_mono_i16};
use crate::{Error, Result};

/// Bounded so a stalled consumer drops audio rather than growing without limit.
const QUEUE_FRAMES: usize = 256;

/// Where a recording's audio comes from.
#[derive(Clone, Debug)]
pub enum Source {
    Microphone,
    /// A 16 kHz mono WAV played in real time in place of the microphone, so a
    /// dictation can be driven end to end without one.
    Wav(PathBuf),
}

/// What a WAV source sends at a time: 30 ms, about what a device delivers.
const WAV_FRAME: usize = 480;

pub struct Recording {
    /// 16 kHz mono.
    pub samples: Vec<i16>,
    pub stopped_by: Decision,
    /// Whether loudness ever held long enough to count as speech.
    pub heard_speech: bool,
    /// RMS of the loudest 20 ms window, in `i16` units.
    pub loudest: f64,
}

impl Recording {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / TARGET_RATE as f64)
    }
}

pub fn default_input() -> Result<cpal::Device> {
    cpal::default_host()
        .default_input_device()
        .ok_or(Error::NoInputDevice)
}

/// The format to open the device in: 16 kHz mono, `I16` if offered, else `F32`.
fn target_format(device: &cpal::Device) -> Result<cpal::SampleFormat> {
    let configs = device
        .supported_input_configs()
        .map_err(|e| Error::Audio(e.to_string()))?;
    let mut formats: Vec<cpal::SampleFormat> = configs
        .filter(|c| {
            c.min_sample_rate() <= TARGET_RATE
                && TARGET_RATE <= c.max_sample_rate()
                && c.channels() == 1
        })
        .map(|c| c.sample_format())
        .collect();
    formats.sort_by_key(|f| !matches!(f, cpal::SampleFormat::I16));
    formats
        .into_iter()
        .find(|f| matches!(f, cpal::SampleFormat::I16 | cpal::SampleFormat::F32))
        .ok_or_else(|| {
            Error::Audio(format!(
                "no {TARGET_RATE} Hz mono input; resampling is not implemented"
            ))
        })
}

/// Record until the detector, `stop`, or the end of the source says to finish.
///
/// `on_audio` sees every sample recorded so far, each time a frame arrives.
pub fn record(
    source: &Source,
    silence: SilenceConfig,
    stop: Arc<AtomicBool>,
    mut on_audio: impl FnMut(&[i16]),
) -> Result<Recording> {
    let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(QUEUE_FRAMES);
    // Held until recording ends: dropping the stream closes the device.
    let _stream = match source {
        Source::Microphone => Some(open_microphone(tx)?),
        Source::Wav(path) => {
            play_wav(path, tx)?;
            None
        }
    };

    let mut detector = SilenceDetector::new(silence);
    let mut samples = Vec::new();
    let mut stopped_by = Decision::Continue;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(frame) => {
                let decision = detector.push(&frame);
                samples.extend_from_slice(&frame);
                on_audio(&samples);
                if decision != Decision::Continue {
                    stopped_by = decision;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(Recording {
        samples,
        stopped_by,
        heard_speech: detector.heard_speech(),
        loudest: detector.loudest(),
    })
}

fn open_microphone(tx: mpsc::SyncSender<Vec<i16>>) -> Result<cpal::Stream> {
    let device = default_input()?;
    let format = target_format(&device)?;
    let config = cpal::StreamConfig {
        channels: 1,
        sample_rate: TARGET_RATE,
        buffer_size: cpal::BufferSize::Default,
    };
    let on_error = |err| tracing::warn!(%err, "input stream error");

    let stream = match format {
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &_| {
                let _ = tx.try_send(data.to_vec());
            },
            on_error,
            None,
        ),
        _ => device.build_input_stream(
            config,
            move |data: &[f32], _: &_| {
                let _ = tx.try_send(to_mono_i16(data, 1));
            },
            on_error,
            None,
        ),
    }
    .map_err(|e| Error::Audio(e.to_string()))?;

    stream.play().map_err(|e| Error::Audio(e.to_string()))?;
    tracing::info!(rate = TARGET_RATE, ?format, "recording");
    Ok(stream)
}

/// Send a WAV's samples at the rate a microphone would, from a thread that
/// ends at the end of the file or once the recording stops listening.
fn play_wav(path: &Path, tx: mpsc::SyncSender<Vec<i16>>) -> Result<()> {
    let samples = read_wav(path)?;
    tracing::info!(file = %path.display(), secs = samples.len() as f64 / TARGET_RATE as f64, "recording from a file");
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        for (i, frame) in samples.chunks(WAV_FRAME).enumerate() {
            let due = Duration::from_secs_f64((i * WAV_FRAME) as f64 / TARGET_RATE as f64);
            std::thread::sleep(due.saturating_sub(started.elapsed()));
            if tx.send(frame.to_vec()).is_err() {
                return;
            }
        }
    });
    Ok(())
}

/// The samples of a 16 kHz mono 16-bit WAV.
pub fn read_wav(path: &Path) -> Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| Error::Audio(format!("{}: {e}", path.display())))?;
    let spec = reader.spec();
    if spec.sample_rate != TARGET_RATE || spec.channels != 1 || spec.bits_per_sample != 16 {
        return Err(Error::Audio(format!(
            "{}: {} Hz, {} channels, {} bits; need {TARGET_RATE} Hz mono 16-bit",
            path.display(),
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample
        )));
    }
    reader
        .samples::<i16>()
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| Error::Audio(format!("{}: {e}", path.display())))
}

/// One line describing the input, for `doctor`.
pub fn describe_input() -> Result<String> {
    let device = default_input()?;
    let name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "<unnamed>".into());
    let format = match target_format(&device) {
        Ok(format) => format!("{TARGET_RATE} Hz mono {format:?}"),
        Err(err) => err.to_string(),
    };
    Ok(format!("{name} - will open at {format}"))
}

/// Write 16 kHz mono samples as a WAV file.
pub fn write_wav(path: &std::path::Path, samples: &[i16]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(path, spec).map_err(|e| Error::Audio(e.to_string()))?;
    for &sample in samples {
        writer
            .write_sample(sample)
            .map_err(|e| Error::Audio(e.to_string()))?;
    }
    writer.finalize().map_err(|e| Error::Audio(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wav_source_records_the_whole_file_then_ends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.wav");
        // Not a whole number of frames, so the last one is partial.
        let samples: Vec<i16> = (0..5_000).map(|i| (i % 300) as i16).collect();
        write_wav(&path, &samples).unwrap();

        let mut seen = 0;
        let recording = record(
            &Source::Wav(path),
            SilenceConfig::default(),
            Arc::new(AtomicBool::new(false)),
            |all| seen = all.len(),
        )
        .unwrap();
        assert_eq!(recording.samples, samples);
        assert_eq!(seen, samples.len(), "on_audio saw the last frame");
        assert_eq!(recording.stopped_by, Decision::Continue);
    }

    #[test]
    fn a_wav_at_another_rate_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        hound::WavWriter::create(&path, spec)
            .unwrap()
            .finalize()
            .unwrap();
        assert!(
            read_wav(&path)
                .unwrap_err()
                .to_string()
                .contains("44100 Hz")
        );
    }
}
