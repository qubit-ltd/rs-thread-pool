use std::{sync::mpsc, time::Duration};

use qubit_thread_pool::DelayedTaskScheduler;

use super::mod_tests::create_runtime;

#[test]
fn test_delayed_task_scheduler_state_tracks_pending_count() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-state").expect("scheduler should start");
    let (sender, receiver) = mpsc::channel();

    scheduler
        .schedule(Duration::from_millis(10), move || {
            sender.send(()).expect("task should send completion");
        })
        .expect("task should schedule");

    assert_eq!(scheduler.queued_count(), 1);
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("task should run");
    scheduler.shutdown();
    create_runtime().block_on(scheduler.await_termination());
}
