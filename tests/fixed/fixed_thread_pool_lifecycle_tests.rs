use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_thread_pool::FixedThreadPool;

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

#[test]
fn test_fixed_thread_pool_wait_termination_timeout_handles_max_duration() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");
    pool.shutdown();
    pool.wait_termination();

    assert!(pool.wait_termination_timeout(Duration::MAX));
}
