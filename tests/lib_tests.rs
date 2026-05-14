/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
mod dynamic;
mod fixed;

#[test]
fn test_lib_exports_primary_executor_types() {
    let dynamic = qubit_thread_pool::ThreadPool::new(1);
    let fixed = qubit_thread_pool::FixedThreadPool::new(1);

    assert!(dynamic.is_ok());
    assert!(fixed.is_ok());
}
