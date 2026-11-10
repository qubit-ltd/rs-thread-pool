// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::ThreadPool;

use super::mod_tests::wait_until;

#[test]
fn test_thread_pool_config_is_reflected_by_builder_and_stats() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(3)
        .queue_capacity(8)
        .thread_name_prefix("config-check")
        .keep_alive(Duration::from_millis(25))
        .allow_core_thread_timeout(true)
        .build()
        .expect("thread pool should be created with the configured values");
    let stats = pool.stats();

    assert_eq!(1, stats.core_pool_size);
    assert_eq!(3, stats.maximum_pool_size);
    assert_eq!(0, pool.queued_count());
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_runtime_keep_alive_duration_max_does_not_kill_worker() {
    let pool = ThreadPool::builder()
        .pool_size(1)
        .allow_core_thread_timeout(true)
        .prestart_core_threads()
        .build()
        .expect("thread pool should be created");
    wait_until(|| pool.stats().idle_workers == 1);
    pool.set_keep_alive(Duration::MAX)
        .expect("large keep-alive should be accepted");
    let (ran_tx, ran_rx) = std::sync::mpsc::channel();
    pool.submit_callable(move || {
        ran_tx.send(()).expect("test should observe second task");
        Ok::<(), std::io::Error>(())
    })
    .expect("worker should accept a task after runtime update");
    ran_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker should remain alive after runtime update");
    pool.shutdown();
    assert!(pool.wait_termination_timeout(Duration::from_secs(5)));
}
