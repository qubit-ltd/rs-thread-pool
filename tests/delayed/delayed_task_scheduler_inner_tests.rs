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

use qubit_executor::service::{
    ExecutorServiceLifecycle,
    RejectedExecution,
};
use qubit_thread_pool::DelayedTaskScheduler;
use qubit_thread_pool::delayed::{
    delayed_task_scheduler_inner::DelayedTaskSchedulerInner,
    delayed_task_state::DelayedTaskState,
    scheduled_task::ScheduledTask,
};

#[test]
fn test_delayed_task_scheduler_inner_rejects_after_shutdown_via_public_scheduler() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-inner-public").expect("scheduler should start");

    scheduler.shutdown();
    assert!(matches!(
        scheduler.schedule(Duration::ZERO, || {}),
        Err(RejectedExecution::Shutdown),
    ));
    scheduler.wait_termination();
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
fn test_delayed_task_scheduler_inner_stop_cancels_heap_tasks() {
    let inner = DelayedTaskSchedulerInner::new();
    let task_state = Arc::new(AtomicU8::new(DelayedTaskState::PENDING));

    {
        let mut state = inner.state.lock();
        state.tasks.push(ScheduledTask::new(
            Instant::now() + Duration::from_secs(10),
            0,
            Arc::clone(&task_state),
            Box::new(|| {}),
        ));
    }
    inner.queued_task_count.store(1, Ordering::Release);

    let report = inner.stop();

    assert_eq!(report.queued, 1);
    assert_eq!(report.cancelled, 1);
    assert!(DelayedTaskState::is_cancelled(&task_state));
    assert!(inner.is_not_running());
    assert_eq!(inner.running_count(), 0);
}

#[test]
fn test_delayed_task_scheduler_inner_lifecycle_reports_state_and_termination() {
    let inner = DelayedTaskSchedulerInner::new();

    assert_eq!(inner.lifecycle(), ExecutorServiceLifecycle::Running);

    {
        let mut state = inner.state.lock();
        state.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
    }
    assert_eq!(inner.lifecycle(), ExecutorServiceLifecycle::ShuttingDown);

    {
        let mut state = inner.state.lock();
        state.terminated = true;
    }
    assert_eq!(inner.lifecycle(), ExecutorServiceLifecycle::Terminated);
    assert!(inner.is_terminated());
}
