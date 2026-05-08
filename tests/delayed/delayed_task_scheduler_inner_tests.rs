use std::{
    sync::{
        Arc,
        atomic::{
            AtomicU8,
            Ordering,
        },
    },
    time::{
        Duration,
        Instant,
    },
};

use qubit_executor::service::RejectedExecution;
use qubit_thread_pool::DelayedTaskScheduler;
use qubit_thread_pool::delayed::{
    delayed_task_scheduler_inner::DelayedTaskSchedulerInner,
    delayed_task_state::DelayedTaskState,
    scheduled_task::ScheduledTask,
};

use super::mod_tests::create_runtime;

#[test]
fn test_delayed_task_scheduler_inner_rejects_after_shutdown() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-inner").expect("scheduler should start");

    scheduler.shutdown();
    assert!(matches!(
        scheduler.schedule(Duration::ZERO, || {}),
        Err(RejectedExecution::Shutdown),
    ));
    create_runtime().block_on(scheduler.await_termination());
}

#[test]
fn test_delayed_task_scheduler_inner_updates_task_state_counters() {
    let inner = DelayedTaskSchedulerInner::default();
    let cancelled = AtomicU8::new(DelayedTaskState::PENDING);

    inner.queued_task_count.store(1, Ordering::Release);
    assert!(inner.cancel_task_state(&cancelled));
    assert_eq!(inner.queued_count(), 0);
    assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 1);
    assert!(!inner.cancel_task_state(&cancelled));

    let started = AtomicU8::new(DelayedTaskState::PENDING);
    inner.queued_task_count.store(1, Ordering::Release);
    assert!(inner.start_task_state(&started));
    assert_eq!(inner.queued_count(), 0);
    assert!(!inner.start_task_state(&started));
}

#[test]
fn test_delayed_task_scheduler_inner_shutdown_now_cancels_heap_tasks() {
    let inner = DelayedTaskSchedulerInner::new();
    let task_state = Arc::new(AtomicU8::new(DelayedTaskState::PENDING));

    {
        let mut state = inner.state.lock().expect("scheduler state should lock");
        state.tasks.push(ScheduledTask::new(
            Instant::now() + Duration::from_secs(10),
            0,
            Arc::clone(&task_state),
            Box::new(|| {}),
        ));
    }
    inner.queued_task_count.store(1, Ordering::Release);

    let report = inner.shutdown_now();

    assert_eq!(report.queued, 1);
    assert_eq!(report.cancelled, 1);
    assert!(DelayedTaskState::is_cancelled(&task_state));
    assert!(inner.is_shutdown());
    assert_eq!(inner.running_count(), 0);
}
