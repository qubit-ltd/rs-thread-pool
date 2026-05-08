use qubit_thread_pool::{
    ExecutorService,
    ThreadPool,
};

use super::mod_tests::create_runtime;

#[test]
fn test_thread_pool_lifecycle_accessors_report_running_shutdown_and_terminated() {
    let pool = ThreadPool::new(1).unwrap();
    assert!(!pool.is_shutdown());
    assert!(!pool.is_terminated());

    pool.shutdown();

    assert!(pool.is_shutdown());
    create_runtime().block_on(pool.await_termination());
    assert!(pool.is_terminated());
}
