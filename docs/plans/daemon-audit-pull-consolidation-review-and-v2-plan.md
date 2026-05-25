# Daemon Audit Pull Consolidation — Review & V2 Implementation Plan

## Part 1 — Review of the Existing Plan

File reviewed: `docs/plans/daemon-audit-pull-consolidation-implementation-plan.md`.

### What the existing plan gets right

- **Pull-only design with a bounded daemon-side ring** is the correct
  architectural choice. It keeps daemon CPU/memory cost flat regardless of
  workload and decouples audit from the hot tool-call path.
- **Monotonic `seq` + exclusive `after_seq` cursor + `lost_before_seq`**
  matches the OCC/layer-stack causal-grouping model already used in the rest
  of the system. Reusing that primitive makes the pulled stream trivially
  reconcilable with `task_graph` invariants.
- **Single canonical artifact (`sandbox_events.jsonl`)** rather than a new
  parallel format. Keeps existing readers (`performance_report.py`, live
  health checks, downstream notebooks) intact.
- **Subsystem-section keys without a top-level discriminator** is the
  right shape — it lets a single event carry, e.g., both `isolated_workspace`
  *and* `os_resource` payloads without normalization gymnastics.
- **Isolated-workspace exit contract (Phase 8)** correctly identifies the
  highest-risk surface and gates the rollout on orphan-holder evidence.

### Gaps and concerns (drives the V2 plan below)

1. **Overlay narrative is thin and conflates isolated vs ephemeral.**
   The plan lists `workspace_mode: default | ephemeral | isolated` but never
   shows *how the same overlay primitive yields different state/event/resource
   profiles* across the two modes. Specifically:
   - Ephemeral workspaces are short-lived, single-tool-call scoped, and their
     upperdir is *always discarded*; the dominant resource cost is mount/
     unmount churn.
   - Isolated workspaces are long-lived (agent-scoped), have a holder PID +
     cgroup + retained upperdir, and the dominant cost is *steady-state
     residency* (memory_current, upperdir growth, cgroup CPU accumulation).
   The two need different sampling cadences, different lifecycle event
   families, and different report sections. The current plan reuses one
   schema for both and loses signal.

2. **LayerStack + OCC interaction is under-specified.**
   The plan emits layer-stack lease/lock and OCC prepare/apply/commit events
   *in parallel* without exposing the **lease → lock → changeset → commit →
   publish_layer → release** causal chain that defines a write transaction.
   Reviewers reading a report cannot answer "did this OCC conflict happen
   under lease L on manifest version V, and did the squash that ran 200ms
   later invalidate L?" without manually joining on `operation_id`.

3. **Background tool calls are entirely absent.**
   `engine/background/task_supervisor.py` already runs an
   `EOS_BACKGROUND_HEARTBEAT_INTERVAL_S` loop and emits
   `BackgroundTaskStarted` stream events with a RUNNING → {COMPLETED, FAILED,
   CANCELLED} → DELIVERED state machine. None of that surfaces in the audit
   pull. A background shell that runs for 20 minutes is invisible between
   start and delivery.

4. **Plugin instrumentation assumes "LSP/Pyright".**
   The plan never names plugins explicitly, but the absence of any plugin
   section will encourage emitters to bake LSP-specific fields into
   `overlay_workspace`. Plugins are generic (`backend/src/plugins/catalog/`
   currently ships `lsp`, but the catalog is open-ended — formatters,
   indexers, language servers, build daemons, MCP bridges). They need a
   generic `plugin` subsystem section keyed by `plugin_id` + `plugin_kind` so
   a future Ruff or `tsc --watch` plugin slots in without schema churn.

5. **No per-tool time accounting.**
   The plan logs mount_ms / run_ms / capture_upperdir_ms / publish_ms at the
   *overlay* level, but the consumer wants per-tool: "how much wall time did
   `read_file`, `edit_file`, `shell`, `grep`, plugin tools, and background
   tools consume, broken into queue / mount / exec / capture / publish /
   release?" Without this, the report cannot answer "which tool is slow".

6. **Performance & resource report sections are listed but not laid out.**
   `Phase 7` enumerates keys but does not specify the rendered MD layout,
   percentile tables, or per-tool/per-plugin/per-workspace-mode breakdowns.
   This is what readers actually look at; leaving it implicit guarantees
   inconsistent output.

7. **Overhead claims are unstated.**
   The plan should *quantify and verify* that the heartbeat/pull/audit path
   itself does not regress sandbox throughput. Without an explicit
   "overhead budget + measured overhead" gate, the pull cadence will drift
   upward over time and eventually compete with tool execution.

8. **Disk-usage and log-persistence story is implicit.**
   Sandbox-side disk is bounded by the ring's `max_bytes` and by upperdir
   sampling cadence, but the plan never says so plainly. On the host side,
   `sandbox_events.jsonl` has no rotation, compression, or retention
   contract. For long runs (>1h, 10k+ tool calls) this file becomes a
   multi-GB liability.

9. **Eight phases is too many.**
   Phases 1-3 (ring + RPC + emitters) are one feature. Phases 4-6 (puller +
   normalize + persist) are one feature. Phases 7-8 (report + isolated
   gating) are one feature. Eight phases dilutes review and lets the
   isolated-workspace gate slip.

10. **`Phase 6` example payload conflicts with the section-keys decision.**
    Design decision #9 says "subsystem-section keys in *event payloads*", but
    the Phase 6 recorder row puts `isolated_workspace` and `os_resource`
    inside `payload` *next to* `daemon_event`. That works, but it means
    `daemon_event` itself also has section keys, so consumers see the same
    data twice. Plan should pick one: pulled raw under `daemon_event`, *or*
    promoted section keys at `payload.*` — not both.

---

## Part 2 — V2 Implementation Plan (3 Phases)

### Guiding principles

- **Pull-only**, bounded daemon ring, single canonical artifact
  (`sandbox_events.jsonl`). Inherits from V1.
- **One schema, six subsystem sections, generic plugin section**:
  `daemon`, `layer_stack`, `overlay_workspace`, `occ`, `isolated_workspace`,
  `os_resource`, **`plugin`**, **`background_tool`**, **`tool_call`**.
- **Causal chain over flat events**: every write transaction carries
  `operation_id` *and* `lease_id` *and* `changeset_id` so the report can
  reconstruct lease → lock → changeset → commit → publish → release.
- **Overhead budget is a release gate**, not a hope.
- **Disk budget is fixed at both ends**: bounded ring on sandbox side,
  rotated + compressed + retention-capped artifacts on host side.

---

### Phase 1 — Daemon Ring, Pull RPC, and Subsystem Schema

**Goal:** A bounded daemon-side audit ring with a pull RPC, plus a frozen
event schema that covers all subsystems including plugins and background
tool calls. No emitters wired yet beyond a minimal smoke set.

#### Deliverables

1. `backend/src/sandbox/daemon/audit_buffer.py` — ring with monotonic `seq`,
   `max_events` (default 50000), `max_bytes` (default 8 MiB), priority lanes
   (critical / normal / sample), and `dropped_event_count` /
   `lost_before_seq` counters.
2. `api.audit.pull` and `api.audit.snapshot` RPC ops registered in
   `backend/src/sandbox/daemon/rpc/dispatcher.py`. Wrappers in
   `backend/src/sandbox/api/daemon_audit.py`. Snapshot is O(1) over cached
   gauges; pull is O(returned events).
3. **Frozen event schema v1** documented inline:
   `daemon`, `layer_stack`, `overlay_workspace`, `occ`,
   `isolated_workspace`, `os_resource`, `plugin`, `background_tool`,
   `tool_call`.
4. Minimal smoke emitters for `daemon.started`,
   `daemon.audit_buffer_pressure`, and an `os_resource.sampled` heartbeat
   on the default sampler tick (overridable by the Phase 2 adaptive cadence
   policy — see table in Phase 2).

#### State / event / resource demonstration (Phase 1 schema commitment)

**Overlay workspace — isolated vs ephemeral side-by-side**

| Property | `ephemeral` | `isolated` |
| --- | --- | --- |
| Lifecycle states | `mount → exec → capture → publish → unmount` (per tool call) | `enter → active → (tool…tool…sample…) → exit` (per agent / per task) |
| Dominant event family | `overlay_workspace.mounted/published/cleaned` | `isolated_workspace.entered/sampled/exited/orphan_check_completed` |
| Upperdir fate | always discarded at `unmount` | retained until `exit`, optionally promoted via OCC |
| Holder PID | none (caller is daemon-internal) | exactly one external holder PID + cgroup |
| Sampling cadence | lifecycle boundaries only | lifecycle boundaries + 500 ms steady-state |
| Memory profile | spike during exec, free at unmount | resident; `memory_current_bytes` + `memory_peak_bytes` tracked across lifetime |
| Disk profile | bounded by single tool's write | bounded by `EOS_ISOLATED_WORKSPACE_UPPERDIR_MAX_BYTES`; emit warning at 80 % |
| CPU profile | sum of `run_ms` per call | continuous `cpu_usage_usec_delta` per sample window |
| Cleanup signal | `scratch_removed=true` on every event | `scratch_removed`, `cgroup_removed`, `holder_pid_alive=false` on `exited` |
| Failure signal | non-zero `cleanup_ms` only | non-zero `orphan_holder_count / orphan_cgroup_count / orphan_scratch_count` |

**LayerStack — lease/lock/squash event family**

```
layer_stack.lease_requested  (operation_step=20, lease_id, manifest_version)
layer_stack.lease_acquired   (operation_step=20, lease_wait_ms)
layer_stack.lock_acquired    (operation_step=30, lock_wait_ms)
layer_stack.snapshot_prepared(operation_step=40, prepare_snapshot_ms, layer_count)
layer_stack.squash_triggered (squash_trigger_reason, squash_input_layers)
layer_stack.squash_completed (squash_result_layers, manifest_root_hash)
layer_stack.lease_released   (operation_step=130, lease_hold_ms)
```

Every event carries `lease_id` so the report can render a per-lease
timeline. `manifest_root_hash` lets a stale-base OCC rejection cite the
exact manifest version it was rejected against.

**OCC — changeset transaction family**

```
occ.changeset_prepared       (operation_step=70, changeset_id, changed_path_count)
occ.transaction_lock_acquired(operation_step=90, transaction_lock_wait_ms)
occ.apply_committed          (operation_step=110, apply_ms, commit_ms, committed_layer_id)
occ.publish_layer            (publish_layer_ms, committed_layer_bytes)
occ.conflict_rejected        (conflict_kind, conflict_path, conflict_reason,
                              base_manifest_version, current_manifest_version)
```

Conflict events explicitly carry both the writer's `base_manifest_version`
and the daemon's `current_manifest_version`, matching the
[[project_ephemeralos_layerstack_occ_design]] stale-base story.

**Background tool calls — generic, plugin-agnostic**

```
background_tool.started      (background_task_id, task_kind, tool_name, agent_id)
background_tool.heartbeat    (uptime_ms, status=RUNNING)
background_tool.completed    (status ∈ {COMPLETED, FAILED, CANCELLED}, exit_code, duration_ms)
background_tool.delivered    (delivery_latency_ms)
```

These mirror the existing `BackgroundTaskStatus` lattice in
`engine/background/task_supervisor.py`
(`RUNNING → {COMPLETED, FAILED, CANCELLED} → DELIVERED`). Heartbeats reuse
`EOS_BACKGROUND_HEARTBEAT_INTERVAL_S` (default 60 s) — no new timer thread.
Background tool emission cost is therefore *bounded by the existing
heartbeat*, not added on top.

**Plugin — generic, not LSP-specific**

```
plugin.session_started   (plugin_id, plugin_kind, plugin_version, workspace_handle_id)
plugin.tool_invoked      (plugin_id, plugin_tool_name, request_bytes)
plugin.tool_completed    (plugin_tool_name, duration_ms, response_bytes, status)
plugin.session_stopped   (plugin_id, lifetime_ms, requests_served, peak_rss_bytes)
plugin.error             (plugin_id, error_kind, message_hash)
```

`plugin_kind` examples: `language_server`, `formatter`, `indexer`,
`build_daemon`, `mcp_bridge`, `custom`. The current `lsp` plugin
(`backend/src/plugins/catalog/lsp/`) is *one instance* of `plugin_kind =
"language_server"`. No field in the schema names "pyright", "lsp", or
"language" — those are values, not keys. A future Ruff long-running daemon
or a `tsc --watch` plugin emits the same event family unchanged.

**Per-tool timing — every tool, foreground and background**

```
tool_call.started   (tool_id, tool_name, agent_id, workspace_mode)
tool_call.phase     (phase ∈ {queued, mount, exec, capture, publish, release}, duration_ms)
tool_call.finished  (total_ms, exit_status, bytes_in, bytes_out)
```

Phases are emitted incrementally so even an aborted call yields per-phase
data. `workspace_mode` lets the report split the same `tool_name` between
`ephemeral` and `isolated` cohorts — answers "is `edit_file` slower in
isolated mode?".

#### Resource & overhead budget for Phase 1 itself

The pull/heartbeat/ring path is intentionally cheap. Budgets (verified by
benchmarks in Phase 3 gate):

| Component | Memory ceiling | CPU ceiling | Disk (sandbox) | Notes |
| --- | ---: | ---: | ---: | --- |
| Daemon ring | 8 MiB (`max_bytes`) | < 0.1 % avg, < 1 % p99 | 0 (never spills to disk) | hard-capped by `max_bytes` + `max_events` |
| `api.audit.pull` (1 s cadence) | < 1 MiB transient per call | ~2 ms CPU per call at 1000 events | 0 | O(returned events), not O(retained) |
| `api.audit.snapshot` | 0 | < 0.5 ms | 0 | reads cached gauges only |
| Heartbeat (background tool) | reuses existing 60 s timer | unchanged | 0 | zero new threads |
| Upperdir disk samples | 0 | bounded by sample budget; emits `sample_budget_exhausted` | reads only — never writes | cached with TTL |

#### Tests for Phase 1

- `test_audit_buffer_ordering` — `seq` is strictly monotonic across lanes.
- `test_audit_buffer_eviction_events_and_bytes` — both caps enforced.
- `test_audit_buffer_critical_lane_survives_sample_pressure`.
- `test_pull_cursor_exclusive_and_drops_reported`.
- `test_snapshot_is_o1_under_load` — generate 1 M synthetic events,
  assert snapshot latency p99 < 1 ms.

---

### Phase 2 — Runner Puller, Emitters, and Generic Plugin / Background Instrumentation

**Goal:** Wire daemon emitters across all subsystems, stand up the runner-side
puller, normalize and persist into `sandbox_events.jsonl`, and instrument the
generic plugin + background tool surfaces.

#### Deliverables

1. `backend/src/task_center_runner/audit/daemon_pull.py` —
   `DaemonAuditPuller` with cursor state, adaptive interval policy, and
   final-drain on stop. Stats: `pull_count`, `empty_pull_count`,
   `events_pulled`, `pull_error_count`, `dropped_event_count`,
   `lost_before_seq`, `max_buffer_pressure`, `final_cursor`.
2. Daemon emitters (one PR per subsystem, mergeable independently):
   - `layer_stack` — instrument `layer_stack_runtime.py`.
   - `overlay_workspace` — instrument
     `sandbox/overlay/{lifecycle,handle,namespace_runner}.py` and both
     `ephemeral_workspace/pipeline.py` and `isolated_workspace/pipeline.py`,
     stamping `workspace_mode` correctly.
   - `occ` — instrument `occ_runtime_services.py` +
     `changeset_projection.py`.
   - `isolated_workspace` — instrument
     `sandbox/isolated_workspace/manager.py` with the full lifecycle
     family from Phase 1 schema.
   - `os_resource` — extend existing command-execution resource metrics.
3. **Generic plugin instrumentation surface** in
   `backend/src/plugins/core/loader.py` — emit `plugin.*` events from the
   loader and from a thin wrapper around plugin-tool dispatch. *No* code in
   `plugins/catalog/lsp/` should know about audit; the surface lives in
   `core/` so future plugins inherit it for free.
4. **Background tool instrumentation** in
   `backend/src/engine/background/task_supervisor.py` — emit
   `background_tool.*` on `_set_terminal_status` transitions and on every
   heartbeat tick. Reuses the existing 60 s heartbeat — no new loop.
5. **Per-tool phase emitters** in
   `backend/src/engine/tool_call/dispatch.py` — emit `tool_call.phase`
   on entry/exit of each phase. Phase boundaries derive from existing
   dispatch milestones, so cost is one append per boundary.
6. Normalizer in `task_center_runner/audit/sandbox_events.py` — preserve
   raw event under `payload["daemon_event"]` *and* promote subsystem
   sections to `payload.<section>`. This duplication is intentional and
   resolves V1 gap #10 in favor of *both*: `daemon_event` is the
   forensic-grade verbatim record for audit replay/debugging, while
   promoted `payload.<section>` keys are the stable consumer surface for
   the report pipeline and downstream readers. Dedupe stream + pull by
   `seq` then `(operation_id, event, operation_step, tool_id)`.
7. `sandbox_events.jsonl` writer gains **rotation + gzip** at 64 MiB per
   file with `EOS_AUDIT_ARTIFACT_RETENTION_FILES=8` (default) — solves
   the unbounded host-side artifact growth gap.

#### Adaptive pull cadence (already correct in V1; restated for Phase 2)

| Condition | Interval | Rationale |
| --- | ---: | --- |
| active run, default | 1 s | normal cadence |
| idle (no inflight) | 5 s | background tool heartbeat dominates |
| isolated workspace active | 500 ms | catch orphan/holder drift fast |
| buffer pressure ≥ 0.8 | 250 ms | drain before eviction |
| final drain | until empty or 3 s cap | bounded teardown |

#### Disk & log persistence contract (closes gap #8)

- **Sandbox side:** zero disk writes for the audit path. Ring is in-memory
  only. Upperdir size queries are cached and bounded; full tree walks are
  forbidden. Per-sandbox disk usage is whatever the workload writes —
  audit adds nothing.
- **Host side:** `sandbox_events.jsonl` is rotated at 64 MiB, gzipped on
  rotation, capped at 8 historical files per run (configurable). Worst-case
  on-disk footprint per run ≈ 64 MiB live + 8 × ~10 MiB compressed
  ≈ ~150 MiB. `performance_report.{json,md}` is written once, post-run,
  with no rotation needed.
- **Retention beyond a run** is the responsibility of the existing run-dir
  GC (no change in this plan), so this surface inherits the
  `EOS_TIER_RUN_ID` artifact-stability contract.

#### Tests for Phase 2

- `test_puller_final_drain_before_recorder_dispose`.
- `test_puller_never_blocks_tool_dispatch` — inject 250 ms pull stall,
  assert tool latency unchanged.
- `test_plugin_events_are_kind_generic` — register a fake
  `plugin_kind="indexer"` plugin, assert it emits the same event family
  as the LSP plugin with no LSP-specific keys.
- `test_background_tool_lifecycle_emits_full_lattice` — RUNNING →
  COMPLETED → DELIVERED, RUNNING → FAILED → DELIVERED,
  RUNNING → CANCELLED → DELIVERED.
- `test_isolated_workspace_orphan_check_after_exit` — kill holder mid-run,
  assert `orphan_holder_count > 0` in pulled events.
- `test_sandbox_events_jsonl_rotates_at_64mib_and_caps_history`.

---

### Phase 3 — Consolidated Performance & Resource Report + Release Gates

**Goal:** Render a human-readable, decision-grade performance & resource
report; gate rollout on measured overhead and isolated-workspace orphan
counts.

#### Deliverables

1. Extend `task_center_runner/audit/performance_report.py` to produce a
   structured `sandbox.sections` object *and* a rendered Markdown report
   whose layout is fixed below.
2. Add `sandbox.daemon_audit_pull` block with puller stats (from Phase 2).
3. Add `sandbox.overhead` block with measured cost of the audit path
   itself (see overhead gate below).
4. Promote `daemon_audit_pull.enabled=true` as default for sandbox-backed
   `task_center_runner` runs after gates pass.

#### Fixed report layout (`performance_report.md`)

```
# Performance & Resource Report — <run_id>

## 1. Summary
   - duration_total_ms, tools_called, background_tools, sandbox_ops
   - peak: rss_bytes, upperdir_bytes_total, layer_count
   - audit: events_pulled, dropped_event_count, max_buffer_pressure

## 2. Per-tool timing (foreground)
   | tool_name | workspace_mode | calls | p50_ms | p95_ms | p99_ms | total_ms |
   | read_file | ephemeral      |  421  |   2.1  |  6.4   |  18.7  |   1832   |
   | edit_file | isolated       |  119  |  11.3  | 42.0   | 110.2  |   3110   |
   | shell     | isolated       |   34  | 184.0  | 920.0  |1820.0  |   9881   |
   | grep      | ephemeral      |   88  |   4.0  | 11.5   |  31.0  |    612   |
   | glob      | ephemeral      |   42  |   1.8  |  5.0   |   9.1  |    109   |
   | write_file| isolated       |   28  |   8.9  | 30.0   |  72.0  |    444   |

## 3. Per-tool phase breakdown
   | tool_name | queued_ms | mount_ms | exec_ms | capture_ms | publish_ms | release_ms |
   (per-tool stacked bar in MD via fenced ascii; same numbers in JSON)

## 4. Background tool calls
   | task_id | tool | started | duration_ms | status | delivery_latency_ms |
   - heartbeat coverage: <heartbeats_emitted>/<expected> = NN %
   - longest-running: <task_id> <duration_ms>

## 5. Plugin activity (generic, per plugin_id × plugin_kind)
   | plugin_id | plugin_kind     | sessions | tool_calls | p95_ms | peak_rss_bytes | errors |
   | lsp-py    | language_server |    3     |   188      |  42.0  |   312 MiB      |   0    |
   | ruff-d    | formatter       |    1     |    91      |   8.1  |    48 MiB      |   0    |
   | idx-1     | indexer         |    1     |    12      | 220.0  |   180 MiB      |   2    |

## 6. Overlay workspace — isolated vs ephemeral
   Side-by-side table (from Phase 1 schema). Includes:
     - total mount_ms, total cleanup_ms
     - upperdir_bytes p50/p95/max per mode
     - changed_path_count per mode
     - lifecycle distribution

## 7. LayerStack
   - leases: count, wait_ms p50/p95, hold_ms p50/p95
   - locks:  count, wait_ms p50/p95, hold_ms p50/p95
   - manifest depth over time (ascii sparkline)
   - squashes: triggered, completed, input_layers → result_layers

## 8. OCC
   - transactions: prepared, committed, rejected
   - conflict matrix: conflict_kind × count, top conflict paths
   - prepare_ms / apply_ms / commit_ms / publish_layer_ms p50/p95

## 9. Isolated workspace
   - handles: opened, closed, evicted
   - upperdir growth distribution
   - orphan counts after exit (MUST be 0 — release gate)
   - holder PID liveness after exit

## 10. OS resource (process / cgroup)
   - CPU: user / system / throttled (us, deltas over run)
   - Memory: rss peak, memory_peak_bytes per workspace
   - IO: read/write bytes & ops

## 11. Daemon audit pull
   - pull_count, empty_pull_count, events_pulled
   - dropped_event_count, lost_before_seq
   - max_buffer_pressure, final_cursor
   - puller CPU% (measured), puller wall-ms total

## 12. Audit path overhead (release gate)
   - daemon ring memory: max retained_bytes / max_bytes
   - daemon CPU attributable to audit: < 1 % p99 (gate)
   - runner CPU attributable to puller: < 0.5 % p99 (gate)
   - tool latency delta with vs without puller: < 1 ms p95 (gate)
   - artifact disk: live + rotated, total bytes

## 13. Warnings
   (audit dropped, pressure > 80 %, orphan counts > 0, upperdir > 80 %
    of cap, memory peak > threshold, OCC conflict cluster, lock_wait p95
    over threshold, squash failed-to-reduce)
```

#### Release gates (Phase 3 cannot ship without these passing)

1. **Audit overhead gate** — run the `layer_stack_occ_overlay` mock suite
   and one heavy live-e2e run twice (puller on / off), pinned to the V1
   reproducibility anchor `EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0`
   under `EOS_SANDBOX_PROVIDER=docker` +
   `EOS_ISOLATED_WORKSPACE_ENABLED=true`. Required:
   - tool-call wall-time p95 delta ≤ 1 ms
   - daemon RSS delta ≤ 16 MiB (ring + scratch)
   - runner CPU delta ≤ 0.5 % averaged over run
   - sandbox disk delta = 0 bytes
2. **Isolated workspace gate** — every isolated-workspace exit reports
   `orphan_holder_count == 0`, `orphan_cgroup_count == 0`,
   `orphan_scratch_count == 0`, `open_handle_count == 0` at run
   completion, and `holder_pid_alive == false` after exit.
3. **Drop-free pull gate** — `dropped_event_count == 0` and
   `lost_before_seq == 0` across the gate suite. If pressure > 0.8 is
   observed, raise the adaptive cadence floor before shipping; do not
   raise ring caps as a workaround.
4. **Artifact bound gate** — `sandbox_events.jsonl` rotation kicks in
   correctly during a synthetic 1 M-event run; total host-side footprint
   stays within `64 MiB + 8 × rotated` cap.

#### Tests for Phase 3

- `test_performance_report_md_layout_is_stable` — golden-file diff.
- `test_performance_report_json_contains_all_subsystem_sections`.
- `test_per_tool_phase_breakdown_matches_emitted_phases`.
- `test_overhead_gate_metrics_present_and_below_thresholds`.
- `test_isolated_workspace_gate_fails_on_synthetic_orphan`.
- `test_report_renders_without_lsp_specific_strings` — sanity check that
  `plugin` section is generic.

---

## Part 3 — Summary of How V2 Addresses Each Requirement

| Requirement | Addressed by |
| --- | --- |
| 1. States/events/resources for overlay (isolated vs ephemeral), layerstack, OCC, background tool calls | Phase 1 schema tables + Phase 3 report sections §2/§4/§6/§7/§8/§9/§10 |
| 2. Background tool calls + generic (non-LSP) plugin details | Phase 1 `background_tool.*` + `plugin.*` (keyed by `plugin_kind`); Phase 2 instrumentation lives in `plugins/core` and `engine/background`, not in `plugins/catalog/lsp`; report §4 + §5 |
| 3. Detailed per-tool time stats | Phase 1 `tool_call.phase` events; Phase 3 report §2 + §3 with p50/p95/p99 and per-phase breakdown |
| 4. Detailed performance & resource report | Phase 3 fixed MD layout (§1–§13) with structured JSON mirror; release-gate-grade |
| 5. Heartbeat/audit/pull is cheap; sandbox disk controllable; host log persistence managed | Phase 1 overhead budget table; Phase 2 disk contract (zero sandbox writes, bounded ring) + host-side rotation/gzip/retention; Phase 3 §11 + §12 + overhead release gate |
