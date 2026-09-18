# Qubit Thread Pool 用户手册

[English](user_guide.md) · 适用于 `qubit-thread-pool` 0.10.0 及 Rust 1.94 或更高版本。

## 手册目标与读者

本手册面向需要把阻塞式或其他同步工作移出提交线程的 Rust 服务开发者。它说明当任务接纳、结果等待和服务关闭都会影响线上行为时，如何选择和使用 Qubit 线程池。本 crate 不替代 Rayon 的 CPU 密集型分治计算，也不负责调度异步 future。

## 概念模型

`ThreadPool` 是可伸缩线程池：它按需创建 worker，先增长到 core size，再排队；只有有界队列满时，才可能继续增长到 maximum size。`FixedThreadPool` 在创建时启动固定数量的 worker，运行期间不调整规模。两者都实现 `ExecutorService`；任务被接纳与任务最终成功是两个不同阶段。

| 概念 | 含义 |
| --- | --- |
| core size | 动态线程池的常规 worker 目标数量。 |
| maximum size | 有界队列承压时动态线程池允许的 worker 上限。 |
| queue capacity | 等待队列中的任务数，不含已由 worker 取走的任务。 |
| `TaskHandle` | callable 提交后得到的句柄；通过 `get()` 观察任务完成结果。 |

## 贯穿场景：处理突发的阻塞式文档转换

设想一个面向 HTTP 的文档转换服务。转换代码是阻塞式的：平时只需两个 worker，流量突发时最多可用八个，同时必须拒绝超额工作，不能无限积压内存。成功标准是：已接纳的转换能返回结果，过载能明确反馈给调用方。

## 安装与最小配置

在项目中加入与当前依赖策略相匹配的 crate 版本：

```toml
[dependencies]
qubit-thread-pool = "0.10"
```

示例中需要导入 `ExecutorService`，因为提交和生命周期方法由该 trait 提供。

## 核心工作流

为突发流量创建有界队列的动态线程池，提交 callable 并等待结果。下面的可观察结果为 `42`。

```rust
use std::io;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::ThreadPool;

let pool = ThreadPool::builder()
    .core_pool_size(2)
    .maximum_pool_size(8)
    .queue_capacity(128)
    .thread_name_prefix("convert-worker")
    .build()?;

let handle = pool.submit_callable(|| Ok::<usize, io::Error>(40 + 2))?;
assert_eq!(handle.get()?, 42);

pool.shutdown();
pool.wait_termination();
# Ok::<(), Box<dyn std::error::Error>>(())
```

不关心返回值的 runnable 使用 `submit`；需要观察返回值或任务错误时使用 `submit_callable`。`join()` 只等待已接纳任务处理完毕，不会发起关闭请求，因此之后仍可继续提交。

## 进阶用法

worker 数量长期稳定时选择 `FixedThreadPool`；它会预启动配置数量的 worker。若动态线程池更看重启动延迟，可在 builder 中配置 `prestart_core_threads()`，或对已创建的池调用 `prestart_all_core_threads()`。两个 builder 都支持 worker 和 task hook：hook 在 worker 线程中运行，收到稳定的 worker index；其自身 panic 会被忽略。task hook 位于执行路径上，应保持短小。

动态线程池使用无界队列时，达到 core size 后仍会排队；仅把 maximum size 调大不会带来突发扩容。只有在可以接受队列内存持续增长时，才应选择无界队列。

## 错误与诊断

底层自定义任务返回 `PoolJobSubmissionError`；`AcceptancePanicked` 表示接纳
回调发生 panic，任务尚未发布。标准执行器服务方法仍返回通用的
`SubmissionError`。

builder 的非法配置会返回 `ExecutorServiceBuilderError`，例如 queue capacity 为零，或 core size 大于 maximum size。提交时，有界队列满会得到 `SubmissionError::Saturated`；接纳入口关闭后会得到 `SubmissionError::Shutdown`。callable 被接纳后，其自身执行错误仍会通过 `TaskHandle::get()` 返回。

排查压力时可查看 `queued_count()`、`running_count()`、`live_worker_count()` 或 `stats()`。这些值用于观测，不代表严格的任务顺序；两种线程池都不保证任务启动或完成顺序严格一致。

## 排障

- **动态线程池始终不超过 core size。** 检查队列是否为无界队列；需要在压力下扩容时配置 `queue_capacity(...)`。
- **提交被拒绝。** 先区分 `Saturated` 与 `Shutdown`：前者意味着应降低需求或调整有界容量，后者说明线程池已停止接纳。
- **服务退出前任务未完成。** 在有序关闭阶段调用 `shutdown()`，随后调用 `wait_termination()`。

## 限制与最佳实践

`stop()` 会取消尚未开始的排队工作，但无法强制终止已经在 OS 线程上运行的任务；需要平滑收尾时应使用 `shutdown()`。根据排队任务的内存占用与延迟目标选择队列容量。普通业务场景宜在构造时确定线程池规模；运行时调整尺寸主要适合明确的控制面操作。

## 延伸阅读

参阅 [README](../README.zh_CN.md)、[English user guide](user_guide.md) 与 [API 文档](https://docs.rs/qubit-thread-pool)。
