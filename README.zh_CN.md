# Qubit Thread Pool

[![CircleCI](https://circleci.com/gh/qubit-ltd/rs-thread-pool.svg?style=shield)](https://circleci.com/gh/qubit-ltd/rs-thread-pool)
[![Coverage Status](https://coveralls.io/repos/github/qubit-ltd/rs-thread-pool/badge.svg?branch=main)](https://coveralls.io/github/qubit-ltd/rs-thread-pool?branch=main)
[![Crates.io](https://img.shields.io/crates/v/qubit-thread-pool.svg?color=blue)](https://crates.io/crates/qubit-thread-pool)
[![Rust](https://img.shields.io/badge/rust-1.94+-blue.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![English Documentation](https://img.shields.io/badge/docs-English-blue.svg)](README.md)

面向 Rust 的线程池 executor service。

## 概览

Qubit Thread Pool 为同步工作提供基于 OS 线程的 `ExecutorService` 实现。它包含适合突发负载的动态 `ThreadPool`，以及适合稳定 worker 数量的 `FixedThreadPool`。

本 crate 基于 `qubit-executor` 构建，因此与其它 Qubit executor 实现共享任务接受、关闭、取消和 `TaskHandle` 语义。普通使用不需要依赖 Tokio 或 Rayon。

## 功能

- 提供动态 `ThreadPool`，支持分离的 core worker 与 maximum worker 限制。
- 提供固定大小 `FixedThreadPool`，用于可预测的 worker 数量。
- 支持有界或无界队列配置。
- 动态池支持懒创建 worker，也支持预启动 core worker。
- 动态池支持 keep-alive 与可选 core 线程超时。
- 支持配置 worker 线程名前缀和栈大小。
- 提供 `PoolJob`，用于需要提交类型擦除 job 的高级集成场景。
- 提供 `ThreadPoolStats`，用于观察线程池配置和运行时计数。
- 共享 `ExecutorService` 生命周期方法，包括 `shutdown`、`shutdown_now` 和 `await_termination`。
- 提供 Criterion benchmark 与测试数据，用于对比 Qubit 线程池、`threadpool` 和 Rayon。

## 线程池模型

`ThreadPool` 采用常见的 core-size / maximum-size 执行器模型。它会懒创建 worker 直到 core size，之后优先排队；当有界队列无法继续接收任务时，再向 maximum size 增长。这适用于负载不均匀，并希望短时间突发可以使用额外线程但不长期保留这些线程的场景。

`FixedThreadPool` 启动并维持固定数量的 worker。它适合容量规划简单、worker 数量需要稳定，或调度可预测性比动态扩缩更重要的场景。

## 排队与拒绝

线程池可以使用无界队列或有界队列。有界队列能明确表达背压：当线程池无法接收任务时，提交会返回 `RejectedExecution::Saturated`，而不是静默增加内存使用。

`submit` 或 `submit_callable` 成功只表示线程池接受了任务。任务之后仍可能失败、panic 或被取消；最终结果需要通过返回的 `TaskHandle` 观察。

## 关闭行为

`shutdown` 会停止接受新任务，并允许已接受的任务完成。`shutdown_now` 会停止接受新任务，并取消仍在队列中或尚未开始的工作。已经运行在 OS 线程上的任务不会被强制杀死，而是由任务自身代码决定何时结束。

`await_termination` 返回一个 future，在已请求 shutdown 且所有已接受工作完成或取消后完成。

## 快速开始

### 动态线程池

```rust
use std::io;

use qubit_thread_pool::{ExecutorService, ThreadPool};

let pool = ThreadPool::builder()
    .core_pool_size(2)
    .maximum_pool_size(8)
    .queue_capacity(128)
    .thread_name_prefix("app-worker")
    .build()?;

let handle = pool.submit_callable(|| Ok::<usize, io::Error>(40 + 2))?;
assert_eq!(handle.get()?, 42);
pool.shutdown();
# Ok::<(), Box<dyn std::error::Error>>(())
```

### 固定大小线程池

```rust
use std::io;

use qubit_thread_pool::{ExecutorService, FixedThreadPool};

let pool = FixedThreadPool::builder()
    .pool_size(4)
    .queue_capacity(256)
    .build()?;

let handle = pool.submit_callable(|| Ok::<usize, io::Error>(6 * 7))?;
assert_eq!(handle.get()?, 42);
pool.shutdown();
# Ok::<(), Box<dyn std::error::Error>>(())
```

## 如何选择 Executor

当服务包含 blocking 工作、流量有突发性，并且需要 core/maximum worker 调优时，使用 `ThreadPool`。当目标 worker 数量稳定且不应动态增长时，使用 `FixedThreadPool`。

CPU 密集型、适合 divide-and-conquer 的工作，优先使用 `qubit-rayon-executor`。Tokio 应用中的 Tokio blocking 任务或 async IO future，优先使用 `qubit-tokio-executor`。应用层需要统一路由这些执行域时，使用 `qubit-execution-services`。

## Benchmark

本 crate 包含随线程池代码从原 concurrent 模块迁移出来的 Criterion benchmark。这些 benchmark 用代表性的提交模式，对比 Qubit 线程池实现与 `threadpool`、Rayon 等常见开源实现。

运行 benchmark：

```bash
cargo bench --bench thread_pool_bench
```

benchmark 输入与历史对比数据保存在 `test-data` 下。

## 测试

快速在本地跑一遍：

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

若要与持续集成（CI）保持一致，请在仓库根目录依次执行：`./align-ci.sh` 将本地工具链与配置对齐到 CI 规则，再执行 `./ci-check.sh` 复现流水线中的检查。需要查看或生成测试覆盖率时，使用 `./coverage.sh`。

## 参与贡献

欢迎通过 Issue 与 Pull Request 参与本仓库。建议：

- 报告缺陷、讨论设计或较大能力扩展时，可先开 Issue 对齐方向再投入实现。
- 单次 PR 尽量聚焦单一主题，便于代码审查与合并历史。
- 提交 PR 前请先运行 `./align-ci.sh`，再运行 `./ci-check.sh`，确保本地与 CI 使用同一套规则且能通过流水线等价检查。
- 若修改运行期行为，请补充或更新相应测试；若影响对外 API 或用户可见行为，请同步更新本文档或相关 rustdoc。
- 如果修改调度、排队或关闭行为，且该行为同时适用于 `ThreadPool` 和 `FixedThreadPool`，请覆盖两者的测试。

向本仓库贡献内容即表示您同意以 [Apache License, Version 2.0](LICENSE)（与本项目相同）授权您的贡献。

## 许可证与版权

版权所有 © 2026 Haixing Hu，Qubit Co. Ltd.。

本软件依据 [Apache License, Version 2.0](LICENSE) 授权；完整许可文本见仓库根目录的 `LICENSE` 文件。

## 作者与维护

**Haixing Hu** — Qubit Co. Ltd.

| | |
| --- | --- |
| **源码仓库** | [github.com/qubit-ltd/rs-thread-pool](https://github.com/qubit-ltd/rs-thread-pool) |
| **API 文档** | [docs.rs/qubit-thread-pool](https://docs.rs/qubit-thread-pool) |
| **Crate 发布** | [crates.io/crates/qubit-thread-pool](https://crates.io/crates/qubit-thread-pool) |
