use std::{
    io,
    sync::mpsc,
};

use qubit_thread_pool::{
    ExecutorService,
    TaskExecutionError,
    ThreadPool,
};

use super::mod_tests::wait_started;

#[test]
fn test_thread_pool_worker_queue_path_runs_bounded_public_workload() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(1)
        .queue_capacity(4)
        .build()
        .expect("thread pool should be created");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = pool
        .submit_tracked(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    wait_started(started_rx);
    let queued = pool
        .submit_callable(|| Ok::<usize, io::Error>(7))
        .expect("queued task should be accepted");

    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    running.get().expect("running task should complete");
    assert_eq!(queued.get().expect("queued task should complete"), 7);
    pool.shutdown();
    pool.wait_termination();
}

#[test]
fn test_thread_pool_worker_queue_path_is_drained_by_public_stop() {
    let pool = ThreadPool::builder()
        .core_pool_size(1)
        .maximum_pool_size(1)
        .queue_capacity(4)
        .build()
        .expect("thread pool should be created");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = pool
        .submit_tracked(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    wait_started(started_rx);
    let queued = pool
        .submit_tracked_callable(|| Ok::<usize, io::Error>(7))
        .expect("queued task should be accepted");

    let report = pool.stop();

    assert_eq!(report.queued, 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    running.get().expect("running task should complete");
    pool.wait_termination();
}
