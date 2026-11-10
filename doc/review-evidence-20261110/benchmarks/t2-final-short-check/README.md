# Final short Criterion checks

These checks use the final benchmark shape: every cancellation race batch has 256 accepted tasks; target IDs are selected by a deterministic seeded shuffle with `ceil(256 * target_percent / 100)` targets (10%: 26, 50%: 128, 100%: 256); ticket handoff is in groups of four. The natural and gated race groups provide 36 cases total (workers 1/4, producers 1/4, cancellers 1/2, target 10/50/100; gated cases use one worker). The scale group provides six cases (queue 16384/65536 × front/middle/back).

- `cargo bench --locked --bench thread_pool_bench --no-run` — exit 0.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel_race --test` — exit 0; all 36 race cases reported `Success`. Natural cases reported both run callbacks and failed cancellation attempts; gated cases reported successful cancellation callbacks. Each case asserted accepted = run + cancel callback, true = cancel callback, attempts = true + false, exact selected target count, unique terminal IDs, and final queued/running counts zero.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancellation_scale_position --test` — exit 0; all six scale cases reported `Success`, each restoring queue depth after every timed cancellation.

Raw stdout/stderr are in the adjacent `.log` files; their SHA-256 values are in `SHA256SUMS`. This is a compile/behavior smoke check only, not performance evidence.
