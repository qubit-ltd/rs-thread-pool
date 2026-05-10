use std::io;

use qubit_thread_pool::{ExecutorService, FixedThreadPool};

use super::mod_tests::create_runtime;

#[test]
fn test_fixed_worker_queue_drains_multiple_submitted_jobs() {
    let pool = FixedThreadPool::builder()
        .pool_size(2)
        .queue_capacity(8)
        .build()
        .expect("fixed thread pool should build");
    let handles = (0..4)
        .map(|value| {
            pool.submit_callable(move || Ok::<_, io::Error>(value))
                .expect("running pool should accept task")
        })
        .collect::<Vec<_>>();
    let mut values = handles
        .into_iter()
        .map(|handle| handle.get().expect("task should complete"))
        .collect::<Vec<_>>();

    values.sort_unstable();
    assert_eq!(values, vec![0, 1, 2, 3]);
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}
