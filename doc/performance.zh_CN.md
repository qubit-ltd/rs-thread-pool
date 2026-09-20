# Qubit Thread Pool 性能说明

[English](performance.md) · 本文说明 0.10.0 版本的测量边界。

## 当前 benchmark 分组

实际定义以 `benches/thread_pool_bench.rs` 为准。运行全部分组使用
`cargo bench --bench thread_pool_bench --locked`，也可以按名称筛选：

```bash
cargo bench --bench thread_pool_bench --locked -- thread_pool_steady_state
cargo bench --bench thread_pool_bench --locked -- thread_pool_end_to_end
cargo bench --bench thread_pool_bench --locked -- thread_pool_drop_request
```

| 分组 | 计时内容 | 计时外工作与解读边界 |
| --- | --- | --- |
| `thread_pool_steady_state` | 提交一批任务，并等待结果或完成信号。 | 在迭代外创建线程池；动态池预启动 worker；关闭不计时。 |
| `thread_pool_end_to_end` | Qubit 动态池/固定池的创建、提交、完成、shutdown 和 `wait_termination`。 | 包含 Qubit 生命周期终止等待；动态池按需启动 worker。 |
| `thread_pool_drop_request` | 外部 `threadpool` / Rayon 的创建、任务完成及池析构请求。 | 无法观察 worker 终止，不等同于 Qubit 的端到端计时。 |
| `thread_pool_granularity` | 不同任务粒度下动态池的端到端批处理。 | 名义总工作量为 2,048,000 次迭代，每个任务的成本按确定性分布变化。 |
| `thread_pool_idle_wakeup` | 向一个已预启动且空闲的 worker 提交无操作任务，直到收到完成信号。 | 不计入创建过程和 worker 恢复空闲的等待。 |

前三组每个 case 使用 2,000 个任务、256 次基础内部迭代，以及 1/4/8 个 worker。任务成本采用确定性的钟形分布，并使用 `black_box`。稳态组中，Qubit 通过 callable 句柄收集结果，外部 `threadpool` 使用 channel，Rayon 使用并行迭代与归约；它们比较的是应用执行路径，不是纯队列操作成本。Criterion 默认样本数为 20。粒度组使用 32/256/2,048 次基础迭代，不把时间阈值当作正确性测试。

## 解读与复现

比较时应保持分组、工作负载、worker 数量、编译器和机器一致。每次记录结果时都应附上 commit、Rust 版本、硬件/OS 和命令。预热、后台负载和任务粒度都会影响排序。drop-request 不包含 worker 终止等待，不能当作完整生命周期成本；steady-state 也不代表线程池启动时间。当前 benchmark 不读取历史 `test-data` 数据集。

## 历史观测：2026-05-11

以下表格从 README 保留迁入，属于**历史观测，不是 0.10.0 的性能承诺**。记录环境为 Apple M3 Max、16 个硬件线程、Rust 1.94.1，历史执行命令为
`cargo bench --bench thread_pool_bench -- thread_pool_submit_modes`。
当前 benchmark 已不再注册这个分组。

数值为 Criterion 平均墙钟时间，越低越好，每个 case 提交 2,000 个任务。当时 `submit_tracked` 与 `submit` 使用相同的 channel 完成等待方式，避免混入句柄等待差异。这些数据不是当前稳态组或端到端组的新测量结果。原 README 未记录精确 commit 和 OS 版本，因此不能承诺精确复现历史数值。

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


## 相关文档

参阅[当前设计](design.zh_CN.md)、[用户手册](user_guide.zh_CN.md)和
[README](../README.zh_CN.md)。带日期的实验报告继续保存在仓库中，作为历史快照；这些报告和 `test-data` 均不进入 crate 发布包。
