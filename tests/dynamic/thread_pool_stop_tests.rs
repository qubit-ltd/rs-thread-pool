// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Stop and cancellation tests for [`qubit_thread_pool::ThreadPool`].

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::CancelResult;
use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_thread_pool::PoolJob;
use qubit_thread_pool::ThreadPool;

use super::mod_tests::create_single_worker_pool;
use super::mod_tests::wait_started;

fn ok_usize_task() -> Result<usize, io::Error> {
    Ok(42)
}

#[test]
fn test_thread_pool_stop_cancels_queued_tasks() {
    let pool = create_single_worker_pool();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = pool
        .submit_tracked(move || {
            started_tx.send(()).expect("test should receive task start signal");
            release_rx.recv().map_err(|err| io::Error::other(err.to_string()))?;
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
fn test_thread_pool_stop_does_not_report_ticket_cancellation_as_its_own() {
    let pool = Arc::new(ThreadPool::new(1).expect("thread pool should be created"));
    let (worker_started_tx, worker_started_rx) = mpsc::channel();
    let (release_worker_tx, release_worker_rx) = mpsc::channel();
    pool.submit_job(PoolJob::new(
        Box::new(move || {
            worker_started_tx.send(()).expect("worker should start");
            release_worker_rx.recv().expect("worker should be released");
        }),
        Box::new(|| {}),
    ))
    .expect("blocking job should be accepted");
    worker_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker should start");

    let (cancel_started_tx, cancel_started_rx) = mpsc::channel();
    let (release_cancel_tx, release_cancel_rx) = mpsc::channel();
    let (job, ticket) = pool.prepare_cancellable_job(
        Box::new(|| {}),
        Box::new(|| panic!("cancelled job must not run")),
        Box::new(move || {
            cancel_started_tx.send(()).expect("cancellation should start");
            release_cancel_rx.recv().expect("cancellation should be released");
        }),
    );
    pool.submit_job(job).expect("ticketed job should be accepted");
    let (cancel_result_tx, cancel_result_rx) = mpsc::channel();
    std::thread::spawn(move || {
        cancel_result_tx
            .send(ticket.cancel_queued())
            .expect("cancellation result should be received");
    });
    cancel_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("ticket should remove the job and start cancellation");

    let report = pool.stop();
    assert_eq!(report.queued, 0);
    assert_eq!(report.cancelled, 0);
    assert_eq!(report.running, 1);

    release_cancel_tx.send(()).expect("cancellation should be released");
    assert!(
        cancel_result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ticket cancellation should finish")
    );
    release_worker_tx.send(()).expect("worker should be released");
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
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
            started_tx.send(()).expect("test should receive task start signal");
            release_rx.recv().map_err(|err| io::Error::other(err.to_string()))?;
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
fn test_thread_pool_stop_waits_for_inflight_accept_then_cancels() {
    let pool = Arc::new(
        ThreadPool::builder()
            .pool_size(1)
            .prestart_core_threads()
            .build()
            .expect("thread pool should be created"),
    );
    super::mod_tests::wait_until(|| pool.stats().idle_workers == 1);
    let ran = Arc::new(AtomicBool::new(false));
    let (accept_started_tx, accept_started_rx) = mpsc::channel();
    let (release_accept_tx, release_accept_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    let submit_pool = Arc::clone(&pool);
    let submit_ran = Arc::clone(&ran);
    let submit_thread = std::thread::spawn(move || {
        submit_pool.submit_job(PoolJob::with_accept(
            Box::new(move || {
                accept_started_tx
                    .send(())
                    .expect("test should receive accept start signal");
                release_accept_rx.recv().expect("test should release accept callback");
            }),
            Box::new(move || {
                submit_ran.store(true, Ordering::Release);
            }),
            Box::new(move || {
                cancelled_tx.send(()).expect("test should receive cancellation signal");
            }),
        ))
    });
    accept_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("accept callback should start");
    let stop_pool = Arc::clone(&pool);
    let (stop_tx, stop_rx) = mpsc::channel();
    let stop_thread = std::thread::spawn(move || {
        let report = stop_pool.stop();
        stop_tx.send(report).expect("test should receive stop report");
    });

    assert!(
        stop_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "stop should wait while submit is inside accept",
    );
    release_accept_tx
        .send(())
        .expect("accept callback should receive release signal");
    submit_thread
        .join()
        .expect("submit caller should not panic")
        .expect("in-flight submit should be accepted before stop drains");
    let report = stop_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("stop should finish after accept is released");
    stop_thread.join().expect("stop caller should not panic");

    assert_eq!(report.queued, 1);
    assert_eq!(report.cancelled, 1);
    cancelled_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("accepted queued job should be cancelled");
    assert!(
        !ran.load(Ordering::Acquire),
        "accepted queued job must not run after stop",
    );
    pool.wait_termination();
    assert!(pool.is_terminated());
}

#[test]
fn test_stop_cancels_spawned_worker_job_while_acceptance_is_inflight() {
    let pool = Arc::new(
        ThreadPool::builder()
            .pool_size(1)
            .build()
            .expect("thread pool should be created"),
    );
    let (accept_started_tx, accept_started_rx) = mpsc::channel();
    let (release_accept_tx, release_accept_rx) = mpsc::channel();
    let (ran_tx, ran_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    let submit_pool = Arc::clone(&pool);
    let submit_thread = std::thread::spawn(move || {
        submit_pool.submit_job(PoolJob::with_accept(
            Box::new(move || {
                accept_started_tx
                    .send(())
                    .expect("test should receive accept start signal");
                release_accept_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("test should release accept callback");
            }),
            Box::new(move || {
                ran_tx.send(()).expect("run callback should be observable");
            }),
            Box::new(move || {
                cancelled_tx.send(()).expect("cancel callback should be observable");
            }),
        ))
    });

    accept_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("accept callback should start");
    let stop_pool = Arc::clone(&pool);
    let stop_thread = std::thread::spawn(move || stop_pool.stop());
    let stop_started = (0..100).any(|_| {
        if pool.lifecycle() == ExecutorServiceLifecycle::Stopping {
            true
        } else {
            std::thread::sleep(Duration::from_millis(1));
            false
        }
    });
    assert!(stop_started, "stop should enter stopping state");
    release_accept_tx
        .send(())
        .expect("accept callback should receive release signal");

    submit_thread
        .join()
        .expect("submit thread should not panic")
        .expect("accepted job should report success");
    let report = stop_thread.join().expect("stop thread should not panic");
    assert_eq!(report.cancelled, 1);
    assert!(
        cancelled_rx.recv_timeout(Duration::from_secs(1)).is_ok(),
        "accepted job should be cancelled after stop"
    );
    assert!(
        ran_rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "cancelled job must not run"
    );
    pool.wait_termination();
    assert_eq!(pool.running_count(), 0);
    assert_eq!(pool.queued_count(), 0);
}

#[test]
fn test_thread_pool_repeated_stop_does_not_recount_in_progress_cancellation() {
    let pool = Arc::new(create_single_worker_pool());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_running_tx, release_running_rx) = mpsc::channel();
    let running = pool
        .submit_tracked(move || {
            started_tx.send(()).expect("test should receive task start signal");
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
            release_cancel_rx.recv().expect("test should release cancel callback");
        }),
    ))
    .expect("queued custom job should be accepted");

    let stop_pool = Arc::clone(&pool);
    let stop_thread = std::thread::spawn(move || stop_pool.stop());
    cancel_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first stop should start cancellation");

    let repeated = pool.stop();
    assert_eq!(repeated.queued, 0);
    assert_eq!(repeated.cancelled, 0);

    release_cancel_tx
        .send(())
        .expect("cancel callback should receive release signal");
    let first = stop_thread.join().expect("stop thread should not panic");
    assert_eq!(first.queued, 1);
    assert_eq!(first.cancelled, 1);

    release_running_tx
        .send(())
        .expect("running task should receive release signal");
    running.get().expect("running task should complete");
    pool.wait_termination();
    assert!(pool.is_terminated());
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
