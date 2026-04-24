# Terminal Submission

Team-mode agents finish by calling a terminal submission tool. The query loop stops after the terminal tool call, and the executor reads structured metadata written by that tool.

## Terminal Tools

- Planners: `submit_plan`
- Replanners: `submit_replan`
- Developers and reviewers: `submit_task_success` or `request_replan`

`submit_plan` and `submit_replan` write `resolved_plan` and `plan_is_replan`. `submit_task_success` and `request_replan` write `task_summary` and `task_summary_type`.
For `submit_plan` and `submit_replan`, every `new_tasks` item carries the full task briefing in `spec`; no separate short `description` label is required.
Planners call `submit_plan(new_tasks=[...])` only; replanners call `submit_replan(new_tasks=[...], cancel_ids=[...])` only. They do not submit free-text `output` or `summary` fields, and terminal submissions do not create file notes.
Developers and reviewers should use `submit_task_success(summary=...)` for evidence-rich terminal summaries and `request_replan(reason=...)` when the lane is blocked or still red.

## Executor Dispatch

The executor maps terminal metadata to one `TaskStatusUpdate`, and
`TaskQueue` hands that update to `TaskCoordinator`:

- `submit_plan(...)` becomes `TaskStatusUpdate(EXPANDED, plan=...)`.
- `submit_replan(...)` becomes `TaskStatusUpdate(EXPANDED, replan=...)`.
- `submit_task_success(summary=...)` becomes `TaskStatusUpdate(DONE, summary=...)`.
- `request_replan(reason=...)` becomes `TaskStatusUpdate(REQUEST_REPLAN, summary=...)`.

Planner and replanner parents with children do not become `done` at submission
time. They move through `expanded`; after all direct children are terminal,
`TaskCoordinator` synthesizes a parent roll-up from child submissions and marks
the parent `done`. Terminal validator summaries are preferred when present;
otherwise the coordinator concatenates terminal non-validator leaf summaries.
