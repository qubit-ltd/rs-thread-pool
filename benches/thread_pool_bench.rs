/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
/*!
 * Benchmark for [`qubit_thread_pool::ThreadPool`].
 */

use std::convert::Infallible;
use std::hint::black_box;
use std::sync::mpsc;

use criterion::{
    BenchmarkId,
    Criterion,
    Throughput,
    criterion_group,
    criterion_main,
};
use qubit_thread_pool::FixedThreadPool;
use qubit_thread_pool::{
    ExecutorService,
    ThreadPool,
};
use rayon::{
    ThreadPoolBuilder,
    prelude::*,
};
use threadpool::ThreadPool as ExternalThreadPool;

/// Workload kind used by cross-implementation submission benchmarks.
#[derive(Clone, Copy)]
enum Workload {
    /// Small CPU-bound task.
    Light,
    /// Medium CPU-bound task.
    Medium,
    /// Larger CPU-bound task.
    Heavy,
}

impl Workload {
    /// Returns the benchmark name for this workload.
    fn name(self) -> &'static str {
        match self {
            Self::Light => "cpu_light",
            Self::Medium => "cpu_medium",
            Self::Heavy => "cpu_heavy",
        }
    }

    /// Returns the center iteration count for this workload.
    fn base_iters(self) -> usize {
        match self {
            Self::Light => 128,
            Self::Medium => 2_048,
            Self::Heavy => 16_384,
        }
    }

    /// Returns the deterministic seed for this workload's task distribution.
    fn seed(self) -> u64 {
        match self {
            Self::Light => 0x9e37_79b9_7f4a_7c15,
            Self::Medium => 0xbf58_476d_1ce4_e5b9,
            Self::Heavy => 0x94d0_49bb_1331_11eb,
        }
    }
}

/// Returns the workload set used by the submission mode benchmark.
fn benchmark_workloads() -> [Workload; 3] {
    [Workload::Light, Workload::Medium, Workload::Heavy]
}

/// Runs one batch of no-op tasks and waits until the pool terminates.
/// Runs one batch of light CPU tasks and waits until the pool terminates.
fn run_cpu_light_batch(pool_size: usize, task_count: usize) {
    run_cpu_work_batch(pool_size, task_count, 128);
}

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

/// Executes one benchmark workload and returns its deterministic result.
fn run_workload(workload: Workload, task_index: usize) -> usize {
    compute_distributed_cpu_work(workload.base_iters(), task_index, workload.seed())
}

/// Waits until an executor service has fully terminated.
fn wait_for_termination<P>(pool: &P)
where
    P: ExecutorService,
{
    pool.wait_termination();
}

/// Runs one batch of CPU tasks with configurable per-task work and waits until
/// the pool terminates.
fn run_cpu_work_batch(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = ThreadPool::new(pool_size).expect("thread pool should be created");
    run_cpu_work_batch_on_pool(&pool, task_count, inner_iters);
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Runs one batch of CPU tasks on a prestarted dynamic pool.
fn run_prestarted_cpu_work_batch(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = ThreadPool::builder()
        .pool_size(pool_size)
        .prestart_core_threads()
        .build()
        .expect("thread pool should be created");
    run_cpu_work_batch_on_pool(&pool, task_count, inner_iters);
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Runs one CPU work batch on an already created dynamic pool.
fn run_cpu_work_batch_on_pool(pool: &ThreadPool, task_count: usize, inner_iters: usize) {
    let mut handles = Vec::with_capacity(task_count);
    let seed = inner_iters as u64;
    for task_index in 0..task_count {
        let iterations = inner_iters;
        let index = task_index;
        let handle = pool
            .submit_callable(move || {
                Ok::<usize, Infallible>(compute_distributed_cpu_work(iterations, index, seed))
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

/// Runs one batch with Rayon using equivalent task count and per-task work.
fn run_rayon_cpu_work_batch(worker_count: usize, task_count: usize, inner_iters: usize) {
    let pool = ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .expect("rayon thread pool should be created");
    let seed = inner_iters as u64;
    let sum = pool.install(|| {
        (0..task_count)
            .into_par_iter()
            .map(|task_index| compute_distributed_cpu_work(inner_iters, task_index, seed))
            .reduce(|| 0usize, usize::wrapping_add)
    });
    black_box(sum);
}

/// Runs one batch on the dynamic Qubit pool through `submit`.
fn run_dynamic_submit_batch(pool_size: usize, task_count: usize, workload: Workload) {
    let pool = ThreadPool::new(pool_size).expect("thread pool should be created");
    run_dynamic_submit_batch_on_pool(&pool, task_count, workload);
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Runs one batch on the prestarted dynamic Qubit pool through `submit`.
fn run_dynamic_prestarted_submit_batch(pool_size: usize, task_count: usize, workload: Workload) {
    let pool = ThreadPool::builder()
        .pool_size(pool_size)
        .prestart_core_threads()
        .build()
        .expect("thread pool should be created");
    run_dynamic_submit_batch_on_pool(&pool, task_count, workload);
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Runs one dynamic submit batch on an already created pool.
fn run_dynamic_submit_batch_on_pool(pool: &ThreadPool, task_count: usize, workload: Workload) {
    let (sender, receiver) = mpsc::channel();
    for task_index in 0..task_count {
        let sender = sender.clone();
        pool.submit(move || {
            let _ignored = sender.send(run_workload(workload, task_index));
            Ok::<(), Infallible>(())
        })
        .expect("task should be accepted");
    }
    drop(sender);
    let sum = receiver
        .into_iter()
        .take(task_count)
        .fold(0usize, usize::wrapping_add);
    black_box(sum);
}

/// Runs one batch on the dynamic Qubit pool through `submit_tracked`.
fn run_dynamic_submit_tracked_batch(pool_size: usize, task_count: usize, workload: Workload) {
    let pool = ThreadPool::new(pool_size).expect("thread pool should be created");
    let (sender, receiver) = mpsc::channel();
    let mut handles = Vec::with_capacity(task_count);
    for task_index in 0..task_count {
        let sender = sender.clone();
        let handle = pool
            .submit_tracked(move || {
                let _ignored = sender.send(run_workload(workload, task_index));
                Ok::<(), Infallible>(())
            })
            .expect("task should be accepted");
        handles.push(handle);
    }
    drop(sender);
    let sum = receiver
        .into_iter()
        .take(task_count)
        .fold(0usize, usize::wrapping_add);
    black_box(sum);
    black_box(handles.len());
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Runs one batch on the fixed Qubit pool through `submit`.
fn run_fixed_submit_batch(pool_size: usize, task_count: usize, workload: Workload) {
    let pool = FixedThreadPool::new(pool_size).expect("fixed thread pool should be created");
    let (sender, receiver) = mpsc::channel();
    for task_index in 0..task_count {
        let sender = sender.clone();
        pool.submit(move || {
            let _ignored = sender.send(run_workload(workload, task_index));
            Ok::<(), Infallible>(())
        })
        .expect("task should be accepted");
    }
    drop(sender);
    let sum = receiver
        .into_iter()
        .take(task_count)
        .fold(0usize, usize::wrapping_add);
    black_box(sum);
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Runs one batch on the fixed Qubit pool through `submit_tracked`.
fn run_fixed_submit_tracked_batch(pool_size: usize, task_count: usize, workload: Workload) {
    let pool = FixedThreadPool::new(pool_size).expect("fixed thread pool should be created");
    let (sender, receiver) = mpsc::channel();
    let mut handles = Vec::with_capacity(task_count);
    for task_index in 0..task_count {
        let sender = sender.clone();
        let handle = pool
            .submit_tracked(move || {
                let _ignored = sender.send(run_workload(workload, task_index));
                Ok::<(), Infallible>(())
            })
            .expect("task should be accepted");
        handles.push(handle);
    }
    drop(sender);
    let sum = receiver
        .into_iter()
        .take(task_count)
        .fold(0usize, usize::wrapping_add);
    black_box(sum);
    black_box(handles.len());
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Runs one batch with the external `threadpool` crate through `execute`.
fn run_external_threadpool_submit_batch(pool_size: usize, task_count: usize, workload: Workload) {
    let pool = ExternalThreadPool::new(pool_size);
    let (sender, receiver) = mpsc::channel();
    for task_index in 0..task_count {
        let sender = sender.clone();
        pool.execute(move || {
            let _ignored = sender.send(run_workload(workload, task_index));
        });
    }
    drop(sender);
    let sum = receiver
        .into_iter()
        .take(task_count)
        .fold(0usize, usize::wrapping_add);
    black_box(sum);
}

/// Runs one batch with Rayon using equivalent task count and workload.
fn run_rayon_submit_batch(worker_count: usize, task_count: usize, workload: Workload) {
    let pool = ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .expect("rayon thread pool should be created");
    pool.install(|| {
        (0..task_count).into_par_iter().for_each(|task_index| {
            black_box(run_workload(workload, task_index));
        });
    });
}

/// Benchmarks throughput under different worker counts and task types.
fn bench_thread_pool_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_throughput");
    let workers = [1usize, 2, 4, 8];
    let task_count = 2_000usize;
    group.throughput(Throughput::Elements(task_count as u64));
    for worker_count in workers {
        group.bench_with_input(
            BenchmarkId::new("cpu_light_tasks", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_cpu_light_batch(wc, task_count)),
        );
    }
    group.finish();
}

/// Compares thread pool throughput against Rayon on the same workload model.
fn bench_thread_pool_vs_rayon(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_vs_rayon");
    let workers = [1usize, 4, 8];
    let granularities = [256usize, 2_048];
    let total_iters = 2_048_000usize;
    for worker_count in workers {
        for inner_iters in granularities {
            let task_count = total_iters / inner_iters;
            group.throughput(Throughput::Elements(task_count as u64));
            let thread_pool_id = format!("thread_pool/workers={worker_count}/iters={inner_iters}");
            group.bench_with_input(
                BenchmarkId::from_parameter(thread_pool_id),
                &worker_count,
                |b, &wc| b.iter(|| run_cpu_work_batch(wc, task_count, inner_iters)),
            );
            let rayon_id = format!("rayon/workers={worker_count}/iters={inner_iters}");
            group.bench_with_input(
                BenchmarkId::from_parameter(rayon_id),
                &worker_count,
                |b, &wc| b.iter(|| run_rayon_cpu_work_batch(wc, task_count, inner_iters)),
            );
        }
    }
    group.finish();
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
                b.iter(|| run_cpu_work_batch(wc, task_count, inner_iters))
            });
        }
    }
    group.finish();
}

/// Compares dynamic, fixed, Rayon, and external thread-pool implementations.
fn bench_thread_pool_implementations(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_implementations");
    let workers = [1usize, 4, 8];
    let inner_iters = 256usize;
    let task_count = 2_000usize;
    group.throughput(Throughput::Elements(task_count as u64));
    for worker_count in workers {
        group.bench_with_input(
            BenchmarkId::new("dynamic_thread_pool", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_cpu_work_batch(wc, task_count, inner_iters)),
        );
        group.bench_with_input(
            BenchmarkId::new("dynamic_prestarted_thread_pool", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_prestarted_cpu_work_batch(wc, task_count, inner_iters)),
        );
        group.bench_with_input(
            BenchmarkId::new("fixed_thread_pool", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_fixed_cpu_work_batch(wc, task_count, inner_iters)),
        );
        group.bench_with_input(
            BenchmarkId::new("external_threadpool", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_external_threadpool_cpu_work_batch(wc, task_count, inner_iters)),
        );
        group.bench_with_input(
            BenchmarkId::new("rayon", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_rayon_cpu_work_batch(wc, task_count, inner_iters)),
        );
    }
    group.finish();
}

/// Compares `submit`, `submit_tracked`, external `threadpool`, and Rayon by task type.
fn bench_thread_pool_submit_modes(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_submit_modes");
    let workers = [1usize, 4, 8];
    let task_count = 2_000usize;
    group.throughput(Throughput::Elements(task_count as u64));
    for workload in benchmark_workloads() {
        for worker_count in workers {
            let case = format!("{}/workers={worker_count}", workload.name());
            group.bench_with_input(
                BenchmarkId::new("dynamic_submit", &case),
                &worker_count,
                |b, &wc| b.iter(|| run_dynamic_submit_batch(wc, task_count, workload)),
            );
            group.bench_with_input(
                BenchmarkId::new("dynamic_prestarted_submit", &case),
                &worker_count,
                |b, &wc| b.iter(|| run_dynamic_prestarted_submit_batch(wc, task_count, workload)),
            );
            group.bench_with_input(
                BenchmarkId::new("dynamic_submit_tracked", &case),
                &worker_count,
                |b, &wc| b.iter(|| run_dynamic_submit_tracked_batch(wc, task_count, workload)),
            );
            group.bench_with_input(
                BenchmarkId::new("fixed_submit", &case),
                &worker_count,
                |b, &wc| b.iter(|| run_fixed_submit_batch(wc, task_count, workload)),
            );
            group.bench_with_input(
                BenchmarkId::new("fixed_submit_tracked", &case),
                &worker_count,
                |b, &wc| b.iter(|| run_fixed_submit_tracked_batch(wc, task_count, workload)),
            );
            group.bench_with_input(
                BenchmarkId::new("external_threadpool_execute", &case),
                &worker_count,
                |b, &wc| b.iter(|| run_external_threadpool_submit_batch(wc, task_count, workload)),
            );
            group.bench_with_input(BenchmarkId::new("rayon", &case), &worker_count, |b, &wc| {
                b.iter(|| run_rayon_submit_batch(wc, task_count, workload))
            });
        }
    }
    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_thread_pool_throughput, bench_thread_pool_granularity,
        bench_thread_pool_vs_rayon, bench_thread_pool_implementations,
        bench_thread_pool_submit_modes
);
criterion_main!(benches);

/// Runs one batch of CPU tasks on the fixed-size Qubit pool.
fn run_fixed_cpu_work_batch(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = FixedThreadPool::new(pool_size).expect("fixed thread pool should be created");
    let mut handles = Vec::with_capacity(task_count);
    let seed = inner_iters as u64;
    for task_index in 0..task_count {
        let iterations = inner_iters;
        let index = task_index;
        let handle = pool
            .submit_callable(move || {
                Ok::<usize, Infallible>(compute_distributed_cpu_work(iterations, index, seed))
            })
            .expect("task should be accepted");
        handles.push(handle);
    }
    let mut sum = 0usize;
    for handle in handles {
        sum = sum.wrapping_add(handle.get().expect("task should complete"));
    }
    black_box(sum);
    pool.shutdown();
    wait_for_termination(&pool);
}

/// Runs one batch with the external `threadpool` crate.
fn run_external_threadpool_cpu_work_batch(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = ExternalThreadPool::new(pool_size);
    let (sender, receiver) = std::sync::mpsc::channel();
    let seed = inner_iters as u64;
    for task_index in 0..task_count {
        let sender = sender.clone();
        pool.execute(move || {
            let _ = sender.send(compute_distributed_cpu_work(inner_iters, task_index, seed));
        });
    }
    drop(sender);
    let sum = receiver
        .into_iter()
        .take(task_count)
        .fold(0usize, usize::wrapping_add);
    black_box(sum);
}
