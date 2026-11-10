# Final short Criterion checks, revision 2

This log set follows static review cleanup: the historical cancellation group remains at queue depths 1/128/4096, while long queue measurements live only in the dedicated position group; race summaries explicitly record the unbounded queue and zero rejected jobs. Each race batch has 256 accepted tasks, with deterministic seeded shuffle targets rounded up to 26/128/256 for 10/50/100 percent. Ticket handoff is in groups of four.

- `cargo bench --locked --bench thread_pool_bench --no-run` — exit 0.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel_race --test` — exit 0; 36 cases. Natural cases observed run callbacks and failed cancels; gated cases observed successful cancellations. Accounting checks assert accepted = run + cancel callback, cancel true = callback, attempts = true + false, attempts = selected targets, unique terminal IDs, and zero queued/running tasks. The queue is unbounded, so rejected = 0.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancellation_scale_position --test` — exit 0; six cases across 16384/65536 × front/middle/back, maintaining exact queue depth after each measured cancellation.

The adjacent logs are complete stdout/stderr. This is behavioral smoke evidence only, not performance evidence. SHA-256 values are in `SHA256SUMS`.
