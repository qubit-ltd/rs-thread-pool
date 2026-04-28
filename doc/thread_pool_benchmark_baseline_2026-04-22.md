# ThreadPool Benchmark Baseline (2026-04-22)

## 环境信息

- 日期: 2026-04-22
- 机器: Apple Silicon macOS (`Darwin 25.3.0`)
- CPU: `hw.physicalcpu=16`, `hw.logicalcpu=16`
- Rust crate: pre-split thread-pool baseline

## 执行命令

```bash
cargo bench --bench thread_pool_bench -- --warm-up-time 0.02 --measurement-time 0.04
```

> 说明: 本次基线用于相对对比（同机、同参数下前后版本对比），不是跨机器绝对性能结论。

## 关键结果摘录

### 1) ThreadPool 自身吞吐（`thread_pool_throughput`）

- `noop_tasks/1`: 约 `4.04 Melem/s`
- `noop_tasks/4`: 约 `1.97 Melem/s`
- `noop_tasks/8`: 约 `0.42 Melem/s`
- `cpu_light_tasks/1`: 约 `3.05 Melem/s`
- `cpu_light_tasks/4`: 约 `1.95 Melem/s`
- `cpu_light_tasks/8`: 约 `0.42 Melem/s`

### 2) 固定总工作量粒度测试（`thread_pool_granularity`）

- `workers=1/iters=256`: 约 `2.84 Melem/s`
- `workers=4/iters=256`: 约 `1.98 Melem/s`
- `workers=8/iters=256`: 约 `0.44 Melem/s`
- `workers=1/iters=2048`: 约 `0.83 Melem/s`
- `workers=4/iters=2048`: 约 `1.84 Melem/s`
- `workers=8/iters=2048`: 约 `0.46 Melem/s`

### 3) 与 Rayon 对照（`thread_pool_vs_rayon`）

#### `iters=256`

- `workers=1`: ThreadPool `~2.76 Melem/s`, Rayon `~11.18 Melem/s`
- `workers=4`: ThreadPool `~2.01 Melem/s`, Rayon `~32.67 Melem/s`
- `workers=8`: ThreadPool `~0.44 Melem/s`, Rayon `~29.75 Melem/s`

#### `iters=2048`

- `workers=1`: ThreadPool `~0.83 Melem/s`, Rayon `~1.40 Melem/s`
- `workers=4`: ThreadPool `~1.78 Melem/s`, Rayon `~4.17 Melem/s`
- `workers=8`: ThreadPool `~0.42 Melem/s`, Rayon `~3.92 Melem/s`

## 当前结论

1. 小任务和中等粒度任务下，`ThreadPool` 在 `8 workers` 时出现明显扩展性下降。
2. 与 Rayon 对比，当前实现在并发扩展方面存在数量级差距。
3. 下一步重构应优先针对队列竞争和调度策略（局部队列 + work-stealing）。

## 回归对比建议

后续每次重构完成后，保留相同命令和参数执行，并重点对比：

1. `thread_pool_vs_rayon/thread_pool/workers=4/iters=256`
2. `thread_pool_vs_rayon/thread_pool/workers=8/iters=256`
3. `thread_pool_vs_rayon/thread_pool/workers=4/iters=2048`
4. `thread_pool_vs_rayon/thread_pool/workers=8/iters=2048`
