# Handoff: ScenarioEventSource migration — fan-out phase (Phase 2+)

**Status as of 2026-05-29.** Supersedes the Phase 2/3/4 sections of
`docs/plans/mock_event_source_HANDOFF.md` (those predate the budget/fan-out
realization below). Phase 0 (seam) + Phase 1 (core + 3 simple probes) remain as
described there and are GREEN. Authoritative detail map:
`docs/plans/mock_event_source_MIGRATION_MAP.md` (307 KB — per-probe specs,
executor-action catalogue, per-test assertion→graph_summary rewrites, full
tests/mock inventory, Phase-3 deletion checklist).

Goal (unchanged): drive every mock agent through the **real** `engine/query/loop.py`
via an injected per-agent event source, so a mock and a real agent differ **only**
in the event source — then delete the imperative `MockSquadRunner` and its
14 mock-only lifecycle `EventType`s.

## The decision that shapes this phase (read first)

A single agent is capped at `tool_call_limit` (executor=75; loop hard-fails at
1.5×=113 — `loop.py:_terminal_not_submitted`). Heavy executor probes issue
**250–2000 tool calls** and the loop's background path is fire-and-forget. Of the
52 scenario-running test files, **43 hit heavy/background probes**.

**User directive (do NOT reopen): solve the budget the way the architecture
intends — `planner → generator fan-out`, everything through the loop. NOT an
off-loop carve-out.** Each generator stays within budget and runs through the
real loop; heavy work is decomposed into a generator DAG (LayerStack/OCC handles
the parallel writes — the project's core thesis). The "differs only in the event
source" invariant is preserved. See memory `mock_event_source_heavy_probe_fanout_decision`.

The background probes (12) are the one exception: their blocking-`await`+per-call-
`wait_for`-cancel `call_tool` pattern is not how a real agent works, so they get a
separate rewrite to the real-agent background model. **User sequenced this LAST
("prove fan-out first").**

| Piece | Status |
|---|---|
| Seam (Phase 0) + 3 simple probes (Phase 1) | ✅ green |
| Fan-out infrastructure (queue-bridge, STEP-0, model-reg, metrics fix, helper) | ✅ DONE this session |
| `high_concurrency` proven through the loop under the flag (20-worker fan-out e2e) | ✅ green (19s) |
| heavy_io_zoned + plugin ×6 + ephemeral ×4 (non-cancellation minus same_path_conflict) | ✅ green under flag (2026-05-29, see §"Session 2026-05-29 cont.") |
| full_stack / capacity / full_case script-actions (PreparedToolScriptEngine bridge) | ⏳ ANALYZED + spec'd, not implemented (bridgeable; needs ctx-threading + terminal routing + test migration) |
| auto_squash + ephemeral same_path_conflict | ⏳ RECLASSIFIED fan-out-class (NOT mechanical — see §"Session"); recipes documented |
| `complex_project_build` ×6 decomposition (the hard one) | ❌ NOT STARTED |
| Migrate ~11 remaining event-asserting test files | ❌ NOT STARTED |
| 12 background-probe rewrites (real-agent background model) | ❌ NOT STARTED |
| Phase 3 deletions + flag flip default-on | ❌ NOT STARTED |
| Final: all 144 `tests/mock` green | ❌ NOT STARTED |

---

## How to run / verify

- **Use the uv venv**, never global pytest: `cd backend && ../.venv/bin/python -m pytest …`.
- The new runner is behind a flag, default OFF. Select it per-run with the env var:
  ```bash
  cd backend
  EOS_MOCK_EVENT_SOURCE_RUNNER=1 ../.venv/bin/python -m pytest <scenario_test> -p no:cacheprovider
  ```
- Heavy/IWS suites run on the local docker sweevo sandbox here (no skip; gates
  true + docker provider). `live_e2e_heavy_enabled()` and `database_configured()`
  are both True on this host.
- **Per-scenario verification, not a big-bang flip.** Port a scenario's probe(s)
  + migrate its test's event assertions, verify it green under the env var, repeat.
  Flip the flag default-on (`scenarios/builder.py`) only after all scenarios pass
  under it.
- Regression smoke (must stay green): the 3 proof tests
  (`contracts/test_scenario_event_source_spike.py`,
  `test_scenario_loop_runner_planner_submit.py`,
  `test_correctness_via_event_source.py`, 5 tests/49s) + 198 default-off unit
  tests (`tests/unit_test/test_engine` + `test_tools/test_tool_execution.py` +
  `test_notification/test_terminal_call_reminder.py`).

---

## Infrastructure built this session (the reusable pattern)

All under `backend/src/task_center_runner/`:

- **`agent/mock/probe_bridge.py`** — the queue-bridge. `bridge_turns(factory,
  artifact_out, normalize)` runs an imperative `call_tool`-based probe as a
  concurrent task and `yield`s one `Turn(ToolCall)` per dequeued call at the top
  level of the role TurnScript, resolving the probe's awaited future with the
  loop-normalized `ToolResult`. `bridge_probe_for(action, probe_ctx)` maps an
  executor action → `(probe_factory, summary)`. **Rejects `background_task_id`**
  (raises NotImplementedError) — background probes are a separate rewrite.
  See memory `mock_event_source_queue_bridge`.
- **`agent/mock/scenario_adapter.py:_executor_script`** — tries `PROBE_BUILDERS`
  (generator probes) first, then `bridge_probe_for` (imperative probes), else
  `NotImplementedError`. **This is where you add new probe families.**
- **`agent/mock/probes.py:ProbeContext`** — gained `publish` / `publish_mock_record`
  / `record_check` / `metadata` so it serves as the out-of-band callback bundle
  the heavy probes expect (re-homed SANDBOX_*/MOCK_SANDBOX_CHECK_* events).
- **`agent/mock/scenario_loop_runner.py`** — STEP 0a: `_inspect_prompt` +
  `_record_initial_messages` ported in (publishes `MOCK_PROMPT_INSPECTED`).
- **`hooks/registry.py:MutableMockState`** — STEP 0b: `set_next_advisor_verdict` +
  `consume_advisor_verdict` (for the negative-path test rewrite).
- **`core/runner.py:_active_mock_model_if_enabled`** — registers a throwaway active
  model row when the flag is on (the new path goes through `spawn_agent` which
  needs one). Idempotent (skips if a proof-test fixture already activated one).
  All scenario tests funnel through `run_scenario`, so none need a per-test fixture.
- **`audit/metrics.py:_pop_start`** — fixes Start↔Complete pairing: the real loop's
  `ToolExecutionStartedEvent` omits `tool_use_id` (the old `_call_tool` hand-emitted
  it), so the aggregator falls back to `(tool_name, agent_run_id)` when the exact
  key misses. Engine untouched (stays at the seam). Affects ALL heavy scenarios'
  perf reports under the new runner.
- **`tests/mock/_focused_scenario_contracts.py:count_role_tasks(report, role,
  status=...)`** — the §4.1 replacement for `count_events(<ROLE>_INVOKED/SUCCESS)`.

---

## Remaining work (ordered; follow the proven pattern)

### A. Port the other already-fan-out-shaped foreground scenarios
The proven recipe (mirror `high_concurrency`):
1. Add `bridge_probe_for` entries in `probe_bridge.py` for the scenario's executor
   actions, wiring `probe_ctx.metadata` / `publish` / `publish_mock_record` /
   `record_check` into the probe's kwargs (import the probe module lazily).
2. Migrate the scenario's test event-assertions to `graph_summary`
   (`count_role_tasks`, etc.) per `MIGRATION_MAP.md` "Assertion → graph_summary
   rewrites".
3. Verify green under `EOS_MOCK_EVENT_SOURCE_RUNNER=1`.

Scenario → probe module → test (verify per-generator budget ≤75 first):
- **heavy_io_zoned** (`heavy_io_zoned_seed/worker:N/reconcile`) → `heavy_io_zoned_probe.py`
  → `tests/.../layer_stack_occ_overlay/test_heavy_io_zoned_concurrent.py`.
  Structurally identical to high_concurrency — **do this first.**
- **plugin** (`plugin_*` ×6) → `plugin_workspace_probe.py` → `tests/.../plugin/*`.
- **ephemeral non-cancellation** (`ephemeral_workspace_all_verbs/concurrent_writes/
  policy/o1_disk/same_path_conflict`) → `ephemeral_workspace_probe.py` →
  `tests/.../ephemeral_workspace/*`. (`ephemeral_workspace_cancellation` → §C.)
- **auto_squash** (`auto_squash_commit_resume_probe`) → currently in `runner.py`
  1083-1257; map verdict is REWRITE-AS-GENERATOR (it's small) → add to `probes.py`
  `PROBE_BUILDERS`. Test `test_auto_squash_commit_resume.py`.
- **full_stack / capacity / script-actions** (`inspect_user_input`, `execute_package:N`,
  `final_reconciliation`, `occ_conflict_matrix`, `overlay_edge_matrix`,
  `layerstack_squash_lease`, `lsp_refresh_semantics`, `recursive_oversized_matrix`,
  `full_stack_final_reconciliation`, `capacity_metrics_full_system`,
  `recursive_step`) use `PreparedToolScriptEngine` (`tool_scripts.py`,
  `full_stack_tool_scripts.py`, `capacity_actions/`). The engine takes `call_tool`
  → bridge it: a `bridge_probe_for` factory that builds
  `PreparedToolScriptEngine(bridge_call_tool).run(<script>(ctx))`. Tests:
  `test_full_case_user_input.py`, `test_full_system_capacity_matrix.py`,
  `test_full_stack_adversarial.py`, `test_capacity_scenario_packs.py`.
  NOTE: `fail`/`fail:` → `submit_execution_blocker`; `request_recursive_goal:`/
  `request_recursive_matrix:` → `submit_execution_handoff` — these need their own
  advisor-gated terminal turn in `_executor_script` (not the success terminal).
  Also wire `MutableMockState.consume_failure` into the adapter for failure-injection
  scenarios.

### B. Decompose `complex_project_build` (×6) — the hard, non-mechanical piece
`complex_project_build_probe.py` (+ `_shell_edit_lsp` + `_grep_glob`) is a single
executor running a ~2000-call **sequential** pipeline (phase0 bootstrap → edit
amplification → auto-squash saturation → read amplification → lsp saturation),
with data dependencies (`stats` read from prior tool results, `_tool_call_floor`
loops). It cannot run as one budget-bounded generator. Reshape the scenario
planner to fan out generator tasks (seed/bootstrap → N parallel work generators
each doing a budget-sized slice → reconcile), preserving what the probe asserts
(auto-squash depth, OCC conflicts, layer depth, LSP). **Plan this with a fresh
advisor pass before implementing.** Tests: `tests/.../project_build/*` (smoke +
full variants), `test_project_build_shell_edit_lsp_three_parallel_agents.py`.
Heavy `complex_project_build` modules note: probe mutates `ctx.metadata.cwd` in
phase0 — the bridge must let the probe use the loop's live `tool_metadata`
(it does: `probe_ctx.metadata`), and `normalize_result` preserves `.metadata.timings`
(the floor loops depend on it).

### C. Background-probe rewrite (task #5, sequenced last among Phase 2)
12 `background_shell_*` + `ephemeral_workspace_cancellation`. Rewrite to the
real-agent background model: `shell(background=True)` (synthetic started block) +
`wait_background_tasks` / `cancel_background_task` observed across loop turns.
This changes probe structure and some assertions; `MIGRATION_MAP.md` →
`background_shell` / `ephemeral_workspace` specs enumerate every `call_tool` site
(56 / 34) and the BG/cancel hazards. Tests: `tests/.../background_tool/*`,
`test_ephemeral_cancellation_drops_partial_upperdir.py`.

### D. Phase 3 deletions + flip (after all scenarios green under the flag)
Follow `MIGRATION_MAP.md` "Phase-3 deletion checklist" verbatim (STEP 0→6,
**EventType-enum removal STRICTLY LAST**). Highlights: delete the imperative
`MockSquadRunner` internals (extract-then-delete `_call_tool`/`_record_tool_check`/
`_caller`/`_stream_run_id` only if still needed — confirm nothing references them);
delete `_advisor_approval.py` (relocate `build_advisor_approval_messages` into the
test fixtures module first); strip `expected_event_sequence` from `scenarios/base.py`
+ every scenario; remove `RunReport.seen_event_types` + `_assert_ordered_subsequence`/
`_assert_event_counts`; drop `hooks/builtins.py` VERIFIER emit sites (lines 135/162/199);
re-point the external `test_sweevo_audit_recorder.py:393` enum consumer; remove the
16 lifecycle members from `audit/events.py:61-76`. Then flip
`scenarios/builder.py` flag default-on and run all 144.

### E. Final sweep
Run all `tests/mock` under the flag (default-on), classify + fix. The 80
`isolated_workspace` tests are runner-agnostic (don't run scenarios) and should be
unaffected — confirm, don't migrate.

---

## Critical gotchas
- **Per-generator budget ≤75.** Before bridging a scenario, confirm its
  per-generator probe stays under the executor's 75-call limit; if not, the
  scenario planner must fan out more generators.
- **Bridge rejects `background_task_id`** — if a probe passes it, that probe is a
  §C background rewrite, not a §A bridge target.
- **Advisor gate per terminal.** Every gated terminal needs the preceding
  `ask_advisor` turn (already emitted by `_executor_script`); the advisor/explorer
  profiles are registered in `agent/mock/definitions.py`.
- **Parallel agents edit this repo.** Stage with explicit paths; do NOT touch
  `docs/plans/ultra_complex_bundled_scenario_TEST_PLAN.md` or
  `docs/plans/planner_prior_iteration_context_IMPL_PLAN.md` (other agents).

## Pointers
- Detail map: `docs/plans/mock_event_source_MIGRATION_MAP.md`
- Prior handoff (Phases 0/1 still valid): `docs/plans/mock_event_source_HANDOFF.md`
- Auto-memory: `mock_event_source_heavy_probe_fanout_decision`,
  `mock_event_source_queue_bridge`, `mock_event_source_seam_integration_map`,
  `mock_event_source_phase1_adapter_design`, `mock_event_source_must_emit_tool_use_deltas`.
- Proven reference implementation: the `high_concurrency` port — `probe_bridge.py`
  (`bridge_probe_for` high_concurrency entries) +
  `tests/.../test_high_concurrency_layerstack_overlay_occ.py` (migrated assertions).

---

## Session 2026-05-29 cont. — §A mechanical core landed + key reclassifications

> **Forward plan for all remaining/deferred work:** `docs/plans/mock_event_source_DEFERRED_IMPL_PLAN.md`
> (ordered: [1] N concurrent same-instance docker sandboxes for parallel verify [FIRST] → [2] full_stack/
> capacity script bridge + terminal routing → [3] multiagent fan-out promotions + nested goals → [4] §C
> background rewrites → [5] test migrations + Phase D flip + Phase E). Executor `tool_call_limit` is now 100.

### ✅ Verified green under `EOS_MOCK_EVENT_SOURCE_RUNNER=1` (docker, this session)
11 scenarios, all via the queue-bridge mirror of `high_concurrency`:
- **heavy_io_zoned** — 3 bridge branches (`heavy_io_zoned_{seed,worker:N,reconcile}`) in
  `probe_bridge.py`. Test was already graph-shaped. 126s.
- **plugin ×6** — `plugin_{read_only_lsp_refresh,write_allowed_publish,intent_contract,
  iws_policy,setup_failure,service_evict}` bridge branches. Single-action scenarios,
  sequential, ≤29 calls each.
- **ephemeral ×4** — `ephemeral_workspace_{all_verbs,concurrent_writes,policy,o1_disk}`
  bridge branches. (o1_disk = ~104 loop calls, fits under the 113 hard ceiling;
  `_layer_metrics`/`_runtime_sample` are out-of-band and don't count.)

Bridge entries are pure additions to `bridge_probe_for`; `ProbeContext.publish_mock_record`
covers the bespoke per-zone/worker `SandboxCheck`. All preserve "differ only in event source."

### ⚠️ PRODUCTION CHANGE — executor toolset extended (user-approved)
The real loop dispatches by name through the agent's registered toolset; the old
`MockSquadRunner._call_tool` dispatched tool **objects** directly, bypassing the allowlist.
`plugin_write_allowed_publish` (needs `lsp.apply_workspace_edit`) and `plugin_iws_policy`
(needs `enter_isolated_workspace`/`exit_isolated_workspace`) failed with `Unknown tool`
because **no agent profile granted them**. Since the mock executor loads the *production*
profile (invariant), the fix was to extend `backend/src/agents/profile/main/executor.md`
`allowed_tools` with those 3 tools (`has_tool()` resolves all 3). Regression-clean: 90
agent/spawn/iws-gate unit tests + high_concurrency + the 3 proof tests all green afterward.
The IWS "pipeline not initialized" log on iws_policy is a harmless fail-open, not the blocker.

### ⏳ RECLASSIFIED as fan-out-class (the map's "mechanical" verdict was WRONG)
- **auto_squash_commit_resume** — `AUTO_SQUASH_MAX_DEPTH=100` ⇒ probe does `write_count=104`
  + ~10 calls ≈ **114 > 113 hard ceiling** (`ceil(1.5×75)`). Test asserts `write_count==104`
  and `depth_before>100` (needs 100+ sequential OCC commits — cannot batch). A single
  generator hard-fails. **Recipe:** split the scenario (`scenarios/sandbox/auto_squash_commit_resume.py`,
  currently 1 task) into 2 executor tasks — gen1 = 104 depth-seed writes (≈106 calls < 113),
  gen2 (deps gen1) = edit-seed+2 edits+4 reads+shell+conflict+summary — and rewrite the
  runner probe (`runner.py:1083`) as two generators in `probes.py`. Group with §B + advisor pass.
- **ephemeral same_path_conflict** — probe `asyncio.gather`s 4 same-path writes and asserts
  ≥1 OCC conflict (`if not failed_indexes: raise "no typed conflicts"`). The queue-bridge
  **serializes** every call into one loop turn → no race → no conflict → guaranteed failure.
  Needs a real concurrency-preserving fan-out (N racing generators like high_concurrency's
  `CONFLICT_WORKER_COUNT`). NOT bridgeable as-is. (NB: ephemeral `concurrent_writes` IS safe —
  its `asyncio.gather` writes are disjoint and it asserts per-write source tags, not races.)

### ⏳ full_stack / capacity / full_case script-actions — analyzed, bridgeable, NOT yet implemented
`PreparedToolScriptEngine` (`tool_scripts.py`) runs `script.steps` **sequentially**, so
bridging is semantically faithful (unlike ephemeral's gather). The scenarios **already fan out**
(e.g. `full_stack_adversarial` emits separate `occ_matrix`/`overlay_matrix`/`lsp_matrix`/
`layerstack_matrix` executor tasks), and each matrix script is bounded by cell count
(~8-12 cells × 2-3 tools ≈ 20-40 < 75). The `lsp_matrix` `workspace_edit_publish` cell uses
`lsp.apply_workspace_edit` — already unblocked by the executor.md change above.
**Implementation needed (3 parts, all required to verify a scenario end-to-end):**
1. **ctx-threading** — `bridge_probe_for` only gets `probe_ctx`; script factories need
   `ScenarioContext` (`ctx`). Thread `ctx` into the bridge (or handle script actions inside
   `_executor_script`, which already has `ctx`). Factory builds
   `PreparedToolScriptEngine(bridge_call_tool).run(<script>(ctx), metadata, emit)` and returns
   `result.artifact`. `publish_full_stack_script` is NOT needed (FULL_STACK_SCRIPT_COMPLETED is
   removed in Phase D; tests migrate away from it).
2. **terminal routing in `_executor_script`** (currently only emits `submit_execution_success`).
   Add: `fail`/`fail:<reason>` → `submit_execution_blocker`; `request_recursive_goal:<id>` /
   `request_recursive_matrix:<id>` → `submit_execution_handoff` (recursive goal via
   `scenario.recursive_handoff_goal(ctx)`), each preceded by its own `ask_advisor` turn for THAT
   terminal, then return. Also wire `MutableMockState.consume_failure(role="...", attempt_id, checkpoint)`
   for failure-injection (the verifier path already uses it; method exists in registry.py).
   **Shared executor path — re-run high_concurrency + the 3 proof tests after.**
3. **test migration** — `test_full_stack_adversarial.py` + `test_full_system_capacity_matrix.py`
   (+ full_case) still assert lifecycle-event counts the new runner doesn't emit. Map specs:
   capacity at MIGRATION_MAP ~1611-1765, full_stack ~1765+.

### Recovered: full Executor-action catalogue (MIGRATION_MAP §1174 was BLANK — API error)
Source: `runner.py:_run_executor` (372-829). Terminal disposition per action:
- `fail` / `fail:<reason>` → **submit_execution_blocker** (return).
- `request_recursive_goal:<id>`, `request_recursive_matrix:<id>` → **submit_execution_handoff** (return).
- everything else → **submit_execution_success** (after the action loop).
- generator probes (PROBE_BUILDERS): `preflight`, `sandbox_integrity`, `final_probe`.
- script-engine (PreparedToolScriptEngine): `inspect_user_input`, `execute_package:<id>`,
  `final_reconciliation`, `inspect_full_user_input`, `occ_conflict_matrix`, `overlay_edge_matrix`,
  `layerstack_squash_lease`, `lsp_refresh_semantics`, `recursive_oversized_matrix`,
  `full_stack_final_reconciliation`, `capacity_metrics_full_system`, `recursive_step`.
- bridge probes: `high_concurrency_*` ✅, `heavy_io_zoned_*` ✅, `ephemeral_workspace_*` ✅ (minus
  cancellation), `plugin_*` ✅, `complex_project_build*` (§B), `background_*` (§C), `auto_squash_commit_resume_probe` (fan-out).

### Hazards for the test-migration phase
- **`count_role_tasks` signature conflict.** The LIVE `_focused_scenario_contracts.count_role_tasks(report, role, *, status)`
  (report-level, `agent_name` match; used by high_concurrency test) is INCOMPATIBLE with the
  per-attempt `count_role_tasks(attempt, *, role, agent_name, status)` the MIGRATION_MAP's
  test-migration specs assume. Reconcile as single writer (keep report-level OR add a distinct
  per-attempt helper name) and tell migration authors the live signature. `_focused_scenario_contracts.py`
  still has `_assert_ordered_subsequence` + `_assert_event_counts` + `FocusedScenarioCase.min_event_counts/absent_events`
  (all removed during the test-migration / Phase D).
- **MIGRATION_MAP gaps**: the Executor-action catalogue (§1174) and `test_correctness.py` (§1182)
  sections are blank (`API Error` during generation). Catalogue recovered above; `test_correctness`
  must be derived from source.

### Workflow note
The anchor-validation workflow's `agentType: 'Explore'` + `schema` combo failed to emit
StructuredOutput on 3 of 6 long-running agents ("completed without calling StructuredOutput").
The 3 that succeeded were shorter analyses. For schema-forced fan-out, prefer the default
workflow subagent over `Explore`, or keep per-agent scope small.

### Files changed this session (unstaged)
- `backend/src/task_center_runner/agent/mock/probe_bridge.py` (+heavy_io_zoned ×3, +plugin ×6,
  +ephemeral ×4 branches; same_path_conflict intentionally left unbridged → clean NotImplementedError)
- `backend/src/agents/profile/main/executor.md` (+lsp.apply_workspace_edit, +enter/exit_isolated_workspace).
  Verified regression-clean: named default-off parity gate `test_engine` + `test_tool_execution.py`
  + `test_terminal_call_reminder.py` (198 passed) + 90 agent/spawn/iws unit + docker (high_concurrency + 3 proof tests).
- `backend/src/task_center_runner/agent/mock/event_source.py` — removed dead Phase-1 scaffolding
  `turns_to_script` (zero callers; adapter builds per-role coroutines that branch on results) + its `__all__` entry.
- Dead-code sweep of the 5 migration files (probe_bridge/scenario_adapter/event_source/probes/scenario_loop_runner):
  removed `probe_bridge._DONE` (unused sentinel; the queue uses a `"done"` string). ruff F401/F811/F841 clean.
  `_UnusedApiClient` (scenario_loop_runner) is a deliberate documented stub — kept. The repetitive
  `bridge_probe_for` branches are intentional (lazy per-family imports keep the package graph DAG-shaped);
  a table-driven rewrite would over-abstract / break lazy imports — NOT refactored.
- **Legacy NOT removed (correctly): `MockSquadRunner` internals + the 14 lifecycle EventTypes are Phase-D,
  gated behind the default-off flag (still the active path for all 144 mock tests). Removing now breaks them.**

### Pre-existing test failures (parallel-agent collateral, NOT this work)
`tests/mock/contracts/test_runner_imports.py` has 4 reds (3× `prompt_inspector_*`, `registered_mock_agents_install_and_restore`),
confirmed pre-existing by stashing this session's 3 tracked files and re-running (still red). Cause: the
parallel context-engine "rework role recipes" work (HEAD advanced to `ff476401e` mid-session; `context_engine/*`
still dirty) drifted the planner-context vocab the inspector fixtures string-match. Owner = the context-engine
agent; vocab still in flux so not fixed here (see memory `mock_runner_inspects_planner_context_vocab`).
- `docs/plans/mock_event_source_FANOUT_HANDOFF.md` (this update)
