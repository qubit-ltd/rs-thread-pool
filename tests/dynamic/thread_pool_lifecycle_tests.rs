use qubit_thread_pool::{
    ExecutorService,
    ExecutorServiceLifecycle,
    ThreadPool,
};

#[test]
fn test_thread_pool_lifecycle_accessors_report_running_shutdown_and_terminated() {
    let pool = ThreadPool::new(1).unwrap();
    assert!(!pool.is_not_running());
    assert!(!pool.is_terminated());

    pool.shutdown();

    assert!(pool.is_not_running());
    pool.wait_termination();
    assert!(pool.is_terminated());
}

#[test]
fn test_thread_pool_lifecycle_reports_shutting_down_with_running_work() {
    let pool = ThreadPool::new(1).expect("thread pool should be created");
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let handle = pool
        .submit_tracked(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            Ok::<(), std::io::Error>(())
        })
        .expect("thread pool should accept task");

    super::mod_tests::wait_started(started_rx);
    assert_eq!(pool.lifecycle(), ExecutorServiceLifecycle::Running);
    pool.shutdown();

    assert_eq!(pool.lifecycle(), ExecutorServiceLifecycle::ShuttingDown);
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    handle.get().expect("running task should complete");
    pool.wait_termination();
    assert_eq!(pool.lifecycle(), ExecutorServiceLifecycle::Terminated);
}
