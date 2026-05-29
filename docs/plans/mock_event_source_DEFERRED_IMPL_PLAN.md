# Implementation Plan: deferred mock event-source migration items (next-agent handoff)

**Created 2026-05-29.** Exports the deferred items from
`docs/plans/mock_event_source_FANOUT_HANDOFF.md` ("Session 2026-05-29 cont.") into an
ordered, implementation-ready plan. Read the FANOUT_HANDOFF first for what is **already
green** (11 §A scenarios verified under the flag) and the recovered executor-action
catalogue. This doc is the forward plan for everything still open.

Authoritative detail map: `docs/plans/mock_event_source_MIGRATION_MAP.md` (note its
Executor-action catalogue §1174 and `test_correctness` §1182 are blank — API errors during
generation; catalogue is recovered in FANOUT_HANDOFF).

## Standing context (changed this session — assume these are DONE)

- **Executor `tool_call_limit: 75 → 100`** (`backend/src/agents/profile/main/executor.md:5`).
  Structural hard ceiling is now `ceil(1.5 × 100) = 150` (`backend/src/config/sections/engine.py`).
  **All budget math below uses 100/150.** Verified: 253 default-off unit tests pass; nothing pinned 75.
- **Executor toolset extended** (`executor.md` `allowed_tools`): `+lsp.apply_workspace_edit`,
  `+enter_isolated_workspace`, `+exit_isolated_workspace`. Regression-clean (named parity gate 198
  + agent/spawn/iws unit + docker high_concurrency + 3 proof tests).
- Mock executor loads the **production** main profiles (`agent/mock/definitions.py`) — the
  "mock & real differ only in the event source" invariant. **No mock-only toolset/limit overrides.**
- Docker verify is the bottleneck; **Item 1 removes the serial constraint** and should land first.

---

## Item 1 — N concurrent same-`instance_id` docker sandboxes (FIRST; foundational)

> **✅ DONE + verified 2026-05-30** (dynamic workflow `wnli90i6k`). The 3-edit change landed exactly as
> spec'd: `pyproject.toml` (+`pytest-xdist>=3.6.0` in `dev`), `_sweevo_sandbox_name(instance, worker=None)`
> appends `-{worker}` before `_truncate_dns_label` (suffix survives the `[-8:]` tail even for 70-char ids),
> and `fixtures.py:sweevo_image_sandbox` reads `PYTEST_XDIST_WORKER` and threads it into BOTH
> `_acquire_sweevo_session_lock(..., worker)` (lock key `f"{instance_id}-{worker}"`) and
> `_sweevo_sandbox_name(..., worker)`. Backward-compat: `setup.py`/`_provision.py`/the lock unit test pass
> no worker → unchanged. `-n 2` smoke proved two **concurrent** containers (`…-gw0` + `…-gw1` alive together
> for the full 128s run). The 11 §A scenarios pass **11/11 under `-n 3` (139s)**. Two findings for the next agent:
> - **Sync command must include the `docker` extra:** `uv sync --extra dev --extra docker`. `docker` is a
>   *separate* optional extra (`[docker]`, ~line 36), NOT in `dev` — a plain `uv sync --extra dev`
>   **uninstalls** the `docker` SDK and the sweevo fixture dies with `ModuleNotFoundError: No module named 'docker'`.
> - **Use `n=3` on this host, not the default 5.** `-n 5` tripped one host-capacity flake (residual LSP
>   warm-session lease, `active_leases==1`, on `test_ephemeral_lowerdir_disk_is_o1_under_100_calls`) from 5
>   simultaneous `pip install -e .` + LSP builds saturating the host; standalone + `-n 3` are clean. Not a logic bug.

**Goal:** run mock-scenario tests on **N concurrent docker sandboxes of the same
`instance_id`** (configurable `n`, default 5), so verification parallelizes instead of
serializing on the per-instance host flock + the single session-scoped sandbox. This
accelerates every other item.

**Why it's a 3-edit change, not a rearchitecture.** Serialization today comes from exactly two
shared resources: the deterministic Docker container **name** (`_sweevo_sandbox_name`, per
`instance_id`) and the host **flock** keyed on `instance_id` (`_acquire_sweevo_session_lock`).
Everything else is already per-process / per-container isolated (verified):
- No host bind mount; writable state is a per-container **tmpfs** (`/eos-mount-scratch`); layer-stack
  root `/tmp/eos-sandbox-runtime/layer-stack` is in-container → OCC/overlay/LayerStack isolated by the
  container FS boundary (`sandbox/provider/docker/{adapter.py:186-194,303-324, client.py:40,71-112}`,
  `sandbox/overlay/writable_dirs.py:13`, `sandbox/daemon/paths.py:15`).
- Daemon TCP port is per-container random (`adapter.py:181` publishes `("127.0.0.1", None)`), endpoint
  cached per `sandbox_id` (`sandbox/host/daemon_client.py:175,224-236`).
- DB is a per-test SQLite file (`core/stores.py:139-141`); `db_engine` session fixture → per worker process.
- Provider bootstrap is process-global first-call-wins → per worker process.
- `EOS_TIER_RUN_ID` does **not** touch this path (only live_e2e + audit sink). Reuse here is purely
  name-based (`_find_existing_sandbox_by_name`).
- Audit output keyed by `task_center_run_id` (unique per run) → no cross-worker collision.

**Parallelization mechanism: pytest-xdist.** `-n <N>` spawns `gw0..gw{N-1}` worker **processes**;
each `scope="session"` fixture runs once per worker, so `sweevo_image_sandbox` provisions one
container per worker. xdist sets `PYTEST_XDIST_WORKER=gwK` — the per-worker key to suffix.

**The 3 edits (+ deps):**
1. **Add `pytest-xdist>=3.6.0`** to `pyproject.toml` `dev` extra (~line 43; not installed today),
   then `uv sync --extra dev`.
2. **Per-worker container name** — `benchmarks/sweevo/models.py:126` `_sweevo_sandbox_name`: add
   optional `worker: str | None = None`, append **before** `_truncate_dns_label` so the suffix lands
   in the preserved `[-8:]` tail:
   ```python
   def _sweevo_sandbox_name(instance: SWEEvoInstance, worker: str | None = None) -> str:
       base = f"sweevo-{instance.instance_id}"
       if worker:
           base = f"{base}-{worker}"
       return _truncate_dns_label(base)
   ```
   The other caller (`benchmarks/sweevo/setup.py:404`) passes no worker → benchmark naming unchanged.
3. **Thread worker id into the fixture** — `environments/sweevo_image/fixtures.py` `sweevo_image_sandbox`:
   read `worker = os.environ.get("PYTEST_XDIST_WORKER")` (None without `-n`) and pass to BOTH
   `_sweevo_sandbox_name(instance, worker)` and `_acquire_sweevo_session_lock(instance_id, worker)`.
4. **Per-worker flock slug** — `fixtures.py:167,179`: key the lock path on `(instance_id, worker)`:
   ```python
   def _acquire_sweevo_session_lock(instance_id: str, worker: str | None = None) -> _SweevoSessionLock:
       key = instance_id if not worker else f"{instance_id}-{worker}"
       lock_path = _LOCK_DIR / f"sweevo-{_lock_slug(key)}.lock"
   ```
   → `gw0..gwN` take N different lock files (no cross-worker serialization); same-worker reruns still
   serialize (preserves the "don't clobber one container's binding" invariant). `_lock_slug` /
   `_release_*` unchanged. Reset isolation (`workspace` fixture) already keys off the per-worker
   `request.session` set → correct per worker, no change.

**Run command (configurable `n`):**
```bash
uv run pytest backend/src/task_center_runner/tests/mock -n "${EOS_SWEEVO_XDIST_N:-5}"
```
(The mock suite is outside `testpaths`, so pass the path explicitly.) `n` is the tunable — drop to
2–3 on a constrained host.

**Risks (verify-first / mitigate):**
1. **xdist × pytest-asyncio session-scoped async fixture** — smoke-test FIRST: run `-n 2` on the ~9
   sandbox tests and assert two distinct `-gw0/-gw1` containers via `docker ps`. (pytest-asyncio 1.3.0
   loop_scope behavior is the one wrinkle.)
2. **Worker-count instability leaks containers** (no teardown; sweevo persists). Switching `-n` orphans
   `-gwK`; reclaim with `docker ps --filter name=sweevo- -q | xargs docker rm -f`. Keep `EOS_SWEEVO_XDIST_N`
   stable in CI.
3. **Concurrent-setup thundering herd** — N simultaneous `pip install -e .` + LSP builds spike CPU/IO;
   could trip the 360s setup timeout. Default `n` to a host-appropriate value; consider snapshot pre-bake.
4. **Image pull race on cold cache** — pre-pull the sweevo image once before the parallel run.
5. **Tiny subsets amortize poorly** — every worker pays full provision; for a few tests use small `n`.
6. Docker Desktop CPU/RAM cap bounds real parallelism; raise it or lower `n`. Daytona provider out of scope.

**Verification:** the `-n 2` two-container smoke; then run the 11 already-green §A scenarios under
`-n 5` and confirm same pass + wall-clock drop.

---

## Item 2 — full_stack / capacity / full_case script-actions (PreparedToolScriptEngine bridge)

**Status:** analyzed, bridgeable, not implemented. `PreparedToolScriptEngine` (`tool_scripts.py`) runs
`script.steps` **sequentially**, so the queue-bridge is semantically faithful (unlike ephemeral's
`asyncio.gather`). Scenarios already fan out (e.g. `full_stack_adversarial` emits separate
`occ_matrix`/`overlay_matrix`/`lsp_matrix`/`layerstack_matrix` executor tasks); each matrix script is
bounded by cell count (~8–12 cells × 2–3 tools ≈ 20–40 ≪ 100). The `lsp_matrix` `workspace_edit_publish`
cell uses `lsp.apply_workspace_edit` — already unblocked.

**Three parts (all required to verify a scenario end-to-end):**
1. **ctx-threading** — `bridge_probe_for` only gets `probe_ctx`; the script factories need
   `ScenarioContext` (`ctx`). Thread `ctx` into the bridge (or handle script actions directly inside
   `_executor_script`, which already holds `ctx`). Factory builds
   `PreparedToolScriptEngine(bridge_call_tool).run(<script>(ctx), metadata=metadata, emit=_noop_emit)`
   and returns `result.artifact`. **Do NOT** emit `publish_full_stack_script` — `FULL_STACK_SCRIPT_COMPLETED`
   is removed in Phase D and the tests migrate away from it.
2. **Terminal routing in `_executor_script`** (today only `submit_execution_success`). Add, each preceded
   by its own `ask_advisor` turn for THAT terminal, then return:
   - `fail` / `fail:<reason>` → `submit_execution_blocker`
   - `request_recursive_goal:<id>` / `request_recursive_matrix:<id>` → `submit_execution_handoff`
     (recursive goal text via `scenario.recursive_handoff_goal(ctx)`)
   Also wire `MutableMockState.consume_failure(role=, attempt_id=, checkpoint=)` for failure-injection
   (verifier path already uses it; method exists in `hooks/registry.py`).
   **Shared executor path — after this change re-run high_concurrency + the 3 proof tests** (regression).
3. **Test migration** — `tests/mock/sandbox/full_stack/test_full_stack_adversarial.py` +
   `tests/mock/sandbox/capacity/test_full_system_capacity_matrix.py` (+ full_case) still assert lifecycle
   event counts the new runner doesn't emit. Map specs: capacity ~MIGRATION_MAP 1611-1765, full_stack ~1765+.

**Action catalogue (recovered, `runner.py:_run_executor`):** script-engine actions are `inspect_user_input`,
`execute_package:<id>`, `final_reconciliation`, `inspect_full_user_input`, `occ_conflict_matrix`,
`overlay_edge_matrix`, `layerstack_squash_lease`, `lsp_refresh_semantics`, `recursive_oversized_matrix`,
`full_stack_final_reconciliation`, `capacity_metrics_full_system`, `recursive_step`. Verify each script's
step count ≤100 (matrices are bounded; `execute_package:N` depends on package size — check the largest).

---

## Item 3 — Multiagent fan-out promotions + nested-goal coverage (directive #3)

Promote the deferred single-agent complex scenarios into planner→generator DAGs (the project's core
fan-out thesis), and add nested-goal (`request_recursive_goal` → `submit_execution_handoff` → child goal,
gated by `is_recursive_goal`) coverage. Reference impl: `high_concurrency_probe.py`
(`run_*_{seed,worker,reconcile}`) + `probe_bridge.py` `bridge_probe_for` index-parse. Nested-goal pattern:
`scenarios/pipeline/nested_goal.py`; gate `scenarios/_scenario_helpers/goal_origin.py:20`.

**Shared 3-tier DAG for fan-outs:** `seed/bootstrap → N work generators (write per-gen fragments) → reconcile
(read all fragments, aggregate, emit the ONE canonical artifact + run global-consistency phases)`.
Aggregate floors (counts) are **sums** → partitioning preserves them; single-file artifacts + global ratios
+ the auto-squash depth need a reconcile owner (or a chained generator). Add fail-fast in reconcile (like
`_reconcile_summary`) so a short slice fails before the slow live contract read.

### 3a. auto_squash_commit_resume (small → chained multiagent)
Under limit=100 the ~114-call probe now *fits* a single agent (<150) — but per directive #3 promote it:
- DAG: `seed → cpb_squash_a (≈90 sequential depth-building edits on one private file) → cpb_squash_b
  (deps=[a], +≈20 edits continuing the SAME file's chain → manifest depth crosses 100, ≥10 squash events)
  → reconcile/verify (edit/read/conflict/summary; summary `write_count == AUTO_SQUASH_MAX_DEPTH+4 == 104`
  aggregated)`.
- **The depth assertion (`depth_before > 100`) needs >100 sequential OCC commits on ONE uninterrupted
  layer chain** — preserve by **chaining** two budget-bounded generators (b deps a, same file, same shared
  sandbox), NOT parallelizing them. This is the canonical recipe reused by complex_project_build (below).
- Reshape `scenarios/sandbox/auto_squash_commit_resume.py` (1 task → seed + a→b + reconcile); rewrite
  `runner.py:_run_auto_squash_commit_resume_probe` (1083) as generator probes in `probes.py` + bridge branches.

### 3b. ephemeral same_path_conflict (→ racing generators)
The probe `asyncio.gather`s 4 same-path writes and asserts ≥1 OCC conflict; the queue-bridge serializes →
no race → fails. **Promote to real fan-out:** planner emits `seed → N concurrent writer generators
(deps=[seed], each writes the same path once with `allow_error`) → reconcile (asserts ≥1 typed conflict +
the retry/final-content contract)`. Exactly `high_concurrency`'s `CONFLICT_WORKER_COUNT` racing on
`shared/conflict.txt`. Budget trivial (~3 calls/gen). Re-add the `bridge_probe_for` branch (reverted to
`NotImplementedError` this session) once the scenario fans out.

### 3c. complex_project_build ×6 (the hard single→multi promotion)
**Decisive constraint:** all 6 share ONE assertion model in `tests/mock/_project_build_contracts.py`,
aggregating over **all** `report.tool_calls` (whole attempt) + reading ONE metrics file per workspace
(`/ephemeral-os/.metrics/{perf,summary}.json`, `pytest.xml`). Floors (`tool_calls>=2000` full / 250 smoke;
`edit:write>=4.0`; per-LSP-tool floors; api floors; junit floor) are **sums** → fan-out preserves them.
Single-file aggregates (perf/summary, the global `logical_edit_index % 3 == 2` routing ratio, auto-squash
`depth_before>100`) need the **reconcile** generator. Uniform DAG: `seed/bootstrap → N work gens (disjoint
file slices, write fragments) → reconcile (sum fragments, run global phases: pytest, intentional conflict
[the one `SANDBOX_CONFLICT_DETECTED`], tri-source, emit canonical perf/summary)`.

**Budget (≤90/gen target, 100 cap, 150 ceiling):** full floor 2000 → keep total as a sum: e.g.
`WORK_GEN_COUNT≈24` × ~82 calls + seed ~20 + reconcile ~40 ≈ 2028. Smoke 250 → 3 work gens. Make
`WORK_GEN_COUNT` a per-variant module constant (mirror `WORKER_COUNT`).

**Sequential deps re-expressed:**
- `_phase0_bootstrap` cwd-rebind (probe lines 245-307) moves to **seed**. **Load-bearing risk:**
  `_reset_workspace_base` passes `reset=True` (line 395) — a work gen must NOT reset (would wipe the
  seed skeleton). Add a `reset: bool` so seed resets, work/reconcile rebind-without-reset (or inherit the
  process-global binding and only set `metadata.repo_root/cwd/exec_cwd`). **Biggest correctness risk.**
- Open-ended `_tool_call_floor` while-loops become **fixed-size slices** sized at build time (deterministic,
  ≤90, no 150-blowout); reconcile verifies `Σ` meets the floor + fails fast.
- **auto-squash `depth_before>100`**: the ONE thing that can't parallelize. Dedicate a **chained pair**
  (`cpb_squash_a` 90 edits → `cpb_squash_b` deps=[a] +20 on the same private file in the shared sandbox →
  depth crosses 100 across the dep edge before squash). Smoke `require_squash_events=False` → drop the
  squash chain in smoke.
- `perf.tool_use.total_calls ≈ len(probe_tool_calls)` within ±5 → fragment counters must count exactly
  what each loop dispatched (the bridge counts each `call_tool` as one real loop turn → 1:1).

**Per-variant extras:**
- `complex_project_build_shell_edit_lsp`: the global `logical_edit_index` routing ratio (1/3, ±0.03) is a
  pure function of the **global index** — planner assigns each gen a contiguous index range (via per-task
  `context_message`); routing stays exact regardless of which gen runs. Reconcile sums `edit_routing`.
  Keep the existing `..._shared_bootstrap` rendezvous + `_SHARED_ATTEMPT_BOOTSTRAPS` intact — the
  `test_project_build_shell_edit_lsp_three_parallel_agents.py` test depends on it (asserts
  `task_center_status=="failed"` + 4 `aborted_version` conflicts) and is **already a fan-out** (do not break).
- `complex_project_build_grep_glob`: grep/glob/edit floors are sums; reconcile owns `_phase_f_search_sweep`
  + writes `perf.scenario` = the full scenario name (hardcode, not per-gen).

**Nested-goal insertion points (add `RECURSIVE_GOAL_REQUESTED/COMPLETED` coverage):** the refactor pass
(`_phase_d_refactor`), the diagnostic break-detect-repair cycle (shell_edit_lsp), or the search audit
(grep_glob) are each self-contained → one work gen emits `request_recursive_goal:<k>`; the child goal's
planner (gated by `is_recursive_goal`) fans out its own seed/work/reconcile and closes via `GoalClosureReport`;
the parent reconcile waits on it. Child edits land in the same shared `/ephemeral-os` → roll into the
aggregate contract counters untouched. Add the new events to `expected_event_sequence` only on the variant(s)
that wire the nested goal.

**Cross-cutting:** new probe entries mirror `run_high_concurrency_{seed,worker,reconcile}_probe` signatures
(reuse the existing phase helpers — slice, don't rewrite); add `bridge_probe_for` branches parsing
`cpb_<variant>_{seed,work:<i>,reconcile}` (+ `cpb_squash_a/b`); rewrite each scenario's tasks/task_specs from
1 task to the DAG (use the `_three_agent_plan` idiom); `executor_actions(ctx)` keys off `ctx.context_message`.
Tests change from single-task `status=="done"` to multi-task launch assertions; the **contract asserts are
unchanged** (they aggregate).

Anchors: probes `complex_project_build{,_shell_edit_lsp,_grep_glob}_probe.py`; contracts
`tests/mock/_project_build_contracts.py`; scenarios `scenarios/sandbox/complex_project_build*.py`; runner
dispatch `runner.py:404` (handoff), `:616-660` (cpb), `:888-909` (`_scenario_context`).

---

## Item 4 — §C background-probe rewrite (real-agent background model)

**Reframe:** the queue-bridge rejects `background_task_id` because the loop's background path is
fire-and-forget (`launch_background_tool` returns a synthetic started-block immediately) while the old
probes blocked on the real result + per-call `wait_for`-cancel. The 12 `background_shell_*` modes + the
ephemeral `cancellation` probe are **re-expressed as generator probes** (NOT bridged) using the real
control tools. Drop the queue-bridge / `runner._call_tool` direct-dispatch / `BackgroundTaskSupervisor`
machinery for these.

**Gating facts (verified):**
- **No profile change needed.** `cancel_background_task` / `check_background_task_result` /
  `wait_background_tasks` are **auto-synthesized** by `factory._finalize_tool_registry_and_prompt:137-146`
  whenever a registered tool has `background != "forbidden"` (shell is `background="optional"`) and
  `agent_type != SUBAGENT`. They're correctly absent from `allowed_tools`.
- **OCC abort surface survives the control channel.** `shell` puts `status/changed_paths/conflict_reason/
  mutation_source/exit_code/stdout` into `ToolResult.output` (JSON); `check_background_task_result` returns
  it verbatim. Only `timings`/`error_kind` are metadata-only and dropped — re-source any p95 from
  **foreground** turns.
- Background path preserves the workspace binding (`tool_call.py:91-101` merges into full context metadata)
  → OCC publish identical to foreground.
- IWS gate `RequireNoInflightBackgroundTasks` is a real in-loop prehook on enter/exit → `exit_iws_drain`
  collapses to a single real agent (no fabricated identity / probe-owned supervisor).
- `cancel_background_task` has no `"all"`; the loop's terminal `finally: cancel_all()` reaps everything →
  fan-out launches need no explicit per-task cancel.
- Recover a launched task's `sandbox_invocation_id` from the shared supervisor
  `probe_ctx.metadata.background_task_manager.iter_all()` (the caller can no longer supply it).

**Shared generator pattern:** `launch = yield ToolCall("shell", {cmd, timeout, "background": True})` → regex
`task_id="bg_N"` from the started-block; `observe = yield ToolCall("check_background_task_result", {task_id})`
→ parse `result` JSON; `block-all = wait_background_tasks`; `cancel = cancel_background_task`; out-of-band
`sandbox_api.inflight_count/heartbeat` + file reads. **Launch sequentially** (each returns instantly; long
sleeps keep prior tasks running — no mode needs simultaneous dispatch). Register the 13 in `probes.py`
`PROBE_BUILDERS` keyed by **action string** (collapse the action→mode indirection) → `_executor_script`
routes them via `PROBE_BUILDERS.get(action)`, no `bridge_probe_for` fallback.

**Per-mode rewrite table (assertion rewrites, all ≤100):** golden (~11), stop (~13, explicit per-task cancel),
interleave (~9, p95 from fg turns), **exhaustion (~86 — budget-critical: 80 launches, NO explicit cancel, rely
on terminal reap)**, partial_write_cancel (~7), maintenance (~6), late_cancel_race (~6, the late cancel
*failing* on a completed task is the assertion), mixed_fg_bg_same_path_conflict (~7), heartbeat_loss (~10,
recover iids from supervisor), exit_iws_drain (~12, real enter/exit gate), engine_restart_no_lease_leak (~9,
cancel+drain-to-0 simulates abandonment), **many_small_writes (~52 — cap the BG count env ≤~30 or it exceeds
100)**, ephemeral cancellation (~8). Full per-mode turn sequences + the 5 BG/cancel hazards (partial-write,
late-cancel, heartbeat-loss, exit-iws-drain, engine-restart) are in the research output — reproduce them when
implementing.

Anchors: rewrite `background_shell_probe.py` (delete `call_tool`/BG plumbing/supervisor) +
`ephemeral_workspace_probe.py:452-555`; control tools `tools/background/*`; auto-synthesis
`engine/agent/factory.py:137-146`; IWS gate `tools/_hooks/require_no_inflight_background_tasks.py`;
OCC-in-output `tools/sandbox/shell/shell.py:123-141`; loop teardown `engine/query/loop.py:311`.

---

## Item 5 — test migrations + Phase D deletions/flip + Phase E sweep

1. **Migrate the ~11 event-asserting test files** to `graph_summary` (per MIGRATION_MAP "Assertion →
   graph_summary rewrites"). **Hazard: `count_role_tasks` signature conflict.** The LIVE
   `_focused_scenario_contracts.count_role_tasks(report, role, *, status)` (report-level, `agent_name`
   match; used by high_concurrency) is INCOMPATIBLE with the per-attempt
   `count_role_tasks(attempt, *, role, agent_name, status)` the map's specs assume. Reconcile as single
   writer (keep report-level OR add a distinct per-attempt helper name) and tell migration authors the live
   signature. Add `attempt_outcome`, `recursive_goals` helpers. `test_correctness.py`'s "no ask_advisor in
   transcript" assertion is INVERTED (real `ask_advisor` turns now appear). `test_correctness` map section
   is blank (API error) — derive from source.
2. **Phase D deletions (STRICTLY after all scenarios green under the flag; EventType-enum removal LAST):**
   delete `MockSquadRunner` internals (`_run_*`, `_call_tool`, `_approve_terminal`, `_run_*_probe`,
   `_record_tool_check`, the `_*_EVENT_BY_TOOL` maps, lifecycle `_publish`); delete `_advisor_approval.py`
   (relocate `build_advisor_approval_messages` into test fixtures first); strip `expected_event_sequence`
   from `scenarios/base.py` + every scenario; remove `RunReport.seen_event_types` +
   `_assert_ordered_subsequence`/`_assert_event_counts` + `FocusedScenarioCase.min_event_counts/absent_events`;
   drop `hooks/builtins.py` VERIFIER emits; re-point `test_sweevo_audit_recorder.py:393`; remove the 16
   lifecycle members from `audit/events.py`. Then **flip `scenarios/builder.py` flag default-on.**
3. **Phase E sweep:** run all 144 `tests/mock` under the flag (now parallel via Item 1), classify + fix.
   The 80 `isolated_workspace` tests are runner-agnostic — confirm unaffected, don't migrate.

---

## Suggested order
1. **Item 1** (parallel sandboxes) — unblocks fast iteration on everything else.
2. **Item 2** (script-actions bridge + terminal routing) — also lands `fail`/`handoff` routing that Item 3 reuses.
3. **Item 3** (fan-out promotions: 3a auto_squash → 3b same_path_conflict → 3c complex_project_build ×6) +
   nested goals. Plan 3c with a fresh advisor pass (it's the hardest; the cwd-reset hazard is load-bearing).
4. **Item 4** (background rewrites).
5. **Item 5** (test migrations → Phase D flip → Phase E sweep).

## Gotchas (carry forward)
- Per-generator budget ≤100 (ceiling 150). Confirm before bridging; fan out more generators if over.
- Advisor gate per terminal — every gated terminal needs its `ask_advisor` turn.
- Parallel agents edit this repo (HEAD advanced to `ff476401e` mid-session; `context_engine/*` dirty →
  `tests/mock/contracts/test_runner_imports.py` has 4 pre-existing reds from vocab drift, NOT migration
  regressions). Stage with explicit paths; never `git add <dir>`.
- Auto-memory: `mock_event_source_real_loop_toolset_enforcement`, `mock_event_source_queue_bridge`,
  `mock_event_source_heavy_probe_fanout_decision`, `mock_runner_inspects_planner_context_vocab`,
  `mock_scenario_bypasses_engine_loop`.
