/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! Thread pool implementation. Each source file is named after the single
//! public type it defines (snake_case), except private helper types split into
//! their own similarly named files.

// 子模块 `thread_pool` 存放公开类型 `ThreadPool`，与父模块同名是刻意分层；避免 clippy::module_inception 误报
#![allow(clippy::module_inception)]

mod pool_job;
mod thread_pool;
mod thread_pool_build_error;
mod thread_pool_builder;
mod thread_pool_config;
mod thread_pool_inner;
mod thread_pool_lifecycle;
mod thread_pool_state;
mod thread_pool_stats;

pub use pool_job::PoolJob;
pub use thread_pool::ThreadPool;
pub use thread_pool_build_error::ThreadPoolBuildError;
pub use thread_pool_builder::ThreadPoolBuilder;
pub use thread_pool_stats::ThreadPoolStats;
