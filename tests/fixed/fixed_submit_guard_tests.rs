use std::{
    io,
    sync::{
        Arc,
        atomic::Ordering,
    },
};

use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
    RejectedExecution,
    fixed::{
        fixed_submit_guard::FixedSubmitGuard,
        fixed_thread_pool_inner::FixedThreadPoolInner,
        fixed_worker_runtime::FixedWorkerRuntime,
    },
};

use super::mod_tests::create_runtime;

fn create_inner() -> FixedThreadPoolInner {
    let runtime = FixedWorkerRuntime::new(0);
    FixedThreadPoolInner::new(1, None, vec![Arc::clone(&runtime.queue)])
}

#[test]
fn test_fixed_submit_guard_rejects_after_shutdown_closes_admission() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");

    pool.shutdown();
    let rejected = pool.submit(|| Ok::<_, io::Error>(()));
    create_runtime().block_on(pool.await_termination());

    assert!(matches!(rejected, Err(RejectedExecution::Shutdown)));
}

#[test]
fn test_fixed_submit_guard_notifies_when_last_submitter_leaves_closed_admission() {
    let inner = create_inner();
    inner.inflight_submissions.store(1, Ordering::Release);
    inner.accepting.store(false, Ordering::Release);

    drop(FixedSubmitGuard { inner: &inner });

    assert_eq!(inner.inflight_count(), 0);
}
