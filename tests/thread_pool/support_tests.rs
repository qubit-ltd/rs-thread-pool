/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Shared support for thread-pool integration tests.

use std::time::Duration;

use qubit_thread_pool::service::ThreadPool;

/// Creates a current-thread Tokio runtime for driving async termination APIs in sync tests.
pub(crate) fn create_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime for thread pool tests")
}

/// Creates a single-worker pool for deterministic queue tests.
pub(crate) fn create_single_worker_pool() -> ThreadPool {
    ThreadPool::new(1).expect("thread pool should be created")
}

/// Waits until a blocking task reports that it has started.
pub(crate) fn wait_started(receiver: std::sync::mpsc::Receiver<()>) {
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("task should start within timeout");
}

/// Waits until a condition becomes true or fails the test.
pub(crate) fn wait_until<F>(mut condition: F)
where
    F: FnMut() -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(condition(), "condition should become true within timeout");
}
