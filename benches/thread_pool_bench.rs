// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
/*!
 * Benchmark for [`qubit_thread_pool::ThreadPool`].
 */

use std::convert::Infallible;
use std::hint::black_box;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use qubit_executor::ExecutorService;
use qubit_thread_pool::FixedThreadPool;
use qubit_thread_pool::ThreadPool;
use rayon::ThreadPoolBuilder;
use rayon::iter::IntoParallelIterator;
use rayon::iter::ParallelIterator;
use threadpool::ThreadPool as ExternalThreadPool;

/// Performs a deterministic amount of CPU work for one task.
fn compute_cpu_work(inner_iters: usize) -> usize {
    let mut acc = 0usize;
    for i in 0..inner_iters {
        acc = acc.wrapping_add(black_box(i));
    }
    acc
}

/// Mixes a task index into a deterministic pseudo-random value.
fn mix_task_index(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Returns a deterministic bell-shaped iteration count for one task.
///
/// The distribution is an integer Irwin-Hall approximation: summing multiple
/// uniform samples gives most tasks a cost near `base_iters`, while retaining a
/// visible long and short tail for scheduler and stealing behavior.
fn distributed_inner_iters(base_iters: usize, task_index: usize, seed: u64) -> usize {
    const SAMPLE_COUNT: usize = 6;
    const SAMPLE_MAX: usize = 255;

    let mut sample_sum = 0usize;
    let mut value = seed ^ task_index as u64;
    for sample_index in 0..SAMPLE_COUNT {
        value = mix_task_index(value ^ sample_index as u64);
        sample_sum += (value & SAMPLE_MAX as u64) as usize;
    }

    let center = (SAMPLE_COUNT * SAMPLE_MAX) / 2;
    let spread = base_iters / 2;
    let offset = sample_sum as isize - center as isize;
    let scaled = offset * spread as isize / center as isize;
    if scaled.is_negative() {
        base_iters.saturating_sub((-scaled) as usize).max(1)
    } else {
        base_iters.saturating_add(scaled as usize).max(1)
    }
}

/// Performs distributed CPU work for one task.
fn compute_distributed_cpu_work(base_iters: usize, task_index: usize, seed: u64) -> usize {
    let inner_iters = distributed_inner_iters(base_iters, task_index, seed);
    compute_cpu_work(inner_iters)
}

/// Waits until an executor service has fully terminated.
fn wait_for_termination<P>(pool: &P)
where
    P: ExecutorService,
{
    pool.wait_termination();
}

/// Fixture that measures one task waking a prestarted idle worker.
struct IdleWakeupFixture<P> {
    /// Pool containing exactly one worker that returns to the idle state.
    pool: P,
    /// Sends task-completion notifications from the worker to the benchmark.
    sender: mpsc::Sender<()>,
    /// Receives task-completion notifications before timing stops.
    receiver: mpsc::Receiver<()>,
}

impl<P> IdleWakeupFixture<P> {
    /// Creates a fixture and waits for its worker to become idle.
    ///
    /// # Parameters
    ///
    /// * `pool` - Single-worker pool whose wake-up path is measured.
    /// * `idle_worker_count` - Reads the pool's current idle-worker count.
    ///
    /// # Returns
    ///
    /// A fixture ready to measure idle-worker wake-ups.
    fn new(pool: P, idle_worker_count: fn(&P) -> usize) -> Self {
        let (sender, receiver) = mpsc::channel();
        let fixture = Self { pool, sender, receiver };
        fixture.wait_for_idle_worker(idle_worker_count);
        fixture
    }

    /// Measures submission through completion while the worker starts idle.
    ///
    /// Returning the worker to idle happens after timing stops so the measured
    /// duration isolates the submit-and-wake path.
    ///
    /// # Parameters
    ///
    /// * `submit` - Submits a task that signals completion through the sender.
    /// * `idle_worker_count` - Reads the pool's current idle-worker count.
    ///
    /// # Returns
    ///
    /// The duration from submission until task completion.
    fn round_trip(&self, submit: fn(&P, mpsc::Sender<()>), idle_worker_count: fn(&P) -> usize) -> Duration {
        let started = std::time::Instant::now();
        submit(&self.pool, self.sender.clone());
        self.receiver.recv().expect("benchmark task should signal completion");
        let elapsed = started.elapsed();
        self.wait_for_idle_worker(idle_worker_count);
        elapsed
    }

    /// Waits until the fixture's only worker has re-entered the idle state.
    ///
    /// # Parameters
    ///
    /// * `idle_worker_count` - Reads the pool's current idle-worker count.
    fn wait_for_idle_worker(&self, idle_worker_count: fn(&P) -> usize) {
        while idle_worker_count(&self.pool) != 1 {
            thread::yield_now();
        }
    }
}

/// Submits a completion-signalling task to a dynamic pool.
///
/// # Parameters
///
/// * `pool` - Dynamic pool whose idle worker should run the task.
/// * `completed_sender` - Reports task completion to the benchmark thread.
fn submit_dynamic_idle_wakeup(pool: &ThreadPool, completed_sender: mpsc::Sender<()>) {
    pool.submit(move || {
        completed_sender
            .send(())
            .expect("benchmark should receive dynamic task completion");
        Ok::<(), Infallible>(())
    })
    .expect("dynamic pool should accept benchmark task");
}

/// Submits a completion-signalling task to a fixed pool.
///
/// # Parameters
///
/// * `pool` - Fixed pool whose idle worker should run the task.
/// * `completed_sender` - Reports task completion to the benchmark thread.
fn submit_fixed_idle_wakeup(pool: &FixedThreadPool, completed_sender: mpsc::Sender<()>) {
    pool.submit(move || {
        completed_sender
            .send(())
            .expect("benchmark should receive fixed task completion");
        Ok::<(), Infallible>(())
    })
    .expect("fixed pool should accept benchmark task");
}

/// Returns the number of idle workers in a dynamic pool.
///
/// # Parameters
///
/// * `pool` - Dynamic pool whose state is observed.
///
/// # Returns
///
/// The current idle-worker count.
fn dynamic_idle_worker_count(pool: &ThreadPool) -> usize {
    pool.stats().idle_workers
}

/// Returns the number of idle workers in a fixed pool.
///
/// # Parameters
///
/// * `pool` - Fixed pool whose state is observed.
///
/// # Returns
///
/// The current idle-worker count.
fn fixed_idle_worker_count(pool: &FixedThreadPool) -> usize {
    pool.stats().idle_workers
}

/// Submits one CPU work batch to an existing dynamic pool and waits for every
/// task to finish.
///
/// # Parameters
///
/// * `pool` - The pre-existing dynamic pool that accepts the tasks.
/// * `task_count` - Number of tasks submitted in this batch.
/// * `inner_iters` - Center CPU-work iteration count for each task.
fn submit_dynamic_cpu_batch(pool: &ThreadPool, task_count: usize, inner_iters: usize) {
    let mut handles = Vec::with_capacity(task_count);
    let seed = inner_iters as u64;
    for task_index in 0..task_count {
        let handle = pool
            .submit_callable(move || {
                Ok::<usize, Infallible>(compute_distributed_cpu_work(inner_iters, task_index, seed))
            })
            .expect("task should be accepted");
        handles.push(handle);
    }
    let mut sum = 0usize;
    for handle in handles {
        sum = sum.wrapping_add(handle.get().expect("task should complete"));
    }
    black_box(sum);
}

/// Submits one CPU work batch to an existing fixed pool and waits for every
/// task to finish.
///
/// # Parameters
///
/// * `pool` - The pre-existing fixed pool that accepts the tasks.
/// * `task_count` - Number of tasks submitted in this batch.
/// * `inner_iters` - Center CPU-work iteration count for each task.
fn submit_fixed_cpu_batch(pool: &FixedThreadPool, task_count: usize, inner_iters: usize) {
    let seed = inner_iters as u64;
    let mut handles = Vec::with_capacity(task_count);
    for task_index in 0..task_count {
        handles.push(
            pool.submit_callable(move || {
                Ok::<usize, Infallible>(compute_distributed_cpu_work(inner_iters, task_index, seed))
            })
            .expect("task should be accepted"),
        );
    }
    let sum = handles.into_iter().fold(0usize, |sum, handle| {
        sum.wrapping_add(handle.get().expect("task should complete"))
    });
    black_box(sum);
}

/// Submits one CPU work batch to an external pool and waits for every task to
/// finish.
///
/// # Parameters
///
/// * `pool` - The pre-existing external pool that executes the tasks.
/// * `task_count` - Number of tasks submitted in this batch.
/// * `inner_iters` - Center CPU-work iteration count for each task.
fn submit_external_cpu_batch(pool: &ExternalThreadPool, task_count: usize, inner_iters: usize) {
    let (sender, receiver) = mpsc::channel();
    let seed = inner_iters as u64;
    for task_index in 0..task_count {
        let sender = sender.clone();
        pool.execute(move || {
            sender
                .send(compute_distributed_cpu_work(inner_iters, task_index, seed))
                .expect("result should be received");
        })
    }
    drop(sender);
    let sum = receiver.into_iter().take(task_count).fold(0usize, usize::wrapping_add);
    black_box(sum);
}

/// Submits one CPU work batch to a Rayon pool and waits for every task to
/// finish.
///
/// # Parameters
///
/// * `pool` - The pre-existing Rayon pool that executes the tasks.
/// * `task_count` - Number of tasks submitted in this batch.
/// * `inner_iters` - Center CPU-work iteration count for each task.
fn submit_rayon_cpu_batch(pool: &rayon::ThreadPool, task_count: usize, inner_iters: usize) {
    let seed = inner_iters as u64;
    let sum = pool.install(|| {
        (0..task_count)
            .into_par_iter()
            .map(|task_index| compute_distributed_cpu_work(inner_iters, task_index, seed))
            .reduce(|| 0usize, usize::wrapping_add)
    });
    black_box(sum);
}

/// Creates, uses, and shuts down a dynamic pool within one benchmark sample.
///
/// # Parameters
///
/// * `pool_size` - Number of workers created for the sample.
/// * `task_count` - Number of tasks submitted in the sample.
/// * `inner_iters` - Center CPU-work iteration count for each task.
fn run_dynamic_cpu_batch_end_to_end(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = ThreadPool::new(pool_size).expect("dynamic pool should be created");
    submit_dynamic_cpu_batch(&pool, task_count, inner_iters);
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Creates, uses, and shuts down a fixed pool within one benchmark sample.
///
/// # Parameters
///
/// * `pool_size` - Number of workers created for the sample.
/// * `task_count` - Number of tasks submitted in the sample.
/// * `inner_iters` - Center CPU-work iteration count for each task.
fn run_fixed_cpu_batch_end_to_end(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = FixedThreadPool::new(pool_size).expect("fixed pool should be created");
    submit_fixed_cpu_batch(&pool, task_count, inner_iters);
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Creates an external pool, completes one task batch, then requests worker
/// shutdown by dropping the pool.
///
/// This does not wait for the external pool's workers to terminate.
///
/// # Parameters
///
/// * `pool_size` - Number of workers created for the sample.
/// * `task_count` - Number of tasks submitted in the sample.
/// * `inner_iters` - Center CPU-work iteration count for each task.
fn run_external_cpu_batch_through_drop_request(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = ExternalThreadPool::new(pool_size);
    submit_external_cpu_batch(&pool, task_count, inner_iters);
    pool.join();
    drop(pool);
}

/// Creates a Rayon pool, completes one task batch, then requests worker
/// shutdown by dropping the pool.
///
/// This does not wait for Rayon workers to terminate.
///
/// # Parameters
///
/// * `pool_size` - Number of workers created for the sample.
/// * `task_count` - Number of tasks submitted in the sample.
/// * `inner_iters` - Center CPU-work iteration count for each task.
fn run_rayon_cpu_batch_through_drop_request(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = ThreadPoolBuilder::new()
        .num_threads(pool_size)
        .build()
        .expect("rayon pool should be created");
    submit_rayon_cpu_batch(&pool, task_count, inner_iters);
    drop(pool);
}

/// Benchmarks scheduling overhead vs task granularity under fixed total work.
fn bench_thread_pool_granularity(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_granularity");
    let workers = [1usize, 4, 8];
    let granularities = [32usize, 256, 2_048];
    let total_iters = 2_048_000usize;
    for worker_count in workers {
        for inner_iters in granularities {
            let task_count = total_iters / inner_iters;
            let id = format!("workers={worker_count}/iters={inner_iters}");
            group.throughput(Throughput::Elements(task_count as u64));
            group.bench_with_input(BenchmarkId::from_parameter(id), &worker_count, |b, &wc| {
                b.iter(|| run_dynamic_cpu_batch_end_to_end(wc, task_count, inner_iters))
            });
        }
    }
    group.finish();
}

/// Measures the idle-worker submit-and-wake path for both Qubit pool types.
fn bench_thread_pool_idle_wakeup(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_idle_wakeup");
    group.throughput(Throughput::Elements(1));

    let dynamic_pool = ThreadPool::builder()
        .pool_size(1)
        .prestart_core_threads()
        .build()
        .expect("dynamic thread pool should be created");
    let dynamic = IdleWakeupFixture::new(dynamic_pool, dynamic_idle_worker_count);
    group.bench_function("dynamic_prestarted", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                elapsed += dynamic.round_trip(submit_dynamic_idle_wakeup, dynamic_idle_worker_count);
            }
            elapsed
        });
    });

    let fixed = IdleWakeupFixture::new(
        FixedThreadPool::new(1).expect("fixed thread pool should be created"),
        fixed_idle_worker_count,
    );
    group.bench_function("fixed", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                elapsed += fixed.round_trip(submit_fixed_idle_wakeup, fixed_idle_worker_count);
            }
            elapsed
        });
    });
    group.finish();

    dynamic.pool.shutdown();
    wait_for_termination(&dynamic.pool);
    fixed.pool.shutdown();
    wait_for_termination(&fixed.pool);
}

/// Compares task submission and completion on pools created outside the timed
/// iterations.
fn bench_thread_pool_steady_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_steady_state");
    let workers = [1usize, 4, 8];
    let inner_iters = 256usize;
    let task_count = 2_000usize;
    group.throughput(Throughput::Elements(task_count as u64));
    for worker_count in workers {
        let dynamic = ThreadPool::builder()
            .core_pool_size(worker_count)
            .maximum_pool_size(worker_count)
            .prestart_core_threads()
            .build()
            .expect("dynamic pool should be created");
        group.bench_with_input(BenchmarkId::new("dynamic", worker_count), &worker_count, |b, &_wc| {
            b.iter(|| submit_dynamic_cpu_batch(&dynamic, task_count, inner_iters))
        });
        dynamic.shutdown();
        wait_for_termination(&dynamic);

        let fixed = FixedThreadPool::new(worker_count).expect("fixed pool should be created");
        group.bench_with_input(BenchmarkId::new("fixed", worker_count), &worker_count, |b, &_wc| {
            b.iter(|| submit_fixed_cpu_batch(&fixed, task_count, inner_iters))
        });
        fixed.shutdown();
        wait_for_termination(&fixed);

        let external = ExternalThreadPool::new(worker_count);
        group.bench_with_input(BenchmarkId::new("external", worker_count), &worker_count, |b, &_wc| {
            b.iter(|| submit_external_cpu_batch(&external, task_count, inner_iters))
        });
        external.join();
        drop(external);

        let rayon = ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .build()
            .expect("rayon pool should be created");
        group.bench_with_input(BenchmarkId::new("rayon", worker_count), &worker_count, |b, &_wc| {
            b.iter(|| submit_rayon_cpu_batch(&rayon, task_count, inner_iters))
        });
        drop(rayon);
    }
    group.finish();
}

/// Compares pool construction, task completion, shutdown, and worker
/// termination costs for pool implementations with an observable termination
/// barrier.
fn bench_thread_pool_end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_end_to_end");
    let workers = [1usize, 4, 8];
    let inner_iters = 256usize;
    let task_count = 2_000usize;
    group.throughput(Throughput::Elements(task_count as u64));
    for worker_count in workers {
        group.bench_with_input(
            BenchmarkId::new("dynamic_lifecycle", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_dynamic_cpu_batch_end_to_end(wc, task_count, inner_iters)),
        );
        group.bench_with_input(
            BenchmarkId::new("fixed_lifecycle", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_fixed_cpu_batch_end_to_end(wc, task_count, inner_iters)),
        );
    }
    group.finish();
}

/// Measures construction, task completion, and pool drop requests for
/// implementations whose worker termination cannot be observed.
fn bench_thread_pool_drop_request(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_drop_request");
    let workers = [1usize, 4, 8];
    let inner_iters = 256usize;
    let task_count = 2_000usize;
    group.throughput(Throughput::Elements(task_count as u64));
    for worker_count in workers {
        group.bench_with_input(
            BenchmarkId::new("external_drop_request", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_external_cpu_batch_through_drop_request(wc, task_count, inner_iters)),
        );
        group.bench_with_input(
            BenchmarkId::new("rayon_drop_request", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_rayon_cpu_batch_through_drop_request(wc, task_count, inner_iters)),
        );
    }
    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_thread_pool_steady_state, bench_thread_pool_end_to_end,
        bench_thread_pool_drop_request, bench_thread_pool_granularity,
        bench_thread_pool_idle_wakeup
);
criterion_main!(benches);
