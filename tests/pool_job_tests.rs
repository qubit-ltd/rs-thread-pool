use std::sync::{
    Arc,
    atomic::{
        AtomicBool,
        Ordering,
    },
};

use qubit_thread_pool::{
    ExecutorService,
    ThreadPool,
};

#[test]
fn test_pool_job_internals_run_via_public_submit() {
    let pool = ThreadPool::new(1).expect("thread pool should build");
    let completed = Arc::new(AtomicBool::new(false));
    let completed_for_task = Arc::clone(&completed);

    pool.submit(move || {
        completed_for_task.store(true, Ordering::Release);
        Ok::<(), std::io::Error>(())
    })
    .expect("task should submit");
    pool.join();

    assert!(completed.load(Ordering::Acquire));
    pool.shutdown();
    pool.wait_termination();
}
