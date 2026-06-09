---
intent: read_only
terminal: true
hooks: [no_background_sessions, {disallow_nested_planner_deferral: {max_depth: 1}}, advisor_approval]
---
Terminate the planner run by submitting this attempt's worker plan.

Inputs:
- `plan_spec`: nonblank attempt-level explanation.
- `work_items`: nonempty list of `{ id, agent_name, work_spec, needs }`.
- `deferred_goal_for_next_iteration`: optional nonblank goal carried to the next iteration.

Rules:
- `id` is planner-authored and unique within the plan.
- `agent_name` must resolve to a worker-capable agent profile.
- `work_spec` is the worker's executable instruction.
- `needs` names direct work item dependencies by work item id.
- The dependency graph must be acyclic.

Behavior:
- Records `TaskOutcome::Planner { plan_spec, work_items, deferred_goal_for_next_iteration }`.
- Materializes `AttemptExecutionTree.nodes` from the work items.
