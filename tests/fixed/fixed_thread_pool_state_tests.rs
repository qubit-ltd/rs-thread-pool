use std::io;

use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
};

#[test]
fn test_fixed_thread_pool_state_is_reflected_in_stats() {
    let pool = FixedThreadPool::new(2).expect("fixed thread pool should build");
    let handle = pool
        .submit_callable(|| Ok::<_, io::Error>(()))
        .expect("running pool should accept task");

    handle.get().expect("task should complete");
    let stats = pool.stats();
    assert_eq!(stats.core_pool_size, 2);
    assert_eq!(stats.maximum_pool_size, 2);
    assert_eq!(stats.running_tasks, 0);

    pool.shutdown();
    pool.wait_termination();
}
