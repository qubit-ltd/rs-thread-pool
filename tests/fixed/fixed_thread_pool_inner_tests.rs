/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::{
    io,
    sync::mpsc,
};

use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
};

/// Tests fixed-pool shared counters through public task execution.
#[test]
fn test_fixed_thread_pool_inner_tracks_public_task_counts() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");
    let (started_sender, started_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();

    pool.submit(move || {
        started_sender
            .send(())
            .expect("test should observe task start");
        release_receiver
            .recv()
            .expect("test should release running task");
        Ok::<_, io::Error>(())
    })
    .expect("task should be accepted");

    started_receiver
        .recv()
        .expect("worker should start submitted task");
    let running_stats = pool.stats();
    assert_eq!(running_stats.submitted_tasks, 1);
    assert_eq!(running_stats.running_tasks, 1);

    release_sender
        .send(())
        .expect("running task should still be waiting");
    pool.join();
    let idle_stats = pool.stats();
    assert_eq!(idle_stats.queued_tasks, 0);
    assert_eq!(idle_stats.running_tasks, 0);
    assert_eq!(idle_stats.completed_tasks, 1);

    pool.shutdown();
    pool.wait_termination();
}
