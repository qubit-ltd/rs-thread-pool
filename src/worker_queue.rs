/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! Worker-local queue primitives shared by thread-pool implementations.

use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{
            AtomicBool,
            Ordering,
        },
    },
};

use crossbeam_deque::{
    Injector,
    Steal,
    Stealer,
    Worker,
};

use super::thread_pool::PoolJob;

/// Queue owned by one worker and used for local dispatch plus stealing.
pub(crate) struct WorkerQueue {
    /// Logical worker index used as a stable identity key.
    worker_index: usize,
    /// Cross-thread inbox used by submitters to route work to this worker.
    inbox: Injector<PoolJob>,
    /// Stealer half of the worker-owned local deque.
    stealer: Stealer<PoolJob>,
    /// Whether this queue belongs to a worker that has reached run-loop start.
    active: AtomicBool,
}

impl WorkerQueue {
    /// Creates an empty shared queue handle for one worker.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker owning this queue.
    /// * `stealer` - Read-only stealing handle for the owner-local deque.
    ///
    /// # Returns
    ///
    /// A shared queue handle with an empty cross-thread inbox.
    fn new(worker_index: usize, stealer: Stealer<PoolJob>) -> Self {
        Self {
            worker_index,
            inbox: Injector::new(),
            stealer,
            active: AtomicBool::new(false),
        }
    }

    /// Returns the owning worker index.
    ///
    /// # Returns
    ///
    /// The worker index associated with this queue.
    #[inline]
    pub(crate) fn worker_index(&self) -> usize {
        self.worker_index
    }

    /// Returns whether this queue is currently active.
    ///
    /// # Returns
    ///
    /// `true` when the owning worker has started its run loop.
    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Marks this queue as active after worker run-loop start.
    ///
    /// # Returns
    ///
    /// `true` when this call performed the state transition.
    pub(crate) fn activate(&self) -> bool {
        self.active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Marks this queue as inactive when the worker exits.
    ///
    /// # Returns
    ///
    /// `true` when this call performed the state transition.
    pub(crate) fn deactivate(&self) -> bool {
        self.active
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Appends a job to the worker's cross-thread inbox.
    ///
    /// # Parameters
    ///
    /// * `job` - Job to enqueue.
    pub(crate) fn push_back(&self, job: PoolJob) {
        self.inbox.push(job);
    }

    /// Pops one job from this worker's cross-thread inbox into its local deque.
    ///
    /// # Parameters
    ///
    /// * `local` - Owner-local deque receiving any stolen batch remainder.
    ///
    /// # Returns
    ///
    /// `Some(job)` when the inbox or destination local deque provides a job,
    /// otherwise `None`.
    pub(crate) fn pop_inbox_into(&self, local: &Worker<PoolJob>) -> Option<PoolJob> {
        steal_batch_and_pop(&self.inbox, local)
    }

    /// Steals one job from this worker's local deque or inbox into `dest`.
    ///
    /// # Parameters
    ///
    /// * `dest` - Owner-local deque receiving any stolen batch remainder.
    ///
    /// # Returns
    ///
    /// `Some(job)` when the victim queue provides a job, otherwise `None`.
    pub(crate) fn steal_into(&self, dest: &Worker<PoolJob>) -> Option<PoolJob> {
        steal_batch_and_pop(&self.stealer, dest).or_else(|| steal_batch_and_pop(&self.inbox, dest))
    }

    /// Drains all queued jobs from this queue.
    ///
    /// # Returns
    ///
    /// A vector containing all queued jobs currently visible through this
    /// queue's local stealer and inbox.
    pub(crate) fn drain(&self) -> Vec<PoolJob> {
        let mut jobs = Vec::new();
        while let Some(job) = steal_one(&self.stealer) {
            jobs.push(job);
        }
        while let Some(job) = steal_one(&self.inbox) {
            jobs.push(job);
        }
        jobs
    }
}

/// Worker-owned queue runtime.
///
/// The shared [`WorkerQueue`] can be seen by submitters, shutdown, and thieves,
/// but only the owning worker thread may touch [`Self::local`].
pub(crate) struct WorkerRuntime {
    /// Shared metadata and externally visible inbox for this worker.
    pub(crate) queue: Arc<WorkerQueue>,
    /// Owner-only deque used by the worker for batched and stolen jobs.
    pub(crate) local: Worker<PoolJob>,
    /// Owner-only cursor used to rotate steal victim probing.
    steal_cursor: Cell<usize>,
}

impl WorkerRuntime {
    /// Creates a worker runtime and its shared queue handle.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker owning this runtime.
    /// # Returns
    ///
    /// A runtime whose shared queue handle can be registered for submitters and
    /// thieves while its local deque remains owner-only.
    pub(crate) fn new(worker_index: usize) -> Self {
        let local = Worker::new_fifo();
        let queue = Arc::new(WorkerQueue::new(worker_index, local.stealer()));
        Self {
            queue,
            local,
            steal_cursor: Cell::new(worker_index.wrapping_add(1)),
        }
    }

    /// Returns the owning worker index.
    ///
    /// # Returns
    ///
    /// Stable worker index for this runtime.
    #[inline]
    pub(crate) fn worker_index(&self) -> usize {
        self.queue.worker_index()
    }

    /// Returns the next steal-probing start index for the given queue count.
    ///
    /// # Parameters
    ///
    /// * `queue_count` - Number of currently registered worker queues.
    ///
    /// # Returns
    ///
    /// Start offset for the next victim scan.
    pub(crate) fn next_steal_start(&self, queue_count: usize) -> usize {
        let current = self.steal_cursor.get();
        self.steal_cursor.set(current.wrapping_add(1));
        current % queue_count
    }
}

/// Steals one job with immediate retry on transient contention.
///
/// # Parameters
///
/// * `source` - Queue source to probe.
///
/// # Returns
///
/// `Some(job)` when the source contains a job, otherwise `None`.
pub(crate) fn steal_one<S>(source: &S) -> Option<PoolJob>
where
    S: QueueStealSource,
{
    loop {
        match source.steal_one() {
            Steal::Success(job) => return Some(job),
            Steal::Empty => return None,
            Steal::Retry => continue,
        }
    }
}

/// Steals a batch into `dest` and returns one job.
///
/// # Parameters
///
/// * `source` - Queue source that may provide one or more jobs.
/// * `dest` - Owner-local deque receiving any stolen batch remainder.
///
/// # Returns
///
/// `Some(job)` when the source or destination yields a job, otherwise `None`.
pub(crate) fn steal_batch_and_pop<S>(source: &S, dest: &Worker<PoolJob>) -> Option<PoolJob>
where
    S: QueueStealSource,
{
    loop {
        match source.steal_batch_and_pop(dest) {
            Steal::Success(job) => return Some(job),
            Steal::Empty => return None,
            Steal::Retry => continue,
        }
    }
}

/// Small adapter trait over crossbeam steal sources used by pool queues.
pub(crate) trait QueueStealSource {
    /// Steals one job from this source.
    fn steal_one(&self) -> Steal<PoolJob>;

    /// Steals a batch into `dest` and pops one job from `dest`.
    fn steal_batch_and_pop(&self, dest: &Worker<PoolJob>) -> Steal<PoolJob>;
}

impl QueueStealSource for Injector<PoolJob> {
    #[inline]
    fn steal_one(&self) -> Steal<PoolJob> {
        self.steal()
    }

    #[inline]
    fn steal_batch_and_pop(&self, dest: &Worker<PoolJob>) -> Steal<PoolJob> {
        Injector::steal_batch_and_pop(self, dest)
    }
}

impl QueueStealSource for Stealer<PoolJob> {
    #[inline]
    fn steal_one(&self) -> Steal<PoolJob> {
        self.steal()
    }

    #[inline]
    fn steal_batch_and_pop(&self, dest: &Worker<PoolJob>) -> Steal<PoolJob> {
        Stealer::steal_batch_and_pop(self, dest)
    }
}
