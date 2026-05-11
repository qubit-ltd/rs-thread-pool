use std::{
    sync::mpsc,
    time::Duration,
};

use qubit_thread_pool::{
    DelayedTaskScheduler,
    ExecutorServiceLifecycle,
};

#[test]
fn test_delayed_task_scheduler_lifecycle_reports_shutdown_and_termination() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-lifecycle").expect("scheduler should start");

    assert!(!scheduler.is_not_running());
    scheduler.shutdown();
    scheduler.wait_termination();

    assert!(scheduler.is_not_running());
    assert!(scheduler.is_terminated());
}

#[test]
fn test_delayed_task_scheduler_lifecycle_accessors_report_running_and_stopping() {
    let scheduler = DelayedTaskScheduler::new("test-delayed-lifecycle-accessors")
        .expect("scheduler should start");

    assert_eq!(scheduler.lifecycle(), ExecutorServiceLifecycle::Running);
    assert!(scheduler.is_running());
    assert!(!scheduler.is_shutting_down());
    assert!(!scheduler.is_stopping());

    scheduler.stop();
    scheduler.wait_termination();

    assert_eq!(scheduler.lifecycle(), ExecutorServiceLifecycle::Terminated);
    assert!(!scheduler.is_running());
    assert!(!scheduler.is_shutting_down());
    assert!(!scheduler.is_stopping());
}

#[test]
fn test_delayed_task_scheduler_lifecycle_reports_shutting_down_with_pending_work() {
    let scheduler = DelayedTaskScheduler::new("test-delayed-lifecycle-shutting-down")
        .expect("scheduler should start");
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    scheduler
        .schedule(Duration::ZERO, move || {
            started_tx
                .send(())
                .expect("test should receive delayed task start signal");
            release_rx.recv().expect("test should release delayed task");
        })
        .expect("delayed task should schedule");

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("delayed task should start");
    scheduler.shutdown();

    assert_eq!(
        scheduler.lifecycle(),
        ExecutorServiceLifecycle::ShuttingDown
    );
    assert!(scheduler.is_shutting_down());

    release_tx
        .send(())
        .expect("delayed task should receive release signal");
    scheduler.wait_termination();
}
