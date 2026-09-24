# Qubit Thread Pool

[![Rust CI](https://github.com/qubit-ltd/rs-thread-pool/actions/workflows/ci.yml/badge.svg)](https://github.com/qubit-ltd/rs-thread-pool/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https://qubit-ltd.github.io/rs-thread-pool/coverage-badge.json)](https://qubit-ltd.github.io/rs-thread-pool/coverage/)
[![Crates.io](https://img.shields.io/crates/v/qubit-thread-pool.svg?color=blue)](https://crates.io/crates/qubit-thread-pool)
[![Rust](https://img.shields.io/badge/rust-1.94+-blue.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![English Document](https://img.shields.io/badge/Document-English-blue.svg)](README.md)

Qubit Thread Pool 用 OS 线程执行同步 Rust 任务，支持有界接纳、结果观察和明确的关闭流程。服务可以把阻塞工作移出请求线程，无需为此引入异步运行时。

## 安装

```toml
[dependencies]
qubit-thread-pool = "0.11"
qubit-executor = "0.8"
```

需要 Rust 1.94 或更高版本。导入 `ExecutorService` trait 时，必须直接声明 `qubit-executor` 依赖；普通线程池用法不依赖 Tokio 或 Rayon。

## 快速开始

处理突发阻塞任务的服务可以保留两个 core worker，在队列承压时增长至八个，并拒绝超额提交。下面的最小示例提交一个能返回结果的任务，观察结果后等待线程池平滑终止。

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io;

    use qubit_executor::service::ExecutorService;
    use qubit_thread_pool::ThreadPool;

    let pool = ThreadPool::builder()
        .core_pool_size(2)
        .maximum_pool_size(8)
        .queue_capacity(128)
        .thread_name_prefix("app-worker")
        .build()?;

    let handle = pool.submit_callable(|| Ok::<usize, io::Error>(40 + 2))?;
    assert_eq!(handle.get()?, 42);
    pool.shutdown();
    pool.wait_termination();
    Ok(())
}
```

容量长期稳定时，可使用
`FixedThreadPool::builder().pool_size(4).queue_capacity(128).build()?`，
在创建时预启动 worker。`FixedThreadPool::default()` 按可用并行度选择 worker 数量，并使用无界队列；若需要处理创建 worker 的错误，应使用 builder，避免默认构造失败时 panic。

## 选择线程池

| 线程池 | 适用场景 | worker 策略 |
| --- | --- | --- |
| `ThreadPool` | 阻塞工作量变化明显，需要为突发流量预留扩容空间。 | 按需增长到 core size，随后排队；有界队列承压时再增长到 maximum size。 |
| `FixedThreadPool` | worker 数量需要保持稳定。 | 创建时启动配置数量的 worker，运行期间不调整规模。 |

两者都支持线程名称/栈配置、worker/task hook、`ThreadPoolStats`、callable 结果和 tracked 任务。动态池还支持预启动、keep-alive 和可选的 core 超时。两个池都不调度异步 future，不实现 CPU 分治调度，也不保证严格的任务启动或完成顺序。

## 排队与背压

有界队列通过 `SubmissionError::Saturated` 明确反馈过载，应用可以限流、按自身策略重试或调整容量。无界队列可能持续占用内存；仅增加 maximum size 不会使动态池突破 core size 扩容。队列容量不是未完成任务总数的上限：运行中的任务不占队列槽位，动态扩容还可以额外预留任务槽位。

不需要结果时使用 `submit`；需要结果时使用 `submit_callable`；还需要状态与执行前取消时使用 tracked 提交。底层 `ThreadPool::submit_job` 返回 `PoolJobSubmissionError`，其中包含 `AcceptancePanicked`；接纳失败不会调用 run 或 cancel。回调的使用限制见用户手册。
下游任务注册表还可以通过 `prepare_cancellable_job` 获取 ticket，在任务仍处于排队状态时将其移出队列。取消时序和回调限制见用户手册。

## shutdown 与 stop

`shutdown()` 关闭接纳入口后返回，不等待提交或已接纳任务结束；随后调用 `wait_termination()` 才能完成平滑关闭。`stop()` 会等待正在接纳的提交，并取消尚未跨过 worker 领取边界的任务，无法强制终止运行中的工作，也不是终止屏障。`join()` 等待任务处理完毕，但不关闭接纳入口。这些等待包含取消回调，不能从同一个池的任务中调用。统计信息和 `StopReport` 用于监控，不是同步屏障。

## 延伸阅读

[中文版用户手册](doc/user_guide.zh_CN.md)和[英文用户手册](doc/user_guide.md)详细说明回调、队列策略、生命周期及诊断方法。另可参阅[当前设计](doc/design.zh_CN.md)、[性能与历史测量](doc/performance.zh_CN.md)、[0.9 到 0.10 迁移指南](doc/migration_0.9_to_0.10.zh_CN.md)以及 [API 文档](https://docs.rs/qubit-thread-pool)。

## 测试

```bash
# 使用默认 feature 集运行测试
cargo test

# 使用项目声明的全部 feature 运行测试
cargo test --all-features

# 运行项目 CI 检查
./ci-check.sh

# 检查代码覆盖率
./coverage.sh
```

## 许可证

Copyright (c) 2025 - 2026. Haixing Hu. All rights reserved.

本项目基于 Apache License 2.0 授权。完整许可证文本请参阅
[LICENSE](LICENSE)。

## 贡献

欢迎贡献。请遵循 Rust API 指南，及时更新公共 API 文档与测试，并在提交
Pull Request 前运行 `./align-ci.sh`格式化代码，运行`./ci-check.sh`对齐CI要求。

## 作者

**Haixing Hu** - *Qubit Co. Ltd.*

仓库地址：[https://github.com/qubit-ltd/rs-thread-pool](https://github.com/qubit-ltd/rs-thread-pool)
