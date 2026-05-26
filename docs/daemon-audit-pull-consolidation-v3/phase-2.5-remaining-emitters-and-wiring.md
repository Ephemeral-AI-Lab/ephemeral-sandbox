# Phase 2.5 — Remaining Emitters, Instrumentation, and Wiring

> **Prerequisites:** Read [`README.md`](README.md) first for cross-cutting
> contracts. Read [`phase-1-audit-buffer-and-pull-rpc.md`](phase-1-audit-buffer-and-pull-rpc.md)
> for the ring + RPC contract. Read [`phase-2-emitters-and-puller.md`](phase-2-emitters-and-puller.md)
> and the slice-1 report
> [`phase-2-slice-1-report.md`](phase-2-slice-1-report.md) for what is
> already shipped.

## Status (2026-05-26)

Slices 1–6 (`overlay_workspace`, `isolated_workspace`, `occ`,
`os_resource.sampled`, generic plugin shim, background tool emitters +
puller-to-recorder wiring surface) shipped. See
[`phase-2.5-implementation-report.md`](phase-2.5-implementation-report.md)
for what landed, contracts honored, and deferred items. Slices 7
(per-tool phase slow-tail flush) and 8 (end-to-end heavy-run regression)
remain open; both carry their own non-trivial design surface and ship
independently per the plan's "one PR per slice" guidance.

## Goal

Close out the Phase 2 deliverables that were deferred from slice 1. When
Phase 2.5 lands, the overall Phase 2 goal is achieved: every subsystem
emits its event family into the daemon ring; the runner-side puller
drains it into the rotating JSONL sink; and the dispatcher contributes
per-tool envelope + slow-tail phase events. After Phase 2.5, **Phase 3
(report rendering + release gates) is the only remaining work for V3.**

## What Phase 2.5 finishes (mapped to Phase 2 deliverables)

| Phase 2 deliverable | Slice 1 status | Phase 2.5 closes |
|---|---|---|
| 1. `DaemonAuditPuller` library | ✅ shipped | wiring into `AuditRecorder` (slice 6) |
| 2. Daemon emitters (5 subsystems) | `layer_stack` ✅ | `overlay_workspace`, `isolated_workspace`, `occ`, `os_resource` (slices 1–4) |
| 3. Generic plugin shim | deferred | slice 5 |
| 4. Background tool instrumentation | deferred | slice 6 (shares the recorder-wiring slice) |
| 5. Per-tool phase emitters | deferred | slice 7 |
| 6. Normalizer (dedupe + epoch) | ✅ shipped | n/a |
| 7. Rotation + gzip | ✅ shipped | n/a |

The shipped pieces (puller library, normalizer, sink, `_iter_jsonl`
extension, `layer_stack` emitters, `daemon_event` boundary lint) are
**not modified** by Phase 2.5 unless a heavy-run profile shows the
synchronous gzip path on a flame chart (see §Risk).

## Slice order (one PR per row)

The order matches the slice-1 report's deferred list verbatim so reviewers
can map this plan back to the report 1:1. Each slice is independently
mergeable. Slice 6 ("puller-to-recorder wiring") is the moment
`sandbox_events.jsonl` switches from the stream-bridge to the pull path;
ship it AFTER at least one full subsystem-emitter slice (so the wired
path has real events to carry).

### Slice 1 — `overlay_workspace` emitters

**Files to instrument:**
- `backend/src/sandbox/overlay/lifecycle.py`
- `backend/src/sandbox/overlay/handle.py`
- `backend/src/sandbox/overlay/namespace_runner.py`
- `backend/src/sandbox/ephemeral_workspace/pipeline.py`

**Schema additions** (in `backend/src/sandbox/daemon/audit_schema.py`):

```python
@dataclass
class OverlayWorkspaceSection:
    operation_id: str | None = None
    workspace_mode: str = "ephemeral"          # always "ephemeral" here
    workspace_handle_id: str | None = None
    lease_id: str | None = None
    manifest_root_hash: str | None = None
    mount_ms: float | None = None
    cleanup_ms: float | None = None
    scratch_removed: bool | None = None
    cleanup_failure_kind: str | None = None
```

**Events:**

```
overlay_workspace.mounted        (operation_id, lease_id, manifest_root_hash, mount_ms)
overlay_workspace.published      (operation_id, committed_layer_id, publish_layer_ms)
overlay_workspace.cleaned        (operation_id, cleanup_ms, scratch_removed=True)
overlay_workspace.cleanup_failed (operation_id, cleanup_failure_kind, cleanup_ms)
```

All on `critical` lane. Stamp `workspace_mode="ephemeral"` on every event.

**Tests** (under `backend/tests/unit_test/test_sandbox/test_overlay/`):
- `test_overlay_workspace_emits_mounted_published_cleaned` — one full
  ephemeral lifecycle; assert three events present with shared
  `operation_id`.
- `test_overlay_workspace_cleanup_failed_emits_failure_kind` — force a
  `shutil.rmtree` failure on scratch; assert `cleanup_failed` event with
  non-empty `cleanup_failure_kind`.

---

### Slice 2 — `isolated_workspace` emitters

**Files to instrument:**
- `backend/src/sandbox/isolated_workspace/pipeline.py`
- `backend/src/sandbox/isolated_workspace/_control_plane/pipeline_registry.py`
- `backend/src/sandbox/isolated_workspace/_control_plane/pipeline_state.py`
- `backend/src/sandbox/isolated_workspace/_control_plane/orphan_reaper.py`
- `backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py`
- `backend/src/sandbox/isolated_workspace/_control_plane/linux_runtime.py`

**Schema additions:**

```python
@dataclass
class IsolatedWorkspaceSection:
    operation_id: str | None = None
    workspace_mode: str = "isolated"
    workspace_handle_id: str | None = None
    agent_id: str | None = None
    holder_pid: int | None = None
    holder_pid_alive: bool | None = None
    cgroup_id: str | None = None
    cgroup_removed: bool | None = None
    scratch_removed: bool | None = None
    upperdir_bytes: int | None = None
    upperdir_cap_bytes: int | None = None
    memory_current_bytes: int | None = None
    memory_peak_bytes: int | None = None
    cpu_usage_usec_delta: int | None = None
    orphan_holder_count: int = 0
    orphan_cgroup_count: int = 0
    orphan_scratch_count: int = 0
    sampled_at_monotonic_s: float | None = None
```

**Events:**

```
isolated_workspace.entered              (critical) — at enter_isolated_workspace
isolated_workspace.sampled              (sample, 500 ms cadence)
isolated_workspace.exited               (critical) — at exit_isolated_workspace
isolated_workspace.evicted              (critical) — orphan reap path
isolated_workspace.orphan_check_completed (critical) — at every exit; carries orphan_*_count
isolated_workspace.orphan_reaped        (critical) — when reaper kills a holder
```

The 500 ms sampler ticks already exist inside the control-plane
pipeline_state; piggyback the emit there. No new threads.

**Tests:**
- `test_isolated_workspace_lifecycle_emits_full_family` — enter → 1 sample
  → exit; assert all three events with shared `operation_id` and
  `workspace_handle_id`.
- `test_isolated_workspace_orphan_check_after_exit` — kill holder mid-run;
  assert `orphan_holder_count > 0` in both the pulled events and
  `isolated_workspace.exited` payload.
- `test_isolated_workspace_sampled_lane_is_sample` — assert
  `isolated_workspace.sampled` lands on `sample` lane.

---

### Slice 3 — `occ` emitters

**Files to instrument:**
- `backend/src/sandbox/occ/service.py` (was previously planned in
  `sandbox/daemon/occ_runtime_services.py` and
  `sandbox/daemon/changeset_projection.py`; the actual OCC emitters live
  in the service module — Phase 3 deferral D16)

**Schema additions:**

```python
@dataclass
class OccSection:
    operation_id: str | None = None
    operation_step: int | None = None
    changeset_id: str | None = None
    changed_path_count: int | None = None
    transaction_lock_wait_ms: float | None = None
    apply_ms: float | None = None
    commit_ms: float | None = None
    committed_layer_id: str | None = None
    publish_layer_ms: float | None = None
    committed_layer_bytes: int | None = None
    conflict_kind: str | None = None
    conflict_path: str | None = None
    conflict_reason: str | None = None
    base_manifest_version: int | None = None
    current_manifest_version: int | None = None
```

**Events:**

```
occ.changeset_prepared        (normal, operation_step=70)
occ.transaction_lock_acquired (normal, operation_step=90)
occ.apply_committed           (normal, operation_step=110)
occ.publish_layer             (normal)
occ.conflict_rejected         (critical) — both manifest versions populated
```

**Tests:**
- `test_occ_apply_committed_carries_changeset_id`
- `test_occ_conflict_rejected_carries_both_manifest_versions`
- `test_occ_apply_committed_lane_is_normal_conflict_is_critical`

---

### Slice 4 — `os_resource.sampled`

**Files to instrument:** the command-exec resource-metrics tick (look up
its current location; it is invoked from the daemon command-exec path,
not a new sampler).

**Constraint:** zero new threads (Phase 1 revertability + README "zero new
threads" principle). Piggyback option (a) per `phase-2 §2 os_resource`.

**Events:**

```
os_resource.sampled (sample) — payload uses Phase 1 OsResourceSection
                                (rss_bytes, cpu_user_s, cpu_system_s,
                                 sampled_at_monotonic_s)
```

**Tests:**
- `test_os_resource_sampled_emitted_on_existing_tick` — assert
  `os_resource.sampled` appears in the ring after triggering one
  command-exec; assert no new threads created (use `threading.active_count`
  diff).
- `test_os_resource_sampled_lane_is_sample`.

---

### Slice 5 — Generic plugin shim

**Files to instrument:**
- `backend/src/plugins/core/loader.py` ONLY

**No edits in `backend/src/plugins/catalog/lsp/`.** This is the central
enforcement point for [req 2 — generic plugin].

**Schema additions:**

```python
@dataclass
class PluginSection:
    plugin_id: str
    plugin_kind: str          # "language_server", "formatter", "indexer", ...
    plugin_version: str | None = None
    plugin_tool_name: str | None = None
    request_bytes: int | None = None
    response_bytes: int | None = None
    duration_ms: float | None = None
    status: str | None = None
    error_kind: str | None = None
    message_hash: str | None = None
    workspace_handle_id: str | None = None
    agent_id: str | None = None
    peak_resident_bytes: int | None = None
```

**Events:**

```
plugin.tool_invoked          (normal)
plugin.tool_completed        (normal)
plugin.error                 (normal)
plugin.peak_resident_sampled (sample) — on the OS resource sampler tick
                                         when a plugin process is identified
```

**Mechanism:** wrap plugin-tool dispatch in `plugins/core/loader.py` with a
thin emitter shim that fires the invoked/completed/error trio. Identify
the plugin's process for `peak_resident_sampled` from the shim's context.

**Tests:**
- `test_plugin_events_are_kind_generic` — register a fake
  `plugin_kind="indexer"` plugin; assert it emits the same event family
  as LSP with NO LSP-specific keys. Grep the emitted JSON for `"lsp"`
  and `"pyright"` as **keys** → must be 0 hits (values are OK).
- `test_plugin_error_carries_error_kind`.

---

### Slice 6 — Background tools + puller-to-recorder wiring

Two changes that are most reviewable together: the puller starts
consuming background-tool events at the same moment the recorder begins
writing to the sink via the pull path.

**Files (background tool):**
- `backend/src/engine/background/task_supervisor.py`

**Schema additions:**

```python
@dataclass
class BackgroundToolSection:
    background_task_id: str
    task_kind: str | None = None
    tool_name: str | None = None
    agent_id: str | None = None
    uptime_ms: float | None = None
    status: str | None = None     # mirrors BackgroundTaskStatus
    exit_code: int | None = None
    duration_ms: float | None = None
    error_kind: str | None = None
    cancel_reason: str | None = None
    delivery_latency_ms: float | None = None
```

**Events** — emit from `_set_terminal_status` and `collect_completed`:

```
background_tool.started   (normal)
background_tool.heartbeat (sample) — on the existing 60 s timer
background_tool.completed (normal)
background_tool.failed    (normal)
background_tool.cancelled (normal)
background_tool.delivered (normal)
```

**Constraint:** zero new threads. Heartbeat reuses
`EOS_BACKGROUND_HEARTBEAT_INTERVAL_S`. Heartbeat event MUST carry
`background_task_id` (Critic P1 fix from V3.1).

**Files (puller wiring):**
- `backend/src/task_center_runner/audit/recorder.py` — extend
  `AuditRecorder.start()` to construct + start a `DaemonAuditPuller`
  pointed at `sandbox.api.audit_pull`; emit callback feeds
  `normalize_pulled_event` → `RotatingJsonlSink.append_event`.
  Extend `dispose()` to `await puller.stop()` BEFORE flushing message
  recorders. Add a `puller_stats()` accessor for the perf report.
- A new helper `sandbox_id` lookup may be needed if the recorder doesn't
  already have one; route via the existing `_sandbox_id` field.

**Tests:**
- `test_background_tool_lifecycle_emits_full_lattice` — three runs:
  RUNNING→COMPLETED→DELIVERED, RUNNING→FAILED→DELIVERED,
  RUNNING→CANCELLED→DELIVERED. Assert exactly one terminal event per run.
- `test_background_tool_heartbeat_reuses_existing_timer` — assert
  `threading.active_count()` unchanged across a heartbeat tick; assert
  heartbeat event present with `background_task_id`.
- `test_puller_final_drain_before_recorder_dispose` — start recorder,
  inject 50 events into the ring, call `dispose()`, assert all 50
  events present in the JSONL sink (no events left in ring).
- `test_puller_never_blocks_tool_dispatch` — inject a 250 ms pull stall
  in the transport; assert tool-call wall-time unchanged (delta < 5 ms).
- `test_dedupe_pull_supersedes_stream_when_both_present` (full pipeline
  version of slice-1's unit test) — pull + stream both emit a logically
  identical `occ.apply_committed`; assert the JSONL row carries the pull
  version's richer fields.

---

### Slice 7 — Per-tool phase emitters (slow-tail flush)

**File to instrument:**
- `backend/src/engine/tool_call/dispatch.py`

**Schema additions:**

```python
@dataclass
class ToolCallSection:
    tool_id: str
    tool_name: str
    agent_id: str | None = None
    workspace_mode: str | None = None   # "default" | "ephemeral" | "isolated"
    workspace_handle_id: str | None = None
    phase: str | None = None            # one of queued/mount/exec/capture/publish/release
    duration_ms: float | None = None
    total_ms: float | None = None
    exit_status: str | None = None
    bytes_in: int | None = None
    bytes_out: int | None = None
    phase_totals_rollup: dict[str, float] | None = None
```

**Events:**

```
tool_call.started  (normal) — always emit
tool_call.phase    (sample) — per slow-tail rule below
tool_call.finished (normal) — always emit, with phase_totals_rollup populated
```

**Slow-tail mechanism** (closes the V3.1 P1 fix replacing 1-in-N
sampling):

1. Per-call: a thread-local fixed-size deque of `{phase, duration_ms}`
   (max 6 entries, ~96 bytes).
2. Per-`tool_name`: a rolling deque of last 100 `total_ms` values
   (~800 bytes per active tool_name), protected by a per-`tool_name`
   `threading.Lock`. Critical section is O(1) — append + drop-oldest +
   P95 via `statistics.quantiles(n=20)[18]` (or `sorted()[idx]` on a
   fixed-size list which is fine at N=100).
3. On `tool_call.finished`:
   - Cold window (rolling-window has < 100 entries) → flush all phase
     events.
   - Slow tail (rolling-window full AND `total_ms ≥ P95`) → flush.
   - Else → discard phase buffer; `phase_totals_rollup` still
     populated on `tool_call.finished` from in-process timers.

**Tests:**
- `test_tool_call_phase_slow_tail_flush` — 200 invocations of a fake
  `smoke_tool` with deterministic timings `[10ms × 190, 500ms × 10]`;
  assert (a) first 100 calls flush all 6 phases; (b) of remaining 100,
  the 5 with `total_ms ≥ P95` flush all phases; (c) other 95 flush no
  phase events but DO emit `tool_call.finished` with populated
  `phase_totals_rollup`.
- `test_tool_call_finished_rollup_present_when_phases_discarded` —
  one fast-tail call after warmup; assert rollup populated with all 6
  phase keys.
- `test_tool_call_envelope_always_emits_on_normal_lane`.

---

### Slice 8 — End-to-end heavy-run regression

Not a new emitter slice; this slice locks the Phase 2 acceptance criteria
into CI now that every subsystem is wired.

**Tests** (lifted verbatim from `phase-2 §Tests`):
- `test_sandbox_events_jsonl_rotates_at_64mib_and_caps_history` — synthetic
  1 M-event mock-suite run; assert exactly N rotated files; live file ≤
  64 MiB.
- `test_sandbox_events_jsonl_rotation_path_stable_under_eos_tier_run_id` —
  full-pipeline version of slice-1's sink test, end-to-end.
- `test_iter_jsonl_concatenates_rotated_gzipped_history` — full-pipeline
  version (slice 1 covers the sink-unit version).
- `test_no_consumer_reads_daemon_event_under_default_config` — full mock
  suite with `EOS_AUDIT_FORENSIC_RAW_ENABLED` unset.
- `test_forensic_raw_present_when_env_enabled` — full mock suite with
  the env enabled.
- Acceptance assertion: after one full mock-suite run,
  `dropped_event_count == 0` and `lost_before_seq == 0`.

## Acceptance criteria for Phase 2.5

When the last slice merges, all of the following are true:

- Every subsystem listed in [README §Subsystem section keys] has at least
  one emitter wired and tested.
- `AuditRecorder.start()` starts a `DaemonAuditPuller`; `dispose()`
  awaits its final drain. The stream-bridge path
  (`task_center_runner/audit/stream_bridge.py` +
  `_record_sandbox_event`) remains as fallback but is no longer the
  primary writer.
- `sandbox_events.jsonl` under a mock-suite run contains rows from all
  9 subsystem sections (verified by jq query in the heavy-run test).
- No new threads in `task_supervisor.py` or anywhere else (verified by
  `threading.active_count()` diff in dedicated tests).
- All Phase 2 tests from the original
  `phase-2-emitters-and-puller.md §Tests` list pass.
- `dropped_event_count == 0` and `lost_before_seq == 0` on a full mock
  suite.
- `.venv/bin/ruff check` clean on all touched files.
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/ backend/tests/unit_test/test_task_center_runner/`
  green.

When all of the above hold, **Phase 2's overall goal is achieved.** Phase
3 (report rendering, release gates, default-on rollout) is the only
remaining V3 work.

## Cross-cutting contracts that this phase MUST honor

Every slice in 2.5 inherits, unchanged, these contracts from the shipped
foundation:

- **Schema is additive only.** New sections / new event names / new
  optional fields stay v1. Rename or remove → v2 (do not do this in 2.5).
- **`payload["daemon_event"]` boundary.** Only the normalizer writes it.
  Slice-1's `test_daemon_event_writer_module_boundary` CI lint will fail
  any slice that adds a new reference. Do not add one.
- **Lane assignment.** See [README §Lane assignment]. Every new event
  family added here MUST appear in that table BEFORE merging the slice;
  if not, the table is wrong and you are silently widening v1.
- **Causal chain (Principle 3).** Every transaction event carries
  `operation_id` (+ `lease_id` for layer_stack / overlay,
  `changeset_id` for occ, `workspace_handle_id` for isolated /
  ephemeral). Missing identifiers are a P0 review block.
- **Zero new threads.** No slice in 2.5 may introduce a new timer or
  thread. Use existing ticks: command-exec resource metrics for
  `os_resource.sampled` + `plugin.peak_resident_sampled`;
  `EOS_BACKGROUND_HEARTBEAT_INTERVAL_S` for `background_tool.heartbeat`;
  500 ms isolated-workspace sampler for
  `isolated_workspace.sampled`.

## Risk notes

- **Synchronous gzip on rotation** (deferred call from slice 1): if the
  heavy-run test in slice 8 shows a rotation pause > 200 ms on flame
  charts, move gzip behind a `ThreadPoolExecutor` with bounded queue
  depth = 2 inside this phase (counts as a sink hot-fix, not a new
  slice). Otherwise leave as-is.
- **Plugin shim under in-process plugins.** The current loader is an
  import-time singleton without per-invocation lifecycle (V3 ADR
  drivers). Slice 5's shim wraps the dispatch *call site*, which is the
  only viable insertion point until the real plugin-session model lands
  (follow-up FU#2). Keep the shim trivial; do not anticipate the future
  session model.
- **`isolated_workspace` sampler tick** at 500 ms produces ~120 events
  per minute per open workspace. With one concurrent isolated workspace
  this is fine (~7 k events/hour, well under ring cap). If we ever run
  isolated workspaces in parallel and the ring sees pressure, raise the
  cadence floor before raising the sample interval.
- **`background_tool.heartbeat`** at 60 s is sparse, but the heartbeat
  event MUST carry `background_task_id` (V3.1 Critic P1) — easy to
  forget when piggy-backing.

## Out of scope (still)

- Performance-report rendering, release gates, default-on rollout —
  Phase 3.
- Stream-bridge code removal — follow-up FU#1, after K=5 clean heavy
  runs post-merge.
- Real plugin session lifecycle (`plugin.session_*`) — follow-up FU#2.
- Ring-by-lane separation (`sample` ring sharded out from `critical`) —
  follow-up FU#4, only triggered if overhead gate fails permanently.
