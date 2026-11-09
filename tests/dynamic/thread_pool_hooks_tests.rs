// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Hook tests for [`qubit_thread_pool::ThreadPool`].

use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::ThreadPool;

struct PanickingPayload;

impl Drop for PanickingPayload {
    fn drop(&mut self) {
        panic!("payload destructor panic");
    }
}

struct NestedPanickingPayload(Arc<AtomicUsize>);

impl Drop for NestedPanickingPayload {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("nested payload destructor panic");
    }
}

struct OuterPanickingPayload {
    drops: Arc<AtomicUsize>,
    nested_drops: Arc<AtomicUsize>,
}

struct CountedPayload(Arc<AtomicUsize>);

impl Drop for CountedPayload {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for OuterPanickingPayload {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
        std::panic::panic_any(NestedPanickingPayload(Arc::clone(&self.nested_drops)));
    }
}

use super::mod_tests::wait_started;

fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
}

struct ReleaseExitHookOnDrop(mpsc::Sender<()>);

impl Drop for ReleaseExitHookOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
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
fn test_thread_pool_hook_payload_destructor_panic_is_contained() {
    for hook_kind in 0..4 {
        let builder = ThreadPool::builder().pool_size(1);
        let builder = match hook_kind {
            0 => builder.before_worker_start(|_| std::panic::panic_any(PanickingPayload)),
            1 => builder.before_task(|_| std::panic::panic_any(PanickingPayload)),
            2 => builder.after_task(|_| std::panic::panic_any(PanickingPayload)),
            _ => builder.after_worker_stop(|_| std::panic::panic_any(PanickingPayload)),
        };
        let pool = builder.build().expect("pool builds");
        let (done_tx, done_rx) = mpsc::channel();
        pool.submit(move || {
            done_tx.send(()).expect("task progress observed");
            Ok::<(), io::Error>(())
        })
        .expect("task accepted");
        assert!(
            done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "task progress failed for hook {hook_kind}"
        );
        pool.shutdown();
        assert!(
            pool.wait_termination_timeout(Duration::from_secs(2)),
            "termination failed for hook {hook_kind}"
        );
    }
}

#[test]
fn test_thread_pool_nested_hook_payload_destructor_panic_is_contained() {
    let outer_drops = Arc::new(AtomicUsize::new(0));
    let nested_drops = Arc::new(AtomicUsize::new(0));
    let hook_outer_drops = Arc::clone(&outer_drops);
    let hook_nested_drops = Arc::clone(&nested_drops);
    let pool = ThreadPool::builder()
        .pool_size(1)
        .before_task(move |_| {
            std::panic::panic_any(OuterPanickingPayload {
                drops: Arc::clone(&hook_outer_drops),
                nested_drops: Arc::clone(&hook_nested_drops),
            })
        })
        .build()
        .expect("pool builds");
    let (done_tx, done_rx) = mpsc::channel();
    pool.submit(move || {
        done_tx.send(()).expect("task progress observed");
        Ok::<(), io::Error>(())
    })
    .expect("task accepted");
    done_rx.recv_timeout(Duration::from_secs(2)).expect("task progresses");
    pool.shutdown();
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
    assert_eq!(outer_drops.load(Ordering::Relaxed), 1);
    assert_eq!(nested_drops.load(Ordering::Relaxed), 0);
}

#[test]
fn test_thread_pool_ordinary_hook_panic_payload_is_dropped() {
    let payload_drops = Arc::new(AtomicUsize::new(0));
    let hook_payload_drops = Arc::clone(&payload_drops);
    let pool = ThreadPool::builder()
        .pool_size(1)
        .before_task(move |_| std::panic::panic_any(CountedPayload(Arc::clone(&hook_payload_drops))))
        .build()
        .expect("pool builds");
    let (done_tx, done_rx) = mpsc::channel();
    pool.submit(move || {
        done_tx.send(()).expect("task progress observed");
        Ok::<(), io::Error>(())
    })
    .expect("task accepted");
    done_rx.recv_timeout(Duration::from_secs(2)).expect("task progresses");
    pool.shutdown();
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
    assert_eq!(payload_drops.load(Ordering::Relaxed), 1);
}

#[test]
fn test_thread_pool_runs_task_hooks_for_queued_jobs() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let pool = ThreadPool::builder()
        .pool_size(1)
        .before_task({
            let events = Arc::clone(&events);
            move |_| events.lock().expect("events should lock").push("before")
        })
        .after_task({
            let events = Arc::clone(&events);
            move |_| events.lock().expect("events should lock").push("after")
        })
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
        .submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("queued task should be accepted");

    release_tx.send(()).expect("running task should receive release signal");
    running.get().expect("running task should complete");
    queued.get().expect("queued task should complete");
    pool.shutdown();
    pool.wait_termination();

    let events = events.lock().expect("events should lock");
    assert_eq!(events.iter().filter(|event| **event == "before").count(), 2);
    assert_eq!(events.iter().filter(|event| **event == "after").count(), 2);
}

#[test]
fn test_termination_waits_for_shutdown_exit_hook() {
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
        .expect("thread pool should be created");

    pool.shutdown();
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker exit hook should start");

    assert!(!pool.is_terminated());
    assert!(!pool.wait_termination_timeout(Duration::from_millis(20)));

    release_tx.send(()).expect("worker exit hook should be released");
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
}

#[test]
fn test_termination_waits_for_all_concurrent_exit_hooks() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let _release_guards = [
        ReleaseExitHookOnDrop(release_tx.clone()),
        ReleaseExitHookOnDrop(release_tx.clone()),
    ];
    let pool = ThreadPool::builder()
        .pool_size(2)
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
        .expect("thread pool should be created");

    pool.shutdown();
    for _ in 0..2 {
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("both worker exit hooks should start");
    }

    assert!(!pool.is_terminated());
    assert!(!pool.wait_termination_timeout(Duration::from_millis(20)));

    release_tx.send(()).expect("first exit hook should be released");
    assert!(!pool.wait_termination_timeout(Duration::from_millis(20)));
    release_tx.send(()).expect("second exit hook should be released");
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
}

#[test]
fn test_termination_waits_for_core_size_shrink_exit_hooks_and_preserves_capacity() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let _release_guards = [
        ReleaseExitHookOnDrop(release_tx.clone()),
        ReleaseExitHookOnDrop(release_tx.clone()),
    ];
    let pool = ThreadPool::builder()
        .core_pool_size(2)
        .maximum_pool_size(2)
        .keep_alive(Duration::from_millis(20))
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
        .expect("thread pool should be created");

    assert_eq!(pool.live_worker_count(), 2);
    pool.set_core_pool_size(1).expect("core pool size should shrink");
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("excess core worker should enter its exit hook");
    assert_eq!(pool.core_pool_size(), 1);
    assert_eq!(pool.stats().live_workers, 1);
    assert_eq!(pool.live_worker_count(), 1);

    pool.shutdown();
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("remaining worker should enter its exit hook");
    assert_eq!(pool.live_worker_count(), 0);
    assert!(!pool.is_terminated());
    assert!(!pool.wait_termination_timeout(Duration::from_millis(20)));

    release_tx.send(()).expect("first exit hook should be released");
    assert!(!pool.wait_termination_timeout(Duration::from_millis(20)));
    release_tx.send(()).expect("second exit hook should be released");
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
}
