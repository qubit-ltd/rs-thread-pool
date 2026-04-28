/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! Tests for [`qubit_thread_pool::service::ThreadPoolBuildError`].

use qubit_thread_pool::service::{
    RejectedExecution,
    ThreadPoolBuildError,
};

#[test]
fn test_thread_pool_build_error_from_rejected_execution_shutdown() {
    let error: ThreadPoolBuildError = RejectedExecution::Shutdown.into();

    let ThreadPoolBuildError::SpawnWorker { source, .. } = error else {
        panic!("expected spawn worker build error");
    };
    assert_eq!(source.to_string(), "thread pool shut down during prestart");
}

#[test]
fn test_thread_pool_build_error_from_rejected_execution_saturated() {
    let error: ThreadPoolBuildError = RejectedExecution::Saturated.into();

    let ThreadPoolBuildError::SpawnWorker { source, .. } = error else {
        panic!("expected spawn worker build error");
    };
    assert_eq!(source.to_string(), "thread pool saturated during prestart");
}
