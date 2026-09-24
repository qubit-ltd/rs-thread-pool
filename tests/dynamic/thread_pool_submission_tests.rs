// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Submission tests for [`qubit_thread_pool::ThreadPool`].

use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::ExecutorServiceBuilderError;
use qubit_thread_pool::PoolJob;
use qubit_thread_pool::ThreadPool;

use super::mod_tests::create_single_worker_pool;
use super::mod_tests::wait_started;
use super::mod_tests::wait_until;

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

    assert_eq!(handle.get().expect("callable should complete successfully"), 42,);
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_submit_custom_job_runs_job() {
    let pool = ThreadPool::new(1).expect("thread pool should be created");
    let (done_tx, done_rx) = mpsc::channel();

    pool.submit_job(PoolJob::new(
        Box::new(move || {
            done_tx.send("run").expect("test should receive custom job completion");
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
            started_tx.send(()).expect("test should receive task start signal");
            release_rx.recv().map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    wait_started(started_rx);
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();

    pool.submit_job(PoolJob::with_accept(
        Box::new(move || {
            accepted_tx.send(()).expect("test should receive custom job acceptance");
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

    assert!(pool.prestart_core_thread().expect("core worker should prestart"));
    super::mod_tests::wait_until(|| pool.stats().idle_workers == 1);
    let handle = pool
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("thread pool should accept callable");

    assert_eq!(handle.get().expect("callable should complete"), 42);
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_bounded_submit_queues_when_worker_busy() {
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
            started_tx.send(()).expect("test should receive task start signal");
            release_rx.recv().map_err(|err| io::Error::other(err.to_string()))?;
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
fn test_thread_pool_core_size_update_grows_next_submission() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(2)
        .queue_capacity(1)
        .build()
        .expect("pool should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = pool
        .submit_callable(move || {
            started_tx.send(()).expect("start signal should send");
            release_rx.recv().map_err(|error| io::Error::other(error.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("first job should be accepted");
    wait_started(started_rx);
    pool.set_core_pool_size(2).expect("core increase should succeed");
    let second = pool
        .submit_callable(|| Ok::<(), io::Error>(()))
        .expect("second job should be accepted");
    wait_until(|| pool.live_worker_count() == 2);
    release_tx.send(()).expect("first job should be released");
    first.get().expect("first job should complete");
    second.get().expect("second job should complete");
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_blocked_accept_does_not_hold_state_lock() {
    let pool = Arc::new(
        ThreadPool::builder()
            .pool_size(1)
            .prestart_core_threads()
            .build()
            .expect("thread pool should be created"),
    );
    super::mod_tests::wait_until(|| pool.stats().idle_workers == 1);
    let (accept_started_tx, accept_started_rx) = mpsc::channel();
    let (release_accept_tx, release_accept_rx) = mpsc::channel();
    let submit_pool = Arc::clone(&pool);
    let submit_thread = std::thread::spawn(move || {
        submit_pool.submit_job(PoolJob::with_accept(
            Box::new(move || {
                accept_started_tx
                    .send(())
                    .expect("test should receive accept start signal");
                release_accept_rx.recv().expect("test should release accept callback");
            }),
            Box::new(|| {}),
            Box::new(|| panic!("accepted job should run after accept is released")),
        ))
    });
    accept_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("accept callback should start");
    let (stats_tx, stats_rx) = mpsc::channel();
    let stats_pool = Arc::clone(&pool);
    std::thread::spawn(move || {
        let stats = stats_pool.stats();
        stats_tx
            .send(stats.live_workers)
            .expect("test should receive stats snapshot");
    });

    assert_eq!(
        stats_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stats should not block on custom accept"),
        1,
    );
    release_accept_tx
        .send(())
        .expect("accept callback should receive release signal");
    submit_thread
        .join()
        .expect("submit caller should not panic")
        .expect("job should be accepted");
    pool.join();
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_direct_accept_can_reenter_pool_without_deadlock() {
    let pool = Arc::new(ThreadPool::new(1).expect("thread pool should be created"));
    let callback_pool = Arc::clone(&pool);
    let (result_tx, result_rx) = mpsc::channel();
    let submit_pool = Arc::clone(&pool);
    std::thread::spawn(move || {
        let result = submit_pool.submit_job(PoolJob::with_accept(
            Box::new(move || {
                let _ = callback_pool.stats();
            }),
            Box::new(|| {}),
            Box::new(|| {}),
        ));
        result_tx.send(result).expect("test should receive submit result");
    });

    result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("direct acceptance callback must not deadlock on the pool monitor")
        .expect("job should be accepted");
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_lazy_pool_keeps_worker_alive_during_acceptance() {
    let pool = Arc::new(
        ThreadPool::builder()
            .pool_size(1)
            .keep_alive(Duration::from_millis(10))
            .allow_core_thread_timeout(true)
            .build()
            .expect("pool should be created"),
    );
    let callback_pool = Arc::clone(&pool);
    let (ran_tx, ran_rx) = mpsc::channel();
    pool.submit_job(PoolJob::with_accept(
        Box::new(move || {
            wait_until(|| callback_pool.stats().idle_workers == 1);
            std::thread::sleep(Duration::from_millis(50));
        }),
        Box::new(move || ran_tx.send(()).expect("run signal")),
        Box::new(|| {}),
    ))
    .expect("job should be accepted");
    let run_result = ran_rx.recv_timeout(Duration::from_secs(1));
    pool.stop();
    pool.wait_termination();
    run_result.expect("accepted job must retain a worker while acceptance is in flight");
}

#[test]
fn test_lazy_pool_acceptance_runs_on_submitter_thread() {
    let pool = ThreadPool::new(1).expect("pool should be created");
    let acceptance_thread = Arc::new(Mutex::new(None));
    let callback_thread = Arc::clone(&acceptance_thread);
    let submitter_thread = std::thread::current().id();
    pool.submit_job(PoolJob::with_accept(
        Box::new(move || {
            *callback_thread.lock().expect("acceptance thread lock") = Some(std::thread::current().id());
        }),
        Box::new(|| {}),
        Box::new(|| panic!("accepted job should not be cancelled")),
    ))
    .expect("job should be accepted");
    let observed_thread = *acceptance_thread.lock().expect("acceptance thread lock");
    pool.shutdown();
    pool.wait_termination();
    assert_eq!(observed_thread, Some(submitter_thread));
}

/// Signals when a queued callback's captured value is released.
struct TicketDropProbe(mpsc::Sender<()>);

impl Drop for TicketDropProbe {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// Occupies the only worker until the returned sender releases it.
fn block_ticket_worker(pool: &ThreadPool) -> mpsc::Sender<()> {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    pool.submit_job(PoolJob::new(
        Box::new(move || {
            started_tx.send(()).expect("worker starts");
            release_rx.recv().expect("worker released");
        }),
        Box::new(|| {}),
    ))
    .expect("blocker accepted");
    started_rx.recv_timeout(Duration::from_secs(2)).expect("worker starts");
    release_tx
}

#[test]
fn test_ticket_cancel_releases_capture_capacity_and_preserves_fifo() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .queue_capacity(1)
        .build()
        .expect("pool builds");
    let release = block_ticket_worker(&pool);
    let (drop_tx, drop_rx) = mpsc::channel();
    let probe = TicketDropProbe(drop_tx);
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let (job, ticket) = pool.prepare_cancellable_job(
        Box::new(|| {}),
        Box::new(move || drop(probe)),
        Box::new(move || cancel_tx.send(()).expect("cancellation observed")),
    );
    assert!(!ticket.cancel_queued(), "unaccepted jobs cannot be cancelled");
    pool.submit_job(job).expect("ticket job accepted");
    assert!(matches!(
        pool.submit_job(PoolJob::new(Box::new(|| {}), Box::new(|| {}))),
        Err(qubit_thread_pool::PoolJobSubmissionError::Rejected(
            qubit_executor::service::SubmissionError::Saturated
        ))
    ));
    assert!(ticket.cancel_queued());
    drop_rx.try_recv().expect("capture dropped before cancellation returns");
    cancel_rx.try_recv().expect("callback ran before cancellation returns");
    assert!(!ticket.cancel_queued());
    assert_eq!(pool.stats().queued_tasks, 0);
    assert_eq!(pool.stats().cancelled_tasks, 1);
    pool.submit_job(PoolJob::new(Box::new(|| {}), Box::new(|| {})))
        .expect("capacity immediately reusable");
    release.send(()).expect("release blocker");
    pool.shutdown();
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
    assert_eq!(pool.stats().completed_tasks, 2);
}

#[test]
fn test_ticket_removal_preserves_remaining_fifo_order() {
    let pool = ThreadPool::new(1).expect("pool builds");
    let release = block_ticket_worker(&pool);
    let (order_tx, order_rx) = mpsc::channel();
    let mut tickets = Vec::new();
    for index in 0..5 {
        let order_tx = order_tx.clone();
        let (job, ticket) = pool.prepare_cancellable_job(
            Box::new(|| {}),
            Box::new(move || {
                order_tx.send(index).expect("record run order");
            }),
            Box::new(|| {}),
        );
        pool.submit_job(job).expect("job accepted");
        tickets.push(ticket);
    }
    assert!(tickets[2].cancel_queued());
    release.send(()).expect("release blocker");
    pool.shutdown();
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
    assert_eq!(order_rx.try_iter().collect::<Vec<_>>(), vec![0, 1, 3, 4]);
}

#[test]
fn test_ticket_cancel_races_worker_claim_and_stop_once() {
    for stop in [false, true] {
        for _ in 0..32 {
            let pool = Arc::new(ThreadPool::new(1).expect("pool builds"));
            let release = block_ticket_worker(&pool);
            let (outcome_tx, outcome_rx) = mpsc::channel();
            let run_tx = outcome_tx.clone();
            let (job, ticket) = pool.prepare_cancellable_job(
                Box::new(|| {}),
                Box::new(move || run_tx.send("run").expect("run observed")),
                Box::new(move || outcome_tx.send("cancel").expect("cancel observed")),
            );
            pool.submit_job(job).expect("job accepted");
            let barrier = Arc::new(std::sync::Barrier::new(3));
            let cancel_barrier = Arc::clone(&barrier);
            let (cancel_result_tx, cancel_result_rx) = mpsc::channel();
            std::thread::spawn(move || {
                cancel_barrier.wait();
                cancel_result_tx.send(ticket.cancel_queued()).expect("cancel result");
            });
            let action_barrier = Arc::clone(&barrier);
            let action_pool = Arc::clone(&pool);
            let action_release = release.clone();
            let (action_tx, action_rx) = mpsc::channel();
            std::thread::spawn(move || {
                action_barrier.wait();
                if stop {
                    action_pool.stop();
                } else {
                    action_release.send(()).expect("release worker");
                }
                action_tx.send(()).expect("competing action result");
            });
            barrier.wait();
            action_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("competing action finishes");
            let cancelled = cancel_result_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("canceller finishes");
            if stop {
                release.send(()).expect("release worker");
            }
            pool.shutdown();
            assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
            let outcomes = outcome_rx.try_iter().collect::<Vec<_>>();
            assert_eq!(outcomes.len(), 1);
            if cancelled {
                assert_eq!(outcomes, vec!["cancel"]);
            }
            let stats = pool.stats();
            assert_eq!(stats.queued_tasks, 0);
            assert_eq!(stats.running_tasks, 0);
            assert_eq!(stats.completed_tasks + stats.cancelled_tasks, stats.submitted_tasks);
        }
    }
}

#[test]
fn test_ticket_cancel_during_accept_waits_and_releases_captures() {
    let pool = Arc::new(ThreadPool::new(1).expect("pool builds"));
    let release_worker = block_ticket_worker(&pool);
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (drop_tx, drop_rx) = mpsc::channel();
    let probe = TicketDropProbe(drop_tx);
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let (job, ticket) = pool.prepare_cancellable_job(
        Box::new(move || {
            accepted_tx.send(()).expect("acceptance starts");
            release_rx.recv().expect("acceptance released");
        }),
        Box::new(move || drop(probe)),
        Box::new(move || cancel_tx.send(()).expect("cancel observed")),
    );
    let submit_pool = Arc::clone(&pool);
    let (submitted_tx, submitted_rx) = mpsc::channel();
    std::thread::spawn(move || submitted_tx.send(submit_pool.submit_job(job)).expect("submit result"));
    accepted_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("acceptance starts");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let cancel_barrier = Arc::clone(&barrier);
    let (cancel_result_tx, cancel_result_rx) = mpsc::channel();
    std::thread::spawn(move || {
        cancel_barrier.wait();
        cancel_result_tx.send(ticket.cancel_queued()).expect("cancel result");
    });
    barrier.wait();
    release_tx.send(()).expect("release acceptance");
    assert!(
        cancel_result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("canceller finishes")
    );
    submitted_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("submitter finishes")
        .expect("job accepted");
    drop_rx.try_recv().expect("capture released");
    cancel_rx.try_recv().expect("cancel callback completed");
    assert_eq!(pool.stats().queued_tasks, 0);
    release_worker.send(()).expect("release worker");
    pool.shutdown();
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
}

#[test]
fn test_ticket_acceptance_panic_and_rejection_never_cancel() {
    let pool = ThreadPool::new(1).expect("pool builds");
    let (job, ticket) = pool.prepare_cancellable_job(
        Box::new(|| panic!("acceptance fails")),
        Box::new(|| panic!("must not run")),
        Box::new(|| panic!("must not cancel")),
    );
    assert_eq!(
        pool.submit_job(job),
        Err(qubit_thread_pool::PoolJobSubmissionError::AcceptancePanicked)
    );
    assert!(!ticket.cancel_queued());
    assert_eq!(pool.stats().submitted_tasks, 0);
    let (job, ticket) =
        pool.prepare_cancellable_job(Box::new(|| panic!("must not accept")), Box::new(|| {}), Box::new(|| {}));
    pool.shutdown();
    assert!(pool.submit_job(job).is_err());
    assert!(!ticket.cancel_queued());
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
}

#[test]
fn test_ticket_cancel_callback_can_submit_to_same_queue() {
    let pool = Arc::new(ThreadPool::new(1).expect("pool builds"));
    let release = block_ticket_worker(&pool);
    let cancel_pool = Arc::clone(&pool);
    let (job, ticket) = pool.prepare_cancellable_job(
        Box::new(|| {}),
        Box::new(|| {}),
        Box::new(move || {
            cancel_pool
                .submit_job(PoolJob::new(Box::new(|| {}), Box::new(|| {})))
                .expect("reentrant submit");
        }),
    );
    pool.submit_job(job).expect("accepted");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || done_tx.send(ticket.cancel_queued()).expect("cancel result"));
    assert!(
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("queue lock released before callback")
    );
    release.send(()).expect("release worker");
    pool.shutdown();
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
}

/// A run capture whose destructor exercises the cancellation unwind boundary.
struct TicketPanickingDrop;

impl Drop for TicketPanickingDrop {
    fn drop(&mut self) {
        panic!("cancelled run capture destructor panicked");
    }
}

#[test]
fn test_ticket_cancel_contains_run_capture_drop_panic_and_finishes_accounting() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .queue_capacity(1)
        .build()
        .expect("pool builds");
    let release = block_ticket_worker(&pool);
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let capture = TicketPanickingDrop;
    let (job, ticket) = pool.prepare_cancellable_job(
        Box::new(|| {}),
        Box::new(move || drop(capture)),
        Box::new(move || cancel_tx.send(()).expect("cancellation observed")),
    );
    pool.submit_job(job).expect("ticket job accepted");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ticket.cancel_queued()));
    release.send(()).expect("release blocker");
    pool.shutdown();
    assert!(result.is_ok(), "capture Drop panic must remain inside the job boundary");
    assert!(result.expect("cancellation does not unwind"));
    assert!(!ticket.cancel_queued(), "cancellation remains one-shot");
    cancel_rx.try_recv().expect("cancellation callback ran once");
    assert!(cancel_rx.try_recv().is_err());
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
    let stats = pool.stats();
    assert_eq!(stats.submitted_tasks, 2);
    assert_eq!(stats.completed_tasks, 1);
    assert_eq!(stats.cancelled_tasks, 1);
    assert_eq!(stats.queued_tasks, 0);
    assert_eq!(stats.running_tasks, 0);
}
