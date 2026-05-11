/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`DelayedTaskScheduler`](qubit_thread_pool::DelayedTaskScheduler).

use std::{
    sync::mpsc,
    time::Duration,
};

use qubit_thread_pool::{
    DelayedTaskScheduler,
    ExecutorBuildError,
};

#[test]
fn test_delayed_task_scheduler_runs_earliest_deadline_first() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-scheduler").expect("scheduler should start");
    let (sent_tx, sent_rx) = mpsc::channel::<&'static str>();

    for _ in 0..8 {
        let sent_tx = sent_tx.clone();
        scheduler
            .schedule(Duration::from_millis(250), move || {
                sent_tx.send("long").expect("long task should send");
            })
            .expect("long delay should schedule");
    }
    scheduler
        .schedule(Duration::from_millis(30), move || {
            sent_tx.send("short").expect("short delay should send");
        })
        .expect("short delay should schedule");

    assert_eq!(
        sent_rx
            .recv_timeout(Duration::from_millis(150))
            .expect("short delay should not wait behind long delays"),
        "short"
    );
    scheduler.shutdown();
    scheduler.wait_termination();
}

#[test]
fn test_delayed_task_scheduler_reports_spawn_failure() {
    let result = DelayedTaskScheduler::with_stack_size(
        "test-delayed-scheduler-spawn-failure",
        Some(usize::MAX),
    );

    assert!(matches!(
        result,
        Err(ExecutorBuildError::SpawnWorker { .. })
    ));
}

#[test]
fn test_delayed_task_scheduler_cancel_skips_pending_task() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-scheduler-cancel").expect("scheduler should start");
    let (sent_tx, sent_rx) = mpsc::channel::<()>();

    let handle = scheduler
        .schedule(Duration::from_millis(120), move || {
            sent_tx.send(()).expect("cancelled task should not send");
        })
        .expect("delay should schedule");

    assert!(handle.cancel());
    assert!(!handle.cancel());
    assert!(
        sent_rx.recv_timeout(Duration::from_millis(180)).is_err(),
        "cancelled task should not run"
    );
    scheduler.shutdown();
    scheduler.wait_termination();
    assert!(scheduler.is_terminated());
}

#[test]
fn test_delayed_task_scheduler_stop_cancels_pending_task() {
    let scheduler = DelayedTaskScheduler::new("test-delayed-scheduler-stop-now")
        .expect("scheduler should start");
    let handle = scheduler
        .schedule(Duration::from_secs(10), || {})
        .expect("delayed task should schedule");

    let report = scheduler.stop();

    assert_eq!(report.queued, 1);
    assert_eq!(report.cancelled, 1);
    assert!(handle.is_cancelled());
    scheduler.wait_termination();
}
