//! The engine's health: whether the change source is delivering.

use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};

/// Whether the engine's index is being kept up to date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineHealth {
    /// The change source is delivering normally.
    Live,
    /// The change source stopped or failed and is being restarted, or a full
    /// rescan is catching up after lost history. The previous snapshot keeps
    /// serving.
    Recovering,
    /// Restarting the change source keeps failing (the last error is
    /// given). The writer keeps retrying with growing waits; the previous
    /// snapshot keeps serving.
    Failed(String),
}

/// The current health plus the channels told about every change.
pub(crate) struct HealthState {
    current: Mutex<EngineHealth>,
    subscribers: Mutex<Vec<Sender<EngineHealth>>>,
}

impl HealthState {
    pub(crate) fn new(initial: EngineHealth) -> HealthState {
        HealthState {
            current: Mutex::new(initial),
            subscribers: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn get(&self) -> EngineHealth {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Sets the health; subscribers are told only when it changes.
    pub(crate) fn set(&self, health: EngineHealth) {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *current == health {
            return;
        }
        *current = health.clone();
        // Sent while `current` is held, so subscribers see changes in order.
        let mut subs = self
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        subs.retain(|tx| tx.send(health.clone()).is_ok());
    }

    pub(crate) fn subscribe(&self) -> Receiver<EngineHealth> {
        let (tx, rx) = mpsc::channel();
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(tx);
        rx
    }
}
