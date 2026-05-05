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
fn test_thread_pool_state_reports_queue_saturation_and_shutdown_cancellation() {
    let pool = create_single_worker_pool();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = pool
        .submit(move || {
            started_tx.send(()).unwrap();
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .unwrap();
    wait_started(started_rx);
    let queued = pool.submit(|| Ok::<_, io::Error>(())).unwrap();
    let report = pool.shutdown_now();

    assert_eq!(1, report.queued);
    assert_eq!(1, report.cancelled);
    assert!(queued.is_done());
    release_tx.send(()).unwrap();
    running.get().unwrap();
    create_runtime().block_on(pool.await_termination());
}
