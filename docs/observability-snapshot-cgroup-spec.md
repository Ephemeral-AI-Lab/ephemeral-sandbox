# Spec: Snapshot-only observability + two-level cgroup resource accounting

Status: draft · Scope: `sandbox-runtime`, `sandbox-daemon`, `sandbox-observability`,
`sandbox-manager` · Supersedes: the `OperationTrace` profiler and the
completed-execution projection pipeline.

## 0. Goals

1. **Remove** the in-process span/trace profiler and the completed-execution
   projection pipeline. Observability becomes a pure **live-state sampler**.
2. **Implement** real cgroup v2 resource accounting at **two scopes**:
   - **Sandbox-wide** — the whole sandbox (daemon + all workspaces).
   - **Per-workspace** — each workspace, aggregating the namespace executions
     nested under it.
3. Keep resources **keyed to workspace, not execution** (no per-execution rows).
   Namespace executions exist in the cgroup *tree* (for correct placement and
   cleanup) but are **not** reported as separate resource rows.

Non-goals: per-execution resource rows; CPU/memory *limits/throttling* (we only
*account*, we do not enforce); cgroup v1.

---

## 1. Why cgroup is stubbed today (context)

- `crates/sandbox-daemon/src/observability/cgroup.rs` contains only the
  `CgroupSample` struct and an `unavailable()` constructor — **no reader**.
- `cgroup` appears nowhere in `sandbox-runtime`/`namespace-execution`/
  `namespace-process`. The isolation model is **namespace-based**; no cgroup is
  ever created and no process is ever placed in one. Hence the hardcoded
  `CgroupSample::unavailable("cgroup path unavailable")` — there is no path.
- The schema, record, snapshot JSON, and the e2e "P1 cgroup verdict"
  (`sandbox-e2e-live-test/src/report.rs`) were built **contract-first**; the
  producer + reader were deferred. This spec fills that gap.

---

## Part A — Observability removal

### A.1 Delete (profiler + completed-execution pipeline)

**`sandbox-runtime/operation`**
- `observability.rs`: delete everything except `RuntimeObservabilitySnapshot`
  and `RuntimeWorkspaceSnapshot`. Removes `OperationTrace`, `SpanGuard`,
  `TraceState`, `SpanKey`, `span_keys`, `CompletedOperationTrace`,
  `CompletedOperationSpan`, `CommandFinalizationTraceMetadata`, `AsyncTraceSink`,
  `measure_optional*`.
- `operation.rs`: drop the `trace` param from `dispatch_operation` and the
  `OperationEntry.dispatch` fn-pointer type; remove the `measure_optional`
  wrappers and `operation_dispatch_span`.
- All handlers (`cli_definition/{command,workspace_session,layerstack}_operations.rs`,
  the no-op handlers carrying the param) and `command/service/exec_command.rs`,
  `layerstack/service/impls/squash.rs`: drop `trace`, unwrap `measure_*`.
- `command/finalize.rs`: delete `FinalizationTrace`, `emit_finalization_trace`,
  and the empty-closure measurement. **Keep** the finalization policy
  (workspace cleanup) in `build_on_complete`.
- `services.rs` / `command/service/core.rs`: delete the `*_with_async_trace_sink`
  constructors and the `async_trace_sink` field/getter.
- `lib.rs`: drop the seven trace re-exports; `dispatch_operation(operations, request)`.
- **Completed-execution buffer (now dead):** delete `NamespaceExecutionLedger`,
  `NamespaceExecutionRecord`, `CompletedNamespaceExecutionMeta`,
  `record_completed`/`drain`/`ack` from `namespace_execution.rs`; remove
  `completed_namespace_executions` from `RuntimeObservabilitySnapshot` and
  `ack_completed_namespace_executions` from `services.rs`. **Keep**
  `RuntimeNamespaceExecutionSnapshot` (active) and the `NamespaceExecutionId` /
  terminal-status re-exports.

**`sandbox-daemon`**
- `server/dispatch.rs`: call `dispatch_operation(&operations, &request)`; delete
  trace creation and the `insert_completed_operation_trace` block (this also
  removes a synchronous SQLite write from the request critical path).
- `server/runtime.rs`: `SandboxRuntimeOperations::from_config(...)` — drop sink wiring.
- `observability/service.rs`: delete `insert_completed_operation_trace`,
  `insert_completed_async_operation_trace`, `async_trace_sink`, the deep-span
  latch (`update_enabled_deep_span_keys`, `enable_*`, `DEEP_SPAN_*`, the
  `enabled_deep_span_keys` field), `span_records_for_trace`,
  `response_error_call_index`, the trace-id builders, the
  `completed_namespace_executions` loop + `ack` call, and
  `recent_trace_values`/`request_trace_value`/`namespace_trace_value`/
  `public_trace_id`. Drop `recent_traces` from `snapshot_value` and
  `include_recent_traces`/`trace_limit` from `snapshot_read_options`.
- `observability/namespace_execution.rs`: delete `trace_record`; keep
  `snapshot_record` (active executions).

**`sandbox-observability`**
- `records.rs`/`lib.rs`: delete `TraceRecord`, `SpanRecord`, and the
  namespace-execution trace record type.
- `store.rs`: delete `insert_trace` and `insert_namespace_execution_trace`; drop
  both `read_recent_*_traces` from snapshot assembly.
- `store/read.rs` + `store/rows.rs`: delete `read_recent_request_traces`,
  `read_recent_namespace_traces`, the `*_from_row` helpers,
  `ObservabilityRequestTraceRow`, `ObservabilityNamespaceExecutionTraceRow`, and
  the `recent_*_traces` fields. Keep `resource_window_ms`; drop
  `include_recent_traces`/`trace_limit` from `ObservabilitySnapshotReadOptions`.
- `store/schema.rs`: **do not edit existing migrations** (they are checksummed;
  editing throws `MigrationChecksumMismatch` at startup). Add migration **V7**:
  ```sql
  DROP TABLE IF EXISTS spans;
  DROP TABLE IF EXISTS traces;
  DROP TABLE IF EXISTS namespace_execution_traces;
  ```
  (plus their indexes).

**`sandbox-manager` + CLI**
- `get_observability_tree.rs`: passthrough survives; remove the dead
  `--include-recent-traces` / `--trace-limit` args and the `recent_traces`
  default. Keep `--resource-window-ms`.

**Tests**
- Delete `operation/tests/operation_trace.rs`. De-`trace`
  `operation/tests/{support/mod.rs,exec_command.rs}`. Trim
  `sandbox-daemon/tests/unit/observability.rs` (the `FROM spans` / trace cases)
  and `sandbox-observability/tests/schema.rs`. Update `sandbox-manager` +
  `sandbox-e2e-live-test` fixtures for the removed `recent_traces`.

### A.2 What remains after Part A

DB: `sandbox_snapshots`, `workspace_snapshots`,
`namespace_execution_snapshots`, `resource_samples` (+ `schema_migrations`).
Snapshot JSON: identical minus `recent_traces`. Data flow: engine registry
(active executions) + workspace session (workspaces) + resource samplers →
`write_snapshot()` → SQLite → snapshot RPC → manager aggregation.

---

## Part B — Two-level cgroup resource accounting

### B.1 Hierarchy

> **cgroups are NOT under `/eos`.** `/eos` is the EphemeralOS data/runtime root
> (config, socket, overlay dirs, the SQLite DB) — a regular filesystem. cgroups
> live only inside the **cgroup v2 mount at `/sys/fs/cgroup`**. The stored
> `cgroup_path` is always a `/sys/fs/cgroup/...` path.

cgroup v2, unified hierarchy. `R` = the daemon's own **delegated** cgroup,
discovered at startup from `/proc/self/cgroup` (the `0::/…` line → fs path
`/sys/fs/cgroup<path>`; inside a cgroup-namespaced container this is typically
`/sys/fs/cgroup`). We build only two levels under it:

```
R/                       ← SANDBOX scope (read here; daemon + all workspaces)
├── _daemon/             ← daemon's own processes (leaf; vacates R)
├── workspace-<wsid>/    ← WORKSPACE scope (read here; holds the workload directly)
└── workspace-<wsid>/
```

- **Sandbox-wide sample** (`workspace_id = NULL`): read `R`.
- **Per-workspace sample** (`workspace_id = <wsid>`): read `R/workspace-<wsid>`.
- **No per-execution cgroup.** Because we track only **active** workspaces +
  **live** executions and report at the workspace level (not per execution),
  every execution's process is placed **directly** into its workspace cgroup.
  The workspace cgroup is a leaf — no `exec-*` children. Live executions are
  enumerated via `namespace_execution_snapshots` (the active list), not cgroups.

This maps onto the **existing** `resource_samples` model unchanged — one
sandbox-root row + one row per active workspace. **No schema change** for the
two scopes.

**Accounting semantics:** the workspace cgroup persists for the workspace
session's lifetime. `memory.current` reflects currently-live processes;
`cpu.stat usage_usec` is cumulative since cgroup creation (rate is derived from
deltas across the `resource_samples` history). When the session is destroyed the
cgroup is removed, so a finished workspace leaves no row — consistent with
snapshot-only, live-state tracking.

### B.2 cgroup v2 constraints (must handle)

1. **Delegated-root discovery.** Parse `/proc/self/cgroup` line `0::<path>`;
   fs path = `/sys/fs/cgroup<path>` = `R`. The daemon runs inside the sandbox
   container, so `R` is the container's delegated subtree.
2. **"No internal processes."** A cgroup that enables controllers for children
   cannot also hold member processes. Only `R` has children, so only `R` must be
   vacated: at startup create `R/_daemon`, move the daemon
   (`echo <pid> > R/_daemon/cgroup.procs`), **then** enable controllers
   (`+cpu +memory` → `R/cgroup.subtree_control`). The `workspace-<wsid>` cgroups
   are **leaves** (no children), so they legally hold the workload processes
   directly and do **not** set `subtree_control`.
3. **Controller availability.** Only enable/read controllers listed in
   `R/cgroup.controllers`. If `cpu`/`memory` were not delegated, degrade to
   `CgroupSample::unavailable("controller not delegated: …")` — never error.

### B.3 Producer (creation, placement, cleanup)

Ownership respects the boundary law: cgroup *fs setup* is a runtime/execution
concern; the daemon owns only root discovery + its own move.

- **Daemon startup (`sandbox-daemon`):** discover `R`, create `R/_daemon`, move
  self, enable `subtree_control`. Expose `R` to the runtime via `ServerConfig`
  (new `cgroup_root: Option<PathBuf>`; `None` when cgroup is unavailable →
  whole feature degrades gracefully).
- **Workspace create (`operation/workspace_session/.../create_workspace_session.rs`):**
  given `R`, create the leaf `R/workspace-<wsid>` (no `subtree_control`). Record
  the path on the session model so it can surface in the snapshot (B.5).
- **Execution spawn (`namespace-execution` engine / `launcher.rs`):** place the
  spawned process into its workspace cgroup by one of:
  - *Baseline:* parent-side write of `child.id()` to
    `R/workspace-<wsid>/cgroup.procs` immediately after `command.spawn()`
    (membership inherits across the `ns-runner` re-exec, fork/exec, and `setns`).
    Simple; the brief pre-placement window is negligible for accounting.
  - *Race-free upgrade:* open the workspace `cgroup.procs` fd in the parent and
    write `"0"` from inside the existing `pre_exec` hook
    (`install_pgid_leader_hook`), or use `clone3(CLONE_INTO_CGROUP)`. Both are
    async-signal-safe with a pre-opened fd.
- **Cleanup:** `rmdir R/workspace-<wsid>` on **session destroy**
  (`workspace_session/.../destroy_session.rs`), after its processes are gone.
  `rmdir` requires the cgroup empty; a one-shot session's cgroup dies with the
  session, a persistent session's cgroup lives across its commands and is removed
  only when the session ends. No per-execution cgroup lifecycle to manage.

### B.4 Reader (sampling)

Replace the two `CgroupSample::unavailable(...)` call sites in
`observability/service.rs` (`resource_record(None, …)` and
`workspace_resource_record`) with a real reader in `cgroup.rs`:

| `CgroupSample` field | cgroup v2 source |
|---|---|
| `cpu_usage_usec` | `cpu.stat` → `usage_usec` |
| `memory_current_bytes` | `memory.current` |
| `memory_max_bytes` / `memory_max_unlimited` | `memory.max` (`"max"` ⇒ unlimited) |
| `cgroup_available` | all required files present + readable |
| `cgroup_path` / `cgroup_error` | the path read / first failure reason |

- Sandbox sample reads `R`. Per-workspace sample reads `R/workspace-<wsid>`,
  whose path comes from the snapshot (B.5).

### B.5 Data flow (one new field)

The daemon must know each workspace's cgroup path. The runtime created it and
owns it, so it carries it: add `cgroup_path: Option<PathBuf>` to
`RuntimeWorkspaceSnapshot` (sourced from the workspace session model). The
daemon reads that path for the per-workspace sample and stores it in the
existing `resource_samples.cgroup_path` column. No other schema change.

### B.6 Failure handling (hard requirement)

cgroup is **best-effort and must never block command execution.** If root
discovery, cgroup creation, or placement fails, the command still runs; the
sample degrades to `cgroup_available = false` with a precise `cgroup_error`.
This matches the existing `unavailable()` contract and the e2e P1 verdict's
"unavailable" branch.

### B.7 Config

`ServerConfig.cgroup_root: Option<PathBuf>` (or an explicit
`enable_cgroup_accounting: bool`). Default: auto-detect; absent/unwritable ⇒
disabled with a logged reason.

---

## Part C — Per-workspace live view (display)

The snapshot's canonical output (the manager `get_observability_tree` node) is a
**single live view**: each active workspace shows its **live namespace
executions and its live cgroup/disk resource usage together** in one node. This
is the surface that replaces the removed `recent_traces` — it reflects only
current state.

### C.1 Output shape

```jsonc
{
  "sandbox_id": "sbox-1",
  "lifecycle_state": "ready",
  "availability": "available",
  "sampled_at_unix_ms": 1750000000000,
  "resources": {                       // SANDBOX-wide — cgroup R + sandbox disk
    "latest": { "sample_delta_ms": …,
                "cgroup": { "available": true, "cpu_usage_usec": …,
                            "cpu_usage_delta_usec": …,
                            "memory_current_bytes": …, "memory_max_bytes": …,
                            "memory_current_delta_bytes": …,
                            "memory_max_unlimited": false, "error": null },
                "disk":  { "upperdir_bytes": …, "upperdir_delta_bytes": …, … } },
    "history": [ … ]                   // time-series from resource_samples
  },
  "workspaces": [{
    "workspace_id": "ws-abc",
    "lifecycle_state": "active",
    "resources": {                     // PER-WORKSPACE — cgroup R/workspace-ws-abc + workspace disk
      "latest": { "sample_delta_ms": …,
                  "cgroup": { "available": true, "cpu_usage_usec": …,
                              "cpu_usage_delta_usec": …,
                              "memory_current_bytes": …,
                              "memory_current_delta_bytes": …, … },
                  "disk":  { "upperdir_bytes": …, "upperdir_delta_bytes": …, … } },
      "history": [ … ]
    },
    "active_namespace_executions": [    // LIVE executions joined into the same node
      { "namespace_execution_id": "nsexec-123", "operation": "exec_command",
        "lifecycle_state": "running", "sampled_at_unix_ms": …, "error": null }
    ]
  }]
}
```

### C.2 Assembly

`resources` and `active_namespace_executions` are **already sibling keys** of the
workspace node in `observability/service.rs::workspace_value`. Part C requires no
new display code beyond Part B:

- **Live executions under workspace** — the existing join in `workspace_value`:
  `rows.active_namespace_executions.filter(|e| e.workspace_session_id ==
  workspace.workspace_id)` → `namespace_execution_value`.
- **Live resources under workspace** — `resource_bundle_value(Some(workspace_id),
  rows)`, whose `cgroup` block is now populated by the Part B reader against
  `R/workspace-<wsid>` (was always `unavailable`).
- **Sandbox-wide resources** — `resource_bundle_value(None, rows)`, `cgroup`
  populated by the reader against `R`.

So implementing Part B (reader + the `cgroup_path` field, B.4/B.5) lights up the
`cgroup` blocks at both scopes within this existing node; this section fixes that
combined node as the contract.

### C.3 Viewing it

```sh
sandbox-cli manager get_observability_tree
sandbox-cli manager get_observability_tree --sandbox-id sbox-1 --resource-window-ms 60000
```

Returns `{ "sandboxes": [ { "resources": {…}, "workspaces": [ { "resources":
{…}, "active_namespace_executions": [ … ] } ] } ] }` — each workspace's live
executions and live resource usage in one place.

### C.4 Notes

- **Live-only.** `active_namespace_executions` comes from the full-replace
  `namespace_execution_snapshots` table, so finished executions disappear on the
  next sample; `resources.latest` is the current cgroup reading. No history of
  completed executions is shown (consistent with Part A).
- **`lifecycle_state` is currently the constant `"running"`** for every active
  execution (`namespace_execution.rs::snapshot_record`). Enriching it (e.g.
  `starting`/`running`/`yielding`) is an optional follow-up: thread a real state
  through `RuntimeNamespaceExecutionSnapshot` instead of hardcoding it.
- **Read-only command space complexity.** For commands that only read from lower
  layers and have bounded output, writable overlay growth is effectively O(1)
  relative to repository/workspace size because lower-layer reads do not
  copy-up into `upperdir`. Output transcripts remain O(output size), process
  memory is whatever the command allocates, and metadata-changing operations
  (`chmod`, deletes, writes, renames) can still create upperdir entries.

---

## 2. Sequencing & coordination

1. **Part A first** (pure subtraction) — shrinks the surface cgroup work lands on.
2. **Part B** producer, then reader, then the new snapshot field.
3. **Coordination:** Part A's completed-execution removal and Part B's spawn-path
   placement both touch the `consolidate-namespace-execution-types` branch area,
   where a parallel worker is active (a file already moved mid-session). Sequence
   against their work; keep edits additive and localized.

## 3. Test & acceptance plan

- **Part A:** workspace builds trace-free; deleted/trimmed tests; manager + e2e
  snapshots pass without `recent_traces`.
- **Part B (unit):** cgroup reader parses `cpu.stat`/`memory.current`/
  `memory.max` from a temp cgroup-shaped fixture; degrades cleanly on missing
  files/controllers.
- **Part B (integration, Linux cgroup v2):** after an `exec_command`, the
  workspace cgroup shows non-zero `cpu_usage_usec`/`memory_current_bytes`, and
  the sandbox sample ≥ the workspace sample; the workspace cgroup is `rmdir`'d on
  session destroy and a destroyed workspace yields no further rows.
- **Part C (display):** `get_observability_tree` returns each workspace node with
  a populated `resources.latest.cgroup` block **and** its
  `active_namespace_executions` together; a workspace running a command shows ≥1
  live execution alongside non-zero workspace cgroup counters in the same node.
- **Acceptance:** the e2e **P1 cgroup verdict** flips from "unavailable" to
  "available" with real counters at both sandbox and workspace scope.

## 4. Risks & open questions

- **Delegation may omit `cpu`/`memory`.** Then per-workspace CPU/mem is
  impossible; we degrade gracefully. Confirm the container runtime delegates
  both controllers in the packaged (`xtask package`) environment.
- **Placement race** (baseline parent-side write): brief pre-placement window.
  Upgrade to `pre_exec`-fd-write / `CLONE_INTO_CGROUP` if strictness is required.
- **`rmdir` timing:** the workspace cgroup must be empty before removal; tie it to
  session destroy after the engine confirms no live executions remain.
- **Cumulative CPU across executions:** placing processes directly in the
  workspace cgroup means `cpu.stat` accumulates over all the session's commands.
  That is the intended "active workspace" semantics; per-execution CPU is out of
  scope.
- **Open:** should sandbox-wide read `R` (includes `_daemon` overhead) or the
  sum of workspace cgroups only? This spec reads `R` (honest whole-sandbox).
</content>
</invoke>
