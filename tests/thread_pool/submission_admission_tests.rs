use std::io;

use qubit_thread_pool::service::{
    ExecutorService,
    RejectedExecution,
    ThreadPool,
};

use super::create_runtime;

#[test]
fn test_submission_admission_closes_when_pool_shutdown_starts() {
    let pool = ThreadPool::new(1).unwrap();
    pool.shutdown();
    let result = pool.submit(|| Ok::<_, io::Error>(()));

    assert!(matches!(result, Err(RejectedExecution::Shutdown)));
    create_runtime().block_on(pool.await_termination());
}
