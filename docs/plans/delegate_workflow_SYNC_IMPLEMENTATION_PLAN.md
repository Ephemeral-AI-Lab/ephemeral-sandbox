# delegate_workflow Synchronous Workflow Tool Implementation Plan

Status: draft
Date: 2026-06-01

## User Decisions

- Tool name: `delegate_workflow`.
- The calling generator agent waits until the delegated workflow reaches a final workflow status.
- The tool returns the full delegated workflow outcomes to the same agent.
- No timeout or cancellation semantics in this phase. Those are deferred.

## Goal

Replace the current terminal handoff model with a normal blocking workflow tool.
The generator agent should behave like a general agent that can call workflow as
a capability, inspect the returned outcomes, continue reasoning or tool use, and
then finish its own task with `submit_generator_outcome`.

Target control flow:

```text
parent generator RUNNING
  -> calls delegate_workflow(goal=...)
  -> parent task becomes WAITING_WORKFLOW while child workflow runs
  -> child Workflow -> Iteration -> Attempt lifecycle completes
  -> parent task returns to RUNNING
  -> delegate_workflow returns child workflow status + flattened outcomes
  -> parent agent continues
  -> parent agent eventually calls submit_generator_outcome
```

This replaces:

```text
parent generator calls submit_workflow_handoff terminal
  -> parent run stops
  -> child workflow closes
  -> parent generator is directly marked DONE or FAILED
```

## Non-Goals

- Do not add timeout behavior.
- Do not add user cancellation behavior.
- Do not redesign Workflow, Iteration, Attempt, planner, reducer, or run root
  bootstrap semantics.
- Do not introduce peer-to-peer agent messaging.
- Do not use `EphemeralAttemptAgentLauncher.wait_for_idle()` from inside
  `delegate_workflow`.
- Do not keep `delegate_workflow` as a terminal tool.

## Current Anchors

- Current terminal tool:
  `backend/src/tools/submission/generator/submit_workflow_handoff/submit_workflow_handoff.py`
- Generator context:
  `backend/src/tools/submission/context/generator.py`
- Child workflow start:
  `backend/src/task_center/workflow/starter.py`
- Child workflow close route:
  `backend/src/task_center/workflow/lifecycle.py`
- Parent task state mutation:
  `backend/src/task_center/attempt/orchestrator.py`
- Launcher pending task behavior:
  `backend/src/task_center/attempt/launch.py`
- Executor profile:
  `backend/src/agents/profile/main/executor.md`
- Existing focused tests:
  `backend/tests/unit_test/test_tools/test_submission_main_role_terminals.py`
  `backend/tests/unit_test/test_task_center/test_lifecycle/test_child_workflow_handoff.py`
  `backend/tests/unit_test/test_task_center/test_lifecycle/test_phase04_workflow_request_start.py`

## Design

### Tool Surface

Add a non-terminal tool named `delegate_workflow`.

Recommended input:

```json
{
  "goal": "The delegated workflow goal, including relevant findings and constraints."
}
```

Recommended output model:

```json
{
  "workflow_id": "child workflow id",
  "workflow_status": "succeeded | failed",
  "outcomes": [
    {
      "status": "success | failed",
      "role": "generator | reducer",
      "task_id": "...",
      "outcome": "..."
    }
  ]
}
```

Return the output as JSON validated by a Pydantic output model, not as an
unstructured sentence. The agent needs machine-readable child outcomes to decide
how to continue its own task.

Set:

- `is_terminal_tool=False`
- `intent=Intent.LIFECYCLE`
- `background` remains forbidden

Using `Intent.LIFECYCLE` keeps the call ordered as a state-changing lifecycle
operation. The existing engine lifecycle batch policy can reject sibling tools
so the routing-state change is not mixed with unrelated file or shell work.

### Profile Exposure

Move workflow delegation out of executor terminals and into normal allowed tools.

Executor profile target:

```yaml
allowed_tools:
  - delegate_workflow
  - ...
terminals:
  - submit_generator_outcome
```

Update executor prose:

- `delegate_workflow` is a blocking tool for delegating sub-work to a child
  workflow and receiving child outcomes.
- `submit_generator_outcome` remains the only generator terminal.
- After `delegate_workflow` returns, the generator must decide whether to keep
  working, call more tools, delegate again if permitted, or submit success/failure.

Do not route `delegate_workflow` through the terminal catalog. It is not a final
submission and should not be described by `tools/_terminals/registry.py`.

### Hook Policy

Keep the existing safety shape, but rename it around delegation rather than
handoff.

Recommended hooks:

- `RequireNoInflightBackgroundTasks("delegate_workflow")`
- renamed nested-depth prehook, for example
  `DisallowNestedWorkflowDelegation("delegate_workflow")`

Do not use `AdvisorApprovalPreHook` for `delegate_workflow` in the first pass.
That hook and `ask_advisor` are explicitly framed around terminal submissions.
The final `submit_generator_outcome` can still be advisor-gated if the profile
requires advisor approval before terminal submissions.

Keep or update the "delegate before edits" reminder only if that policy is still
desired for synchronous delegation. If kept, rename strings from
`submit_workflow_handoff` to `delegate_workflow`.

### Waiting Primitive

Do not implement waiting by calling `launcher.wait_for_idle()` inside the tool.
The parent agent is itself one of the launcher's pending tasks, so waiting for
launcher idleness from inside that parent can deadlock.

Add a small workflow-specific waiter keyed by `workflow_id`.

Proposed module:

```text
backend/src/task_center/workflow/waiter.py
```

Proposed shape:

```python
@dataclass(frozen=True, slots=True)
class DelegatedWorkflowResult:
    workflow_id: str
    workflow_status: str
    outcomes: tuple[ExecutionTaskOutcome, ...]


class WorkflowCompletionWaiter:
    def watch(self, workflow_id: str) -> None: ...
    async def wait(self, workflow_id: str) -> DelegatedWorkflowResult: ...
    def resolve(self, result: DelegatedWorkflowResult) -> None: ...
```

Wire one waiter into `AttemptDeps`, created in
`TaskCenterEntry._create_runtime(...)` and in unit-test runtimes.

The waiter is process-local, matching the existing process-local
`AttemptOrchestratorRegistry`. This is acceptable for the current runner model
because child workflows and their parent task run inside the same active runtime.

### Start And Wait Flow

Extend `GeneratorSubmissionContext` with a method that starts a child workflow
and waits for it:

```python
async def delegate_workflow(self, *, goal: str) -> DelegatedWorkflowResult:
    started = WorkflowStarter(runtime=self.runtime).start(
        prompt=goal,
        parent_task_id=self.task_center_task_id,
    )
    self.runtime.workflow_completion_waiter.watch(started.workflow_id)
    return await self.runtime.workflow_completion_waiter.wait(started.workflow_id)
```

Implementation detail: register the waiter before the tool first awaits. The
current launcher starts child agents with `asyncio.create_task(...)`; the child
cannot complete until the parent coroutine yields, so this avoids the normal
same-loop race without introducing more state.

Make `wait(...)` resilient anyway: before awaiting the future, re-read the
workflow row and return immediately if it is already closed. That protects the
tool if a future test double or launcher path ever completes child work
synchronously.

### Child Workflow Close Flow

Change the attempt-bound child close route.

Current behavior:

```text
WorkflowLifecycle._route_close(...)
  -> parent_orchestrator.apply_child_workflow_outcome(...)
  -> parent generator WAITING_WORKFLOW -> DONE or FAILED
  -> child outcomes copied onto parent task
  -> stage advancer may advance dependents
```

Target behavior for `delegate_workflow`:

```text
WorkflowLifecycle._route_close(...)
  -> parent_orchestrator.resolve_delegated_workflow(...)
  -> parent generator WAITING_WORKFLOW -> RUNNING
  -> child outcomes sent to WorkflowCompletionWaiter
  -> no parent task outcome written yet
  -> no stage advancement yet
```

The parent generator's eventual `submit_generator_outcome` remains the only path
that writes the parent generator outcome and advances the parent attempt.

Recommended orchestrator method:

```python
def resolve_delegated_workflow(
    self,
    *,
    generator_task: dict[str, Any],
    child_workflow: Workflow,
) -> None:
    ...
```

Expected behavior:

- Require the parent task to be `WAITING_WORKFLOW`.
- Assert it is still a generator task for this attempt.
- Build `workflow_outcomes(child_workflow, iteration_store=...)`.
- CAS parent task from `WAITING_WORKFLOW` back to `RUNNING`.
- Leave `child_workflow_id` as the audit/back-link to the most recent delegated
  workflow. Do not clear it in this phase.
- Resolve the workflow waiter with status and outcomes.
- Do not call `_stage_advancer.advance_ready_tasks()`.

Failed child workflows return `workflow_status="failed"` and failed outcomes to
the parent agent. They do not automatically fail the parent generator. The
parent agent may summarize the failure, recover, delegate differently, or submit
its own failure.

### Root Workflow Path

Do not change the root workflow bootstrap path.

The root workflow still uses the synthetic `<run_id>:root` generator and
`RunController.on_root_workflow_closed(...)` to finish the TaskCenter run. The
new synchronous `delegate_workflow` behavior only applies to real
attempt-bound generator tasks.

### Naming And File Moves

Prefer adding the new tool and deleting the old one instead of maintaining both
names.

Suggested edits:

- Add `backend/src/tools/workflow/delegate_workflow/`
- Add `backend/src/tools/workflow/__init__.py`
- Add `make_workflow_tools()` and register it from
  `backend/src/tools/_framework/factory.py`
- Remove `submit_workflow_handoff` from `make_submission_tools()`
- Rename `SUBMIT_WORKFLOW_HANDOFF_TOOL_NAME` to `DELEGATE_WORKFLOW_TOOL_NAME`
  or add a new constant and remove the old constant
- Update agent profile frontmatter and tests from `submit_workflow_handoff` to
  `delegate_workflow`

If the first implementation needs the smallest possible diff, the tool can live
temporarily under `tools/submission/generator/delegate_workflow`, but the
logical owner should still be documented as a workflow tool, not a terminal
submission.

## Implementation Phases

### Phase 1: Tool Contract And Registration

- Add `delegate_workflow` input and output schemas.
- Register it as a non-terminal lifecycle tool.
- Remove `submit_workflow_handoff` from the default registered tool set.
- Update `backend/tests/unit_test/test_tools/test_submission_tool_registration.py`.
- Update `backend/tests/contracts/test_tool_intent_drift.py`.

Verification:

```bash
uv run pytest -q \
  backend/tests/unit_test/test_tools/test_submission_tool_registration.py \
  backend/tests/contracts/test_tool_intent_drift.py
```

### Phase 2: Runtime Waiter

- Add `WorkflowCompletionWaiter`.
- Add `workflow_completion_waiter` to `AttemptDeps`.
- Wire it in `TaskCenterEntry._create_runtime(...)`.
- Update test runtime builders to pass a waiter.
- Add focused unit tests for:
  - waiter resolves exactly one waiting workflow id
  - missing waiter result fails clearly if `delegate_workflow` is called without
    runtime wiring

Verification:

```bash
uv run pytest -q backend/tests/unit_test/test_task_center/test_lifecycle
```

### Phase 3: Close Route Semantics

- Replace `apply_child_workflow_outcome(...)` with
  `resolve_delegated_workflow(...)` or change its behavior and name.
- On child close, restore parent task to `RUNNING`.
- Resolve the waiter with flattened child workflow outcomes.
- Do not write outcomes to the parent task.
- Do not advance the parent attempt from the close route.

Focused regression expectations:

- During child execution: parent generator is `WAITING_WORKFLOW`.
- After child success: parent generator is `RUNNING`; `delegate_workflow`
  returns `workflow_status="succeeded"` and child outcomes.
- After child failure: parent generator is `RUNNING`; `delegate_workflow`
  returns `workflow_status="failed"` and child outcomes.
- Parent attempt does not pass or fail until parent calls
  `submit_generator_outcome`.

Verification:

```bash
uv run pytest -q \
  backend/tests/unit_test/test_task_center/test_lifecycle/test_child_workflow_handoff.py \
  backend/tests/unit_test/test_task_center/test_lifecycle/test_phase04_workflow_request_start.py
```

The first file should be renamed or rewritten around delegation rather than
handoff.

### Phase 4: Agent Profile And Prompt Updates

- Move `delegate_workflow` into executor `allowed_tools`.
- Remove `submit_workflow_handoff` from executor `terminals`.
- Update executor instructions to explain blocking delegation.
- Update nested-depth reminder and prehook strings.
- Remove terminal catalog entry for `submit_workflow_handoff`.
- Keep `submit_generator_outcome` as the generator terminal.

Verification:

```bash
uv run pytest -q \
  backend/tests/unit_test/test_agents/test_agent_markdown.py \
  backend/tests/unit_test/test_agents/test_skill_message.py \
  backend/tests/unit_test/test_tools/test_submission_generator_prompts.py \
  backend/tests/unit_test/test_tools/test_submission_soft_reminders.py
```

Some prompt tests should be renamed rather than preserving handoff wording.

### Phase 5: End-To-End Tool Behavior

Rewrite the existing handoff tool tests around the synchronous behavior.

Key test:

```text
execute delegate_workflow in a task
  -> delegate_workflow is not terminal
  -> parent task becomes WAITING_WORKFLOW while the child is open
  -> drive child planner/generator/reducer to close
  -> delegate_workflow returns child outcomes
  -> parent task is RUNNING
  -> submit_generator_outcome marks parent DONE
  -> parent reducer can run
```

Verification:

```bash
uv run pytest -q backend/tests/unit_test/test_tools/test_submission_main_role_terminals.py
```

Consider renaming this test file or moving the non-terminal workflow tests into
a separate file, because `delegate_workflow` is no longer a submission terminal.

### Phase 6: Architecture Docs Refresh

Update the maintained architecture docs, not only the plan.

Required pages:

- `docs/architecture/index.html`
- `docs/architecture/task_center/index.html`
- `docs/architecture/task_center/lifecycle.html`
- `docs/architecture/task_center/bridges.html`
- `docs/architecture/task_center/terminal-tools.html`
- `docs/architecture/tools/submission.html`
- `docs/architecture/tools/terminals.html`
- `docs/architecture/agent_loops/prompt-context.html`
- `docs/architecture/assets/search-index.js`

Doc claims to change:

- `submit_workflow_handoff` is no longer a terminal tool.
- Delegated workflow no longer directly completes the parent generator.
- Child workflow outcomes return to the same parent agent via
  `delegate_workflow`.
- The parent generator remains responsible for its own terminal outcome.

Verification:

```bash
rg -n "submit_workflow_handoff|workflow handoff|handoff|apply_child_workflow_outcome|WAITING_WORKFLOW -> DONE|does not return to RUNNING" \
  docs/architecture backend/src backend/tests
```

Expected remaining matches should be historical migration notes only, or none if
the implementation fully removes the old concept.

## Success Criteria

- `delegate_workflow` is visible to executor/generator agents as a normal tool.
- `delegate_workflow` is not terminal and does not stop the parent agent run.
- Parent generator status transitions:

```text
RUNNING -> WAITING_WORKFLOW -> RUNNING -> DONE/FAILED
```

- Child workflow close returns flattened workflow outcomes to the tool result.
- Child workflow close does not write parent task outcomes.
- Parent `submit_generator_outcome` remains the only generator terminal.
- The parent attempt cannot close while the parent generator is
  `WAITING_WORKFLOW` or `RUNNING`.
- Existing root run behavior is unchanged.

## Risks And Watchpoints

- Deadlock risk: any implementation that awaits `launcher.wait_for_idle()` from
  inside `delegate_workflow` is wrong.
- Missing waiter risk: if the waiter is not process-local to the same runtime as
  the child close route, the parent tool can wait forever. Keep the waiter in
  `AttemptDeps` and resolve it from the parent orchestrator.
- Prompt drift risk: executor frontmatter, terminal catalog, advisor prompt, and
  architecture docs currently describe handoff as terminal. Update them in the
  same patch family.
- Semantics drift risk: if child failure still marks the parent generator
  `FAILED`, the implementation is still the old workflow-engine model.
- Test naming risk: tests that keep "handoff terminal" names after the behavior
  change will hide wrong assumptions.

## Deferred Questions

- Should `delegate_workflow` be allowed after edits have started?
- Should `delegate_workflow` require advisor review, a different lightweight
  review, or no review?
- Should a parent generator be allowed to call `delegate_workflow` more than once
  serially after the first child has closed?
- What should happen if the parent agent crashes while waiting?
- What should happen if the child workflow never closes?
- How should explicit user cancellation propagate into parent and child state?
