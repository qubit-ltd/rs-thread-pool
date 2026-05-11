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
    io,
    sync::mpsc,
};

use qubit_thread_pool::{
    ExecutorService,
    FixedThreadPool,
};

/// Tests fixed worker runtime metadata through configured thread names.
#[test]
fn test_fixed_worker_runtime_uses_configured_thread_name_prefix() {
    let pool = FixedThreadPool::builder()
        .pool_size(1)
        .thread_name_prefix("fixed-runtime-test")
        .build()
        .expect("fixed thread pool should build");
    let (sender, receiver) = mpsc::channel();

    pool.submit(move || {
        let name = std::thread::current().name().unwrap_or_default().to_owned();
        sender.send(name).expect("test should receive worker name");
        Ok::<_, io::Error>(())
    })
    .expect("task should be accepted");

    let worker_name = receiver.recv().expect("worker should send its name");
    assert!(worker_name.starts_with("fixed-runtime-test-"));

    pool.shutdown();
    pool.wait_termination();
}
