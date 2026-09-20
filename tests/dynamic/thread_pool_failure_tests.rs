// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Failure-path tests for [`qubit_thread_pool::ThreadPool`].

use std::io;
use std::panic::PanicHookInfo;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_thread_pool::PoolJob;
use qubit_thread_pool::PoolJobSubmissionError;
use qubit_thread_pool::ThreadPool;

use super::mod_tests::create_single_worker_pool;
use super::mod_tests::wait_started;

static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

struct PanicHookGuard {
    previous_hook: Option<PanicHook>,
}

impl PanicHookGuard {
    fn suppress() -> Self {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        Self {
            previous_hook: Some(previous_hook),
        }
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if let Some(previous_hook) = self.previous_hook.take() {
            std::panic::set_hook(previous_hook);
        }
    }
}

fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
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
        Err(SubmissionError::WorkerSpawnFailed { .. }),
    ));
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_spawn_failure_does_not_accept_or_cancel_direct_job() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .stack_size(usize::MAX)
        .build()
        .expect("thread pool should be created lazily");
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();

    let result = pool.submit_job(PoolJob::with_accept(
        Box::new(move || {
            accepted_tx
                .send(())
                .expect("test should receive acceptance signal");
        }),
        Box::new(|| panic!("custom job should not run when worker spawn fails")),
        Box::new(move || {
            cancelled_tx
                .send(())
                .expect("test should receive cancellation signal");
        }),
    ));

    assert!(matches!(
        result,
        Err(PoolJobSubmissionError::Rejected(
            SubmissionError::WorkerSpawnFailed { .. }
        )),
    ));
    assert!(
        accepted_rx.try_recv().is_err(),
        "a worker-spawn rejection must happen before the acceptance callback",
    );
    assert!(
        cancelled_rx.try_recv().is_err(),
        "rejected job must not be cancelled as accepted queued work",
    );
    assert_eq!(pool.stats().submitted_tasks, 0);
    assert_eq!(pool.queued_count(), 0);
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
        Err(SubmissionError::WorkerSpawnFailed { .. }),
    ));
    assert_eq!(pool.queued_count(), 0);
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_spawn_failure_does_not_accept_or_cancel_queued_start_job() {
    let pool = ThreadPool::builder()
        .core_pool_size(0)
        .maximum_pool_size(1)
        .queue_capacity(1)
        .stack_size(usize::MAX)
        .build()
        .expect("thread pool should be created lazily");
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();

    let result = pool.submit_job(PoolJob::with_accept(
        Box::new(move || {
            accepted_tx
                .send(())
                .expect("test should receive acceptance signal");
        }),
        Box::new(|| panic!("custom job should not run when worker spawn fails")),
        Box::new(move || {
            cancelled_tx
                .send(())
                .expect("test should receive cancellation signal");
        }),
    ));

    assert!(matches!(
        result,
        Err(PoolJobSubmissionError::Rejected(
            SubmissionError::WorkerSpawnFailed { .. }
        )),
    ));
    assert!(
        accepted_rx.try_recv().is_err(),
        "a worker-spawn rejection must happen before the acceptance callback",
    );
    assert!(
        cancelled_rx.try_recv().is_err(),
        "rejected queued-start job must not be cancelled",
    );
    assert_eq!(pool.stats().submitted_tasks, 0);
    assert_eq!(pool.queued_count(), 0);
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_custom_job_panic_does_not_kill_worker_or_leak_running_count() {
    let _panic_hook_lock = PANIC_HOOK_LOCK
        .lock()
        .expect("panic hook lock should not be poisoned");
    let _panic_hook_guard = PanicHookGuard::suppress();
    let pool = ThreadPool::new(1).expect("thread pool should be created");
    pool.submit_job(PoolJob::new(
        Box::new(|| panic!("custom job panic should be isolated")),
        Box::new(|| panic!("running custom job should not be cancelled")),
    ))
    .expect("custom job should be accepted");

    super::mod_tests::wait_until(|| pool.stats().completed_tasks == 1);
    assert_eq!(pool.running_count(), 0);

    let (done_tx, done_rx) = mpsc::channel();
    pool.submit(move || {
        done_tx
            .send(())
            .expect("test should receive second task completion");
        Ok::<(), io::Error>(())
    })
    .expect("worker should still accept work after custom job panic");

    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("worker should continue processing later work");
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_initial_accept_panic_does_not_kill_worker_or_leak_running_count() {
    let _panic_hook_lock = PANIC_HOOK_LOCK
        .lock()
        .expect("panic hook lock should not be poisoned");
    let _panic_hook_guard = PanicHookGuard::suppress();
    let pool = ThreadPool::new(1).expect("thread pool should be created");

    let result = pool.submit_job(PoolJob::with_accept(
        Box::new(|| panic!("custom accept panic should be isolated")),
        Box::new(|| panic!("job should not run when accept panics")),
        Box::new(|| panic!("running custom job should not be cancelled")),
    ));
    assert!(matches!(
        result,
        Err(PoolJobSubmissionError::AcceptancePanicked)
    ));

    super::mod_tests::wait_until(|| pool.stats().completed_tasks == 0);
    assert_eq!(pool.running_count(), 0);

    let (done_tx, done_rx) = mpsc::channel();
    pool.submit(move || {
        done_tx
            .send(())
            .expect("test should receive later task completion");
        Ok::<(), io::Error>(())
    })
    .expect("worker should still accept work after accept panic");

    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("worker should continue processing later work");
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_queued_accept_panic_does_not_unwind_or_leak_state() {
    let _panic_hook_lock = PANIC_HOOK_LOCK
        .lock()
        .expect("panic hook lock should not be poisoned");
    let _panic_hook_guard = PanicHookGuard::suppress();
    let pool = create_single_worker_pool();
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
    let ran = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));

    let submit_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
        let ran = Arc::clone(&ran);
        let cancelled = Arc::clone(&cancelled);
        || {
            pool.submit_job(PoolJob::with_accept(
                Box::new(|| panic!("custom accept panic should be isolated")),
                Box::new(move || {
                    ran.store(true, Ordering::Release);
                }),
                Box::new(move || {
                    cancelled.store(true, Ordering::Release);
                }),
            ))
        }
    }));

    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    running.get().expect("running task should complete");
    pool.join();

    assert!(submit_result.is_ok(), "submit must contain accept panic");
    assert!(matches!(
        submit_result.expect("submit should not unwind"),
        Err(PoolJobSubmissionError::AcceptancePanicked)
    ));
    assert!(
        !ran.load(Ordering::Acquire),
        "job must not run after its accept callback panics",
    );
    assert!(
        !cancelled.load(Ordering::Acquire),
        "job must not be cancelled after its accept callback panics",
    );
    assert_eq!(pool.queued_count(), 0);
    assert_eq!(pool.running_count(), 0);
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_stop_contains_custom_cancel_panic() {
    let _panic_hook_lock = PANIC_HOOK_LOCK
        .lock()
        .expect("panic hook lock should not be poisoned");
    let _panic_hook_guard = PanicHookGuard::suppress();
    let pool = create_single_worker_pool();
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
    pool.submit_job(PoolJob::new(
        Box::new(|| panic!("queued job should not run after stop")),
        Box::new(|| panic!("custom cancel panic should be isolated")),
    ))
    .expect("queued custom job should be accepted");

    let stop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool.stop()));
    release_tx
        .send(())
        .expect("running task should receive release signal");
    running.get().expect("running task should complete");
    pool.wait_termination();

    assert!(stop_result.is_ok(), "stop must contain cancel panic");
    let report = stop_result.expect("stop should not unwind");
    assert_eq!(report.queued, 1);
    assert_eq!(report.cancelled, 1);
    assert!(pool.is_terminated());
}

#[test]
fn test_rejected_execution_compares_by_variant() {
    let left = SubmissionError::WorkerSpawnFailed {
        source: Arc::new(io::Error::other("left")),
    };
    let right = SubmissionError::WorkerSpawnFailed {
        source: Arc::new(io::Error::other("right")),
    };

    assert_eq!(left, right);
    assert_ne!(SubmissionError::Shutdown, SubmissionError::Saturated);
    assert_eq!(
        SubmissionError::Saturated.to_string(),
        "task rejected because the executor service is saturated",
    );
}
