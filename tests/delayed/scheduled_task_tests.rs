use std::{
    cmp::Ordering as CompareOrdering,
    collections::BinaryHeap,
    sync::{
        Arc,
        atomic::AtomicU8,
    },
    time::{
        Duration,
        Instant,
    },
};

use qubit_thread_pool::delayed::{
    delayed_task_state::DelayedTaskState,
    scheduled_task::ScheduledTask,
};

fn task(deadline: Instant, sequence: usize) -> ScheduledTask {
    ScheduledTask::new(
        deadline,
        sequence,
        Arc::new(AtomicU8::new(DelayedTaskState::PENDING)),
        Box::new(|| {}),
    )
}

#[test]
fn test_scheduled_task_orders_earliest_deadline_first_for_binary_heap() {
    let now = Instant::now();
    let mut heap = BinaryHeap::new();

    heap.push(task(now + Duration::from_millis(20), 0));
    heap.push(task(now + Duration::from_millis(5), 1));

    assert_eq!(heap.pop().expect("heap should contain task").sequence, 1);
    assert_eq!(heap.pop().expect("heap should contain task").sequence, 0);
}

#[test]
fn test_scheduled_task_equal_deadline_and_sequence_compare_equal() {
    let now = Instant::now();
    let first = task(now, 7);
    let second = task(now, 7);

    assert_eq!(first.cmp(&second), CompareOrdering::Equal);
    assert_eq!(first.partial_cmp(&second), Some(CompareOrdering::Equal));
    assert!(first == second);
}

#[test]
fn test_scheduled_task_uses_sequence_as_deadline_tiebreaker() {
    let now = Instant::now();
    let first = task(now, 0);
    let second = task(now, 1);

    assert_eq!(first.cmp(&second), CompareOrdering::Greater);
    assert!(first != second);
}
