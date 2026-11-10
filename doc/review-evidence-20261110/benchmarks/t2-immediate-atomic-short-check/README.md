# Immediate-handoff, atomic-sample short checks

This revision gives each ticket to its prestarted canceller immediately after the producer's successful `submit_job`. Producers/cancellers record operation durations in per-ID `AtomicU64` arrays after the measured call/handoff; the main thread derives percentile vectors after all terminal events. Both run and cancellation callbacks share a symmetric helper: one batch-relative clock read, one timestamp store, one terminal-ID CAS, one outcome counter increment, and one terminal event send. Each canceller owns its unique receiver directly, with no receiver mutex.

- `cargo bench --locked --bench thread_pool_bench --no-run` — exit 0.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel_race --test` — exit 0; all 36 cases. Every batch accepted 256 tasks. Natural outcomes include run and failed cancellation, while gated cases observe successful cancellation. Accounting verifies accepted = run + cancellation callback, cancellation true = callback, attempts = true + false = rounded target count, exactly one terminal status per task, and zero queued/running stats. Queue is unbounded, rejected count is zero.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancellation_scale_position --test` — exit 0; all six position/depth cases passed with exact restored depth.

Raw logs and SHA-256 values are adjacent. This is smoke evidence only; timed Criterion rounds are not included.
