//! What the pane shows while a dictation is in progress.
//!
//! The label carries a TTL and is refreshed while this process lives, so a
//! recorder that is killed leaves no stale indicator behind. Cadence comes
//! from the sink: a remote refresh costs a round trip, a local one does not.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::sink::Sink;

/// A host that has gone away fails every refresh, and each failure costs a
/// timeout. Give up and let the label expire instead.
const MAX_FAILURES: u32 = 3;

/// Shows a label on a pane until dropped.
pub struct Indicator {
    label: Arc<Mutex<String>>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
    sink: Sink,
    pane: String,
}

impl Indicator {
    pub fn show(sink: Sink, pane: String, label: impl Into<String>) -> Self {
        let label = Arc::new(Mutex::new(label.into()));
        let stop = Arc::new((Mutex::new(false), Condvar::new()));

        let worker = std::thread::spawn({
            let (sink, pane) = (sink.clone(), pane.clone());
            let (label, stop) = (Arc::clone(&label), Arc::clone(&stop));
            let (ttl, refresh) = (sink.ttl(), sink.refresh());
            move || {
                let (lock, cvar) = &*stop;
                let mut failures = 0;
                loop {
                    let text = label.lock().map(|l| l.clone()).unwrap_or_default();
                    // A failure here costs an indicator, never a transcript.
                    match sink.set_label(&pane, &text, ttl) {
                        Ok(()) => failures = 0,
                        Err(err) => {
                            tracing::debug!(%err, "indicator");
                            failures += 1;
                            if failures >= MAX_FAILURES {
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
            label,
            stop,
            worker: Some(worker),
            sink,
            pane,
        }
    }

    /// Change what the pane shows.
    pub fn set(&self, label: impl Into<String>) {
        let text = label.into();
        if let Ok(mut current) = self.label.lock() {
            current.clone_from(&text);
        }
        let _ = self.sink.set_label(&self.pane, &text, self.sink.ttl());
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
        let _ = self.sink.clear_label(&self.pane);
    }
}
