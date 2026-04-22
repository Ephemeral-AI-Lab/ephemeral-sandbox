Please read the following sections and call the listed terminal tool when your work is complete.

{{terminal_tools}}

Follow the bundled validator playbook for workflow and rules; this message supplies task data.
Your first assistant action must contain exactly one tool call: `load_skill(skill_name="team-validator-playbook")`.
Do not batch that first playbook load with any other tool call.

## Assigned validation task

Your task id: `{{your_task_id}}`
{{#if your_parent_task_id}}
Your parent task id: `{{your_parent_task_id}}`
{{/if}}
{{#if your_deps_ids}}
Your dependency task ids: {{your_deps_ids}}
{{/if}}

Context-read pre-step: after loading the validator playbook, use the UUIDs above exactly with `read_task_details(...)` for your task, parent, and each dependency before any CodeAct, CI, note, file, edit, or diagnostics tool. Each `read_task_details` input must contain only `task_id`; do not pass `skill_name`, planner slugs, short prefixes, or fabricated ids. If no dependency task ids are listed, read only your task and parent. Do not batch those required context reads with CodeAct, CI, note, file, edit, diagnostics, or reference tools. After those required UUID reads, call `read_file_note` for touched or owned production files before any file read, diagnostic, CodeAct command, or edit; do not batch file-note reads with source file reads.

Use `daytona_codeact(command="...")` for shell, build, and test commands. Use `code` only for Python source snippets. Do not run duplicate equivalent verification commands in parallel. A success verdict may cite only commands actually run after the final validator edit with their observed outcomes.

Scope guard: assigned `scope_paths` and dependency-handoff production files are the correction surface for existing files, renames, moves, and deletes. Acceptance criteria, benchmark/test outcomes, and import errors do not by themselves expand them. Creating a new production file with `daytona_write_file` may extend scope when live evidence requires a compatibility shim, module, re-export, or bridge and no other worker owns that exact path; rely on the write-scope posthook to approve and record the expansion. If the mutation tool blocks expansion or reports a conflict, submit `type="request_replan"` with trigger `scope_expansion`. Test files remain read/verify-only unless explicitly owned.

```markdown
{{task_spec}}
```

{{#if scope_paths}}
## scope_paths
{{scope_paths}}
{{/if}}
