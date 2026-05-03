/*******************************************************************************
 *
 *    Copyright (c) 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Coverage-only tests for defensive queue stealing branches.

#[test]
fn test_exercise_coverage_support_branches() {
    let branches = qubit_thread_pool::coverage_support::exercise_all();

    assert!(branches.contains(&"queue-steal-retry"));
    assert!(branches.contains(&"worker-queue-drain"));
}
