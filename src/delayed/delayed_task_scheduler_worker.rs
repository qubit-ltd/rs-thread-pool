/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::{
    sync::{
        Arc,
        atomic::Ordering,
    },
    time::Instant,
};

use qubit_executor::service::ExecutorServiceLifecycle;

use super::delayed_task_scheduler_inner::DelayedTaskSchedulerInner;
use super::delayed_task_scheduler_state::DelayedTaskSchedulerState;
use super::delayed_task_state::is_task_cancelled;

/// Worker loop entry point for delayed task schedulers.
pub struct DelayedTaskSchedulerWorker;

impl DelayedTaskSchedulerWorker {
    /// Runs the delayed task scheduler loop.
    ///
    /// # Parameters
    ///
    /// * `inner` - Shared scheduler state.
    pub fn run(inner: Arc<DelayedTaskSchedulerInner>) {
        run_delayed_scheduler(inner);
    }
}

/// Runs the delayed task scheduler loop.
///
/// # Parameters
///
/// * `inner` - Shared scheduler state.
fn run_delayed_scheduler(inner: Arc<DelayedTaskSchedulerInner>) {
    loop {
        let task = {
            let mut state = inner.state.lock().expect("scheduler state should lock");
            loop {
                prune_cancelled_front(&mut state);
                if state.lifecycle == ExecutorServiceLifecycle::Stopping {
                    inner.terminate(&mut state);
                    return;
                }
                if state.tasks.is_empty() && state.lifecycle != ExecutorServiceLifecycle::Running {
                    inner.terminate(&mut state);
                    return;
                }
                let Some(next_deadline) = state.tasks.peek().map(|task| task.deadline) else {
                    state = inner
                        .condition
                        .wait(state)
                        .expect("scheduler state wait should not poison");
                    continue;
                };
                let now = Instant::now();
                if next_deadline > now {
                    let timeout = next_deadline.saturating_duration_since(now);
                    let (next_state, _) = inner
                        .condition
                        .wait_timeout(state, timeout)
                        .expect("scheduler state wait should not poison");
                    state = next_state;
                    continue;
                }
                break state.tasks.pop();
            }
        };
        if let Some(mut task) = task {
            if !inner.start_task_state(&task.state) {
                continue;
            }
            let Some(action) = task.task.take() else {
                continue;
            };
            inner.running_task_count.fetch_add(1, Ordering::AcqRel);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
            inner.running_task_count.fetch_sub(1, Ordering::AcqRel);
            inner.completed_task_count.fetch_add(1, Ordering::AcqRel);
            inner.condition.notify_all();
        }
    }
}

/// Removes already-cancelled tasks from the front of the deadline heap.
///
/// # Parameters
///
/// * `state` - Locked scheduler state.
fn prune_cancelled_front(state: &mut DelayedTaskSchedulerState) {
    while state
        .tasks
        .peek()
        .is_some_and(|task| is_task_cancelled(&task.state))
    {
        state.tasks.pop();
    }
}
