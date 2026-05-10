/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`DelayedTaskHandle`](qubit_thread_pool::DelayedTaskHandle).

use std::{sync::mpsc, time::Duration};

use qubit_thread_pool::DelayedTaskScheduler;

use super::mod_tests::create_runtime;

#[test]
fn test_delayed_task_handle_reports_cancelled_state() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-handle").expect("scheduler should start");
    let (sent_tx, sent_rx) = mpsc::channel::<()>();

    let handle = scheduler
        .schedule(Duration::from_millis(100), move || {
            sent_tx.send(()).expect("cancelled task should not send");
        })
        .expect("delay should schedule");

    assert!(!handle.is_cancelled());
    assert!(handle.cancel());
    assert!(handle.is_cancelled());
    assert!(
        sent_rx.recv_timeout(Duration::from_millis(140)).is_err(),
        "cancelled task should not run"
    );
    scheduler.shutdown();
    create_runtime().block_on(scheduler.await_termination());
}
