use std::{
    io,
    sync::{
        Arc,
        atomic::Ordering,
    },
};

use qubit_thread_pool::{
    ExecutorService,
    ExecutorServiceLifecycle,
    FixedThreadPool,
    PoolJob,
    fixed::fixed_thread_pool_inner::FixedThreadPoolInner,
};

use super::mod_tests::wait_until;

fn noop_job() -> PoolJob {
    PoolJob::new(Box::new(|| {}), Box::new(|| {}))
}

fn create_inner(pool_size: usize, queue_capacity: Option<usize>) -> Arc<FixedThreadPoolInner> {
    Arc::new(FixedThreadPoolInner::new(pool_size, queue_capacity))
}

#[test]
fn test_fixed_thread_pool_inner_tracks_task_counts() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");
    let handle = pool
        .submit_callable(|| Ok::<_, io::Error>(7))
        .expect("running pool should accept task");

    assert_eq!(handle.get().expect("task should complete"), 7);
    let stats = pool.stats();
    assert_eq!(stats.submitted_tasks, 1);
    assert_eq!(stats.completed_tasks, 1);

    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_fixed_thread_pool_inner_cancels_claimed_job_when_stopping() {
    let inner = create_inner(1, None);
    inner.queued_task_count.store(1, Ordering::Release);
    inner.stop_now.store(true, Ordering::Release);

    assert!(inner.accept_claimed_job(noop_job()).is_none());

    assert_eq!(inner.queued_count(), 0);
    assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 1);
}

#[test]
fn test_fixed_thread_pool_inner_stop_cancels_global_queue_jobs() {
    let inner = create_inner(1, None);
    inner.global_queue.push(noop_job());
    inner.global_queue.push(noop_job());
    inner.queued_task_count.store(2, Ordering::Release);

    let report = inner.stop();

    assert_eq!(report.queued, 2);
    assert_eq!(report.cancelled, 2);
    assert_eq!(inner.queued_count(), 0);
    assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 2);
}

#[test]
fn test_fixed_thread_pool_inner_stop_waits_for_inflight_submitters() {
    let inner = create_inner(1, None);
    inner.inflight_submissions.store(1, Ordering::Release);
    let shutdown_inner = Arc::clone(&inner);

    let shutdown = std::thread::spawn(move || shutdown_inner.stop());
    wait_until(|| {
        inner
            .state
            .read(|state| matches!(state.lifecycle, ExecutorServiceLifecycle::Stopping))
    });
    inner.inflight_submissions.store(0, Ordering::Release);
    inner.state.notify_all();
    let report = shutdown.join().expect("stop should return");

    assert_eq!(report.queued, 0);
    assert!(inner.is_not_running());
}

#[test]
fn test_fixed_thread_pool_inner_notifies_idle_waiters_when_idle() {
    let inner = create_inner(1, None);
    inner.idle_waiter_count.store(1, Ordering::Release);

    inner.notify_idle_waiters_if_idle();

    assert!(inner.is_idle());
}

#[test]
fn test_fixed_thread_pool_inner_lifecycle_and_stats_report_termination() {
    let inner = create_inner(1, None);

    assert_eq!(inner.lifecycle(), ExecutorServiceLifecycle::Running);
    inner
        .state
        .write(|state| state.lifecycle = ExecutorServiceLifecycle::ShuttingDown);

    assert_eq!(inner.lifecycle(), ExecutorServiceLifecycle::Terminated);
    let stats = inner.stats();
    assert_eq!(stats.lifecycle, ExecutorServiceLifecycle::Terminated);
    assert!(stats.terminated);
}
