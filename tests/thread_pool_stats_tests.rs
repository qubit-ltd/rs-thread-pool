// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Tests for [`qubit_thread_pool::ThreadPoolStats`].

use std::io;

use qubit_executor::service::ExecutorService;
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_thread_pool::FixedThreadPool;
use qubit_thread_pool::ThreadPool;
use qubit_thread_pool::ThreadPoolStats;

#[test]
fn test_thread_pool_stats_default_uses_empty_running_snapshot() {
    let stats = ThreadPoolStats::default();

    assert_eq!(stats.lifecycle, ExecutorServiceLifecycle::Running);
    assert_eq!(stats.core_pool_size, 0);
    assert_eq!(stats.maximum_pool_size, 0);
    assert_eq!(stats.live_workers, 0);
    assert_eq!(stats.idle_workers, 0);
    assert_eq!(stats.queued_tasks, 0);
    assert_eq!(stats.queue_capacity, None);
    assert_eq!(stats.running_tasks, 0);
    assert_eq!(stats.submitted_tasks, 0);
    assert_eq!(stats.completed_tasks, 0);
    assert_eq!(stats.cancelled_tasks, 0);
    assert!(!stats.terminated);
}

#[test]
fn test_thread_pool_stats_reflect_configuration() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(3)
        .queue_capacity(2)
        .build()
        .expect("thread pool should be created");

    let s = pool.stats();
    assert_eq!(s.core_pool_size, 1);
    assert_eq!(s.maximum_pool_size, 3);
    assert_eq!(s.queue_capacity, Some(2));
    assert_eq!(s.queued_tasks, 0);
    assert_eq!(s.lifecycle, ExecutorServiceLifecycle::Running);
    assert!(!s.terminated);

    pool.shutdown();
    pool.wait_termination();
    let s = pool.stats();
    assert_eq!(s.lifecycle, ExecutorServiceLifecycle::Terminated);
    assert!(s.terminated);
}

#[test]
fn test_dynamic_stats_report_completed_tasks_after_termination() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(1)
        .build()
        .expect("dynamic pool should build");
    let task = pool
        .submit_tracked(|| Ok::<(), io::Error>(()))
        .expect("task should be accepted");
    task.get().expect("task should complete");
    pool.shutdown();
    pool.wait_termination();

    let stats = pool.stats();
    assert_eq!(stats.submitted_tasks, 1);
    assert_eq!(stats.completed_tasks, 1);
    assert_eq!(stats.cancelled_tasks, 0);
    assert_eq!(stats.queued_tasks, 0);
    assert_eq!(stats.running_tasks, 0);
    assert!(stats.terminated);
}

#[test]
fn test_fixed_stats_report_completed_tasks_after_termination() {
    let pool = FixedThreadPool::builder()
        .pool_size(1)
        .queue_capacity(3)
        .build()
        .expect("fixed pool should build");
    let task = pool
        .submit_tracked(|| Ok::<(), io::Error>(()))
        .expect("task should be accepted");
    task.get().expect("task should complete");
    pool.shutdown();
    pool.wait_termination();

    let stats = pool.stats();
    assert_eq!(stats.queue_capacity, Some(3));
    assert_eq!(stats.submitted_tasks, 1);
    assert_eq!(stats.completed_tasks, 1);
    assert_eq!(stats.cancelled_tasks, 0);
    assert_eq!(stats.queued_tasks, 0);
    assert_eq!(stats.running_tasks, 0);
    assert!(stats.terminated);
}

#[test]
fn test_dynamic_and_fixed_stats_report_unbounded_queue_capacity() {
    let dynamic = ThreadPool::new(1).expect("dynamic pool should build");
    let fixed = FixedThreadPool::new(1).expect("fixed pool should build");

    assert_eq!(dynamic.stats().queue_capacity, None);
    assert_eq!(fixed.stats().queue_capacity, None);

    dynamic.shutdown();
    fixed.shutdown();
    dynamic.wait_termination();
    fixed.wait_termination();
}
