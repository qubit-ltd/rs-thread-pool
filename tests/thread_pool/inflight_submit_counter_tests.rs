use std::{
    io,
    sync::mpsc,
};

use qubit_thread_pool::service::{
    ExecutorService,
    RejectedExecution,
};

use super::{
    create_runtime,
    create_single_worker_pool,
    wait_started,
};

#[test]
fn test_inflight_submit_counter_is_observable_when_shutdown_rejects_late_submit() {
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
    pool.shutdown();
    let rejected = pool.submit(|| Ok::<_, io::Error>(()));
    release_tx.send(()).unwrap();
    first.get().unwrap();
    create_runtime().block_on(pool.await_termination());

    assert!(matches!(rejected, Err(RejectedExecution::Shutdown)));
    assert!(pool.is_terminated());
}
