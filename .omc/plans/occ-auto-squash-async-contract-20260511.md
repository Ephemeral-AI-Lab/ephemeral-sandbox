# OCC Auto-Squash Async Maintenance Contract

Generated: 2026-05-11
Parent plan: `.omc/plans/occ-auto-squash-optimization-experiment-plan-20260511.md`
Status: Opt-in only. Default runtime behavior remains `EOS_OCC_SQUASH_MODE=sync`.

## Contract

`EOS_OCC_SQUASH_MODE=async` treats layer-stack squash as maintenance after a
successful OCC publish:

1. The originating tool call succeeds once its layer is published. A later
   squash failure does not rewrite that tool result into an error.
2. Squash failures are recorded as structured maintenance records on the
   `OccService` and exposed through `api.layer_metrics.auto_squash`.
3. Teardown/reset callers can drain pending maintenance with a 10 s default
   budget via `OccService.drain_auto_squash_maintenance(...)`.
4. Snapshot leases keep the same semantics as sync mode. Squash may rewrite the
   active manifest, but leased manifests remain readable and garbage collection
   remains responsible for protecting pinned layers.
5. Concurrent commits still publish through the same `OccSerialMerger` and
   `LayerStackManager.squash(...)` CAS-safe manifest rewrite path. Async mode
   only changes when the post-publish squash runs.
6. Public tool payload fields keep their current meaning: `status`,
   `changed_paths`, `conflict_reason`, `bytes_written`, and related fields
   still describe the OCC publish result, not background maintenance.
7. Promotion to default requires the scenario-based behavior gate and perf gate
   in `.omc/plans/occ-auto-squash-perf-verification-test-plan-20260511.md`.

## Failure Visibility

The first shipped visibility surface is `api.layer_metrics.auto_squash`:

- `mode`
- `max_depth`
- `queue_depth`
- `maintenance_errors`
- `last_maintenance_error`

This is intentionally diagnostic rather than user-facing. A future promotion PR
must define escalation policy for repeated maintenance errors before async mode
can become the default.
