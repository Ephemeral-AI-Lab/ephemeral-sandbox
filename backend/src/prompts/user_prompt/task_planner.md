Please read the following sections and call the listed terminal tool when your work is complete.

{{terminal_tools}}

Your task id: `{{your_task_id}}` — pass this exact id to `read_task_details(task_id=...)` when you need scout notes, parent plan, or your own inherited context.
{{#if your_deps_ids}}
Your dependency task ids: {{your_deps_ids}} — call `read_task_details(task_id=<dep>)` on each dep to load its hand-off.
{{/if}}
{{#if your_parent_task_id}}
Your parent task id: `{{your_parent_task_id}}` — call `read_task_details(task_id=...)` on the parent for the full parent plan, sibling scope, or coordination notes.
{{/if}}

## Your task

1. Please read the assigned planner task and inherited context.
2. Reuse the assigned task, parent context, and current Task Center notes first. If those already name concrete owner files and a child-lane split, skip scouts and shape the DAG from that inherited evidence. Use `read_file_note(file_path="...")` for known noted files, and use CI or scouts only for missing or contradictory owner boundaries. Before `run_subagent`, scrub scout `target_paths` to live production owner files/directories; keep benchmark tests and missing test-derived paths in task prose or `task_note`. Never launch `run_subagent` scouts on benchmark test paths or use scouts to locate or correct benchmark test paths; scout the production owner path instead. After `run_subagent` scouts, read their notes on the current task with `read_task_details(task_id="<your current task id>")`; do not pass `bg_*` background ids to `read_task_details`. If a scout id reports `delivered`, `Posted.`, `[COMPLETED]`, `[ALREADY_COMPLETED]`, or `[NO TASKS RUNNING]`, stop checking or waiting on that id and read the posted notes. A `Posted.` background envelope is only a pointer to scout findings; the next useful action is `read_task_details(task_id="<your current task id>")` when exact scout paths are unclear, or `read_file_note(file_path="...")` for known scout scopes, not another background tool. If scout notes conflict with CI but the owner split is defensible, submit with uncertainty instead of launching another scout wave.
3. Analyze the subtask objective, expected outcome, and remaining uncertainty.
4. Explore only enough to justify missing child task ownership and scope boundaries.
5. Draft the child plan and verify dependencies, short descriptions, scope paths, and structured specs.
6. Keep benchmark or verification test targets in task prose and acceptance criteria, not developer, validator, or child-planner `scope_paths`, unless tests are explicitly the owned bug surface. If the only concrete paths are test files, broaden to the nearest live production owner boundary or leave the tests as evidence in `spec`; do not submit test paths as implementation scope.
7. Make `scope_paths` broad enough for the likely production edit set. If a missing module, compatibility shim, re-export module, or import bridge is part of the legitimate production surface, include the exact new path plus its adjacent live owner, or use the nearest package boundary when ownership is uncertain. Keep benchmark-test paths as evidence, not implementation scope.
8. If `ci_query_symbol(...)` reports no indexed symbols for an exact file and `ci_workspace_structure(...)` shows a directory or nested files for that owner family, treat the exact file as disproved. Do not pass that exact file to scouts, developers, validators, or child planners; use the live directory boundary or confirmed nested production files instead.
9. Before the terminal `submit_plan(...)` call, self-check the payload once. Validation errors on the terminal call count as a bad post call; include required descriptions, structured specs, top-level dependencies for real sequencing needs, and terminal validator coverage. Always include at least one terminal `validator` task when the plan has non-validator tasks; use one validator by default, never include more than 2 terminal validators, and make their top-level `deps` cover every same-layer non-validator task.
10. Do not add dependencies merely because tasks belong to the same benchmark, mention adjacent files, or have overlapping `scope_paths`. Use `deps` only when one task genuinely needs another task's output, when the same exact file has a known edit-order dependency, or when unresolved ownership should be delegated to one child `team_planner`.
11. The terminal `submit_plan(...)` call takes only `new_tasks`. Do not author a prose summary — the system generates the outcome summary automatically once your children complete. Focus your energy on structuring the task split, owner evidence, dependency shape, validator coverage, and scope boundaries inside each task's `description` and `spec`.

## Assigned planner task

```markdown
{{task_spec}}
```

{{#if scope_paths}}
## scope_paths
{{scope_paths}}
{{/if}}

{{#if context_from_dependencies}}
## Context from dependencies
{{context_from_dependencies}}

{{/if}}
{{#if recent_scope_changes}}
## Recent changes in your scope
{{recent_scope_changes}}

{{/if}}
{{#if parent_context}}
## Parent context
{{parent_context}}
{{/if}}
