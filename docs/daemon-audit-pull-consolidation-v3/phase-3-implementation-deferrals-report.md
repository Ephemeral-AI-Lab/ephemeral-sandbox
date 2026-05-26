# Phase 3 — Deferrals Implementation Report

**Date:** 2026-05-26
**Scope:** Close the 16 code-side deferrals listed in
[`phase-3-implementation-deferrals.md`](phase-3-implementation-deferrals.md)
(D1-D16). Live-e2e fixture work and operator hand-off remain tracked in
the [Phase 3 implementation report](phase-3-implementation-report.md)
§Deferred items and the V3 README §Follow-ups.

**Outcome:** All 16 deferrals **landed** as in-tree code or doc changes;
each item is pinned by a synthetic test under
`backend/tests/unit_test/test_task_center_runner/test_performance_report_deferrals.py`.
Together with the existing Phase 3 acceptance suite the V3 report now
populates every JSON field and every Markdown column from real data
when the corresponding emitter is active — no remaining `"—"` / `0` /
`_percentile_record([])` placeholders.

---

## Summary

| ID | Status | Tests landed |
|---|---|---|
| [D1](#d1) — surface `mount` / `publish` phases | ✅ | `test_d1_mount_and_publish_phases_populate_when_emitted` |
| [D2](#d2) — emit `occ.prepare_ms` | ✅ | `test_d2_occ_prepare_ms_populates_percentile` |
| [D3](#d3) — cgroup IO + CPU-throttle counters | ✅ | `test_d3_os_resource_io_and_throttle_delta` |
| [D4](#d4) — sample ephemeral `upperdir_bytes` | ✅ | `test_d4_ephemeral_upperdir_bytes_populates` |
| [D5](#d5) — rename `started_at` → `started_seq` | ✅ | `test_d5_background_table_uses_started_seq` |
| [D6](#d6) — memory-peak threshold to config | ✅ | `test_d6_memory_peak_threshold_honours_central_config` |
| [D7](#d7) — aggregate `upperdir_cap_bytes` | ✅ | `test_d7_upperdir_cap_warning_fires` |
| [D8](#d8) — document & warn on `event_count` vs `events_pulled` | ✅ | `test_d8_events_count_drift_warning` |
| [D9](#d9) — wire `evaluate_artifact_bound_gate` into §12 | ✅ | `test_d9_artifact_bound_pass_surfaces_in_verdict` + helper test |
| [D10](#d10) — normalize `_phase_bar` fractions | ✅ | `test_d10_phase_bar_normalizes_overlapping_fractions` |
| [D11](#d11) — remove dead `queued_ms_direct` branch | ✅ | covered by D1 path (no new branch) |
| [D12](#d12) — dual-disable refusal in `AuditRecorder.start()` | ✅ | `test_d12_recorder_start_refuses_dual_disable` |
| [D13](#d13) — mirror `floor_ms` + `stream_fallback` in Pydantic | ✅ | `test_d13_floor_ms_central_config_overrides_default` + `_stream_fallback_central_config` |
| [D14](#d14) — `methodology_present` sentinel + gate guard | ✅ | `test_d14_methodology_present_false_when_missing` + `_overhead_gate_fails_when_methodology_absent` |
| [D15](#d15) — forensic-raw delta surfacer | ✅ | `test_d15_forensic_deltas_surface_when_enabled` |
| [D16](#d16) — plan-doc cosmetic corrections | ✅ | doc-only (no test) |

---

## What landed (per deferral)

<a id="d1"></a>
### D1 — Surface `mount` / `publish` phases via `record_phase`

| File | Change |
|---|---|
| `backend/src/sandbox/daemon/audit_schema.py` | New `safe_record_phase(phase, duration_ms)` helper — lazy-imports `engine.tool_call.phase_buffer.record_phase` so the sandbox package does not carry an unconditional engine dependency. No-ops outside a tool-dispatch buffer (tests, ad-hoc scripts). |
| `backend/src/sandbox/overlay/lifecycle.py` | `acquire()` now calls `safe_record_phase("mount", mount_ms)` after the `overlay_workspace.mounted` emit. Same timing value used in both. |
| `backend/src/sandbox/occ/service.py` | `_emit_occ_commit_events` calls `safe_record_phase("publish", apply_ms)` on the apply→publish boundary (only when `result.published_manifest_version is not None`). Also threads `publish_layer_ms` onto the `occ.publish_layer` event. |
| `backend/src/task_center_runner/audit/performance_report.py` | Comment on `_PHASE_ORDER` updated to reflect that all 6 phases now carry data. The renderer's `_format_phase_cell` already handled the `"—"` → numeric transition without code change. |

**Effect:** §2 per-tool table populates `mount_ms` / `publish_ms`
columns; §3 ASCII bar now renders `Q`/`M`/`E`/`C`/`P`/`R` glyphs in
proportion to the per-phase fraction.

<a id="d2"></a>
### D2 — Emit `occ.prepare_ms` from the OCC service

| File | Change |
|---|---|
| `backend/src/sandbox/daemon/audit_schema.py` | Added `prepare_ms: float \| None = None` to `OccSection`. |
| `backend/src/sandbox/occ/service.py` | `prepare_changeset_sync` now stamps the full prepare elapsed (`monotonic_now() - total_start`) into the emitted `occ.changeset_prepared` event as `prepare_ms`. |
| `backend/src/task_center_runner/audit/performance_report.py` | `_section_occ` reads `prepare_ms` from `occ.changeset_prepared` rows — replaced the `_percentile_record([])` placeholder. |

<a id="d3"></a>
### D3 — Surface cgroup IO + CPU-throttle counters on `os_resource.sampled`

| File | Change |
|---|---|
| `backend/src/sandbox/daemon/audit_schema.py` | Added 5 fields to `OsResourceSection`: `cpu_throttled_us`, `io_read_bytes`, `io_write_bytes`, `io_read_ops`, `io_write_ops`. |
| `backend/src/sandbox/_shared/command_exec_resource_metrics.py` | `_emit_os_resource_sample` pulls the new counters from `resource.cgroup.{cpu_throttled_usec,io_rbytes,io_wbytes,io_rios,io_wios}` (already collected by `_add_cgroup_cpu_stats` / `_add_cgroup_io_stats`) and stamps them onto the emitted event. |
| `backend/src/task_center_runner/audit/performance_report.py` | `_section_os_resource` now derives `throttled_us_delta`, `io.read_bytes`, `io.write_bytes`, `io.read_ops`, `io.write_ops` from the first / last samples using the same `[-1] - [0]` pattern already used for `cpu_user_s_delta`. |

**Effect:** §10 IO and throttle columns show real deltas when the
workload reads / writes / gets CPU-throttled.

<a id="d4"></a>
### D4 — Sample ephemeral upperdir bytes per-call

| File | Change |
|---|---|
| `backend/src/sandbox/daemon/audit_schema.py` | Added `upperdir_bytes: int \| None` and `changed_path_count: int \| None` to `OverlayWorkspaceSection`. |
| `backend/src/sandbox/ephemeral_workspace/pipeline.py` | `EphemeralPipeline.run_tool_call` now stamps both fields on the `overlay_workspace.published` event using the new `_upperdir_total_bytes(handle.upperdir)` helper (bounded walk, `EOS_OVERLAY_UPPERDIR_SAMPLE_ENTRY_LIMIT` default 5000). |
| `backend/src/task_center_runner/audit/performance_report.py` | `_section_overlay_workspace` now feeds the `overlay_workspace.published` samples through `_percentile_record`, replacing the empty placeholder for `ephemeral.upperdir_bytes`. |

<a id="d5"></a>
### D5 — Background tool `started_at` → `started_seq`

| File | Change |
|---|---|
| `backend/src/task_center_runner/audit/performance_report.py` | Renamed the JSON field `started_at` → `started_seq` in `_section_background_tool_calls` and the §4 column header / cell. Docstring inline now states the field is the daemon-ring sequence number (canonical ordering proxy until daemon timestamps flow through the pull RPC). Column alignment updated to right-aligned integer. |

Selected option (b) from the deferral — doc fix; threading a real
timestamp through the pull RPC stays a follow-up for the API+normalizer
slice (not required to close §4's `"—"`).

<a id="d6"></a>
### D6 — Move §13 memory-peak threshold into config

| File | Change |
|---|---|
| `backend/src/config/sections/runner.py` | New `AuditWarningsConfig(ModuleConfigBase)` with `memory_peak_warn_bytes: int = 4 * 1024**3`; added `RunnerConfig.audit_warnings`. |
| `backend/src/task_center_runner/audit/performance_report.py` | `_collect_warnings` now reads the threshold via the new `_memory_peak_warn_bytes()` helper (lazy-import of central config; falls back to 4 GiB when central config is unavailable in test contexts). |

<a id="d7"></a>
### D7 — Aggregate `upperdir_cap_bytes` in `_section_isolated_workspace`

| File | Change |
|---|---|
| `backend/src/task_center_runner/audit/performance_report.py` | `_section_isolated_workspace` now walks every `isolated_workspace.sampled` event and takes the max `upperdir_cap_bytes`, surfacing it as `isolated_workspace["upperdir_cap_bytes"]`. `_collect_warnings` reads it from there instead of the empty `overlay_workspace.isolated.upperdir_bytes` map; the dead `if isolated_workspace.get("upperdir_cap_bytes")` branch can now fire. |

<a id="d8"></a>
### D8 — Document & warn on `event_count` vs `events_pulled` divergence

| File | Change |
|---|---|
| `backend/src/task_center_runner/audit/performance_report.py` | (a) `_section_summary` docstring now explains the difference and links to §11's `daemon_restarts_observed` as the root-cause hint. (b) `_collect_warnings` emits the new `audit.events_count_drift` row when `abs(jsonl_row_count - puller.events_pulled) > 0`. The detail string reports the delta and points the operator at §11. |

<a id="d9"></a>
### D9 — Wire `evaluate_artifact_bound_gate` into §12

| File | Change |
|---|---|
| `backend/src/task_center_runner/audit/performance_report.py` | New `_collect_artifact_inventory(run_dir)` helper walks `sandbox_events.jsonl` (live) + `sandbox_events.jsonl.<N>.gz` (rotated history). `build_performance_report` calls it once and threads the inventory into `_section_overhead`, which invokes `evaluate_artifact_bound_gate` and exposes the verdict under `overhead.gate.verdict.artifact_bound_pass` + the raw verdict under `overhead.gate.artifact_bound` and the inventory under `overhead.artifact_inventory`. |
| `backend/src/task_center_runner/audit/performance_report.py` | §12 Markdown renderer now appends `artifact_bound_pass=<bool>` to the gate-verdict line. |

**Effect:** all 4 V3 release gates surface in the §12 verdict block —
operator no longer has to call `evaluate_artifact_bound_gate` separately.

<a id="d10"></a>
### D10 — Normalize `_phase_bar` fractions before glyph allocation

| File | Change |
|---|---|
| `backend/src/task_center_runner/audit/performance_report.py` | `_phase_bar` now renormalizes when `sum(fractions) > 1.0` before glyph allocation. Prevents the rightmost glyphs from being silently truncated when phase fractions overlap (mount/publish recorded inside the framework's `exec` phase). Width cap retained as a defensive guard. |

<a id="d11"></a>
### D11 — Remove dead `queued_ms_direct` branch

| File | Change |
|---|---|
| `backend/src/task_center_runner/audit/performance_report.py` | Removed `_queued_ms_samples` field from the per-tool bucket and the matching `rollup.get("queued_ms_direct")` collection block. The framework dispatcher emits `queued_ms` through `phase_totals_rollup`, not as a separate field; no emitter ever populated `queued_ms_direct`. Per the deferral text, option (a) — straight delete. |

<a id="d12"></a>
### D12 — Push dual-disable refusal into `AuditRecorder.start()`

| File | Change |
|---|---|
| `backend/src/task_center_runner/audit/recorder.py` | `AuditRecorder.start()` now lazy-imports and invokes `_refuse_dual_disable_when_isolated_workspace_enabled()` before any disk / listener work. The engine entrypoint still calls the same helper at the top of `run_pipeline`, so misconfig is caught BEFORE sandbox provisioning when going through `run_pipeline`, AND when going through any other recorder construction path. |
| `backend/src/task_center_runner/core/engine.py` | Updated the helper's docstring to note the dual call site (engine entrypoint + recorder `start()`); no behaviour change for `run_pipeline` callers. |

<a id="d13"></a>
### D13 — Mirror puller-floor + stream-fallback env vars as Pydantic config

| File | Change |
|---|---|
| `backend/src/config/sections/runner.py` | Added `floor_ms: int = 100` and `stream_fallback: bool = True` to `DaemonAuditPullConfig`. |
| `backend/src/task_center_runner/audit/daemon_pull.py` | `DaemonAuditPuller.__init__` now resolves the floor with explicit precedence: explicit kwarg → `EOS_DAEMON_AUDIT_PULL_FLOOR_MS` env (when set) → `RunnerConfig.daemon_audit_pull.floor_ms` via central config → `DEFAULT_FLOOR_MS` (100 ms). New `_runner_config_floor_ms()` helper handles the central-config read defensively. |
| `backend/src/task_center_runner/core/engine.py` | New `_stream_fallback_enabled()` helper mirrors the recorder's `_daemon_audit_pull_enabled` precedence (env wins; central config is the default). `_refuse_dual_disable_when_isolated_workspace_enabled` now consults it instead of the raw env helper, so flipping `RunnerConfig.daemon_audit_pull.stream_fallback = false` in central config is enough to opt out without an env var. |

<a id="d14"></a>
### D14 — `methodology_present` sentinel + gate guard

| File | Change |
|---|---|
| `backend/src/task_center_runner/audit/performance_report.py` | `_section_overhead` now sets `methodology.methodology_present = bool(overhead_metadata is not None)`. Consumers can distinguish "no measurement supplied" from "0 paired runs" without inferring it from sentinel zeros. |
| `backend/src/task_center_runner/audit/release_gates.py` | `evaluate_audit_overhead_gate` now requires either `n_paired_runs`, `n_calls`, or `bootstrap_resamples` to be truthy before declaring `passed=True`; surfaces the resolved `methodology_present` boolean in the verdict block so operators see exactly why the gate failed. |

<a id="d15"></a>
### D15 — Forensic-raw delta surfacer (debug mode)

| File | Change |
|---|---|
| `backend/src/task_center_runner/audit/daemon_event_normalizer.py` | New `collect_forensic_deltas(rows)` — only callable from within the normalizer module (preserves the `payload["daemon_event"]` writer/reader boundary enforced by `test_daemon_event_writer_module_boundary`). Returns `None` unless `EOS_AUDIT_FORENSIC_RAW_ENABLED=true`; otherwise walks each row's `payload.daemon_event.payload.<section>` and compares each scalar field against the promoted `payload.<section>` value, emitting `(seq, key, promoted_value, daemon_event_value)` rows on drift. |
| `backend/src/task_center_runner/audit/performance_report.py` | `build_performance_report` delegates to the normalizer helper and only tacks `sections.forensic_deltas` onto the JSON when the env gate is on. The §13 renderer appends a `### 13.1 Forensic-raw drift (debug-mode)` block listing drift rows; when no drift is detected, no block is rendered. |

<a id="d16"></a>
### D16 — Plan-doc cosmetic corrections

| File | Change |
|---|---|
| `docs/daemon-audit-pull-consolidation-v3/phase-2-emitters-and-puller.md` | §Deliverable 2 OCC bullet rewritten to point at `sandbox/occ/service.py`; §Deliverable 6 Normalizer rewritten to point at both `daemon_event_normalizer.py` + `sandbox_events_sink.py`; CI lint name updated to reference the real module; §Deferred-to-slices list updated to drop the two non-existent `sandbox/daemon/*` file names. |
| `docs/daemon-audit-pull-consolidation-v3/phase-2.5-remaining-emitters-and-wiring.md` | §Slice 3 file list rewritten to `sandbox/occ/service.py` with a one-line note explaining the V3-initial-plan correction. |

---

## Tests landed

`backend/tests/unit_test/test_task_center_runner/test_performance_report_deferrals.py`
is the single new test file with 17 tests covering D1-D15 (D11 has no
positive test path — the dead branch removal is verified by no
regression in the existing per-tool timing tests; D16 is doc-only).

```
$ .venv/bin/pytest \
    backend/tests/unit_test/test_task_center_runner/test_performance_report_deferrals.py \
    backend/tests/unit_test/test_task_center_runner/test_performance_report_v3.py \
    -q --no-header
33 passed in 0.32s
```

Full task_center_runner + sandbox/daemon + sandbox/overlay + sandbox/occ
+ sandbox/api + sandbox/command_exec scope:

```
431 passed in 3.07s
```

`.venv/bin/ruff check` clean on every touched file.

---

## Architectural notes

### Sandbox → engine dependency for `record_phase`

D1 introduces a soft dependency from `sandbox/overlay/lifecycle.py` and
`sandbox/occ/service.py` on `engine.tool_call.phase_buffer.record_phase`.
The architectural layering convention is `engine → tools → sandbox` (the
sandbox is the lower-level subsystem). To preserve that boundary at
module-load time, the call is routed through `safe_record_phase()` in
`sandbox/daemon/audit_schema.py`, which lazy-imports the engine helper
inside the function body. The function silently no-ops when no
per-call phase buffer is active (i.e. outside a tool-dispatch context),
so test fixtures and ad-hoc scripts that call the lifecycle helpers
directly are unaffected.

This mirrors the existing pattern: `safe_emit` in the same module
lazy-imports `sandbox.daemon.audit_buffer` to break a cycle.

### Forensic-raw boundary still enforced

The `test_daemon_event_writer_module_boundary` lint test grep-scans
the source tree for `payload["daemon_event"]` / `payload.get("daemon_event")`
outside the normalizer module. D15 puts the new forensic walker inside
`daemon_event_normalizer.py` (the existing reader/writer of that key)
so the boundary stays clean — `performance_report.py` only delegates
through `collect_forensic_deltas` and never names the forbidden key
directly.

### Default-on safety

D12 hardens the dual-disable refusal so any recorder construction —
not just the `run_pipeline` happy path — refuses to start under the
"both audit paths off AND isolated_workspace on" misconfig. D13
extends this so the toggles can be flipped through central config alone
(no env required), with env retaining override precedence for per-shell
operator intervention.

D14 prevents the overhead gate from passing when methodology metadata
is entirely absent. Combined with D9 (which adds the 4th gate to the
verdict block) and D8 (which warns on JSONL vs puller drift), the §12
verdict is now the single answer an operator needs for "did Phase 3
audit ship cleanly on this run".

---

## Cleanup performed

- **Removed dead `_queued_ms_samples` collection branch** in
  `_section_per_tool_timing` (D11). The bucket field, the rollup-side
  read, and the post-loop emission all go away — no caller wrote that
  field.
- **Removed the dead `if isolated_workspace.get("upperdir_cap_bytes")`
  guard** indirectly via D7: the section now always emits an integer
  cap (0 when unobserved) so the §13 warning path can fire.
- **Renamed `started_at` → `started_seq`** end-to-end in §4 (D5). No
  external consumer reads the field today (the section only landed in
  Phase 3); the rename is safe.
- **`ruff check` clean** across all touched files.

---

## Items NOT covered by this report (still deferred)

These remain in the
[Phase 3 implementation report §Deferred items](phase-3-implementation-report.md#deferred-items)
and the V3 README §Follow-ups:

- Release-gate evidence on the dask-heavy live-e2e fixture (4 gates × 3 paired runs).
- K=5 stream-bridge retirement countdown.
- Linear/GitHub tracking issues for the ADR follow-ups.
- FU#1 stream-bridge code removal (gated on K=5).
- FU#2 real plugin session lifecycle (`plugin.session_*`) (gated on a second plugin kind).
- FU#3 plugin-kind catalog expansion (opportunistic).
- FU#4 per-subsystem ring sharding (only if §12 overhead gate fails post-ship).

The artifact-bound gate (FU equivalent for §12 verdict) is the only
operational gate that now has an in-process implementation surfaced in
§12 — the other three already had it via the existing evaluator
harness; the per-fixture evidence is operator hand-off.

---

*End of deferrals implementation report.*
