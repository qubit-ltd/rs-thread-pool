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
    panic::PanicHookInfo,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use qubit_thread_pool::{
    CancelResult, ExecutorService, ExecutorServiceBuilderError, PoolJob, SubmissionError,
    TaskExecutionError, ThreadPool,
};

use super::mod_tests::{create_single_worker_pool, wait_started};

static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

struct PanicHookGuard {
    previous_hook: Option<Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>>,
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

fn ok_usize_task() -> Result<usize, io::Error> {
    Ok(42)
}

#[test]
fn test_thread_pool_runs_configured_hooks() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let pool = ThreadPool::builder()
        .pool_size(1)
        .before_worker_start({
            let events = Arc::clone(&events);
            move |_| events.lock().expect("events should lock").push("start")
        })
        .before_task({
            let events = Arc::clone(&events);
            move |_| events.lock().expect("events should lock").push("before")
        })
        .after_task({
            let events = Arc::clone(&events);
            move |_| events.lock().expect("events should lock").push("after")
        })
        .after_worker_stop({
            let events = Arc::clone(&events);
            move |_| events.lock().expect("events should lock").push("stop")
        })
        .build()
        .expect("thread pool should be created");

    pool.submit(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("thread pool should accept task");
    pool.join();
    pool.shutdown();
    pool.wait_termination();

    let events = events.lock().expect("events should lock");
    assert!(events.contains(&"start"));
    assert!(events.contains(&"before"));
    assert!(events.contains(&"after"));
    assert!(events.contains(&"stop"));
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
fn test_thread_pool_submit_custom_job_runs_job() {
    let pool = ThreadPool::new(1).expect("thread pool should be created");
    let (done_tx, done_rx) = mpsc::channel();

    pool.submit_job(PoolJob::new(
        Box::new(move || {
            done_tx
                .send("run")
                .expect("test should receive custom job completion");
        }),
        Box::new(|| panic!("custom job should not be cancelled")),
    ))
    .expect("thread pool should accept custom job");

    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("custom job should run"),
        "run",
    );
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_submit_custom_job_accepts_and_cancels_queued_job() {
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
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();

    pool.submit_job(PoolJob::with_accept(
        Box::new(move || {
            accepted_tx
                .send(())
                .expect("test should receive custom job acceptance");
        }),
        Box::new(|| panic!("queued custom job should not run")),
        Box::new(move || {
            cancelled_tx
                .send(())
                .expect("test should receive custom job cancellation");
        }),
    ))
    .expect("thread pool should accept queued custom job");

    accepted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("custom job should be accepted");
    let report = pool.stop();
    assert_eq!(report.queued, 1);
    cancelled_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("queued custom job should be cancelled");
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    running.get().expect("running task should complete");
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

    assert!(matches!(result, Err(SubmissionError::Shutdown)));
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

    assert!(matches!(rejected, Err(SubmissionError::Shutdown)));
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
        Err(ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum { .. }),
    ));
    assert!(matches!(
        pool.set_maximum_pool_size(0),
        Err(ExecutorServiceBuilderError::ZeroMaximumPoolSize),
    ));
    assert!(pool.set_core_pool_size(2).is_ok());
    assert!(matches!(
        pool.set_maximum_pool_size(1),
        Err(ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum { .. }),
    ));
    assert!(matches!(
        pool.set_keep_alive(Duration::ZERO),
        Err(ExecutorServiceBuilderError::ZeroKeepAlive),
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
        Err(SubmissionError::WorkerSpawnFailed { .. }),
    ));
    assert!(
        accepted_rx.try_recv().is_err(),
        "rejected job must not cross the acceptance boundary",
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
        Err(SubmissionError::WorkerSpawnFailed { .. }),
    ));
    assert!(
        accepted_rx.try_recv().is_err(),
        "rejected queued-start job must not be accepted",
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

    pool.submit_job(PoolJob::with_accept(
        Box::new(|| panic!("custom accept panic should be isolated")),
        Box::new(|| panic!("job should not run when accept panics")),
        Box::new(|| panic!("running custom job should not be cancelled")),
    ))
    .expect("custom job should be accepted by the pool");

    super::mod_tests::wait_until(|| pool.stats().completed_tasks == 1);
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
    submit_result
        .expect("submit should not unwind")
        .expect("pool should accept the callback-failing job");
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
fn test_thread_pool_wait_termination_waits_for_custom_cancel_callback() {
    let pool = Arc::new(create_single_worker_pool());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_running_tx, release_running_rx) = mpsc::channel();
    let running = pool
        .submit_tracked(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_running_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    wait_started(started_rx);
    let (cancel_started_tx, cancel_started_rx) = mpsc::channel();
    let (release_cancel_tx, release_cancel_rx) = mpsc::channel();

    pool.submit_job(PoolJob::new(
        Box::new(|| panic!("queued job should not run after stop")),
        Box::new(move || {
            cancel_started_tx
                .send(())
                .expect("test should receive cancel start signal");
            release_cancel_rx
                .recv()
                .expect("test should release cancel callback");
        }),
    ))
    .expect("queued custom job should be accepted");

    let stop_pool = Arc::clone(&pool);
    let stop_thread = std::thread::spawn(move || stop_pool.stop());
    cancel_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("custom cancel callback should start");

    release_running_tx
        .send(())
        .expect("running task should receive release signal");
    running.get().expect("running task should complete");
    super::mod_tests::wait_until(|| pool.live_worker_count() == 0);

    let (terminated_tx, terminated_rx) = mpsc::channel();
    let wait_pool = Arc::clone(&pool);
    let wait_thread = std::thread::spawn(move || {
        wait_pool.wait_termination();
        terminated_tx
            .send(())
            .expect("test should receive termination signal");
    });

    let terminated_before_cancel_completed = terminated_rx
        .recv_timeout(Duration::from_millis(50))
        .is_ok();
    release_cancel_tx
        .send(())
        .expect("cancel callback should receive release signal");
    let report = stop_thread.join().expect("stop thread should not panic");
    assert_eq!(report.queued, 1);
    assert_eq!(report.cancelled, 1);
    if !terminated_before_cancel_completed {
        terminated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("termination should complete after cancel callback returns");
    }
    wait_thread
        .join()
        .expect("wait thread should not panic after termination");
    assert!(
        !terminated_before_cancel_completed,
        "termination must wait until cancel callback completes",
    );
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
