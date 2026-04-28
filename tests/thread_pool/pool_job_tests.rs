/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! Tests for [`qubit_thread_pool::service::PoolJob`].

use std::sync::{
    Arc,
    atomic::{
        AtomicBool,
        Ordering,
    },
};

use qubit_thread_pool::service::{
    ExecutorService,
    PoolJob,
    ThreadPool,
};

use super::create_runtime;

#[test]
fn test_thread_pool_submit_job_runs_type_erased_job() {
    let pool = ThreadPool::new(1).expect("thread pool should be created");
    let ran = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));

    pool.submit_job(PoolJob::new(
        {
            let ran = Arc::clone(&ran);
            Box::new(move || {
                ran.store(true, Ordering::Release);
            })
        },
        {
            let cancelled = Arc::clone(&cancelled);
            Box::new(move || {
                cancelled.store(true, Ordering::Release);
            })
        },
    ))
    .expect("type-erased pool job should be accepted");

    pool.shutdown();
    create_runtime().block_on(pool.await_termination());

    assert!(ran.load(Ordering::Acquire));
    assert!(!cancelled.load(Ordering::Acquire));
}
