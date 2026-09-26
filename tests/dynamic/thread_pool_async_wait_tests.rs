// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::io;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::ThreadPool;
use tokio::time::timeout;

#[tokio::test]
async fn test_thread_pool_capacity_changes_when_worker_takes_queued_job() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .queue_capacity(1)
        .build()
        .expect("pool should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    pool.submit(move || {
        started_tx.send(()).expect("start signal should send");
        release_rx.recv().expect("release signal should arrive");
        Ok::<(), io::Error>(())
    })
    .expect("running task should be accepted");
    started_rx.recv().expect("task should start");
    pool.submit(|| Ok::<(), io::Error>(()))
        .expect("queue should accept one task");
    assert!(matches!(
        pool.submit(|| Ok::<(), io::Error>(())),
        Err(qubit_executor::service::SubmissionError::Saturated)
    ));
    let mut changes = pool.capacity_changes();
    release_tx.send(()).expect("running task should release");
    timeout(Duration::from_secs(1), changes.changed())
        .await
        .expect("taking queued work should publish capacity change")
        .expect("pool notification sender should remain open");
    pool.shutdown();
    timeout(Duration::from_secs(1), pool.await_termination())
        .await
        .expect("pool should terminate");
}

#[tokio::test]
async fn test_thread_pool_capacity_changes_on_shutdown() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .queue_capacity(1)
        .build()
        .expect("pool should build");
    let mut changes = pool.capacity_changes();
    pool.shutdown();
    timeout(Duration::from_secs(1), changes.changed())
        .await
        .expect("shutdown should publish capacity change")
        .expect("pool notification sender should remain open");
    pool.await_termination().await;
}

#[tokio::test]
async fn test_thread_pool_capacity_changes_after_queued_ticket_cancellation() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .queue_capacity(1)
        .build()
        .expect("pool should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    pool.submit(move || {
        started_tx.send(()).expect("start signal should send");
        release_rx.recv().expect("release signal should arrive");
        Ok::<(), io::Error>(())
    })
    .expect("running task should be accepted");
    started_rx.recv().expect("task should start");
    let (job, ticket) = pool.prepare_cancellable_job(Box::new(|| {}), Box::new(|| {}), Box::new(|| {}));
    pool.submit_job(job).expect("queued task should be accepted");
    let mut changes = pool.capacity_changes();
    assert!(ticket.cancel_queued());
    timeout(Duration::from_secs(1), changes.changed())
        .await
        .expect("queued cancellation should publish capacity change")
        .expect("pool notification sender should remain open");
    pool.submit(|| Ok::<(), io::Error>(()))
        .expect("cancelled queue slot should be reusable");
    let _report = pool.stop();
    release_tx.send(()).expect("running task should be released");
    pool.await_termination().await;
}

#[tokio::test]
async fn test_thread_pool_await_termination_waits_for_shutdown() {
    let pool = ThreadPool::new(1).expect("pool should build");
    let wait = pool.await_termination();
    tokio::pin!(wait);
    assert!(timeout(Duration::from_millis(20), &mut wait).await.is_err());
    pool.shutdown();
    timeout(Duration::from_secs(1), wait)
        .await
        .expect("empty pool should terminate");
    pool.await_termination().await;
}

#[tokio::test]
async fn test_thread_pool_await_termination_waits_for_running_task_and_multiple_waiters() {
    let pool = ThreadPool::new(1).expect("pool should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    pool.submit(move || {
        started_tx.send(()).expect("start signal should send");
        release_rx.recv().expect("release signal should arrive");
        Ok::<(), io::Error>(())
    })
    .expect("task should be accepted");
    started_rx.recv().expect("task should start");
    pool.shutdown();

    let first = pool.await_termination();
    tokio::pin!(first);
    assert!(timeout(Duration::from_millis(20), &mut first).await.is_err());
    let second = pool.await_termination();
    release_tx.send(()).expect("task should release");
    timeout(Duration::from_secs(1), async { tokio::join!(first, second) })
        .await
        .expect("both waiters should complete");
}

#[tokio::test]
async fn test_thread_pool_await_termination_can_be_cancelled() {
    let pool = ThreadPool::new(1).expect("pool should build");
    let mut wait = Box::pin(pool.await_termination());
    assert!(timeout(Duration::from_millis(20), &mut wait).await.is_err());
    drop(wait);
    pool.shutdown();
    timeout(Duration::from_secs(1), pool.await_termination())
        .await
        .expect("new waiter should complete");
}

#[tokio::test]
async fn test_thread_pool_await_termination_after_stop_cancels_queued_task() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .queue_capacity(1)
        .build()
        .expect("pool should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    pool.submit(move || {
        started_tx.send(()).expect("start signal should send");
        release_rx.recv().expect("release signal should arrive");
        Ok::<(), io::Error>(())
    })
    .expect("running task should be accepted");
    started_rx.recv().expect("task should start");
    pool.submit(|| Ok::<(), io::Error>(()))
        .expect("queued task should be accepted");
    let report = pool.stop();
    assert_eq!(report.cancelled, 1);
    let mut wait = Box::pin(pool.await_termination());
    assert!(timeout(Duration::from_millis(20), &mut wait).await.is_err());
    release_tx.send(()).expect("task should release");
    timeout(Duration::from_secs(1), wait)
        .await
        .expect("stopped pool should terminate");
}
