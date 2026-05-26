# Phase 3 — Implementation Deferrals (Code-side closers)

> **Status:** ALL 16 deferrals (D1-D16) **landed** on 2026-05-26. See
> [`phase-3-implementation-deferrals-report.md`](phase-3-implementation-deferrals-report.md)
> for the implementation report.
>
> **Scope:** Implementation work that was shipped as a stub / placeholder
> during Phase 3 and that the next agent can land **without any live-e2e
> infrastructure or operator hand-off**. Items requiring the dask-heavy
> live fixture, the K=5 stream-bridge retirement countdown, or
> Linear/GitHub issue filing are tracked separately in the V3 README
> §Follow-ups and the [Phase 3 implementation report](phase-3-implementation-report.md)
> §Deferred items.
>
> **Goal of closing these items:** every JSON field and MD column in the
> V3 §1-§13 report carries real data; no remaining "—" / `0` /
> `_percentile_record([])` sentinels except those that are genuinely
> impossible to populate from the current emitter set.
>
> **Author:** Phase 3 implementation, 2026-05-26.

## Quick index

| Priority | ID | Closes |
|---|---|---|
| P1 | [D1](#d1) | §2 + §3 mount/publish columns now render `"—"` |
| P1 | [D2](#d2) | §8 OCC `prepare_ms` always zero |
| P1 | [D3](#d3) | §10 OS resource IO/throttle counters always zero |
| P1 | [D4](#d4) | §6 ephemeral `upperdir_bytes` empty |
| P2 | [D5](#d5) | §4 background tool `started_at` is seq, not timestamp |
| P2 | [D6](#d6) | §13 `os_resource.memory_peak` threshold is a magic constant |
| P2 | [D7](#d7) | §13 upperdir-cap warning can never fire (input not aggregated) |
| P2 | [D8](#d8) | §1 `event_count` vs §11 `events_pulled` divergence undocumented |
| P2 | [D9](#d9) | `evaluate_artifact_bound_gate` exposed but no live caller |
| P2 | [D10](#d10) | `_phase_bar` glyph width clamp truncates rightmost phases |
| P2 | [D11](#d11) | `_section_per_tool_timing` collects `queued_ms_direct` from no emitter |
| P3 | [D12](#d12) | Engine dual-disable refusal only fires from `run_pipeline` |
| P3 | [D13](#d13) | `RunnerConfig.daemon_audit_pull` only models `enabled` (no floor / stream-fallback fields) |
| P3 | [D14](#d14) | §12 overhead methodology zeros are ambiguous (no `methodology_present` sentinel) |
| P4 | [D15](#d15) | Forensic-raw delta surfacer for §13 (debug-mode) |
| P4 | [D16](#d16) | Plan-doc cosmetic corrections (FU#6 from V3 README) |

Each item below is self-contained: file path, what's there now, what
needs to land, and how to verify.

---

## P1 — Schema-additive emitter closers

These items close real "—" / 0 placeholders in the V3 report. Each is
**schema-additive** (adds an optional field to an existing section, or
adds a new event type within an existing family) — no v2 schema bump.

<a id="d1"></a>
### D1 — Surface `mount` and `publish` phases via `record_phase`

| What | Where |
|---|---|
| Stub today | `backend/src/task_center_runner/audit/performance_report.py:54-62` (`_PHASE_ORDER`) — six phases declared; the framework only records four (queued / exec / capture / release). §2's `mount_ms` and `publish_ms` columns render `"—"` via `_format_phase_cell` (`backend/src/task_center_runner/audit/performance_report.py:1696-1707`). |
| What to land | Call `engine.tool_call.phase_buffer.record_phase("mount", duration_ms)` from `backend/src/sandbox/overlay/lifecycle.py` at the overlay mount boundary, and `record_phase("publish", duration_ms)` from `backend/src/sandbox/occ/service.py` at the publish-layer boundary. The `phase_buffer.record_phase` API is already exposed (Slice 7). |
| Verify | Extend `backend/tests/unit_test/test_task_center_runner/test_performance_report_v3.py::test_per_tool_phase_breakdown_matches_emitted_phases` to synthesize a `tool_call.finished` with a populated `mount_ms` / `publish_ms` rollup; assert the §2 table renders numeric values (not `"—"`). |
| Acceptance | A foreground `write_file` call's §2 row populates all 6 phase columns; the §3 ASCII bar shows all 6 glyph types. |

This is FU#5 from the V3 README §Follow-ups, restated here with concrete file:line.

<a id="d2"></a>
### D2 — Emit `occ.prepare_ms` from the OCC service

| What | Where |
|---|---|
| Stub today | `backend/src/task_center_runner/audit/performance_report.py:855` — `"prepare_ms": _percentile_record([]),  # not recorded directly today` |
| What to land | (a) Add `prepare_ms: float \| None = None` to `OccSection` in `backend/src/sandbox/daemon/audit_schema.py`. (b) Emit it from `backend/src/sandbox/occ/service.py` on the prepare→ack boundary (timer scope around `changeset_preparation.prepare_sync`). (c) Update `_section_occ` to call `_samples(indexed.get("occ.changeset_prepared", []), "occ", "prepare_ms")`. |
| Verify | Synthetic `occ.changeset_prepared` row with `prepare_ms=12.5` → §8 `prepare_ms.p50 == 12.5`. |
| Acceptance | §8 `prepare_ms` percentile record is non-empty after a single OCC apply cycle. |

<a id="d3"></a>
### D3 — Surface cgroup IO + CPU-throttle counters on `os_resource.sampled`

| What | Where |
|---|---|
| Stub today | `backend/src/task_center_runner/audit/performance_report.py:954-963` — `throttled_us_delta`, `read_bytes`, `write_bytes`, `read_ops`, `write_ops` hardcoded `0`. |
| What to land | (a) Add the following fields to `OsResourceSection` in `backend/src/sandbox/daemon/audit_schema.py`: `cpu_throttled_us`, `io_read_bytes`, `io_write_bytes`, `io_read_ops`, `io_write_ops`. (b) Populate from `cgroup_v2_reader` (whichever sampler currently writes `OsResourceSection`). (c) In `_section_os_resource`, compute deltas across the first and last samples (same `[-1] - [0]` pattern as `cpu_user_s_delta`). |
| Verify | Add a section-builder unit test that feeds two `os_resource.sampled` rows with monotonic IO counters → §10 `io.read_bytes` reflects the delta. |
| Acceptance | A real heavy-run report shows non-zero §10 IO counters when the workload reads/writes. |

<a id="d4"></a>
### D4 — Sample ephemeral upperdir bytes per-call

| What | Where |
|---|---|
| Stub today | `backend/src/task_center_runner/audit/performance_report.py:745` — `ephemeral["upperdir_bytes"] = _percentile_record([])`. |
| What to land | Two acceptable shapes: (a) Add `upperdir_bytes` field to `OverlayWorkspaceSection` in `backend/src/sandbox/daemon/audit_schema.py` and populate it on the `overlay_workspace.published` event (one sample per tool call). (b) Add a new `overlay_workspace.sampled` event family on the `sample` lane, mirroring the isolated_workspace sampler. Option (a) is simpler and adequate for the report. |
| Verify | Synthetic `overlay_workspace.published` rows with varying `upperdir_bytes` → §6 ephemeral row's `upperdir_bytes` percentile record matches the input distribution. |
| Acceptance | §6 side-by-side table shows ephemeral upperdir percentiles when overlay workloads run. |

---

## P2 — Report-side polish (no schema change)

These are bug-class or quality issues in the report builder/renderer
itself. None require touching the daemon or emitters.

<a id="d5"></a>
### D5 — Background tool `started_at` should be a timestamp, not a seq

| What | Where |
|---|---|
| Stub today | `backend/src/task_center_runner/audit/performance_report.py:533` — `"started_at": event_row.get("seq")` (the daemon-ring sequence number, used as a stable ordering proxy because pulled events don't carry a top-level `ts`). |
| What to land | Either: (a) Thread daemon-emit timestamp through the pull RPC (add `ts: float` at the event level in the wire payload; populate at `audit_buffer.append`); update the normalizer to copy it to the JSONL row; read it here. (b) Accept that seq IS the canonical ordering and update the phase-3 spec's column description from "started_at" → "started_seq". |
| Verify | If (a): synthetic `background_tool.started` with `ts=1716700000.0` → §4 row's `started_at == "2026-05-26T..."`. If (b): doc-only. |
| Acceptance | §4 column header matches the field's true semantics. |

<a id="d6"></a>
### D6 — Move §13 memory-peak threshold into config

| What | Where |
|---|---|
| Magic constant | `backend/src/task_center_runner/audit/performance_report.py:1192` — `if rss_peak and rss_peak > 4 * 1024 * 1024 * 1024:  # 4 GiB warn` |
| What to land | Add `memory_peak_warn_bytes: int = 4 * 1024**3` to a new `AuditWarningsConfig` under `RunnerConfig` (alongside `daemon_audit_pull`). Read it in `_collect_warnings` via `get_central_config()`. |
| Verify | Unit test that flips the config threshold to 1 byte and asserts the warning fires on any non-zero RSS. |
| Acceptance | Operators can tune the threshold per-environment without code changes. |

<a id="d7"></a>
### D7 — Aggregate `upperdir_cap_bytes` so the §13 upperdir warning can fire

| What | Where |
|---|---|
| Dead branch today | `backend/src/task_center_runner/audit/performance_report.py:1199-1208` — reads `isolated_workspace.get("upperdir_cap_bytes")`, but `_section_isolated_workspace` (`backend/src/task_center_runner/audit/performance_report.py:884-928`) never aggregates this field at the section level. |
| What to land | In `_section_isolated_workspace`, iterate `isolated_workspace.sampled` events, take the max of `upperdir_cap_bytes`, and surface it under `isolated_workspace["upperdir_cap_bytes"]`. |
| Verify | Synthetic exit with `upperdir_bytes=900 MiB, upperdir_cap_bytes=1 GiB` → §13 warning `overlay_workspace.upperdir_cap` row present. |
| Acceptance | The warning has a code path that can fire. |

<a id="d8"></a>
### D8 — Document `event_count` vs `events_pulled` divergence

| What | Where |
|---|---|
| Two numbers can disagree | `backend/src/task_center_runner/audit/performance_report.py:281` (`event_count` from JSONL row count) vs `backend/src/task_center_runner/audit/performance_report.py:300-310` (`events_pulled` from `PullerStats`). After a daemon restart or partial-flush, JSONL rows + the puller counter can drift. |
| What to land | Either: (a) Add a `delta_pulled_vs_jsonl` field that surfaces the divergence and a §13 warning when non-zero. (b) Just document it in the §1 summary docstring with the canonical interpretation. |
| Verify | Synthetic puller stats with `events_pulled=10` + JSONL with 12 rows → the warning fires or the doc explains the case. |
| Acceptance | Operators investigating a report can resolve the question without source reading. |

<a id="d9"></a>
### D9 — Wire `evaluate_artifact_bound_gate` into §12

| What | Where |
|---|---|
| Function exposed but never called from prod code | `backend/src/task_center_runner/audit/release_gates.py:135-156` — `evaluate_artifact_bound_gate` is tested in isolation but not surfaced into the §12 verdict block. |
| What to land | (a) Add a `_collect_artifact_inventory(run_dir) -> dict` helper to `performance_report.py` that walks `run_dir` for `sandbox_events.jsonl*` files and counts `live_bytes` + `rotated_bytes` + `rotated_file_count`. (b) Call `evaluate_artifact_bound_gate(...)` in `_section_overhead` and merge its verdict under `overhead.gate.verdict.artifact_bound_pass`. |
| Verify | Synthetic `run_dir` with a 100 MiB live file → verdict reflects the cap check. |
| Acceptance | All 4 V3 release gates are observable in the §12 verdict block. |

<a id="d10"></a>
### D10 — Normalize `_phase_bar` fractions before glyph allocation

| What | Where |
|---|---|
| Truncation bug | `backend/src/task_center_runner/audit/performance_report.py:1739-1751` — `_phase_bar` clamps with `if len(bar) > width: bar = bar[:width]`, which silently drops the rightmost phase(s) when phase fractions sum > 1.0 (numeric noise, or overlapping conceptual phases). |
| What to land | Normalize: `total = sum(fractions[p] for p in _PHASE_ORDER); normalized = {p: f/total for p, f in fractions.items()}` before glyph allocation. |
| Verify | Unit test feeding `{queued: 0.6, exec: 0.6}` → bar shows both glyph types proportionally without truncation. |
| Acceptance | §3 bars never silently lose information. |

<a id="d11"></a>
### D11 — Remove the dead `queued_ms_direct` branch

| What | Where |
|---|---|
| Reads a field no emitter writes | `backend/src/task_center_runner/audit/performance_report.py:411-415` — `bucket["_queued_ms_samples"]` is filled from `rollup.get("queued_ms_direct")`, but no caller (engine / framework / Slice 7 phase buffer) emits this field today. |
| What to land | Either: (a) Delete the `_queued_ms_samples` collection (it's never read after collection anyway). (b) Have `backend/src/engine/tool_call/dispatch.py` populate `queued_ms_direct` from the dispatcher's pre-framework timer for diagnostic granularity. |
| Verify | If (a): the field is gone; no test cares. If (b): synthetic dispatcher call exercises the new field. |
| Acceptance | No dead branches in `_section_per_tool_timing`. |

---

## P3 — Surface / wiring tightening

<a id="d12"></a>
### D12 — Push dual-disable refusal into `AuditRecorder.start()`

| What | Where |
|---|---|
| Single entry point today | `backend/src/task_center_runner/core/engine.py:74-95` (`_refuse_dual_disable_when_isolated_workspace_enabled`) — called only from `run_pipeline`. If a different code path constructs `AuditRecorder` directly (e.g. ad-hoc scripts under `backend/src/task_center_runner/tests/mock/_fixtures/` or future host adapters), the safety check is silently skipped. |
| What to land | Move the check into `AuditRecorder.start()` in `backend/src/task_center_runner/audit/recorder.py:225`. Engine becomes a thin caller. |
| Verify | Update `backend/tests/unit_test/test_task_center_runner/test_performance_report_v3.py::test_engine_refuses_dual_disable_when_isolated_workspace_enabled` to also exercise the path via `AuditRecorder.start()`. Add a test that exercises a custom recorder construction (non-engine code path). |
| Acceptance | Any recorder construction with the dual-disable misconfig refuses to start. |

<a id="d13"></a>
### D13 — Mirror puller-floor + stream-fallback env vars as Pydantic config

| What | Where |
|---|---|
| Today | Only `enabled` exists on `DaemonAuditPullConfig` (`backend/src/config/sections/runner.py:21-32`). The puller-floor knob lives as an env var (`EOS_DAEMON_AUDIT_PULL_FLOOR_MS`, consumed in `backend/src/task_center_runner/audit/daemon_pull.py:119`) and the stream-fallback toggle (`EOS_AUDIT_STREAM_FALLBACK`, consumed in `backend/src/task_center_runner/core/engine.py:31`) has no Pydantic surface at all. |
| What to land | Add `floor_ms: int = 100` and `stream_fallback: bool = True` to `DaemonAuditPullConfig`; have the puller and the engine consult the Pydantic config first, env second (same precedence as `enabled`). |
| Verify | Unit test that flipping `RunnerConfig.daemon_audit_pull.floor_ms = 250` raises the puller's effective floor. |
| Acceptance | Operators can configure every Phase 3 toggle declaratively. |

<a id="d14"></a>
### D14 — Add a `methodology_present` sentinel to §12

| What | Where |
|---|---|
| Ambiguous zeros | `backend/src/task_center_runner/audit/performance_report.py:1052-1059` — when `overhead_metadata is None`, methodology fields render as `n_calls=0, n_paired_runs=0, ...`. A consumer reading `n_paired_runs == 0` can't distinguish "no methodology recorded" from "0 paired runs". |
| What to land | Add `methodology_present: bool` (or use `None` sentinels). Update `evaluate_audit_overhead_gate` to require `methodology_present == True` to pass. |
| Verify | Unit test that absent methodata yields `passed=False` AND surfaces `methodology_present=False`. |
| Acceptance | The §12 gate cannot pass on missing measurements. |

---

## P4 — Doc / debug-mode

<a id="d15"></a>
### D15 — Forensic-raw delta surfacer for §13 (debug-mode only)

| What | Where |
|---|---|
| Today | When `EOS_AUDIT_FORENSIC_RAW_ENABLED=true`, the daemon-event raw is written into the JSONL but the V3 report ignores it (correct per Principle 2 single-source-of-truth). There's no debug-mode that surfaces forensic deltas. |
| What to land | Add an opt-in `_render_forensic_deltas(rows)` block under §13 that compares promoted-section vs daemon_event and reports `(event_seq, key, promoted_value, daemon_event_value)` mismatches. Gated by the same `EOS_AUDIT_FORENSIC_RAW_ENABLED` env. |
| Verify | Test that flips the env, synthesizes a row with intentional drift, asserts the delta surfaces in §13. |
| Acceptance | Operators investigating a "report looks wrong" report can audit the forensic raw without source reading. |

<a id="d16"></a>
### D16 — Plan-doc cosmetic corrections (FU#6)

| What | Where |
|---|---|
| Doc-only drift | (a) `docs/daemon-audit-pull-consolidation-v3/phase-2.5-remaining-emitters-and-wiring.md` §slice-3 file list still names `sandbox/daemon/occ_runtime_services.py` and `sandbox/daemon/changeset_projection.py`; the actual emitters live in `sandbox/occ/service.py`. (b) `docs/daemon-audit-pull-consolidation-v3/phase-2-emitters-and-puller.md` §Deliverable 6 names `task_center_runner/audit/sandbox_events.py`; slice 1 split it into `daemon_event_normalizer.py` + `sandbox_events_sink.py`. |
| What to land | Doc edits only. |
| Verify | Manual read-through. |
| Acceptance | Plan docs reflect shipped file layout. |

---

## Out of scope for this list

Per the task description, the following are EXPLICITLY excluded — they
require live-e2e infrastructure or operator hand-off and are tracked
separately in [`phase-3-implementation-report.md`](phase-3-implementation-report.md)
§Deferred items + the V3 README §Follow-ups:

- Release-gate evidence on the dask-heavy live-e2e fixture (4 gates × 3 paired runs).
- K=5 stream-bridge retirement countdown.
- ADR follow-up issues filed in Linear/GitHub (only doc entries today).
- FU#1 stream-bridge code removal (gated on K=5).
- FU#2 real plugin session lifecycle `plugin.session_*` (gated on a second plugin kind landing).
- FU#3 plugin-kind catalog expansion (opportunistic).
- FU#4 per-subsystem ring sharding (only if §12 overhead gate fails post-ship).

## Suggested sequencing for a single follow-up agent

1. Land **D1** first — it eliminates the most visible "—" cells in §2/§3 and is purely additive.
2. Pair **D2 + D3** in one PR — both add fields to existing sections; touches `audit_schema.py` once.
3. Land **D4** standalone — touches `overlay_workspace` event family.
4. Sweep through **D5-D11** in a single "report polish" PR — all purely in `performance_report.py`.
5. Land **D12 + D13** in a "wiring tightening" PR — touches `recorder.py` + `runner.py` + `engine.py`.
6. **D14, D15, D16** opportunistic.

With D1-D4 landed, every cell in the V3 report carries real data on a
real heavy run — the report is "release-grade" without further code.
With D5-D14 landed, the implementation is polish-grade and the
release-gate suite can run unattended.
