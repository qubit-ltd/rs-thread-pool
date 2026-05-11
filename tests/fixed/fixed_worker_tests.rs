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
    sync::{
        Arc,
        atomic::{
            AtomicUsize,
            Ordering,
        },
    },
};

use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
};

/// Tests fixed workers execute submitted public tasks.
#[test]
fn test_fixed_worker_runs_public_submitted_task() {
    let pool = FixedThreadPool::new(2).expect("fixed thread pool should build");
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..8 {
        let counter = Arc::clone(&counter);
        pool.submit(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok::<_, io::Error>(())
        })
        .expect("task should be accepted");
    }

    pool.join();
    assert_eq!(counter.load(Ordering::Relaxed), 8);

    pool.shutdown();
    pool.wait_termination();
}
