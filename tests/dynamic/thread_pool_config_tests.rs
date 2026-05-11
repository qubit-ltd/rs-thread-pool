use std::time::Duration;

use qubit_thread_pool::{
    ExecutorService,
    ThreadPool,
};

#[test]
fn test_thread_pool_config_is_reflected_by_builder_and_stats() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(3)
        .queue_capacity(8)
        .thread_name_prefix("config-check")
        .keep_alive(Duration::from_millis(25))
        .allow_core_thread_timeout(true)
        .build()
        .unwrap();
    let stats = pool.stats();

    assert_eq!(1, stats.core_pool_size);
    assert_eq!(3, stats.maximum_pool_size);
    assert_eq!(0, pool.queued_count());
    pool.shutdown();
    pool.wait_termination();
}
