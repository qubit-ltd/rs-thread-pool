// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Test-only scheduling controls for notification protocol regressions.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// Production-path phase at which a test can pause a worker or submitter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    /// Worker observed an empty queue immediately before parking while running.
    IdleWaitBoundary,
    /// Idle worker observed an empty queue immediately before parking.
    IdleWorkerWaitBoundary,
    /// Shutting-down worker observed in-flight admission immediately before
    /// parking.
    ShutdownWaitBoundary,
    /// Submitter reserved queue capacity and reached its acceptance boundary.
    AcceptanceEntered,
    /// Last submitter left admission immediately before its monitor
    /// notification.
    SubmitterDeparture,
    /// Job accounting and queue publication completed before worker
    /// notification.
    JobPublished,
}

struct ArmedGate {
    phase: Phase,
    release: mpsc::Receiver<()>,
    arrived: mpsc::Sender<()>,
}

/// One-pool scheduling control. It is inert unless a one-shot gate is armed.
pub(crate) struct NotificationControl {
    armed: Mutex<Option<ArmedGate>>,
    events_tx: mpsc::Sender<Phase>,
    events_rx: Mutex<mpsc::Receiver<Phase>>,
}

impl NotificationControl {
    /// Creates an inert per-pool controller for deterministic test scheduling.
    pub(crate) fn new() -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        Self {
            armed: Mutex::new(None),
            events_tx,
            events_rx: Mutex::new(events_rx),
        }
    }

    /// Arms a single matching production-path phase.
    pub(super) fn arm(self: &Arc<Self>, phase: Phase) -> Gate {
        let (release_tx, release_rx) = mpsc::channel();
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let previous = self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(ArmedGate {
                phase,
                release: release_rx,
                arrived: arrived_tx,
            });
        assert!(previous.is_none(), "only one notification gate may be armed per pool");
        Gate {
            phase,
            arrival: arrived_rx,
            release: release_tx,
        }
    }

    /// Reports an observable phase without pausing.
    pub(crate) fn signal(&self, phase: Phase) {
        let _ = self.events_tx.send(phase);
    }

    /// Waits for and verifies a non-blocking phase event.
    pub(super) fn wait_for(&self, expected: Phase) {
        let event = self
            .events_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv_timeout(CONTROL_TIMEOUT)
            .unwrap_or_else(|error| panic!("did not observe {:?} phase: {error}", expected));
        assert_eq!(event, expected, "unexpected notification phase event");
    }

    /// Pauses at the next matching point, reporting arrival before blocking.
    pub(crate) fn pause(&self, phase: Phase) {
        let release = {
            let mut armed = self.armed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if armed.as_ref().is_some_and(|gate| gate.phase == phase) {
                armed.take().map(|gate| (gate.release, gate.arrived))
            } else {
                None
            }
        };
        if let Some((release, arrived)) = release {
            let _ = arrived.send(());
            let _ = release.recv_timeout(CONTROL_TIMEOUT);
        }
    }
}

/// Test-side handle for observing and releasing one pause.
pub(super) struct Gate {
    phase: Phase,
    arrival: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

impl Gate {
    /// Waits until the production path reaches this gate.
    pub(super) fn wait_arrived(&self) {
        self.arrival
            .recv_timeout(CONTROL_TIMEOUT)
            .unwrap_or_else(|error| panic!("did not reach {:?} phase: {error}", self.phase));
    }

    /// Releases a paused production thread.
    pub(super) fn release(&self) {
        let _ = self.release.send(());
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = self.release.send(());
    }
}
