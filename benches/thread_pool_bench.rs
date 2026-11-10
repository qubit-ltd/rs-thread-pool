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
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use qubit_executor::ExecutorService;
use qubit_thread_pool::FixedThreadPool;
use qubit_thread_pool::PoolJob;
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

/// Calculates and prints operation-level latency percentiles for one case.
fn report_latency_samples(label: &str, samples: &[Duration]) {
    if samples.is_empty() {
        return;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let percentile = |percent: usize| {
        if sorted.is_empty() {
            return Duration::ZERO;
        }
        sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)]
    };
    let mut overhead = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let started = Instant::now();
        black_box(());
        overhead.push(started.elapsed());
    }
    overhead.sort_unstable();
    let recording = AtomicU64::new(0);
    let mut recording_overhead = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let started = Instant::now();
        let elapsed_ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        recording.store(elapsed_ns, Ordering::Relaxed);
        recording_overhead.push(started.elapsed());
    }
    recording_overhead.sort_unstable();
    eprintln!(
        "TP006 latency case={label} phase=criterion_warmup_plus_measurement sample_count={} p50_ns={} p95_ns={} p99_ns={} timer_overhead_sample_count=10000 timer_overhead_p50_ns={} timer_overhead_p95_ns={} atomic_recording_overhead_sample_count=10000 atomic_recording_overhead_p50_ns={} atomic_recording_overhead_p95_ns={}",
        samples.len(),
        percentile(50).as_nanos(),
        percentile(95).as_nanos(),
        percentile(99).as_nanos(),
        overhead[overhead.len() / 2].as_nanos(),
        overhead[(overhead.len() - 1) * 95 / 100].as_nanos(),
        recording_overhead[recording_overhead.len() / 2].as_nanos(),
        recording_overhead[(recording_overhead.len() - 1) * 95 / 100].as_nanos(),
    );
}

/// Waits until every prestarted dynamic worker is idle.
fn wait_for_dynamic_idle_workers(pool: &ThreadPool, worker_count: usize) {
    while pool.stats().idle_workers != worker_count {
        thread::yield_now();
    }
}

/// Runs one preconditioned sparse-arrival round trip.
fn sparse_arrival_round_trip(pool: &ThreadPool, idle_workers: usize, samples: &mut Vec<Duration>) {
    wait_for_dynamic_idle_workers(pool, idle_workers);
    let (sender, receiver) = mpsc::channel();
    let started = Instant::now();
    pool.submit(move || {
        sender.send(()).expect("benchmark should receive task completion");
        Ok::<(), Infallible>(())
    })
    .expect("pool should accept sparse arrival");
    receiver.recv().expect("sparse-arrival task should complete");
    samples.push(started.elapsed());
}

/// Creates a queued ticketed no-op job.
fn make_ticketed_noop(pool: &ThreadPool) -> (PoolJob, qubit_thread_pool::PoolJobTicket) {
    pool.prepare_cancellable_job(Box::new(|| {}), Box::new(|| {}), Box::new(|| {}))
}

/// Blocks the only worker until the caller has completed queued operations.
fn start_blocked_worker(pool: &ThreadPool) -> SyncSender<()> {
    let (started_sender, started_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::sync_channel(0);
    pool.submit(move || {
        started_sender
            .send(())
            .expect("benchmark should observe blocked worker");
        release_receiver.recv().expect("benchmark should release worker");
        Ok::<(), Infallible>(())
    })
    .expect("pool should accept worker blocker");
    started_receiver.recv().expect("worker blocker should start");
    release_sender
}

/// Generates deterministic cancellation positions for one queue.
fn cancellation_order(queue_len: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<_> = (0..queue_len).collect();
    let mut state = seed;
    for index in (1..queue_len).rev() {
        state = mix_task_index(state);
        order.swap(index, state as usize % (index + 1));
    }
    order
}

/// Returns the smallest number of queue replicas that makes a ratio exact.
fn cancellation_replicas(queue_len: usize, cancel_percent: usize) -> usize {
    let numerator = queue_len * cancel_percent;
    let mut left = numerator;
    let mut right = 100;
    while right != 0 {
        (left, right) = (right, left % right);
    }
    100 / left
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

/// Measures one sparse arrival after confirming that every configured worker
/// has returned to idle.
fn bench_thread_pool_sparse_multiworker(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_sparse_multiworker");
    for worker_count in [1usize, 4, 8, 16] {
        let pool = ThreadPool::builder()
            .core_pool_size(worker_count)
            .maximum_pool_size(worker_count)
            .prestart_core_threads()
            .build()
            .expect("dynamic pool should be created");
        wait_for_dynamic_idle_workers(&pool, worker_count);
        let mut samples = Vec::new();
        group.throughput(Throughput::Elements(1));
        group.bench_function(BenchmarkId::new("all_workers_idle", worker_count), |bencher| {
            bencher.iter_custom(|iterations| {
                let mut measured = Duration::ZERO;
                for _ in 0..iterations {
                    let before = samples.len();
                    sparse_arrival_round_trip(&pool, worker_count, &mut samples);
                    measured += samples[before];
                }
                measured
            });
        });
        report_latency_samples(&format!("sparse/workers={worker_count}"), &samples);
        pool.shutdown();
        wait_for_termination(&pool);
    }
    group.finish();
}

/// Measures ticket cancellation with the only worker held outside the queue.
fn bench_thread_pool_ticket_cancellation(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_ticket_cancellation");
    for queue_len in [1usize, 128, 4_096] {
        for cancel_percent in [10usize, 50, 100] {
            let replicas = cancellation_replicas(queue_len, cancel_percent);
            let total_tickets = queue_len * replicas;
            let cancel_count = total_tickets * cancel_percent / 100;
            let mut samples = Vec::new();
            let mut fixtures = Vec::with_capacity(replicas);
            for _ in 0..replicas {
                let pool = ThreadPool::builder()
                    .core_pool_size(1)
                    .maximum_pool_size(1)
                    .prestart_core_threads()
                    .build()
                    .expect("dynamic pool should be created");
                let release_worker = start_blocked_worker(&pool);
                let mut tickets = Vec::with_capacity(queue_len);
                for _ in 0..queue_len {
                    let (job, ticket) = make_ticketed_noop(&pool);
                    pool.submit_job(job).expect("ticketed job should be accepted");
                    tickets.push(ticket);
                }
                assert_eq!(pool.queued_count(), queue_len);
                fixtures.push((pool, release_worker, tickets));
            }
            let order = cancellation_order(total_tickets, 0x5eed_cafe);
            let mut cancellations_per_pool = vec![0usize; replicas];
            group.throughput(Throughput::Elements(cancel_count as u64));
            group.bench_function(
                BenchmarkId::new(format!("queue={queue_len}/cancel={cancel_percent}%"), "dynamic"),
                |bencher| {
                    bencher.iter_custom(|iterations| {
                        let mut measured = Duration::ZERO;
                        for _ in 0..iterations {
                            for &flat_index in order.iter().take(cancel_count) {
                                let (pool_index, ticket_index) = (flat_index / queue_len, flat_index % queue_len);
                                let started = Instant::now();
                                assert!(fixtures[pool_index].2[ticket_index].cancel_queued());
                                cancellations_per_pool[pool_index] += 1;
                                let elapsed = started.elapsed();
                                measured += elapsed;
                                samples.push(elapsed);
                            }
                            for &flat_index in order.iter().take(cancel_count) {
                                let (pool_index, ticket_index) = (flat_index / queue_len, flat_index % queue_len);
                                let (job, ticket) = make_ticketed_noop(&fixtures[pool_index].0);
                                fixtures[pool_index]
                                    .0
                                    .submit_job(job)
                                    .expect("replacement ticket should be accepted");
                                fixtures[pool_index].2[ticket_index] = ticket;
                            }
                            for (pool, _, _) in &fixtures {
                                assert_eq!(pool.queued_count(), queue_len);
                            }
                        }
                        measured
                    });
                },
            );
            report_latency_samples(
                &format!("ticket_cancel/queue={queue_len}/ratio={cancel_percent}%/replicas={replicas}/seed=0x5eedcafe"),
                &samples,
            );
            for (pool_index, (pool, release_worker, _tickets)) in fixtures.into_iter().enumerate() {
                release_worker.send(()).expect("worker should be released");
                pool.shutdown();
                wait_for_termination(&pool);
                let stats = pool.stats();
                assert_eq!(stats.cancelled_tasks, cancellations_per_pool[pool_index]);
                assert_eq!(stats.completed_tasks, 1 + queue_len);
                assert_eq!(stats.queued_tasks, 0);
            }
        }
    }
    group.finish();

    bench_thread_pool_ticket_cancellation_scale_positions(c);
    bench_thread_pool_contended_submit_consume(c);
}

/// Measures cancellation at stable front/middle/back positions in long queues.
/// A worker is held before each queue and a replacement ticket is appended
/// after each timed cancellation, restoring the queue to its exact depth.
fn bench_thread_pool_ticket_cancellation_scale_positions(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_pool_ticket_cancellation_scale_position");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(2));
    for queue_len in [16_384usize, 65_536] {
        for position in ["front", "middle", "back"] {
            let pool = ThreadPool::builder()
                .core_pool_size(1)
                .maximum_pool_size(1)
                .prestart_core_threads()
                .build()
                .expect("scale pool should start");
            let release_worker = start_blocked_worker(&pool);
            let mut tickets = std::collections::VecDeque::with_capacity(queue_len);
            for _ in 0..queue_len {
                let (job, ticket) = make_ticketed_noop(&pool);
                pool.submit_job(job)
                    .expect("scale ticket should be accepted");
                tickets.push_back(ticket);
            }
            assert_eq!(
                pool.queued_count(),
                queue_len,
                "actual pre-measurement queue depth"
            );
            let mut samples = Vec::new();
            group.throughput(Throughput::Elements(1));
            group.bench_function(
                BenchmarkId::new(format!("queue={queue_len}/position={position}"), "dynamic"),
                |b| {
                    b.iter_custom(|iterations| {
                        let mut measured = Duration::ZERO;
                        for _ in 0..iterations {
                            let index = match position {
                                "front" => 0,
                                "middle" => tickets.len() / 2,
                                "back" => tickets.len() - 1,
                                _ => unreachable!(),
                            };
                            assert_eq!(tickets.len(), queue_len);
                            assert_eq!(pool.queued_count(), queue_len);
                            let started = Instant::now();
                            let cancelled = tickets[index].cancel_queued();
                            let elapsed = started.elapsed();
                            assert!(cancelled, "selected ticket should remain queued");
                            measured += elapsed;
                            samples.push(elapsed);
                            tickets.remove(index);
                            let (job, replacement) = make_ticketed_noop(&pool);
                            pool.submit_job(job)
                                .expect("replacement ticket should be accepted");
                            tickets.push_back(replacement);
                            assert_eq!(
                                pool.queued_count(),
                                queue_len,
                                "replacement restores measured depth"
                            );
                        }
                        measured
                    });
                },
            );
            report_latency_samples(
                &format!("ticket_cancel_scale/queue={queue_len}/position={position}"),
                &samples,
            );
            for ticket in &tickets {
                assert!(ticket.cancel_queued());
            }
            assert_eq!(pool.queued_count(), 0);
            release_worker
                .send(())
                .expect("scale worker should be released");
            pool.shutdown();
            wait_for_termination(&pool);
        }
    }
    group.finish();
}

/// Shared accounting for one cancellation-race batch. A terminal byte is set
/// by exactly one of the run/cancel callbacks for every accepted task.
struct RaceBatch {
    terminals: Vec<AtomicU8>,
    submitted_at_ns: Vec<AtomicU64>,
    terminal_at_ns: Vec<AtomicU64>,
    submit_call_ns: Vec<AtomicU64>,
    cancel_call_ns: Vec<AtomicU64>,
    time_origin: Instant,
    accepted: AtomicUsize,
    ran: AtomicUsize,
    cancelled: AtomicUsize,
    cancel_true: AtomicUsize,
    cancel_false: AtomicUsize,
    cancel_attempts: AtomicUsize,
    duplicate_terminal: AtomicUsize,
}

#[derive(Default)]
struct RaceTotals {
    accepted: usize,
    ran: usize,
    cancelled: usize,
    attempts: usize,
    cancel_true: usize,
    cancel_false: usize,
}

impl RaceBatch {
    fn new(task_count: usize) -> Self {
        Self {
            terminals: (0..task_count).map(|_| AtomicU8::new(0)).collect(),
            submitted_at_ns: (0..task_count).map(|_| AtomicU64::new(0)).collect(),
            terminal_at_ns: (0..task_count).map(|_| AtomicU64::new(0)).collect(),
            submit_call_ns: (0..task_count).map(|_| AtomicU64::new(0)).collect(),
            cancel_call_ns: (0..task_count).map(|_| AtomicU64::new(0)).collect(),
            time_origin: Instant::now(),
            accepted: AtomicUsize::new(0),
            ran: AtomicUsize::new(0),
            cancelled: AtomicUsize::new(0),
            cancel_true: AtomicUsize::new(0),
            cancel_attempts: AtomicUsize::new(0),
            cancel_false: AtomicUsize::new(0),
            duplicate_terminal: AtomicUsize::new(0),
        }
    }

    fn now_ns(&self) -> u64 {
        self.time_origin.elapsed().as_nanos().min(u64::MAX as u128) as u64
    }
}

/// Performs the same terminal timestamp, ID claim, and route counter updates
/// from both the run and cancellation callbacks.
fn record_race_terminal(batch: &RaceBatch, id: usize, terminal: u8) {
    let terminal_at = batch.now_ns();
    batch.terminal_at_ns[id].store(terminal_at, Ordering::Relaxed);
    if batch.terminals[id]
        .compare_exchange(0, terminal, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        batch.duplicate_terminal.fetch_add(1, Ordering::Relaxed);
    }
    if terminal == 1 {
        batch.ran.fetch_add(1, Ordering::Relaxed);
    } else {
        batch.cancelled.fetch_add(1, Ordering::Relaxed);
    }
}

/// Estimates the symmetric terminal callback bookkeeping and channel-send
/// instrumentation outside Criterion's measured race windows.
fn report_terminal_instrumentation_overhead() {
    const SAMPLE_COUNT: usize = 10_000;
    let batch = RaceBatch::new(SAMPLE_COUNT);
    let (sender, receiver) = mpsc::channel();
    let mut run_samples = Vec::with_capacity(SAMPLE_COUNT / 2);
    let mut cancel_samples = Vec::with_capacity(SAMPLE_COUNT / 2);
    for id in 0..SAMPLE_COUNT {
        let terminal = if id % 2 == 0 { 1 } else { 2 };
        let started = Instant::now();
        record_race_terminal(&batch, id, terminal);
        sender
            .send(())
            .expect("overhead sample receiver should be alive");
        let elapsed = started.elapsed();
        if terminal == 1 {
            run_samples.push(elapsed);
        } else {
            cancel_samples.push(elapsed);
        }
        receiver
            .try_recv()
            .expect("terminal event should be available");
    }
    for (label, samples) in [
        ("run", run_samples.as_slice()),
        ("cancel", cancel_samples.as_slice()),
    ] {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let percentile =
            |percent: usize| sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)];
        eprintln!(
            "TP008 terminal_instrumentation phase=standalone_calibration route={label} sample_count={} p50_ns={} p95_ns={} p99_ns={} included=batch_relative_clock_atomic_timestamp_status_cas_counter_channel_send",
            samples.len(),
            percentile(50).as_nanos(),
            percentile(95).as_nanos(),
            percentile(99).as_nanos(),
        );
    }
}

enum RaceTicket {
    Ticket(usize, qubit_thread_pool::PoolJobTicket),
    ProducerDone,
}

enum RaceCommand {
    Run(Arc<RaceBatch>),
    Stop,
}

/// Measures a real handoff race with one persistent pool and persistent actors.
/// The gate variant holds its sole worker outside the queue until cancellation
/// attempts finish, making successful cancellation observable by construction.
fn bench_thread_pool_ticket_cancel_race(c: &mut Criterion) {
    report_terminal_instrumentation_overhead();
    let mut natural = RaceTotals::default();
    let mut gated_totals = RaceTotals::default();
    for is_gated in [false, true] {
        let group_name = if is_gated {
            "thread_pool_ticket_cancel_race_gated"
        } else {
            "thread_pool_ticket_cancel_race"
        };
        let mut group = c.benchmark_group(group_name);
        let cancel_targets: &[usize] = if is_gated {
            &[10, 50, 100]
        } else {
            &[0, 10, 50, 100]
        };
        for workers in if is_gated {
            vec![1usize]
        } else {
            vec![1usize, 4]
        } {
            for producers in [1usize, 4] {
                for cancellers in [1usize, 2] {
                    for &cancel_percent in cancel_targets {
                        let totals = race_case(
                            &mut group,
                            workers,
                            producers,
                            cancellers,
                            cancel_percent,
                            is_gated,
                        );
                        if is_gated {
                            gated_totals_add(&mut gated_totals, totals);
                        } else {
                            gated_totals_add(&mut natural, totals);
                        }
                    }
                }
            }
        }
        group.finish();
    }
    // Criterion invokes every function even when its filter selects another
    // group; only enforce evidence requirements when this group actually ran.
    if natural.accepted > 0 {
        assert!(
            natural.ran > 0 && natural.cancel_false > 0,
            "natural competition must observe worker wins and failed cancellation attempts"
        );
        assert!(
            gated_totals.cancel_true > 0,
            "worker-gated variant must observe successful cancellation"
        );
    }
}

fn gated_totals_add(total: &mut RaceTotals, sample: RaceTotals) {
    total.accepted += sample.accepted;
    total.ran += sample.ran;
    total.cancelled += sample.cancelled;
    total.attempts += sample.attempts;
    total.cancel_true += sample.cancel_true;
    total.cancel_false += sample.cancel_false;
}

fn race_case(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    workers: usize,
    producers: usize,
    cancellers: usize,
    cancel_percent: usize,
    gated: bool,
) -> RaceTotals {
    const TASKS_PER_BATCH: usize = 256;
    let jobs_per_producer = TASKS_PER_BATCH / producers;
    let jobs = jobs_per_producer * producers;
    let target_count = jobs.saturating_mul(cancel_percent).div_ceil(100);
    let mut target_ids = vec![false; jobs];
    let target_seed =
        0x5eed_cafe_u64 ^ workers as u64 ^ ((producers as u64) << 8) ^ ((cancellers as u64) << 16);
    for id in cancellation_order(jobs, target_seed)
        .into_iter()
        .take(target_count)
    {
        target_ids[id] = true;
    }
    let target_ids = Arc::new(target_ids);
    // Keep this race unbounded so every attempted submission reaches the
    // accepted-job accounting; back-pressure/rejection is not under test here.
    let pool = ThreadPool::builder()
        .core_pool_size(workers)
        .maximum_pool_size(workers)
        .prestart_core_threads()
        .build()
        .expect("race pool should start");
    group.throughput(Throughput::Elements(jobs as u64));
    let mut submit_latency = Vec::new();
    let mut cancel_true_latency = Vec::new();
    let mut cancel_false_latency = Vec::new();
    let mut submit_to_run_latency = Vec::new();
    let mut gate_setup_latency = Vec::new();
    let mut gate_release_latency = Vec::new();
    let mut totals = RaceTotals::default();
    group.bench_function(BenchmarkId::new(format!("workers={workers}/producers={producers}/cancellers={cancellers}/cancel={cancel_percent}%/jobs={jobs}"), if gated { "gated" } else { "natural" }), |b| {
        b.iter_custom(|iterations| {
            let barrier = Arc::new(Barrier::new(producers + cancellers + 1));
            let mut producer_commands = Vec::new();
            let mut producer_receivers = Vec::new();
            for _ in 0..producers {
                let (tx, rx) = mpsc::channel::<RaceCommand>();
                producer_commands.push(tx); producer_receivers.push(rx);
            }
            let mut cancel_commands = Vec::new();
            let mut cancel_receivers = Vec::new();
            for _ in 0..cancellers {
                let (tx, rx) = mpsc::channel::<RaceCommand>();
                cancel_commands.push(tx); cancel_receivers.push(rx);
            }
            let (ticket_senders, ticket_receivers): (Vec<_>, Vec<_>) = (0..cancellers).map(|_| mpsc::channel()).unzip();
            let (done_tx, done_rx) = mpsc::channel::<()>();
            let (terminal_tx, terminal_rx) = mpsc::channel::<()>();
            let mut elapsed = Duration::ZERO;
            thread::scope(|scope| {
                for (producer_index, rx) in producer_receivers.into_iter().enumerate() {
                    let batch_barrier = Arc::clone(&barrier);
                    let pool = &pool;
                    let senders = ticket_senders.clone();
                    let done = done_tx.clone();
                    let terminal = terminal_tx.clone();
                    scope.spawn(move || loop {
                        let batch = match rx.recv().unwrap() { RaceCommand::Run(batch) => batch, RaceCommand::Stop => break };
                        batch_barrier.wait();
                        for offset in 0..jobs_per_producer {
                            let id = producer_index * jobs_per_producer + offset;
                            let run_state = Arc::clone(&batch);
                            let cancel_state = Arc::clone(&batch);
                            let run_terminal = terminal.clone();
                            let cancel_terminal = terminal.clone();
                            let (job, ticket) = pool.prepare_cancellable_job(
                                Box::new(|| {}),
                                Box::new(move || {
                                    record_race_terminal(&run_state, id, 1);
                                    run_terminal.send(()).unwrap();
                                }),
                                Box::new(move || {
                                    record_race_terminal(&cancel_state, id, 2);
                                    cancel_terminal.send(()).unwrap();
                                }),
                            );
                            batch.submitted_at_ns[id].store(batch.now_ns(), Ordering::Release);
                            let submitted = Instant::now();
                            pool.submit_job(job).expect("race task should be accepted");
                            let submit_duration = submitted.elapsed();
                            senders[id % cancellers].send(RaceTicket::Ticket(id, ticket)).unwrap();
                            batch.submit_call_ns[id].store(submit_duration.as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
                            batch.accepted.fetch_add(1, Ordering::Relaxed);
                        }
                        for sender in &senders { sender.send(RaceTicket::ProducerDone).unwrap(); }
                        done.send(()).unwrap();
                    });
                }
                for (rx, command_rx) in ticket_receivers.into_iter().zip(cancel_receivers) {
                    let target_ids = Arc::clone(&target_ids);
                    let batch_barrier = Arc::clone(&barrier);
                    let done = done_tx.clone();
                    scope.spawn(move || loop {
                        let batch = match command_rx.recv().unwrap() { RaceCommand::Run(batch) => batch, RaceCommand::Stop => break };
                        batch_barrier.wait();
                        let mut ended = 0;
                        while ended < producers {
                            match rx.recv().unwrap() {
                                RaceTicket::ProducerDone => ended += 1,
                                RaceTicket::Ticket(id, ticket) => {
                                    if target_ids[id] {
                                        batch.cancel_attempts.fetch_add(1, Ordering::Relaxed);
                                        let started = Instant::now();
                                        let won = ticket.cancel_queued();
                                        let cancel_duration = started.elapsed();
                                        batch.cancel_call_ns[id].store(cancel_duration.as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
                                        if won {
                                            batch.cancel_true.fetch_add(1, Ordering::Relaxed);
                                        } else {
                                            batch.cancel_false.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }
                        }
                        done.send(()).unwrap();
                    });
                }
                drop(done_tx); drop(terminal_tx);
                for _ in 0..iterations {
                    let batch = Arc::new(RaceBatch::new(jobs));
                    wait_for_dynamic_idle_workers(&pool, workers);
                    let release = if gated {
                        let (started_tx, started_rx) = mpsc::channel();
                        let (release_tx, release_rx) = mpsc::sync_channel(0);
                        let gate_started = Instant::now();
                        pool.submit(move || { started_tx.send(()).unwrap(); release_rx.recv().unwrap(); Ok::<(), Infallible>(()) }).unwrap();
                        started_rx.recv().unwrap();
                        gate_setup_latency.push(gate_started.elapsed());
                        Some(release_tx)
                    } else { None };
                    for command in &producer_commands { command.send(RaceCommand::Run(Arc::clone(&batch))).unwrap(); }
                    for command in &cancel_commands { command.send(RaceCommand::Run(Arc::clone(&batch))).unwrap(); }
                    let start = Instant::now(); barrier.wait();
                    for _ in 0..producers + cancellers {
                        done_rx.recv_timeout(Duration::from_secs(30)).expect("persistent race actor should finish its batch");
                    }
                    if let Some(release) = release {
                        let gate_started = Instant::now();
                        release.send(()).unwrap();
                        gate_release_latency.push(gate_started.elapsed());
                    }
                    let accepted = batch.accepted.load(Ordering::Acquire);
                    for _ in 0..accepted {
                        terminal_rx.recv_timeout(Duration::from_secs(30)).expect("accepted task should reach one terminal callback");
                    }
                    elapsed += start.elapsed();
                    for id in 0..jobs {
                        let status = batch.terminals[id].load(Ordering::Relaxed);
                        submit_latency.push(Duration::from_nanos(batch.submit_call_ns[id].load(Ordering::Relaxed)));
                        if target_ids[id] {
                            let cancelled = Duration::from_nanos(batch.cancel_call_ns[id].load(Ordering::Relaxed));
                            if status == 2 { cancel_true_latency.push(cancelled); }
                            else if status == 1 { cancel_false_latency.push(cancelled); }
                        }
                        if status == 1 {
                            let submitted = batch.submitted_at_ns[id].load(Ordering::Acquire);
                            let terminal = batch.terminal_at_ns[id].load(Ordering::Relaxed);
                            submit_to_run_latency.push(Duration::from_nanos(terminal.saturating_sub(submitted)));
                        }
                    }
                    assert_eq!(accepted, batch.ran.load(Ordering::Relaxed) + batch.cancelled.load(Ordering::Relaxed),
                        "accepted={accepted} run={} cancel={} true={} false={} attempts={} duplicate={}",
                        batch.ran.load(Ordering::Relaxed), batch.cancelled.load(Ordering::Relaxed),
                        batch.cancel_true.load(Ordering::Relaxed), batch.cancel_false.load(Ordering::Relaxed),
                        batch.cancel_attempts.load(Ordering::Relaxed), batch.duplicate_terminal.load(Ordering::Relaxed));
                    assert_eq!(batch.cancel_true.load(Ordering::Relaxed), batch.cancelled.load(Ordering::Relaxed));
                    assert_eq!(batch.cancel_attempts.load(Ordering::Relaxed), batch.cancel_true.load(Ordering::Relaxed) + batch.cancel_false.load(Ordering::Relaxed));
                    assert_eq!(batch.cancel_attempts.load(Ordering::Relaxed), jobs.saturating_mul(cancel_percent).div_ceil(100));
                    assert_eq!(batch.duplicate_terminal.load(Ordering::Relaxed), 0, "task ID received multiple terminal callbacks");
                    assert!(batch.terminals.iter().all(|id| id.load(Ordering::Relaxed) != 0));
                    while pool.queued_count() != 0 || pool.running_count() != 0 { thread::yield_now(); }
                    let stats = pool.stats();
                    assert_eq!(stats.queued_tasks, 0);
                    assert_eq!(stats.running_tasks, 0);
                    totals.accepted += accepted;
                    totals.ran += batch.ran.load(Ordering::Relaxed);
                    totals.cancelled += batch.cancelled.load(Ordering::Relaxed);
                    totals.attempts += batch.cancel_attempts.load(Ordering::Relaxed);
                    totals.cancel_true += batch.cancel_true.load(Ordering::Relaxed);
                    totals.cancel_false += batch.cancel_false.load(Ordering::Relaxed);
                }
                for command in &producer_commands { command.send(RaceCommand::Stop).unwrap(); }
                for command in &cancel_commands { command.send(RaceCommand::Stop).unwrap(); }
                drop(producer_commands);
                drop(cancel_commands);
            });
            elapsed
        });
    });
    let label = format!(
        "ticket_cancel_race/workers={workers}/producers={producers}/cancellers={cancellers}/cancel={cancel_percent}%/gated={gated}"
    );
    report_latency_samples(&format!("{label}/submit_call"), &submit_latency);
    report_latency_samples(&format!("{label}/cancel_true"), &cancel_true_latency);
    report_latency_samples(&format!("{label}/cancel_false"), &cancel_false_latency);
    report_latency_samples(&format!("{label}/submit_to_run"), &submit_to_run_latency);
    report_latency_samples(&format!("{label}/gate_setup"), &gate_setup_latency);
    report_latency_samples(&format!("{label}/gate_release"), &gate_release_latency);
    eprintln!(
        "TP007 race case={label} queue=unbounded rejected=0 accepted={} run={} cancel_callback={} cancel_attempts={} cancel_true={} cancel_false={} target_rule=deterministic_seeded_shuffle selected=ceil(jobs*percent/100) target_count_per_batch={} jobs_per_batch={} ticket_handoff=immediate_after_submit",
        totals.accepted,
        totals.ran,
        totals.cancelled,
        totals.attempts,
        totals.cancel_true,
        totals.cancel_false,
        target_count,
        jobs
    );
    pool.shutdown();
    wait_for_termination(&pool);
    totals
}
/// Historical control: measures a simultaneous producer burst while workers
/// consume ticketed jobs, with no cancellation attempts.
fn bench_thread_pool_contended_submit_consume(c: &mut Criterion) {
    const WORKERS: usize = 4;
    const PRODUCERS: usize = 4;
    const JOBS_PER_PRODUCER: usize = 64;
    let mut group = c.benchmark_group("thread_pool_ticket_cancellation_contended");
    let pool = ThreadPool::builder()
        .core_pool_size(WORKERS)
        .maximum_pool_size(WORKERS)
        .prestart_core_threads()
        .build()
        .expect("dynamic pool should be created");
    let mut samples = Vec::new();
    group.throughput(Throughput::Elements((PRODUCERS * JOBS_PER_PRODUCER) as u64));
    group.bench_function("producers=4/workers=4/jobs=256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(PRODUCERS + 1));
                let (completion_sender, completion_receiver) = mpsc::channel();
                let started = thread::scope(|scope| {
                    let mut producers = Vec::with_capacity(PRODUCERS);
                    for _ in 0..PRODUCERS {
                        let producer_pool = &pool;
                        let producer_barrier = std::sync::Arc::clone(&barrier);
                        let producer_sender = completion_sender.clone();
                        producers.push(scope.spawn(move || {
                            producer_barrier.wait();
                            for _ in 0..JOBS_PER_PRODUCER {
                                let operation_started = Instant::now();
                                let sample_sender = producer_sender.clone();
                                let (job, _ticket) = producer_pool.prepare_cancellable_job(
                                    Box::new(|| {}),
                                    Box::new(move || {
                                        sample_sender
                                            .send(operation_started.elapsed())
                                            .expect("benchmark should collect operation latency");
                                    }),
                                    Box::new(|| {}),
                                );
                                producer_pool.submit_job(job).expect("job should be accepted");
                            }
                        }));
                    }
                    let started = Instant::now();
                    barrier.wait();
                    for producer in producers {
                        producer.join().expect("producer should finish");
                    }
                    drop(completion_sender);
                    for _ in 0..PRODUCERS * JOBS_PER_PRODUCER {
                        samples.push(completion_receiver.recv().expect("job should be consumed"));
                    }
                    started.elapsed()
                });
                elapsed += started;
            }
            elapsed
        });
    });
    report_latency_samples("ticket_cancel/contended_submit_consume/producers=4/workers=4", &samples);
    group.finish();
    pool.shutdown();
    wait_for_termination(&pool);
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
        bench_thread_pool_idle_wakeup, bench_thread_pool_sparse_multiworker,
        bench_thread_pool_ticket_cancellation, bench_thread_pool_ticket_cancel_race
);
criterion_main!(benches);
