use std::io;

use qubit_thread_pool::{ExecutorService, FixedThreadPool};

use super::mod_tests::create_runtime;

#[test]
fn test_fixed_worker_runtime_uses_configured_thread_name_prefix() {
    let pool = FixedThreadPool::builder()
        .pool_size(1)
        .thread_name_prefix("fixed-runtime-check")
        .build()
        .expect("fixed thread pool should build");
    let thread_name = pool
        .submit_callable(|| {
            Ok::<_, io::Error>(std::thread::current().name().unwrap_or_default().to_owned())
        })
        .expect("running pool should accept task")
        .get()
        .expect("task should complete");

    assert!(thread_name.starts_with("fixed-runtime-check-"));
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
}
