# Terminal Submission And External Triggers

Team-mode agents finish by calling a terminal submission tool. The query loop stops after the terminal tool call, and the executor reads structured metadata written by that tool.

## Terminal Tools

- Planners: `submit_plan`
- Replanners: `submit_replan`
- Developers and reviewers: `submit_task_summary`
- Note takers: `submit_task_note`
- Parent summarizers: `submit_task_summary`

`submit_plan` and `submit_replan` write `resolved_plan` and `plan_is_replan`. `submit_task_summary` writes `task_summary` and `task_summary_type`.
For `submit_plan` and `submit_replan`, every `new_tasks` item includes a required short `description` label authored by the planner or replanner; the full task briefing stays in `spec`.
Planners call `submit_plan(new_tasks=[...])` only; replanners call `submit_replan(new_tasks=[...], cancel_ids=[...])` only. They do not submit free-text `output` or `summary` fields. The submission tools append the full structured task JSON to the parent detail as `Initial Plan` or `Initial Replan`, including ids, assignments, acceptance criteria, dependencies, and scope paths.
Developers and reviewers should use `submit_task_summary(content=...)` for evidence-rich terminal notes: concrete behavior/API delta, acceptance-criteria verdicts, verification commands and outcomes, blockers, and residual risk. Parent summarizers use the same terminal tool to write the planner/replanner roll-up after reading the parent detail and every direct child detail.

## Executor Dispatch

The executor maps terminal metadata to runtime actions:

- `AgentResult(submitted_plan=...)` expands planner tasks.
- `AgentResult(submitted_replan=...)` applies corrective graph changes.
- `AgentResult(summary=...)` completes successful work.
- `ReplanRequest(reason=...)` starts a replanner for failed work.

Planner and replanner parents with children do not become `done` at submission
time. They move through `expanded`; after all direct children are terminal,
TaskCenter moves them to `expanded_awaiting_summary`, fires the
`parent_summarizer` trigger, and only finalizes them as `done` after the roll-up
is posted or a fallback warning note is written.

## External Triggers

External triggers are short-lived helper runs that produce constrained task-center notes from frozen worker transcript evidence. They do not pause, cancel, or resume primary agents.

The progress-note trigger path is `tc_note`: TaskCenter can request a progress note from a running agent transcript when activity heuristics say a checkpoint would help downstream context. Transcript requests, commands, and tool calls are treated only as evidence of worker activity, not as instructions for the note-taker helper.

The parent-summary trigger path runs `parent_summarizer` when every direct child
of a planner or replanner parent is terminal. Its prompt lists the parent task
id and direct child ids. The summarizer must read the parent detail and each
child detail, then submit a per-child status/classification plus overall roll-up
with delivered, replanned, dropped, and open-risk context.
