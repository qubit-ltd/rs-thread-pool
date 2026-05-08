use std::io;

use qubit_thread_pool::{
    ExecutorService,
    PoolJob,
    ThreadPool,
    dynamic::thread_pool_worker_queue::ThreadPoolWorkerQueue,
};

fn noop_job() -> PoolJob {
    PoolJob::new(Box::new(|| {}), Box::new(|| {}))
}

#[test]
fn test_worker_queue_executes_multiple_enqueued_jobs() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(1)
        .queue_capacity(4)
        .build()
        .unwrap();
    let handles: Vec<_> = (0..4)
        .map(|value| {
            pool.submit_callable(move || Ok::<_, io::Error>(value))
                .unwrap()
        })
        .collect();
    let mut values: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.get().unwrap())
        .collect();

    values.sort_unstable();
    assert_eq!(vec![0, 1, 2, 3], values);
    pool.shutdown();
}

#[test]
fn test_thread_pool_worker_queue_drains_local_jobs() {
    let queue = ThreadPoolWorkerQueue::new(3);

    assert_eq!(queue.worker_index(), 3);
    assert!(queue.pop_front().is_none());
    queue.push_back(noop_job());
    queue.push_back(noop_job());

    let jobs = queue.drain();

    assert_eq!(jobs.len(), 2);
    assert!(queue.steal_back().is_none());
}
