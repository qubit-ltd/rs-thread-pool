# Qubit Thread Pool 设计

[English](design.md) · 本文描述 0.12.1 版本的当前设计。

## 范围

本文以接纳入口、共享计数、线程池内部状态、worker 实现，以及生命周期、提交、取消和 Loom 测试为依据，说明动态池和固定池的 OS 线程执行模型。历史实验报告不作为当前架构规范。

## 组件

| 组件 | 职责 |
| --- | --- |
| `ThreadPool` / `FixedThreadPool` | 对外实现 `ExecutorService`，提供生命周期操作。 |
| 线程池 inner 与 state | 保存各自的 worker 策略、monitor、生命周期和通知逻辑。 |
| `AdmissionGate` | 用一个原子值保存关闭位和正在接纳的提交数。 |
| `PoolAccounting` | 统一管理槽位预留，以及排队、运行、取消中和完成计数。 |
| `PoolJob` / `PoolJobTicket` | 管理回调所有权；ticket 可一次性竞争移除动态池中的排队任务。 |
| 动态 FIFO / 固定池 `Injector` | 动态 worker 使用可移除的 `Arc<QueuedJob>` 条目；固定池仍使用 crossbeam injector。 |
| worker 循环与 hook | 领取任务、隔离展开式 panic、登记完成并退出。 |

两个池都没有 worker 本地队列，也没有直接向新 worker 交付首个任务的独立通道。动态池和固定池保留各自的策略；共享计数模块并不是通用调度器。

## 提交与接纳

提交守卫先进入 `AdmissionGate`。关闭入口是原子操作：之后的提交无法进入，已经进入的提交仍可正常离开。动态池随后持有状态 monitor，检查 `Running`、预留 worker 名额、创建必要的 worker，并预留任务槽位。固定池已有 worker，只需预留槽位。

接纳及必要的 worker 创建成功后，提交线程在状态 monitor 外同步执行接纳回调。接纳前拒绝任务时，accept、run、cancel 都不会调用。接纳回调 panic 时释放槽位，返回 `PoolJobSubmissionError::AcceptancePanicked`，不执行 run 或 cancel。接纳成功后先增加 submitted 和 queued 计数，再发布到对应线程池的队列。动态队列会保留 ticket 任务的可移除条目标识；固定池的发布方式不变。提交守卫在离开时释放接纳计数，错误路径也不例外。`Ok(())` 仅表示任务已被接纳，不代表已经开始或成功完成。

接纳回调可以调用同一个池的 `shutdown`：它关闭入口，但不等待当前回调。不能调用 `stop`、`join` 或 `wait_termination`，因为这些操作可能等待当前提交本身。所有回调都应保持短小。动态池的提交即使已经进入接纳入口，若在预留资源前持锁观察到关闭，仍会被拒绝。

## 队列容量与 worker 扩缩

动态 `ThreadPool::try_submit_callable` 在接纳成功前保留 typed callable。
Shutdown、Saturated 和 WorkerSpawnFailed 会在 `RejectedTask<C>` 中返还同一个
callable；池不会调用或销毁它。资源预留后，任务沿用现有接纳及 FIFO 发布路径。
意外的接纳失败会展开并回滚预留，不会伪装成可恢复拒绝。原
`submit_callable` 仍消费被拒绝任务。

空闲超时最多按一小时的定时片段等待。片段超时会重新计算剩余 keep-alive，
只有完整时长届满才退休 worker。定时器注册或完成错误会降级为一次无期限
monitor 等待，避免 panic 和忙轮询，同时仍可由生命周期或配置通知唤醒；发生
此错误时空闲回收可能延迟。

动态池按以下顺序接纳：先向 core size 增长（没有 worker 时至少创建一个），否则尝试预留有界队列槽位；槽位不足时向 maximum size 增长；仍无法接收时返回 `Saturated`。创建 worker 失败会在调用接纳回调前返回 `WorkerSpawnFailed`，并回滚 worker 名额。无界队列不会触发超出 core size 的压力扩容；core size 为零时仍会创建一个 worker，确保任务可执行。

配置的容量约束普通排队槽位接纳，不是已接纳任务总数或内存的硬上限。成功扩容的路径可以额外预留一个交接槽位，再把任务发布到动态 FIFO；任务不绑定新 worker。槽位覆盖已接纳或尚在接纳的排队工作，以及尚未完成的取消回调。任务转入运行时释放槽位，因此 `queued_count()` 不一定等于已预留槽位数。

固定池预启动配置数量的 worker，运行期间不扩容。动态池根据 keep-alive、core 超时策略和 maximum size 调整回收空闲 worker。只剩最后一个允许超时的 worker 时，若仍有提交可能发布任务，该 worker 不会退出。运行时修改 core size 影响后续接纳和显式预启动，不会立即为现有队列创建 worker。

启用 `async-wait` 时，运行时成功修改 core 或 maximum size 也会发布容量提示。增加 maximum 可能使先前饱和的提交变为可接纳，但通知不预留槽位，调用方仍须重试并处理饱和。

## worker 领取与取消

固定池 worker 从 Injector 领取任务，发生竞争时重试；动态池 worker 从可移除 FIFO 取下一条任务。worker 在领取前和最终领取决策处检查 `stop_now`：决策时已观察到 stop 就取消任务，否则登记运行所有权并执行。stop 若发生在决策之后，即使用户代码尚未开始，也不能撤销该运行任务。

stop 还会等待正在接纳的提交离开，再清空可见的排队任务。动态池的 `PoolJobTicket::cancel_queued` 与 worker、stop 竞争同一份排队所有权；若尚未被它们领取，ticket 会从队列中移除该任务。任务所有权只会被一个 run 或 cancel 路径消费。ticket 或 stop 的取消可能由取消调用者、提交线程、stop 调用线程或 worker 执行，具体取决于竞争结果。只有取消回调及其捕获值释放后才释放槽位；即使发生已捕获的展开式 panic，也会完成计数。

接纳、ticket 预约、发布、回滚和取消完成的条目状态转换由生产调用方与
`pool_job/internal/queued_job.rs` 中的 Loom 模型共用，转换 helper 位于
`pool_job/internal/queued_job_state.rs`。普通构建使用标准 mutex、condition variable
和线程身份；同时启用 `cfg(loom)` 与 `loom-model` 时，`internal/sync.rs` 切换为
Loom 同步原语。这些模型覆盖条目所有权和部分 accounting 转换，不覆盖 FIFO 锁、
外层接纳协议或完整线程池生命周期。每个模型最多使用五个 Loom 线程并探索至多
10,000 个分支，不限制排列或抢占次数；分支见证记录发布结果，以及取消完成前后
的容量状态。外层线程池行为仍由真实线程测试验证。

## 生命周期状态机

```text
Running --shutdown--> ShuttingDown --drained/workers exited--> Terminated
Running --stop------> Stopping -----cancel/drain/workers exited--> Terminated
ShuttingDown --stop-> Stopping
```

shutdown 关闭接纳入口，持 monitor 切换生命周期并唤醒 worker，不等待提交或已接纳任务结束。stop 关闭入口、设置停止标志、等待正在接纳的提交、取出排队任务，再在 monitor 外执行取消回调。它不会强制中断运行中的任务，也不是终止屏障。重复调用生命周期操作不会重新开放接纳。

两种池都要求生命周期不再是 `Running` 且任务计数状态空闲，才算终止。动态池还要求 `live_workers == 0` 且 `exiting_workers == 0`。动态 worker 离开调度循环时会从 `live_workers` 中移除，并计入 `exiting_workers`，直到 `after_worker_stop` hook 返回。因此，动态池的 `live_worker_count()` 不包含正在运行退出 hook 的 worker，但终止等待仍会等它们完成。动态池的 `wait_termination`、`wait_termination_timeout` 和 `await_termination` 使用此谓词；启用 `async-wait` 时，终止 watch 仅在谓词成立后发布终止状态。

固定池使用不同的谓词：生命周期不再是 `Running`、`live_workers == 0`，且任务计数状态空闲。固定 worker 在运行 `after_worker_stop` hook 期间仍计入 `live_workers`，hook 返回后才递减该计数。两种池达到终止状态时，退出 hook 都已返回；但这不保证操作系统已物理回收线程。`join` 只等待任务计数状态空闲，不关闭接纳入口，也不要求空闲 worker 退出。并发提交可能延长等待，或使刚观察到的空闲状态失效。若任务或回调的自身进展是同池终止的前提，就不能在该任务或回调中等待终止；否则它会阻止等待条件成立。

## 计数不变量

- 接纳计数覆盖所有仍可能发布任务或回滚预留的提交。
- 每个已接纳任务只有一个最终计数归属：执行路径结束后记为 completed，或取消回调结束后记为 cancelled。
- 槽位恰好释放一次：接纳失败回滚时、领取并转入运行时，或取消结束时。排队转运行时，先登记运行所有权，再释放槽位，避免出现错误的空闲窗口。
- 静止状态下满足 `submitted = completed + cancelled`；稳定的中间状态下，尚未结束的任务归属于排队、运行或取消中。各计数分别原子更新，因此任意并发快照不保证跨字段等式成立。
- 空闲要求正在接纳的提交、预留槽位、运行任务和取消回调全部为零。终止还要求生命周期已关闭、没有登记中的 worker，也没有尚未结束退出 hook 的 worker。

已捕获的任务错误或 panic 仍会结束 worker 计数，因此 `completed_tasks` 不等于业务成功数。通过句柄取消的 tracked 任务仍可能走 worker 计数路径；`cancelled_tasks` 统计已接纳后仍在排队时被取消的任务，包括 ticket 取消和立即 stop。

## 等待与通知

每个池独立管理 monitor 和等待条件。worker 登记为空闲后，检查队列及待处理唤醒令牌，再进入等待。提交线程用原子令牌为尚未获得唤醒的空闲 worker 申请一次通知，并在 monitor 下唤醒；worker 离开空闲状态时消费令牌。这避免了“已登记空闲但尚未开始等待”窗口中的通知丢失。

提交等待者、空闲等待者计数，以及最后一个提交离开时的通知，让 stop、join 和终止等待者能在原子状态更新后重新检查条件。收到通知不代表任务已经结束，等待必须再次判断条件。取消回调完成也参与通知。

## panic 隔离

接纳、用户 run/cancel 回调、普通任务执行和 hook 都在各自边界捕获展开式 panic。接纳失败属于提交错误，不触发取消回调。hook 在 worker 线程执行，接收稳定的 worker index，panic 会被忽略。hook 的 panic payload 会在第二个展开边界内销毁；如果其析构再次 panic，会刻意遗留这个嵌套 payload，以便 worker 继续运行。该机制无法从进程 abort 或阻塞的析构函数中恢复，也不能强制中断阻塞任务。回调和 hook 应避免等待依赖自身才能推进的工作。

## 监控快照

`ThreadPoolStats` 合并 monitor 保护的生命周期/worker 状态与独立读取的任务计数。动态池的 `StopReport` 统计本次 stop 实际移出队列并取消的任务；并发 ticket 取消由 ticket 自身报告，不归入 stop。固定池保留原有尽力观测口径，可能包含 stop 竞争期间 worker 侧取消的情况。两者都不是原子事务、严格顺序保证或完成屏障。同步应使用生命周期等待，任务结果应通过句柄观察。

## 下游扩展点

普通 runnable、callable 和 tracked 提交使用 `ExecutorService`。`ThreadPool::prepare_cancellable_job` 返回尚未提交的 `PoolJob` 与可克隆的 `PoolJobTicket`；任务提交到原动态池后，`cancel_queued()` 只有在调用方赢得排队所有权竞争时才返回 `true`。接纳前、接纳失败，或任务已被其他调用者、worker、stop 领取时，返回 `false`。成功取消会先执行 cancel 回调、释放捕获值和排队槽位并更新计数，再返回。若接纳正在执行，ticket 会在不持有池锁的情况下等待；接纳成功后由提交线程在发布队列前完成取消。所有回调都在池锁与队列锁之外运行。ticket 无法中断已被 worker 领取的任务，也不适用于 `FixedThreadPool`。其他下游任务注册表仍可使用 `ThreadPool::submit_job` 和 `PoolJob::with_accept` 发布接纳状态，并自行维护执行与取消状态。处理错误时应区分 `PoolJobSubmissionError::Rejected(SubmissionError)` 和 `AcceptancePanicked`；接纳回调若先修改外部状态再 panic，需要自行安排恢复。

## 非目标

本 crate 不提供异步 future 调度、截止时间、优先级、worker 本地工作窃取、严格 FIFO 执行或强制线程取消，也不承诺在所有 benchmark 中领先。使用方式见[用户手册](user_guide.zh_CN.md)、[性能说明](performance.zh_CN.md)及[迁移指南](migration_0.9_to_0.10.zh_CN.md)。
