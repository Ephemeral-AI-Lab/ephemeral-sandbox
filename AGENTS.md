# Agent Collaboration and Implementation Notes

This codebase is edited across multiple agent sessions at the same time. A dirty
worktree is usually expected and should be treated as parallel agent activity,
not as a reason to stop.

## Project Context

- Python package metadata lives in `pyproject.toml`. The project supports Python
  `>=3.10`; lint/type tooling is configured for Python 3.11.
- Use `uv` for dependency management and command execution. Typical setup is
  `uv sync --extra dev`; run project commands with `uv run ...` when the virtual
  environment is not already active.
- Main backend areas are `backend/src/task_center`, `backend/src/engine`, and
  `backend/src/sandbox`.

## Codebase Memory And Architecture

Use `docs/architecture/index.html` as the maintained codebase-memory and
architecture bundle before making architecture-shaped changes. The root page
links the module pages for `docs/architecture/task_center`,
`docs/architecture/agent_loops`, `docs/architecture/tools`, and
`docs/architecture/sandbox`; those pages are the first stop for ownership,
workflow, invariants, diagnostics, and refresh triggers. Treat the older
TaskCenter harness reference at `docs/task_center_harness_and_context_engine.html`
as historical background and stale-claim comparison material; the maintained
cross-module map now lives under `docs/architecture`.

- Treat the code checkout as source truth and `docs/architecture` as the
  curated memory layer. If an architectural claim matters, verify the current
  code anchor and update the smallest affected architecture page rather than
  adding disconnected notes. When refreshing architecture docs, follow each
  page's `data-last-reviewed-commit` and `data-evidence-paths` metadata.
- TaskCenter is the persisted multi-agent control plane. Coordination flows
  through TaskCenter state, terminal submissions, context packets, and lifecycle
  reports; do not introduce peer-to-peer agent communication or a global agent
  orchestrator. Its durable model is Workflow -> Iteration -> Attempt, with each
  Attempt owning one planner -> generator DAG -> evaluator try.
- `ContextEngine` builds recipe-driven packets from store state for role,
  retry, deferral, and evaluation contexts. Keep lifecycle policy in TaskCenter
  handlers/managers, not hidden in context construction. Recipes live under
  `backend/src/task_center/context_engine/recipes`.
- TaskCenter state is grounded in `backend/src/task_center/workflow/state.py`,
  `backend/src/task_center/iteration/state.py`, and
  `backend/src/task_center/attempt/state.py`. Handoff runs through
  `submit_execution_handoff`, `WorkflowStarter.start(WorkflowOrigin.task(...))`,
  `WorkflowClosureReportRouter`, and
  `AttemptOrchestrator.apply_workflow_closure_report`.
- `AttemptOrchestrator` is per-Attempt lifecycle machinery, not permission to
  add a global orchestration layer. Related launch, stage-advance, and close
  behavior lives under `backend/src/task_center/attempt`.
- The engine loop owns agent execution and terminal-tool enforcement.
  Successful terminal tools are stamped as terminating by
  `backend/src/tools/_framework/execution/tool_call.py`; dispatch and loop exit
  run through `backend/src/engine/tool_call/dispatch.py` and
  `backend/src/engine/query/loop.py`. Terminal tools must be called alone;
  those terminal results are TaskCenter state inputs, not just user-facing
  messages. Background execution is an engine dispatch mode, not a provider-level
  persistent shell session.
- Sandbox is the tool-execution environment. Agents run outside the sandbox and
  call provider-backed sandbox APIs for file, shell, plugin, and workspace
  actions. Provider selection lives in
  `backend/src/sandbox/provider/bootstrap.py` and
  `backend/src/config/sections/sandbox.py`; Docker is default unless
  `EOS_SANDBOX_PROVIDER` or central config selects Daytona. Provider bootstrap
  is process-global and first-call-wins.
- Workspace routing lives in
  `backend/src/sandbox/daemon/workspace_tool_dispatch.py`. Shared workspace
  `read_file`, `write_file`, and `edit_file` use daemon-owned LayerStack/OCC
  fast paths when a workspace binding exists. Shell, search, and plugin-style
  operations use the overlay pipeline; write-capable overlay results publish
  through OCC-gated paths. LayerStack/OCC services live in
  `backend/src/sandbox/layer_stack` and `backend/src/sandbox/occ`; overlay
  execution lives in `backend/src/sandbox/ephemeral_workspace` and
  `backend/src/sandbox/overlay`.
- Isolated workspace mode is an explicit `enter_isolated_workspace` /
  `exit_isolated_workspace` lifecycle. It gives an agent a persistent private
  workspace for that isolated session through the active `agent_id` handle, not
  a separate public `isolated_workspace_id` routing parameter. Writes are
  captured and audited but not OCC-published; exit tears down the namespace,
  releases the snapshot lease, and removes scratch state. Enter rejects active
  sandbox-bound background work, exit cancels or drains it, and plugin/LSP
  operations are blocked while isolated mode is active for that agent. The code
  lives in
  `backend/src/tools/isolated_workspace`,
  `backend/src/sandbox/host/isolated_workspace_lifecycle.py`, and
  `backend/src/sandbox/isolated_workspace`; the architecture references are
  `docs/architecture/tools/isolated-workspace.html` and
  `docs/architecture/sandbox/workspaces.html`.

## Parallel Agent Work

- Do not revert, overwrite, or discard another agent's work unless the user
  explicitly asks for that.
- If existing changes are outside the current plan, infer the likely intent from
  file names, diffs, tests, and surrounding code, then adjust your own plan
  around that work instead of blocking. Ask only when ambiguity makes safe
  progress impossible.
- Keep your edits scoped to your task, but integrate with concurrent changes
  when needed for correctness.
- If tests fail because of another agent's in-progress work, it is acceptable to
  help fix those failures when the fix is clear and compatible with your task;
  then continue your own work.
- Before committing or staging, distinguish your intended changes from unrelated
  concurrent work unless the user explicitly asked to include everything.

## Before Coding

- State material assumptions before acting when the task or ownership boundary is
  ambiguous.
- If a request has multiple plausible interpretations, name the options and pick
  the smallest safe interpretation, or ask when guessing would risk the user's
  work.
- Push back on unnecessary complexity. Prefer the direct implementation that
  solves the stated problem.

## Implementation Style

- Write the minimum code that satisfies the request. Do not add speculative
  features, configuration, extension points, or abstractions.
- If the solution is growing large and a smaller design would solve the same
  problem, simplify before continuing.
- Avoid defensive branches for impossible states unless the surrounding codebase
  already requires that style.
- Match the existing code's style and ownership boundaries even when you would
  design greenfield code differently.

## Surgical Scope

- Touch only the files and lines needed for the user's request.
- Do not opportunistically refactor adjacent code, reformat unrelated files, or
  delete pre-existing dead code.
- Clean up imports, variables, functions, and files that your own changes made
  unused, but leave unrelated cleanup as a note unless asked.
- Every changed line should have a clear reason tied to the task, a test fix, or
  compatibility with parallel work.

## Verification

- Convert the request into concrete success criteria before or while
  implementing.
- For bugs, prefer a failing test or focused reproduction before the fix when
  practical.
- For refactors, preserve behavior and run the narrowest convincing checks before
  and after risky changes when practical.
- For multi-step tasks, keep a short plan with a verification step for each
  meaningful phase, then iterate until the criteria are met.
