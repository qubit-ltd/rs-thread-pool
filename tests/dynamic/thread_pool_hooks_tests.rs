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
use std::sync::mpsc;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::ThreadPool;

use super::mod_tests::wait_started;

fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
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
