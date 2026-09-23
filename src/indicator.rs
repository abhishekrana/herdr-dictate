//! What the pane shows while a dictation is in progress.
//!
//! The label carries a TTL and is refreshed while this process lives, so a
//! recorder that is killed leaves no stale indicator behind. Cadence comes
//! from the sink: a remote refresh costs a round trip, a local one does not.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::session::Phase;
use crate::sink::Sink;

/// A host that has gone away fails every refresh, and each failure costs a
/// timeout. Give up and let the label expire instead.
const MAX_FAILURES: u32 = 3;

/// Shows a label on a pane until dropped.
pub struct Indicator {
    phase: Arc<Mutex<Phase>>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
    sink: Sink,
    pane: String,
}

impl Indicator {
    pub fn show(sink: Sink, pane: String, phase: Phase) -> Self {
        let phase = Arc::new(Mutex::new(phase));
        let stop = Arc::new((Mutex::new(false), Condvar::new()));

        let worker = std::thread::spawn({
            let (sink, pane) = (sink.clone(), pane.clone());
            let (phase, stop) = (Arc::clone(&phase), Arc::clone(&stop));
            let (ttl, refresh) = (sink.ttl(), sink.refresh());
            move || {
                let (lock, cvar) = &*stop;
                let mut failures = 0;
                loop {
                    let now = phase.lock().map(|p| *p).unwrap_or_default();
                    // A failure here costs an indicator, never a transcript.
                    match sink.set_label(&pane, now, ttl) {
                        Ok(()) => failures = 0,
                        Err(err) => {
                            failures += 1;
                            tracing::warn!(at = sink.describe(), %pane, failures, %err, "indicator not set");
                            if failures >= MAX_FAILURES {
                                tracing::warn!(at = sink.describe(), %pane, "indicator given up; it expires on its own");
                                return;
                            }
                        }
                    }
                    let Ok(stopped) = lock.lock() else { return };
                    // Waiting rather than sleeping means Drop does not hold
                    // delivery back for the rest of a refresh interval.
                    let Ok((stopped, _)) = cvar.wait_timeout(stopped, refresh) else {
                        return;
                    };
                    if *stopped {
                        return;
                    }
                }
            }
        });

        Self {
            phase,
            stop,
            worker: Some(worker),
            sink,
            pane,
        }
    }

    /// Change what the pane shows.
    pub fn set(&self, phase: Phase) {
        if let Ok(mut current) = self.phase.lock() {
            *current = phase;
        }
        if let Err(err) = self.sink.set_label(&self.pane, phase, self.sink.ttl()) {
            tracing::warn!(at = self.sink.describe(), pane = %self.pane, %err, "indicator not set");
        }
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.stop;
        if let Ok(mut stopped) = lock.lock() {
            *stopped = true;
            cvar.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Err(err) = self.sink.clear_label(&self.pane) {
            tracing::warn!(at = self.sink.describe(), pane = %self.pane, %err, "indicator not cleared");
        }
    }
}
