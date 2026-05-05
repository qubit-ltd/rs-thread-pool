use std::io;

use qubit_thread_pool::service::{
    ExecutorService,
    ThreadPool,
};

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
