# Phase 03 - Agent Roles and Tool Gates

## Goal

Port role semantics and terminal-tool gating onto the new
`ComplexTaskRequest` / `TaskSegment` / `HarnessGraph` state model.

This phase should preserve the public agent contract while changing where
state is read from.

## Role model

TaskCenter owns four main agent roles, all scoped to one `HarnessGraph` except
the requesting executor, which is paused while its complex task request runs.

| Role | Scope | Tools / terminals |
| ---- | ----- | ----------------- |
| Planner | one `HarnessGraph` | `submit_full_plan`, `submit_partial_plan` |
| Generator executor | one `HarnessGraph` DAG node | `submit_execution_success`, `submit_execution_failure`, `request_complex_task_solution` |
| Generator verifier | one `HarnessGraph` DAG node | `submit_verification_success`, `submit_verification_failure` |
| Evaluator | sink for one `HarnessGraph` | `submit_evaluation_success`, `submit_evaluation_failure` |

Planner has no failure terminal. Executor, verifier, and evaluator are the
roles that can declare failure.

`request_complex_task_solution` is not a terminal failure. It is a
non-terminal orchestration request that can pause the executor and later return
a complex-task close report.

## Planner terminal signatures

Planner submissions must define the segment contract directly.

```python
submit_full_plan(
    task_specification: str,
    evaluation_criteria: list[str],
    tasks: list[{"id": str, "agent_name": str, "deps": list[str]}],
    task_specs: dict[str, str],
) -> TerminalSubmission
```

```python
submit_partial_plan(
    task_specification: str,
    evaluation_criteria: list[str],
    tasks: list[{"id": str, "agent_name": str, "deps": list[str]}],
    task_specs: dict[str, str],
    continuation_goal: str,
) -> TerminalSubmission
```

Each `tasks` item is a flat graph node with exactly `id`, `agent_name`, and
`deps`. `task_specs` maps each task id to that task's detailed instructions.
The keys in `task_specs` must exactly match the task ids in `tasks`: no missing
specs, no extra specs, and no duplicate task ids.

`task_specification` describes the exact work for the current segment.
`evaluation_criteria` lists the pass/fail conditions the evaluator must
use to evaluate this segment's result. `HarnessGraphOrchestrator` passes both
fields to the evaluator as evaluation instructions. For `submit_partial_plan`,
`continuation_goal` describes what the next segment should solve if this graph
is accepted as the segment's closing graph.

## Helper roles

| Helper | Entry point | Blocking | Edit authority | TaskCenter node? |
| ------ | ----------- | -------- | -------------- | ---------------- |
| Explorer | `run_subagent(name="explorer", prompt)` | no | read-only | no |
| Advisor | `ask_advisor(tool_name, tool_payloads, prompt)` | yes | no edits | no |
| Resolver | `ask_resolver(issues_to_resolve)` | yes | may edit | no |

Resolver is called by a verifier or evaluator when it finds issues it cannot
resolve through read-only checks. It returns `resolved` plus summaries to the
calling task.

## State-dependent tool policy

Tool availability depends on:

- complex task request origin,
- task segment continuation chain,
- current harness graph,
- task role,
- task message/tool history.

The runtime composes two layers:

- Soft layer: reminders inject currently relevant constraints.
- Hard layer: prehooks enforce the same constraints before handlers run.

Neither layer mutates the system prompt or dynamically changes tool
registration.

## Tool gating matrix

| Tool | Block when | State source | Soft behavior | Hard behavior |
| ---- | ---------- | ------------ | ------------- | ------------- |
| `submit_partial_plan` | current request already has a prior segment with non-null `continuation_goal` | `TaskSegment.previous_segment_id` walk plus each segment's `continuation_goal` | remind planner that only `submit_full_plan` is allowed | prehook blocks recursive partial plan |
| `submit_full_plan` / `submit_partial_plan` malformed generator graph | duplicate task id, unknown agent name, missing or extra task spec, cycle, dangling dependency, or unknown task ref | handler-level validation | none | handler returns `ToolResult(is_error=True, output=reason)` |
| `request_complex_task_solution` | executor has called any edit tool at least once | agent message history | remind executor after first edit | prehook blocks after edit |
| `submit_evaluation_success` | evaluator has at least five unresolved resolver calls | agent message history | warn at four unresolved resolver calls | prehook blocks success at five |
| `submit_verification_success` | verifier has at least five unresolved resolver calls | agent message history | warn at four unresolved resolver calls | prehook blocks success at five |
| evaluator spawn | any generator in current `HarnessGraph` is not `DONE` | current harness graph task statuses | none | `HarnessGraphOrchestrator` does not spawn evaluator |
| next harness graph after failed graph | `harness_graphs_used >= retry_budget` | current task segment state | none | `TaskSegmentManager` cannot spend retry budget on another graph; it closes the segment failed if the current graph failed |
| failure terminals | never blocked for owning roles | role policy | none | allowed |

## Gate enforcement flow

```
agent calls tool(input)
        |
        v
prehook(tool_input, tool_context)
        |
        +-- reads:
        |     task_center
        |     complex_task_request
        |     task_segment
        |     harness_graph
        |     task role
        |     conversation_messages
        |
        +-- ALLOW -> run handler -> ComplexTaskRequestHandler,
                       TaskSegmentManager, or HarnessGraphOrchestrator
                       observes transition
        |
        +-- BLOCK -> ToolResult(is_error=True, output=reason)
                    agent chooses a different path
```

Soft layer examples:

- First edit detected: `request_complex_task_solution` is now disabled.
- Resolver unresolved count is four: one resolver call remains before success
  is blocked.
- Previous segment already used a partial plan: only `submit_full_plan` is
  permitted.

## Implementation tasks

1. Ensure each tool receives complex task, segment, and harness graph context
   when applicable.
2. Port existing prehooks to read `ComplexTaskRequest`, `TaskSegment`,
   `HarnessGraph`, role, and conversation state.
3. Add malformed generator graph validation to plan submission handlers,
   including task id uniqueness, known agent names, exact `task_specs` coverage,
   and dependency validity.
4. Add recursive partial-plan gating by walking `TaskSegment.previous_segment_id`
   and checking each prior segment's `continuation_goal`.
5. Add `request_complex_task_solution` after-edit gating from message history.
6. Add resolver-count gating for verifier and evaluator success terminals.
7. Keep soft reminders aligned with hard prehook behavior.
8. Add tests for each gate at both notification and enforcement level where
   practical.

## Phase exit criteria

- Every terminal or orchestration request is accepted or rejected from the new
  state model.
- Recursive partial plan is blocked across `TaskSegment` continuation lineage.
- `request_complex_task_solution` is blocked after executor edits.
- Resolver unresolved-count gates still force failure at the limit.
- Malformed plans fail inline without marking the harness graph failed.
