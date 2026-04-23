# Terminal Submission And External Triggers

Team-mode agents finish by calling a terminal submission tool. The query loop stops after the terminal tool call, and the executor reads structured metadata written by that tool.

## Terminal Tools

- Planners: `submit_plan`
- Replanners: `submit_replan`
- Developers and reviewers: `submit_task_success` or `request_replan`
- Parent summarizers: `submit_task_success` or `request_replan`

`submit_plan` and `submit_replan` write `resolved_plan` and `plan_is_replan`. `submit_task_success` and `request_replan` write `task_summary` and `task_summary_type`.
For `submit_plan` and `submit_replan`, every `new_tasks` item includes a required short `description` label authored by the planner or replanner; the full task briefing stays in `spec`.
Planners call `submit_plan(new_tasks=[...])` only; replanners call `submit_replan(new_tasks=[...], cancel_ids=[...])` only. They do not submit free-text `output` or `summary` fields. The submission tools append the full structured task JSON to the parent detail as `Initial Plan` or `Initial Replan`, including ids, assignments, acceptance criteria, dependencies, and scope paths.
Developers and reviewers should use `submit_task_success(summary=...)` for evidence-rich completion notes and `request_replan(reason=...)` when the lane is blocked or still red. Parent summarizers use the same two-tool surface to either write the planner/replanner roll-up or replan the summarized parent when child evidence stays unresolved.

## Executor Dispatch

The executor maps terminal metadata to runtime actions:

- `AgentResult(submitted_plan=...)` expands planner tasks.
- `AgentResult(submitted_replan=...)` applies corrective graph changes.
- `AgentResult(summary=...)` completes successful work.
- `ReplanRequest(reason=...)` starts a replanner for failed work.

Planner and replanner parents with children do not become `done` at submission
time. They move through `expanded`; after all direct children are terminal,
TaskCenter moves them to `expanded_awaiting_summary`, injects a dispatchable
`parent_summarizer` sidecar task, and only finalizes them as `done` after the
roll-up is posted.

## Parent Summary Sidecar

The parent-summary path is now a first-class team task, not an external trigger.
When every direct child of a planner or replanner parent is terminal,
TaskCenter creates a READY `parent_summarizer` sidecar with
`fired_by_task_id` pointing at the awaiting-summary parent. The normal executor
runs it with `read_task_details` and one terminal submission tool; successful
submission posts the authoritative parent roll-up and finalizes the parent.
If the summarizer finds unresolved child evidence, it submits
`request_replan(reason=...)` instead; the executor replans the summarized parent
rather than marking it `done`.
