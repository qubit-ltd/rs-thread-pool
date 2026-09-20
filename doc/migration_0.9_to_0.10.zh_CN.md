# 从 0.9 迁移到 0.10

## 工具链与依赖

0.10 要求 Rust 1.94，并使用 `qubit-executor 0.8`、`qubit-function 0.18`、
`qubit-lock 0.14` 这一依赖版本线。代码直接导入 executor trait 或类型时，
请显式声明：

```toml
qubit-thread-pool = "0.10"
qubit-executor = "0.8"
```

## API 变化

- executor 类型请使用 `qubit_executor::{...}` 路径；本 crate 不再提供已移除
  的便捷 re-export。
- `ThreadPool::submit_job` 返回 `PoolJobSubmissionError`。接纳失败匹配
  `Rejected(SubmissionError)`，接纳回调 panic 匹配 `AcceptancePanicked`。
- 动态线程池会先创建直接执行任务所需的 worker，再执行接纳回调；接纳回调在
  线程池监视器锁之外运行。因此 worker 创建失败时不会调用接纳、执行或取消回调。
- `stop()` 的取消边界现在是 worker 领取任务的时刻。直接派发任务可能在并发
  stop 等待期间完成接纳，随后在 run 回调前被取消。`StopReport` 应视为时间点
快照；不要把 `submit_job` 返回 `Ok(())` 当作 run 回调已经开始的证明。
- 接纳回调会在提交路径上同步执行。可以调用同一个线程池的 `shutdown`，它会关闭接纳
  并立即返回。不能同步调用 `stop`、`join` 或 `wait_termination`，否则等待可能与
  当前提交互相阻塞。

普通 `ExecutorService` 提交方法仍使用原有的 `SubmissionError` 契约。检查在
worker 任务中调用 `join()` 或 `wait_termination()` 的代码，避免任务等待自身
所在的线程池变为空闲。
