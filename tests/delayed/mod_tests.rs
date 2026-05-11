use std::{
    sync::mpsc,
    time::Duration,
};

use qubit_thread_pool::DelayedTaskScheduler;

#[test]
fn test_delayed_module_exports_scheduler_entrypoint() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-module").expect("scheduler should start");
    let (sent_tx, sent_rx) = mpsc::channel();

    scheduler
        .schedule(Duration::from_millis(1), move || {
            sent_tx.send(()).expect("scheduled task should send");
        })
        .expect("running scheduler should accept task");

    sent_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("scheduled task should run");
    scheduler.shutdown();
    scheduler.wait_termination();
}
