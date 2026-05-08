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
    PoolJob,
    fixed::{
        fixed_thread_pool_inner::FixedThreadPoolInner,
        fixed_thread_pool_lifecycle::FixedThreadPoolLifecycle,
        fixed_worker_runtime::FixedWorkerRuntime,
    },
};

use super::mod_tests::{
    create_runtime,
    wait_until,
};

fn noop_job() -> PoolJob {
    PoolJob::new(Box::new(|| {}), Box::new(|| {}))
}

fn create_inner(
    pool_size: usize,
    queue_capacity: Option<usize>,
) -> (Arc<FixedThreadPoolInner>, Vec<FixedWorkerRuntime>) {
    let runtimes = (0..pool_size)
        .map(FixedWorkerRuntime::new)
        .collect::<Vec<_>>();
    let queues = runtimes
        .iter()
        .map(|runtime| Arc::clone(&runtime.queue))
        .collect::<Vec<_>>();
    (
        Arc::new(FixedThreadPoolInner::new(pool_size, queue_capacity, queues)),
        runtimes,
    )
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
    create_runtime().block_on(pool.await_termination());
}

#[test]
fn test_fixed_thread_pool_inner_cancels_claimed_and_worker_jobs_when_stopping() {
    let (inner, runtimes) = create_inner(1, None);
    let runtime = &runtimes[0];
    runtime.local.push(noop_job());
    runtime.queue.push_back(noop_job());
    inner.queued_task_count.store(3, Ordering::Release);
    inner.stop_now.store(true, Ordering::Release);

    assert!(inner.accept_claimed_job(noop_job(), runtime).is_none());

    assert_eq!(inner.queued_count(), 0);
    assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 3);
}

#[test]
fn test_fixed_thread_pool_inner_shutdown_now_waits_for_inflight_submitters() {
    let (inner, _runtimes) = create_inner(1, None);
    inner.inflight_submissions.store(1, Ordering::Release);
    let shutdown_inner = Arc::clone(&inner);

    let shutdown = std::thread::spawn(move || shutdown_inner.shutdown_now());
    wait_until(|| {
        inner
            .state
            .read(|state| matches!(state.lifecycle, FixedThreadPoolLifecycle::Stopping))
    });
    inner.inflight_submissions.store(0, Ordering::Release);
    inner.state.notify_all();
    let report = shutdown.join().expect("shutdown_now should return");

    assert_eq!(report.queued, 0);
    assert!(inner.is_shutdown());
}
