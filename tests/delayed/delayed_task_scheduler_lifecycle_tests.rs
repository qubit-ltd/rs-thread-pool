use qubit_thread_pool::DelayedTaskScheduler;

use super::mod_tests::create_runtime;

#[test]
fn test_delayed_task_scheduler_lifecycle_reports_shutdown_and_termination() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-lifecycle").expect("scheduler should start");

    assert!(!scheduler.is_shutdown());
    scheduler.shutdown();
    create_runtime().block_on(scheduler.await_termination());

    assert!(scheduler.is_shutdown());
    assert!(scheduler.is_terminated());
}
