# Daemon Audit Pull Consolidation — V3 (Index)

> **What this folder is:** the V3 plan for the daemon audit pull consolidation,
> split into one shared context document (this README) and three per-phase
> deliverable specs. Engineers picking up a phase should read this file
> first, then the phase file they own.

## Documents in this folder

| File | Scope |
|---|---|
| `README.md` (this file) | Lineage, RALPLAN-DR summary, V2 review, **cross-cutting contracts**, ADR, requirement traceability |
| `phase-1-audit-buffer-and-pull-rpc.md` | Bounded daemon ring + `api.audit.{pull,snapshot,reset_floor}` + frozen v1 schema + smoke emitters |
| `phase-2-emitters-and-puller.md` | Daemon emitters (layer_stack / overlay / occ / isolated_workspace / os_resource), runner puller, generic plugin + background tool instrumentation, normalizer, rotation/gzip |
| `phase-2-slice-1-report.md` | Implementation report for the slice-1 (foundation) cut of Phase 2 — puller library, normalizer, sink, layer_stack emitters |
| `phase-2.5-remaining-emitters-and-wiring.md` | Slice-by-slice plan that completes Phase 2's overall goal: overlay / isolated_workspace / occ / os_resource emitters, plugin shim, background tool, dispatcher slow-tail flush, puller-to-recorder wiring |
| `phase-2.5-implementation-report.md` | Implementation report for Phase 2.5 slices 1–6 (every emitter family + recorder puller wiring surface). Slices 7 and 8 deferred. |
| `phase-2.6-dispatcher-heavy-run-and-closers.md` | Phase 2.6 plan: closes Phase 2's overall goal — dispatcher slow-tail (slice 7), heavy-run regression (slice 8), plus four 2.5 closers (changeset_id, real isolated_workspace sampler cadence, PluginManifest.kind, async aclose). |
| `phase-2.6-implementation-report.md` | Implementation report for Phase 2.6 — slice 7, slice 8, and closers A/C/D/F. With this report, Phase 2 is closed; Phase 3 is the only remaining V3 work. |
| `phase-3-report-and-release-gates.md` | Consolidated performance & resource report (§1–§13), 4 release gates, default-on rollout |
| `phase-3-implementation-report.md` | Implementation report for Phase 3 — V3 report layout, release-gate evaluator harness, default-on opt-out env gate, engine dual-disable refusal. **With this report, V3 is code-complete**; remaining work is operational (gate evidence + K=5 countdown). |
| `phase-3-implementation-deferrals.md` | Code-side closers for Phase 3 — 16 deferrals (D1–D16) shipped as stubs / placeholders during Phase 3, each with file:line + verification recipe. Excludes live-e2e + operator hand-off items (those live in the implementation report and V3 §Follow-ups). |
| `phase-3-implementation-deferrals-report.md` | Implementation report for the 16 deferrals (D1–D16). With this report, every JSON field and Markdown column in the V3 §1–§13 report carries real data when its emitter is active — no remaining `"—"` / `0` placeholders. |

## Lineage

- **V1** = `docs/plans/daemon-audit-pull-consolidation-implementation-plan.md` (8 phases, original).
- **V2** = `docs/plans/daemon-audit-pull-consolidation-review-and-v2-plan.md` (3 phases, V1 review + rewrite).
- **V3** = `docs/plans/daemon-audit-pull-consolidation-v3-plan.md` (V2 + closure of 10 residual gaps). This folder is the **per-phase split** of V3; the single-file V3 plan remains the canonical historical record.

### Revision history
- **V3.0** — initial draft.
- **V3.1** — applied iteration-1 consensus-loop fixes from Architect (steelman + 4 named adjustments) and Critic (ITERATE verdict with P0/P1/P2 issues):
  - **P0 fixes:** corrected wrong file path (`isolated_workspace/manager.py` → `_control_plane/{pipeline_registry,pipeline_state,orphan_reaper,workspace_handle_lifecycle,linux_runtime}.py`); release-gate methodology now specifies warmup + N≥1000 + paired-bootstrap 95 % CI; resolved safety-gate-vs-toggle ambiguity (gate evaluated with puller on; runtime invariant added; engine refuses dual-disable when isolated_workspace is on).
  - **P1 fixes:** replaced `tool_call.phase` 1-in-N sampling with slow-tail buffered flush (Principle 3 upheld for outliers); `payload.daemon_event` now env-gated (`EOS_AUDIT_FORENSIC_RAW_ENABLED`, default off) + module boundary + CI lint test; Phase 1 acceptance now includes full causal-chain smoke; `background_tool.heartbeat` carries `background_task_id`; §5 plugin-table test switched from golden-file to schema-shape assertions.
  - **P2 fixes:** added pre-merge requirement to file follow-up tracking issues; renamed `peak_rss_*` → `peak_resident_*` (multi-process futureproof); added `retained_bytes`/`retained_events`/per-lane drop counters explicitly; added Pull RPC trust model + daemon-restart epoch handling sections.
- **V3.2 (iteration-2 polish)** — Architect re-review verdict: PROCEED; Critic re-review verdict: APPROVE (consensus reached). 3 micro-corrections applied: (a) per-`tool_name` lock semantics for dispatcher rolling-window; (b) `daemon.restart_observed` added to lane-assignment table (critical lane); (c) stale traceability-row label corrected (golden-file → schema-shape).
- **V3.3 (split)** — single-file V3 plan reorganized into per-phase files under `docs/daemon-audit-pull-consolidation-v3/`. Content is byte-equivalent to V3.2; only the layout changed.

### Phase progress

| Phase | Status | Implementation report |
|---|---|---|
| Phase 1 — bounded daemon ring + pull/snapshot/reset_floor RPCs + frozen v1 schema | ✅ landed | (no separate report; covered by tests in `test_sandbox/test_daemon`) |
| Phase 2 (slices 1–6) — daemon emitters + puller + normalizer + rotation | ✅ landed | [`phase-2-slice-1-report.md`](phase-2-slice-1-report.md), [`phase-2.5-implementation-report.md`](phase-2.5-implementation-report.md) |
| Phase 2.6 — dispatcher slow-tail (slice 7) + heavy-run regression (slice 8) + closers A/C/D/F | ✅ landed | [`phase-2.6-implementation-report.md`](phase-2.6-implementation-report.md) |
| Phase 3 — V3 report layout (§1–§13) + release-gate evaluator + default-on opt-out + dual-disable refusal | ✅ landed | [`phase-3-implementation-report.md`](phase-3-implementation-report.md) |
| Phase 3 deferrals — code-side closers D1–D16 (mount/publish phases, occ.prepare_ms, cgroup IO/throttle, ephemeral upperdir, started_seq, config knobs, artifact-bound gate verdict, forensic-raw deltas, ...) | ✅ landed | [`phase-3-implementation-deferrals-report.md`](phase-3-implementation-deferrals-report.md) |
| Release-gate evidence on dask-heavy live-e2e fixture | ⚠ operator hand-off | n/a (synthetic-event tests pin the evaluator math; live-fixture run is operational work) |
| FU#1 stream-bridge retirement (K=5 clean heavy runs → flip default) | ⚠ operational | n/a |

**With Phase 3's implementation report, V3 is code-complete.** Remaining work is operational: execute the 4-gate suite on the dask-heavy fixture, then start the K=5 retirement countdown for the stream-bridge.

---

## RALPLAN-DR Summary

### Principles
1. **Pull-only audit; bounded daemon ring; single canonical artifact.** Daemon never writes audit to disk. Pull RPC is O(returned events), never O(retained).
2. **One schema with subsystem section keys; generic by construction.** No section key contains a vendor or technology name. `plugin_kind` is a value, never a key.
3. **Causal chain over flat events.** Every write transaction carries `operation_id` + `lease_id` + `changeset_id` so the report reconstructs `lease → lock → changeset → commit → publish → release` without manual joins.
4. **Overhead is a release gate, not a hope. Disk is bounded at both ends.** Sandbox-side zero-write; host-side rotated + gzipped + retention-capped.

### Decision Drivers (top 3)
1. **Production safety of isolated-workspace exit.** Orphan holder PIDs / cgroups / scratch dirs are data-leak risks and the highest-blast-radius surface in the codebase.
2. **Bounded resource cost.** The audit path itself must not regress sandbox throughput; memory, CPU, and disk are all capped numerically.
3. **Future-proofness for plugins and background tools.** A new plugin kind (formatter daemon, indexer, MCP bridge, …) or a new background tool family must drop in without a schema bump or vendor-named field.

### Viable Options
- **A. V2 as-is (3-phase, dual-write).** Pros: zero delta from current draft; ready to execute. Cons: 10 residual gaps remain implicit (see Part 1); consumer-divergence risk between `payload.daemon_event` and promoted `payload.<section>`; phase-event budget math missing; cadence floor mechanism undefined.
- **B. V3 = V2 + closure of 10 residual gaps (recommended; this plan).** Pros: addresses all 5 user requirements at depth; explicit lane-assignment table; defined cadence floor; defined stream-bridge sunset; integrates with `EOS_TIER_RUN_ID`. Cons: more discipline at review time; lane-assignment + sampling rule are verbose.
- **C. 2-phase compression (ring+RPC+emitters together).** Pros: shorter timeline. Cons: merging ring and emitters loses the smoke-only Phase 1 safety net; ring and emitters are independently testable and should be reviewed separately. **Rejected.**

### Pre-mortem (3 scenarios — deliberate mode)
1. **Overhead gate fails on heavy live-e2e runs.** Tool-call p95 wall-time delta > 1 ms under puller-on vs puller-off comparison. Likelihood: medium. Blast radius: blocks ship of default-on toggle. Mitigation: adaptive cadence floor (`EOS_DAEMON_AUDIT_PULL_FLOOR_MS`) + per-tool phase sampling rule + `daemon_audit_pull.enabled=false` fallback so the rest of the consolidation still ships.
2. **`tool_call.phase` events flood the ring.** A 10 k-call run with 6 phases each = 60 k events > 50 k ring cap → critical lifecycle events evicted → orphan-detection invariants broken. Likelihood: high without explicit budget math. Blast radius: silent loss of safety evidence on long runs. Mitigation: `tool_call.phase` assigned to `sample` lane; **slow-tail buffered flush** at the dispatcher (always flush during cold window of first 100 calls per `tool_name`, then flush only when `total_ms ≥ P95` of rolling-window) — this combines bounded ring cost with full causal-chain preservation for outlier calls (Principle 3 upheld for the slow tail); critical lane reserved for isolated_workspace lifecycle; `tool_call.finished.phase_totals_rollup` always populated from in-process timers so per-tool aggregate stats survive even when phase events are not flushed.
3. **Plugin session emission has no clear emit site.** The current loader (`backend/src/plugins/core/loader.py`) is an import-time singleton (`_LOAD_CACHE: dict[Path, list[BaseTool]]`) with no native per-invocation lifecycle. Likelihood: certain (already true today). Blast radius: design hole — V2's `plugin.session_started/stopped` events would have no real emit point. Mitigation: V3 drops `plugin.session_*` from the v1 schema; emits `plugin.tool_invoked` / `plugin.tool_completed` / `plugin.error` only; defers real plugin session model to a follow-up plan; `plugin.session_*` can be added additively later without a schema bump.

### Expanded Test Plan
- **Unit:** ring eviction priority (critical survives sample-pressure flood); pull cursor exclusive semantics; pressure formula; lane assignment per event family; phase-event budget math; plugin event genericness check (grep for `"lsp"` / `"pyright"` as keys → must be 0).
- **Integration:** puller + emitters end-to-end against a mock daemon; dedupe correctness across stream + pull (pull supersedes when both present); rotation/gzip/retention on a synthetic 100 MiB run; EOS_TIER_RUN_ID artifact-path stability.
- **E2E:** live e2e heavy run with `EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0` under `EOS_SANDBOX_PROVIDER=docker` + `EOS_ISOLATED_WORKSPACE_ENABLED=true`; puller-on vs puller-off comparison for the overhead gate; isolated-workspace orphan gate at run completion.
- **Observability:** harness snapshots `audit_buffer` stats every 10 s; assert `max_buffer_pressure < 0.8` across full run; assert `orphan_holder_count == orphan_cgroup_count == orphan_scratch_count == 0` at every isolated_workspace exit; assert `dropped_event_count == 0` and `lost_before_seq == 0` end-to-end.

---

## Part 1 — Review of V2

### V2 strengths (kept verbatim in V3)
- 3-phase shape (Ring+RPC / Emitters+Puller / Report+Gates) is the correct cut.
- Causal-chain principle (`lease_id` + `changeset_id` + `operation_id` on every transaction event) is correct and reuses the project's [[project_ephemeralos_layerstack_occ_design]] story.
- Disk contract (zero sandbox writes + host rotation/gzip/retention) is the right model.
- Generic plugin section keyed by `plugin_kind` is the correct shape.
- `BackgroundTaskStatus` lattice reuse + existing 60 s heartbeat reuse (no new threads) is correct.
- Side-by-side ephemeral-vs-isolated workspace property table is excellent.
- Per-tool phase events (`tool_call.phase`) is the right answer to "where did time go".
- Release gates (overhead + isolated-workspace orphan) are well-chosen.

### V2 residual gaps (closed by V3)

| # | Gap in V2 | V3 closure |
|--:|---|---|
| 1 | Schema evolution rule missing — V2 says "Frozen event schema v1" but no bump policy | V3 §Schema contract: additive=v1; rename/remove=v2; consumers reject unknown majors |
| 2 | Dual-write authoritativeness ambiguous — both `payload.daemon_event` and promoted `payload.<section>` exist; who reads which? | V3 §Dual-write authoritativeness: `payload.<section>` is the consumer surface; `payload.daemon_event` is forensic-only |
| 3 | `tool_call.phase` budget math missing — 6 phases × 10 k calls = 60 k events > 50 k ring cap | V3 §Lane assignment (phase events on `sample` lane) + slow-tail buffered flush at the dispatcher (cold window + P95 slow-tail); `tool_call.finished.phase_totals_rollup` always populated; budget estimate ≤ ~32 k events on a 10 k-call run |
| 4 | Plugin session lifecycle mismatch — loader is import-time singleton; `plugin.session_*` events have no emit site | V3 drops `plugin.session_*` from v1 schema; keeps `plugin.tool_invoked/completed/error` only; defers session model to follow-up |
| 5 | Lane assignment table missing — V2 mentions critical/normal/sample but doesn't map event families | V3 §Lane assignment: full table mapping every event family to a lane with rationale |
| 6 | Adaptive cadence floor mechanism missing — V2 says "raise the floor" but doesn't define the floor | V3 defines `EOS_DAEMON_AUDIT_PULL_FLOOR_MS` (default 100 ms) + pressure-based escalation rule |
| 7 | Buffer pressure formula undefined — V2 reports `pressure: 0.91` but never says how it's computed | V3 specifies `pressure = max(retained_bytes/max_bytes, retained_events/max_events)` |
| 8 | Stream-bridge fallback sunset undefined — V1's Decision #6 keeps it indefinitely | V3 defines retirement gate (K=5 consecutive clean heavy runs → flip default); full removal is a follow-up phase out of scope |
| 9 | `EOS_TIER_RUN_ID` integration unstated — referenced in memory but not the plan | V3 §Disk contract: rotated artifacts live under EOS_TIER_RUN_ID-stable paths |
| 10 | Per-mode timing rollup missing — req #3 says "time per tool"; V2 aggregates across modes | V3 §Report layout §2: per-tool tables explicitly split by `workspace_mode` (default / ephemeral / isolated) |

---

## Cross-cutting contracts (apply to all 3 phases)

These contracts are frozen in Phase 1, consumed in Phase 2, and verified in Phase 3.
Phase files reference this section by anchor (`README.md#schema-contract` etc.)
rather than duplicating — single source of truth.

### Schema contract
- Schema identifier: `sandbox.daemon.audit.pull.v1`
- **Additive changes** (new field on existing section, new event name, new subsystem section) → stay v1
- **Breaking changes** (rename or remove a field, change a field's semantics) → bump to v2
- Consumers MUST reject unknown major versions explicitly; current consumers reject `v2+` until updated

### Subsystem section keys (frozen at v1)
`daemon`, `layer_stack`, `overlay_workspace`, `occ`, `isolated_workspace`, `os_resource`, `plugin`, `background_tool`, `tool_call`

### Dual-write authoritativeness (env-gated forensic raw)
- `payload.<section>` (promoted, structured) = **consumer surface, always written**. Report builder, downstream notebooks, live health checks MUST read from here. This is the only authoritative view.
- `payload.daemon_event` (verbatim raw) = **forensic-only, opt-in**. Written ONLY when `EOS_AUDIT_FORENSIC_RAW_ENABLED=true` (default: `false`). Used for manual audit replay and debugging when the promoted view looks wrong. Operators flip the env var per-run when investigating a specific incident.
- Consumer-divergence enforcement (closes Architect A2 / Critic P1):
  1. Module-boundary: the normalizer (`task_center_runner/audit/sandbox_events.py`) is the only writer of `payload.daemon_event`.
  2. CI lint rule: a repo-level grep job fails CI if any file outside `task_center_runner/audit/sandbox_events.py` or test files references `payload["daemon_event"]` / `payload.get("daemon_event")` / `["daemon_event"]` outside an opt-in test fixture.
  3. Default-off test: `test_no_consumer_reads_daemon_event_under_default_config` runs a full mock suite with `EOS_AUDIT_FORENSIC_RAW_ENABLED` unset and asserts `daemon_event` key absent from every recorded payload.
  4. Negative test: `test_report_consumer_reads_promoted_payload_section_not_daemon_event` corrupts `payload.daemon_event` (with the env enabled); asserts the report is unchanged.

### Buffer pressure formula and tracked counters

```
pressure = max(retained_bytes / max_bytes, retained_events / max_events)
```

Audit-buffer tracked counters (all reported in every pull response under `buffer`):
- `retained_events` — count of events currently in the ring (across all lanes)
- `retained_bytes` — sum of encoded-size estimate of events currently in the ring
- `max_events`, `max_bytes` — configured caps
- `pressure` — derived from formula above
- `dropped_event_count` — total events evicted since daemon boot
- `dropped_event_count_by_lane` — `{critical: int, normal: int, sample: int}`
- `lost_before_seq` — exclusive lower bound; events with `seq < lost_before_seq` are no longer retrievable

Reported in every pull response under `buffer`. The puller raises its cadence floor when `pressure > 0.8` sustained for 3 consecutive pulls.

### Lane assignment

Every emitted event belongs to exactly one lane. Eviction priority: `sample` evicted first, then `normal`, then `critical`. Lane assignment is part of the schema (changing a lane is a v2 break).

| Event family | Lane | Rationale |
|---|---|---|
| `daemon.{started,stopped,audit_buffer_pressure}` | critical | self-observability of the audit path |
| `daemon.restart_observed` (synthesized by puller on epoch boundary) | critical | epoch boundary is unconditionally consequential for report correctness |
| `isolated_workspace.{entered,exited,evicted,orphan_check_completed,orphan_reaped}` | critical | exit safety / orphan-detection invariants |
| `overlay_workspace.{mounted,published,cleaned,cleanup_failed}` | critical | lifecycle proof per tool call |
| `layer_stack.{squash_triggered,squash_completed,squash_failed}` | critical | manifest depth invariants |
| `occ.conflict_rejected` | critical | OCC stale-base evidence (debugging concurrent writes) |
| `background_tool.{started,completed,failed,cancelled,delivered}` | normal | terminal-state events for long-running tools |
| `layer_stack.{lease_requested,lease_acquired,lease_released,lock_acquired,snapshot_prepared}` | normal | timing data |
| `occ.{changeset_prepared,transaction_lock_acquired,apply_committed,publish_layer}` | normal | timing data |
| `plugin.{tool_invoked,tool_completed,error}` | normal | plugin observability |
| `tool_call.{started,finished}` | normal | per-tool envelope (always present) |
| `isolated_workspace.sampled` (500 ms cadence) | sample | periodic; tolerable to drop under pressure |
| `os_resource.sampled` (heartbeat cadence) | sample | periodic; tolerable to drop under pressure |
| `background_tool.heartbeat` (60 s cadence) | sample | periodic; tolerable to drop under pressure |
| `plugin.peak_resident_sampled` | sample | periodic |
| `tool_call.phase` | sample (with per-tool sampling rule) | high volume — 6 phases × N calls |

### Per-tool phase sampling rule (slow-tail buffered flush)

Goal: bounded ring cost AND complete causal chain preserved for the slow tail (Principle 3 upheld for outlier debugging).

Mechanism:
- The dispatcher (`engine/tool_call/dispatch.py`) maintains a thread-local **phase buffer** during each tool call: a fixed-size ring of `{phase, timestamp, duration_ms}` records (max 6 entries — one per phase). Cost: ~96 bytes per in-flight call.
- The dispatcher also maintains a per-`tool_name` rolling-window of the last 100 finished calls' `total_ms` (in-process; ~800 bytes per active tool_name). Each rolling window is protected by a per-`tool_name` lock; contention is acceptable because the critical section is O(1) under a fixed-size deque (append + drop-oldest + P95 lookup via an auxiliary sorted structure).
- On `tool_call.finished`, the dispatcher decides whether to flush the phase buffer to the daemon ring:
  - **Cold window:** if rolling-window has fewer than 100 samples for this `tool_name`, ALWAYS flush (cold-start coverage).
  - **Slow tail:** if `total_ms ≥ P95(rolling-window)`, ALWAYS flush (captures the slowest ~5% of calls).
  - **Otherwise:** discard the phase buffer (the call's aggregate is still captured via `phase_totals_rollup` on `tool_call.finished`).
- All flushed `tool_call.phase` events go on `sample` lane (evicted last under critical-lane pressure).
- Always emit `tool_call.started` and `tool_call.finished` on `normal` lane — envelope is unconditional.
- `tool_call.finished.phase_totals_rollup` is a map `{queued_ms, mount_ms, exec_ms, capture_ms, publish_ms, release_ms}` computed from in-process timers (NOT from emitted phase events). Per-tool aggregate p50/p95/p99 reports are accurate even when phase events are flushed out.

Why slow-tail instead of 1-in-N: the slow tail is exactly where causal-chain reconstruction matters (join against `layer_stack.lock_acquired`, `occ.transaction_lock_acquired`, etc.). 1-in-N would drop the very calls that need investigation. Slow-tail captures the outliers without flooding the ring on hot tools.

Definition of `total_ms` for the gate: wall-clock from `tool_call.started` to `tool_call.finished`, measured via `monotonic_now()`.

Budget estimate (10 k tool-call run, 50 distinct tool_names, 200 calls/tool average):
- Cold window flushes: 50 × 100 = 5,000 calls × 6 phases = 30,000 phase events
- Slow-tail flushes (after warmup): 50 × 100 calls × 5 % × 6 phases = 1,500 phase events
- Total per run: ~31,500 phase events (well within 50,000 ring cap; combined with `normal`+`critical` lanes leaves headroom).

### Adaptive cadence policy with floor enforcement

Floor: `EOS_DAEMON_AUDIT_PULL_FLOOR_MS` (default 100 ms) — the puller never polls faster than this regardless of pressure or workspace mode.

Pressure-based floor escalation:
- If `pressure > 0.8` sustained for 3 consecutive pulls → raise floor by 50 % (cap at 1000 ms).
- Floor is never auto-lowered. Operators can manually reset via `api.audit.reset_floor` (gated by `EOS_DAEMON_AUDIT_ALLOW_FLOOR_RESET=true`).

Interval table (target intervals are clamped to floor):

| Condition | Target interval | Notes |
|---|---:|---|
| active run, default | 1 s | normal cadence |
| idle (no inflight) | 5 s | background heartbeat dominates |
| isolated workspace active | 500 ms | catch orphan / holder drift fast |
| buffer pressure ≥ 0.8 | 250 ms | drain before eviction |
| final drain (puller stop) | until empty or 3 s cap | bounded teardown |

### Disk & log persistence contract

- **Sandbox side:** zero disk writes for the audit path. Ring is in-memory only. Upperdir size queries are TTL-cached and bounded; full tree walks are forbidden. Per-sandbox disk usage is whatever the workload writes — audit adds nothing.
- **Host side:** `sandbox_events.jsonl` rotates at 64 MiB. Gzip on rotation. Retention cap: `EOS_AUDIT_ARTIFACT_RETENTION_FILES` (default 8). Worst-case footprint per run ≈ 64 MiB live + 8 × ~10 MiB compressed ≈ 150 MiB. `performance_report.{json,md}` written once post-run, no rotation needed.
- **Artifact stability:** rotated `sandbox_events.jsonl.gz.N` files live under the EOS_TIER_RUN_ID-stable artifact path (per `eos_tier_run_id_artifact_stability` invariant), so `run_tiered.py`'s resume-on-restart contract holds without modification.
- **Retention beyond a single run:** inherits existing run-directory GC; no new retention policy added in this plan.

### Stream-bridge fallback sunset

V1 Decision #6 keeps stream-derived sandbox events as a fallback alongside daemon-pulled events; V2 inherits this without a retirement gate. V3:

- Retirement gate: after Phase 3 ships, if `dropped_event_count == 0` AND `lost_before_seq == 0` across **K = 5 consecutive heavy live-e2e runs** (one per week minimum), flip `EOS_AUDIT_STREAM_FALLBACK=false` as default.
- Stream-bridge code removal is a **follow-up phase OUT OF SCOPE** for this plan; a tracking issue MUST be filed before this plan merges (see ADR §Follow-ups).

### Pull RPC trust model

- `api.audit.pull`, `api.audit.snapshot` — trusted-transport. Daemon and runner share the in-sandbox AF_UNIX socket; no per-call authentication. The transport's filesystem permissions (socket file `0600`, owned by the sandbox user) are the authentication boundary, the same model used by every other daemon RPC.
- `api.audit.reset_floor` — operator escape hatch, NOT a security boundary. Gated by `EOS_DAEMON_AUDIT_ALLOW_FLOOR_RESET=true` env check at handler entry. The env gate exists to prevent test-suite or automation accidents, not to defend against malicious callers (if a caller can reach the AF_UNIX socket, they already have the same trust level as the runner).

### Daemon-restart epoch handling

The audit ring is in-memory; daemon restart loses all retained events. The puller MUST observe and report this cleanly:

- On daemon boot, the ring assigns a `boot_epoch_id` (e.g., monotonic-clock value at start) and reports it in every pull/snapshot response under `snapshot.daemon.boot_epoch_id`.
- The puller tracks the last-seen `boot_epoch_id` alongside its cursor.
- If the next pull returns a different `boot_epoch_id`, the puller treats this as an epoch boundary: it sets its local cursor to 0, records `boot_epoch_boundary_observed=true` in puller stats, increments `daemon_restarts_observed`, and resumes pulling from the new epoch's seq=0.
- Events from the previous epoch are not re-pulled (they are lost). Report §11 shows `daemon_restarts_observed` so a heavy run with a daemon crash is visible to the reader.

---

## Architectural Decision Record

**Decision:** Implement V3 plan (V2 + closure of 10 residual gaps) in 3 phases.

**Drivers:**
1. Production safety of isolated-workspace exit (highest blast radius — orphan PIDs/cgroups/scratch are data leaks).
2. Bounded resource cost — the audit path must not regress sandbox throughput.
3. Future-proofness for new plugin kinds and tool families.

**Alternatives considered:**
- **V2 as-is** — rejected; 10 residual gaps remain implicit (consumer divergence between `daemon_event` and promoted sections; cadence floor undefined; phase-event budget math missing; plugin lifecycle mismatch unaddressed; stream-bridge sunset undefined).
- **2-phase compression** — rejected; merging ring+RPC with emitters loses the smoke-only Phase 1 safety net and concentrates risk; ring and emitters are independently testable and deserve separate review.
- **Add real plugin session lifecycle now** — rejected; current loader is import-time singleton; introducing a session abstraction is a multi-file refactor outside this plan's scope; V3 accepts the regression in session-level observability and explicitly defers to a follow-up.

**Why chosen (V3):**
- Closes consumer-divergence by declaring `payload.<section>` authoritative (gap #2).
- Closes phase-event budget hole with lane assignment + per-tool sampling (gap #3).
- Closes plugin lifecycle hole by dropping `plugin.session_*` from v1 and deferring honestly (gap #4).
- Closes cadence runaway by defining `EOS_DAEMON_AUDIT_PULL_FLOOR_MS` + escalation (gap #6).
- Closes pressure-formula ambiguity (gap #7).
- Defines stream-bridge sunset gate (gap #8).
- Integrates with `EOS_TIER_RUN_ID` artifact stability (gap #9).
- Splits per-tool reports by `workspace_mode` so "is `edit_file` slower in isolated mode?" is directly answerable (gap #10).

**Consequences:**
- Phase 1 is purely additive (ring + RPC + schema, no instrumentation) — easy to revert if needed.
- Phase 2 touches many files (one PR per subsystem) — biggest review surface; mitigated by per-subsystem PR splits.
- Phase 3 carries hard release gates that could block ship if overhead is too high. Mitigation: `daemon_audit_pull.enabled=false` fallback toggle lets the rest of the consolidation ship even if the default-on rollout is deferred.
- Stream-bridge code remains in V3; retirement is a follow-up plan.
- Plugin session lifecycle remains undelivered; follow-up plan needed.
- `tool_call.phase` slow-tail flush captures full causal chain for the slowest ~5 % of calls per tool but discards phase events for fast-path calls. Aggregate per-tool stats remain accurate via `phase_totals_rollup` on `tool_call.finished`. Long-tail debugging ("why was *this* call slow?") is fully supported; uniform sampling debugging ("show me phase boundaries for every call") is not — and is an explicitly accepted tradeoff in service of Principle 4 (bounded resource cost).

### Follow-ups (out of scope for this plan)

**Pre-merge requirement** (closes Critic P2): the following tracking issues MUST be filed and linked in this ADR BEFORE this plan merges. Without filed issues, "follow-up" becomes "permanent regression".

1. **Stream-bridge code removal** after retirement gate passes (K=5 consecutive clean heavy runs). Issue title: `[Audit] Remove stream-bridge fallback after K=5 clean runs (post-V3)`. Trigger: retirement gate observed in 5 weekly heavy runs.
2. **Real plugin session model** + `plugin.session_*` events (additive, no schema bump). Issue title: `[Plugins] Introduce per-workspace plugin session lifecycle`. Trigger: any second plugin kind added to `plugins/catalog/` (forces the question).
3. **Plugin-kind catalog expansion** (Ruff daemon, `tsc --watch`, mypy daemon — each new kind drops into the existing schema). No specific trigger; opportunistic.
4. **Ring-by-lane separation** if audit overhead gate fails permanently — investigate per-subsystem ring sharding. Trigger: overhead gate failure post-ship.
5. **`mount` / `publish` phase recording** from `sandbox/overlay/lifecycle.py` and `sandbox/occ/service.py` via the `engine.tool_call.phase_buffer.record_phase` API. Phase 2.6 slice 7 wired the four framework-boundary phases (`queued` / `exec` / `capture` / `release`); the two remaining phases live below the framework and need a one-call hook at each emit site. Issue title: `[Audit] Surface mount/publish phases via record_phase from overlay+OCC`. Trigger: Phase 3 report §2 renders "—" for the two columns until this lands (per phase-3 spec; not a regression). Schema is additive (no new event family); per-tool-name rolling P95 / slow-tail decision is unaffected because the four phases the framework already records dominate envelope total_ms; rollup picks up the two extras automatically once the call sites are added.
6. **Plan-doc cosmetic corrections** — (a) `phase-2.5-remaining-emitters-and-wiring.md` §slice-3 file list still names `sandbox/daemon/occ_runtime_services.py` and `sandbox/daemon/changeset_projection.py`; the actual emitters live in `sandbox/occ/service.py`. (b) `phase-2-emitters-and-puller.md` §Deliverable 6 names `task_center_runner/audit/sandbox_events.py`; slice 1 split it into `daemon_event_normalizer.py` + `sandbox_events_sink.py`. Both are doc-only fixes — code already ships against the real file names and CI lints enforce the contracts. Trigger: opportunistic; no functional impact.

Each follow-up issue references this plan by path and version (V3) so future readers can trace the original decision context.

---

## Requirement traceability

| User requirement | Addressed by |
|---|---|
| **1.** States/events/resources for overlay (isolated vs ephemeral), layerstack, OCC, background tool calls | Phase 1 schema tables (ephemeral-vs-isolated property table; `layer_stack`, `occ`, `background_tool` event families with operation_step + lease_id + changeset_id + manifest_root_hash); Phase 3 report §2 / §4 / §6 / §7 / §8 / §9 / §10 |
| **2.** Background tool calls + GENERIC (non-LSP) plugin details | Phase 1 `background_tool.*` family (reuses existing `BackgroundTaskStatus` lattice and existing 60 s heartbeat); `plugin.*` family keyed by `plugin_id` + `plugin_kind` (values: `language_server`, `formatter`, `indexer`, `build_daemon`, `mcp_bridge`, `custom`); Phase 2 instrumentation lives in `backend/src/plugins/core/loader.py` and `backend/src/engine/background/task_supervisor.py` — NOT in `backend/src/plugins/catalog/lsp/`; `test_plugin_events_are_kind_generic` and `test_report_renders_without_lsp_specific_strings` enforce no LSP-named keys; Phase 3 report §4 + §5 |
| **3.** Detailed per-tool time stats | Phase 1 `tool_call.{started,phase,finished}` schema with `phase_totals_rollup` always populated; Phase 2 slow-tail buffered phase flush (cold window + P95 slow-tail) + always-emit envelope; Phase 3 report §2 (per-tool, **per-workspace-mode**, p50/p95/p99 across all 6 phases) + §3 (top-10 phase breakdown). Per-call causal chain preserved for the slowest ~5 % of calls per tool (Principle 3 upheld for outlier debugging). |
| **4.** Detailed performance & resource report | Phase 3 fixed Markdown layout §1–§13 with structured JSON mirror; release-gate-grade; `test_performance_report_md_layout_structure` schema-shape assertions (column-regex + `plugin_kind ∈ enum`); plus `test_performance_report_json_contains_all_subsystem_sections` |
| **5.** Heartbeat / audit / pull cheap; sandbox disk controllable; host log persistence managed | Phase 1 overhead budget table (8 MiB ring, < 1 % p99 daemon CPU, zero new threads, snapshot O(1)); Phase 2 disk contract (zero sandbox writes; host rotation at 64 MiB + gzip + 8-file retention; EOS_TIER_RUN_ID-stable artifact paths); Phase 3 §11 + §12 + overhead release gate with explicit fallback toggle (`daemon_audit_pull.enabled=false`) |

---

*End of index. Open the phase files for deliverables, tests, and acceptance criteria.*
