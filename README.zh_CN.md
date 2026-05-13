# Qubit Thread Pool

[![Rust CI](https://github.com/qubit-ltd/rs-thread-pool/actions/workflows/ci.yml/badge.svg)](https://github.com/qubit-ltd/rs-thread-pool/actions/workflows/ci.yml)
[![Coverage Status](https://coveralls.io/repos/github/qubit-ltd/rs-thread-pool/badge.svg?branch=main)](https://coveralls.io/github/qubit-ltd/rs-thread-pool?branch=main)
[![Crates.io](https://img.shields.io/crates/v/qubit-thread-pool.svg?color=blue)](https://crates.io/crates/qubit-thread-pool)
[![Rust](https://img.shields.io/badge/rust-1.94+-blue.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![English Documentation](https://img.shields.io/badge/docs-English-blue.svg)](README.md)

面向 Rust 的线程池 executor service。

## 概览

Qubit Thread Pool 为同步工作提供基于 OS 线程的 `ExecutorService` 实现。它包含适合突发负载的动态 `ThreadPool`、适合稳定 worker 数量的 `FixedThreadPool`，以及用于可取消 deadline 回调的 `DelayedTaskScheduler`。

本 crate 基于 `qubit-executor` 构建，因此与其它 Qubit executor 实现共享任务接受、关闭、取消和 `TaskHandle` 语义。普通使用不需要依赖 Tokio 或 Rayon。

## 功能

- 提供动态 `ThreadPool`，支持分离的 core worker 与 maximum worker 限制。
- 提供固定大小 `FixedThreadPool`，用于可预测的 worker 数量。
- `FixedThreadPool` 实现 `Default`：通过 `FixedThreadPoolBuilder::default()` 的默认配置构建（worker 数量取可用并行度、无界队列、默认线程名前缀）。若 worker 线程创建失败则会 panic；需要 `Result` 时请使用 `FixedThreadPoolBuilder::default().build()`。
- 提供单线程 `DelayedTaskScheduler`，用于可取消的延迟回调。
- 支持有界或无界队列配置。
- 动态池支持懒创建 worker，也支持预启动 core worker。
- 动态池支持 keep-alive 与可选 core 线程超时。
- 支持配置 worker 线程名前缀和栈大小。
- 支持 worker 与 task 生命周期 hook，用于轻量级观测。
- 提供 `ThreadPoolStats`，用于观察线程池配置和运行时计数。
- 共享 `ExecutorService` 生命周期方法，包括 `shutdown`、`stop` 和 `wait_termination`。
- 提供 Criterion benchmark 与测试数据，用于对比 Qubit 线程池、`threadpool` 和 Rayon。

## 线程池模型

`ThreadPool` 采用常见的 core-size / maximum-size 执行器模型。它会懒创建 worker 直到 core size，之后优先排队；当有界队列无法继续接收任务时，再向 maximum size 增长。这适用于负载不均匀，并希望短时间突发可以使用额外线程但不长期保留这些线程的场景。

`FixedThreadPool` 启动并维持固定数量的 worker。它适合容量规划简单、worker 数量需要稳定，或调度可预测性比动态扩缩更重要的场景。

`FixedThreadPool::default()` 与 `FixedThreadPoolBuilder::default().build()` 等价，但构建失败会转为 panic；若需要处理错误，请使用 builder 的 `build()` 并处理 `ExecutorServiceBuilderError`。

内部实现上，动态池把已接受但尚未开始的任务放入由 monitor 保护的全局 FIFO 队列，并注册 worker-owned stealer 用于 worker 生命周期管理和本地队列清理。FIFO 描述的是全局等待队列，不表示任务启动或完成顺序具有严格 FIFO 保证。固定池使用 lock-free 全局 injector，并只唤醒有需要的 idle worker，使 fire-and-forget submit 路径更短且更可预测。

`ThreadPool` 的运行时尺寸调整接口主要面向显式控制面操作，例如运维限流、临时扩容或故障处置。普通业务代码应优先在构造时确定线程池大小。运行时调整 core size 会影响后续提交和预启动行为，但不会主动为已经排队的任务创建 worker。

## 排队与拒绝

线程池可以使用无界队列或有界队列。有界队列能明确表达背压：当线程池无法接收任务时，提交会返回 `SubmissionError::Saturated`，而不是静默增加内存使用。

`submit` 成功只表示线程池接受了一个 fire-and-forget runnable。需要最终结果时使用 `submit_callable` 获取 `TaskHandle`；还需要状态和启动前取消时，使用 `submit_tracked` 或 `submit_tracked_callable`。

## 生命周期 Hook

`ThreadPoolBuilder` 与 `FixedThreadPoolBuilder` 都支持可选的 worker / task 观测 hook：

- `before_worker_start`
- `after_worker_stop`
- `before_task`
- `after_task`

每个 hook 都会收到稳定的 worker index，并在 worker 线程上执行。hook 发生 panic 时会被捕获并忽略，因此观测代码不会杀死 worker，也不会破坏 executor 计数。hook 位于执行热路径上，应该保持短小。

```rust
use qubit_thread_pool::{ExecutorService, FixedThreadPool};

let pool = FixedThreadPool::builder()
    .pool_size(4)
    .before_task(|worker| {
        std::hint::black_box(worker);
    })
    .after_task(|worker| {
        std::hint::black_box(worker);
    })
    .build()?;

pool.submit(|| Ok::<(), std::io::Error>(()))?;
pool.shutdown();
# Ok::<(), Box<dyn std::error::Error>>(())
```

## 关闭行为

`shutdown` 会停止接受新任务，并允许已接受的任务完成。`stop` 会停止接受新任务，并取消仍在队列中或尚未开始的工作。已经运行在 OS 线程上的任务不会被强制杀死，而是由任务自身代码决定何时结束。

`wait_termination` 会阻塞当前线程，直到已请求 shutdown 且所有已接受工作完成或取消。

`DelayedTaskScheduler` 使用相同的生命周期语义：`shutdown` 拒绝新的延迟回调，并让已接受回调在 deadline 到达后执行；`stop` 会取消尚未开始的回调。

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

若使用与 `FixedThreadPoolBuilder::default()` 相同的默认配置，也可写 `let pool = FixedThreadPool::default();`，等价于 `FixedThreadPoolBuilder::default().build()`，但若 worker 线程无法创建则会 panic。

### 延迟任务调度器

```rust
use std::sync::mpsc;
use std::time::Duration;

use qubit_thread_pool::DelayedTaskScheduler;

let scheduler = DelayedTaskScheduler::new("app-delay")?;
let (tx, rx) = mpsc::channel();

let handle = scheduler.schedule(Duration::from_millis(25), move || {
    tx.send("ready").expect("receiver should be alive");
})?;

assert_eq!(rx.recv()?, "ready");
assert!(!handle.cancel());
scheduler.shutdown();
# Ok::<(), Box<dyn std::error::Error>>(())
```

## 如何选择 Executor

当服务包含 blocking 工作、流量有突发性，并且需要 core/maximum worker 调优时，使用 `ThreadPool`。当目标 worker 数量稳定且不应动态增长时，使用 `FixedThreadPool`。当你需要大量延迟回调且不希望为每个延迟任务分配一个正在 sleep 的 OS 线程时，使用 `DelayedTaskScheduler`；调度器回调应保持轻量，较重工作应再提交到线程池。

CPU 密集型、适合 divide-and-conquer 的工作，优先使用 `qubit-rayon-executor`。Tokio 应用中的 Tokio blocking 任务或 async IO future，优先使用 `qubit-tokio-executor`。应用层需要统一路由这些执行域时，使用 `qubit-execution-services`。

## Benchmark

本 crate 包含随线程池代码从原 concurrent 模块迁移出来的 Criterion benchmark。这些 benchmark 用代表性的提交模式，对比 Qubit 线程池实现与 `threadpool`、Rayon 等常见开源实现。

运行 benchmark：

```bash
cargo bench --bench thread_pool_bench
```

提交模式 benchmark 会对比 `ThreadPool.submit`、`ThreadPool.submit_tracked`、`FixedThreadPool.submit`、`FixedThreadPool.submit_tracked`、外部 `threadpool` crate 和 Rayon，并覆盖 `cpu_light`、`cpu_medium`、`cpu_heavy` 三类任务。CPU 任务耗时使用确定性的钟形分布生成，避免所有任务几乎同时完成，从而更容易体现调度与 stealing 行为差异。

benchmark 输入与历史对比数据保存在 `test-data` 下。

### 最新本地运行结果

最新一次本地运行在 2026-05-11 执行
`cargo bench --bench thread_pool_bench -- thread_pool_submit_modes`，环境为
Apple M3 Max、16 个硬件线程、Rust 1.94.1。下表为
`thread_pool_submit_modes` 的 Criterion mean wall-clock time，数值越低越好。
每个 case 提交 2,000 个任务。`submit_tracked` case 使用与 `submit` 相同的
channel completion 等待方式，避免把 handle wait 成本混入提交模式对比。

#### `cpu_light`

| Workers | `ThreadPool.submit` | `ThreadPool.submit_tracked` | `FixedThreadPool.submit` | `FixedThreadPool.submit_tracked` | `threadpool.execute` | Rayon |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0.444 ms | 0.546 ms | 0.388 ms | 0.439 ms | 0.386 ms | 0.144 ms |
| 4 | 0.726 ms | 1.285 ms | 0.560 ms | 0.981 ms | 0.740 ms | 0.082 ms |
| 8 | 1.758 ms | 4.561 ms | 0.967 ms | 1.402 ms | 1.065 ms | 0.142 ms |

#### `cpu_medium`

| Workers | `ThreadPool.submit` | `ThreadPool.submit_tracked` | `FixedThreadPool.submit` | `FixedThreadPool.submit_tracked` | `threadpool.execute` | Rayon |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2.031 ms | 2.133 ms | 2.029 ms | 2.115 ms | 2.037 ms | 1.439 ms |
| 4 | 1.354 ms | 1.057 ms | 1.321 ms | 1.296 ms | 1.455 ms | 0.425 ms |
| 8 | 1.902 ms | 3.868 ms | 0.959 ms | 1.280 ms | 2.022 ms | 0.391 ms |

#### `cpu_heavy`

| Workers | `ThreadPool.submit` | `ThreadPool.submit_tracked` | `FixedThreadPool.submit` | `FixedThreadPool.submit_tracked` | `threadpool.execute` | Rayon |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 14.256 ms | 14.360 ms | 14.251 ms | 14.198 ms | 14.157 ms | 11.078 ms |
| 4 | 4.384 ms | 4.588 ms | 4.715 ms | 4.533 ms | 4.594 ms | 3.311 ms |
| 8 | 3.505 ms | 3.502 ms | 3.391 ms | 3.993 ms | 4.335 ms | 2.965 ms |

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

Copyright (c) 2026. Haixing Hu.

本软件依据 [Apache License, Version 2.0](LICENSE) 授权；完整许可文本见仓库根目录的 `LICENSE` 文件。

## 作者与维护

**Haixing Hu** — Qubit Co. Ltd.

| | |
| --- | --- |
| **源码仓库** | [github.com/qubit-ltd/rs-thread-pool](https://github.com/qubit-ltd/rs-thread-pool) |
| **API 文档** | [docs.rs/qubit-thread-pool](https://docs.rs/qubit-thread-pool) |
| **Crate 发布** | [crates.io/crates/qubit-thread-pool](https://crates.io/crates/qubit-thread-pool) |
