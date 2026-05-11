use std::time::Duration;

use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
};

pub(crate) fn wait_started(receiver: std::sync::mpsc::Receiver<()>) {
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("task should start within timeout");
}

pub(crate) fn wait_until<F>(mut condition: F)
where
    F: FnMut() -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(condition(), "condition should become true within timeout");
}

#[test]
fn test_fixed_module_exports_thread_pool_entrypoint() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");

    assert_eq!(pool.pool_size(), 1);
    pool.shutdown();
    pool.wait_termination();
}
