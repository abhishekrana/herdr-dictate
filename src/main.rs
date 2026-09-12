//! Command-line entry point. Every subcommand is a plain function over the
//! library, so Herdr invoking it from a keybinding and a user running it in a
//! shell take the same path.

use std::io::Read;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use herdr_dictate::{
    capture, config, context::Context, doctor, engine, indicator::Indicator, ipc::Client, server,
    session, session::MachineRef, session::Phase, session::Session, settings::Settings, setup,
    sink, sink::Delivery, sink::Sink,
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
    /// Open the shared ssh connection to the selected machine, so the next
    /// dictation does not pay for a handshake.
    Warm,
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
        Command::Warm => warm().map(|()| ExitCode::SUCCESS),
    }
}

/// One chip for the tab bar; an unreadable config falls back to the defaults.
fn status() -> Result<()> {
    let text = Settings::load().unwrap_or_default().status;
    let chip = match session::peek().context("reading the dictation state")? {
        Some(session) if session.phase == Phase::Transcribing => text.transcribing,
        Some(_) => text.recording,
        None => text.idle,
    };
    println!("{chip}");
    Ok(())
}

/// Long enough to read, short enough to expire on its own.
const FLASH: std::time::Duration = std::time::Duration::from_secs(5);

/// A pane label is a few words wide, so say what broke and leave the detail to
/// the log.
fn warning(err: &herdr_dictate::Error) -> String {
    let detail = err.to_string();
    let detail = detail.split(':').next().unwrap_or("unreachable").trim();
    format!("⚠ dictate: {detail} unreachable")
}

/// A dictation is two invocations: the first records, the second stops it.
fn toggle(submit: bool) -> Result<ExitCode> {
    if let Some(running) = session::live()? {
        session::stop(&running).context("stopping the recorder")?;
        tracing::info!(pid = running.pid, "stopping");
        return Ok(ExitCode::SUCCESS);
    }

    let client = Client::from_env().context("connecting to Herdr")?;
    let settings = Settings::load().context("reading the plugin config")?;
    let local_pane = match Context::from_env()?.target_pane() {
        Some(pane) => pane,
        None => client
            .focused_pane()
            .context("asking Herdr which pane is focused")?,
    };

    // Resolving before the microphone opens is what makes an unreachable
    // machine cost an error rather than a transcript. Nothing below may move
    // past capture::record.
    let latch = match sink::selected(&settings.remote) {
        Ok(Some(latch)) => latch,
        Ok(None) => sink::Latch::local(Sink::Local(client.clone()), local_pane),
        Err(err) => {
            // An error only reaches the plugin log, which nobody is reading
            // when a keypress does nothing. Never a fallback: no transcript
            // goes anywhere local.
            let _ = client.set_pane_label(&local_pane, &warning(&err), FLASH);
            return Err(err.into());
        }
    };
    let (sink, pane) = (latch.sink, latch.pane);

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, std::sync::Arc::clone(&stop))
        .context("registering the stop signal")?;

    // Both cleared on drop, however this function leaves.
    let mut active = session::Active::begin(Session {
        pid: std::process::id(),
        pane: pane.clone(),
        submit,
        phase: Phase::Recording,
        machine: latch.machine.map(|m| MachineRef {
            id: m.id,
            label: m.label,
        }),
    })?;
    tracing::info!(%pane, submit, at = sink.describe(), "recording");
    let indicator = Indicator::show(sink.clone(), pane.clone(), "● dictating");
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
    let handed_over = std::time::Instant::now();
    let delivery = match sink.deliver(&pane, &text, submit) {
        Ok(delivery) => delivery,
        Err(err) => {
            let kept = session::keep_undelivered(&text)?;
            return Err(
                anyhow::Error::new(err).context(format!("the words are in {}", kept.display()))
            );
        }
    };
    if delivery == Delivery::AgentGone {
        tracing::warn!(%pane, "the agent had gone, so the words were left unsent");
    }
    tracing::info!(
        %pane,
        at = sink.describe(),
        chars = text.len(),
        ?delivery,
        ms = handed_over.elapsed().as_millis(),
        stopped_by = ?recording.stopped_by,
        "delivered"
    );

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

/// Open the shared ssh connection ahead of a dictation.
///
/// An optimisation, never a dependency: a cold connection costs latency the
/// user spends speaking anyway, so failing here is not worth reporting as a
/// failure of anything.
fn warm() -> Result<()> {
    let started = std::time::Instant::now();
    let settings = Settings::load().context("reading the plugin config")?;
    match sink::selected(&settings.remote) {
        Ok(Some(latch)) => tracing::info!(
            at = latch.sink.describe(),
            pane = %latch.pane,
            ms = started.elapsed().as_millis(),
            "warm"
        ),
        Ok(None) => tracing::info!("no machine selected"),
        Err(err) => tracing::debug!(%err, "could not warm the connection"),
    }
    Ok(())
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
    let settings = Settings::load().context("reading the plugin config")?;
    let local_pane = match Context::from_env()?.target_pane() {
        Some(pane) => pane,
        None => client
            .focused_pane()
            .context("asking Herdr which pane is focused")?,
    };
    let latch = match sink::selected(&settings.remote)? {
        Some(latch) => latch,
        None => sink::Latch::local(Sink::Local(client), local_pane),
    };

    // A missing pane answers with pane_not_found rather than succeeding silently.
    let delivery = latch
        .sink
        .deliver(&latch.pane, text, submit)
        .with_context(|| format!("typing into {}", latch.pane))?;
    tracing::info!(
        pane = %latch.pane,
        at = latch.sink.describe(),
        chars = text.len(),
        ?delivery,
        "delivered"
    );
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
