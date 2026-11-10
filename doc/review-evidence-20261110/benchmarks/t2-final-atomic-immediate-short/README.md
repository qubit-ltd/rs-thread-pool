# Final atomic, immediate-handoff short checks

This revision brackets submit-call latency with `Instant::now()` immediately around `pool.submit_job`; the batch-relative submitted timestamp is captured immediately before that bracket. Each ticket is handed to its prestarted canceller immediately after submit returns. Submit/cancel call duration values are stored to per-ID AtomicU64 slots after the measured call/handoff and are converted into percentile vectors by the main thread after the timer stops. The calibration log reports pure timer and atomic clock/store recording overhead. Both run and cancel callbacks share one terminal helper and perform symmetric terminal accounting.

- `cargo bench --locked --bench thread_pool_bench --no-run` — exit 0.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel_race --test` — exit 0; 36 natural/gated cases, 256 accepted tasks each, exact deterministic target counts (26/128/256), all accounting and terminal-state assertions pass. The natural matrix records run and failed cancels; the gated matrix records successful cancels. All race cases are unbounded and report rejected=0.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancellation_scale_position --test` — exit 0; six position/depth cases (16384/65536 × front/middle/back) pass with exact depth restoration.

Adjacent `.log` files preserve complete stdout/stderr; `SHA256SUMS` records their hashes. These are smoke checks only.
