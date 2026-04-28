# ThreadPool vs Rayon Design Review (2026-04-24)

## Scope

This note compares the current `ThreadPool` implementation with Rayon source
checked out at:

- `/Users/starfish/working/github/rayon-git`
- Commit: `c4dac9f82ee03f61e855075b668229a9e4d759ad`

The goal is not to copy Rayon wholesale. Rayon is a data-parallel runtime with
structured `join` and parallel iterator APIs, while this crate exposes a general
executor-style `ThreadPool` with `submit_callable`, `TaskHandle`, cancellation,
shutdown, bounded queues, and Java-like pool sizing semantics. The useful part
is to identify which low-contention scheduling ideas fit our API.

## Rayon Core Advantages

### 1. Hot work is worker-local, not state-lock-local

Rayon creates one `crossbeam_deque::Worker<JobRef>` and one `Stealer<JobRef>`
per worker in `Registry::new`:

- `rayon-core/src/registry.rs:248`
- `rayon-core/src/registry.rs:269`
- `rayon-core/src/registry.rs:647`

External submissions go through a global `Injector<JobRef>`, but work created
inside a Rayon worker goes to that worker's local deque:

- `rayon-core/src/registry.rs:409`
- `rayon-core/src/registry.rs:426`
- `rayon-core/src/registry.rs:727`

This gives Rayon the main scalability property: a busy worker normally pushes
and pops its own work without contending on a global state lock. Other workers
only touch that queue through the `Stealer` side.

### 2. Rayon knows whether submit happens inside a worker

Rayon stores the current worker in thread-local state:

- `rayon-core/src/registry.rs:670`
- `rayon-core/src/registry.rs:697`

`Registry::inject_or_push` uses that TLS identity to choose local deque vs
global injector. This is a major difference from our current `ThreadPool`: our
public `submit_callable` path does not currently know whether it is called from
one of our own workers, so every submit is treated as an external submit.

### 3. `join` avoids handle-style blocking

Rayon's `join_context` publishes one branch as a stack job, executes the other
branch inline, then either runs the local job directly or helps execute other
work while waiting:

- `rayon-core/src/join/mod.rs:132`
- `rayon-core/src/join/mod.rs:139`
- `rayon-core/src/join/mod.rs:153`
- `rayon-core/src/join/mod.rs:157`

This is materially different from submitting many independent tasks and waiting
on many handles. It avoids one allocation/handle/result path per logical item
and keeps the waiting worker productive.

### 4. Rayon controls task granularity

Rayon's parallel iterators split work recursively and adaptively instead of
turning every element into an individual runtime task:

- `src/iter/plumbing/mod.rs:245`
- `src/iter/plumbing/mod.rs:267`
- `src/iter/plumbing/mod.rs:385`
- `src/iter/plumbing/mod.rs:406`
- `src/iter/plumbing/mod.rs:431`

This explains a large part of the CPU and graph benchmark gap. Our current
benchmarks submit one `ThreadPool` task per logical unit, while Rayon often runs
whole chunks per task after adaptive splitting.

### 5. Sleep/wakeup state is atomic-first

Rayon packs sleeping, inactive, and job-event counters into one `AtomicUsize`:

- `rayon-core/src/sleep/counters.rs:3`
- `rayon-core/src/sleep/counters.rs:89`
- `rayon-core/src/sleep/counters.rs:128`

Workers first spin/yield briefly, then sleep through per-worker condition
variables only after atomic state says sleeping is safe:

- `rayon-core/src/sleep/mod.rs:68`
- `rayon-core/src/sleep/mod.rs:88`
- `rayon-core/src/sleep/mod.rs:214`

The global sleep data is not a single mutex-protected `ThreadPoolState`.

### 6. Job representation is minimal for structured work

Rayon uses a small `JobRef` containing a pointer and execute function:

- `rayon-core/src/job.rs:27`
- `rayon-core/src/job.rs:62`

For `join`, `StackJob` lets Rayon advertise stack-owned work safely:

- `rayon-core/src/job.rs:68`
- `rayon-core/src/job.rs:97`

Our `PoolJob` and `TaskHandle` path is more general, but more expensive. That
cost is acceptable for executor semantics, but it should not be mistaken for
Rayon's lower-level data-parallel task cost.

## Current ThreadPool Observations

### What has already improved

The current implementation has already moved several submit-path values out of
the state lock:

- atomic lifecycle and admission gate: `src/task/service/thread_pool/cas.rs`
- fast-path queued slot reservation: `thread_pool_inner.rs:350`
- submit fast path before taking `state_monitor`: `thread_pool_inner.rs:606`
- atomic queue/running/submitted counters: `thread_pool_inner.rs:203`

This is directionally correct.

### Remaining bottlenecks

1. `WorkerQueue` uses `Injector<PoolJob>` instead of `Worker/Stealer`.

Current local queues are defined as:

- `src/task/service/thread_pool/thread_pool_inner.rs:35`
- `src/task/service/thread_pool/thread_pool_inner.rs:40`
- `src/task/service/thread_pool/thread_pool_inner.rs:123`

Because `Injector` has no owner-only pop, both local pop and remote steal use
the same `steal()` loop. That gives us multiple queues, but not the same
owner-fast-path property Rayon gets from `Worker::pop()`.

2. Workers still acquire the state monitor before polling queues.

`wait_for_job` locks state first and then calls queue polling:

- `src/task/service/thread_pool/thread_pool_inner.rs:1341`
- `src/task/service/thread_pool/thread_pool_inner.rs:1347`

That means worker job acquisition is still serialized by `state_monitor`, even
when actual queue data lives elsewhere.

3. The global queue is still a mutex-protected `VecDeque`.

- `src/task/service/thread_pool/thread_pool_inner.rs:190`
- `src/task/service/thread_pool/thread_pool_inner.rs:513`
- `src/task/service/thread_pool/thread_pool_inner.rs:550`

This is likely still hot under external submit-heavy workloads.

4. `worker_queues` is an `RwLock` on enqueue and steal paths.

- `src/task/service/thread_pool/thread_pool_inner.rs:219`
- `src/task/service/thread_pool/thread_pool_inner.rs:699`
- `src/task/service/thread_pool/thread_pool_inner.rs:984`

Read locks are cheaper than a single exclusive lock, but this is still a shared
synchronization point on the scheduler hot path.

5. Wakeup and idle accounting still depend on `state_monitor`.

Idle counters are mirrored atomically, but entering and leaving idle state still
mutates locked state:

- `src/task/service/thread_pool/thread_pool_inner.rs:1356`
- `src/task/service/thread_pool/thread_pool_inner.rs:1375`
- `src/task/service/thread_pool/thread_pool_inner.rs:1408`

This is simpler than Rayon's atomic sleep protocol, but it keeps the worker idle
transition coupled to the same lock used for lifecycle and pool sizing.

## Benchmark Interpretation

The existing dataset benchmarks are useful, but they currently compare two
different abstraction levels:

- ThreadPool path: one `submit_callable` plus one `TaskHandle` per logical task.
- Rayon path: `into_par_iter`, adaptive splitting, chunk-level execution, and no
  public handle per element.

That means CPU and graph deltas include:

- scheduler overhead,
- task granularity overhead,
- `TaskHandle` allocation/result overhead,
- wakeup behavior,
- queue contention.

The IO benchmark is closer because blocking file reads dominate more of the
runtime. This matches the current results: IO is near Rayon, while CPU and graph
are far behind.

For decision-grade optimization, keep two benchmark tracks:

1. Scheduler primitive comparison: submit the same number of standalone jobs to
   ThreadPool and to Rayon `scope`/`spawn` style APIs.
2. Data-parallel comparison: add a chunked `ThreadPool` benchmark so each pool
   task processes a slice/chunk, then compare with Rayon parallel iterators.

## Recommended Improvement Direction

### Phase 1: Make the benchmark split explicit

Before deeper scheduler rewrites, add benchmark groups that separate:

- per-task executor overhead,
- chunked CPU throughput,
- chunked graph traversal,
- nested submit from inside a worker.

This will tell us whether the next bottleneck is queue acquisition, handle cost,
or missing work splitting.

### Phase 2: Replace local queues with true owner/stealer deques

Change each worker queue from `Injector<PoolJob>` to:

- owner side: `crossbeam_deque::Worker<PoolJob>`
- public steal side: `crossbeam_deque::Stealer<PoolJob>`

The owner `Worker` should stay in the worker thread object and not be reachable
from arbitrary submitters. Shared scheduler metadata should keep only stealers.

This is the closest Rayon-inspired improvement that fits our existing API.

### Phase 3: Move worker queue polling outside the state monitor

Worker loop should first try:

1. local pop,
2. global injector pop,
3. steal from victims,

without holding `state_monitor`. Only when no work is found should it enter the
state monitor to update idle/lifecycle/retirement state and potentially wait.

This is likely the highest-value lock-scope reduction left.

### Phase 4: Replace `global_queue: Mutex<VecDeque<_>>` with `Injector<_>`

A `crossbeam_deque::Injector<PoolJob>` is a better fit for external submissions
than a locked `VecDeque`. This also aligns with Rayon and lets workers steal
from global injected work without serializing every pop through a mutex.

Bounded queue capacity should remain an atomic reservation counter; the queue
itself does not need to enforce capacity.

### Phase 5: Add worker TLS and local submit fast path

Introduce a thread-local marker for the current `ThreadPool` worker. When
`submit_callable` is called from one of this pool's workers, push to that
worker's local deque instead of the global injector.

This only helps nested submit workloads, but it is essential for Rayon-like
divide-and-conquer patterns.

### Phase 6: Consider a structured parallel API separately

Rayon's biggest CPU advantage is not just a faster queue; it is also the
structured `join`/parallel-iterator model. For this crate, that should be a
separate API layer, for example:

- `ThreadPool::scope`
- `ThreadPool::join`
- `ThreadPool::parallel_for_chunks`

These APIs can avoid a `TaskHandle` per element and can use stack-scoped jobs or
chunk-level jobs. They should not be mixed into the executor-style
`submit_callable` path.

## Near-Term Implementation Order

1. Add scheduler-vs-data-parallel benchmark split.
2. Convert global queue to `crossbeam_deque::Injector<PoolJob>`.
3. Introduce a `WorkerRuntime` object owning `Worker<PoolJob>` and exposing only
   `Stealer<PoolJob>` through shared metadata.
4. Refactor `wait_for_job` so queue polling happens before taking
   `state_monitor`.
5. Add TLS-based same-pool local submit.
6. Revisit sleep/wakeup counters after the queue lock removal; do not copy
   Rayon's full sleep protocol until measurements show `state_monitor` idle
   transitions are still hot.

