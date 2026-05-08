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
    fixed::{
        fixed_thread_pool_inner::FixedThreadPoolInner,
        fixed_thread_pool_lifecycle::FixedThreadPoolLifecycle,
        fixed_worker::wait_for_fixed_pool_work,
    },
};

use super::mod_tests::{
    create_runtime,
    wait_until,
};

#[test]
fn test_fixed_worker_runs_submitted_task() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");
    let value = pool
        .submit_callable(|| Ok::<_, io::Error>(42))
        .expect("running pool should accept task")
        .get()
        .expect("task should complete");

    assert_eq!(value, 42);
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_fixed_worker_wait_returns_when_running_pool_has_pending_wake() {
    let inner = FixedThreadPoolInner::new(1, None, Vec::new());
    inner.pending_worker_wakes.store(1, Ordering::Release);

    assert!(wait_for_fixed_pool_work(&inner));
    assert_eq!(inner.idle_worker_count.load(Ordering::Acquire), 0);
}

#[test]
fn test_fixed_worker_wait_returns_for_shutdown_queue_and_shutdown_completion() {
    let inner = FixedThreadPoolInner::new(1, None, Vec::new());
    inner
        .state
        .write(|state| state.lifecycle = FixedThreadPoolLifecycle::Shutdown);
    inner.queued_task_count.store(1, Ordering::Release);

    assert!(wait_for_fixed_pool_work(&inner));

    inner.queued_task_count.store(0, Ordering::Release);
    assert!(!wait_for_fixed_pool_work(&inner));
}

#[test]
fn test_fixed_worker_wait_unparks_when_running_work_arrives() {
    let inner = Arc::new(FixedThreadPoolInner::new(1, None, Vec::new()));
    let worker_inner = Arc::clone(&inner);
    let waiter = std::thread::spawn(move || wait_for_fixed_pool_work(&worker_inner));

    wait_until(|| inner.idle_worker_count.load(Ordering::Acquire) == 1);
    inner.queued_task_count.store(1, Ordering::Release);
    inner.state.notify_all();

    assert!(waiter.join().expect("worker wait should return"));
}

#[test]
fn test_fixed_worker_wait_unparks_when_shutdown_inflight_work_finishes() {
    let inner = Arc::new(FixedThreadPoolInner::new(1, None, Vec::new()));
    inner
        .state
        .write(|state| state.lifecycle = FixedThreadPoolLifecycle::Shutdown);
    inner.inflight_submissions.store(1, Ordering::Release);
    let worker_inner = Arc::clone(&inner);
    let waiter = std::thread::spawn(move || wait_for_fixed_pool_work(&worker_inner));

    wait_until(|| inner.idle_worker_count.load(Ordering::Acquire) == 1);
    inner.inflight_submissions.store(0, Ordering::Release);
    inner.state.notify_all();

    assert!(!waiter.join().expect("worker wait should return"));
}
