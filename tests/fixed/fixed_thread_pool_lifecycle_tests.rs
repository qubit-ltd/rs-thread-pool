use qubit_thread_pool::{
    ExecutorService,
    ExecutorServiceLifecycle,
    FixedThreadPool,
};

#[test]
fn test_fixed_thread_pool_lifecycle_reports_shutdown_and_termination() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");

    assert!(!pool.is_not_running());
    assert_eq!(pool.lifecycle(), ExecutorServiceLifecycle::Running);
    pool.shutdown();
    assert!(pool.is_not_running());
    pool.wait_termination();

    assert!(pool.is_terminated());
    assert_eq!(pool.lifecycle(), ExecutorServiceLifecycle::Terminated);
}
