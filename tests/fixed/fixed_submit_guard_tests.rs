use std::{
    io,
    sync::atomic::Ordering,
};

use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
    SubmissionError,
    fixed::{
        fixed_submit_guard::FixedSubmitGuard,
        fixed_thread_pool_inner::FixedThreadPoolInner,
    },
};

fn create_inner() -> FixedThreadPoolInner {
    FixedThreadPoolInner::new(1, None)
}

#[test]
fn test_fixed_submit_guard_rejects_after_public_shutdown() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");

    pool.shutdown();
    let rejected = pool.submit(|| Ok::<_, io::Error>(()));

    assert!(matches!(rejected, Err(SubmissionError::Shutdown)));
    pool.wait_termination();
}

#[test]
fn test_fixed_submit_guard_notifies_when_last_submitter_leaves_closed_admission() {
    let inner = create_inner();
    inner.inflight_submissions.store(1, Ordering::Release);
    inner.accepting.store(false, Ordering::Release);
    inner.submit_waiter_count.store(1, Ordering::Release);

    drop(FixedSubmitGuard { inner: &inner });

    assert_eq!(inner.inflight_count(), 0);
}

#[test]
fn test_fixed_submit_guard_notifies_idle_waiters_when_pool_becomes_idle() {
    let inner = create_inner();
    inner.inflight_submissions.store(1, Ordering::Release);
    inner.idle_waiter_count.store(1, Ordering::Release);
    inner.submit_waiter_count.store(1, Ordering::Release);

    drop(FixedSubmitGuard { inner: &inner });

    assert_eq!(inner.inflight_count(), 0);
    assert!(inner.is_idle());
}

#[test]
fn test_fixed_submit_guard_only_leaves_one_inflight_submitter() {
    let inner = create_inner();
    inner.inflight_submissions.store(2, Ordering::Release);
    inner.accepting.store(false, Ordering::Release);

    drop(FixedSubmitGuard { inner: &inner });

    assert_eq!(inner.inflight_count(), 1);
}
