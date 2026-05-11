/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`qubit_thread_pool::ThreadPool`].

use std::{
    io,
    sync::{
        Arc,
        atomic::{
            AtomicBool,
            Ordering,
        },
        mpsc,
    },
    time::Duration,
};

use qubit_thread_pool::{
    CancelResult,
    ExecutorBuildError,
    ExecutorService,
    RejectedExecution,
    TaskExecutionError,
    ThreadPool,
};

use super::mod_tests::{
    create_single_worker_pool,
    wait_started,
};

fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
}

fn ok_usize_task() -> Result<usize, io::Error> {
    Ok(42)
}

#[test]
fn test_thread_pool_submit_acceptance_is_not_task_success() {
    let pool = ThreadPool::new(2).expect("thread pool should be created");

    pool.submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("thread pool should accept shared runnable")
        .get()
        .expect("shared runnable should complete successfully");

    let handle = pool
        .submit_tracked(|| Err::<(), _>(io::Error::other("task failed")))
        .expect("thread pool should accept runnable");

    let err = handle
        .get()
        .expect_err("accepted runnable should report task failure through handle");
    assert!(matches!(err, TaskExecutionError::Failed(_)));
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_submit_callable_returns_value() {
    let pool = ThreadPool::new(2).expect("thread pool should be created");

    let handle = pool
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("thread pool should accept callable");

    assert_eq!(
        handle.get().expect("callable should complete successfully"),
        42,
    );
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_submit_and_join_wait_for_detached_task() {
    let pool = ThreadPool::new(1).expect("thread pool should be created");
    let completed = Arc::new(AtomicBool::new(false));
    let completed_for_task = Arc::clone(&completed);

    pool.submit(move || {
        completed_for_task.store(true, Ordering::Release);
        Ok::<(), io::Error>(())
    })
    .expect("thread pool should accept detached task");
    pool.join();

    assert!(completed.load(Ordering::Acquire));
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_submit_wakes_prestarted_idle_worker() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(1)
        .build()
        .expect("thread pool should be created");

    assert!(
        pool.prestart_core_thread()
            .expect("core worker should prestart")
    );
    super::mod_tests::wait_until(|| pool.stats().idle_workers == 1);
    let handle = pool
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("thread pool should accept callable");

    assert_eq!(handle.get().expect("callable should complete"), 42);
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_bounded_submit_uses_worker_queue_when_worker_busy() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(1)
        .queue_capacity(4)
        .build()
        .expect("thread pool should be created");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = pool
        .submit_tracked(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    wait_started(started_rx);
    let queued = pool
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    assert_eq!(pool.queued_count(), 1);
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    running.get().expect("running task should complete");
    assert_eq!(queued.get().expect("queued task should complete"), 42);
    pool.shutdown();
    pool.wait_termination();
}

#[tokio::test]
async fn test_thread_pool_handle_can_be_awaited() {
    let pool = ThreadPool::new(2).expect("thread pool should be created");

    let handle = pool
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("thread pool should accept callable");

    assert_eq!(handle.await.expect("handle should await result"), 42);
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_shutdown_rejects_new_tasks() {
    let pool = ThreadPool::new(1).expect("thread pool should be created");

    pool.shutdown();
    let result = pool.submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(result, Err(RejectedExecution::Shutdown)));
    pool.wait_termination();
    assert!(pool.is_not_running());
    assert!(pool.is_terminated());
}

#[test]
fn test_thread_pool_shutdown_drains_queued_tasks() {
    let pool = create_single_worker_pool();
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
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    pool.shutdown();
    let rejected = pool.submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>);
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first
        .get()
        .expect("first task should complete successfully");

    assert!(matches!(rejected, Err(RejectedExecution::Shutdown)));
    assert_eq!(second.get().expect("queued task should still run"), 42);
    pool.wait_termination();
    assert!(pool.is_terminated());
}

#[test]
fn test_thread_pool_stop_cancels_queued_tasks() {
    let pool = create_single_worker_pool();
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
    let queued = pool
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    let report = pool.stop();

    assert_eq!(report.queued, 1);
    assert_eq!(report.running, 1);
    assert_eq!(report.cancelled, 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled),));
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    pool.wait_termination();
    assert!(pool.is_terminated());
}

#[test]
fn test_thread_pool_stop_is_idempotent_from_stopping() {
    let pool = ThreadPool::new(1).expect("thread pool should be created");

    let first = pool.stop();
    let second = pool.stop();

    assert_eq!(first.queued, 0);
    assert_eq!(first.running, 0);
    assert_eq!(second.queued, 0);
    assert_eq!(second.running, 0);
    pool.wait_termination();
    assert!(pool.is_terminated());
}

#[test]
fn test_thread_pool_cancel_before_start_reports_cancelled() {
    let pool = create_single_worker_pool();
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
    let queued = pool
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    assert_eq!(queued.cancel(), CancelResult::Cancelled);
    assert!(queued.is_done());
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled),));
    pool.shutdown();
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    pool.wait_termination();
}

#[test]
fn test_thread_pool_accessors_and_dynamic_settings() {
    let pool = ThreadPool::builder()
        .core_pool_size(0)
        .maximum_pool_size(2)
        .queue_capacity(1)
        .build()
        .expect("thread pool should be created");

    assert_eq!(pool.queued_count(), 0);
    assert_eq!(pool.running_count(), 0);
    assert_eq!(pool.live_worker_count(), 0);
    assert_eq!(pool.core_pool_size(), 0);
    assert_eq!(pool.maximum_pool_size(), 2);
    assert!(pool.set_core_pool_size(1).is_ok());
    assert!(pool.set_maximum_pool_size(3).is_ok());
    assert!(pool.set_keep_alive(Duration::from_millis(25)).is_ok());
    pool.allow_core_thread_timeout(true);
    assert_eq!(pool.core_pool_size(), 1);
    assert_eq!(pool.maximum_pool_size(), 3);
    assert!(matches!(
        pool.set_core_pool_size(4),
        Err(ExecutorBuildError::CorePoolSizeExceedsMaximum { .. }),
    ));
    assert!(matches!(
        pool.set_maximum_pool_size(0),
        Err(ExecutorBuildError::ZeroMaximumPoolSize),
    ));
    assert!(pool.set_core_pool_size(2).is_ok());
    assert!(matches!(
        pool.set_maximum_pool_size(1),
        Err(ExecutorBuildError::CorePoolSizeExceedsMaximum { .. }),
    ));
    assert!(matches!(
        pool.set_keep_alive(Duration::ZERO),
        Err(ExecutorBuildError::ZeroKeepAlive),
    ));
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_reports_worker_spawn_failure() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .stack_size(usize::MAX)
        .build()
        .expect("thread pool should be created lazily");

    let result = pool.submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(
        result,
        Err(RejectedExecution::WorkerSpawnFailed { .. }),
    ));
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_cancels_queued_job_when_initial_worker_spawn_fails() {
    let pool = ThreadPool::builder()
        .core_pool_size(0)
        .maximum_pool_size(1)
        .queue_capacity(1)
        .stack_size(usize::MAX)
        .build()
        .expect("thread pool should be created lazily");

    let result = pool.submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(
        result,
        Err(RejectedExecution::WorkerSpawnFailed { .. }),
    ));
    assert_eq!(pool.queued_count(), 0);
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_rejected_execution_compares_by_variant() {
    let left = RejectedExecution::WorkerSpawnFailed {
        source: Arc::new(io::Error::other("left")),
    };
    let right = RejectedExecution::WorkerSpawnFailed {
        source: Arc::new(io::Error::other("right")),
    };

    assert_eq!(left, right);
    assert_ne!(RejectedExecution::Shutdown, RejectedExecution::Saturated);
    assert_eq!(
        RejectedExecution::Saturated.to_string(),
        "task rejected because the executor service is saturated",
    );
}

#[test]
fn test_thread_pool_stop_after_shutdown_is_idempotent() {
    let pool = ThreadPool::new(1).expect("thread pool should be created");

    pool.shutdown();
    let report = pool.stop();

    assert_eq!(report.queued, 0);
    assert_eq!(report.running, 0);
    assert_eq!(report.cancelled, 0);
    pool.wait_termination();
    assert!(pool.is_terminated());
}
