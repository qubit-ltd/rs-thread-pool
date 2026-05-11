use std::sync::atomic::{
    AtomicU8,
    Ordering,
};

use qubit_thread_pool::delayed::delayed_task_state::{
    DelayedTaskState,
    cancel_task_state,
    is_task_cancelled,
    start_task_state,
};

#[test]
fn test_delayed_task_state_transitions_from_pending_once() {
    let state = AtomicU8::new(DelayedTaskState::PENDING);

    assert!(cancel_task_state(&state));
    assert!(DelayedTaskState::is_cancelled(&state));
    assert!(is_task_cancelled(&state));
    assert!(!cancel_task_state(&state));
    assert!(!start_task_state(&state));
}

#[test]
fn test_delayed_task_state_start_prevents_later_cancellation() {
    let state = AtomicU8::new(DelayedTaskState::PENDING);

    assert!(start_task_state(&state));
    assert_eq!(state.load(Ordering::Acquire), DelayedTaskState::STARTED);
    assert!(!cancel_task_state(&state));
    assert!(!DelayedTaskState::is_cancelled(&state));
}
