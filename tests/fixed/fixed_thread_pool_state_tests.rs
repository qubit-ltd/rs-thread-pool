/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
};

/// Tests fixed-pool state snapshots through the public stats API.
#[test]
fn test_fixed_thread_pool_state_is_reflected_in_stats() {
    let pool = FixedThreadPool::new(2).expect("fixed thread pool should build");
    let running_stats = pool.stats();

    assert_eq!(running_stats.lifecycle, ExecutorServiceLifecycle::Running);
    assert_eq!(running_stats.core_pool_size, 2);
    assert_eq!(running_stats.maximum_pool_size, 2);
    assert_eq!(running_stats.live_workers, 2);
    assert!(!running_stats.terminated);

    pool.shutdown();
    pool.wait_termination();

    let terminated_stats = pool.stats();
    assert_eq!(
        terminated_stats.lifecycle,
        ExecutorServiceLifecycle::Terminated
    );
    assert_eq!(terminated_stats.live_workers, 0);
    assert!(terminated_stats.terminated);
}
