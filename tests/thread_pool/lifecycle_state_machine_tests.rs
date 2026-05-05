use std::io;

use qubit_thread_pool::service::{
    ExecutorService,
    ThreadPool,
};

use super::create_runtime;

#[test]
fn test_lifecycle_state_machine_transitions_are_observable_through_shutdown_modes() {
    let graceful = ThreadPool::new(1).unwrap();
    graceful.shutdown();
    assert!(graceful.is_shutdown());
    create_runtime().block_on(graceful.await_termination());
    assert!(graceful.is_terminated());

    let immediate = ThreadPool::new(1).unwrap();
    let report = immediate.shutdown_now();
    assert_eq!(0, report.queued);
    assert!(immediate.submit(|| Ok::<_, io::Error>(())).is_err());
    create_runtime().block_on(immediate.await_termination());
    assert!(immediate.is_terminated());
}
