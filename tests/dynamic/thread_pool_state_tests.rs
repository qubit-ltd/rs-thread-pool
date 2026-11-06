// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! State-transition tests for the dynamic thread pool.

use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::ThreadPool;

use super::mod_tests::create_single_worker_pool;
use super::mod_tests::wait_started;

struct ReleaseExitHookOnDrop(mpsc::Sender<()>);

impl Drop for ReleaseExitHookOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn test_thread_pool_state_reports_queue_saturation_and_shutdown_cancellation() {
    let pool = create_single_worker_pool();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = pool
        .submit_tracked(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .unwrap();
    wait_started(started_rx);
    let queued = pool.submit_tracked(|| Ok::<_, io::Error>(())).unwrap();
    let report = pool.stop();

    assert_eq!(1, report.queued);
    assert_eq!(1, report.cancelled);
    assert!(queued.is_done());
    release_tx.send(()).unwrap();
    running.get().unwrap();
    pool.wait_termination();

    let stats = pool.stats();
    assert_eq!(stats.submitted_tasks, 2);
    assert_eq!(stats.completed_tasks, 1);
    assert_eq!(stats.cancelled_tasks, 1);
    assert!(stats.terminated);
}

#[test]
fn test_termination_waits_for_stop_exit_hook() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let _release_guard = ReleaseExitHookOnDrop(release_tx.clone());
    let pool = ThreadPool::builder()
        .pool_size(1)
        .prestart_core_threads()
        .after_worker_stop(move |_| {
            entered_tx.send(()).expect("test should receive hook entry");
            release_rx
                .lock()
                .expect("release receiver should lock")
                .recv_timeout(Duration::from_secs(3))
                .expect("test should release worker exit hook");
        })
        .build()
        .expect("pool should build");

    pool.stop();
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker exit hook should start");
    assert!(!pool.is_terminated());
    assert!(!pool.wait_termination_timeout(Duration::from_millis(20)));

    release_tx.send(()).expect("worker exit hook should be released");
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
}

#[test]
fn test_termination_waits_for_core_timeout_exit_hook_before_shutdown() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let _release_guard = ReleaseExitHookOnDrop(release_tx.clone());
    let pool = ThreadPool::builder()
        .pool_size(1)
        .keep_alive(Duration::from_millis(20))
        .allow_core_thread_timeout(true)
        .prestart_core_threads()
        .after_worker_stop(move |_| {
            entered_tx.send(()).expect("test should receive hook entry");
            release_rx
                .lock()
                .expect("release receiver should lock")
                .recv_timeout(Duration::from_secs(3))
                .expect("test should release worker exit hook");
        })
        .build()
        .expect("pool should build");

    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("core worker should exit after its timeout");
    pool.shutdown();
    assert!(!pool.is_terminated());
    assert!(!pool.wait_termination_timeout(Duration::from_millis(20)));

    release_tx.send(()).expect("worker exit hook should be released");
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
}
