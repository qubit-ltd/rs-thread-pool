// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::io;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::ThreadPool;

#[test]
fn test_worker_runs_job_on_named_worker_thread() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(1)
        .thread_name_prefix("runtime-check")
        .build()
        .expect("thread pool should be created");
    let name = pool
        .submit_callable(|| Ok::<_, io::Error>(std::thread::current().name().unwrap_or_default().to_owned()))
        .expect("thread pool should accept callable")
        .get()
        .expect("callable should complete successfully");

    assert!(
        name.starts_with("runtime-check-"),
        "worker thread name should use the configured prefix"
    );
    pool.shutdown();
}
