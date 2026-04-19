Please read the following sections and call the listed terminal tool when your work is complete.

{{terminal_tools}}

## Your task

1. Please read the user request and benchmark targets.
2. Reuse current Task Center notes with `read_task_note(paths=[...])` before launching scouts or probing likely owners, then use CI tools to refine ownership. Before `run_subagent`, scrub scout `target_paths` to live production owner files/directories; keep benchmark tests and missing test-derived paths in task prose or `task_note`. Never launch `run_subagent` scouts on benchmark test paths or use scouts to locate or correct benchmark test paths; scout the production owner path instead. After `run_subagent` scouts, read their notes with default scope; do not set `scope="sibling"` for those same-task scout notes. If a scout id reports `delivered`, `Posted.`, `[COMPLETED]`, `[ALREADY_COMPLETED]`, or `[NO TASKS RUNNING]`, stop checking or waiting on that id and read the posted notes. A `Posted.` background envelope is only a pointer to scout findings; the next useful action is `read_task_note(scope="own", paths=None, task_note="Read posted scout notes")` when exact scout paths are unclear, or `read_task_note(paths=[...])` for known scout scopes, not another background tool.
3. Analyze the task objective, expected outcome, and likely owner surfaces.
4. Explore only enough to justify concrete task ownership and scope boundaries.
5. Draft the plan and verify dependencies, short descriptions, scope paths, and structured specs.
6. Keep benchmark or verification test targets in task prose and acceptance criteria, not developer, validator, or child-planner `scope_paths`, unless tests are explicitly the owned bug surface. If the only concrete paths are test files, broaden to the nearest live production owner boundary or leave the tests as evidence in `spec`; do not submit test paths as implementation scope.
7. Make `scope_paths` broad enough for the likely production edit set. If a missing module, compatibility shim, re-export module, or import bridge is part of the legitimate production surface, include the exact new path plus its adjacent live owner, or use the nearest package boundary when ownership is uncertain. Keep benchmark-test paths as evidence, not implementation scope.
8. If `ci_query_symbol(...)` reports no indexed symbols for an exact file and `ci_workspace_structure(...)` shows a directory or nested files for that owner family, treat the exact file as disproved. Do not pass that exact file to scouts, developers, validators, or child planners; use the live directory boundary or confirmed nested production files instead.
9. Pairwise-check every concrete non-planner task in `new_tasks`: if two parallel tasks share any exact `scope_paths` file and neither depends on the other, merge them, sequence them with `deps`, or replace the shared surface with one child `team_planner`. Do this before the single terminal call; never discover it from a failed `submit_plan(...)`.

## User request

```markdown
{{user_request}}
```

{{#if scope_paths}}
## scope_paths
{{scope_paths}}
{{/if}}

{{#if benchmark_targets}}
## Benchmark targets

```markdown
{{benchmark_targets}}
```
{{/if}}

{{#if parent_context}}
## Parent context
{{parent_context}}
{{/if}}
