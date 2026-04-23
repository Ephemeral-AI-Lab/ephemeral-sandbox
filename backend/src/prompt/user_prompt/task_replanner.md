Please read the following sections and call the listed terminal tool when your work is complete.

{{terminal_tools}}

Follow the bundled team-replanner playbook for workflow and rules; this message supplies task data.
Your first assistant action must contain exactly one tool call: `load_skill(skill_name="team-replanner-playbook")`.
Do not batch that first playbook load with any other tool call.

## Assigned replanning task

Your task id: `{{your_task_id}}`
{{#if your_parent_task_id}}
Your parent task id: `{{your_parent_task_id}}`
{{/if}}
{{#if your_failed_task_id}}
Failed task id: `{{your_failed_task_id}}`
{{/if}}
{{#if your_deps_ids}}
Your dependency task ids: {{your_deps_ids}}
{{/if}}

Context-read pre-step: after loading the replanner playbook, use the UUIDs above exactly with `read_task_details(...)` for your task, parent, failed task, and each dependency, then call `read_task_graph()` to enumerate siblings before CI, notes, diagnosis, corrective planning, or `submit_replan(...)`. Each `read_task_details` input must contain only `task_id`; do not pass `skill_name`, planner slugs, short prefixes, or fabricated ids. Do not batch those required context reads with daytona_shell, CI, note, file, edit, diagnostics, reference, or submission tools.

Benchmark tests are evidence only. Do not create `new_tasks` that own, edit, skip, xfail, rewrite, or reconfigure tests, benchmark harness files, or pytest configuration unless the original user request explicitly asks to repair tests rather than production behavior. Put test paths only in acceptance criteria commands.

Account for every named failing variant in the failed task summary. Do not close a variant by calling it a test design issue, unsupported combination, out of scope, residual risk, or broadly covered by a validator unless a new repair/diagnostic task or an explicitly identified live repair owner actually covers the production seam for that variant.

If the failed task proposes a concrete code rule or one-line fix, verify that rule against every observed expected/actual value in the same failing assertion before using `Diagnostics decision: trivial_direct_replan`. If the rule fixes one value but contradicts another, create diagnostic repair work to derive the correct production rule instead of copying the proposed fix.

The failed task id listed above is the original `request_replan` task. It must never appear in `cancel_ids`. Before submitting, compare each `cancel_ids` entry against that failed task id and remove it. Always include the top-level `cancel_ids` key in `submit_replan`; use `cancel_ids=[]` when no sibling should be cancelled. Do not call `submit_replan` until you have loaded the action reference matching your cancellation decision and then loaded `terminal-contract`; if a validation error still rejects `cancel_ids`, treat that as feedback, remove the rejected id, and submit a corrected payload.

```markdown
{{task_spec}}
```

{{#if scope_paths}}
## scope_paths
{{scope_paths}}
{{/if}}
