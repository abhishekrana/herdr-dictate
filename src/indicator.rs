//! What the pane shows while a dictation is in progress.
//!
//! The label carries a TTL and is refreshed while this process lives, so a
//! recorder that is killed leaves no stale indicator behind.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::ipc::Client;

/// Long enough to survive a slow refresh, short enough that a dead process
/// clears quickly.
const TTL: Duration = Duration::from_millis(3_000);
const REFRESH: Duration = Duration::from_millis(1_000);

/// Shows a label on a pane until dropped.
pub struct Indicator {
    label: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    client: Client,
    pane: String,
}

impl Indicator {
    pub fn show(client: Client, pane: String, label: impl Into<String>) -> Self {
        let label = Arc::new(Mutex::new(label.into()));
        let stop = Arc::new(AtomicBool::new(false));

        let worker = std::thread::spawn({
            let (client, pane) = (client.clone(), pane.clone());
            let (label, stop) = (Arc::clone(&label), Arc::clone(&stop));
            move || {
                while !stop.load(Ordering::Relaxed) {
                    let text = label.lock().map(|l| l.clone()).unwrap_or_default();
                    // A failure here costs an indicator, never a transcript.
                    if let Err(err) = client.set_pane_label(&pane, &text, TTL) {
                        tracing::debug!(%err, "indicator");
                    }
                    std::thread::sleep(REFRESH);
                }
            }
        });

        Self {
            label,
            stop,
            worker: Some(worker),
            client,
            pane,
        }
    }

    /// Change what the pane shows.
    pub fn set(&self, label: impl Into<String>) {
        if let Ok(mut current) = self.label.lock() {
            *current = label.into();
        }
        let text = self.label.lock().map(|l| l.clone()).unwrap_or_default();
        let _ = self.client.set_pane_label(&self.pane, &text, TTL);
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = self.client.clear_pane_label(&self.pane);
    }
}
