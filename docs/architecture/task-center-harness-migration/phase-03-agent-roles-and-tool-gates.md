# Phase 03 - Agent Roles and Tool Gates

## Goal

Port role semantics and terminal-tool gating onto the new graph and Attempt
state model.

This phase should preserve the public agent contract while changing where
state is read from.

## Role model

TaskCenter owns three main agent roles, all scoped to one Attempt of one
`HarnessGraph`.

| Role | Scope | Terminals |
| ---- | ----- | --------- |
| Planner | one Attempt | `submit_full_plan`, `submit_partial_plan` |
| Generator executor | one Attempt DAG node | `submit_execution_success`, `submit_execution_failure`, `submit_request_plan` |
| Generator verifier | one Attempt DAG node | `submit_verification_success`, `submit_verification_failure` |
| Evaluator | sink for one Attempt | `submit_evaluation_success`, `submit_evaluation_failure` |

Planner has no failure terminal. Executor, verifier, and evaluator are the
only roles that can declare failure.

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

- graph lineage,
- current Attempt,
- task role,
- task message/tool history.

The runtime composes two layers:

- Soft layer: reminders inject currently relevant constraints.
- Hard layer: prehooks enforce the same constraints before handlers run.

Neither layer mutates the system prompt or dynamically changes tool
registration.

## Tool gating matrix

| Terminal | Block when | State source | Soft behavior | Hard behavior |
| -------- | ---------- | ------------ | ------------- | ------------- |
| `submit_partial_plan` | planner's `prior_graph_id` chain already contains `plan_shape = partial` | local graph plus `prior_graph_id` walk | remind planner that only `submit_full_plan` is allowed | prehook blocks recursive partial plan |
| `submit_full_plan` / `submit_partial_plan` malformed DAG | cycle, dangling edge, or unknown task ref | handler-level validation | none | handler returns `ToolResult(is_error=True, output=reason)` |
| `submit_request_plan` | executor has called any edit tool at least once | agent message history | remind executor after first edit | prehook blocks after edit |
| `submit_evaluation_success` | evaluator has at least five unresolved resolver calls | agent message history | warn at four unresolved resolver calls | prehook blocks success at five |
| `submit_verification_success` | verifier has at least five unresolved resolver calls | agent message history | warn at four unresolved resolver calls | prehook blocks success at five |
| evaluator spawn | any generator in current Attempt is not `DONE` | current Attempt task statuses | none | orchestrator does not spawn evaluator |
| next-Attempt spawn | `attempts_used >= retry_budget` | local graph state | none | orchestrator closes graph failed |
| failure terminals | never blocked for owning roles | role policy | none | allowed |

## Gate enforcement flow

```
agent calls submit_<terminal>(input)
        |
        v
prehook(tool_input, tool_context)
        |
        +-- reads:
        |     task_center
        |     harness_graph
        |     attempt
        |     task role
        |     conversation_messages
        |
        +-- ALLOW -> run terminal handler -> local orchestrator observes transition
        |
        +-- BLOCK -> ToolResult(is_error=True, output=reason)
                    agent chooses a different path
```

Soft layer examples:

- First edit detected: `submit_request_plan` is now disabled.
- Resolver unresolved count is four: one resolver call remains before success
  is blocked.
- `prior_graph_id` chain contains a partial plan: only `submit_full_plan` is
  permitted.

## Implementation tasks

1. Ensure each terminal tool receives graph and Attempt context.
2. Port existing prehooks to read `HarnessGraph`, `Attempt`, role, and
   conversation state.
3. Add malformed DAG validation to plan submission handlers.
4. Add recursive partial-plan gating by walking `prior_graph_id`.
5. Add `submit_request_plan` after-edit gating from message history.
6. Add resolver-count gating for verifier and evaluator success terminals.
7. Keep soft reminders aligned with hard prehook behavior.
8. Add tests for each gate at both notification and enforcement level where
   practical.

## Phase exit criteria

- Every terminal is accepted or rejected from the new state model.
- Recursive partial plan is blocked across `CONTINUE_AFTER_PARTIAL_PLAN`
  lineage and reset across `REQUEST_PLAN`.
- `submit_request_plan` is blocked after executor edits.
- Resolver unresolved-count gates still force failure at the limit.
- Malformed plans fail inline without marking the Attempt failed.
