# Follow-up: provider adapter dict-shape divergence

The `ProviderAdapter` protocol claims a "canonical dict" shape but lets
implementations diverge on the state key and vocabulary:

| adapter | key      | vocabulary                                                       |
|---------|----------|------------------------------------------------------------------|
| Daytona | `state`  | `started` / `stopped` / `pending_build` / `build_failed` / `error` |
| Docker  | `status` | `created` / `running` / `paused` / `restarting` / `removing` / `exited` / `dead` |

The sweevo benchmark used to patch around this locally. After the
`task_center_runner.benchmarks.sweevo` refactor (see
`docs/plans/sweevo_layerstack_migration_PLAN.md`), the benchmark is docker-only
and reads `status` directly with docker vocabulary. Daytona-shaped responses
are no longer handled here.

The correct long-term fix is normalizing both adapters at the protocol
boundary — pick one canonical key (recommend `state`) and one vocabulary,
then translate inside each adapter's `_serialize_container` helper. That
unblocks any future multi-provider benchmark consumers.

**Out of scope for this refactor:** shared infra with multiple consumers
(`live_e2e_test`, internal scenarios). File a separate ADR.
