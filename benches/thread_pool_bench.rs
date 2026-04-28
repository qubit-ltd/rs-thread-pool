/*!
 * Benchmark for [`qubit_thread_pool::task::service::ThreadPool`].
 */

use std::convert::Infallible;
use std::hint::black_box;

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

/// Runs one batch of no-op tasks and waits until the pool terminates.
fn run_noop_batch(pool_size: usize, task_count: usize) {
    let pool = ThreadPool::new(pool_size).expect("thread pool should be created");
    let mut handles = Vec::with_capacity(task_count);
    for _ in 0..task_count {
        let handle = pool
            .submit_callable(|| Ok::<usize, Infallible>(1))
            .expect("task should be accepted");
        handles.push(handle);
    }
    let mut sum = 0usize;
    for handle in handles {
        sum += handle.get().expect("task should complete");
    }
    black_box(sum);
    pool.shutdown();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime should be created")
        .block_on(pool.await_termination());
}

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

/// Runs one batch of CPU tasks with configurable per-task work and waits until
/// the pool terminates.
fn run_cpu_work_batch(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = ThreadPool::new(pool_size).expect("thread pool should be created");
    let mut handles = Vec::with_capacity(task_count);
    for _ in 0..task_count {
        let iterations = inner_iters;
        let handle = pool
            .submit_callable(move || Ok::<usize, Infallible>(compute_cpu_work(iterations)))
            .expect("task should be accepted");
        handles.push(handle);
    }
    let mut sum = 0usize;
    for handle in handles {
        sum = sum.wrapping_add(handle.get().expect("task should complete"));
    }
    black_box(sum);
    pool.shutdown();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime should be created")
        .block_on(pool.await_termination());
}

/// Runs one batch with Rayon using equivalent task count and per-task work.
fn run_rayon_cpu_work_batch(worker_count: usize, task_count: usize, inner_iters: usize) {
    let pool = ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .expect("rayon thread pool should be created");
    let sum = pool.install(|| {
        (0..task_count)
            .into_par_iter()
            .map(|_| compute_cpu_work(inner_iters))
            .reduce(|| 0usize, usize::wrapping_add)
    });
    black_box(sum);
}

/// Benchmarks throughput under different worker counts and task types.
fn bench_thread_pool_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_throughput");
    let workers = [1usize, 2, 4, 8];
    let task_count = 2_000usize;
    group.throughput(Throughput::Elements(task_count as u64));
    for worker_count in workers {
        group.bench_with_input(
            BenchmarkId::new("noop_tasks", worker_count),
            &worker_count,
            |b, &wc| b.iter(|| run_noop_batch(wc, task_count)),
        );
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

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_thread_pool_throughput, bench_thread_pool_granularity,
        bench_thread_pool_vs_rayon, bench_thread_pool_implementations
);
criterion_main!(benches);

/// Runs one batch of CPU tasks on the fixed-size Qubit pool.
fn run_fixed_cpu_work_batch(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = FixedThreadPool::new(pool_size).expect("fixed thread pool should be created");
    let mut handles = Vec::with_capacity(task_count);
    for _ in 0..task_count {
        let iterations = inner_iters;
        let handle = pool
            .submit_callable(move || Ok::<usize, Infallible>(compute_cpu_work(iterations)))
            .expect("task should be accepted");
        handles.push(handle);
    }
    let mut sum = 0usize;
    for handle in handles {
        sum = sum.wrapping_add(handle.get().expect("task should complete"));
    }
    black_box(sum);
    pool.shutdown();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime should be created")
        .block_on(pool.await_termination());
}

/// Runs one batch with the external `threadpool` crate.
fn run_external_threadpool_cpu_work_batch(pool_size: usize, task_count: usize, inner_iters: usize) {
    let pool = ExternalThreadPool::new(pool_size);
    let (sender, receiver) = std::sync::mpsc::channel();
    for _ in 0..task_count {
        let sender = sender.clone();
        pool.execute(move || {
            let _ = sender.send(compute_cpu_work(inner_iters));
        });
    }
    drop(sender);
    let sum = receiver
        .into_iter()
        .take(task_count)
        .fold(0usize, usize::wrapping_add);
    black_box(sum);
}
