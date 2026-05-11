/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`qubit_thread_pool::ExecutorServiceBuilderError`].

use qubit_thread_pool::{
    ExecutorServiceBuilderError,
    SubmissionError,
};

#[test]
fn test_executor_build_error_from_submission_error_shutdown() {
    let error: ExecutorServiceBuilderError = SubmissionError::Shutdown.into();

    let ExecutorServiceBuilderError::SpawnWorker { source, .. } = error else {
        panic!("expected spawn worker build error");
    };
    assert_eq!(
        source.to_string(),
        "executor service shut down during prestart"
    );
}

#[test]
fn test_executor_build_error_from_submission_error_saturated() {
    let error: ExecutorServiceBuilderError = SubmissionError::Saturated.into();

    let ExecutorServiceBuilderError::SpawnWorker { source, .. } = error else {
        panic!("expected spawn worker build error");
    };
    assert_eq!(
        source.to_string(),
        "executor service saturated during prestart"
    );
}
