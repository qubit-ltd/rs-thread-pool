# Balanced-instrumentation short checks

The natural run and cancellation callbacks now share the same terminal helper: one batch-relative clock read, terminal timestamp store, ID status CAS, route counter update, and channel send. The callbacks do not lock or append latency vectors. Producer submit-call and canceller true/false-call durations are collected in actor-local preallocated vectors and transferred after each actor finishes its batch. Submit-to-run percentiles are derived after terminal completion from atomic submitted/terminal timestamps. Timer and vector-recording calibration overhead are reported separately.

- `cargo bench --locked --bench thread_pool_bench --no-run` — exit 0.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel_race --test` — exit 0; 36/36 natural and gated cases. All cases accepted exactly 256 tasks, reported unbounded queue / zero rejects, exact cancellation target count, accounting and terminal ID checks; natural totals include run and false-cancel results, and the gated group records successful cancellations.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancellation_scale_position --test` — exit 0; 6/6 queue-depth/position cases, restoring exact depths 16384 or 65536 after each cancellation.

These are smoke checks, not performance evidence. Complete logs and hashes are adjacent.
