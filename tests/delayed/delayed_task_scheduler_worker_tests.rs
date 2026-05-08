use std::{
    sync::{
        Arc,
        atomic::{
            AtomicU8,
            Ordering,
        },
        mpsc,
    },
    time::{
        Duration,
        Instant,
    },
};

use qubit_thread_pool::DelayedTaskScheduler;
use qubit_thread_pool::delayed::{
    delayed_task_scheduler_inner::DelayedTaskSchedulerInner,
    delayed_task_scheduler_lifecycle::DelayedTaskSchedulerLifecycle,
    delayed_task_scheduler_worker::DelayedTaskSchedulerWorker,
    delayed_task_state::DelayedTaskState,
    scheduled_task::ScheduledTask,
};

use super::mod_tests::create_runtime;

#[test]
fn test_delayed_task_scheduler_worker_runs_due_task() {
    let scheduler =
        DelayedTaskScheduler::new("test-delayed-worker").expect("scheduler should start");
    let (sender, receiver) = mpsc::channel();

    scheduler
        .schedule(Duration::ZERO, move || {
            sender.send(11).expect("task should send result");
        })
        .expect("task should schedule");

    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("task should run"),
        11,
    );
    scheduler.shutdown();
    create_runtime().block_on(scheduler.await_termination());
}

#[test]
fn test_delayed_task_scheduler_worker_skips_empty_task_entry() {
    let inner = Arc::new(DelayedTaskSchedulerInner::new());
    let task_state = Arc::new(AtomicU8::new(DelayedTaskState::PENDING));
    let mut task = ScheduledTask::new(Instant::now(), 0, task_state, Box::new(|| {}));
    task.task = None;

    {
        let mut state = inner.state.lock().expect("scheduler state should lock");
        state.tasks.push(task);
    }
    inner.queued_task_count.store(1, Ordering::Release);

    let worker_inner = Arc::clone(&inner);
    let worker = std::thread::spawn(move || DelayedTaskSchedulerWorker::run(worker_inner));
    while inner.queued_count() != 0 {
        std::thread::yield_now();
    }
    {
        let mut state = inner.state.lock().expect("scheduler state should lock");
        state.lifecycle = DelayedTaskSchedulerLifecycle::Stopping;
        inner.condition.notify_all();
    }

    worker.join().expect("worker should exit");
    assert_eq!(inner.completed_task_count.load(Ordering::Acquire), 0);
    assert!(inner.is_terminated());
}
