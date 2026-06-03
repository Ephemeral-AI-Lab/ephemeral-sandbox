# Migration Plan: `task_center_runner` -> `test_runner`

Status: draft
Date: 2026-06-01
Target package: `backend/src/task_center_runner` -> `backend/src/test_runner`

Builds on:

- `docs/plans/task_center_to_workflow_REFACTOR_PLAN.md`
- `docs/plans/sandbox-rust-external-migration-PLAN.md`
- `docs/plans/sandbox-plugin-service-adversarial-plan.md`
- `docs/architecture/index.html`
- `docs/architecture/task_center_runner/`

## 0. Target state

`test_runner` is the test and benchmark harness for the Task-first agentic
framework. It is not Task, Workflow, runtime entry, or sandbox infra.

The canonical request flow under test is:

```text
user request
  -> request row + root Task(role=root, workflow_id=NULL)
  -> root agent runs directly from task.instruction
  -> root agent may call non-terminal delegate_workflow(goal)
  -> delegated workflow agents may plan/run/reduce work
  -> root agent submits submit_root_outcome
  -> request completes
```

Workflow is no longer the first-class agent framework. Agent/Task is first
class; Workflow is a persisted decomposition tool that root and executor agents
can call through `delegate_workflow(goal)`.

Sandbox infra target state is Rust-only for in-sandbox execution. The Python
`backend/src/sandbox` tree keeps only host/API/provider-side code required to
upload, launch, connect to, and expose the Rust sandbox runtime. After the Rust
parity gates pass and safe removal is confirmed, all non-host/API/provider
Python sandbox infra is removed.

## 1. Scope

### In scope

- Rename the package import surface from `task_center_runner.*` to
  `test_runner.*`.
- Rename the on-disk package directory, test paths, architecture module, docs,
  CLI commands, run labels, report schemas, and benchmark entrypoints that carry
  the runner name.
- Rewrite runner scenarios and mock-agent probes around the Task-first request
  flow, `submit_root_outcome`, and non-terminal `delegate_workflow`.
- Convert runner sandbox coverage to the Rust sandbox contract: non-login Bash
  command/session semantics, Rust PPC plugin service behavior, Rust isolated
  workspace lifecycle, and Rust-only daemon/runner paths.
- Use the existing root sandbox benchmark scripts as the reference for fast
  sandbox setup, artifact upload, runtime startup, and live-gate evidence:
  `backend/scripts/bench_sandbox_e2e.py`,
  `backend/scripts/bench_rust_daemon_phase2.py`,
  `backend/scripts/bench_rust_daemon_phase3.py`,
  `backend/scripts/bench_rust_daemon_phase3t_pty.py`,
  `backend/scripts/bench_rust_daemon_phase3t_av7_parity.py`,
  `backend/scripts/bench_rust_daemon_phase3t_mixed_non_plugin.py`,
  `backend/scripts/bench_rust_daemon_phase3t_section7_non_plugin.py`,
  `backend/scripts/bench_rust_daemon_plugin.py`,
  `backend/scripts/bench_rust_daemon_isolated_inspection.py`, and
  `backend/scripts/bench_plugin_refresh_strategies.py`.
- Set live concurrent sandbox runners to `3` and verify that three parallel
  live E2E lanes run without exceeding sandbox quota or leaking leases.
- Define the final pass condition for deleting Python sandbox infra.

### Out of scope

- Reopening the Task-first architecture already documented in
  `task_center_to_workflow_REFACTOR_PLAN.md`.
- Rewriting plugin implementations under `backend/src/plugins/catalog/*`.
  Plugin implementations remain payloads; the sandbox plugin dispatch/importlib
  layer moves to Rust PPC.
- Replacing the Python host API/provider boundary. Host-side launch/connect,
  `api.v1.*`, Docker provider upload, and config/bootstrap code remain Python
  unless a later plan explicitly moves them.

## 2. Evidence from the current checkout

- `backend/src/task_center_runner/core/engine.py` already calls
  `workflow.start_request(...)`, binds a `request_id`, lists tasks by request,
  and records request status. This is the right seam for the renamed harness.
- `backend/src/task_center_runner/core/stores.py` still exposes
  `create_per_test_task_center_stores()` and docstrings still describe
  TaskCenter stores. This is a required rename/semantic cleanup.
- `backend/src/task_center_runner/tests/mock/task_center/` still names the
  old correctness bucket. It should become a root-request-first
  `tests/mock/request/` bucket; only scenarios whose subject is the
  tool-launched decomposition machinery belong under
  `tests/mock/delegated_workflow/`.
- Scenario names and comments still include old root-workflow language:
  `pipeline.initial_workflow`, `recursive_handoff_goal`,
  `request_recursive_workflow`, and background command scenarios that still
  assume deleted generic background-task tools.
- `docs/architecture/task_center_runner/` is already titled "Workflow Runner
  (Testing)" but the path, evidence metadata, CLI examples, and prose still
  point at `task_center_runner`.
- `ephemeralos.yaml` already sets `runner.sandbox_quota: 3`; `RunnerConfig`
  currently defaults `sandbox_quota` to `5`. The migration should make the
  three-runner live E2E contract explicit and tested.
- The current fast-setup references are
  `backend/scripts/bench_sandbox_e2e.py`,
  `backend/scripts/bench_rust_daemon_phase2.py`,
  `backend/scripts/bench_rust_daemon_phase3.py`,
  `backend/scripts/bench_rust_daemon_phase3t_pty.py`,
  `backend/scripts/bench_rust_daemon_phase3t_av7_parity.py`,
  `backend/scripts/bench_rust_daemon_phase3t_mixed_non_plugin.py`,
  `backend/scripts/bench_rust_daemon_phase3t_section7_non_plugin.py`,
  `backend/scripts/bench_rust_daemon_plugin.py`,
  `backend/scripts/bench_rust_daemon_isolated_inspection.py`, and
  `backend/scripts/bench_plugin_refresh_strategies.py`; do not invent a
  parallel provisioning path for the runner before checking those helpers.
- `backend/src/task_center_runner/agent/mock/tool_scripts.py` imports
  `sandbox.occ.service.AUTO_SQUASH_MAX_DEPTH` directly. Any runner import from
  Python sandbox internals is a blocker before Python sandbox removal.

## 3. Migration phases

### Phase A - Freeze the rename boundary

Goal: make the rename mechanical and auditable before changing behavior.

1. Create `backend/src/test_runner/` by moving `backend/src/task_center_runner/`.
2. Replace imports from `task_center_runner.*` to `test_runner.*` across source,
   tests, docs, and scripts.
3. Rename these docs and paths:
   - `docs/architecture/task_center_runner/` -> `docs/architecture/test_runner/`
   - architecture module label: `Workflow Runner (Testing)` -> `Test Runner`
   - `backend/src/task_center_runner/read.md` -> `backend/src/test_runner/read.md`
4. Rename user-facing commands:
   - `python -m task_center_runner.benchmarks.sweevo`
   - becomes `python -m test_runner.benchmarks.sweevo`
5. Rename defaults:
   - `runner.run_label: task_center_runner` -> `test_runner`
   - isolated SQLite bundle directory `task_center_runner/` -> `test_runner/`
   - report schema prefix `task_center_runner.*` -> `test_runner.*`
6. Keep a temporary import shim only if an external caller still needs one:
   `backend/src/task_center_runner/__init__.py` may raise a clear deprecation
   error or re-export `test_runner` for one short transition. The preferred final
   state has no `task_center_runner` package.

Exit gate:

```bash
rg -n "task_center_runner|TaskCenter|task_center_runner\\.performance_report" \
  backend/src backend/tests docs scripts
```

Allowed hits are limited to historical plan references and the short-lived
compatibility shim if retained.

### Phase B - Rename TaskCenter semantics inside the harness

Goal: remove old terminology without changing the runner's role.

1. Rename core objects:
   - `TaskStoreBundle` docstrings: "TaskCenter stores" -> "Task/request stores"
   - `create_per_test_task_center_stores()` ->
     `create_per_test_task_stores()`
   - test helpers and mocks that still mention `task_center_run_id` ->
     `request_id`
2. Reclassify test buckets around the new first-class unit:
   - `tests/mock/task_center/` -> `tests/mock/request/` for the canonical
     user request -> root Task -> root agent -> `submit_root_outcome` flow.
   - `tests/mock/delegated_workflow/` only for cases whose direct subject is
     `delegate_workflow` and the resulting Workflow -> Iteration -> Attempt
     machinery.
   - keep sandbox tests under `tests/mock/sandbox/`; they may drive full
     request flows, but their ownership remains sandbox behavior.
   - do not create a broad `tests/mock/workflow/` successor. Workflow is no
     longer the first-class framework boundary.
3. Rename scenario vocabulary:
   - `pipeline.initial_workflow` -> `pipeline.root_delegates_workflow`
   - `recursive_handoff_goal` -> `delegated_workflow_goal`
   - `request_recursive_workflow` -> `delegate_workflow`
   - "root workflow" / "child workflow" -> "root Task" /
     "delegated Workflow"
4. Update `ScenarioContext` and scenario helpers so `ctx.workflow is None` means
   "root Task context", not "entry-origin workflow".
5. Keep graph summaries workflow-specific. Root request summaries belong in a
   separate root/request section of `RunReport`.
6. Add persistence and schema-contract tests that mirror
   `task_center_to_workflow_REFACTOR_PLAN.md`:
   - request row owns `root_task_id`, status, and `finished_at`
   - root Task has `workflow_id=None`, no iteration/attempt ids, and one
     `agent_runs` row
   - delegated planner/generator/reducer Tasks carry `workflow_id`,
     `iteration_id`, and `attempt_id`
   - `context_message` is gone from task-facing assertions; `instruction` is
     the runner-visible field
   - no scenario or report code parses task ids to recover role/attempt
   - no serialized event/report key still depends on `task_center_run_id`

Exit gate:

```bash
uv run pytest -q backend/src/test_runner/tests/mock/contracts
uv run pytest -q backend/src/test_runner/tests/mock/request
uv run pytest -q backend/src/test_runner/tests/mock/delegated_workflow
uv run pytest -q backend/tests/unit_test/test_task_center_runner
```

The last path should be renamed as part of the phase; it is listed here as the
current source anchor to migrate.

### Phase C - Adopt the root-agent-first runtime contract

Goal: make the runner test the actual production request lifecycle.

1. Add explicit root-agent mock scripting support in `ScenarioLoopRunner`.
   Prompt inspection for root must assert:
   - no ContextEngine packet
   - initial user content is the request prompt
   - root terminal is `submit_root_outcome`
   - `delegate_workflow`, `check_workflow_status`, and `cancel_workflow` are
     non-terminal tools when present
2. Add request-focused scenarios:
   - root completes directly with `submit_root_outcome`
   - root delegates one workflow, waits/checks the handle, then submits root
     outcome
   - root delegation failure is synthesized into root outcome rather than
     closing the parent task by workflow close mutation
3. Add launch-seed and `AgentRunRecord` coverage:
   - root `initial_messages` are `[system, user_prompt, skill?]`
   - workflow agent `initial_messages` are `[system, context, task_guidance,
     skill?]`
   - subagent `initial_messages` are `[system, prompt]`
   - advisor `initial_messages` are `[system, parent_transcript,
     review_request]`
   - `AgentEntryMessages.to_messages()` always puts system first
   - `message_history` is written separately from launch-time
     `initial_messages`
   - root agent runs produce message history, token count, terminal result, and
     audit events like any other Task-backed agent
4. Add agent-tool surface scenarios:
   - `tests/mock/agent_tools/subagent/` covers `run_subagent` / explorer-style
     calls from root and executor agents. Assert subagents do not mint persisted
     Task rows, do receive their own launch seed, can read the shared workspace
     through normal tools, return a result into the parent conversation, and
     propagate timeout/cancel/error results without terminating the parent
     unless the parent chooses to submit a failed terminal.
   - `tests/mock/agent_tools/advisor/` covers `ask_advisor` if the advisor
     gate remains a separate helper surface. Assert it sees the parent
     transcript/review request, never mutates request/workflow state directly,
     and returns approval/rejection evidence to the parent agent.
   - `tests/mock/agent_tools/workflow/` covers the workflow-control tools as
     tools, not as the first-class framework: `delegate_workflow`,
     `check_workflow_status`, and `cancel_workflow`. Assert immediate handle
     return, parent Task remains `RUNNING`, second outstanding workflow is
     rejected until checked/cancelled, generator delegation is allowed, reducer
     delegation is not exposed, and cancel/status delivery is visible before
     the parent terminal.
5. Add terminal and role-exposure tests:
   - `submit_root_outcome` may finish only the root Task and double-finish is
     rejected
   - planner/generator/reducer terminals remain scoped to workflow Tasks
   - root profile has `context_recipe=None`
   - reducer profiles do not expose delegation tools
   - helper/subagent profiles do not expose root or workflow terminals
6. Update delegated-workflow scenarios to cover executor delegation rather than
   terminal handoff. The parent task remains `RUNNING` until its own terminal
   submission.
7. Delete runner assumptions around:
   - synthetic root Workflow
   - `submit_workflow_handoff`
   - `WAITING_WORKFLOW`
   - close-time mutation of the parent task
8. Align architecture pages and evidence paths with current files:
   `runtime/entry.py`, `workflow/starter.py`,
   `tools/workflow/delegate_workflow.py`, and `tools/submission/root`.

Exit gate:

```bash
rg -n "submit_workflow_handoff|WAITING_WORKFLOW|root workflow|child workflow|handoff" \
  backend/src/test_runner backend/tests docs/architecture

uv run pytest -q backend/src/test_runner/tests/mock/request
uv run pytest -q backend/src/test_runner/tests/mock/agent_tools
uv run pytest -q backend/src/test_runner/tests/mock/delegated_workflow
uv run pytest -q backend/tests/unit_test/test_tools/test_submission_main_role_terminals.py
```

### Phase D - Convert runner sandbox coverage to Rust runtime contracts

Goal: make the runner a Rust-sandbox validation harness, not a Python sandbox
regression harness.

1. Runtime selection:
   - run sandbox suites with `EOS_SANDBOX_RUNTIME=rust`
   - keep `EOS_SANDBOX_PROVIDER=docker` explicit for local live E2E
   - fail early if the Rust binary upload/signature/protocol pin is missing
2. Command/session tools:
   - remove the public `backend/src/tools/sandbox/shell` tool package; the
     public command surface is `backend/src/tools/sandbox/exec_command` plus
     `backend/src/tools/sandbox/write_stdin`
   - remove the public `backend/src/tools/background` tool package; background
     work is typed-only through:
     - `exec_command` to start finite or yielded command sessions, then
       `write_stdin` to write input or poll with empty input
     - `run_subagent` plus `check_subagent_progress` / `cancel_subagent`
     - `delegate_workflow` plus `check_workflow_status` / `cancel_workflow`
   - daemon RPCs for the command surface are `api.v1.exec_command` and
     `api.v1.write_stdin`; `api.v1.command.write_stdin` may exist only as a
     temporary compatibility alias, and `api.v1.shell` must not be registered,
     exported, or used by test-runner mock tests
   - replace old generic background shell scenarios with typed command/session
     coverage from Phase 3T:
     `exec_command`, `write_stdin`, non-login Bash, process-tree
     cleanup, and active-only session controls
   - split command coverage into explicit directories:
     - `tests/mock/sandbox/command/finite/` for finite `exec_command`:
       finite command completion, stdout/stderr/exit-code shape, timeout,
       non-login Bash environment, workspace-root cwd on every fresh command,
       no stdin/session id, detached-descendant cleanup after Bash exits, and
       OCC publish/capture behavior for successful writes.
     - `tests/mock/sandbox/command/session/` for yielded `exec_command` plus
       `write_stdin`: session id allocation, output ring/yield behavior,
       empty-input polling, Ctrl-C cancellation through `write_stdin`,
       active-only control rejection after terminal state, `cd` persistence
       inside one live command session, no `cd` persistence across separate
       finite commands, process-group cleanup, lease release, and terminal
       result reporting exactly once.
      - for yielded command sessions, the initial `exec_command` result may
        have `status=running` and no `changed_paths`; terminal publish,
        resource, and OCC timing assertions must inspect the final
        `write_stdin` result that observes completion.
   - remove references to `shell(background=True)`,
     `check_background_task_result`, `wait_background_tasks`, and generic shell
     background cancellation from model-facing assertions
3. Plugin service:
   - route plugin scenarios through Rust PPC service operations
   - do not open isolated workspace from plugin scenarios, and do not use
     plugin/LSP APIs to validate isolated workspace behavior
   - assert read-only service refresh does not publish
   - assert write/self-managed callbacks publish through the same daemon OCC
     writer/storage lock as primary publishes
   - add generic service coverage from
     `sandbox-plugin-service-adversarial-plan.md`:
     - `api.plugin.ensure` / `api.plugin.status` and dynamic
       `plugin.<plugin>.<op>` route resolution
     - `PluginServiceKey` reuse isolation across plugin id, digest, service id,
       service profile digest, workspace root, mode, and refresh strategy
     - host runtime cache isolation across sandbox id, plugin id,
       `layer_stack_root`, and `workspace_root`; LSP must not reuse a runtime
       ensured for `/testbed` after rebinding to `/ephemeral-os`
     - daemon `api.plugin.ensure` must reload even when the plugin digest is
       unchanged if parsed op routes, service process specs, or
       `runtime_loaded` differ from the loaded runtime
     - `api.build_workspace_base(reset=True)` must stop plugin service
       snapshots for the reset layer-stack root before rebuilding, matching the
       SWE-EVO reset path used by project-build/materialized-git tests
     - stale-manifest behavior: refresh, retryable `plugin_projection_stale`,
       or restart; never silent stale responses
     - `workspace_snapshot_refresh` strategies:
       `remount_workspace_and_notify`, `remount_workspace`, and
       `restart_service`
     - non-LSP dummy read-only service that caches file content and proves the
       daemon refresh protocol, so generic support is not inferred only from
       Pyright/LSP
    - warm-service process crash, timeout, heartbeat, LRU eviction, and
      process-group teardown
    - PPC request/reply multiplexing and plugin-to-daemon callbacks with
      message-id matching; plugin operations must not serialize on the shared
      service client, and only stale-service refresh/remount lifecycle may gate
      dispatch as a per-service singleflight before requests enter the
      multiplexed stream
    - service workspace byte growth remains bounded across repeated peer
      publishes
4. Isolated workspace:
   - run tests against `eosd ns-holder` + `eosd ns-runner` setns mode
   - assert enter rejects active sandbox-bound background work
   - assert exit drains/cancels active work and releases leases/scratch
   - assert isolated mode never depends on or invokes plugin/LSP operations
   - assert isolated never links/publishes through OCC from runner-visible
     behavior: writes are captured for audit and discarded on exit
   - assert holder death, setup timeout, veth/network failure, and SIGTERM ->
     SIGKILL fallback release leases and scratch
   - assert shell-free network hardening expectations: no dependence on
     in-image `ip`, `sysctl`, Python, cargo, or rustc
5. Remove runner imports from Python sandbox internals. The runner may call
   public tools/API helpers, but must not import from these Python implementation
   packages:
   - `sandbox.daemon`
   - `sandbox.overlay`
   - `sandbox.occ`
   - `sandbox.layer_stack`
   - `sandbox.ephemeral_workspace`
   - `sandbox.isolated_workspace`
   - `sandbox.shared`

Exit gate:

```bash
EOS_SANDBOX_PROVIDER=docker EOS_SANDBOX_RUNTIME=rust \
uv run pytest -q backend/src/test_runner/tests/mock/sandbox

rg -n "from sandbox\\.(daemon|overlay|occ|layer_stack|ephemeral_workspace|isolated_workspace|shared)|import sandbox\\.(daemon|overlay|occ|layer_stack|ephemeral_workspace|isolated_workspace|shared)" \
  backend/src/test_runner backend/tests/unit_test/test_test_runner
```

### Phase E - Make three concurrent live E2E runners the standard

Goal: prove the runner can execute three live sandbox lanes in parallel without
resource leakage or false serialization.

1. Add an explicit config field unless `sandbox_quota` is intentionally reused:
   `runner.live_e2e.concurrent_sandbox_runners: 3`.
2. Set the default and repository config to `3`. If `sandbox_quota` remains the
   backing setting, change `RunnerConfig.sandbox_quota` default from `5` to `3`
   and document that it is the live E2E runner cap.
3. Gate fixture provisioning with a semaphore of size `3` so tests cannot
   accidentally overrun the configured sandbox cap.
4. Build fixture provisioning from the fast setup path already exercised by
   `backend/scripts/bench_sandbox_e2e.py`,
   `backend/scripts/bench_rust_daemon_phase3t_pty.py`, and
   `backend/scripts/bench_rust_daemon_isolated_inspection.py`: build/upload the
   Rust `eosd` artifact once, start the Rust runtime through the same bootstrap
   path, and then lease at most three live sandboxes to pytest workers.
5. Add a smoke command for parallel live execution:

```bash
EOS_SANDBOX_PROVIDER=docker EOS_SANDBOX_RUNTIME=rust \
uv run pytest -q -n 3 backend/src/test_runner/tests/mock/sandbox/project_build
```

6. Add a teardown assertion that all three lanes release:
   sandbox leases, daemon invocations, PTY/session handles, plugin services, and
   isolated workspace holders.

Exit gate:

```bash
EOS_SANDBOX_PROVIDER=docker EOS_SANDBOX_RUNTIME=rust \
uv run pytest -q -n 3 backend/src/test_runner/tests/mock/sandbox
```

If the full sandbox suite is too expensive for every PR, define a fixed
three-lane smoke subset and keep the full command as the cutover gate.

### Phase F - Benchmarks and Rust migration gates

Goal: attach the runner cutover to measured Rust sandbox evidence.

Required benchmark/script lanes:

These scripts are also the fast-setup implementation reference for live E2E
fixture provisioning; runner code should reuse or extract their setup pieces
instead of creating a second sandbox bootstrap path.

```bash
uv run python backend/scripts/build_upload_eosd_docker.py --arch amd64
uv run python backend/scripts/bench_sandbox_e2e.py
uv run python backend/scripts/bench_rust_daemon_phase2.py
uv run python backend/scripts/bench_rust_daemon_phase3.py
uv run python backend/scripts/bench_rust_daemon_phase3t_pty.py
uv run python backend/scripts/bench_rust_daemon_phase3t_av7_parity.py
uv run python backend/scripts/bench_rust_daemon_phase3t_mixed_non_plugin.py
uv run python backend/scripts/bench_rust_daemon_phase3t_section7_non_plugin.py
uv run python backend/scripts/bench_rust_daemon_plugin.py
uv run python backend/scripts/bench_rust_daemon_isolated_inspection.py
uv run python backend/scripts/bench_plugin_refresh_strategies.py
```

Required Rust lanes:

```bash
cd sandbox
cargo fmt --all --check
cargo test --workspace --all-targets
cargo test -p eos-plugin
cargo test -p eos-daemon plugin
```

Required live parity claims:

- CP-4t non-login Bash command/session gate passes.
- CP-4/CP-5 contention gates pass against Rust `exec_command`/PTY session and
  plugin PPC.
- AV-3 cancellation/session cleanup passes under live load.
- AV-4 audit pull loses zero records under CP-4 load.
- AV-7 forward/back on-disk parity passes.
- AV-9 isolated workspace lifecycle parity passes.
- AV-10 plugin parity passes for read-only, write-allowed, and self-managed
  modes.
- Protocol fixture pin and canonical envelope tests pass on both Python and
  Rust sides.
- `put_archive` upload verifies SHA/mode/version and does not require Rust,
  Python, tar, gzip, or base64 inside the target sandbox image.
- Minisign/SHA fail-closed cases reject unsigned, mis-signed, wrong-arch, and
  SHA-mismatched artifacts before exec.
- Capability-negative Docker runs (`EOS_DOCKER_NO_PRIVILEGE=1`) fail with a
  structured capability/probe error and do not leave a half-started Rust
  runtime.
- Per-sandbox runtime selection is stable: no sandbox mixes Python and Rust
  runtimes within its lifetime.

All benchmark reports must state the benchmark category boundary. Raw mount-init
speedups must not be presented as end-to-end shell/tool speedups.

### Phase G - Remove Python sandbox infra

Goal: satisfy the final pass condition.

This phase starts only after Phases A-F are green and Rust is the default
sandbox runtime.

Allowed Python sandbox paths after removal:

- `backend/src/sandbox/api/`
- `backend/src/sandbox/host/`
- `backend/src/sandbox/provider/`
- `backend/src/sandbox/provider/bootstrap.py`
- `backend/src/config/sections/sandbox.py`
- protocol fixtures, runtime-artifact pinning, and host-side upload/signature
  verification code required by the Rust daemon

Removal candidates:

- `backend/src/sandbox/daemon/`
- `backend/src/sandbox/overlay/`
- `backend/src/sandbox/occ/`
- `backend/src/sandbox/layer_stack/`
- `backend/src/sandbox/shared/`
- `backend/src/sandbox/ephemeral_workspace/`
- `backend/src/sandbox/isolated_workspace/`
- Python daemon launch/thin-client/runtime-bundle/chunked-upload paths that the
  Rust plan marks as Phase 5 cutover removals
- Python sandbox plugin importlib dispatch under `ephemeral_workspace/plugin/`

Current Phase G checkpoint (2026-06-03):

- Host daemon selection is Rust-only. `backend/src/sandbox/host/daemon_client.py`
  must reject `EOS_SANDBOX_RUNTIME=python` and generate only `eosd daemon
  --client` / `eosd daemon --spawn` commands.
- Host path constants live in `backend/src/sandbox/host/paths.py`; host/API/
  provider/plugin host code must not import `sandbox.daemon.paths`.
- `backend/src/sandbox/host/runtime_bundle.py` uploads only the Rust-daemon
  plugin bridge payload required by PPC/LSP. It must not bundle Python
  `sandbox/daemon`, `overlay`, `occ`, `layer_stack`, `isolated_workspace`,
  daemon scripts, peer setup scripts, or vendored `pathspec`.
- LSP is still a Rust-daemon service path, not a Python daemon path:
  `sandbox/crates/eos-plugin` owns PPC/service contracts, `eos-daemon` owns
  service process/overlay/OCC behavior, and the Python files kept in the bundle
  are bridge payload modules exercised by `backend/scripts/bench_rust_daemon_plugin.py`.
- Public shell cleanup is source-level, not only a plan assertion:
  `backend/scripts/bench_rust_daemon_phase3.py` and
  `backend/scripts/bench_rust_daemon_plugin.py` must call
  `api.v1.exec_command` with `cmd`; the live-e2e harness may keep a local
  helper named `tool.shell(...)` for test readability, but it must call
  `sandbox_api.exec_command(...)` and return `ExecCommandResult`.
- Test-runner mock project-build probes must call the model-facing
  `exec_command` tool with `cmd`, not the removed shell-style `command`
  argument. `api.v1.write_stdin` remains the canonical daemon stdin op; the
  `api.v1.command.write_stdin` spelling is a Rust/Python compatibility alias
  only.
- The physical deletion pass is still open until the final inventory below is
  clean and `backend/src/sandbox` contains only host/API/provider/config/
  protocol support.

Pass condition:

```bash
rg -n "sandbox\\.(daemon|overlay|occ|layer_stack|shared|ephemeral_workspace|isolated_workspace)" \
  backend/src backend/tests

rg -n "sandbox\\.(daemon|overlay|occ|layer_stack|shared|ephemeral_workspace|isolated_workspace)" \
  docs/architecture

find backend/src/sandbox -maxdepth 2 -type d | sort
```

After confirmed safe removal, `backend/src/sandbox` contains only host/API/
provider/config/protocol support for the Rust sandbox. No test runner code may
import deleted Python sandbox implementation modules. Historical plan files may
still describe removed Python internals, but active architecture pages should
not present them as live implementation.

## 4. Cross-plan coverage matrix

This matrix is the review checklist against the three source plans. A migration
phase is not complete until its row has deterministic unit coverage plus at
least one runner scenario where the behavior crosses the real engine/tool path.

| Source plan | Coverage lane | Must assert |
| --- | --- | --- |
| `task_center_to_workflow_REFACTOR_PLAN.md` | Request/root lifecycle | request row creation, root Task with `workflow_id=None`, root `AgentRunRecord`, `submit_root_outcome` success/failure, exhaustion-to-failed request, no synthetic root Workflow |
| `task_center_to_workflow_REFACTOR_PLAN.md` | Launch messages | root, workflow, subagent, and advisor `initial_messages` shapes; system message first; launch seed separate from `message_history`; root does not use ContextEngine |
| `task_center_to_workflow_REFACTOR_PLAN.md` | Task persistence | `request_id` replaces `task_center_run_id`; `instruction` replaces `context_message`; position columns replace id-string routing; planner-created generator/reducer Tasks carry needs and position columns |
| `task_center_to_workflow_REFACTOR_PLAN.md` | Tool exposure | root terminal only `submit_root_outcome`; planner/generator/reducer terminals scoped to workflow Tasks; `delegate_workflow` non-terminal on root/executor; reducer cannot delegate |
| `task_center_to_workflow_REFACTOR_PLAN.md` | Delegated workflow | immediate handle return, one outstanding workflow per parent Task, status/cancel delivery, parent remains `RUNNING`, parent crash/cancel propagates, no close-time parent mutation |
| `task_center_to_workflow_REFACTOR_PLAN.md` | Planner DAG | at least one reducer, generator-only dependencies, reducer consumes generators, no reducer-to-reducer edges, no dangling generator, created Task-row validation rather than `Planned*` DTO validation |
| `sandbox-rust-external-migration-PLAN.md` | Runtime artifact | `put_archive` upload, SHA/version/mode verification, protocol fixture pin, minisign fail-closed, wrong arch rejected, no target-image toolchain dependency |
| `sandbox-rust-external-migration-PLAN.md` | Command tools | `tty=false` finite command lifecycle, `tty=true` PTY lifecycle, non-login Bash, explicit PATH/env, cwd semantics, detached descendant cleanup, active-only PTY controls, result reported once |
| `sandbox-rust-external-migration-PLAN.md` | OCC/LayerStack | canonical result parity, CAS byte identity, O(1) lowerdir disk/memory, single commit queue, storage-lock serialization, squash/deferred-GC lease retention, forward/back on-disk parity |
| `sandbox-rust-external-migration-PLAN.md` | Isolated workspace | `eosd ns-holder` enter/exit, setns runner, no OCC publish, audit-only writes, network hardening without shell tools, no plugin/LSP calls, holder teardown and lease cleanup under failures |
| `sandbox-rust-external-migration-PLAN.md` | Cutover/deletion | Rust default only after AV/CP gates; capability-negative fallback/probe; no Python bundle/thin-client/runtime-bundle/importlib dispatch; runner import fence against deleted sandbox internals |
| `sandbox-plugin-service-adversarial-plan.md` | Generic service registry | manifest validation, dynamic op route resolution, service key digest isolation, status shape, LRU eviction, crash/timeout/heartbeat teardown |
| `sandbox-plugin-service-adversarial-plan.md` | Freshness | `workspace_snapshot_refresh` before every read, all three refresh strategies, retryable stale errors, non-LSP cached dummy service, no stale response after peer publish |
| `sandbox-plugin-service-adversarial-plan.md` | Plugin writes | read-only never publishes, write workers publish through daemon overlay/OCC, self-managed callbacks use the same per-root writer/storage lock, interleave with direct write/edit/shell |
| `sandbox-plugin-service-adversarial-plan.md` | Plugin/IWS boundary | Plugin live scenarios must not open isolated workspace; isolated workspace coverage must not call plugin/LSP APIs |

## 5. Verification commands

Run after each relevant phase:

```bash
uv run ruff check backend/src/test_runner backend/tests/unit_test/test_test_runner
uv run pytest -q backend/tests/unit_test/test_test_runner
uv run pytest -q backend/src/test_runner/tests/mock/contracts
uv run pytest -q backend/src/test_runner/tests/mock/request
uv run pytest -q backend/src/test_runner/tests/mock/agent_tools
uv run pytest -q backend/src/test_runner/tests/mock/delegated_workflow
```

Run for sandbox cutover:

```bash
EOS_SANDBOX_PROVIDER=docker EOS_SANDBOX_RUNTIME=rust \
uv run pytest -q -n 3 backend/src/test_runner/tests/mock/sandbox

EOS_SANDBOX_PROVIDER=docker EOS_SANDBOX_RUNTIME=rust \
uv run pytest -q backend/src/test_runner/tests/mock/sandbox/command/non_tty \
  backend/src/test_runner/tests/mock/sandbox/command/tty

EOS_SANDBOX_PROVIDER=docker EOS_SANDBOX_RUNTIME=rust \
uv run pytest -q backend/tests/live_e2e_test/sandbox/plugin

EOS_SANDBOX_PROVIDER=docker EOS_SANDBOX_RUNTIME=rust \
uv run pytest -q backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ
```

Run final inventory:

```bash
rg -n "task_center_runner|TaskCenter|submit_workflow_handoff|WAITING_WORKFLOW" \
  backend/src backend/tests docs

rg -n "sandbox\\.(daemon|overlay|occ|layer_stack|shared|ephemeral_workspace|isolated_workspace)" \
  backend/src/test_runner backend/tests/unit_test/test_test_runner
```

## 6. Risk register

| Risk | Mitigation |
| --- | --- |
| Mechanical rename hides behavior changes | Phase A is rename-only; semantic changes start in Phase B/C. |
| Compatibility shims become permanent | Add a removal date and grep gate; preferred final state has no shim. |
| Runner still imports Python sandbox internals | Add import-fence tests before Phase G. |
| Three parallel lanes overrun Docker resources | Back the runner with an explicit semaphore/cap of `3`; assert teardown. |
| Plugin parity passes only for LSP | Add a non-LSP dummy service parity case before claiming generic plugin service support. |
| Root-request tests still behave like workflow tests | Keep `tests/mock/request/` as the primary bucket and reserve `delegated_workflow/` for tool-launched decomposition only. |
| Launch-message regressions hide behind terminal success | Assert `initial_messages` and `message_history` separately for root, workflow, subagent, and advisor launches. |
| PTY and non-PTY command paths collapse into one test | Keep `command/non_tty` and `command/tty` suites separate; each has distinct lifecycle and cleanup assertions. |
| Plugin freshness only tested through Pyright behavior | Require a non-LSP cached dummy service plus Pyright adapter parity before claiming generic plugin support. |
| Rust rollback becomes unsafe after durable publish | Require AV-7 forward/back on-disk parity before write-phase cutover or Python removal. |
| Docs drift after rename | Refresh `docs/architecture/test_runner/*`, evidence paths, and search index in the same phase as the rename. |

## 7. Cutover checklist

- `backend/src/test_runner` exists; `backend/src/task_center_runner` is gone or
  contains only a temporary explicit compatibility shim.
- CLI works: `uv run python -m test_runner.benchmarks.sweevo --help`.
- Request scenarios cover direct root completion and root-launched delegated
  workflow completion.
- Agent-tool scenarios cover subagent, advisor, and workflow-control tools as
  ordinary tools used by root/executor agents.
- Delegated-workflow scenarios cover executor `delegate_workflow` without
  terminal handoff or parent close mutation.
- Command scenarios cover both `tty=false` finite commands and `tty=true` PTY
  sessions, including stdin/progress/cancel and cleanup.
- Sandbox scenarios run with `EOS_SANDBOX_RUNTIME=rust`.
- Three parallel live E2E lanes pass with no resource leaks.
- Runner has no imports from deleted Python sandbox infra modules.
- Rust sandbox CP/AV gates in Phase F are green.
- Python sandbox infra removal inventory is reviewed and deletion is confirmed
  safe.
- Final pass condition holds: all non-host/API/provider Python sandbox infra is
  removed from `backend/src/sandbox`.
