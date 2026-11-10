// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod dynamic;
mod fixed;

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::FixedThreadPool;
use qubit_thread_pool::ThreadPool;

#[test]
fn test_lib_exports_primary_executor_types() {
    let dynamic = ThreadPool::new(1);
    let fixed = FixedThreadPool::new(1);

    assert!(dynamic.is_ok());
    assert!(fixed.is_ok());
}

struct WorkerDestructorPayload;

impl Drop for WorkerDestructorPayload {
    fn drop(&mut self) {
        panic!("nested worker payload destructor panic");
    }
}

struct WorkerDestructorCapture;

impl Drop for WorkerDestructorCapture {
    fn drop(&mut self) {
        std::panic::panic_any(WorkerDestructorPayload);
    }
}

#[derive(Debug)]
struct WorkerDestructorError;

impl Drop for WorkerDestructorError {
    fn drop(&mut self) {
        std::panic::panic_any(WorkerDestructorPayload);
    }
}

struct CountedWorkerPayload(Arc<AtomicUsize>);

impl Drop for CountedWorkerPayload {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

macro_rules! worker_destructor_panic_test {
    ($name:ident, $pool:expr, $error_result:expr, $with_hooks:expr) => {
        #[test]
        fn $name() {
            let after_task_count = Arc::new(AtomicUsize::new(0));
            let pool = $pool(after_task_count.clone(), $with_hooks);
            let first = if $error_result {
                pool.submit(|| Err::<(), _>(WorkerDestructorError))
            } else {
                let capture = WorkerDestructorCapture;
                pool.submit(move || {
                    let _borrow = &capture;
                    Ok::<(), io::Error>(())
                })
            };
            first.expect("first task accepted");

            let (tx, rx) = mpsc::channel();
            let second = pool
                .submit_callable(move || {
                    tx.send(()).expect("observer exists");
                    Ok::<(), io::Error>(())
                })
                .expect("second task accepted");
            rx.recv_timeout(Duration::from_secs(5))
                .expect("worker survives task destructor panic");
            pool.shutdown();
            assert!(pool.wait_termination_timeout(Duration::from_secs(5)));
            second.get().expect("second task succeeds");
            let stats = pool.stats();
            assert_eq!(stats.submitted_tasks, 2);
            assert_eq!(stats.completed_tasks, 2);
            assert_eq!(stats.queued_tasks, 0);
            assert_eq!(stats.running_tasks, 0);
            assert_eq!(stats.cancelled_tasks, 0);
            if $with_hooks {
                assert_eq!(after_task_count.load(Ordering::SeqCst), 2);
            }
        }
    };
}

fn dynamic_pool(after_count: Arc<AtomicUsize>, hooks: bool) -> ThreadPool {
    let builder = ThreadPool::builder().pool_size(1);
    if hooks {
        builder
            .before_task(|_| {})
            .after_task(move |_| {
                after_count.fetch_add(1, Ordering::SeqCst);
            })
            .build()
            .expect("dynamic pool should be created")
    } else {
        builder.build().expect("dynamic pool should be created")
    }
}

fn fixed_pool(after_count: Arc<AtomicUsize>, hooks: bool) -> FixedThreadPool {
    let builder = FixedThreadPool::builder().pool_size(1);
    if hooks {
        builder
            .before_task(|_| {})
            .after_task(move |_| {
                after_count.fetch_add(1, Ordering::SeqCst);
            })
            .build()
            .expect("fixed pool should be created")
    } else {
        builder.build().expect("fixed pool should be created")
    }
}

worker_destructor_panic_test!(
    test_dynamic_worker_destructor_panic_without_hooks,
    dynamic_pool,
    false,
    false
);
worker_destructor_panic_test!(
    test_dynamic_worker_destructor_panic_with_hooks,
    dynamic_pool,
    false,
    true
);
worker_destructor_panic_test!(
    test_dynamic_worker_destructor_error_panic_without_hooks,
    dynamic_pool,
    true,
    false
);
worker_destructor_panic_test!(
    test_dynamic_worker_destructor_error_panic_with_hooks,
    dynamic_pool,
    true,
    true
);
worker_destructor_panic_test!(
    test_fixed_worker_destructor_panic_without_hooks,
    fixed_pool,
    false,
    false
);
worker_destructor_panic_test!(test_fixed_worker_destructor_panic_with_hooks, fixed_pool, false, true);
worker_destructor_panic_test!(
    test_fixed_worker_destructor_error_panic_without_hooks,
    fixed_pool,
    true,
    false
);
worker_destructor_panic_test!(
    test_fixed_worker_destructor_error_panic_with_hooks,
    fixed_pool,
    true,
    true
);

#[test]
fn test_dynamic_worker_destructor_ordinary_payload_drops_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let payload = CountedWorkerPayload(Arc::clone(&drops));
    let pool = ThreadPool::new(1).expect("dynamic pool should be created");
    pool.submit(move || {
        let _borrow = &payload;
        Ok::<(), io::Error>(())
    })
    .expect("task accepted");
    pool.shutdown();
    assert!(pool.wait_termination_timeout(Duration::from_secs(5)));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn test_fixed_worker_destructor_ordinary_payload_drops_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let payload = CountedWorkerPayload(Arc::clone(&drops));
    let pool = FixedThreadPool::new(1).expect("fixed pool should be created");
    pool.submit(move || {
        let _borrow = &payload;
        Ok::<(), io::Error>(())
    })
    .expect("task accepted");
    pool.shutdown();
    assert!(pool.wait_termination_timeout(Duration::from_secs(5)));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
