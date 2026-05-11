use std::io;

use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
};

use super::mod_tests::wait_until;

#[test]
fn test_fixed_thread_pool_inner_tracks_public_task_counts() {
    let pool = FixedThreadPool::new(1).expect("fixed thread pool should build");
    let handle = pool
        .submit_callable(|| Ok::<_, io::Error>(7))
        .expect("running pool should accept task");

    assert_eq!(handle.get().expect("task should complete"), 7);
    wait_until(|| pool.stats().completed_tasks == 1);
    let stats = pool.stats();
    assert_eq!(stats.submitted_tasks, 1);
    assert_eq!(stats.completed_tasks, 1);
    pool.shutdown();
    pool.wait_termination();
}
