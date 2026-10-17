//! A live feed of the dictation state, for anything drawing it outside Herdr.
//!
//! `watch` prints one JSON object per line on stdout - the phase, and while
//! recording the microphone level - the moment either changes. There is no
//! daemon: each watcher binds a Unix datagram socket in the state directory's
//! `watchers/`, and the recorder sends every change to whatever sockets it
//! finds there. A send never blocks the capture loop and a failed one is
//! dropped, so a watcher can only ever cost the recorder a few syscalls.
//!
//! The state files stay the source of truth. A watcher re-reads them about
//! once a second, which covers a recorder that died without saying so and a
//! dictation delivered here from another machine, which sends nothing.

use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::audio::{SilenceConfig, rms};
use crate::session::{self, Phase};
use crate::{Result, remote};

/// The directory under the state directory that holds one socket per watcher.
pub const WATCHERS: &str = "watchers";

/// The fastest level lines go out: about twenty a second, enough for a
/// waveform to move and few enough that a watcher is never a cost.
pub const LEVEL_INTERVAL: Duration = Duration::from_millis(50);

/// How often a watcher re-reads the state files, whatever it has been sent.
const POLL: Duration = Duration::from_secs(1);

/// How long a sender trusts its list of watchers before listing them again.
const RELIST: Duration = Duration::from_millis(500);

/// Big enough for any update; a datagram is read whole or not at all.
const DATAGRAM: usize = 256;

/// One line of the feed, and one datagram from the recorder.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "phase", rename_all = "lowercase")]
pub enum Update {
    Idle,
    /// `level` is 0 to 1, absent where nothing measured it: the state files,
    /// and a dictation on another machine.
    Recording {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<f32>,
    },
    Transcribing,
}

impl Update {
    /// A recording with no level measured yet.
    pub const RECORDING: Self = Self::Recording { level: None };

    /// Recording, with the microphone at `level`.
    pub fn level(level: f32) -> Self {
        Self::Recording { level: Some(level) }
    }

    /// The update a recorder in `phase` sends.
    pub fn from_phase(phase: Phase) -> Self {
        match phase {
            Phase::Recording => Self::RECORDING,
            Phase::Transcribing => Self::Transcribing,
        }
    }

    /// The JSON text of one line or one datagram.
    pub fn encode(self) -> String {
        // Two variants and an optional float: nothing here can fail to serialise.
        serde_json::to_string(&self).unwrap_or_else(|_| r#"{"phase":"idle"}"#.into())
    }

    /// An update from a datagram, or `None` for anything that is not one.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }

    fn same_phase(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

/// Loudness quieter than this reads as nothing, so room tone leaves the meter
/// still; it is a sixth of the default speech threshold.
const FLOOR_RATIO: f64 = 1.0 / 6.0;

/// Loudness that fills the meter: close, loud speech, about 8,000 counts at
/// the default threshold.
const CEILING_RATIO: f64 = 80.0 / 3.0;

/// A window's RMS, in `i16` units, as a meter reading from 0 to 1.
///
/// Logarithmic, so quiet speech moves the meter as much as loud speech does,
/// and anchored to the default silence threshold: speech just loud enough to
/// count as speech reads about 0.35.
pub fn level(rms: f64) -> f32 {
    let threshold = SilenceConfig::default().threshold;
    let (floor, ceiling) = (threshold * FLOOR_RATIO, threshold * CEILING_RATIO);
    if rms.is_nan() || rms <= floor {
        return 0.0;
    }
    let reading = (rms / floor).ln() / (ceiling / floor).ln();
    // Two decimals is finer than any meter draws, and keeps each line short.
    ((reading.clamp(0.0, 1.0) * 100.0).round() / 100.0) as f32
}

/// Turns a growing recording into level readings, at most one per
/// [`LEVEL_INTERVAL`], each over everything heard since the last.
#[derive(Debug, Default)]
pub struct Meter {
    /// Samples already measured.
    fed: usize,
    sent: Option<Instant>,
}

impl Meter {
    /// Everything recorded so far; a reading when one is due.
    pub fn hear(&mut self, recorded: &[i16], now: Instant) -> Option<f32> {
        if self
            .sent
            .is_some_and(|sent| now.duration_since(sent) < LEVEL_INTERVAL)
        {
            return None;
        }
        let fresh = recorded.get(self.fed..).filter(|f| !f.is_empty())?;
        self.fed = recorded.len();
        self.sent = Some(now);
        Some(level(rms(fresh)))
    }
}

/// Sends the recorder's updates to every watcher.
///
/// Nothing here reports an error: a watcher that is gone or not keeping up
/// costs that watcher an update, never the recording.
#[derive(Debug)]
pub struct Broadcaster {
    dir: PathBuf,
    socket: Option<UnixDatagram>,
    peers: Vec<PathBuf>,
    listed: Option<Instant>,
}

impl Broadcaster {
    /// A sender to the watchers under `state_dir`.
    pub fn new(state_dir: &Path) -> Self {
        // Non-blocking, so a watcher whose queue is full drops the update
        // rather than stalling the capture loop.
        let socket = UnixDatagram::unbound()
            .and_then(|s| s.set_nonblocking(true).map(|()| s))
            .inspect_err(|err| tracing::debug!(%err, "watchers will not be told"))
            .ok();
        Self {
            dir: state_dir.join(WATCHERS),
            socket,
            peers: Vec::new(),
            listed: None,
        }
    }

    /// Send `update` to every watcher.
    pub fn send(&mut self, update: Update) {
        let Some(socket) = &self.socket else { return };
        let now = Instant::now();
        // A phase change is rare and must reach a watcher that started a
        // moment ago, so it always lists afresh; levels use the cached list.
        let stale = self
            .listed
            .is_none_or(|listed| now.duration_since(listed) >= RELIST);
        if stale || !matches!(update, Update::Recording { level: Some(_) }) {
            self.peers = list(&self.dir);
            self.listed = Some(now);
        }
        let bytes = update.encode();
        self.peers
            .retain(|peer| match socket.send_to(bytes.as_bytes(), peer) {
                Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
                    // Nothing is bound there: a watcher that was killed.
                    let _ = std::fs::remove_file(peer);
                    false
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
                _ => true,
            });
    }
}

/// The watcher sockets in `dir`.
fn list(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "sock"))
        .collect()
}

/// One watcher's socket, removed again when dropped.
#[derive(Debug)]
pub struct Watcher {
    socket: UnixDatagram,
    path: PathBuf,
}

impl Watcher {
    /// Bind this process's socket under `state_dir`.
    pub fn bind(state_dir: &Path) -> Result<Self> {
        let dir = state_dir.join(WATCHERS);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.sock", std::process::id()));
        // Left behind by an earlier process that had this pid and was killed.
        let _ = std::fs::remove_file(&path);
        let socket = UnixDatagram::bind(&path)?;
        Ok(Self { socket, path })
    }

    /// The next update a recorder sends, waiting up to `timeout`.
    ///
    /// `None` when the wait ends with nothing, or a signal cuts it short.
    pub fn recv(&self, timeout: Duration) -> Option<Update> {
        // A zero timeout means block forever to the socket API.
        let timeout = timeout.max(Duration::from_millis(1));
        self.socket.set_read_timeout(Some(timeout)).ok()?;
        let mut buf = [0u8; DATAGRAM];
        let len = self.socket.recv(&mut buf).ok()?;
        Update::decode(&buf[..len])
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Decides which updates become lines: every level reading, and a phase only
/// when it differs from the last one printed.
#[derive(Debug, Default)]
pub struct Shown {
    last: Option<Update>,
}

impl Shown {
    /// Whether `update` should be printed; remembers it if so.
    pub fn admit(&mut self, update: Update) -> bool {
        let fresh = match (self.last, update) {
            (None, _) => true,
            (_, Update::Recording { level: Some(_) }) => true,
            (Some(last), update) => !last.same_phase(update),
        };
        if fresh {
            self.last = Some(update);
        }
        fresh
    }
}

/// What the state files in `state_dir` say: a live recorder here first, then
/// one on another machine delivering here, else idle.
pub fn current(state_dir: &Path) -> Update {
    if let Ok(Some(session)) = session::peek_at(&state_dir.join(session::STATE_FILE)) {
        return Update::from_phase(session.phase);
    }
    std::fs::read_to_string(state_dir.join(remote::REMOTE_STATE))
        .ok()
        .and_then(|text| remote_phase(&text, now_secs()))
        .map_or(Update::Idle, Update::from_phase)
}

/// `remote.json` as another machine's dictation writes it here.
#[derive(Deserialize)]
struct RemoteState {
    phase: Phase,
    /// Expiry, in seconds since the epoch on this machine's clock.
    until: u64,
}

/// The phase in a `remote.json`, unless it has expired by `now`.
fn remote_phase(text: &str, now: u64) -> Option<Phase> {
    let state: RemoteState = serde_json::from_str(text).ok()?;
    (state.until > now).then_some(state.phase)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Print the state, then every change, until `stop` is set or `out` closes.
///
/// `hangup` says whether the reader of `out` has gone away; it is asked on
/// each re-read of the state files, so a closed pipe ends the watch even when
/// nothing changes to write.
pub fn run(
    state_dir: &Path,
    out: &mut impl Write,
    stop: &AtomicBool,
    hangup: impl Fn() -> bool,
) -> Result<()> {
    let watcher = Watcher::bind(state_dir)?;
    let mut shown = Shown::default();
    let mut emit = |update: Update| -> std::io::Result<()> {
        if shown.admit(update) {
            writeln!(out, "{}", update.encode())?;
            out.flush()?;
        }
        Ok(())
    };

    let result = (|| -> std::io::Result<()> {
        emit(current(state_dir))?;
        let mut polled = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let wait = POLL.saturating_sub(polled.elapsed());
            match watcher.recv(wait) {
                // The recorder has cleared its state file by now, and the
                // files may still name another dictation.
                Some(Update::Idle) => emit(current(state_dir))?,
                Some(update) => emit(update)?,
                None => {}
            }
            if polled.elapsed() >= POLL {
                polled = Instant::now();
                if hangup() {
                    return Ok(());
                }
                emit(current(state_dir))?;
            }
        }
        Ok(())
    })();

    match result {
        // The reader went away: the watch is over, not failed.
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_and_room_tone_read_as_nothing() {
        assert_eq!(level(0.0), 0.0);
        assert_eq!(level(f64::NAN), 0.0);
        assert_eq!(level(40.0), 0.0);
    }

    #[test]
    fn speech_at_the_threshold_reads_about_a_third() {
        let at = level(SilenceConfig::default().threshold);
        assert!((0.3..=0.4).contains(&at), "{at}");
    }

    #[test]
    fn loud_speech_fills_the_meter_and_no_more() {
        assert_eq!(level(8_000.0), 1.0);
        assert_eq!(level(f64::from(i16::MAX)), 1.0);
    }

    #[test]
    fn the_meter_rises_with_loudness() {
        let readings: Vec<f32> = [60.0, 150.0, 300.0, 1_000.0, 4_000.0]
            .into_iter()
            .map(level)
            .collect();
        assert!(readings.windows(2).all(|w| w[0] < w[1]), "{readings:?}");
    }

    #[test]
    fn updates_encode_as_the_documented_lines() {
        assert_eq!(Update::Idle.encode(), r#"{"phase":"idle"}"#);
        assert_eq!(Update::RECORDING.encode(), r#"{"phase":"recording"}"#);
        assert_eq!(
            Update::level(0.42).encode(),
            r#"{"phase":"recording","level":0.42}"#
        );
        assert_eq!(Update::Transcribing.encode(), r#"{"phase":"transcribing"}"#);
    }

    #[test]
    fn updates_round_trip_and_garbage_is_ignored() {
        for update in [
            Update::Idle,
            Update::RECORDING,
            Update::level(0.5),
            Update::Transcribing,
        ] {
            assert_eq!(Update::decode(update.encode().as_bytes()), Some(update));
        }
        assert_eq!(Update::decode(b"{\"phase\":\"dancing\"}"), None);
        assert_eq!(Update::decode(b"\xff"), None);
    }

    #[test]
    fn the_meter_reads_at_most_once_an_interval_over_what_it_skipped() {
        let mut meter = Meter::default();
        let start = Instant::now();
        let mut recorded = vec![0i16; 480];
        assert_eq!(meter.hear(&recorded, start), Some(0.0));

        // Loud audio arriving inside the interval is held, not lost.
        recorded.extend((0..480).map(|i| if i % 2 == 0 { 5_000 } else { -5_000 }));
        assert_eq!(
            meter.hear(&recorded, start + Duration::from_millis(30)),
            None
        );
        let later = meter.hear(&recorded, start + LEVEL_INTERVAL).unwrap();
        assert!(later > 0.8, "{later}");

        // Nothing new since the last reading is no reading.
        assert_eq!(meter.hear(&recorded, start + 3 * LEVEL_INTERVAL), None);
    }

    #[test]
    fn a_phase_prints_once_and_every_level_prints() {
        let mut shown = Shown::default();
        assert!(shown.admit(Update::Idle));
        assert!(!shown.admit(Update::Idle));
        assert!(shown.admit(Update::RECORDING));
        assert!(!shown.admit(Update::RECORDING));
        assert!(shown.admit(Update::level(0.2)));
        assert!(shown.admit(Update::level(0.2)));
        assert!(!shown.admit(Update::RECORDING), "a re-read adds nothing");
        assert!(shown.admit(Update::Transcribing));
        assert!(!shown.admit(Update::Transcribing));
        assert!(shown.admit(Update::Idle));
    }

    #[test]
    fn a_watcher_hears_what_the_broadcaster_sends() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = Watcher::bind(dir.path()).unwrap();
        let mut broadcaster = Broadcaster::new(dir.path());

        broadcaster.send(Update::RECORDING);
        broadcaster.send(Update::level(0.42));
        let timeout = Duration::from_secs(1);
        assert_eq!(watcher.recv(timeout), Some(Update::RECORDING));
        assert_eq!(watcher.recv(timeout), Some(Update::level(0.42)));
        assert_eq!(watcher.recv(Duration::from_millis(10)), None);
    }

    #[test]
    fn a_dropped_watcher_takes_its_socket_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = Watcher::bind(dir.path()).unwrap();
        let path = watcher.path.clone();
        assert!(path.exists());
        drop(watcher);
        assert!(!path.exists());
    }

    #[test]
    fn the_socket_of_a_killed_watcher_is_removed_and_others_kept() {
        let dir = tempfile::tempdir().unwrap();
        let watchers = dir.path().join(WATCHERS);
        std::fs::create_dir_all(&watchers).unwrap();
        // Bound and closed without unlinking, as a killed watcher leaves it.
        let dead = watchers.join("1.sock");
        drop(UnixDatagram::bind(&dead).unwrap());
        let other = watchers.join("notes.txt");
        std::fs::write(&other, "not a socket").unwrap();
        let live = Watcher::bind(dir.path()).unwrap();

        Broadcaster::new(dir.path()).send(Update::Transcribing);
        assert!(!dead.exists(), "the dead socket is gone");
        assert!(other.exists(), "only sockets are touched");
        assert_eq!(
            live.recv(Duration::from_secs(1)),
            Some(Update::Transcribing)
        );
    }

    #[test]
    fn no_watchers_directory_is_no_watchers() {
        let dir = tempfile::tempdir().unwrap();
        Broadcaster::new(dir.path()).send(Update::Idle);
        assert!(!dir.path().join(WATCHERS).exists());
    }

    #[test]
    fn a_remote_dictation_counts_until_it_expires() {
        let text = r#"{"phase":"transcribing","until":1000}"#;
        assert_eq!(remote_phase(text, 999), Some(Phase::Transcribing));
        assert_eq!(remote_phase(text, 1000), None);
        assert_eq!(remote_phase("{\"phase\":\"recording\"", 0), None);
    }

    #[test]
    fn the_state_files_decide_the_current_update() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(current(dir.path()), Update::Idle);

        let remote = dir.path().join(remote::REMOTE_STATE);
        let until = now_secs() + 60;
        std::fs::write(
            &remote,
            format!(r#"{{"phase":"recording","until":{until}}}"#),
        )
        .unwrap();
        assert_eq!(current(dir.path()), Update::RECORDING);

        std::fs::write(&remote, r#"{"phase":"recording","until":1}"#).unwrap();
        assert_eq!(current(dir.path()), Update::Idle, "expired");

        // A local state file whose recorder is not running is not believed.
        let local = dir.path().join(session::STATE_FILE);
        std::fs::write(
            &local,
            r#"{"pid":4294967295,"pane":"w1:p1","submit":false}"#,
        )
        .unwrap();
        assert_eq!(current(dir.path()), Update::Idle);
    }

    #[test]
    fn the_feed_starts_with_the_state_and_ends_on_stop() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        let stop = AtomicBool::new(true);
        run(dir.path(), &mut out, &stop, || false).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "{\"phase\":\"idle\"}\n");
        let watchers = dir.path().join(WATCHERS);
        assert_eq!(
            std::fs::read_dir(watchers).unwrap().count(),
            0,
            "socket removed"
        );
    }

    #[test]
    fn a_closed_reader_ends_the_feed_cleanly() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(false);
        run(dir.path(), &mut Closed, &stop, || false).unwrap();
    }
}
