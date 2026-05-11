use std::io;

use qubit_thread_pool::{
    ExecutorService,
    ThreadPool,
};

#[test]
fn test_thread_pool_worker_runtime_executes_public_task() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(1)
        .build()
        .expect("thread pool should be created");

    let value = pool
        .submit_callable(|| Ok::<usize, io::Error>(7))
        .expect("task should submit")
        .get()
        .expect("task should complete");

    assert_eq!(value, 7);
    pool.shutdown();
    pool.wait_termination();
}
