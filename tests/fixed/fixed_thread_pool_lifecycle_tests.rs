use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
};

use super::mod_tests::create_runtime;

#[test]
fn test_fixed_thread_pool_lifecycle_reports_shutdown_and_termination() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");

    assert!(!pool.is_shutdown());
    pool.shutdown();
    assert!(pool.is_shutdown());
    create_runtime().block_on(pool.await_termination());

    assert!(pool.is_terminated());
}
