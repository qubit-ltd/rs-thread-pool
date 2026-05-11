/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`qubit_thread_pool::ExecutorBuildError`].

use qubit_thread_pool::{
    ExecutorBuildError,
    RejectedExecution,
};

#[test]
fn test_executor_build_error_from_rejected_execution_shutdown() {
    let error: ExecutorBuildError = RejectedExecution::Shutdown.into();

    let ExecutorBuildError::SpawnWorker { source, .. } = error else {
        panic!("expected spawn worker build error");
    };
    assert_eq!(
        source.to_string(),
        "executor service shut down during prestart"
    );
}

#[test]
fn test_executor_build_error_from_rejected_execution_saturated() {
    let error: ExecutorBuildError = RejectedExecution::Saturated.into();

    let ExecutorBuildError::SpawnWorker { source, .. } = error else {
        panic!("expected spawn worker build error");
    };
    assert_eq!(
        source.to_string(),
        "executor service saturated during prestart"
    );
}
