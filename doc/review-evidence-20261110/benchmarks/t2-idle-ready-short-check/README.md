# Idle-ready race short checks

Each race batch waits for all configured prestarted pool workers to be idle before gate setup or actor release; this is outside the measured interval. Submission timestamps are stored before the Instant bracket around `submit_job`; submit-call duration is stored after immediate ticket handoff. Cancellation-call duration is atomically stored after return. The main thread derives latency vectors after the timed terminal checkpoint. Run/cancel callbacks share symmetric AtomicU64 timestamp, terminal CAS, counter, and event work; cancellation receivers are unique and used without mutexes.

- `cargo bench --locked --bench thread_pool_bench --no-run` — exit 0.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel_race --test` — exit 0; 36 cases, 256 accepted tasks each, natural win/loss and gated success outcomes observed.
- `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancellation_scale_position --test` — exit 0; six 16384/65536 front/middle/back cases passed.

Logs and hashes are adjacent. Smoke checks only, not performance evidence.
