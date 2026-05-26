# Phase 2 — Runner Puller, Emitters, Plugin & Background Instrumentation

> **Prerequisites:** Read [`README.md`](README.md) first — it owns the
> cross-cutting contracts (schema, dual-write, lane assignment, sampling rule,
> cadence policy, disk contract, daemon-restart epoch handling).
> Read [`phase-1-audit-buffer-and-pull-rpc.md`](phase-1-audit-buffer-and-pull-rpc.md) — Phase 2 consumes the ring, pull RPC,
> and frozen schema produced there.

## Goal

Wire daemon emitters across all subsystems; stand up the runner-side puller with adaptive cadence + floor enforcement; instrument generic plugin + background tool surfaces; persist normalized events into rotated + gzipped `sandbox_events.jsonl`.

Phase 2 is the largest review surface in V3. To keep PRs reviewable, emitters land **one PR per subsystem**; the puller and normalizer can ride in either the first or last subsystem PR.

## Deliverables

### 1. New file — `backend/src/task_center_runner/audit/daemon_pull.py`

`DaemonAuditPuller` with cursor state, adaptive interval policy with floor enforcement, final-drain on stop.

- Stats published to performance report:
  - `pull_count`, `empty_pull_count`, `events_pulled`, `pull_error_count`
  - `dropped_event_count`, `lost_before_seq`
  - `max_buffer_pressure`
  - `final_cursor`
  - `floor_raises` (count of times the cadence floor escalated)
  - `pull_ms` (p50/p95/p99)
  - `daemon_restarts_observed` (epoch-boundary counter)
- Floor: `EOS_DAEMON_AUDIT_PULL_FLOOR_MS` (default 100 ms); pressure-based escalation per [README §Adaptive cadence policy](README.md#adaptive-cadence-policy-with-floor-enforcement).
- Never blocks the main run on transient pull failures; logs error and continues at next interval.

> **Inherited from Phase 1.** The daemon-side `api.audit.reset_floor` handler
> shipped in Phase 1 is a gated stub (env-checked, returns success). Actual
> floor-state mutation lives on the puller and is owned by this phase; the
> daemon handler stays a no-op since the floor is a runner-side concern. See
> [phase-1 Status §Deferred](phase-1-audit-buffer-and-pull-rpc.md#deferred-not-in-phase-1).

### 2. Daemon emitters (one PR per subsystem)

Each subsystem PR is independently mergeable to keep the review surface small.

#### `layer_stack`
Instrument `backend/src/sandbox/daemon/layer_stack_runtime.py` to emit the lease/lock/squash family ([schema in Phase 1](phase-1-audit-buffer-and-pull-rpc.md#layerstack--leaselocksquash-family)).

#### `overlay_workspace` (ephemeral)
Instrument:
- `backend/src/sandbox/overlay/{lifecycle,handle,namespace_runner}.py`
- `backend/src/sandbox/ephemeral_workspace/pipeline.py`

Stamp `workspace_mode="ephemeral"`. Emit `overlay_workspace.{mounted,published,cleaned,cleanup_failed}` on the [`critical` lane](README.md#lane-assignment).

#### `isolated_workspace`
Instrument:
- `backend/src/sandbox/isolated_workspace/pipeline.py`
- `backend/src/sandbox/isolated_workspace/_control_plane/{pipeline_registry,pipeline_state,orphan_reaper,workspace_handle_lifecycle,linux_runtime}.py`

Emit the full lifecycle family from Phase 1 schema; stamp `workspace_mode="isolated"`.

> Note: V2 said `manager.py` — that path does not exist. Control plane actually lives under `_control_plane/`.

#### `occ`
Instrument:
- `backend/src/sandbox/occ/service.py`

Emit the changeset transaction family ([schema in Phase 1](phase-1-audit-buffer-and-pull-rpc.md#occ--changeset-transaction-family)).

> Plan-doc correction (Phase 3 deferral D16): V3's initial slice notes
> referenced `sandbox/daemon/occ_runtime_services.py` and
> `sandbox/daemon/changeset_projection.py`. The OCC emitters actually
> live in `sandbox/occ/service.py` (the OCC service module). The earlier
> file names never existed on `main`.

#### `os_resource`
Extend existing command-execution resource metrics to emit `os_resource.sampled` on the existing sampler tick (no new sampler).

> **Inherited from Phase 1.** Phase 1 deferred the `os_resource.sampled` smoke
> emitter because no periodic sampler exists in the daemon today (Phase 1's
> revertability contract forbids new threads/timers). `OsResourceSection` is
> pre-defined in `backend/src/sandbox/daemon/audit_schema.py`; this phase must
> either (a) reuse an existing periodic tick (e.g. the command-exec resource
> metrics path or the background-tool 60 s heartbeat) by piggybacking the emit,
> or (b) explicitly own the introduction of a single new sampler timer and
> account for its cost in the overhead budget. Option (a) is preferred and
> matches the "zero new threads" principle echoed for the background-tool
> heartbeat in §4. See [phase-1 Status §Deferred](phase-1-audit-buffer-and-pull-rpc.md#deferred-not-in-phase-1).

### 3. Generic plugin instrumentation in `backend/src/plugins/core/loader.py`

- Wrap plugin-tool dispatch in a thin emitter shim that fires `plugin.tool_invoked` before and `plugin.tool_completed` after.
- Emit `plugin.error` on exception.
- **No code in `backend/src/plugins/catalog/lsp/` learns about audit.** Future plugins (formatters, indexers, MCP bridges) inherit instrumentation for free because it lives in `plugins/core/`.
- `plugin.peak_resident_sampled` emitted on the existing OS resource sampler tick when a plugin process is identified.

This is the central enforcement point for [requirement 2 (generic plugin)](README.md#requirement-traceability) — no schema or implementation knowledge of LSP/Pyright leaks into the audit surface.

### 4. Background tool instrumentation in `backend/src/engine/background/task_supervisor.py`

- Emit `background_tool.{started,completed,failed,cancelled,delivered}` from `_apply_terminal_status_transition` transitions and from the `collect_completed` path.
- Emit `background_tool.heartbeat` on each existing heartbeat tick (60 s).
- **Zero new threads.** The existing `_heartbeat_loop` is the only timer touched.

### 5. Per-tool phase emitters in `backend/src/engine/tool_call/dispatch.py`

- Always emit `tool_call.started` + `tool_call.finished` (envelope) on the `normal` lane.
- Emit `tool_call.phase` per phase, subject to the per-tool sampling rule (see [README §Per-tool phase sampling rule](README.md#per-tool-phase-sampling-rule-slow-tail-buffered-flush)).
- `tool_call.finished.phase_totals_rollup` populated from in-process timers (NOT dependent on phase event emission).
- Per-`tool_name` rolling-window of last 100 `total_ms` values protected by a per-`tool_name` lock; critical section is O(1) under a fixed-size deque.

### 6. Normalizer

The normalization responsibility is split across two co-located modules
(Phase 3 deferral D16 — the original V2 plan mentioned a single
`task_center_runner/audit/sandbox_events.py` module, but slice 1 split
it):

- `backend/src/task_center_runner/audit/daemon_event_normalizer.py` —
  reshape pulled events; only writer of `payload["daemon_event"]`.
- `backend/src/task_center_runner/audit/sandbox_events_sink.py` —
  rotating + gzip JSONL writer for `sandbox_events.jsonl`.

Behaviour (unchanged from V2):

- Promote subsystem sections to `payload["<section>"]` — **always** (consumer surface, what the report builder reads).
- Preserve raw event under `payload["daemon_event"]` — **only when `EOS_AUDIT_FORENSIC_RAW_ENABLED=true`** (default off; forensic-only, never read by automated consumers).
- The normalizer is the **only file** allowed to write `payload["daemon_event"]`; a CI lint rule (added in this phase) enforces the boundary.
- Dedupe stream + pull by `seq` then `(operation_id, event, operation_step, tool_id)`.
- When both stream-derived and pull-derived events match: **pull is authoritative** (richer timing/resource fields).
- Carry `boot_epoch_id` through; on epoch boundary observed, write a synthetic `daemon.restart_observed` event with `previous_epoch_id`, `new_epoch_id` to preserve the timeline.

### 7. `sandbox_events.jsonl` writer gains rotation + gzip

- Rotate at 64 MiB live file.
- Gzip on rotation (background thread, bounded queue depth = 2).
- Retain `EOS_AUDIT_ARTIFACT_RETENTION_FILES` (default 8) historical compressed files per run.
- All files (live + rotated) live under the EOS_TIER_RUN_ID-stable artifact path.

**Reader compatibility:** `performance_report.py:_iter_jsonl` currently reads `sandbox_events.jsonl` as a single file. Phase 2 extends it to concatenate the live file with `sandbox_events.jsonl.<N>.gz` historical files in ascending N order. Tested by `test_iter_jsonl_concatenates_rotated_gzipped_history`.

## Tests

- `test_puller_final_drain_before_recorder_dispose` — recorder dispose blocks until puller drained; assert no events left in ring.
- `test_puller_never_blocks_tool_dispatch` — inject 250 ms pull stall; assert tool latency unchanged.
- `test_puller_floor_raises_under_sustained_pressure` — simulate 5 consecutive pulls with `pressure > 0.8`; assert floor raised by 50 % within the 3-pull threshold.
- `test_puller_floor_never_lowers_automatically` — after escalation, set pressure to 0; assert floor stays raised.
- `test_puller_reset_floor_op_works_when_authorized` — call `api.audit.reset_floor` with env enabled; floor returns to default.
- `test_plugin_events_are_kind_generic` — register a fake `plugin_kind="indexer"` plugin via `plugins/core/loader.py`; assert it emits the same event family as the LSP plugin with NO LSP-specific keys. Grep the emitted JSON for `"lsp"` and `"pyright"` as keys → must be 0 hits (occurrences as `plugin_id` values are OK).
- `test_background_tool_lifecycle_emits_full_lattice` — three runs: RUNNING → COMPLETED → DELIVERED, RUNNING → FAILED → DELIVERED, RUNNING → CANCELLED → DELIVERED; assert exactly one terminal event per run.
- `test_background_tool_heartbeat_reuses_existing_timer` — assert no new threads spawned; assert heartbeat events arrive on the existing `EOS_BACKGROUND_HEARTBEAT_INTERVAL_S` cadence.
- `test_isolated_workspace_orphan_check_after_exit` — kill holder mid-run; assert `orphan_holder_count > 0` in pulled events and in `isolated_workspace.exited` payload.
- `test_sandbox_events_jsonl_rotates_at_64mib_and_caps_history` — synthetic 1 M-event run; assert exactly N rotated files; assert live file ≤ 64 MiB.
- `test_sandbox_events_jsonl_rotation_path_stable_under_eos_tier_run_id` — start run with `EOS_TIER_RUN_ID=test-xyz`; restart mid-run; assert rotated files at the same paths.
- `test_iter_jsonl_concatenates_rotated_gzipped_history` — write 3 rotated `.gz` files plus a live `.jsonl`; assert `_iter_jsonl` returns events from all 4 sources in ascending seq order.
- `test_tool_call_phase_slow_tail_flush` — emit 200 invocations of `smoke_tool` with deterministic `total_ms` from a fixture (e.g., `[10ms × 190, 500ms × 10]`); assert (a) first 100 calls always flush all 6 phases (cold window); (b) of remaining 100 calls, the 5 with `total_ms ≥ P95` flush all phases; (c) the other 95 flush no phase events but DO emit `tool_call.finished` with populated `phase_totals_rollup`.
- `test_tool_call_finished_rollup_present_when_phases_discarded` — fast-tail call (total_ms below P95 in the rolling window); assert `tool_call.finished.phase_totals_rollup` populated with all 6 phase keys.
- `test_dedupe_pull_supersedes_stream_when_both_present` — emit same logical event via both paths; assert consumer sees the pull version (richer fields).
- `test_no_consumer_reads_daemon_event_under_default_config` — full mock suite with `EOS_AUDIT_FORENSIC_RAW_ENABLED` unset; assert `daemon_event` key absent from every recorded payload.
- `test_forensic_raw_present_when_env_enabled` — same suite with `EOS_AUDIT_FORENSIC_RAW_ENABLED=true`; assert `daemon_event` key present and structurally equal to source.
- `test_daemon_event_writer_module_boundary` — CI-grade grep: any file outside `task_center_runner/audit/daemon_event_normalizer.py` (and test files) referencing `payload["daemon_event"]` / `payload.get("daemon_event")` → fail.
- `test_daemon_restart_epoch_handled_by_puller` — simulate `boot_epoch_id` change between pulls; assert puller resets cursor, increments `daemon_restarts_observed`, writes a synthetic `daemon.restart_observed` event.

## Acceptance criteria

- All tests above pass under `.venv/bin/pytest`.
- Mock-suite end-to-end run produces `sandbox_events.jsonl` with all subsystem sections populated (verified by jq query).
- Rotation kicks in correctly on a synthetic 100 MiB run; gzip succeeds; retention cap holds at 8 files.
- `dropped_event_count == 0` and `lost_before_seq == 0` on the full mock suite.
- No new threads created in `task_supervisor.py` (verified by thread count diff before/after).

## Status (2026-05-26)

**Status: FOUNDATION SLICE IMPLEMENTED.** Per the plan's "one PR per
subsystem" guidance, Phase 2 ships as a sequence of mergeable slices. The
first slice lands the pipeline end-to-end with `layer_stack` as the proof
subsystem; the remaining 4 subsystem emitters, plugin/background-tool
instrumentation, and dispatcher slow-tail flush land in subsequent slices.

### Delivered (slice 1 — foundation)
- `backend/src/task_center_runner/audit/daemon_pull.py` — `DaemonAuditPuller`
  with cursor state, adaptive cadence (active/idle/isolated/pressure
  targets), floor enforcement (`EOS_DAEMON_AUDIT_PULL_FLOOR_MS`, default
  100 ms), pressure-based floor escalation (`pressure > 0.8` sustained for
  3 pulls → 1.5× raise, cap 1000 ms), operator-only `reset_floor()`,
  daemon-restart epoch handling (cursor reset + synthetic
  `daemon.restart_observed` emit), final drain on stop (3 s cap), and
  `PullerStats` covering all required counters from §1.
- `backend/src/task_center_runner/audit/daemon_event_normalizer.py` — the
  **sole writer** of `payload["daemon_event"]`, env-gated by
  `EOS_AUDIT_FORENSIC_RAW_ENABLED`. Promotes subsystem sections to
  `payload[<section>]` unconditionally; preserves raw under
  `payload["daemon_event"]` only when forensic raw is enabled. Dedupe key:
  `seq` first, else `(event_type, operation_id, operation_step, tool_id)`.
  `merge_streams()` makes pull authoritative on overlap.
- `backend/src/task_center_runner/audit/sandbox_events_sink.py` — synchronous
  rotating + gzipping JSONL sink. 64 MiB live cap, gzip on rotation,
  `EOS_AUDIT_ARTIFACT_RETENTION_FILES` (default 8) historical files.
  `iter_rotated_jsonl()` concatenates `.<N>.gz` history in ascending order
  with the live file. (Synchronous rotation chosen over the V3 prescription
  of a background thread + bounded queue — at our event rate the 64 MiB
  roll is rare and a 64 MiB gzip completes in well under a second; the
  background-thread design is reserved for slice 2 if the synchronous path
  shows up on a heavy-run profile.)
- `backend/src/task_center_runner/audit/performance_report.py:_iter_jsonl` —
  rewired to use `iter_rotated_jsonl()` so the report reader transparently
  consumes rotated history.
- `backend/src/sandbox/daemon/audit_schema.py` — `LayerStackSection`
  dataclass + `build_layer_stack_event()` helper for typed emitter
  construction.
- `backend/src/sandbox/daemon/layer_stack_runtime.py` — emits
  `layer_stack.{lease_requested, lease_acquired, snapshot_prepared,
  lease_released}` on the `normal` lane through `prepare_workspace_snapshot`
  / `release_lease`. `emit_squash_event()` helper emits the critical-lane
  `layer_stack.squash_{triggered,completed,failed}` family. Per-lease
  start-time bookkeeping lives in `_LEASE_TIMELINE` (cleared by the existing
  test-cache reset).
- Tests:
  - `test_daemon_pull.py` — final drain, floor escalation, floor never
    auto-lowers, `reset_floor()` returns to default, daemon-restart epoch
    handling, transient pull failures do not block (6 tests).
  - `test_sandbox_events_sink.py` — rotates + caps history, rotated-gz
    concatenation under `_iter_jsonl`, EOS_TIER_RUN_ID artifact stability,
    JSONL round trip (4 tests).
  - `test_daemon_event_normalizer.py` — forensic raw absent under default
    config, present when env enabled, pull-supersedes-stream dedupe, and
    **`test_daemon_event_writer_module_boundary`** (CI lint enforcing the
    module boundary across `backend/src/`).
  - `test_layer_stack_emitters.py` — `squash_{triggered,completed,failed}`
    land on the `critical` lane with the causal-chain identifiers populated.
- All 174 tests under `backend/tests/unit_test/test_sandbox/test_daemon/`
  and `backend/tests/unit_test/test_task_center_runner/` pass under
  `.venv/bin/pytest`; `.venv/bin/ruff check` clean on touched files.

### Deferred to subsequent slices

These ride in follow-up PRs to keep slice 1 reviewable; the foundation now
exists so each can drop in without further plumbing.

1. **Subsystem emitters** beyond `layer_stack`:
   - `overlay_workspace` in `sandbox/overlay/{lifecycle,handle,namespace_runner}.py`
     + `sandbox/ephemeral_workspace/pipeline.py` (stamp
     `workspace_mode="ephemeral"`).
   - `isolated_workspace` in `sandbox/isolated_workspace/pipeline.py` plus
     `_control_plane/*` (full lifecycle family, `workspace_mode="isolated"`).
   - `occ` in `sandbox/occ/service.py` (Phase 3 deferral D16 — the
     V2 plan named two non-existent daemon files).
   - `os_resource.sampled` piggybacked on the existing command-execution
     resource-metrics tick (no new thread — option (a) in §2 of the plan).
2. **Generic plugin instrumentation** in `backend/src/plugins/core/loader.py`
   (`plugin.tool_invoked/completed/error/peak_resident_sampled`).
3. **Background tool instrumentation** in
   `backend/src/engine/background/task_supervisor.py`
   (`background_tool.{started,completed,failed,cancelled,delivered,heartbeat}`).
4. **Per-tool phase emitters** in `backend/src/engine/tool_call/dispatch.py`
   (slow-tail buffered flush — cold window + P95 from a per-`tool_name`
   rolling deque; envelope `tool_call.{started,finished}` always emitted on
   `normal` lane; `phase_totals_rollup` populated from in-process timers).
5. **Puller-to-recorder wiring** — slice 1 ships the puller as a library
   ready for integration; hooking it into `AuditRecorder.start()` and
   `dispose()` is its own slice so reviewers can audit the lifecycle in
   isolation.
6. **Tests cross-referenced from §Tests above that depend on the deferred
   pieces** (e.g. `test_plugin_events_are_kind_generic`,
   `test_background_tool_lifecycle_emits_full_lattice`,
   `test_tool_call_phase_slow_tail_flush`,
   `test_isolated_workspace_orphan_check_after_exit`,
   `test_puller_never_blocks_tool_dispatch`) ship alongside the
   corresponding subsystem slice.
7. **Per-PR follow-ups already filed by Phase 1**:
   - Stream-bridge removal after K=5 clean heavy runs.
   - Real plugin session model (`plugin.session_*`).

## What this phase does NOT do

- Does NOT render the performance report. Report rendering + release gates + default-on rollout are [Phase 3](phase-3-report-and-release-gates.md).
- Does NOT remove the stream-bridge fallback. Per the [Stream-bridge fallback sunset](README.md#stream-bridge-fallback-sunset) policy, retirement is a follow-up after K=5 clean heavy runs.
- Does NOT introduce a real plugin session lifecycle. Per ADR follow-up #2, that is a separate plan.
