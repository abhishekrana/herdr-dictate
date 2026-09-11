//! Command-line entry point. Every subcommand is a plain function over the
//! library, so `herdr-dictate <cmd>` is runnable with no terminal attached -
//! which is what lets Herdr invoke it from a keybinding and a user debug it
//! from a shell with the same code path.

use std::io::Read;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use herdr_dictate::{context::Context, ipc::Client};

const USAGE: &str = "\
herdr-dictate - local dictation into the focused Herdr pane

USAGE:
    herdr-dictate deliver [--submit]   read a transcript on stdin, type it into the target pane
    herdr-dictate doctor               report the wiring this plugin depends on
    herdr-dictate --version
";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("HERDR_DICTATE_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Plugin stderr is captured by `herdr plugin log list`, which is
            // where a user looks when a keypress appears to do nothing.
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("deliver") => {
            let submit = args.any(|a| a == "--submit")
                || std::env::var("HERDR_DICTATE_SUBMIT").is_ok_and(|v| v == "1");
            deliver(submit)
        }
        Some("doctor") => doctor(),
        Some("--version" | "-V") => {
            println!("herdr-dictate {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

/// Type a transcript read from stdin into the target pane.
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
    let ctx = Context::from_env()?;
    let pane = match ctx.target_pane() {
        Some(pane) => pane,
        None => client
            .focused_pane()
            .context("asking Herdr which pane is focused")?,
    };

    // A pane that has gone answers with pane_not_found rather than succeeding
    // silently, so there is nothing to check before sending.
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
    let ctx = Context::from_env().unwrap_or_default();
    println!("context     {:?}", ctx.target_pane());
    Ok(())
}
