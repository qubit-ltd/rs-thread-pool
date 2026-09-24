# Qubit Thread Pool Design

[中文版](design.zh_CN.md) · Current design for version 0.10.0.

## Scope

This document describes the current dynamic and fixed OS-thread executors. It
is grounded in the admission, accounting, pool-inner and worker implementations,
and their lifecycle, submission, cancellation and Loom tests. Historical
experiments are not the architecture specification.

## Components

| Component | Responsibility |
| --- | --- |
| `ThreadPool` / `FixedThreadPool` | Public `ExecutorService` facade and lifecycle operations. |
| Pool inner and state | Pool-specific worker policy, monitor, lifecycle and notifications. |
| `AdmissionGate` | One atomic closed bit and in-flight submission count. |
| `PoolAccounting` | Shared slot reservations and queued/running/cancelling/completed counters. |
| `PoolJob` / `PoolJobTicket` | Callback ownership plus an optional one-shot handle for removing a queued dynamic job. |
| Dynamic FIFO / fixed `Injector` | Removable `Arc<QueuedJob>` entries for dynamic workers; the fixed pool keeps its crossbeam injector. |
| Worker loop / hooks | Claim jobs, contain unwinding panics, account completion and retire. |

Neither pool has worker-local queues or private initial-job delivery. Dynamic
and fixed policies stay separate; sharing accounting does not make a universal
scheduler.

## Submission and admission

A submit guard first enters `AdmissionGate`; closing the gate atomically
rejects later entrants while preserving those already inside. Dynamic
submission then holds its state monitor to check `Running`, reserve worker
capacity and spawn any required worker, and reserve a job slot. Fixed
submission reserves a slot for its already-created workers.

Acceptance runs on the submitting thread, outside the state monitor, after
admission and any required worker creation succeed. Rejection invokes none of
accept/run/cancel. An acceptance panic releases the slot and returns
`PoolJobSubmissionError::AcceptancePanicked`; neither run nor cancel executes.
After acceptance succeeds, accounting records submitted and queued work before
the job enters its pool queue. The dynamic queue keeps removable entry identity
for ticketed jobs; fixed-pool publication remains unchanged. Leaving the guard
releases admission even on an error path. `Ok(())` means accepted, not started
or successfully completed.

An accept callback may call `shutdown` on its own pool: shutdown closes
admission without waiting for that callback. It must not call `stop`, `join`
or `wait_termination`, which may wait for the submission itself. Keep all
callbacks short. An admitted dynamic submitter can still be rejected if it
observes shutdown under the monitor before reserving resources.

## Queue capacity and worker growth

Dynamic admission follows this order: grow toward core size (or ensure one
worker when none remain), otherwise reserve a bounded queue slot, otherwise
grow toward maximum size, otherwise return `Saturated`. Worker creation
failure returns `WorkerSpawnFailed` before acceptance and rolls back the
worker reservation. An unbounded queue prevents queue-pressure growth above
core size; a zero-core pool still creates a worker to make progress.

The configured capacity limits ordinary waiting-slot admission, not total
accepted jobs or memory. A successful worker-growth path reserves a handoff
slot independently of that limit, then publishes to the dynamic FIFO; that job
is not tied to the new worker. Slots include accepted or provisional queued
work and cancellation callbacks until their completion. Running jobs release
their slots at claim. `queued_count()` therefore need not equal the number of
reserved slots.

Fixed pools prestart their configured workers and never grow. Dynamic idle
workers may retire according to keep-alive, core timeout and maximum-size
changes. The last timed worker does not retire while a submitter may still
publish a job. Runtime core-size changes affect later admission and explicit
prestart operations; they do not eagerly start workers for existing queues.

## Worker claim and cancellation

Fixed workers steal from the fixed pool's injector, retrying contention;
dynamic workers take the next entry from their removable FIFO. Workers check
`stop_now` before and again at the claim decision. If stop is observed at that
decision, the worker cancels the job; otherwise it claims running ownership and
runs it. A stop racing after this decision cannot revoke the running job, even
if user code has not yet begun.

Stop also drains visible queued jobs after in-flight submitters leave. A
dynamic `PoolJobTicket::cancel_queued` competes for the same queued ownership:
it removes the entry if a worker or stop has not claimed it. Ownership is
consumed by exactly one run or cancel path. Ticket or stop cancellation may
run on the cancelling caller, submitting thread, stop caller or a worker,
depending on the race. Cancellation retains its slot until the callback and
captured-value destruction finish, including contained unwinding panics.

## Lifecycle state machine

```text
Running --shutdown--> ShuttingDown --drained/workers exited--> Terminated
Running --stop------> Stopping -----cancel/drain/workers exited--> Terminated
ShuttingDown --stop-> Stopping
```

Shutdown closes admission, changes lifecycle under the monitor and wakes
workers; it does not wait for submissions or accepted work. Stop closes
admission, publishes the stop flag, waits for in-flight submissions, drains
queued jobs and executes their cancellation callbacks outside the monitor.
It does not forcibly interrupt running jobs and is not a termination barrier.
Repeated lifecycle requests cannot reopen admission.

Termination is observed when lifecycle is no longer `Running`, no workers
remain registered and accounting is idle. `wait_termination` waits for this
condition. `join` waits only for accounting to become idle and does not
close admission or require idle workers to exit. Concurrent submissions can
extend or invalidate an idle observation. These waits must not be made by a
task on the same pool: its own running count prevents it from becoming idle.

## Accounting invariants

- Admission counts every submitter that may publish or roll back a reservation.
- Every accepted job has one terminal accounting outcome: completed after the
  worker run path, or cancelled after the cancellation callback.
- A slot is released exactly once: on acceptance rollback, on running claim,
  or after cancellation finishes. Queued-to-running transfer publishes running
  ownership before releasing the slot, preventing an idle gap.
- At quiescence, `submitted = completed + cancelled`; during a stable
  intermediate state, outstanding jobs belong to queued, running or cancelling
  ownership. Counter updates are separate atomic operations, so arbitrary
  concurrent snapshots need not satisfy cross-field sums.
- Idle requires zero in-flight submissions, reserved slots, running jobs and
  cancellation callbacks. Termination additionally requires closed lifecycle
  and no registered workers.

A contained task error or panic still finishes worker accounting;
`completed_tasks` is not a count of successful business results. A tracked
task cancelled through its handle may still pass through worker accounting;
`cancelled_tasks` counts accepted jobs cancelled while queued, whether by a
ticket or immediate shutdown.

## Waiting and notification

Each pool owns its monitor and wait predicates. Workers register as idle,
check queue state and pending wake tokens, then park. Submitters request at
most one uncovered idle-worker wake with an atomic token and notify under the
monitor; workers consume tokens on leaving idle. This closes the window
between declaring idleness and parking.

Submit/idle waiter counters and final-submitter departure notifications allow
stop, join and termination waiters to recheck predicates after atomic state
changes. Waits always recheck their conditions; a notification is not evidence
that work has finished. Cancellation completion participates in these wakes.

## Panic containment

Acceptance, custom run/cancel callbacks, ordinary task execution and hooks
contain unwinding panics at their respective boundaries. Acceptance failure
is a submission error; it does not execute the cancellation callback.
Hooks run on worker threads with stable worker indices and their panics are
ignored. Neither containment nor cancellation can recover from process abort
or forcibly interrupt a blocked task. Callbacks and hooks must avoid waiting
on work whose progress depends on themselves.

## Monitoring snapshots

`ThreadPoolStats` combines monitor-protected lifecycle/worker data with
independently loaded task counters. `StopReport` includes best-effort
observations, including worker-side cancellation racing with stop. Neither
object is an atomic transaction, a strict ordering guarantee or a completion
barrier. Use lifecycle waits for synchronization, and task handles for results.

## Downstream extension points

Use `ExecutorService` for runnable, callable and tracked submission.
`ThreadPool::prepare_cancellable_job` returns an unsubmitted `PoolJob` and a
cloneable `PoolJobTicket`; after submitting that job to its originating dynamic
pool, `cancel_queued()` returns `true` only if that call takes queued ownership.
It returns `false` before acceptance, after rejection, or if another caller,
worker or stop has taken the job. A successful cancellation finishes the
cancel callback, releases its captures and queue slot, and updates accounting
before returning. It waits for acceptance in progress without holding pool
locks; if acceptance succeeds, the submitter performs cancellation before
publishing the entry. All callbacks execute outside pool and queue locks.
The ticket does not interrupt a worker-claimed task and does not apply to
`FixedThreadPool`. For other downstream work,
`ThreadPool::submit_job` plus `PoolJob::with_accept` allows registries to
publish acceptance and choose their own run/cancel bookkeeping.
Match `PoolJobSubmissionError::Rejected(SubmissionError)` separately from
`AcceptancePanicked`; an acceptance callback that mutates external state
before panicking must arrange its own recovery.

## Non-goals

This crate does not provide async-future scheduling, deadlines, priorities,
worker-local work stealing, strict FIFO execution, forced thread cancellation
or a guarantee that one pool wins every benchmark. See the
[user guide](user_guide.md), [performance notes](performance.md) and
[migration guide](migration_0.9_to_0.10.md).
