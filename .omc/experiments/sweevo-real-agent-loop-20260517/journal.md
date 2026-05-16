# SWE-EVO Real-Agent Loop — 2026-05-17

Starting state: branch `codex/fix-dot-path-normalization-tests` at `2cba70f5f` (`Skip test_sweevo_mock_agent_execution without EPHEMERALOS_DATABASE_URL`). Open worktree edits at bootstrap are `backend/src/task_center_runner/tests/sweevo/test_partial_parent_planner_full_only.py` and `backend/tests/unit_test/test_plugins/test_lsp_catalog.py`; neither is a primary editing surface for this loop, so they are left untouched. The CSV prompt bootstrap for `dask__dask_2023.3.2_2023.4.0` resolved to length `93150`.

## Iter 1 — 2026-05-17 00:45

**Hypothesis:** baseline — no edits, observe what breaks first.
**Primary surface touched:** none — infra-only
**Infra patches (if any):**
- `backend/src/benchmarks/sweevo/__main__.py:393` skip snapshot preflight for images without explicit non-latest versions so CSV runner can use the existing direct-image sandbox path.
- `backend/tests/unit_test/test_benchmarks/test_sweevo_csv_runner_cli.py:165` add coverage that bare images do not call snapshot verification.
**Change-set:**
- `backend/src/benchmarks/sweevo/__main__.py`
- `backend/tests/unit_test/test_benchmarks/test_sweevo_csv_runner_cli.py`

**Run outcome:**
- resolved: false
- f2p: n/a
- p2p_broken: n/a
- duration_s: 2
- status: failed
- terminal failure mode: bootstrap failed because Daytona snapshot `sweevo-dask__dask-10042` is not registered.

**Checklist scores (§2):**
1. planner-terminal: n/a (bootstrap stopped before TaskCenter)
2. planner-explore: n/a (bootstrap stopped before TaskCenter)
3. planner-dag: n/a (bootstrap stopped before TaskCenter)
4. planner-task-specs: n/a (bootstrap stopped before TaskCenter)
5. executor-terminal: n/a (bootstrap stopped before TaskCenter)
6. verifier-terminal: n/a (bootstrap stopped before TaskCenter)
7. evaluator-terminal: n/a (bootstrap stopped before TaskCenter)
8. nesting+parallelism: n/a (bootstrap stopped before TaskCenter)
9. context-engine: n/a (bootstrap stopped before TaskCenter)
10. perf: n/a (bootstrap stopped before sandbox creation)

**Top finding (the one thing to fix next):** The dask dataset image is a bare Docker Hub repo with only `latest`; Daytona rejects bare refs and explicit `:latest` for snapshot creation, and digest snapshot registration is forbidden in this account. The CSV runner was the only benchmark path that forced snapshot preflight before reaching the existing direct-image fallback.
**Next hypothesis:** after allowing CSV runner to skip snapshot preflight for bare images, the same baseline command will reach sandbox provisioning and produce a TaskCenter audit tree.
**Audit refs:** `.omc/experiments/sweevo-real-agent-loop-20260517/iter-1/console.log`

**Guard:** `.venv/bin/pytest backend/tests/unit_test/test_benchmarks/test_sweevo_csv_runner_cli.py -q` -> `10 passed in 0.44s`.

## Iter 2 — 2026-05-17 00:50

**Hypothesis:** CSV runner bare-image fallback will pass snapshot preflight and reach sandbox provisioning/audit creation.
**Primary surface touched:** none — infra validation
**Infra patches (if any):**
- `backend/src/task_center_runner/core/bootstrap.py:18` fix real-agent bootstrap profile root after the file moved under `task_center_runner/core`.
- `backend/tests/unit_test/test_task_center_runner/test_real_agent_bootstrap.py:4` add a path guard for production agent profile loading.
**Change-set:**
- `backend/src/task_center_runner/core/bootstrap.py`
- `backend/tests/unit_test/test_task_center_runner/test_real_agent_bootstrap.py`

**Run outcome:**
- resolved: false
- f2p: n/a
- p2p_broken: n/a
- duration_s: 20
- status: crashed
- terminal failure mode: real-agent bootstrap asserted missing profile root at `backend/src/task_center_runner/agents/profile`.

**Checklist scores (§2):**
1. planner-terminal: n/a (bootstrap stopped before TaskCenter agents)
2. planner-explore: n/a (bootstrap stopped before TaskCenter agents)
3. planner-dag: n/a (bootstrap stopped before TaskCenter agents)
4. planner-task-specs: n/a (bootstrap stopped before TaskCenter agents)
5. executor-terminal: n/a (bootstrap stopped before TaskCenter agents)
6. verifier-terminal: n/a (bootstrap stopped before TaskCenter agents)
7. evaluator-terminal: n/a (bootstrap stopped before TaskCenter agents)
8. nesting+parallelism: n/a (bootstrap stopped before TaskCenter agents)
9. context-engine: n/a (bootstrap stopped before context rendering)
10. perf: n/a (only sandbox creation ran)

**Top finding (the one thing to fix next):** Sandbox creation now works through the bare-image path, but `bootstrap_real_agent_runtime` used a stale `_PROFILE_ROOT` derived from the old file location and failed before loading the production planner/executor/verifier/evaluator profiles.
**Next hypothesis:** after fixing `_PROFILE_ROOT`, the same command will enter TaskCenter agent execution and produce per-agent audit messages.
**Audit refs:** `.omc/experiments/sweevo-real-agent-loop-20260517/iter-2/console.log`

**Guard:** `.venv/bin/pytest backend/tests/unit_test/test_task_center_runner/test_real_agent_bootstrap.py -q` -> `1 passed in 0.35s`; `.venv/bin/pytest backend/tests/unit_test/test_benchmarks/test_sweevo_csv_runner_cli.py -q` -> `10 passed in 0.39s`.

## Iter 3 — 2026-05-17 00:51

**Hypothesis:** fixed profile bootstrap will let the CSV runner enter TaskCenter agent execution and produce per-agent audit messages.
**Primary surface touched:** none — infra validation
**Infra patches (if any):**
- `backend/src/benchmarks/sweevo/__main__.py:428` use host cwd for `RuntimeConfig.cwd` instead of sandbox `/testbed`.
- `backend/src/task_center_runner/core/real_agent_run.py:72` apply the same host-cwd split to the direct real-agent shim.
- `backend/src/task_center_runner/benchmarks/sweevo/csv_runner.py:86` forward sandbox `/testbed` through `ExecutionMetadata.repo_root` / `exec_cwd` for non-entry real agents.
**Change-set:**
- `backend/src/benchmarks/sweevo/__main__.py`
- `backend/src/task_center_runner/core/real_agent_run.py`
- `backend/src/task_center_runner/benchmarks/sweevo/csv_runner.py`
- `backend/tests/unit_test/test_benchmarks/test_sweevo_csv_runner_cli.py`
- `backend/tests/unit_test/test_task_center_runner/test_sweevo_csv_runner_dispatch.py`
- `backend/tests/unit_test/test_task_center_runner/test_real_agent_run.py`

**Run outcome:**
- resolved: false
- f2p: 0/0
- p2p_broken: 0
- duration_s: 51
- status: failed
- terminal failure mode: planner agents crashed before model/tool work because host runtime cwd was set to sandbox path `/testbed`.

**Checklist scores (§2):**
1. planner-terminal: unobservable (planner crashed before response)
2. planner-explore: unobservable (planner crashed before tool use)
3. planner-dag: unobservable (no submitted plan)
4. planner-task-specs: unobservable (no submitted plan)
5. executor-terminal: n/a (no generator tasks launched)
6. verifier-terminal: n/a (no verifier tasks launched)
7. evaluator-terminal: n/a (no evaluator task launched)
8. nesting+parallelism: n/a (no nested execution reached)
9. context-engine: unobservable (planner prompt rendered, but agent crashed before consuming it)
10. perf: pass (only entry handoff observed; `submit_execution_handoff` 84.755 ms, no sandbox hot-path events)

**Top finding (the one thing to fix next):** The real-agent runtime conflated host cwd and sandbox repo dir. Host prompt assembly tried to create `/testbed/.ephemeralos` locally; sandbox tools still need `/testbed`, but only through execution metadata.
**Next hypothesis:** after splitting host `RuntimeConfig.cwd` from sandbox `repo_root` / `exec_cwd`, planners will reach model/tool execution instead of crashing at spawn.
**Audit refs:** `.omc/experiments/sweevo-real-agent-loop-20260517/iter-3/console.log`; `.sweevo_runs/benchmark/sweevo_csv/dask__dask_2023.3.2_2023.4.0/20260516T165257Z_37c040559e9c/run.json`; `.sweevo_runs/benchmark/sweevo_csv/dask__dask_2023.3.2_2023.4.0/20260516T165257Z_37c040559e9c/sweevo_result.json`

**Guard:** `.venv/bin/pytest backend/tests/unit_test/test_benchmarks/test_sweevo_csv_runner_cli.py -q` -> `10 passed in 0.41s`; `.venv/bin/pytest backend/tests/unit_test/test_task_center_runner/test_sweevo_csv_runner_dispatch.py -q` -> `5 passed in 0.38s`; `.venv/bin/pytest backend/tests/unit_test/test_task_center_runner/test_real_agent_run.py -q` -> `1 passed in 0.38s`.

## Iter 4 — 2026-05-17 00:56

**Hypothesis:** host/sandbox cwd split will let planner agents reach model/tool execution instead of crashing at spawn.
**Primary surface touched:** prompts
**Infra patches (if any):** none
**Change-set:**
- `backend/src/agents/profile/main/planner.md`
- `backend/src/agents/profile/main/planner_full_only.md`
- `backend/tests/unit_test/test_agents/test_planner_full_only_md.py`

**Run outcome:**
- resolved: false
- f2p: n/a
- p2p_broken: n/a
- duration_s: 2091
- status: crashed
- terminal failure mode: operator interrupted after nested planner entered a runaway invalid-agent-name loop; no `sweevo_result.json` was produced.

**Checklist scores (§2):**
1. planner-terminal: fail (root planner first submitted `code_executor`/`default`; nested planner kept trying invalid names such as `python_executor`, `transform`, `file_editor`, `apply`)
2. planner-explore: fail (nested planner used repeated explorer/advisor calls for small direct file questions and then searched the target repo for harness agent names)
3. planner-dag: fail (root DAG was wide, but one over-broad categorize task triggered a nested monolithic retry; nested planner never submitted a valid plan)
4. planner-task-specs: fail (categorize spec was over-prescriptive and led the executor into a broken partial implementation plus handoff at tool-limit)
5. executor-terminal: fail (three executors submitted success with evidence; categorize handed off only after exhausting the tool budget and leaving partial edits)
6. verifier-terminal: n/a (no verifier launched)
7. evaluator-terminal: n/a (no evaluator launched)
8. nesting+parallelism: fail (top-level generator siblings ran concurrently; nested planning re-emitted a single monolithic fix attempt and then looped on invalid agent names)
9. context-engine: fail (planner prompt said "registered executor or verifier agent" but did not name `executor` / `verifier`; agents looked in Dask for harness names)
10. perf: fail (`api.shell.overlay_s` hit 14.375s and OCC committed pycache/pytest-cache noise, including one 96-path pycache changeset)

**Top finding (the one thing to fix next):** The planner profiles do not give concrete valid `agent_name` values. Both normal and full-only planners treated agent names as discoverable project metadata, searched `/testbed`, asked the advisor, and burned 13 rejected `submit_plan_closes_goal` calls on invalid names.
**Next hypothesis:** if both planner profiles explicitly name `executor` for generator work and `verifier` for verifier work, the planner will stop guessing repo-local agent names and nested handoffs will reach executable tasks faster.
**Audit refs:** `.omc/experiments/sweevo-real-agent-loop-20260517/iter-4/console.log`; `.sweevo_runs/benchmark/sweevo_csv/dask__dask_2023.3.2_2023.4.0/20260516T165722Z_3398e9f9cf69/goal_01_bb2fb154-ad23-4155-a9f5-1239da47dc2f/iteration_01_9c7e845b-b555-4d58-9f05-8cf9be37746e/attempt_01_17f63ac1-b9de-4093-be74-d7dbe7f75f02/01_planner_17f63ac1-b9de-4093-be74-d7dbe7f75f02:planner/message.jsonl`; `.sweevo_runs/benchmark/sweevo_csv/dask__dask_2023.3.2_2023.4.0/20260516T165722Z_3398e9f9cf69/goal_02_094c1599-7a4e-4cb1-803f-f60c16b06e52/iteration_01_08cbf9c2-06bd-46e8-afef-e8fed65fe8be/attempt_01_05dfce16-81f0-4cc9-9e4d-e547d92b79e7/01_planner_05dfce16-81f0-4cc9-9e4d-e547d92b79e7:planner/message.jsonl`; `.sweevo_runs/benchmark/sweevo_csv/dask__dask_2023.3.2_2023.4.0/20260516T165722Z_3398e9f9cf69/metrics.json`; `.sweevo_runs/benchmark/sweevo_csv/dask__dask_2023.3.2_2023.4.0/20260516T165722Z_3398e9f9cf69/sandbox_events.jsonl`

**Guard:** `.venv/bin/pytest backend/tests/unit_test/test_agents/test_planner_full_only_md.py -q` -> `8 passed in 0.22s`.
