// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::FixedThreadPool;

pub(crate) fn wait_started(receiver: Receiver<()>) {
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("task should start within timeout");
}

pub(crate) fn wait_until<F>(mut condition: F)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(condition(), "condition should become true within timeout");
}

#[test]
fn test_fixed_module_exports_thread_pool_entrypoint() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");

    assert_eq!(pool.pool_size(), 1);
    pool.shutdown();
    pool.wait_termination();
}
