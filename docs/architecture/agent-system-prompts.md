# Agent System Prompts

This document is the index for harness-agent prompt ownership. The installed
system prompts are role-local package assets, not duplicated architecture text:

- Executor: `backend/src/task_center/harness_agents/executor/agent.md`
- Planner: `backend/src/task_center/harness_agents/planner/agent.md`
- Evaluator: `backend/src/task_center/harness_agents/evaluator/agent.md`
- Explorer: `backend/src/agents/builtins.py`

Each role's `definition.py` loads its sibling `agent.md` and installs that
text into the role's `AgentDefinition.system_prompt`.

## HarnessGraph as the single source of truth

Every prompt that crosses a role boundary in a planning unit is rendered
from four note fields stored on the `HarnessGraph` itself
(`backend/src/task_center/model/harness.py`):

| Field | Set when | Source | Used for |
|---|---|---|---|
| `root_goal` | `request_plan` is invoked | the caller's task input | `ROOT_GOAL` in planner & evaluator prompts |
| `request_plan_note` | `request_plan` is invoked | the `request_plan_note` arg | `REQUEST_PLAN_NOTE` in planner & evaluator prompts; the goal this graph as a whole must achieve |
| `handoff_plan_note` | `submit_plan_handoff` is invoked | the `handoff_plan_note` arg | `PLAN_HANDOFF_NOTE` in evaluator prompt; describes the plan itself (PLAN_SHAPE, TOPOLOGY, COVERAGE_MAP, GAP) |
| `evaluator_note` | `submit_plan_handoff` is invoked | the `evaluator_note` arg | the evaluator task's input (rendered as `TASK_INPUT`); the planner's explicit verification brief to the evaluator |

The runtime no longer surfaces sibling/recovery state automatically. If a
caller wants the next planner to see prior context (failed siblings, child
closure summaries, etc.), the caller must fold that material into
`request_plan_note` itself.

## Shared Envelope Rules

Every payload that crosses a role boundary is a single string with
`## ALLCAPS_LABEL` sections.

Every role-specific user prompt starts with a generated `## INSTRUCTIONS`
section that tells the agent to read the supplied context and complete the
role's target payload. The remaining sections carry graph/task data.

### PlannerLaunchContext

Built once when `request_plan` creates a new harness graph, then stored as
the planner's task input.

```text
## INSTRUCTIONS
Read ROOT_GOAL as context and anti-drift anchor. Complete the work described
in REQUEST_PLAN_NOTE by producing the required planner handoff.

## ROOT_GOAL
<the input of the task that called request_plan; the anti-drift anchor>

## REQUEST_PLAN_NOTE
<the verbatim request_plan_note the caller passed; the goal this harness
graph must achieve as a whole>
```

`ROOT_GOAL` and `REQUEST_PLAN_NOTE` are free-form text. They may be the
raw user prompt, a TaskSpec with labeled headings, or arbitrary prose
from an executor/evaluator handoff. Agents must parse them as prose, not
assume a fixed shape.

Owner: `task_center.harness_agents.planner.context`.

### ExecutorLaunchContext

Rebuilt at executor dispatch time. Executors see only their own task input
and DONE direct-dependency summaries.

```text
## INSTRUCTIONS
Read DEPENDENCY_SUMMARIES as locked-in context, then complete the work
described in TASK_INPUT. TASK_INPUT is the task you own.

## TASK_INPUT
<executor task input — raw user prompt for the entry-root executor, a
planner-emitted TaskSpec for executors spawned by submit_plan_handoff, or
arbitrary prose in other shapes>

## DEPENDENCY_SUMMARIES
### <dep_id>
input: <dependency task input>
summaries:
  - [success] <summary text>
```

`TASK_INPUT` is free-form text — the executor must not assume a TaskSpec
shape and must parse whatever it received.

Owner: `task_center.harness_agents.executor.context`.

### EvaluatorLaunchContext

Rebuilt at evaluator dispatch time after every executor child in the harness
graph is terminal.

```text
## INSTRUCTIONS
Read ROOT_GOAL, REQUEST_PLAN_NOTE, PLAN_HANDOFF_NOTE, and child summaries as
context. Complete TASK_INPUT, which is the planner's evaluator_note, by
verifying whether REQUEST_PLAN_NOTE was satisfied.

## ROOT_GOAL
<harness.root_goal>

## REQUEST_PLAN_NOTE
<harness.request_plan_note — the goal this graph must achieve>

## PLAN_HANDOFF_NOTE
<harness.handoff_plan_note — the planner's plan-shape description>

## SUCCESS_CHILD_SUMMARIES
<DONE direct children: success and child_success summaries>

## FAIL_CHILD_SUMMARIES
<FAILED direct children: failure and child_failure summaries>

## BLOCKED_CHILD_SUMMARIES
<dependency-blocked direct children>

## TASK_INPUT
<harness.evaluator_note — the planner's explicit verification brief>
```

`ROOT_GOAL` and `REQUEST_PLAN_NOTE` are free-form text — the evaluator
must parse them as prose. `REQUEST_PLAN_NOTE` is the gate (what this
graph must achieve); `ROOT_GOAL` is the anchor.

Owner: `task_center.harness_agents.evaluator.context`.

## Shared Prompt Dispatcher

`task_center.harness_agents.prompts.build_task_prompt` is the dispatch-time
prompt builder:

- planner tasks use their pre-rendered task input unchanged (the rendered
  `PlannerLaunchContext`)
- executor tasks are wrapped with `ExecutorLaunchContext`
- evaluator tasks are wrapped with `EvaluatorLaunchContext`

## Terminal Tools

| Tool | Caller | Effect |
|---|---|---|
| `request_plan(request_plan_note)` | executor or evaluator | creates a new harness graph; captures caller.input as `root_goal` and `request_plan_note` verbatim; spawns a planner |
| `submit_plan_handoff(tasks, task_inputs, handoff_plan_note, evaluator_note)` | planner | materializes executor children + evaluator; stores `handoff_plan_note` and `evaluator_note` on the harness graph; the evaluator's task input is `evaluator_note` |
| `submit_task_success(summary)` | executor or evaluator | marks DONE |
| `submit_task_failure(summary)` | executor only | marks FAILED |
| `submit_evaluation_failure(summary)` | evaluator only | marks the planning unit FAILED |
| `submit_exploration_result(findings)` | explorer subagent | returns findings to the dispatching parent |

## Related Docs

- `docs/architecture/agent-team-coordination.md` — role boundaries, terminal
  effects, and information-flow diagrams.
- `docs/architecture/gan-task-graph-v1.md` — data model and persistence.
- `docs/architecture/background-tasks-and-subagents.md` — background task and
  explorer details.
