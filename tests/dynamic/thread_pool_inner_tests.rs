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
fn test_thread_pool_inner_tracks_running_queued_and_completed_counts() {
    let pool = create_single_worker_pool();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = pool
        .submit_tracked(move || {
            started_tx.send(()).expect("task start signal should be sent");
            release_rx.recv().map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    wait_started(started_rx);
    let queued = pool
        .submit_callable(|| Ok::<_, io::Error>(11))
        .expect("queued task should be accepted");

    assert_eq!(
        1,
        pool.running_count(),
        "one task should be running before the first task is released"
    );
    assert_eq!(
        1,
        pool.queued_count(),
        "one task should remain queued before the first task is released"
    );
    release_tx.send(()).expect("running task should be released");
    first.get().expect("running task should complete");
    assert_eq!(
        11,
        queued.get().expect("queued task should complete"),
        "queued task should return its submitted value"
    );
    pool.shutdown();
    pool.wait_termination();
    assert!(
        pool.stats().completed_tasks >= 2,
        "running and queued tasks should both be counted as completed"
    );
}
