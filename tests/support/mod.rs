/*******************************************************************************
 *
 *    Copyright (c) 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Test support modules for private queue primitives.

pub(crate) mod thread_pool {
    pub(crate) use qubit_thread_pool::PoolJob;
}

#[path = "../../src/queue_steal_source.rs"]
pub(crate) mod queue_steal_source;

#[path = "../../src/worker_queue.rs"]
pub(crate) mod worker_queue;
