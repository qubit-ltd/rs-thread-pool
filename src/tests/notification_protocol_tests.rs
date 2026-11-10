// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Deterministic production-path tests for pool notification interleavings.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use qubit_executor::service::ExecutorService;

use super::notification_control::NotificationControl;
use super::notification_control::Phase;
use crate::PoolJob;
use crate::PoolJobSubmissionError;
use crate::dynamic::ThreadPool;
use crate::fixed::FixedThreadPool;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

trait ProtocolPool: Sized + Send + Sync + 'static {
    fn create() -> Self;
    fn control(&self) -> Arc<NotificationControl>;
    fn submit(&self, job: PoolJob) -> Result<(), PoolJobSubmissionError>;
    fn graceful_shutdown(&self);
    fn wait_terminated(&self) -> bool;
    fn wait_idle(&self);
}

impl ProtocolPool for ThreadPool {
    fn create() -> Self {
        Self::builder()
            .pool_size(1)
            .queue_capacity(1)
            .build()
            .expect("dynamic pool builds")
    }

    fn control(&self) -> Arc<NotificationControl> {
        self.notification_control()
    }

    fn submit(&self, job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        self.submit_job(job)
    }

    fn graceful_shutdown(&self) {
        self.shutdown();
    }

    fn wait_terminated(&self) -> bool {
        self.wait_termination_timeout(TEST_TIMEOUT)
    }

    fn wait_idle(&self) {
        self.join();
    }
}

impl ProtocolPool for FixedThreadPool {
    fn create() -> Self {
        Self::builder()
            .pool_size(1)
            .queue_capacity(1)
            .build()
            .expect("fixed pool builds")
    }

    fn control(&self) -> Arc<NotificationControl> {
        self.notification_control()
    }

    fn submit(&self, job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        self.submit_job(job)
    }

    fn graceful_shutdown(&self) {
        self.shutdown();
    }

    fn wait_terminated(&self) -> bool {
        self.wait_termination_timeout(TEST_TIMEOUT)
    }

    fn wait_idle(&self) {
        self.join();
    }
}

fn receive<T>(receiver: &mpsc::Receiver<T>, context: &str) -> T {
    receiver
        .recv_timeout(TEST_TIMEOUT)
        .unwrap_or_else(|error| panic!("{context}: {error}"))
}

fn warm_worker<P: ProtocolPool>(pool: &P) {
    let control = pool.control();
    let (ran_tx, ran_rx) = mpsc::channel();
    pool.submit(PoolJob::new(
        Box::new(move || ran_tx.send(()).expect("warm-up observed")),
        Box::new(|| {}),
    ))
    .expect("warm-up job accepted");
    control.wait_for(Phase::JobPublished);
    receive(&ran_rx, "worker warm-up completes");
    pool.wait_idle();
}

fn idle_publish<P: ProtocolPool>() {
    let pool = Arc::new(P::create());
    let control = pool.control();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = PoolJob::new(
        Box::new(move || {
            started_tx.send(()).expect("blocker started");
            receive(&release_rx, "blocker released");
        }),
        Box::new(|| {}),
    );
    pool.submit(first).expect("blocker accepted");
    control.wait_for(Phase::JobPublished);
    receive(&started_rx, "worker starts blocker");

    let acceptance_gate = control.arm(Phase::AcceptanceEntered);
    let (ran_tx, ran_rx) = mpsc::channel();
    let task = PoolJob::with_accept(
        Box::new(|| {}),
        Box::new(move || ran_tx.send(()).expect("published task ran")),
        Box::new(|| {}),
    );
    let submit_pool = Arc::clone(&pool);
    let (submitted_tx, submitted_rx) = mpsc::channel();
    let submitter = thread::spawn(move || submitted_tx.send(submit_pool.submit(task)).expect("submit result"));
    acceptance_gate.wait_arrived();

    let idle_gate = control.arm(Phase::IdleWorkerWaitBoundary);
    release_tx.send(()).expect("release blocker");
    idle_gate.wait_arrived();
    acceptance_gate.release();
    control.wait_for(Phase::JobPublished);
    idle_gate.release();
    receive(&submitted_rx, "single publication completes submission").expect("task accepted");
    receive(&ran_rx, "single publication wakes idle worker");
    submitter.join().expect("submitter joins");
    pool.graceful_shutdown();
    assert!(pool.wait_terminated(), "idle-publish teardown completes");
}

fn shutdown_interleaving<P: ProtocolPool>(acceptance_panics: bool) {
    let pool = Arc::new(P::create());
    warm_worker(pool.as_ref());
    let control = pool.control();
    let acceptance_gate = control.arm(Phase::AcceptanceEntered);
    let (ran_tx, ran_rx) = mpsc::channel();
    let accept: Box<dyn FnOnce() + Send> = if acceptance_panics {
        Box::new(|| panic!("injected acceptance rollback"))
    } else {
        Box::new(|| {})
    };
    let job = PoolJob::with_accept(
        accept,
        Box::new(move || ran_tx.send(()).expect("accepted task ran")),
        Box::new(|| {}),
    );
    let submit_pool = Arc::clone(&pool);
    let (submitted_tx, submitted_rx) = mpsc::channel();
    let submitter = thread::spawn(move || submitted_tx.send(submit_pool.submit(job)).expect("submit result"));
    acceptance_gate.wait_arrived();

    let shutdown_gate = control.arm(Phase::ShutdownWaitBoundary);
    pool.graceful_shutdown();
    shutdown_gate.wait_arrived();
    shutdown_gate.release();

    let departure_gate = control.arm(Phase::SubmitterDeparture);
    acceptance_gate.release();
    departure_gate.wait_arrived();
    departure_gate.release();
    let result = receive(&submitted_rx, "submitter leaves after shutdown");
    submitter.join().expect("submitter joins");
    if acceptance_panics {
        assert_eq!(result, Err(PoolJobSubmissionError::AcceptancePanicked));
        assert!(ran_rx.try_recv().is_err(), "rolled-back task is not published");
    } else {
        result.expect("already-admitted task remains accepted");
        receive(&ran_rx, "accepted task runs after shutdown");
    }
    assert!(
        pool.wait_terminated(),
        "shutdown completes after final admission leaves"
    );
}

fn join_rollback<P: ProtocolPool>() {
    let pool = Arc::new(P::create());
    warm_worker(pool.as_ref());
    let control = pool.control();
    let acceptance_gate = control.arm(Phase::AcceptanceEntered);
    let job = PoolJob::with_accept(
        Box::new(|| panic!("injected acceptance rollback")),
        Box::new(|| panic!("rolled-back task must not run")),
        Box::new(|| {}),
    );
    let submit_pool = Arc::clone(&pool);
    let (submitted_tx, submitted_rx) = mpsc::channel();
    let submitter = thread::spawn(move || submitted_tx.send(submit_pool.submit(job)).expect("submit result"));
    acceptance_gate.wait_arrived();

    let idle_wait_gate = control.arm(Phase::IdleWaitBoundary);
    let join_pool = Arc::clone(&pool);
    let (joined_tx, joined_rx) = mpsc::channel();
    let joiner = thread::spawn(move || {
        join_pool.wait_idle();
        joined_tx.send(()).expect("join result");
    });
    idle_wait_gate.wait_arrived();
    idle_wait_gate.release();

    let departure_gate = control.arm(Phase::SubmitterDeparture);
    acceptance_gate.release();
    departure_gate.wait_arrived();
    departure_gate.release();
    assert_eq!(
        receive(&submitted_rx, "failed acceptance exits admission"),
        Err(PoolJobSubmissionError::AcceptancePanicked),
    );
    receive(&joined_rx, "join wakes after rollback and final admission exit");
    submitter.join().expect("submitter joins");
    joiner.join().expect("joiner joins");
    let (reused_tx, reused_rx) = mpsc::channel();
    pool.submit(PoolJob::new(
        Box::new(move || reused_tx.send(()).expect("reused queue slot task ran")),
        Box::new(|| {}),
    ))
    .expect("rolled-back acceptance releases bounded capacity");
    receive(&reused_rx, "released bounded queue slot is reusable");
    pool.graceful_shutdown();
    assert!(pool.wait_terminated(), "join-rollback teardown completes");
}

#[test]
fn test_notification_protocol_idle_publish() {
    idle_publish::<ThreadPool>();
    idle_publish::<FixedThreadPool>();
}

#[test]
fn test_notification_protocol_shutdown_publish() {
    shutdown_interleaving::<ThreadPool>(false);
    shutdown_interleaving::<FixedThreadPool>(false);
}

#[test]
fn test_notification_protocol_shutdown_rollback() {
    shutdown_interleaving::<ThreadPool>(true);
    shutdown_interleaving::<FixedThreadPool>(true);
}

#[test]
fn test_notification_protocol_join_rollback() {
    join_rollback::<ThreadPool>();
    join_rollback::<FixedThreadPool>();
}
