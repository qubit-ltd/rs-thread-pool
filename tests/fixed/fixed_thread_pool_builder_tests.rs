// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Tests for [`FixedThreadPoolBuilder`](qubit_thread_pool::FixedThreadPoolBuilder).

use std::io;

use qubit_executor::service::ExecutorService;
use qubit_executor::service::ExecutorServiceBuilderError;
use qubit_thread_pool::FixedThreadPool;

use super::mod_tests::wait_until;

#[test]
fn test_fixed_thread_pool_builder_rejects_invalid_configuration() {
    assert!(matches!(
        FixedThreadPool::builder().pool_size(0).build(),
        Err(ExecutorServiceBuilderError::ZeroPoolSize),
    ));
    assert!(matches!(
        FixedThreadPool::builder().pool_size(1).queue_capacity(0).build(),
        Err(ExecutorServiceBuilderError::ZeroQueueCapacity),
    ));
    assert!(matches!(
        FixedThreadPool::builder().pool_size(1).stack_size(0).build(),
        Err(ExecutorServiceBuilderError::ZeroStackSize),
    ));
}

#[test]
fn test_fixed_thread_pool_builder_rejects_nul_thread_name_prefix_without_panicking() {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        FixedThreadPool::builder()
            .pool_size(1)
            .thread_name_prefix("bad\0prefix")
            .build()
    }));
    let result = result.expect("invalid thread name prefix should not panic");
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("invalid thread name prefix should fail validation"),
    };
    match error {
        ExecutorServiceBuilderError::SpawnWorker { source, .. } => {
            assert_eq!(source.kind(), io::ErrorKind::InvalidInput);
        }
        other => panic!("expected SpawnWorker, got {other:?}"),
    }
}

#[test]
fn test_fixed_thread_pool_builder_accepts_unicode_and_empty_thread_name_prefix() {
    for prefix in ["固定池-🌱", ""] {
        let pool = FixedThreadPool::builder()
            .pool_size(1)
            .thread_name_prefix(prefix)
            .build()
            .expect("valid thread name prefix should build");
        let name = pool
            .submit_callable(|| {
                Ok::<_, io::Error>(
                    std::thread::current()
                        .name()
                        .expect("worker should have a name")
                        .to_owned(),
                )
            })
            .expect("pool should accept work")
            .get()
            .expect("worker task should complete");
        if !prefix.is_empty() {
            assert!(name.starts_with(prefix));
        }
        pool.shutdown();
        assert!(pool.wait_termination_timeout(std::time::Duration::from_secs(2)));
    }
}

#[test]
fn test_fixed_thread_pool_builder_reports_worker_spawn_failure() {
    let result = FixedThreadPool::builder().pool_size(1).stack_size(usize::MAX).build();

    assert!(matches!(result, Err(ExecutorServiceBuilderError::SpawnWorker { .. })));
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
