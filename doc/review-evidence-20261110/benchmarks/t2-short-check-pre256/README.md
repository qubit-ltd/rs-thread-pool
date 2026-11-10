# Preliminary short benchmark checks (superseded)

These are complete Criterion `--test` logs from the first implementation pass, before the race batch was corrected to the plan minimum of 256 tasks and deterministic target rounding was changed. They are retained for audit, but are not measurement evidence and are not used for conclusions. Final short checks will be stored in a separate directory after T3 clears the CPU window.

- Race command: `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel_race --test` (exit 0; 36 cases).
- Scale command: `timeout 120s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancellation_scale_position --test` (exit 0; six depth/position cases).
- The race logs used 64 tasks for one producer and 256 tasks for four producers; its early modulo targeting rounded poorly. The current implementation uses exactly 256 total tasks in every case and selects `ceil(jobs * target_percent / 100)` IDs using a scaled deterministic threshold.
