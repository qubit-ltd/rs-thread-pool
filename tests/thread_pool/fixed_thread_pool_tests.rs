/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! Tests for [`FixedThreadPool`](qubit_thread_pool::service::FixedThreadPool).

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
    TaskExecutionError,
    service::{
        ExecutorService,
        FixedThreadPool,
        RejectedExecution,
        ThreadPoolBuildError,
    },
};

use super::{
    create_runtime,
    wait_started,
    wait_until,
};

fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
}

fn ok_usize_task() -> Result<usize, io::Error> {
    Ok(42)
}

fn create_single_worker_pool() -> FixedThreadPool {
    FixedThreadPool::new(1).expect("fixed thread pool should be created")
}

#[test]
fn test_fixed_thread_pool_builder_rejects_invalid_configuration() {
    assert!(matches!(
        FixedThreadPool::builder().pool_size(0).build(),
        Err(ThreadPoolBuildError::ZeroMaximumPoolSize),
    ));
    assert!(matches!(
        FixedThreadPool::builder()
            .pool_size(1)
            .queue_capacity(0)
            .build(),
        Err(ThreadPoolBuildError::ZeroQueueCapacity),
    ));
    assert!(matches!(
        FixedThreadPool::builder()
            .pool_size(1)
            .stack_size(0)
            .build(),
        Err(ThreadPoolBuildError::ZeroStackSize),
    ));
}

#[test]
fn test_fixed_thread_pool_reports_worker_spawn_failure() {
    let result = FixedThreadPool::builder()
        .pool_size(1)
        .stack_size(usize::MAX)
        .build();

    assert!(matches!(
        result,
        Err(ThreadPoolBuildError::SpawnWorker { .. })
    ));
}

#[test]
fn test_fixed_thread_pool_submit_acceptance_is_not_task_success() {
    let pool = FixedThreadPool::new(2).expect("fixed thread pool should be created");

    pool.submit(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("fixed thread pool should accept shared runnable")
        .get()
        .expect("shared runnable should complete successfully");

    let handle = pool
        .submit(|| Err::<(), _>(io::Error::other("task failed")))
        .expect("fixed thread pool should accept runnable");

    let err = handle
        .get()
        .expect_err("accepted runnable should report task failure through handle");
    assert!(matches!(err, TaskExecutionError::Failed(_)));
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_fixed_thread_pool_submit_callable_returns_value() {
    let pool = FixedThreadPool::new(2).expect("fixed thread pool should be created");

    let handle = pool
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("fixed thread pool should accept callable");

    assert_eq!(
        handle.get().expect("callable should complete successfully"),
        42,
    );
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[tokio::test]
async fn test_fixed_thread_pool_handle_can_be_awaited() {
    let pool = FixedThreadPool::new(2).expect("fixed thread pool should be created");

    let handle = pool
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("fixed thread pool should accept callable");

    assert_eq!(handle.await.expect("handle should await result"), 42);
    pool.shutdown();
    pool.await_termination().await;
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
    assert_eq!(pool.running_count(), 0);
    let stats = pool.stats();
    assert_eq!(stats.core_pool_size, 1);
    assert_eq!(stats.maximum_pool_size, 1);
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_fixed_thread_pool_shutdown_rejects_new_tasks() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should be created");

    pool.shutdown();
    let result = pool.submit(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(result, Err(RejectedExecution::Shutdown)));
    create_runtime().block_on(pool.await_termination());
    assert!(pool.is_shutdown());
    assert!(pool.is_terminated());
}

#[test]
fn test_fixed_thread_pool_bounded_queue_rejects_when_saturated() {
    let pool = FixedThreadPool::builder()
        .pool_size(1)
        .queue_capacity(1)
        .build()
        .expect("fixed thread pool should be created");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = pool
        .submit(move || {
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

    let saturated = pool.submit(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(saturated, Err(RejectedExecution::Saturated)));
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    assert_eq!(queued.get().expect("queued task should run"), 42);
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_fixed_thread_pool_shutdown_drains_queued_tasks() {
    let pool = create_single_worker_pool();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = pool
        .submit(move || {
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
    let rejected = pool.submit(ok_unit_task as fn() -> Result<(), io::Error>);
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first
        .get()
        .expect("first task should complete successfully");

    assert!(matches!(rejected, Err(RejectedExecution::Shutdown)));
    assert_eq!(second.get().expect("queued task should still run"), 42);
    create_runtime().block_on(pool.await_termination());
    assert!(pool.is_terminated());
}

#[test]
fn test_fixed_thread_pool_shutdown_now_cancels_queued_tasks() {
    let pool = create_single_worker_pool();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = pool
        .submit(move || {
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

    let report = pool.shutdown_now();

    assert_eq!(report.queued, 1);
    assert_eq!(report.running, 1);
    assert_eq!(report.cancelled, 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled),));
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    create_runtime().block_on(pool.await_termination());
    assert!(pool.is_terminated());
}

#[test]
fn test_fixed_thread_pool_cancel_before_start_reports_cancelled() {
    let pool = create_single_worker_pool();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = pool
        .submit(move || {
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

    assert!(queued.cancel());
    assert!(queued.is_done());
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled),));
    pool.shutdown();
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_fixed_thread_pool_await_termination_waits_for_running_task() {
    let pool = create_single_worker_pool();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_for_task = Arc::clone(&completed);

    pool.submit(move || {
        std::thread::sleep(Duration::from_millis(80));
        completed_for_task.store(true, Ordering::Release);
        Ok::<(), io::Error>(())
    })
    .expect("fixed thread pool should accept task");

    pool.shutdown();
    create_runtime().block_on(pool.await_termination());

    assert!(pool.is_terminated());
    assert!(completed.load(Ordering::Acquire));
}

#[test]
fn test_fixed_thread_pool_multiple_workers_drain_local_queues() {
    let pool = FixedThreadPool::new(2).expect("fixed thread pool should be created");
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();

    for _ in 0..16 {
        let counter_for_task = Arc::clone(&counter);
        handles.push(
            pool.submit(move || {
                counter_for_task.fetch_add(1, Ordering::AcqRel);
                Ok::<(), io::Error>(())
            })
            .expect("fixed thread pool should accept task"),
        );
    }

    for handle in handles {
        handle.get().expect("task should complete");
    }
    wait_until(|| counter.load(Ordering::Acquire) == 16);
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_fixed_thread_pool_large_pool_uses_global_queue_shutdown_now() {
    let pool = FixedThreadPool::new(5).expect("fixed thread pool should be created");
    let release = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = mpsc::channel();
    let mut running = Vec::new();

    for _ in 0..5 {
        let release_for_task = Arc::clone(&release);
        let started_tx = started_tx.clone();
        running.push(
            pool.submit(move || {
                started_tx
                    .send(())
                    .expect("test should receive task start signal");
                while !release_for_task.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok::<(), io::Error>(())
            })
            .expect("fixed thread pool should accept blocking task"),
        );
    }
    drop(started_tx);
    for _ in 0..5 {
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task should start within timeout");
    }

    let queued = pool
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");
    wait_until(|| pool.queued_count() == 1);

    let report = pool.shutdown_now();

    assert_eq!(report.queued, 1);
    assert_eq!(report.cancelled, 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    release.store(true, Ordering::Release);
    for handle in running {
        handle.get().expect("running task should complete");
    }
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_fixed_thread_pool_large_pool_runs_global_queue_tasks() {
    let pool = FixedThreadPool::new(5).expect("fixed thread pool should be created");
    let mut handles = Vec::new();

    for value in 0..10usize {
        handles.push(
            pool.submit_callable(move || Ok::<usize, io::Error>(value))
                .expect("fixed thread pool should accept callable"),
        );
    }

    let mut values = handles
        .into_iter()
        .map(|handle| handle.get().expect("callable should complete"))
        .collect::<Vec<_>>();
    values.sort_unstable();
    assert_eq!(values, (0..10usize).collect::<Vec<_>>());
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_fixed_thread_pool_shutdown_now_cancels_worker_local_batch() {
    let pool = create_single_worker_pool();
    let release = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = mpsc::channel();

    let first = {
        let release_for_task = Arc::clone(&release);
        pool.submit(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            while !release_for_task.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok::<(), io::Error>(())
        })
        .expect("first task should be accepted")
    };
    wait_started(started_rx);

    let mut queued = Vec::new();
    for _ in 0..8 {
        queued.push(
            pool.submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
                .expect("queued task should be accepted"),
        );
    }
    wait_until(|| pool.queued_count() == 8);

    let report = pool.shutdown_now();

    assert_eq!(report.running, 1);
    assert!(report.queued <= 8);
    release.store(true, Ordering::Release);
    first.get().expect("running task should complete");
    create_runtime().block_on(pool.await_termination());

    let mut cancelled = 0usize;
    for handle in queued {
        if matches!(handle.get(), Err(TaskExecutionError::Cancelled)) {
            cancelled += 1;
        }
    }
    assert_eq!(cancelled, 8);
}
