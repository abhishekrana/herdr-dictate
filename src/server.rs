//! A model server that keeps the engine resident between dictations.
//!
//! Loading a model and setting up a GPU context costs more than transcribing a
//! short clip, and a plugin action is a fresh process every press. The server
//! pays that once and is spawned lazily on the first dictation.
//!
//! Binding the socket only after the engine is ready doubles as the readiness
//! signal: a client that can connect can be served.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::settings::Settings;
use crate::{Error, PLUGIN_ID, Result, engine};

/// Guards against a corrupt or hostile frame claiming an enormous buffer.
/// Whisper caps a clip long before this.
const MAX_SAMPLES: u32 = 16_000 * 60 * 30;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// Transcription is the slow part; the client waits rather than giving up.
const CALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Written while a server is running, so `stop` can signal the right process.
pub fn pid_path() -> PathBuf {
    socket_path().with_extension("pid")
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(format!(
                "/tmp/{PLUGIN_ID}-{}",
                rustix::process::getuid().as_raw()
            ))
        });
    dir.join(format!("{PLUGIN_ID}.sock"))
}

/// Transcribe through a running server, or `None` if there is not one.
///
/// Never an error: an unreachable server means fall back, not fail.
pub fn try_transcribe(samples: &[i16]) -> Option<String> {
    match call(samples) {
        Ok(text) => Some(text),
        Err(err) => {
            tracing::debug!(%err, "no model server; transcribing in process");
            None
        }
    }
}

fn call(samples: &[i16]) -> Result<String> {
    let mut stream = UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(CALL_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;

    let mut request = Vec::with_capacity(4 + samples.len() * 2);
    request.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    for sample in samples {
        request.extend_from_slice(&sample.to_le_bytes());
    }
    stream.write_all(&request)?;
    stream.flush()?;

    let mut header = [0u8; 5];
    stream.read_exact(&mut header)?;
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    let text = String::from_utf8_lossy(&body).into_owned();

    if header[0] == 0 {
        Ok(text)
    } else {
        Err(Error::Model(text))
    }
}

/// Where a server started by [`spawn`] logs, beside the recording state.
pub fn log_path() -> Result<PathBuf> {
    Ok(crate::session::state_path()?.with_file_name("server.log"))
}

/// Start a detached server. Returns once it is spawned, not once it is ready.
///
/// Its log starts empty with each server, so it holds one server's life.
pub fn spawn() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let log = log_path().and_then(|path| {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(std::fs::File::create(path)?)
    });
    let stderr = match log {
        Ok(file) => std::process::Stdio::from(file),
        Err(err) => {
            tracing::debug!(%err, "no server log");
            std::process::Stdio::null()
        }
    };

    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        // Its own process group, so the press that spawned it can exit freely.
        .process_group(0)
        .spawn()?;
    Ok(())
}

/// Load the engine, then serve until idle for `idle`.
pub fn serve(settings: &Settings) -> Result<()> {
    let idle = settings.server.idle();
    let path = socket_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A socket left by a crashed server refuses connections; only then is it
    // ours to remove.
    if path.exists() && UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }

    let mut engine = engine::build(&settings.engine, &mut std::io::stderr())?;
    tracing::info!(engine = engine.describe(), "model resident");

    let listener = UnixListener::bind(&path)?;
    std::fs::write(pid_path(), std::process::id().to_string())?;
    let last = Arc::new(Mutex::new(Instant::now()));
    watchdog(Arc::clone(&last), idle, path.clone());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Ok(mut at) = last.lock() {
                    *at = Instant::now();
                }
                if let Err(err) = handle(stream, engine.as_mut()) {
                    tracing::warn!(%err, "client");
                }
                if let Ok(mut at) = last.lock() {
                    *at = Instant::now();
                }
            }
            Err(err) => tracing::warn!(%err, "accept"),
        }
    }
    Ok(())
}

/// Exit once nothing has used the server for `idle`, freeing the model's memory.
fn watchdog(last: Arc<Mutex<Instant>>, idle: Duration, socket: PathBuf) {
    if idle.is_zero() {
        return;
    }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let elapsed = last.lock().map(|at| at.elapsed()).unwrap_or_default();
            if elapsed >= idle {
                tracing::info!(?idle, "idle, exiting");
                let _ = std::fs::remove_file(&socket);
                let _ = std::fs::remove_file(pid_path());
                std::process::exit(0);
            }
        }
    });
}

fn handle(mut stream: UnixStream, engine: &mut dyn engine::Engine) -> Result<()> {
    stream.set_read_timeout(Some(CALL_TIMEOUT))?;

    let mut count = [0u8; 4];
    stream.read_exact(&mut count)?;
    let count = u32::from_le_bytes(count);
    if count > MAX_SAMPLES {
        return respond(
            &mut stream,
            1,
            &format!("{count} samples is more than this server accepts"),
        );
    }

    let mut raw = vec![0u8; count as usize * 2];
    stream.read_exact(&mut raw)?;
    let samples: Vec<i16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();

    let started = Instant::now();
    match engine.transcribe(&samples) {
        Ok(text) => {
            tracing::info!(
                samples = samples.len(),
                ms = started.elapsed().as_millis(),
                "transcribed"
            );
            respond(&mut stream, 0, &text)
        }
        Err(err) => respond(&mut stream, 1, &err.to_string()),
    }
}

fn respond(stream: &mut UnixStream, status: u8, text: &str) -> Result<()> {
    let body = text.as_bytes();
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(status);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    stream.write_all(&out)?;
    stream.flush()?;
    Ok(())
}

/// Stop a running server. Returns whether one was there to stop.
pub fn stop() -> Result<bool> {
    let (socket, pid_file) = (socket_path(), pid_path());
    let pid = std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|raw| raw.trim().parse::<i32>().ok());

    // Unlink first, so nothing connects to a server that is going away.
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&pid_file);

    let Some(pid) = pid.and_then(rustix::process::Pid::from_raw) else {
        return Ok(false);
    };
    match rustix::process::kill_process(pid, rustix::process::Signal::TERM) {
        Ok(()) => Ok(true),
        // Already gone: the files were stale, which is not a failure.
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(err) => Err(Error::Model(format!("stopping the model server: {err}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_sits_under_the_runtime_dir() {
        let path = socket_path();
        assert!(path.to_string_lossy().contains(PLUGIN_ID));
        assert_eq!(path.extension().unwrap(), "sock");
    }

    #[test]
    fn a_call_with_no_server_is_an_error_not_a_panic() {
        // Nothing is listening on a path under a fresh temp dir.
        let dir = tempfile::tempdir().unwrap();
        assert!(UnixStream::connect(dir.path().join("absent.sock")).is_err());
    }

    #[test]
    fn frames_round_trip_through_the_response_encoding() {
        let (mut a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        respond(&mut a, 0, "hello").unwrap();
        let mut header = [0u8; 5];
        b.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 0);
        let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut body = vec![0u8; len];
        b.read_exact(&mut body).unwrap();
        assert_eq!(String::from_utf8(body).unwrap(), "hello");
    }

    #[test]
    fn an_error_response_is_distinguishable_from_text() {
        let (mut a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        respond(&mut a, 1, "it broke").unwrap();
        let mut header = [0u8; 5];
        b.read_exact(&mut header).unwrap();
        assert_eq!(
            header[0], 1,
            "status distinguishes an error from a transcript"
        );
    }
}
