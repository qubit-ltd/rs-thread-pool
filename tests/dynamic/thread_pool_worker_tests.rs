use std::io;

use qubit_thread_pool::{
    ExecutorService,
    ThreadPool,
};

#[test]
fn test_worker_runs_job_on_named_worker_thread() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(1)
        .thread_name_prefix("runtime-check")
        .build()
        .unwrap();
    let name = pool
        .submit_callable(|| {
            Ok::<_, io::Error>(std::thread::current().name().unwrap_or_default().to_owned())
        })
        .unwrap()
        .get()
        .unwrap();

    assert!(name.starts_with("runtime-check-"));
    pool.shutdown();
}
