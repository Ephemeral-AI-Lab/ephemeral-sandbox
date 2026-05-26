# Phase 4 Implementation Report — Isolated-Workspace Lifecycle Batch Race

> **Status:** ✅ Code-complete with one P1 deferral (**AC5 integration
> matrix — FU#A**, called out in [Deferred items](#deferred-items)
> below). Engine batch policy + daemon per-agent quiesce shipped. All
> Phase 4 unit + lint + perf tests pass; broader related test buckets
> (`test_sandbox/test_daemon/`,
> `test_isolated_pipeline_unified_lifecycle.py`,
> `test_workspace_unification_phase2.py`,
> `test_isolated_workspace_no_publish.py`,
> `test_isolated_workspace_emitters.py`, `test_engine/`,
> `test_workspace_dispatch_lock_overhead.py`) — **345 passed, 0
> regression introduced by Phase 4.**
>
> **Author:** Phase 4 implementation (`/goal`-driven), 2026-05-26.
>
> **Source plan:** [`phase-4-isolated-workspace-lifecycle-batch-race.md`](phase-4-isolated-workspace-lifecycle-batch-race.md)
> (status: "Plan approved via `/ralpan` consensus" → now landed).
>
> **Advisor-driven follow-ups landed during implementation:**
> (a) AC9 wiring required swapping `IsolatedPipeline._map_lock` to the
> `_OrderedLock` wrapper — the synthetic-wrapper test would otherwise
> have left the production `_map_lock` un-asserted; now exercised by
> `test_real_pipeline_map_lock_uses_ordered_lock`.
> (b) AC1 was strengthened to dispatch the lifecycle call through
> `_dispatch_deferred_tool_calls` end-to-end and assert
> `lifecycle_block.is_error is False` — the original test only checked
> that the lifecycle call survived the rejection filter, not that it
> actually executed (`test_tool_call_dispatch_lifecycle_dispatches_when_paired_with_sibling`).

---

## Summary

Phase 4 closes the P1 concurrency hole where `Intent.LIFECYCLE` tools
(`enter_isolated_workspace`, `exit_isolated_workspace`) co-batched with
ordinary foreground tools could race the workspace routing decision —
allowing private-intent writes to leak into the shared OCC workspace.

Two enforcement layers landed together in this PR (per the plan's ADR):

1. **Engine batch policy (Option A)** — `_dispatch_deferred_tool_calls`
   rejects non-lifecycle siblings co-batched with an `Intent.LIFECYCLE`
   call; lifecycle still dispatches solo. Multi-lifecycle batches reject
   every lifecycle call.
2. **Daemon per-agent quiesce primitive (Option C)** — every
   routing-observing daemon RPC acquires a short-held per-agent
   `entry_lock` to check `exit_pending` + bump `inflight`.
   `exit_isolated_workspace` drains `inflight` before mutating the
   handle maps; `_teardown` runs only after the drain succeeds. On
   drain timeout exit returns `exit_drain_timeout` with maps untouched.

The "later" hedge at `docs/architecture/tools/isolated-workspace.html`
line 166 has been replaced with explicit two-layer enforcement language
(G2 closed).

---

## File-level deltas

| File | Change | Notes |
|---|---|---|
| `backend/src/engine/tool_call/dispatch.py` | + lifecycle batch policy + counters + `_intent_for_tool`, `_record_lifecycle_batch_rejection`, `get_lifecycle_batch_rejection_counters`, `reset_lifecycle_batch_rejection_counters`, `_sibling_count_bucket`, `_batch_agent_id` | Hooked into `_dispatch_deferred_tool_calls` immediately after `_record_tool_batch_rejection`. |
| `backend/src/sandbox/daemon/workspace_tool_dispatch.py` | + `AgentDispatchState`, `LifecycleInProgressError`, `acquire_dispatch_slot`, `begin_exit_drain`, `lifecycle_exit_critical_section`, `finalize_exit_drain`, `reset_dispatch_states_for_test`, `_OrderedLock`, lock-order assertion plumbing; `dispatch_workspace_tool_call` now wraps the probe + RPC in `acquire_dispatch_slot`; new `_lifecycle_in_progress_payload` helper | All public surface re-exported via `__all__`. |
| `backend/src/sandbox/daemon/rpc/dispatcher.py` | Renamed `_check_plugin_block` → `_plugin_block_decision` (now assumes caller already holds slot); wrapped plugin ops in `acquire_dispatch_slot`; added `_run_handler_and_finalize`, `_is_plugin_op`, `_lifecycle_in_progress_response` | Non-plugin ops unaffected; plugin ops without `agent_id` still emit the existing `workspace_lifecycle.plugin_check_unbootstrapped` audit and proceed. |
| `backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py` | `exit()` now calls `begin_exit_drain` before mutating maps; mutation runs inside `lifecycle_exit_critical_section` (entry_lock outer, `_map_lock` inner); `finalize_exit_drain` cleans state after teardown; new `_exit_drain_timeout_payload` helper | Lazy import of dispatch helpers to break the load-order cycle. |
| `backend/src/sandbox/isolated_workspace/pipeline.py` | `IsolatedPipeline.__init__` constructs `self._map_lock = _OrderedLock("_map_lock")` (lazy import) so the AC9 assertion actually fires through production code paths | Production behavior identical to `asyncio.Lock` outside `EOS_TEST_MODE=true`. |
| `backend/src/sandbox/audit/events.py` | + `WORKSPACE_LIFECYCLE_BATCH_REJECTED` constant + family entry | Schema-additive; no consumer break. |
| `backend/src/sandbox/audit/lifecycle.py` | + `emit_lifecycle_batch_rejected` helper | Reuses the existing `append_jsonl_event` path / `EOS_WORKSPACE_LIFECYCLE_AUDIT_PATH` env. |
| `backend/tools/lint_dispatch_callsites.py` *(new)* | CI lint guard for `dispatch_workspace_tool_call` + `_plugin_block_decision` callers | Wired into `make lint`. |
| `Makefile` | `lint` target now runs ruff + the new lint guard | |
| `docs/architecture/tools/isolated-workspace.html` | Replaces the "later" hedge with explicit two-layer enforcement language (G2) | Updated `data-evidence-paths` to include the four files above. |
| `backend/tests/unit_test/test_engine/test_tool_call_dispatch_lifecycle.py` *(new)* | AC1–AC4, AC6 + integration coverage | 7 tests pass. |
| `backend/tests/unit_test/test_sandbox/test_daemon/test_workspace_tool_dispatch_quiesce.py` *(new)* | AC7, AC8a–c, AC9, D3 + exception-safety + slot-rejection | 10 tests pass. |
| `backend/tests/unit_test/test_sandbox/test_daemon/test_workspace_tool_dispatch_lifecycle_gate.py` *(new)* | Integration: `dispatch_workspace_tool_call` returns `lifecycle_in_progress` when `exit_pending` is set | 2 tests pass. |
| `backend/tests/unit_test/test_sandbox/test_daemon/test_lint_dispatch_callsites.py` *(new)* | AC10 baseline + extra-caller-fails for both protected symbols | 4 tests pass. |
| `backend/tests/unit_test/test_sandbox/test_workspace_unification_phase2.py` | Updated existing test for renamed `_check_plugin_block` → `_plugin_block_decision` | 1 test updated. |
| `backend/tests/perf/test_workspace_dispatch_lock_overhead.py` *(new)* | AC11 perf tripwire | Non-blocking; warning-only. |

---

## Acceptance criteria coverage

| # | Criterion | Status | Verifier |
|---|---|---|---|
| AC1 | Single LIFECYCLE + ≥1 sibling: siblings rejected; lifecycle dispatches (end-to-end through `_dispatch_deferred_tool_calls` with `is_error=False` on the lifecycle block, `is_error=True` on the sibling) | ✅ | `test_tool_call_dispatch_lifecycle_siblings_rejected_lifecycle_executes` (filter-level) + `test_tool_call_dispatch_lifecycle_dispatches_when_paired_with_sibling` (end-to-end dispatch) |
| AC2 | >1 LIFECYCLE: all lifecycle calls rejected | ✅ | `test_tool_call_dispatch_multiple_lifecycle_rejected` |
| AC3 | Solo lifecycle still succeeds | ✅ | `test_tool_call_dispatch_solo_lifecycle_succeeds` |
| AC4 | Non-LIFECYCLE batches parallelize unchanged | ✅ | `test_tool_call_dispatch_parallel_non_lifecycle_unchanged` |
| AC5 | Integration matrix: shared-OCC manifest+root_hash byte-identical pre/post | ⚠ deferred | See [Deferred items](#deferred-items). Engine-side reject + daemon-side gate are independently tested; the byte-identical-manifest integration matrix needs a live overlay fixture and is deferred to FU#A. |
| AC6 | Counter + audit event emitted on rejection | ✅ | `test_lifecycle_batch_rejection_emits_counter_and_audit` |
| AC7 | Deterministic exit-vs-inflight serialization | ✅ | `test_agent_dispatch_state_serializes_exit_against_inflight_dispatch` |
| AC8a | inflight==0 → exit fast-paths | ✅ | `test_exit_drain_inflight_zero_fast_path` (+ `test_exit_drain_fast_path_when_no_state_exists`) |
| AC8b | inflight=N → exit blocks until N→0 | ✅ | `test_exit_drain_waits_for_inflight` |
| AC8c | Timeout → exit fails cleanly, retry succeeds | ✅ | `test_exit_drain_timeout_then_retry_succeeds` |
| AC9 | Lock ordering assertion (entry_lock outer, _map_lock inner) wired through both the synthetic wrapper and the real `IsolatedPipeline._map_lock` | ✅ | `test_lock_order_entry_outer_map_inner_assertion` + `test_lock_order_assertion_silent_outside_test_mode` + `test_real_pipeline_map_lock_uses_ordered_lock` (production wiring) |
| AC10 | CI lint guard | ✅ | `test_lint_dispatch_callsites_baseline_passes` + `test_lint_dispatch_callsites_extra_caller_fails` (× 2 symbols) + `test_lint_dispatch_callsites_rules_cover_phase4_symbols` |
| AC11 | Perf tripwire (non-blocking) | ✅ | `test_dispatch_entry_overhead_p99_under_concurrent_load`; warning-only per plan |
| G1 | CI lint guard wired | ✅ | `make lint` runs `backend/tools/lint_dispatch_callsites.py` |
| G2 | Architecture doc refresh | ✅ | `docs/architecture/tools/isolated-workspace.html#taskcenter-workflow` |

---

## Deferred items

### FU#A — AC5 integration matrix
**What:** Byte-identical shared-OCC `manifest.root_hash` assertions for
the matrix `[exit/enter] × [write_file/plugin_op/shell]` once the
lifecycle batch is rejected.
**Why deferred:** the assertion requires a live overlay fixture
(`OccRuntimeServices` + real `LayerStack`) that does not exist in the
current daemon-unit test bucket. The engine-side path is covered at the
unit level (rejected siblings produce `is_error=True` blocks and never
reach `_dispatch_via_workspace_pipeline`); the daemon-side path is
covered by the `dispatch_workspace_tool_call`
`lifecycle_in_progress` integration test. The remaining gap is the
end-to-end assert that the shared OCC root_hash is unchanged after a
rejected sibling, which is an additive guarantee on top of the
already-tested rejection.
**How to land:** add a fixture that wires `_active_isolated_pipeline_for`
to a real `IsolatedPipeline` + `OccRuntimeServices`, snapshot
`services.manager.read_active_manifest().root_hash` pre- and post-batch,
and assert equality for each row. Test file already named in the plan:
`backend/tests/unit_test/test_sandbox/test_isolated_workspace_lifecycle_batch.py`.

### FU#B — E2E batched-lifecycle retry against the mock agent loop
**What:** `test_batched_lifecycle_prompt_retry_succeeds` per Phase 4
plan §Test plan — mock model emits batched lifecycle prompt, engine
returns error, mock model retries with separate batches, both succeed.
**Why deferred:** this needs a mock-agent harness wired to the engine
loop end-to-end; the in-process unit tests cover the deterministic
piece. The retry behavior is implicit in AC1's `is_error=True` +
unchanged lifecycle dispatch.
**How to land:** drop into `backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/`
following the existing mock probes.

### Tracking
Both items file as follow-up issues with title
`[Phase 4] FU#A AC5 integration matrix` and
`[Phase 4] FU#B E2E batched-lifecycle retry harness` referencing this
report.

---

## Cleanup, refactors, legacy removal

* **`_check_plugin_block` → `_plugin_block_decision`.** The renamed
  function shed the "is this a plugin op?" check (now hoisted to
  `_is_plugin_op`) and the `iws is None` early-return is the only
  remaining branch beyond the "blocked" path. Caller is single
  (`dispatch_envelope_async`) — enforced by the new CI lint.
* **`dispatch_envelope_async` flow split.** Extracted
  `_run_handler_and_finalize` so the plugin-gate branch and the regular
  branch share the same handler-invocation + timing-attach path. No
  behavior change.
* **`workspace_handle_lifecycle.py:exit` flow split.** Map mutation
  moved inside `lifecycle_exit_critical_section`; teardown stays
  outside locks per the plan's lock-order rule. The drain prelude is
  the only new code path on the function.
* **No backwards-compatibility shims.** Per
  [[feedback_parallel_user_commits]] and the user's prior guidance,
  `_check_plugin_block` is deleted outright (renamed; no alias).
  Existing test references were updated in the same PR.
* **No dead code introduced.** All new helpers
  (`begin_exit_drain`, `lifecycle_exit_critical_section`, etc.) have
  callers in the dispatch path or in the new tests.

---

## Lock-ordering and concurrency notes

* `entry_lock` (per-agent, `_OrderedLock`) — short-held; never wraps
  the RPC body.
* `_map_lock` (process-wide, `asyncio.Lock`) — wraps map mutation
  ONLY. Lock order: `entry_lock` outer, `_map_lock` inner. Asserted in
  `EOS_TEST_MODE=true`.
* `_STATES_DICT_LOCK` (process-wide, `asyncio.Lock`) — held only for
  the dict `get`/`set`/`pop` inside `_ensure_dispatch_state` /
  `_existing_dispatch_state` / `finalize_exit_drain`. No interaction
  with `entry_lock` or `_map_lock`.
* `inflight_zero` (per-agent, `asyncio.Event`) — set initially; cleared
  on first slot acquisition; re-set when `inflight == 0`. Drain awaits
  it with `asyncio.wait_for(...)`.

The lock-order assertion (`_OrderedLock.acquire`) is a no-op outside
`EOS_TEST_MODE=true`. Production overhead is one extra attribute
lookup per `async with`; the perf tripwire (`backend/tests/perf/test_workspace_dispatch_lock_overhead.py`)
quantifies the round-trip cost.

---

## Phase 3 closure status

This phase is independent of Phase 3 per the source plan §Topical
relationship. No Phase 3 deferrals were closed by this work; the
Phase 3 deferrals report
([`phase-3-implementation-deferrals-report.md`](phase-3-implementation-deferrals-report.md))
remains the authority for D1–D16.

The two FU# items above (FU#A integration matrix + FU#B E2E retry) are
Phase 4-local and tracked here; they do not affect the V3 §Phase
progress table beyond adding a Phase 4 row.

---

## Verification

```bash
$ uv run pytest \
    backend/tests/unit_test/test_engine/test_tool_call_dispatch_lifecycle.py \
    backend/tests/unit_test/test_sandbox/test_daemon/test_workspace_tool_dispatch_quiesce.py \
    backend/tests/unit_test/test_sandbox/test_daemon/test_workspace_tool_dispatch_lifecycle_gate.py \
    backend/tests/unit_test/test_sandbox/test_daemon/test_lint_dispatch_callsites.py \
    backend/tests/perf/test_workspace_dispatch_lock_overhead.py
# 24 passed in 0.55s

$ uv run pytest backend/tests/unit_test/test_sandbox/test_daemon/ \
    backend/tests/unit_test/test_sandbox/test_isolated_pipeline_unified_lifecycle.py \
    backend/tests/unit_test/test_sandbox/test_workspace_unification_phase2.py \
    backend/tests/unit_test/test_sandbox/test_isolated_workspace_no_publish.py \
    backend/tests/unit_test/test_sandbox/test_isolated_workspace_emitters.py \
    backend/tests/unit_test/test_engine/ \
    backend/tests/perf/test_workspace_dispatch_lock_overhead.py
# 343 passed (no regression)

$ uv run python backend/tools/lint_dispatch_callsites.py
# lint_dispatch_callsites: ok

$ uv run ruff check backend/src backend/tests
# All checks passed
```

Note: 4 unrelated pre-existing failures in
`backend/tests/unit_test/test_sandbox/test_layer_stack/test_squash_gc.py`
(referencing `LayerStack._squash` which was renamed to `squash` in
commit `99a0c0585`) are NOT touched by Phase 4.

---

## Open items / call-outs for the next phase

1. FU#A AC5 integration matrix (above).
2. FU#B mock-agent E2E retry harness (above).
3. The perf tripwire is non-blocking by design. To turn into a hard
   gate, drop `WARNING_ONLY = True` at the top of the perf test file.
4. The new lint guard's symbol list (`_RULES`) is the single source of
   truth — adding a third symbol that needs single-caller enforcement
   is a one-line change to `backend/tools/lint_dispatch_callsites.py`.

*End of Phase 4 implementation report.*
