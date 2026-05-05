use std::{
    io,
    sync::mpsc,
};

use qubit_thread_pool::service::ExecutorService;

use super::{
    create_runtime,
    create_single_worker_pool,
    wait_started,
};

#[test]
fn test_thread_pool_inner_tracks_running_queued_and_completed_counts() {
    let pool = create_single_worker_pool();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = pool
        .submit(move || {
            started_tx.send(()).unwrap();
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .unwrap();
    wait_started(started_rx);
    let queued = pool.submit_callable(|| Ok::<_, io::Error>(11)).unwrap();

    assert_eq!(1, pool.running_count());
    assert_eq!(1, pool.queued_count());
    release_tx.send(()).unwrap();
    first.get().unwrap();
    assert_eq!(11, queued.get().unwrap());
    pool.shutdown();
    create_runtime().block_on(pool.await_termination());
    assert!(pool.stats().completed_tasks >= 2);
}
