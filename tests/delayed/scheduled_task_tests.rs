use std::{
    cmp::Ordering as CompareOrdering,
    sync::{
        Arc,
        atomic::AtomicU8,
    },
    time::{
        Duration,
        Instant,
    },
};

use qubit_thread_pool::delayed::scheduled_task::ScheduledTask;

fn noop_task() {}

#[test]
fn test_scheduled_task_orders_earliest_deadline_first_for_binary_heap() {
    let now = Instant::now();
    let first = ScheduledTask::new(
        now + Duration::from_millis(5),
        1,
        Arc::new(AtomicU8::new(0)),
        Box::new(noop_task),
    );
    let later = ScheduledTask::new(
        now + Duration::from_millis(10),
        0,
        Arc::new(AtomicU8::new(0)),
        Box::new(noop_task),
    );

    assert_eq!(first.cmp(&later), CompareOrdering::Greater);
    assert_eq!(first.partial_cmp(&later), Some(CompareOrdering::Greater));
    assert!(first != later);
}

#[test]
fn test_scheduled_task_equal_deadline_and_sequence_compare_equal() {
    let now = Instant::now();
    let first = ScheduledTask::new(now, 7, Arc::new(AtomicU8::new(0)), Box::new(noop_task));
    let same_position = ScheduledTask::new(now, 7, Arc::new(AtomicU8::new(0)), Box::new(noop_task));

    assert!(first == same_position);
}

#[test]
fn test_scheduled_task_uses_sequence_as_deadline_tiebreaker() {
    let now = Instant::now();
    let earlier_sequence =
        ScheduledTask::new(now, 1, Arc::new(AtomicU8::new(0)), Box::new(noop_task));
    let later_sequence =
        ScheduledTask::new(now, 2, Arc::new(AtomicU8::new(0)), Box::new(noop_task));

    assert_eq!(
        earlier_sequence.cmp(&later_sequence),
        CompareOrdering::Greater,
    );
}
