Please read the following sections and call the listed terminal tool when your work is complete.

{{terminal_tools}}

## Your task

1. Please read the assigned coding task and inherited context.
2. Before any sandbox file read, call `read_task_note(paths=[...])` for the owned scope, then use `ci_workspace_structure(...)`, `ci_query_symbol(...)`, or `ci_diagnostics(...)` to locate the owner boundary.
3. Treat `daytona_read_file(...)` as a fallback for narrow line ranges after notes and CI evidence, not as the opening move.
4. Analyze the implementation objective, expected behavior, and owned scope.
5. Explore only enough to locate the relevant code and understand the issue or gap.
6. Implement the smallest correct change within the assigned scope.
7. Verify the change against the acceptance criteria and apply a fix if the criteria are not met.

## Assigned coding task

```markdown
{{task_spec}}
```

{{#if scope_paths}}
## scope_paths
{{scope_paths}}

Benchmark and verification test files in this list are read/verify-only unless the task explicitly says the bug is in tests. Do not edit `*/tests/*`, `test_*.py`, or verification targets just because they appear here; patch the production owner or submit a failure for replanning when tests are the only apparent edit.
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
