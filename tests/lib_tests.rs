/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
mod delayed;
mod dynamic;
mod fixed;

#[test]
fn test_lib_exports_primary_executor_types() {
    let dynamic = qubit_thread_pool::ThreadPool::new(1);
    let fixed = qubit_thread_pool::FixedThreadPool::new(1);
    let delayed = qubit_thread_pool::DelayedTaskScheduler::new("test-lib-exports");

    assert!(dynamic.is_ok());
    assert!(fixed.is_ok());
    assert!(delayed.is_ok());
}
