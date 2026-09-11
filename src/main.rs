//! Command-line entry point. Every subcommand is a plain function over the
//! library, so Herdr invoking it from a keybinding and a user running it in a
//! shell take the same path.

use std::io::Read;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use herdr_dictate::{capture, config, context::Context, ipc::Client, setup};

/// Distinguishes "declared but not built yet" from an unknown command.
const EXIT_NOT_IMPLEMENTED: u8 = 3;

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
        Command::Toggle { .. } => {
            tracing::error!(
                "toggle is declared but not built yet: capture and the speech engine are still to come"
            );
            Ok(ExitCode::from(EXIT_NOT_IMPLEMENTED))
        }
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
        Command::Doctor => doctor().map(|()| ExitCode::SUCCESS),
    }
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

fn doctor() -> Result<()> {
    println!("herdr-dictate {}", env!("CARGO_PKG_VERSION"));
    match Client::from_env() {
        Ok(client) => {
            println!("socket      {}", client.socket_path().display());
            match client.focused_pane() {
                Ok(pane) => println!("focused     {pane}"),
                Err(err) => println!("focused     unavailable - {err}"),
            }
        }
        Err(err) => println!("socket      {err}"),
    }
    println!(
        "context     {:?}",
        Context::from_env().unwrap_or_default().target_pane()
    );

    match capture::describe_input() {
        Ok(input) => println!("input       {input}"),
        Err(err) => println!("input       unavailable - {err}"),
    }

    let path = config::config_path()?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let missing = config::missing_bindings(&existing);
    println!("config      {}", path.display());
    if missing.is_empty() {
        println!("bindings    all bound");
    } else {
        println!(
            "bindings    {} unbound - run `herdr-dictate setup`",
            missing.len()
        );
    }
    Ok(())
}
