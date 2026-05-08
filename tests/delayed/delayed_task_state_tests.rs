use std::sync::atomic::AtomicU8;

use qubit_thread_pool::delayed::delayed_task_state::DelayedTaskState;

#[test]
fn test_delayed_task_state_transitions_from_pending_once() {
    let state = AtomicU8::new(DelayedTaskState::PENDING);

    assert!(DelayedTaskState::cancel(&state));
    assert!(DelayedTaskState::is_cancelled(&state));
    assert!(!DelayedTaskState::start(&state));
}

#[test]
fn test_delayed_task_state_start_prevents_later_cancellation() {
    let state = AtomicU8::new(DelayedTaskState::PENDING);

    assert!(DelayedTaskState::start(&state));
    assert!(!DelayedTaskState::cancel(&state));
    assert!(!DelayedTaskState::is_cancelled(&state));
}
