//! Command-line entry point. Every subcommand is a plain function over the
//! library, so Herdr invoking it from a keybinding and a user running it in a
//! shell take the same path.

use std::io::Read;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use herdr_dictate::{
    capture, config, context::Context, doctor, engine, indicator::Indicator, ipc::Client, server,
    session, session::Phase, session::Session, settings::Settings, setup,
};

#[derive(Parser)]
#[command(
    name = "herdr-dictate",
    version,
    about = "Local dictation into the focused Herdr pane"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start recording, or stop and insert the transcript.
    Toggle {
        /// Press Enter once the transcript lands.
        #[arg(long)]
        submit: bool,
    },
    /// Read a transcript on stdin and type it into the target pane.
    Deliver {
        #[arg(long)]
        submit: bool,
    },
    /// Show the keybindings this plugin needs, and offer to write them.
    Setup {
        /// Write without asking.
        #[arg(long, conflicts_with = "print")]
        apply: bool,
        /// Show the bindings and write nothing.
        #[arg(long)]
        print: bool,
    },
    /// Record to a WAV file, to check the microphone path.
    Record {
        #[arg(long, value_name = "FILE")]
        out: std::path::PathBuf,
        /// Stop after this many seconds regardless of silence.
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// Transcribe a 16 kHz mono WAV file.
    Transcribe { file: std::path::PathBuf },
    /// Hold the model resident, serving transcriptions until idle.
    Serve,
    /// Stop a running model server.
    ServeStop,
    /// Print one chip for the Herdr tab bar.
    Status,
    /// Report the wiring this plugin depends on.
    Doctor,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("HERDR_DICTATE_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    match run() {
        Ok(code) => code,
        Err(err) => {
            // Herdr captures plugin stderr into `herdr plugin log list`.
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    match Cli::parse().command {
        Command::Toggle { submit } => toggle(submit),
        Command::Deliver { submit } => deliver(submit).map(|()| ExitCode::SUCCESS),
        Command::Setup { apply, print } => {
            let mode = match (apply, print) {
                (true, _) => setup::Mode::Apply,
                (_, true) => setup::Mode::Print,
                _ => setup::Mode::Ask,
            };
            let path = config::config_path()?;
            setup::run(mode, &path, &mut std::io::stdout()).context("writing the keybindings")?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Record { out, seconds } => record(&out, seconds).map(|()| ExitCode::SUCCESS),
        Command::Transcribe { file } => transcribe(&file).map(|()| ExitCode::SUCCESS),
        Command::Serve => {
            let settings = Settings::load().context("reading the plugin config")?;
            server::serve(&settings).context("serving")?;
            Ok(ExitCode::SUCCESS)
        }
        Command::ServeStop => {
            let stopped = server::stop().context("stopping the server")?;
            println!("{}", if stopped { "stopped" } else { "not running" });
            Ok(ExitCode::SUCCESS)
        }
        Command::Status => status().map(|()| ExitCode::SUCCESS),
        Command::Doctor => Ok(run_doctor()),
    }
}

/// One chip for the tab bar. Herdr strips control sequences from a command
/// entry, so colour has to come from the glyph; emoji circles are two cells
/// each, so the chip keeps its width across states.
fn status() -> Result<()> {
    let label = match session::peek().context("reading the dictation state")? {
        Some(session) if session.phase == Phase::Transcribing => "\u{1f7e1} dictate",
        Some(_) => "\u{1f7e2} dictate",
        None => "\u{26aa} dictate",
    };
    println!("{label}");
    Ok(())
}

/// A dictation is two invocations: the first records, the second stops it.
fn toggle(submit: bool) -> Result<ExitCode> {
    if let Some(running) = session::live()? {
        session::stop(&running).context("stopping the recorder")?;
        tracing::info!(pid = running.pid, "stopping");
        return Ok(ExitCode::SUCCESS);
    }

    let client = Client::from_env().context("connecting to Herdr")?;
    let pane = match Context::from_env()?.target_pane() {
        Some(pane) => pane,
        None => client
            .focused_pane()
            .context("asking Herdr which pane is focused")?,
    };
    let settings = Settings::load().context("reading the plugin config")?;

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, std::sync::Arc::clone(&stop))
        .context("registering the stop signal")?;

    // Both cleared on drop, however this function leaves.
    let mut active = session::Active::begin(Session {
        pid: std::process::id(),
        pane: pane.clone(),
        submit,
        phase: Phase::Recording,
    })?;
    tracing::info!(%pane, submit, "recording");
    let indicator = Indicator::show(client.clone(), pane.clone(), "● dictating");
    let recording = capture::record(settings.silence.into(), stop).context("recording")?;

    if recording.samples.is_empty() {
        tracing::info!("nothing recorded");
        return Ok(ExitCode::SUCCESS);
    }

    indicator.set("◌ transcribing");
    active.transcribing();
    let served = settings
        .server
        .enabled
        .then(|| server::try_transcribe(&recording.samples))
        .flatten();
    let warmed = served.is_some();
    let text = match served {
        Some(text) => text,
        None => {
            let mut engine = engine::build(&settings.engine, &mut std::io::stderr())
                .context("loading the engine")?;
            engine
                .transcribe(&recording.samples)
                .context("transcribing")?
        }
    };
    if text.is_empty() {
        tracing::info!(secs = recording.duration().as_secs_f64(), "no speech");
        return Ok(ExitCode::SUCCESS);
    }

    // Cleared before the words arrive, not after.
    drop(indicator);

    // strip_non_speech collapses whitespace, so a dictated newline cannot
    // submit the prompt; only --submit presses Enter.
    client
        .send_text(&pane, &text)
        .with_context(|| format!("typing into {pane}"))?;
    if submit {
        client
            .send_keys(&pane, &["enter"])
            .with_context(|| format!("submitting in {pane}"))?;
    }
    tracing::info!(%pane, chars = text.len(), stopped_by = ?recording.stopped_by, "delivered");

    // Started after delivering, never before: two copies of the model loading
    // at once would slow down the dictation that is paying for it.
    if settings.server.enabled
        && !warmed
        && let Err(err) = server::spawn()
    {
        tracing::debug!(%err, "could not start the model server");
    }
    Ok(ExitCode::SUCCESS)
}

fn deliver(submit: bool) -> Result<()> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the transcript from stdin")?;
    let text = text.trim_end_matches('\n');
    if text.is_empty() {
        tracing::info!("nothing to deliver");
        return Ok(());
    }

    let client = Client::from_env().context("connecting to Herdr")?;
    let pane = match Context::from_env()?.target_pane() {
        Some(pane) => pane,
        None => client
            .focused_pane()
            .context("asking Herdr which pane is focused")?,
    };

    // A missing pane answers with pane_not_found rather than succeeding silently.
    client
        .send_text(&pane, text)
        .with_context(|| format!("typing into {pane}"))?;
    if submit {
        client
            .send_keys(&pane, &["enter"])
            .with_context(|| format!("submitting in {pane}"))?;
    }
    tracing::info!(pane = %pane, chars = text.len(), submit, "delivered");
    Ok(())
}

fn record(out: &std::path::Path, seconds: u64) -> Result<()> {
    let silence = herdr_dictate::audio::SilenceConfig {
        max_duration: std::time::Duration::from_secs(seconds),
        ..Default::default()
    };
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let recording = capture::record(silence, stop).context("recording")?;
    capture::write_wav(out, &recording.samples).context("writing the wav")?;
    println!(
        "{:.1}s, {} samples, stopped by {:?} -> {}",
        recording.duration().as_secs_f64(),
        recording.samples.len(),
        recording.stopped_by,
        out.display()
    );
    Ok(())
}

fn transcribe(file: &std::path::Path) -> Result<()> {
    let mut reader =
        hound::WavReader::open(file).with_context(|| format!("opening {}", file.display()))?;
    let samples: std::result::Result<Vec<i16>, _> = reader.samples::<i16>().collect();
    let samples = samples.context("reading samples")?;

    let settings = Settings::load().context("reading the plugin config")?;

    let started = std::time::Instant::now();
    let served = settings
        .server
        .enabled
        .then(|| server::try_transcribe(&samples))
        .flatten();
    let text = match served {
        Some(text) => {
            eprintln!("via the model server");
            text
        }
        None => {
            let mut engine = engine::build(&settings.engine, &mut std::io::stderr())
                .context("loading the engine")?;
            eprintln!("{}", engine.describe());
            engine.transcribe(&samples).context("transcribing")?
        }
    };
    eprintln!(
        "{:.1}s of audio in {:.1}s",
        samples.len() as f64 / 16000.0,
        started.elapsed().as_secs_f64()
    );
    println!("{text}");
    Ok(())
}

/// Exits non-zero when a check failed, so it is usable in a script.
fn run_doctor() -> ExitCode {
    let checks = doctor::run();
    let _ = doctor::render(&checks, &mut std::io::stdout());
    match doctor::worst(&checks) {
        doctor::Status::Fail => ExitCode::FAILURE,
        _ => ExitCode::SUCCESS,
    }
}
