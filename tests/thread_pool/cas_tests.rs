use std::io;

use qubit_thread_pool::service::{
    ExecutorService,
    ThreadPool,
};

use super::create_runtime;

#[test]
fn test_cas_backed_pool_accepts_and_rejects_work_across_shutdown() {
    let pool = ThreadPool::new(1).expect("thread pool should build");
    let value = pool
        .submit_callable(|| Ok::<_, io::Error>(7))
        .expect("running pool should accept work")
        .get()
        .expect("work should complete");

    pool.shutdown();

    assert_eq!(7, value);
    assert!(pool.submit(|| Ok::<_, io::Error>(())).is_err());
    create_runtime().block_on(pool.await_termination());
}
