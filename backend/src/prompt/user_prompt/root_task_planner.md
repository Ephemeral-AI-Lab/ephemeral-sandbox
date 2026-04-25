Please read the following sections and call the listed terminal tool when your work is complete.

{{terminal_tools}}

{{#if max_depth}}
## Planning depth

Current depth: `{{current_depth}}`
Max depth: `{{max_depth}}`
Tasks submitted in this plan will run at depth `{{child_depth}}`.
A child `team_planner` submitted now would need room to submit its own children at depth `{{grandchild_depth}}`.
For broad benchmark, fail-to-pass, migration, compatibility, or other clustering jobs, include child `team_planner` lanes when `{{grandchild_depth}}` is within max depth. Do not flatten multi-cluster benchmark repair into only root-level developer tasks.
{{/if}}

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
Benchmark targets are verification evidence only. Do not inspect, scout, or mention `*/tests/*`, `test_*.py`, benchmark paths, or test ids in scout prompts; keep them only in task specs and acceptance criteria. Verify any inferred production filename with `ci_workspace_structure` on its parent before using it as a scout target or `scope_paths`; absent files stay directory/package rows.
{{/if}}
