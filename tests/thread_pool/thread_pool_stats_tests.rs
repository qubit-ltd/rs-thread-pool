/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! Tests for [`qubit_thread_pool::service::ThreadPoolStats`].

use qubit_thread_pool::service::{
    ExecutorService,
    ThreadPool,
};

use super::create_runtime;

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
    assert_eq!(s.queued_tasks, 0);
    assert!(!s.shutdown);
    assert!(!s.terminated);

    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
    let s = pool.stats();
    assert!(s.shutdown);
    assert!(s.terminated);
}
