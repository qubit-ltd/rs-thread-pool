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
use std::sync::mpsc::SyncSender;
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
    eprintln!(
        "TP006 latency case={label} phase=criterion_warmup_plus_measurement sample_count={} p50_ns={} p95_ns={} p99_ns={} timer_overhead_sample_count=10000 timer_overhead_p50_ns={} timer_overhead_p95_ns={}",
        samples.len(),
        percentile(50).as_nanos(),
        percentile(95).as_nanos(),
        percentile(99).as_nanos(),
        overhead[overhead.len() / 2].as_nanos(),
        overhead[(overhead.len() - 1) * 95 / 100].as_nanos(),
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

    bench_thread_pool_contended_submit_consume(c);
}

/// Measures a simultaneous producer burst while workers consume ticketed jobs.
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
        bench_thread_pool_ticket_cancellation
);
criterion_main!(benches);
