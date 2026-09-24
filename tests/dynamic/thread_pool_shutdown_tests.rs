// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Shutdown tests for [`qubit_thread_pool::ThreadPool`].

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_thread_pool::PoolJob;
use qubit_thread_pool::ThreadPool;

use super::mod_tests::create_single_worker_pool;
use super::mod_tests::wait_started;

fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
}

fn ok_usize_task() -> Result<usize, io::Error> {
    Ok(42)
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
            started_tx.send(()).expect("test should receive task start signal");
            release_rx.recv().map_err(|err| io::Error::other(err.to_string()))?;
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
    first.get().expect("first task should complete successfully");

    assert!(matches!(rejected, Err(SubmissionError::Shutdown)));
    assert_eq!(second.get().expect("queued task should still run"), 42);
    pool.wait_termination();
    assert!(pool.is_terminated());
}

#[test]
fn test_thread_pool_accept_callback_can_request_shutdown() {
    let pool = Arc::new(
        ThreadPool::builder()
            .pool_size(1)
            .prestart_core_threads()
            .build()
            .expect("thread pool should be created"),
    );
    super::mod_tests::wait_until(|| pool.stats().idle_workers == 1);
    let callback_pool = Arc::clone(&pool);
    let result = pool.submit_job(PoolJob::with_accept(
        Box::new(move || callback_pool.shutdown()),
        Box::new(|| {}),
        Box::new(|| panic!("accepted job should not be cancelled by shutdown")),
    ));
    result.expect("shutdown from an acceptance callback should not deadlock");
    pool.wait_termination();
    assert!(pool.is_terminated());
}

#[test]
fn test_lazy_pool_accept_shutdown_still_runs_job() {
    let pool = Arc::new(ThreadPool::new(1).expect("pool should be created"));
    let callback_pool = Arc::clone(&pool);
    let (ran_tx, ran_rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = mpsc::channel();
    pool.submit_job(PoolJob::with_accept(
        Box::new(move || callback_pool.shutdown()),
        Box::new(move || ran_tx.send(()).expect("run signal")),
        Box::new(move || cancel_tx.send(()).expect("cancel signal")),
    ))
    .expect("in-flight job should be accepted");
    pool.wait_termination();
    ran_rx.recv_timeout(Duration::from_secs(1)).expect("job should run");
    assert!(cancel_rx.try_recv().is_err());
}

#[test]
fn test_lazy_pool_shutdown_during_acceptance_still_runs_job() {
    let pool = Arc::new(ThreadPool::new(1).expect("pool should be created"));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (ran_tx, ran_rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let submit_pool = Arc::clone(&pool);
    let submitter = std::thread::spawn(move || {
        submit_pool.submit_job(PoolJob::with_accept(
            Box::new(move || {
                entered_tx.send(()).expect("accept entered");
                release_rx.recv().expect("accept released");
            }),
            Box::new(move || ran_tx.send(()).expect("run signal")),
            Box::new(move || cancel_tx.send(()).expect("cancel signal")),
        ))
    });
    entered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("accept should start");
    pool.shutdown();
    release_tx.send(()).expect("release accept");
    submitter
        .join()
        .expect("submitter should join")
        .expect("job should be accepted");
    pool.wait_termination();
    ran_rx.recv_timeout(Duration::from_secs(1)).expect("job should run");
    assert!(cancel_rx.try_recv().is_err());
}

#[test]
fn test_thread_pool_shutdown_returns_before_inflight_accept_then_drains() {
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
            Box::new(|| panic!("graceful shutdown should not cancel accepted job")),
        ))
    });
    accept_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("accept callback should start");
    let shutdown_pool = Arc::clone(&pool);
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let shutdown_thread = std::thread::spawn(move || {
        shutdown_pool.shutdown();
        shutdown_tx
            .send(())
            .expect("test should receive shutdown completion signal");
    });

    shutdown_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("shutdown should return while submit is inside accept");
    release_accept_tx
        .send(())
        .expect("accept callback should receive release signal");
    submit_thread
        .join()
        .expect("submit caller should not panic")
        .expect("in-flight submit should be accepted before shutdown drains");
    shutdown_thread.join().expect("shutdown caller should not panic");

    pool.wait_termination();
    assert!(
        ran.load(Ordering::Acquire),
        "graceful shutdown should drain the accepted queued job",
    );
    assert!(pool.is_terminated());
}

#[test]
fn test_thread_pool_wait_termination_waits_for_custom_cancel_callback() {
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
        terminated_tx.send(()).expect("test should receive termination signal");
    });

    let terminated_before_cancel_completed = terminated_rx.recv_timeout(Duration::from_millis(50)).is_ok();
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
fn test_ticket_cancellation_keeps_join_and_shutdown_busy_until_callback_returns() {
    let pool = Arc::new(ThreadPool::new(1).expect("pool builds"));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_worker_tx, release_worker_rx) = mpsc::channel();
    pool.submit_job(PoolJob::new(
        Box::new(move || {
            started_tx.send(()).expect("worker starts");
            release_worker_rx.recv().expect("worker released");
        }),
        Box::new(|| {}),
    ))
    .expect("blocker accepted");
    started_rx.recv_timeout(Duration::from_secs(2)).expect("worker starts");
    let (cancelling_tx, cancelling_rx) = mpsc::channel();
    let (release_cancel_tx, release_cancel_rx) = mpsc::channel();
    let (job, ticket) = pool.prepare_cancellable_job(
        Box::new(|| {}),
        Box::new(|| {}),
        Box::new(move || {
            cancelling_tx.send(()).expect("cancellation starts");
            release_cancel_rx.recv().expect("cancellation released");
        }),
    );
    pool.submit_job(job).expect("job accepted");
    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    std::thread::spawn(move || cancelled_tx.send(ticket.cancel_queued()).expect("cancellation result"));
    cancelling_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("cancellation starts");
    release_worker_tx.send(()).expect("release worker");
    pool.shutdown();
    let join_pool = Arc::clone(&pool);
    let (joined_tx, joined_rx) = mpsc::channel();
    std::thread::spawn(move || {
        join_pool.join();
        joined_tx.send(()).expect("join result");
    });
    assert!(!pool.wait_termination_timeout(Duration::from_millis(20)));
    assert!(
        joined_rx.try_recv().is_err(),
        "join must retain cancellation accounting"
    );
    release_cancel_tx.send(()).expect("release cancel callback");
    assert!(
        cancelled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("cancellation finishes")
    );
    joined_rx.recv_timeout(Duration::from_secs(2)).expect("join finishes");
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
    assert_eq!(pool.stats().cancelled_tasks, 1);
    assert_eq!(pool.stats().completed_tasks, 1);
}
