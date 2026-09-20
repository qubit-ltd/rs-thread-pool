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
| queue capacity | 普通排队槽位的上限，不含运行任务；动态扩容还可额外接纳一个槽位。 |
| `TaskHandle` | callable 提交后得到的句柄；通过 `get()` 观察任务完成结果。 |

## 贯穿场景：处理突发的阻塞式文档转换

设想一个面向 HTTP 的文档转换服务。转换代码是阻塞式的：平时只需两个 worker，流量突发时最多可用八个，同时必须拒绝超额工作，不能无限积压内存。成功标准是：已接纳的转换能返回结果，过载能明确反馈给调用方。

## 安装与最小配置

在项目中加入与当前依赖策略相匹配的 crate 版本：

```toml
[dependencies]
qubit-thread-pool = "0.10"
qubit-executor = "0.8"
```

示例中需要导入 `ExecutorService`，因为提交和生命周期方法由该 trait 提供。

## 核心工作流

为突发流量创建有界队列的动态线程池，提交 callable 并等待结果。下面的可观察结果为 `42`。

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
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
Ok(())
}
```

不关心返回值的 runnable 使用 `submit`；需要观察返回值或任务错误时使用 `submit_callable`。`join()` 只等待已接纳任务处理完毕，不会发起关闭请求，因此之后仍可继续提交。`shutdown()` 关闭接纳入口、切换生命周期并唤醒 worker 后返回，不等待提交或已接纳任务；需要完成屏障时调用 `wait_termination()`。`stop()` 会等待正在接纳的提交，并取消尚未跨过 worker 领取边界的工作。所有已接纳任务都通过全局队列发布，任务可能在 stop 等待期间完成接纳，随后被取消。已经领取为运行状态的任务不会被中断，即使 run 回调尚未开始。`StopReport` 的数量是时间点快照，不是同步屏障。对 `submit_job` 来说，`Ok(())` 只表示接纳成功，不表示任务已开始或执行成功。
接纳入口及必要的 worker 创建成功后，提交线程会在状态 monitor 外同步执行接纳回调，应保持短小。可以在回调中调用同一个线程池的
`shutdown`，因为它只关闭接纳并立即返回，不等待当前提交。不能同步调用 `stop`、
`join` 或 `wait_termination`，因为这些等待可能与当前提交互相阻塞。读取 `stats` 等
非阻塞观测是安全的。
不能在池内任务中等待同一个池的 `join()` 或 `wait_termination()`，因为等待范围包含任务自身。取消回调也不能进行这些等待。取消回调可能在 stop 调用线程执行，也可能与 worker 侧取消并发。空闲和终止等待都会等待取消回调结束；stop 之后若还需要等待运行中的任务完成，应在池外调用 `wait_termination()`。

## 进阶用法

worker 数量长期稳定时选择 `FixedThreadPool`；它会预启动配置数量的 worker。若动态线程池更看重启动延迟，可在 builder 中配置 `prestart_core_threads()`，或对已创建的池调用 `prestart_all_core_threads()`。两个 builder 都支持 worker 和 task hook：hook 在 worker 线程中运行，收到稳定的 worker index；其自身 panic 会被忽略。task hook 位于执行路径上，应保持短小。

动态线程池使用无界队列时，达到 core size 后仍会排队；仅把 maximum size 调大不会带来突发扩容。只有在可以接受队列内存持续增长时，才应选择无界队列。

文档转换服务应在请求入口处理 `Saturated`：限制并发、返回过载响应，或在池外按有界退避策略重试。不要忙等重试，也不要让所有 worker 都阻塞在向同一个满队列提交依赖任务的操作上。槽位还覆盖接纳中和取消中的工作，因此监控中的队列数量低于容量，并不保证下次提交成功。

需要状态和执行前取消时，使用 `submit_tracked` 或 `submit_tracked_callable`。worker hook（`before_worker_start`、`after_worker_stop`、`before_task`、`after_task`）用于观测，不宜承担阻塞协调。线程名称与栈配置作用于新创建的线程；处理任务结果时应与监控计数分开。

## 错误与诊断

底层自定义任务返回 `PoolJobSubmissionError`；`AcceptancePanicked` 表示接纳
回调发生 panic，任务尚未发布，不会调用 run 或 cancel。接纳前拒绝任务时，accept、run、cancel 都不调用。底层 `PoolJobSubmissionError::Rejected` 包装 `SubmissionError`，标准执行器服务方法直接返回通用的 `SubmissionError`。接纳回调若在 panic 前修改了外部状态，集成方需要自行恢复。

builder 的非法配置会返回 `ExecutorServiceBuilderError`，例如 queue capacity 为零，或 core size 大于 maximum size。提交时，有界队列满会得到 `SubmissionError::Saturated`；接纳入口关闭后会得到 `SubmissionError::Shutdown`。动态池在需要创建 worker 时还可能返回 `SubmissionError::WorkerSpawnFailed`，重试前应检查其 source 和 OS 资源限制。callable 被接纳后，其自身执行错误仍会通过 `TaskHandle::get()` 返回。已捕获的展开式 panic 不会杀死 worker，线程池完成计数也不代表业务成功。

排查压力时可查看 `queued_count()`、`running_count()`、`live_worker_count()` 或 `stats()`。这些值由独立计数拼成，只是尽力观测，不是原子快照或完成屏障。两种线程池都不保证严格的任务启动或完成顺序。静止时 submitted 等于 completed 加线程池自身的 cancelled；不要在并发快照上断言该等式。

## 排障

升级时请先阅读[0.9 到 0.10 迁移指南](migration_0.9_to_0.10.zh_CN.md)。

- **动态线程池始终不超过 core size。** 检查队列是否为无界队列；需要在压力下扩容时配置 `queue_capacity(...)`。
- **提交被拒绝。** 先区分 `Saturated` 与 `Shutdown`：前者意味着应降低需求或调整有界容量，后者说明线程池已停止接纳。
- **服务退出前任务未完成。** 在有序关闭阶段调用 `shutdown()`，随后调用 `wait_termination()`。
- **关闭等待一直不返回。** 检查阻塞任务和取消回调，确认没有回调或池内任务在等待当前提交或同一个池。
- **拒绝提交后没有回调执行。** 这是预期行为；只有接纳及必要的 worker 创建成功，才开始调用接纳回调。

## 限制与最佳实践

`stop()` 会取消尚未开始的排队工作，但无法强制终止已经在 OS 线程上运行的任务；需要平滑收尾时应使用 `shutdown()`。根据排队任务的内存占用与延迟目标选择队列容量。普通业务场景宜在构造时确定线程池规模；运行时调整尺寸主要适合明确的控制面操作。

## 延伸阅读

参阅 [README](../README.zh_CN.md)、[English user guide](user_guide.md) 与 [API 文档](https://docs.rs/qubit-thread-pool)。[当前设计](design.zh_CN.md)解释接纳与计数机制，[性能说明](performance.zh_CN.md)定义 benchmark 的测量边界。
