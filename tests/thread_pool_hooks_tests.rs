/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`ThreadPoolHooks`](qubit_thread_pool::ThreadPoolHooks).

use qubit_thread_pool::ThreadPoolHooks;

#[test]
fn test_thread_pool_hooks_builder_and_debug() {
    let hooks = ThreadPoolHooks::new()
        .before_worker_start(|_| {})
        .before_task(|_| {})
        .after_task(|_| {})
        .after_worker_stop(|_| {});

    let debug = format!("{hooks:?}");

    assert!(debug.contains("before_worker_start: true"));
    assert!(debug.contains("after_worker_stop: true"));
    assert!(debug.contains("before_task: true"));
    assert!(debug.contains("after_task: true"));
}
