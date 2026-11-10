# Qubit Thread Pool 性能说明

[English](performance.md) · 当前基准操作说明适用于 0.12.1；已有测量结果保留各自的原始源码基线。

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

## TP-006 争用测量

新增两个分组，用于核查多 worker 稀疏到达和令牌取消路径：

| 分组 | 单次计时操作 | 单次操作计时之外 |
| --- | --- | --- |
| `thread_pool_sparse_multiworker` | 在 1/4/8/16 个 worker 下提交一个无操作任务并等待完成信号。 | 每个样本前等待所有配置 worker 空闲；线程池创建、恢复空闲和关闭均不计入单次操作。 |
| `thread_pool_ticket_cancellation` | 每个延迟样本执行一次成功的 `PoolJobTicket::cancel_queued`；队列长度为 1/128/4,096，取消比例为 10%/50%/100%。 | 用受控阻塞任务固定住 worker，再准备排队令牌。通过最少数量的独立队列实现精确比例（例如队列长度 1、比例 10% 时使用 10 个队列）。队列准备、补交替代令牌、释放 worker、排空和关闭均不计入取消操作累计时间。 |
| `thread_pool_ticket_cancellation_contended` | 4 个提交线程并发提交 256 个带令牌的无操作任务，由 4 个 worker 消费；每个任务样本在准备/提交任务前开始计时，worker 开始执行时结束。 | Criterion case 前创建并复用线程池。批次吞吐量包含提交线程创建和协调开销。 |

令牌位置使用固定种子 `0x5eedcafe` 的确定性 Fisher–Yates 洗牌。每次 Criterion 迭代后，在取消计时之外补交替代令牌，使各队列保持指定长度。基准在清理前核验排队、取消和完成计数。单操作延迟使用独立样本采集，不从 Criterion 汇总统计推导；输出的 p50/p95/p99 使用最近秩法计算，并记录样本数以及 10,000 次 `Instant` 计时器开销校准。样本涵盖 Criterion 预热和测量调用，输出会明确标识；不会把 Criterion 汇总值冒充为单操作分位数。

三轮正式测量均使用 Criterion 样本数 20、预热 0.5 秒、目标测量时长 1 秒，并启用默认 feature（`default = []`）。下表 Criterion 时间和吞吐量采用每轮的中心估计；取消时间为整批操作耗时，吞吐量按成功取消的令牌数计算。延迟分位数来自独立采集的单操作样本。每个分位数列出三轮中位数和最小值–最大值；样本数为该 case 在 Criterion 预热及正式测量调用期间收集到的操作样本总数。

### 稀疏到达

| Worker 数 | Criterion 时间 R1 / R2 / R3（µs） | 吞吐量 R1 / R2 / R3（K 次/秒） | p50 ns 中位数 [范围] | p95 ns 中位数 [范围] | p99 ns 中位数 [范围] | 样本数 R1–R3 最小–最大 |
| ---: | --- | --- | --- | --- | --- | ---: |
| 1 | 21.116 / 14.836 / 13.080 | 47.356 / 67.404 / 76.450 | 15,055 [14,712–15,305] | 20,147 [19,692–24,380] | 28,125 [26,143–29,466] | 80,647–128,115 |
| 4 | 21.732 / 6.646 / 5.852 | 46.016 / 150.480 / 170.880 | 5,530 [5,018–7,832] | 12,388 [11,915–34,346] | 17,765 [15,550–76,045] | 49,143–193,215 |
| 8 | 9.622 / 7.404 / 8.216 | 103.930 / 135.060 / 121.710 | 7,160 [6,883–8,952] | 13,485 [12,065–14,681] | 17,021 [15,712–18,875] | 159,405–175,155 |
| 16 | 19.417 / 20.521 / 18.165 | 51.500 / 48.730 / 55.050 | 19,576 [19,025–19,747] | 26,483 [24,814–27,993] | 32,056 [29,467–35,170] | 70,357–80,017 |

### 令牌取消及并发提交/消费

每行列出队列长度和精确取消比例。“整批”表示“取消令牌数 / 所有副本队列中的令牌总数”；例如 4,096/10% 使用 5 个队列，在 20,480 个令牌中取消 2,048 个。Criterion 时间是整批耗时；操作分位数是单次成功取消耗时。

| 队列 / 比例 | Criterion 整批耗时 R1 / R2 / R3 | 吞吐量 R1 / R2 / R3 | p50 ns 中位数 [范围] | p95 ns 中位数 [范围] | p99 ns 中位数 [范围] | 样本数 R1–R3 最小–最大 |
| --- | --- | --- | --- | --- | --- | ---: |
| 1 / 10%（1/10） | 162.50 / 159.94 / 160.91 ns | 6.154 / 6.252 / 6.215 M 次/秒 | 160 [158–160] | 163 [163–218] | 212 [193–236] | 2,863,605–3,110,565 |
| 1 / 50%（1/2） | 160.52 / 180.46 / 163.03 ns | 6.230 / 5.541 / 6.134 M 次/秒 | 161 [160–161] | 164 [162–215] | 197 [194–235] | 3,101,535–4,199,881 |
| 1 / 100%（1/1） | 160.84 / 680.66 / 164.78 ns | 6.217 / 1.469 / 6.069 M 次/秒 | 163 [160–192] | 168 [163–230] | 206 [185–260] | 730,653–4,202,191 |
| 128 / 10%（64/640） | 15.184 / 22.007 / 15.276 µs | 4.215 / 2.908 / 4.190 M 次/秒 | 233 [232–329] | 284 [254–399] | 367 [362–560] | 2,231,232–2,836,032 |
| 128 / 50%（64/128） | 13.916 / 21.636 / 13.621 µs | 4.599 / 2.958 / 4.699 M 次/秒 | 214 [212–286] | 234 [228–386] | 317 [236–519] | 2,486,592–2,930,112 |
| 128 / 100%（128/128） | 22.286 / 29.087 / 21.978 µs | 5.744 / 4.401 / 5.824 M 次/秒 | 171 [170–222] | 183 [179–306] | 245 [196–439] | 2,526,848–3,064,448 |
| 4,096 / 10%（2,048/20,480） | 8.297 / 3.543 / 3.468 ms | 246.850 / 578.060 / 590.590 K 次/秒 | 1,691 [1,670–2,807] | 3,093 [1,783–3,538] | 3,600 [2,920–4,210] | 690,176–1,120,256 |
| 4,096 / 50%（2,048/4,096） | 5.248 / 2.293 / 2.278 ms | 390.270 / 893.100 / 899.080 K 次/秒 | 1,101 [1,095–2,029] | 1,285 [1,278–2,520] | 1,843 [1,418–3,023] | 559,104–1,382,400 |
| 4,096 / 100%（4,096/4,096） | 1,614.90 / 833.41 / 827.17 µs | 2.536 / 4.915 / 4.952 M 次/秒 | 201 [200–325] | 249 [246–633] | 311 [305–879] | 1,380,352–3,624,960 |
| 并发：4 个提交线程 / 4 个 worker / 256 个任务 | 399.84 / 545.52 / 407.98 µs | 640.250 / 469.280 / 627.480 K 任务/秒 | 27,198 [23,433–30,755] | 83,897 [60,396–84,210] | 112,771 [77,337–141,119] | 399,616–1,115,392 |

每个 case 和每轮各执行 10,000 次计时器开销校准；p50 开销为 22–50 ns，p95 为 24–241 ns。报告这些值但不从测量中扣除；对于约 160 ns 的单令牌取消测量，计时器开销占有可见比例。R1 中 Criterion 提示 4,096 队列的 10%、50%、100% 三个 case 无法在目标 1 秒内完成 20 个样本，估计需要 1.9 秒、1.7 秒和 1.3 秒；Criterion 仍完成 20 个样本并成功退出。这是测量时间提示，不是线程池缺陷证据。 R2 中队列长度 128、100% 取消的 case 也提示目标 1 秒不足，估计需要 1.2 秒；该轮仍完成 20 个样本。不同轮次也有波动：队列长度 128 和 contended case 在 R2 变慢，R3 回到接近 R1；4-worker 稀疏组在 R1 与 R2/R3 间也明显变化。4,096 队列的操作级延迟在 R2/R3 低于 R1，没有呈现随轮次或取消比例稳定恶化的趋势。

**结论：** 测量显示取消成本随队列长度变化，也受到机器负载影响，但没有确认稳定且严重的争用瓶颈。现有证据不足以支持结构优化，因此保留当前队列与 worker 结构。

#### 复现命令与环境

每轮执行下列两条命令，只改变 Criterion baseline 参数：

```bash
cargo bench --bench thread_pool_bench --locked -- thread_pool_sparse_multiworker --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --save-baseline tp006-round1
cargo bench --bench thread_pool_bench --locked -- thread_pool_ticket_cancellation --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --save-baseline tp006-round1
cargo bench --bench thread_pool_bench --locked -- thread_pool_sparse_multiworker --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --baseline tp006-round1
cargo bench --bench thread_pool_bench --locked -- thread_pool_ticket_cancellation --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --baseline tp006-round1
```

最后两条在 R3 再执行一次。R2 稀疏组曾有一次错误地同时使用两个互斥 baseline 参数，退出码为 2：

```bash
cargo bench --bench thread_pool_bench --locked -- thread_pool_sparse_multiworker --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --baseline tp006-round1 --save-baseline tp006-round2
```

修正后的 R2 命令和全部 R3 命令均以退出码 0 完成。系统未安装 Gnuplot，Criterion 使用 plotters 后端。测量机器为 Intel Xeon E5-2680 v4（28 个物理核心 / 56 个逻辑 CPU，两个 NUMA 节点）、Ubuntu 6.8.0-147-generic x86_64、Rust 1.94.0。源代码基线 commit 为 `f02d5fd9f685193c9f605e52964931de67561d26`。共享脏工作树包含 TP-001 至 TP-005 的源码、测试、Rustdoc、README、指南和设计文档改动，以及 TP-006 基准与这两份性能报告。测量只使用合成基准任务。目前无法找到这些历史原始日志，也无法从记录的基线 commit 重建当时的完整脏源码快照。因此保留旧表作为未经复现的历史观测，不将其称为可复现结果。


## 可复现测量：2026-11-09

从实现 commit `0a46d1802605d0f8812246870ba51d1c19e70082`（tree
`d15304c8cdf25d253bf70cfdd6540ffba375a1b9`）对 TP-006 两组进行了三轮
测量；测量期间源码未变。每轮样本数 20、预热 0.5 秒、目标测量 1 秒，
使用默认 feature。R1 保存 Criterion 基线 `rtp-20261109-round1`；R2、R3
均与该基线比较。这些是同一实现的重复性测量，不是改动前后对比。

[证据清单](https://github.com/qubit-ltd/rs-thread-pool/blob/d24a860a8fbc91f8a127f0bde633189c43c78000/doc/review-evidence-20261109/manifest.json)和[归档日志](https://github.com/qubit-ltd/rs-thread-pool/tree/d24a860a8fbc91f8a127f0bde633189c43c78000/doc/review-evidence-20261109/)
记录了原始输出、完整命令、退出结果、环境、lockfile 摘要及 SHA-256。
稀疏到达组 Criterion 中心估计范围为：1 worker 10.114–14.602 µs、4 个
5.997–6.730 µs、8 个 10.097–25.427 µs、16 个 20.348–28.122 µs。取消和
并发测量存在明显轮间波动，应查看各轮原始结果，不把单轮当作代表值。
系统未安装 Gnuplot，Criterion 使用 plotters 后端；机器与工具链信息见清单。

上方旧 TP-006 表格保留作历史参考，但其原始日志缺失，精确的历史脏源码
快照也无法恢复，因此不作为可复现证据。

## ticket 取消竞争与长队列核查：2026-11-10

新增 `thread_pool_ticket_cancel_race` 分组使用持久 actor、每批屏障、提交后立即
交接 ticket，并让 worker 与取消者自然竞争。32 个自然竞争用例覆盖 1/4 个
worker、1/4 个 producer、1/2 个 canceller 和 0/10/50/100% 目标比例；另有 12 个
worker 门控用例确保能够测到成功取消。0% arm 与取消 arm 使用同一 harness。
历史 `thread_pool_ticket_cancellation_contended` 仍只是 submit/consume 参考组，
不作为匹配对照。规模组在 16,384 和 65,536 队列深度测量队首、队中、队尾删除，
每次删除后补回 ticket 以保持实际深度。

三轮独占测量使用默认 feature、样本数 20、相同 commit/tree、Rust 1.94.0、
lockfile 和 Intel Xeon E5-2680 v4 主机。每轮命令为：

```bash
timeout 3600s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel
```

三轮完整输出、代码身份、时间戳、主机信息和摘要存档于
[`doc/review-evidence-20261110/benchmarks`](review-evidence-20261110/benchmarks/)，
逐配置数据和守恒汇总见
[`t2-measurement-summary.md`](review-evidence-20261110/benchmarks/t2-measurement-summary.md)。
三轮均退出 0 并输出全部 44 个 race 摘要。计数满足
`accepted = run + cancel_callback`、`cancel_true = cancel_callback`、
`cancel_attempts = cancel_true + cancel_false`；无界竞争组拒绝数为零。自然竞争
不会被强制为目标取消比例。

1 worker / 4 producers / 1 canceller 的匹配自然竞争结果：

| 轮次 | 取消目标 | 批次时间（µs） | accepted jobs 吞吐（K/s） | submit p95/p99（ns） | submit-to-run p95/p99（ns） |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0% | 564.58 | 453.43 | 5,302 / 10,363 | 26,880 / 185,423 |
| 1 | 50% | 393.61 | 650.39 | 10,943 / 18,005 | 107,375 / 124,664 |
| 1 | 100% | 397.05 | 644.75 | 8,453 / 16,318 | 126,740 / 170,143 |
| 2 | 0% | 587.17 | 435.99 | 4,980 / 8,726 | 24,069 / 38,290 |
| 2 | 50% | 347.29 | 737.14 | 8,886 / 15,424 | 99,729 / 115,283 |
| 2 | 100% | 360.07 | 710.97 | 7,637 / 15,520 | 114,343 / 143,255 |
| 3 | 0% | 677.37 | 377.93 | 5,637 / 11,304 | 29,871 / 47,992 |
| 3 | 50% | 387.09 | 661.35 | 10,270 / 17,180 | 112,675 / 130,002 |
| 3 | 100% | 389.25 | 657.68 | 8,005 / 15,552 | 116,030 / 129,662 |

50% 和 100% 取消的 submit-call p95/p99 在三轮中均高于同 harness 的 0% arm。队中
删除从 16,384 个 entry 时的 5.90–6.29 µs 上升到 65,536 个时的
27.11–28.68 µs；队首删除保持在约 0.19–0.23 µs。这触发了内部队列候选评估，
但不构成公共性能保证或业务 SLO 不达标的证据。取消比例较高时终态工作量也不同，
因此 accepted-job 批次吞吐不是相同工作量的比较。

TP006 分别记录计时器读取和原子记录开销，不从结果中扣除。TP008 run/cancel
校准只覆盖孤立的 callback、时间戳、状态、计数及 channel send 路径，不能作为
共享 channel 竞争的修正值。门控 setup/release 单独报告，不并入自然竞争结果。
索引 FIFO 候选与该基线分开测量。它将队中/队尾规模延迟降低约 95–99%，但使匹配
0% submit-call p95/p99 增加 81%/62%、submit-to-run p95/p99 增加 103%/83%；历史
submit/consume 对照也超过允许的 5% 退化线。因此已移除该候选，继续使用现有 FIFO。
完整数值和原始输出见结果汇总。

## 解读与复现

比较时应保持分组、工作负载、worker 数量、编译器和机器一致。每次记录结果时都应附上 commit、Rust 版本、硬件/OS 和命令。预热、后台负载和任务粒度都会影响排序。drop-request 不包含 worker 终止等待，不能当作完整生命周期成本；steady-state 也不代表线程池启动时间。当前 benchmark 不读取历史 `test-data` 数据集。

## 历史观测：2026-05-11

以下表格从 README 保留迁入，属于**历史观测，不是 0.10.0 的性能承诺，也不构成当前 0.12.1 版本的性能保证**。记录环境为 Apple M3 Max、16 个硬件线程、Rust 1.94.1，历史执行命令为
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
