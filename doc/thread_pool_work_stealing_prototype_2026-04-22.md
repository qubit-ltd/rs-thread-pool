# ThreadPool Work-Stealing Prototype Report (2026-04-22)

## 目标

在不破坏现有 API 和测试语义的前提下，引入“worker 本地队列 + steal”能力，并通过迭代将吞吐回归到重构前基线附近。

## 关键实现策略（当前版本）

1. 保留全局队列作为默认与 fallback 路径。
2. 引入 worker 本地队列和 steal 机制，用于有界队列场景下的竞争缓解。
3. `queued_tasks` 统一计数全局与本地排队任务，确保 `queued_count/stats/shutdown_now` 语义一致。
4. `shutdown_now` 同时清空全局与本地队列并触发取消。

## 验证命令

```bash
cargo test thread_pool --tests
cargo bench --bench thread_pool_bench -- --warm-up-time 0.02 --measurement-time 0.04
```

## 验证结果

- `thread_pool` 相关测试: `26/26` 通过。
- benchmark 全部分组可执行，包含：
  - `thread_pool_throughput`
  - `thread_pool_granularity`
  - `thread_pool_vs_rayon`

## 关键读数（当前轮）

### `thread_pool_vs_rayon`（ThreadPool）

- `workers=1/iters=256`: `~2.77 Melem/s`
- `workers=4/iters=256`: `~1.95 Melem/s`
- `workers=8/iters=256`: `~0.41 Melem/s`
- `workers=1/iters=2048`: `~0.80 Melem/s`
- `workers=4/iters=2048`: `~1.78 Melem/s`
- `workers=8/iters=2048`: `~0.42 Melem/s`

### `thread_pool_vs_rayon`（Rayon 对照）

- `workers=1/iters=256`: `~11.09 Melem/s`
- `workers=4/iters=256`: `~32.38 Melem/s`
- `workers=8/iters=256`: `~31.25 Melem/s`
- `workers=1/iters=2048`: `~1.40 Melem/s`
- `workers=4/iters=2048`: `~4.12 Melem/s`
- `workers=8/iters=2048`: `~3.94 Melem/s`

## 当前结论

1. 本轮优化后，ThreadPool 吞吐已回归到重构前基线附近。
2. 与 Rayon 的数量级差距仍显著，瓶颈核心依然是共享状态锁和任务分发模型。
3. 若要进一步接近业界实现，需要进入下一阶段：更彻底的低锁/无锁调度结构重构（例如基于 `crossbeam-deque` 的 injector + local deque + stealer 模型）。
