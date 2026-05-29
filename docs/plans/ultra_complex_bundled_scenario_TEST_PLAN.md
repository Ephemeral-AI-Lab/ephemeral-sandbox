# Plan: ultra-complex bundled scenario test (mocked agent tools, real docker sandbox)

Status: PLAN (decisions locked 2026-05-29). Ready for phased implementation.
Owner: (you) + planner handoff
Provider: **docker only** (`EOS_SANDBOX_PROVIDER=docker`, persistent `sweevo-<instance_id>`). Daytona out of scope.
Scope: a `task_center_runner` scenario **bundle** that drives the **real** TaskCenter workflow + **real**
docker sandbox operations with **mocked agent tool calls**, exercising all five areas the user named.
Reuse `backend/src/task_center_runner` infra; build new code only where a genuine gap exists.

---

## 0. The load-bearing architectural fact (read first)

There are **two different "mock agent" engines** in this repo, and the five requested areas split
across them. The user chose to build BOTH (see §2).

| Engine | What it is | Drives engine loop? | Drives tool prehooks? | Covers |
|---|---|---|---|---|
| **A. `MockSquadRunner`** (`agent/mock/runner.py`) | Per-role scripted tool decisions; replaces the `AttemptAgentRunner`. | **No** — `loop.py` never runs. | **Yes** — `_call_tool`→`execute_tool_once`→`run_pre_hooks` (`tool_call.py:137,161`). | #1 (file/shell/bg/IWS via probes), #3 (full workflow), #4 (advisor + prehooks), #5 (sandbox layers). |
| **B. Scripted provider + real loop** | Inject `RuntimeConfig.external_api_client=FakeScriptedProvider`; the **real** `run_ephemeral_agent` loop streams scripted `tool_use` blocks. | **Yes** | Yes (same `execute_tool_once`). | #2 (reminder, **150%** hard-fail, `run_exhausted` propagation), #1-**explorer** (mocked subagent tool calls). |

Verified anchors: runner swap at `task_center/attempt/launch.py:102-107`; the engine loop lives only
behind `run_ephemeral_agent` (`launch.py:104`, `engine/query/loop.py:170,212-218,280-294`);
`make_api_client(external)` returns the injected client unchanged (`providers/provider.py:34-35`); the
external client threads through `RuntimeConfig.external_api_client` (`runtime/app_factory.py:51`) →
`config.extras["runtime_config"]` (`core/engine.py:234-237`; example `core/real_agent_run.py:74-88`) →
`QueryContext.api_client` (`engine/agent/factory.py:381`).

---

## 1. Per-area findings — request vs. current state vs. real gap

| # | User's ask | Current state (verified) | Real gap / where it lives |
|---|---|---|---|
| 1 | All tools: file/shell, background (shell + run_subagent), isolated_workspace, **explorer subagent (mocked)**, submission terminals | File/shell/grep/glob, background-shell, enter/exit-IWS, all 11 terminals each have an **existing probe/action** (`agent/mock/*_probe.py` + `_run_executor` branches). **`run_subagent`/explorer is NOT driven by any mock action**; it spawns a *real* engine subagent with a fresh real client (`run_subagent.py:228`, `factory.py:195-197`). | **GAP: explorer** → engine-B + relax `factory.py:195`. All other tools = compose existing probes into the bundle. |
| 2 | Loop: end-with-text → reminder → retry; **budget notifications**; max-tool-call **hard failure** + TaskCenter handling | **One** rule `make_terminal_call_reminder` (always-fires, `fire_once=False`, reports live budget — `factories.py:17-51`). Hard-ceiling = `ceil(1.5×limit)` = **150%**, exit `TERMINAL_NOT_SUBMITTED` (`loop.py:41-57,280-294`). TaskCenter maps no-terminal → role `fail_reason="run_exhausted"` (`attempt/launch.py:254-296`). | **No 75/100/125% tier RULES exist** (verified: whole `notification/` tree = 7 files). Test the single reminder at the 75/100/125% budget marks + 150% hard-fail via **engine-B**. Reachable only on the real loop. |
| 3 | goal→iteration→attempt→planner(defer/full)→generator DAG→`submit_execution_handoff`→nested goal→evaluator; all failure modes; retry→max attempts; success+deferred→new iteration; nested **max level 2** | Fully implemented + covered by `pipeline.*` + `full_stack_adversarial`. Nested **depth>1 silently strips `submit_execution_handoff`** (`_core/terminal_tool_routing.py:51-59,137-150`); attempt budget default **2** (`_core/primitives.py:47`); 5 failure modes (`attempt/orchestrator.py`, `stage_advancer.py`). | **Mostly EXISTING.** Bundle composes into one scenario; `full_stack_adversarial.py` is the template. |
| 4 | Prehooks: `block_in_isolated_mode`, `require_no_in_flight_background_tasks`, `advisor_approval` (**4 cases**) | Prehooks **run in the mock path**. `AdvisorApprovalPreHook` binds approval to `tool_name` (`advisor_approval.py:87`); 4 cases unit-tested (`test_advisor_approval_prehook.py`) + scenario negative-path (`tests/mock/contracts/test_advisor_gate_negative_path.py`); `_approve_terminal` (`runner.py:317-338`) synthesizes the approve pair the prehook checks. | **EXISTING + composable.** Vary `_approve_terminal` `tool_name`/`verdict`/`is_error`/omission inside the bundle. |
| 5 | Sandbox: layerstack (stale lease), overlay, OCC (gitignore-aware), ephemeral, isolated (discard at exit); **3 agents same port 3000** | Lease/version (`layer_stack/lease.py`); OCC `ABORTED_VERSION/OVERLAP` (`occ/changeset.py:144-163`); ephemeral OCC-gated publish; IWS discard + **per-IWS `unshare --net` netns** (`isolated_workspace/_control_plane/namespace_runtime.py:79-116`, veth `network.py:102-146`). Scenarios 0–3 pass on docker here. | **Mostly EXISTING.** Bundle wires these probes into the DAG so one workflow run drives all five layers. |

---

## 2. Decisions (LOCKED)

- **D-A — Area #2 = test the REAL mechanics (no new tier rules).** The described 75/100/125% tiers do
  NOT exist; the implementation is the single repeating `make_terminal_call_reminder`
  (`backend/src/notification/rules/factories.py:17`) + a **150%** `TERMINAL_NOT_SUBMITTED` hard-fail in
  the engine (`backend/src/engine/query/loop.py:41-47`). Tests drive a scripted agent across the
  75/100/125% budget marks and assert the reminder's live budget text + the 150% hard-fail +
  TaskCenter `run_exhausted` propagation. *(If distinct tier rules are wanted later, that's separate
  feature work in `notification/rules/`.)*
- **D-B/D-C — Build the largest scope: MockSquadRunner bundle + engine-B for #2 AND explorer.** A
  `MockSquadRunner` "ultra" scenario covers #1(−explorer)/#3/#4/#5; a new **engine-B harness**
  (scripted provider through the real loop) covers #2 and the **mocked explorer subagent**, which
  requires a narrow relaxation of `engine/agent/factory.py:195` so an injected client reaches
  `AgentType.SUBAGENT`.

---

## 3. The MockSquadRunner bundle (`ultra.full_system_bundle`)

New scenario subclassing `ScenarioBase`, registered in `scenarios/__init__.py`. A **superset of
`full_stack_adversarial`** that injects the sandbox-heavy probes (#5) + prehook cases (#4) into a
multi-iteration, multi-attempt DAG (#3) exercising the tool palette (#1−explorer).

**Iteration 1 — planner DEFERS.** `submit_plan_defers_goal` → phase-1 plan + `deferred_goal_for_next_iteration`.
Generators run ephemeral file ops (read/write/edit/multi_edit/glob/grep) + foreground shell, OCC-published.
Evaluator passes → deferred goal carries to iteration 2 (proves defer→new-iteration inheritance).

**Iteration 2 — planner closes goal with a DAG** (diamond `a,b,c → d`):
- **Task a — ephemeral + OCC:** concurrent same-path writes → exactly **1 `ACCEPTED`, rest `ABORTED_VERSION`**;
  disjoint writes all land (reuse `ephemeral_workspace_probe` / `occ_concurrent`).
- **Task b — background mixed-op:** `pytest`+`pip`+`edit_loop` background tasks → all terminal; overlapping
  bg edits → 1 winner + conflict losers (reuse `background_shell_probe` mixed-op).
- **Task c — isolated workspace + port 3000:** enter IWS → start real server on **port 3000** → exit
  (discard); plus 3 concurrent IWS same-port → no `EADDRINUSE` (netns). Then **handoff**:
  `submit_execution_handoff` → nested goal (**depth 2**); nested planner `submit_plan_closes_goal`; assert a
  depth-2 generator **cannot** see `submit_execution_handoff` (guardrail).
- **Task d — verifier (deps a,b,c):** force a **one-shot failure** via `mutable_state.consume_failure` →
  attempt 1 fails → **retry** → attempt 2 passes.

**Prehook cases in the bundle (#4) — HAPPY paths only (see §3.1 for why):** advisor happy-approve
(automatic via `_approve_terminal`); the cleanup choreography `require_no_in_flight_background_tasks`:
generator with in-flight bg → `cancel`/`wait` → terminal submits clean. The prehook-**block** negatives
(advisor reject/wrong-tool/missing; in-flight-bg-blocks-terminal; `ask_advisor`-in-IWS) stay as **focused
contract tests**, NOT scenario plumbing — `_call_tool` raises on a blocked terminal by design.

**Evaluator (attempt 2):** `submit_evaluation_success` → no deferred goal → goal closes `done`.

**Failure-mode matrix (across attempts/cells):** planner failure (invalid plan), generator failure
(`fail:`→`submit_execution_blocker`), verifier failure (`consume_failure`), evaluator failure (forced
fail→pass), nested-child failure (handoff child fails → parent generator fails), attempt-budget-exhausted.

**Assertions:** `report.task_center_status == "done"`; `passed_prompt_inspections`; `passed_sandbox_checks`;
`report.graph_summary` shape (iteration_count=2, attempt counts, per-role event counts); probe JSON
read-backs via `sandbox_api.read_file`; OCC shapes; `await report.performance_report_task` then V3 sections
(`occ`, `isolated_workspace`, `overlay_workspace`, `background_tool_calls`, lowerdir-O(1) keys).

> Debuggability hedge: prefer adding the sandbox-heavy DAG tasks + prehook cases as **new cells in the
> existing `full_stack_adversarial` matrix** (per-cell metrics) over one opaque monolith, or split into
> 2–3 cooperating scenarios sharing probes.

---

## 3.1 MockSquadRunner modification surface (what actually changes in `runner.py`)

The bundle is primarily a **new scenario file**; `MockSquadRunner` (`agent/mock/runner.py`, 2043 lines) is
**extended, not rewired**. In the minimal/recommended path it needs **no core change** — the scenario
composes existing executor actions. Add to the runner only for novel choreography, via the established
4-step recipe (model: `_run_ephemeral_workspace_probe`):
1. Add a probe fn in a `*_probe.py` that drives real tools via `self._call_tool(...)` and writes a JSON summary.
2. Add a mode entry to the relevant `_run_<x>_probe` dispatcher (e.g. `_run_background_shell_probe` mode dict, runner.py:1410-1438).
3. Add an `elif action == "<verb>":` branch in `_run_executor` (runner.py:372-816; default `submit_execution_success` at 817-822).
4. The scenario's `executor_actions(ctx)` returns `"<verb>"` for that task.

**REUSE (no runner change):** file ops, fg/bg shell, ephemeral+OCC (`occ_conflict_matrix`; ephemeral modes
`all_verbs`/`concurrent_writes`/`same_path_conflict`/`o1_disk`), background mixed-op (`mixed_op_concurrent`),
enter/exit IWS (`background_exit_iws_drains_agent_tasks` action → `exit_iws_drain` mode, runner.py:755,1417),
handoff→nested (`request_recursive_goal:`/`request_recursive_matrix:`), failures (`fail:`, `consume_failure`).

**Only genuinely-new runner candidate (OPTIONAL):** a bespoke "3 IWS on port 3000 in-workflow" action+probe.
Same-port netns isolation is already proven by the standalone IWS suite + scenarios 0–3; add this only if you
want it driven *inside* the workflow run.

**DO NOT modify `runner.py` for:**
- **Advisor #4 negatives** — `_approve_terminal` (317-338) is the single chokepoint and `_call_tool` **raises**
  on a blocked terminal (1712-1713). `tests/mock/contracts/test_advisor_gate_negative_path.py` deliberately
  keeps reject/wrong-tool/missing as focused tests (*"A reviewer tempted to 'fix' this by adding scenario
  plumbing should not"*). The bundle uses only the happy approve path.
- **#2 + explorer** — these run on **engine-B** (real `run_ephemeral_agent` + `FakeScriptedProvider`), which
  does not use `MockSquadRunner`; the `factory.py:195` change is in `engine/agent`, not the mock runner.

Non-terminal prehook blocks (`enter_isolated_workspace` w/ in-flight bg; `ask_advisor` in IWS) *can* be a probe
using `_call_tool(..., allow_error=True)` (precedent runner.py:1190) that asserts the BLOCKED result — they
aren't terminals, so they skip the raise-on-error path. Otherwise keep them focused.

## 3.5 The engine-B companion harness (NEW infra — for #2 and explorer)

A scripted provider driving the **real** `run_ephemeral_agent` loop, beside the mock path.

- **`FakeScriptedProvider(SupportsStreamingMessages)`** — per `stream_message` call emits the next scripted
  assistant turn keyed by `(agent role/name, turn)`: `ToolUseDeltaEvent`(s) + `AssistantMessageCompleteEvent`,
  or a **text-only** turn (no tool_use) for the no-terminal path. The bulk of the new code; it is a test
  fixture, not production. (~150–250 LoC.)
- **New RunConfig assembler** mirroring `core/real_agent_run.py`: `runner_factory` returns `None` (production
  runner → real loop) and `extras={"runtime_config": RuntimeConfig(cwd=..., external_api_client=FakeScriptedProvider(...))}`.
  Reuse `bootstrap_real_agent_runtime` (`core/bootstrap.py`) for agent-registry + model seeding (a `db_kwargs`
  model row must exist or `factory.py:177-182` raises).
- **`factory.py:195` relaxation (production change, narrow):** today `needs_fresh_client = (agent_def.agent_type == AgentType.SUBAGENT)` forces a fresh REAL client for subagents (line 197 nulls `external`). Gate this behind a flag so a test-injected `external` client also reaches subagents; production behavior unchanged by default. Required so the explorer's tool calls are mockable.
- **#2 tests (real loop):**
  - Scripted agent emits non-terminal tool calls; as the count crosses **75% / 100% / 125%** of
    `tool_call_limit`, assert `make_terminal_call_reminder` is injected with the correct
    `used/{limit}` + `ceiling` budget text (`factories.py:32-44`).
  - Continue to **150%** → assert `QueryExitReason.TERMINAL_NOT_SUBMITTED` (`loop.py:41-47,280-294`) and the
    `EphemeralRunResult` carries no terminal → TaskCenter `_report_exhaustion` fires the role
    `fail_reason="run_exhausted"` (`attempt/launch.py:254-296`) → attempt failure → retry/iteration-fail
    propagation. **(This `run_exhausted` path is reachable ONLY here, not from `MockSquadRunner`.)**
  - text-only-no-terminal turn → reminder injected → next scripted turn submits a terminal (retry-after-nudge).
- **Explorer test (real loop):** scripted executor calls `run_subagent(agent_name="explorer", ...)`; the
  explorer subagent (now fake-clientable) runs scripted `read_file`/`grep`/`glob` then
  `submit_exploration_result`; assert `check_background_task_result` reports `subagent_terminal_called=True`,
  and a no-terminal explorer → marked failed.

---

## 4. Checklist (tagged EXISTING / NEW)

### Area #1 — tool palette
- [ ] EXISTING — file ops, fg/bg shell, enter/exit IWS, all 11 terminals (probes + `full_stack_adversarial`/`pipeline.*`).
- [ ] NEW — compose the above probes/actions into the bundle's DAG tasks.
- [ ] NEW — **mocked explorer** via engine-B (`run_subagent`→explorer→`submit_exploration_result`) + `factory.py:195` relax.

### Area #2 — loop / reminder / hard failure (engine-B)
- [ ] EXISTING — reminder + 150% hard-ceiling unit coverage (`test_terminal_call_reminder.py`, `test_hard_ceiling_behavior.py`, `test_terminal_not_submitted_transcript.py`, `test_tool_call_limit.py`, `_fake_provider.py`).
- [ ] NEW — engine-B test: reminder budget text at 75/100/125% marks → 150% `TERMINAL_NOT_SUBMITTED`.
- [ ] NEW — engine-B test: 150% hard-fail → TaskCenter `run_exhausted` role-failure propagation (in-workflow).
- [ ] NEW — engine-B test: text-only turn → reminder → retry submits.

### Area #3 — TaskCenter workflow
- [ ] EXISTING — defer/full-plan, generator DAG, evaluator sink, retry, attempt-budget=2, 5 failure modes, nested depth-2 guardrail (`pipeline.*`, `full_stack_adversarial`).
- [ ] NEW — bundle traverses defer (iter1) → DAG+handoff+nested (iter2) → verifier-retry → close.
- [ ] NEW — assert graph-shape (`graph_summary`) + event-count matrix.

### Area #4 — prehooks
- [ ] EXISTING — advisor 4 cases + `require_no_inflight_background_tasks` + `block_in_isolated_mode` wiring.
- [ ] NEW — bundle exercises HAPPY prehook paths only: advisor approve + bg cleanup-then-submit-clean (no runner change).
- [ ] EXISTING/NEW — prehook-BLOCK negatives stay focused contract tests: advisor wrong-tool+missing EXIST (`test_advisor_gate_negative_path.py`); ADD advisor-reject, enter-w/-in-flight-bg, ask_advisor-in-IWS (probe w/ `allow_error=True` or focused test). Do NOT add scenario plumbing to `_approve_terminal`.

### Area #5 — sandbox
- [ ] EXISTING — OCC conflict, ephemeral verbs, IWS same-port + discard, lowerdir-O(1), layerstack stale-lease pin.
- [ ] NEW — wire OCC + ephemeral + background + IWS-port-3000 probes into the bundle's DAG (one run, five layers).

### Cross-cutting / new infra
- [ ] NEW — `FakeScriptedProvider` + engine-B RunConfig assembler (reuse `real_agent_run.py` + `bootstrap_real_agent_runtime`).
- [ ] NEW — `factory.py:195` flag (test-injected external client reaches `AgentType.SUBAGENT`; prod unchanged).
- [ ] NEW — register `ultra.full_system_bundle` in `SCENARIO_REGISTRY` + `__all__`; pass `test_scenario_suite_imports.py`.
- [ ] NEW — paired tests under `tests/mock/sandbox/` (bundle) + `tests/mock/task_center/` or `tests/.../test_engine`-style (engine-B), gated `@pytest.mark.skipif(not database_configured())` (+ `not live_e2e_heavy_enabled()` for heavy).
- [ ] NEW — any novel sandbox choreography not covered by an existing action ⇒ add a `_run_executor` branch in `runner.py` and/or a new `*_probe.py` (budget this honestly).

---

## 5. Suggested build order (phased)
1. **Phase 1 — bundle skeleton (MockSquadRunner):** copy `full_stack_adversarial`, register `ultra.full_system_bundle`, get a minimal defer→close run green. Verify graph-shape.
2. **Phase 2 — wire #5 probes** (ephemeral/OCC, background mixed-op, IWS port-3000) into the DAG tasks; assert OCC + perf-report shapes.
3. **Phase 3 — weave #4 prehook cases** (advisor variants, in-flight bg, ask_advisor-in-IWS).
4. **Phase 4 — engine-B harness:** `FakeScriptedProvider` + assembler; cover #2 (reminder marks → 150% hard-fail → `run_exhausted`) and text→retry.
5. **Phase 5 — explorer:** `factory.py:195` flag + scripted explorer run via `run_subagent`.

## 6. Verification (run with `.venv/bin/pytest`, never global pytest)
```
env EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 EOS_SANDBOX_PROVIDER=docker \
    EOS_ISOLATED_WORKSPACE_ENABLED=true EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true \
    EPHEMERALOS_DATABASE_URL=sqlite:///./.ephemeralos/ephemeralos.db \
    uv run pytest -vv --tb=short -p no:randomly <bundle + engine-B test files>
```
- Bundle: `task_center_status=="done"`, prompt+sandbox checks, graph-shape, OCC shapes, V3 sections, lowerdir-O(1).
- engine-B: reminder marks + 150% hard-fail + `run_exhausted` propagation + explorer terminal flag.
- `ruff`/`mypy` clean on changed lines; `test_scenario_suite_imports.py` green.

## 7. Risks
- **`factory.py:195` is a production change** to subagent client resolution — gate behind a flag, default
  prod behavior unchanged, test narrowly.
- **One mega-scenario is hard to debug** — prefer the `full_stack_adversarial` matrix-cell approach or split.
- **macOS host:** use the **scenario** IWS path (proven on docker), not the standalone IWS RPC suite (skips here).
- **`EOS_ISOLATED_WORKSPACE_ENABLED=true`** forces an audit-path safety gate (`core/engine.py:83-115`); keep daemon-pull on.
- **#2 reachability:** `run_exhausted` is engine-B only — never assert it via `MockSquadRunner`.
- **Area #2 scope creep:** D-A tests existing behavior; building real 75/100/125% tier rules is out of scope unless explicitly requested.
