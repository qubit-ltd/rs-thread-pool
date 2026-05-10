/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`qubit_thread_pool::ThreadPoolBuilder`].

use std::{io, sync::mpsc, time::Duration};

use qubit_thread_pool::{ExecutorService, RejectedExecution, ThreadPool, ThreadPoolBuildError};

use super::mod_tests::{create_runtime, wait_started, wait_until};

fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
}

#[test]
fn test_thread_pool_bounded_queue_rejects_when_saturated() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .queue_capacity(1)
        .build()
        .expect("thread pool should be created");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = pool
        .submit_tracked(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("first task should be accepted");
    wait_started(started_rx);

    let second = pool
        .submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("second task should fill the queue");
    let third = pool.submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(third, Err(RejectedExecution::Saturated)));
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first
        .get()
        .expect("first task should complete successfully");
    second
        .get()
        .expect("queued task should complete successfully");
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_thread_pool_grows_above_core_when_queue_is_full() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(2)
        .queue_capacity(1)
        .keep_alive(Duration::from_millis(50))
        .build()
        .expect("thread pool should be created");
    let (first_started_tx, first_started_rx) = mpsc::channel();
    let (third_started_tx, third_started_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let (release_third_tx, release_third_rx) = mpsc::channel();

    let first = pool
        .submit_tracked(move || {
            first_started_tx
                .send(())
                .expect("test should receive first start signal");
            release_first_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("first task should start on the core worker");
    wait_started(first_started_rx);

    let second = pool
        .submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("second task should be queued");
    let third = pool
        .submit_tracked(move || {
            third_started_tx
                .send(())
                .expect("test should receive third start signal");
            release_third_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("third task should create a non-core worker");
    wait_started(third_started_rx);

    let fourth = pool.submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(fourth, Err(RejectedExecution::Saturated)));
    assert_eq!(pool.stats().live_workers, 2);
    release_third_tx
        .send(())
        .expect("third task should receive release signal");
    third
        .get()
        .expect("third task should complete successfully");
    wait_until(|| pool.stats().live_workers == 1);
    release_first_tx
        .send(())
        .expect("first task should receive release signal");
    first
        .get()
        .expect("first task should complete successfully");
    second
        .get()
        .expect("queued task should complete successfully");
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_thread_pool_excess_workers_retire_after_maximum_size_decreases() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(2)
        .queue_capacity(1)
        .keep_alive(Duration::from_secs(5))
        .build()
        .expect("thread pool should be created");
    let (first_started_tx, first_started_rx) = mpsc::channel();
    let (third_started_tx, third_started_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let (release_third_tx, release_third_rx) = mpsc::channel();

    let first = pool
        .submit_tracked(move || {
            first_started_tx
                .send(())
                .expect("test should receive first start signal");
            release_first_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("first task should start on the core worker");
    wait_started(first_started_rx);

    let second = pool
        .submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("second task should be queued");
    let third = pool
        .submit_tracked(move || {
            third_started_tx
                .send(())
                .expect("test should receive third start signal");
            release_third_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("third task should create an extra worker");
    wait_started(third_started_rx);

    assert_eq!(pool.live_worker_count(), 2);
    pool.set_maximum_pool_size(1)
        .expect("maximum size should shrink to current core size");
    release_third_tx
        .send(())
        .expect("third task should receive release signal");

    third
        .get()
        .expect("third task should complete successfully");
    wait_until(|| pool.live_worker_count() == 1);
    release_first_tx
        .send(())
        .expect("first task should receive release signal");
    first
        .get()
        .expect("first task should complete successfully");
    second
        .get()
        .expect("queued task should complete successfully");
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_thread_pool_prestarts_core_workers() {
    let pool = ThreadPool::builder()
        .pool_size(2)
        .prestart_core_threads()
        .build()
        .expect("thread pool should be created");

    assert_eq!(pool.live_worker_count(), 2);
    assert_eq!(pool.stats().live_workers, 2);
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_thread_pool_prestart_core_thread_reports_state() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .build()
        .expect("thread pool should be created");

    assert!(pool.prestart_core_thread().expect("worker should start"));
    assert!(
        !pool
            .prestart_core_thread()
            .expect("no worker should be needed")
    );
    pool.shutdown();
    assert!(matches!(
        pool.prestart_core_thread(),
        Err(RejectedExecution::Shutdown),
    ));
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_thread_pool_prestart_all_core_threads_reports_state() {
    let pool = ThreadPool::builder()
        .pool_size(2)
        .build()
        .expect("thread pool should be created");

    assert_eq!(
        pool.prestart_all_core_threads()
            .expect("all core workers should start"),
        2,
    );
    assert_eq!(
        pool.prestart_all_core_threads()
            .expect("all core workers already started"),
        0,
    );
    pool.shutdown();
    assert!(matches!(
        pool.prestart_all_core_threads(),
        Err(RejectedExecution::Shutdown),
    ));
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_thread_pool_core_threads_can_timeout() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .keep_alive(Duration::from_millis(80))
        .allow_core_thread_timeout(true)
        .prestart_core_threads()
        .build()
        .expect("thread pool should be created");

    assert_eq!(pool.live_worker_count(), 1);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(pool.live_worker_count(), 1);
    wait_until(|| pool.live_worker_count() == 0);
    assert!(!pool.is_terminated());
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_thread_pool_builder_sets_unbounded_queue_and_thread_options() {
    let pool = ThreadPool::builder()
        .core_pool_size(0)
        .maximum_pool_size(1)
        .queue_capacity(1)
        .unbounded_queue()
        .thread_name_prefix("custom-pool")
        .stack_size(2 * 1024 * 1024)
        .keep_alive(Duration::from_millis(20))
        .allow_core_thread_timeout(true)
        .build()
        .expect("thread pool should be created");

    let name = pool
        .submit_callable(|| {
            Ok::<String, io::Error>(
                std::thread::current()
                    .name()
                    .expect("worker thread should have a name")
                    .to_owned(),
            )
        })
        .expect("task should be accepted")
        .get()
        .expect("task should complete");

    assert!(name.starts_with("custom-pool-"));
    wait_until(|| pool.live_worker_count() == 0);
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_thread_pool_prestart_reports_build_spawn_failure() {
    let result = ThreadPool::builder()
        .pool_size(1)
        .stack_size(usize::MAX)
        .prestart_core_threads()
        .build();

    assert!(matches!(
        result,
        Err(ThreadPoolBuildError::SpawnWorker { .. })
    ));
}

#[test]
fn test_thread_pool_builder_rejects_invalid_configuration() {
    assert!(matches!(
        ThreadPool::builder().pool_size(0).build(),
        Err(ThreadPoolBuildError::ZeroMaximumPoolSize),
    ));
    assert!(matches!(
        ThreadPool::builder().queue_capacity(0).build(),
        Err(ThreadPoolBuildError::ZeroQueueCapacity),
    ));
    assert!(matches!(
        ThreadPool::builder().stack_size(0).build(),
        Err(ThreadPoolBuildError::ZeroStackSize),
    ));
    assert!(matches!(
        ThreadPool::builder()
            .core_pool_size(2)
            .maximum_pool_size(1)
            .build(),
        Err(ThreadPoolBuildError::CorePoolSizeExceedsMaximum { .. }),
    ));
    assert!(matches!(
        ThreadPool::builder().keep_alive(Duration::ZERO).build(),
        Err(ThreadPoolBuildError::ZeroKeepAlive),
    ));
}
