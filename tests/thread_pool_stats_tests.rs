/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`qubit_thread_pool::ThreadPoolStats`].

use qubit_thread_pool::{ExecutorService, ExecutorServiceLifecycle, ThreadPool};

#[test]
fn test_thread_pool_stats_reflect_configuration() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(3)
        .queue_capacity(2)
        .build()
        .expect("thread pool should be created");

    let s = pool.stats();
    assert_eq!(s.core_pool_size, 1);
    assert_eq!(s.maximum_pool_size, 3);
    assert_eq!(s.queued_tasks, 0);
    assert_eq!(s.lifecycle, ExecutorServiceLifecycle::Running);
    assert!(!s.terminated);

    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
    let s = pool.stats();
    assert_eq!(s.lifecycle, ExecutorServiceLifecycle::Terminated);
    assert!(s.terminated);
}

fn create_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime should build for stats tests")
}
