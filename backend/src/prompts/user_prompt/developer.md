Please read the following sections and call the listed terminal tool when your work is complete.

{{terminal_tools}}

## Your task

1. Please read the assigned coding task and inherited context.
2. Before any sandbox file read, call `read_task_note(paths=[...])` for the owned scope, then use `ci_workspace_structure(...)`, `ci_query_symbol(...)`, or `ci_diagnostics(...)` to locate the owner boundary.
3. Treat `daytona_read_file(...)` as a fallback for narrow line ranges after notes and CI evidence, not as the opening move.
4. Analyze the implementation objective, expected behavior, and owned scope.
5. Explore only enough to locate the relevant code and understand the issue or gap.
6. Implement the smallest correct change, using `scope_paths` as the default coordination surface. If live evidence shows one adjacent production owner outside `scope_paths` is required for the same bug, you may widen to that file deliberately and mention the widened path in your terminal summary.
7. Verify the change against the acceptance criteria and apply a fix if the criteria are not met.
8. Do not spend the final tool call on inspection, CodeAct, diagnostics, cleanup, or another edit. If a budget warning appears and you cannot finish verification while reserving one call for `submit_task_summary(...)`, submit `type="request_replan"` with the current evidence now.
9. Never use `daytona_codeact` for path moves or git-index mutation tokens such as `mv`, `shutil.move`, `os.rename`, `git rm`, or `git mv`; use `daytona_move_file` for repo path moves. Pure removals such as `rm`, `unlink`, `os.remove`, `Path.unlink`, and `shutil.rmtree` may run through CodeAct because the overlay audit path converts tracked removals into OCC-gated deletes and rejects unsupported removal shapes. If `daytona_delete_file` or `daytona_move_file` fails, do not retry the same delete/move tool; submit a failure with the tool error.
10. Use repo-relative paths or `/testbed/...` sandbox paths in Daytona and CI tools. Never pass host workspace paths such as `/Users/...` into sandbox tools, and never search host directories from CodeAct.
11. If any tool result warns about `outside write_scope`, treat it as a coordination warning, not a hard failure: refresh notes, confirm the edit still belongs to the same bug and does not collide with sibling work, then either continue with the widened production owner or submit `submit_task_summary(type="request_replan", content=...)` if the task needs a different owner, multiple unrelated owners, sequencing, or explicit test-file authorization. If a tool reports `verification-surface write allowed`, revert or avoid the test edit unless the task explicitly owns a test-only bug. If any test, CodeAct, or diagnostic output shows `ModuleNotFoundError`, `ImportError`, or collection failure naming a missing module outside `scope_paths`, you may create or edit the missing production path only when live non-test evidence or the assigned objective shows it is the intended repository surface; otherwise summarize the missing-path evidence for replanning. For path moves, file renames, shims, and re-exports, check both source and destination and proceed only when the destination is a justified production owner, not just a benchmark-test spelling.
12. End this lane with exactly one `submit_task_summary(...)` call. If verification is incomplete, the tool budget is low, the owner is wrong, or the task is still red, call `submit_task_summary(type="request_replan", content=...)` with the evidence instead of continuing without a terminal submission.

## Assigned coding task

```markdown
{{task_spec}}
```

{{#if scope_paths}}
## scope_paths
{{scope_paths}}

Benchmark and verification test files in this list are read/verify-only unless the task explicitly says the bug is in tests. Do not edit `*/tests/*`, `test_*.py`, or verification targets just because they appear here; patch the production owner or submit a failure for replanning when tests are the only apparent edit.
If live evidence identifies a missing module, compatibility shim, re-export, import bridge, or production owner outside this list, treat it as a widened edit decision. Proceed only when the path is an adjacent production owner for the same objective and sibling notes do not show a conflict; otherwise submit `submit_task_summary(type="request_replan", content=...)` with the path and evidence so replanning can widen or resequence the task.
If verification fails with `ModuleNotFoundError`, `ImportError`, or collection failure for a module outside `scope_paths`, use live production evidence to decide whether the missing path is the intended repository surface. If it is, a coordinated write may proceed even though the path is outside scope; if the only evidence is benchmark-test spelling and no production owner is known, submit a failure summary with the missing module and command output.
For file moves/renames, compatibility shims, and re-export bridges, check both endpoints. A source path inside `scope_paths` does not by itself authorize an absent destination outside `scope_paths`; the destination needs live production evidence or clear objective ownership before calling `daytona_move_file(...)`, `daytona_write_file(...)`, or `daytona_edit_file(...)`.
Before any `daytona_write_file(...)` or `daytona_edit_file(...)`, compare the target file to `scope_paths`. If it is outside scope, make the widened-edit decision explicitly, refresh notes when needed, and include the widened path and rationale in the terminal summary. Replan only when the widened path changes ownership or coordination materially.
If a Daytona tool emits an `outside write_scope` warning, treat the packet as observability evidence. Do not claim success without naming the widened path and verification; do not keep widening across unrelated owner surfaces.
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
