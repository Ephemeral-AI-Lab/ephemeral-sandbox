# Handoff: ScenarioEventSource migration (replace MockSquadRunner)

> **⚠️ Phases 2/3/4 below are SUPERSEDED.** They predate the tool-call-budget /
> planner→generator fan-out realization. For current Phase 2+ status, the
> validated fan-out architecture, the queue-bridge infrastructure, and the
> ordered remaining work, follow **`docs/plans/mock_event_source_FANOUT_HANDOFF.md`**.
> Phases 0 and 1 (the seam + 3 simple probes) described below are still accurate
> and GREEN.

**Status as of 2026-05-29.** Implements `docs/plans/mock_event_source_IMPL_PLAN.md`.
Goal: drive every mock agent through the **real** `engine/query/loop.py` via an
injected per-agent event source, so the mock and a real agent differ *only* in
the event source — then delete the imperative `MockSquadRunner` and its
mock-only lifecycle events.

**Phases 0 and the core of Phase 1 are DONE and proven on docker.** The hard
design + integration risks are retired; what remains is mechanical migration
following the now-proven pattern.

| Phase | Scope | Status |
|---|---|---|
| 0 | seam (default-off) + portability/budget spike | ✅ DONE (gate passed) |
| 1 | `ScenarioLoopRunner` + adapter + 3 probes → CorrectnessTesting green | ✅ core PROVEN; ⚠️ 2 PRESERVE helpers pending |
| 2 | port remaining ~10 probes + scripts; migrate ~20 assertions; flip flag | ❌ NOT STARTED |
| 3 | delete MockSquadRunner internals + 14 lifecycle EventTypes | ❌ NOT STARTED |
| 4 | ultra bundle + 5-area coverage | ❌ NOT STARTED |
| final | all 144 mock tests green (goal #3) | ❌ NOT STARTED |

---

## How to run / verify (read this first)

- **Use the uv venv**, never global pytest/ruff: `backend/../.venv/bin/python -m pytest …`
  (global pytest reports spurious failures — pytest-asyncio not loaded).
- Mock scenario tests run against a **docker sweevo sandbox on this host** (no
  skip; gates true + docker provider). They are gated on `database_configured()`.
- The new runner is **behind a flag**, default OFF:
  `EOS_MOCK_EVENT_SOURCE_RUNNER=1` selects `ScenarioLoopRunner`; unset keeps the
  old `MockSquadRunner`. This lets the 144 existing tests stay green during
  migration.
- The mock path now goes through `spawn_agent`, which needs an **active model
  row**. Tests register a throwaway one (see `_active_mock_model` fixture in the
  proof tests). `ensure_runtime_stores_ready()` does NOT work here (no
  `models/registry.json`).

**Currently-green proof tests (all PASS on docker):**
```bash
cd backend
../.venv/bin/python -m pytest \
  src/task_center_runner/tests/mock/contracts/test_scenario_event_source_spike.py \
  src/task_center_runner/tests/mock/contracts/test_scenario_loop_runner_planner_submit.py \
  src/task_center_runner/tests/mock/contracts/test_correctness_via_event_source.py \
  -v -p no:cacheprovider
```
**Default-off parity (production unaffected by the seam):**
```bash
cd backend
../.venv/bin/python -m pytest tests/unit_test/test_engine \
  tests/unit_test/test_tools/test_tool_execution.py \
  tests/unit_test/test_notification/test_terminal_call_reminder.py -q   # 198 passed
```

---

## The validated architecture

A mock agent run = the **real** `run_ephemeral_agent` → `spawn_agent` → `run_query`
loop, with `QueryContext.event_source` set to a `ScenarioEventSource` instead of
streaming from the provider `api_client`.

- **Seam (production code, default-off):** `event_source` defaults `None` ⇒
  byte-identical to production. `RuntimeConfig.event_source_factory(agent_def)`
  builds the per-agent source; `spawn_agent` assigns it.
- **`ScenarioEventSource`** (`agent/mock/event_source.py`): one `__call__` per
  loop turn. Reads trailing `ToolResultBlock`s from the built request, advances
  the agent's turn-coroutine, and emits **one `ToolUseDeltaEvent` per tool_use**
  (REQUIRED for budget parity — the loop counts at stream time and populates
  `streamed_tool_use_ids`) followed by one `AssistantMessageCompleteEvent`.
- **Two-level coroutine bridge** (key constraint): `ScenarioEventSource` ↔
  `TurnScript` (yields `Turn`, receives `list[ToolResultBlock]`). Probe
  sub-coroutines yield a single `ToolCall` and receive a normalized `ToolResult`.
  The role `TurnScript` drives a probe via an `asend` loop where
  `yield Turn(calls=(call,))` **must live in the top-level generator** (Python
  forbids hiding an async-gen yield inside a helper method).
- **Lazy `script_builder(context)`**: `spawn_agent` passes the factory only the
  `AgentDefinition`, so the role script is built on the first `__call__` from
  `context.tool_metadata` (task_id, `attempt_runtime`).
- **Adapter-all** (decided, do not reopen): keep the tested imperative probe
  bodies; the transform is `await self._call_tool(t, args, …)` →
  `result = yield ToolCall(t.name, args)`. `ProbeContext` re-homes the
  out-of-band sandbox verification.

---

## ✅ DONE

### Phase 0 — the seam (Component A) + spike
Modified (all default-off; 198 unit tests green):
- `backend/src/engine/query/context.py` — `EventSource` type alias +
  `QueryContext.event_source` field.
- `backend/src/engine/query/loop.py` — `_provider_event_source()` + the single
  swap in `_consume_provider_stream`: `source = context.event_source or _provider_event_source`.
- `backend/src/runtime/app_factory.py` — `RuntimeConfig.event_source_factory`.
- `backend/src/engine/agent/factory.py` — `spawn_agent` sets
  `query_context.event_source` from the factory.

Spike (PASSED): `backend/src/task_center_runner/tests/mock/contracts/test_scenario_event_source_spike.py`
— 3 tests proving: tool effect via real dispatch; terminal-alone enforcement;
budget parity exact (foreground=1, rejected-batch=2 counted-but-not-executed,
background=2) via the per-tool delta emission; terminal tool_use in returned messages.

### Phase 1 — runner + adapter + probes (CORE PROVEN)
New files:
- `backend/src/task_center_runner/agent/mock/event_source.py` — `ScenarioEventSource`,
  `Turn`, `ToolCall`, `TurnScript`, `latest_tool_results`, `turns_to_script`.
- `backend/src/task_center_runner/agent/mock/scenario_adapter.py` — `scenario_script_for`
  (role → `TurnScript`), `build_scenario_context`, `normalize_result`. Handles
  roles: planner / executor / verifier / evaluator / **advisor**. Emits an
  `ask_advisor` turn before each gated terminal.
- `backend/src/task_center_runner/agent/mock/scenario_loop_runner.py` —
  `ScenarioLoopRunner` (thin `AttemptAgentRunner`: publishes `MOCK_LAUNCH_RECORDED`,
  bridges loop `ToolExecutionCompletedEvent` → `MOCK_TOOL_CALL_RECORDED`, sets
  `event_source_factory`, delegates to `run_ephemeral_agent`) +
  `make_mock_runtime_config`.
- `backend/src/task_center_runner/agent/mock/probes.py` — `ProbeContext` +
  `preflight_probe` / `sandbox_integrity_probe` / `final_probe` (the 3
  CorrectnessTesting needs), `PROBE_BUILDERS`, `PROBE_SUMMARY`.

Modified:
- `backend/src/task_center_runner/scenarios/builder.py` — flag
  `EOS_MOCK_EVENT_SOURCE_RUNNER` selects the runner; always injects a real
  `RuntimeConfig` into `extras["runtime_config"]` (safe for the old runner).
- `backend/src/task_center_runner/agent/mock/definitions.py` —
  `mock_agent_definitions()` now also loads `helper/` (advisor) + `subagent/`
  (explorer), which the real loop spawns.

Proof tests (PASS on docker):
- `test_scenario_loop_runner_planner_submit.py` — planner→executor→evaluator
  terminals + advisor sub-agent through the full pipeline; goal `done` in store.
- `test_correctness_via_event_source.py` — full CorrectnessTesting (eval-fail
  retry → partial-plan defer → continuation) + 3 probes; asserts `graph_summary`
  + `passed_sandbox_checks`.

### Three integration realities the plan under-specified (resolved + tested)
1. **RuntimeConfig threading** — `core/engine.py:run_pipeline` (~234) passed a
   bare `SimpleNamespace(cwd=…)` to `start_task_center_run`; the runner needs a
   real `RuntimeConfig`. Fixed by injecting it via `extras["runtime_config"]`.
2. **Advisor gate** — ALL submission terminals carry `AdvisorApprovalPreHook`
   (`tools/_hooks/advisor_approval.py`). The old runner injected synthetic
   approval (`_approve_terminal` + `agent/mock/_advisor_approval.py`); the new
   path emits a real `ask_advisor(tool_name=<terminal>, …)` turn whose advisor
   sub-agent is scripted to approve. Gate passes when `conversation_messages`
   holds the latest advisor `ToolResultBlock` (`helper_role=="advisor"`,
   `verdict=="approve"`, not error) paired by `tool_use_id` with an originating
   `ask_advisor` call whose `tool_name` == the terminal. Required registering
   the advisor/explorer profiles (the mock never spawned them before).
3. **Notification robustness** — `latest_tool_results` scans BACKWARD for the
   most recent user message with `ToolResultBlock`s (the loop appends a reminder
   user-message at the top of each turn after the tool-result message).

---

## ❌ NOT DONE

### Phase 1 remainder — PRESERVE helpers (fold into Phase 2; coupled to its assertions)
`ScenarioLoopRunner` does NOT yet port:
- `_inspect_prompt` → `MOCK_PROMPT_INSPECTED` (populates `report.prompt_inspections`).
  Source: `runner.py:1748-1840` (+ `_current_attempt_and_iteration` 1865-1879).
- `_record_initial_messages` (writes `message.jsonl`). Source: `runner.py:1842-1863`.
The graph-based proofs don't need these, but `tests/mock/task_center/test_correctness.py`
asserts `passed_prompt_inspections` and `message.jsonl` content — so port them
when migrating that test.

### Phase 2 — migrate the rest (the bulk)
1. **Port the other ~10 probes** to coroutine + `ProbeContext` (mechanical:
   `_call_tool` → `yield ToolCall`; out-of-band sandbox work → `ProbeContext`
   helpers). Sources in `runner.py`:
   `_run_auto_squash_commit_resume_probe` (1083), `_run_complex_project_build_probe`
   (1258), `_run_high_concurrency_{seed,worker,reconcile}_probe` (1282/1298/1319),
   `_run_heavy_io_zoned_{seed,worker,reconcile}_probe` (1335/1351/1372),
   `_run_background_shell_probe` (1388), `_run_ephemeral_workspace_probe` (1440),
   `_run_plugin_workspace_probe` (1475),
   `_run_complex_project_build_shell_edit_lsp_probe` (1510),
   `_run_complex_project_build_grep_glob_probe` (1536).
2. **Port the `PreparedToolScriptEngine` scripts** (`agent/mock/tool_scripts.py`,
   `full_stack_tool_scripts.py`, `capacity_actions/`) — the executor actions
   `execute_package:`, `*_matrix`, `*_reconciliation`, `inspect_*_user_input`,
   `lsp_refresh_semantics`, `layerstack_squash_lease`, `recursive_step`,
   `capacity_metrics_full_system`, etc.
3. **Extend `_executor_script`** (`scenario_adapter.py`) to handle ALL executor
   action strings (currently it raises `NotImplementedError` for anything beyond
   preflight/sandbox_integrity/final_probe). Full list at `runner.py:_run_executor`
   (~382-823): `fail`/`fail:`, `request_recursive_goal:`, `request_recursive_matrix:`,
   and the script/probe actions above. Recursive handoff uses
   `submit_execution_handoff`; failure uses `submit_execution_blocker` (both gated).
4. **Add shared graph-shape helpers** to `tests/mock/_focused_scenario_contracts.py`:
   `count_role_tasks`, `attempt_outcome`, `recursive_goals` (event→graph mapping
   in IMPL_PLAN §4.1).
5. **Migrate ~20 focused-scenario assertions** from `min_event_counts` /
   `expected_event_sequence` → `graph_summary` (workflow fan-out by test file,
   AFTER the helpers land — every migrated test imports them). Migrate
   `test_correctness.py` (its "no `ask_advisor` in transcript" assertion at
   lines 168-198 is now INVERTED — real `ask_advisor` turns DO appear; and its
   `count_events(PLANNER_INVOKED/EVALUATOR_INVOKED)` hooks key off deleted events).
6. **Flip the flag default ON** in `builder.py` once scenarios are ported; keep
   `test_scenario_suite_imports.py` green.
7. **Consolidate** the duplicated `_active_mock_model` fixture into
   `tests/mock/conftest.py`.

### Phase 3 — delete the imperative engine + lifecycle events
- Delete `MockSquadRunner` internals (~1900 LoC): `_run_planner/_run_executor/`
  `_run_verifier/_run_evaluator`, `_call_tool`, `_approve_terminal`, all
  `_run_*_probe`, `_record_tool_check`, `_script_engine`, the `_*_EVENT_BY_TOOL`
  maps, and every lifecycle `_publish(EventType.*_INVOKED/…)`.
- Delete `agent/mock/_advisor_approval.py` + its unit re-export.
- Remove the lifecycle EventTypes from `audit/events.py` ("agent invocations"
  block): `PLANNER_INVOKED`, `PLANNER_COMPLETES_GOAL_PLAN`, `PLANNER_DEFERS_GOAL_PLAN`,
  `PLANNER_REPLAN`, `EXECUTOR_INVOKED/SUCCESS/FAILURE`, `VERIFIER_INVOKED/SUCCESS/FAILURE`,
  `EVALUATOR_INVOKED/SUCCESS/FAILURE`, `RECURSIVE_GOAL_REQUESTED/COMPLETED`,
  `FULL_STACK_SCRIPT_COMPLETED`. **KEEP (re-homed, still emitted by `ProbeContext`):**
  `SANDBOX_BATCH_EDIT_APPLIED`, `SANDBOX_CONFLICT_DETECTED`, `MOCK_SANDBOX_CHECK_RECORDED`,
  `MOCK_LAUNCH_RECORDED`, `MOCK_TOOL_CALL_RECORDED`, `MOCK_PROMPT_INSPECTED`,
  `SANDBOX_TOOL_CANCELLED`.
- Remove `Scenario.expected_event_sequence` (`scenarios/base.py:51,74`) + every
  per-scenario declaration; remove `RunReport.seen_event_types` +
  `_assert_ordered_subsequence`/`_assert_event_counts` machinery in
  `_focused_scenario_contracts.py`.
- `hooks/builtins.py` ALSO emits `VERIFIER_INVOKED`/`VERIFIER_SUCCESS` — drop those.
- Repoint `tests/mock/contracts/test_advisor_gate_negative_path.py`: a blocked
  terminal is now a scripted `ask_advisor` REJECT turn (inject via
  `MutableMockState.consume_advisor_verdict()` — method NOT yet added; the
  adapter's `_advisor_script` already reads it via `getattr`).

### Phase 4 + final sweep
- Author `ultra.full_system_bundle` + 5-area coverage as turn-scripts; assert via
  store state. This is the case that exercises `run_subagent`/explorer (background)
  + the #2 ceiling path — where the budget-parity delta emission matters.
- Run all 144 mock tests under `backend/src/task_center_runner/tests/mock` green
  (goal #3) — use a workflow fan-out to run subsets in parallel + classify
  failures. 33 of the 144 reference `agent.mock`.

---

## Critical gotchas for the next agent

- **Adapter-all is decided** — do not rewrite probes native; transform call sites.
- **The advisor gate** means every gated terminal needs a preceding `ask_advisor`
  turn + a scripted advisor sub-agent. The advisor/explorer profiles must be
  registered (done in `definitions.py`).
- **Budget parity** depends on emitting one `ToolUseDeltaEvent` per tool_use.
  Don't drop it.
- **`yield` must be top-level** in the `TurnScript` generator — drive probe
  sub-generators via an explicit `asend` loop (see `_executor_script`).
- **Per-turn reminder messages** can displace the trailing tool-result message —
  `latest_tool_results` scans backward (don't "simplify" it to `messages[-1]`).
- **Parallel agents edit this repo.** A dirty worktree is expected. Files NOT
  part of this task (do not touch): `docs/plans/ultra_complex_bundled_scenario_TEST_PLAN.md`
  (modified by another agent), `docs/plans/planner_prior_iteration_context_IMPL_PLAN.md`
  (added by another agent). Stage with explicit paths; never `git add <dir>`.

## File map
- Plan: `docs/plans/mock_event_source_IMPL_PLAN.md` (authoritative spec; note its
  `launch.py:40,134` refers to `backend/src/task_center/attempt/launch.py`).
- Auto-memory (richer detail): `~/.claude/projects/-Users-yifanxu-machine-learning-LoVC-EphemeralOS/memory/`
  → `mock_event_source_seam_integration_map.md`, `mock_event_source_phase1_adapter_design.md`,
  `mock_event_source_must_emit_tool_use_deltas.md`, `mock_scenario_bypasses_engine_loop.md`.
