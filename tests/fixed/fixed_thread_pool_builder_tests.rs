/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`FixedThreadPoolBuilder`](qubit_thread_pool::FixedThreadPoolBuilder).

use std::io;

use qubit_thread_pool::{
    ExecutorService,
    ExecutorServiceBuilderError,
    FixedThreadPool,
};

use super::mod_tests::wait_until;

#[test]
fn test_fixed_thread_pool_builder_rejects_invalid_configuration() {
    assert!(matches!(
        FixedThreadPool::builder().pool_size(0).build(),
        Err(ExecutorServiceBuilderError::ZeroMaximumPoolSize),
    ));
    assert!(matches!(
        FixedThreadPool::builder()
            .pool_size(1)
            .queue_capacity(0)
            .build(),
        Err(ExecutorServiceBuilderError::ZeroQueueCapacity),
    ));
    assert!(matches!(
        FixedThreadPool::builder()
            .pool_size(1)
            .stack_size(0)
            .build(),
        Err(ExecutorServiceBuilderError::ZeroStackSize),
    ));
}

#[test]
fn test_fixed_thread_pool_builder_reports_worker_spawn_failure() {
    let result = FixedThreadPool::builder()
        .pool_size(1)
        .stack_size(usize::MAX)
        .build();

    assert!(matches!(
        result,
        Err(ExecutorServiceBuilderError::SpawnWorker { .. })
    ));
}

#[test]
fn test_fixed_thread_pool_builder_sets_thread_options() {
    let pool = FixedThreadPool::builder()
        .pool_size(1)
        .queue_capacity(4)
        .unbounded_queue()
        .thread_name_prefix("fixed-custom")
        .stack_size(2 * 1024 * 1024)
        .build()
        .expect("fixed thread pool should be created with custom options");

    let name = pool
        .submit_callable(|| {
            Ok::<_, io::Error>(
                std::thread::current()
                    .name()
                    .expect("fixed worker should be named")
                    .to_owned(),
            )
        })
        .expect("fixed thread pool should accept callable")
        .get()
        .expect("callable should return worker name");

    assert!(name.starts_with("fixed-custom-"));
    assert_eq!(pool.pool_size(), 1);
    assert_eq!(pool.live_worker_count(), 1);
    assert_eq!(pool.queued_count(), 0);
    wait_until(|| pool.running_count() == 0);
    assert_eq!(pool.running_count(), 0);
    let stats = pool.stats();
    assert_eq!(stats.core_pool_size, 1);
    assert_eq!(stats.maximum_pool_size, 1);
    pool.shutdown();
    pool.wait_termination();
}
