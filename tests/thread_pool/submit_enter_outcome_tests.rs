use std::io;

use qubit_thread_pool::service::{
    ExecutorService,
    RejectedExecution,
    ThreadPool,
};

use super::create_runtime;

#[test]
fn test_submit_enter_outcome_accepts_running_pool_and_rejects_closed_pool() {
    let pool = ThreadPool::new(1).unwrap();
    pool.submit(|| Ok::<_, io::Error>(()))
        .unwrap()
        .get()
        .unwrap();
    pool.shutdown();

    assert!(matches!(
        pool.submit(|| Ok::<_, io::Error>(())),
        Err(RejectedExecution::Shutdown),
    ));
    create_runtime().block_on(pool.await_termination());
}
