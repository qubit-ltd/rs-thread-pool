// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::io;
use std::sync::mpsc;

use qubit_executor::service::ExecutorService;

use super::mod_tests::create_single_worker_pool;
use super::mod_tests::wait_started;

#[test]
fn test_thread_pool_state_reports_queue_saturation_and_shutdown_cancellation() {
    let pool = create_single_worker_pool();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = pool
        .submit_tracked(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .unwrap();
    wait_started(started_rx);
    let queued = pool.submit_tracked(|| Ok::<_, io::Error>(())).unwrap();
    let report = pool.stop();

    assert_eq!(1, report.queued);
    assert_eq!(1, report.cancelled);
    assert!(queued.is_done());
    release_tx.send(()).unwrap();
    running.get().unwrap();
    pool.wait_termination();

    let stats = pool.stats();
    assert_eq!(stats.submitted_tasks, 2);
    assert_eq!(stats.completed_tasks, 1);
    assert_eq!(stats.cancelled_tasks, 1);
    assert!(stats.terminated);
}
