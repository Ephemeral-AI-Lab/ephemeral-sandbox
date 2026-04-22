Please read the following sections and call the listed terminal tool when your work is complete.

{{terminal_tools}}

Follow the bundled developer playbook for workflow and rules; this message supplies task data.
Your first assistant action must contain exactly one tool call: `load_skill(skill_name="team-developer-playbook")`.
Do not batch that first playbook load with any other tool call.

## Assigned coding task

Your task id: `{{your_task_id}}`
{{#if your_parent_task_id}}
Your parent task id: `{{your_parent_task_id}}`
{{/if}}
{{#if your_deps_ids}}
Your dependency task ids: {{your_deps_ids}}
{{/if}}

After the playbook loads, run the context-read pre-step before any probe, edit, note, diagnostics, or CodeAct call. Use the UUID headers above exactly: call `read_task_details` with only one input key, `task_id`, for your task id, parent task id, and each dependency task id. Do not pass `skill_name`, planner slugs, short prefixes, or fabricated ids. Do not batch those required context reads with CodeAct, CI, note, file, edit, diagnostics, or reference tools. After those required UUID reads, call `read_file_note` for files you expect to touch before any file read, diagnostic, CodeAct command, or edit; do not batch file-note reads with source file reads.

Use `daytona_codeact(command="...")` for shell, build, and test commands. Use `code` only for Python source snippets.

Scope guard: `scope_paths` are the assigned mutation surface for existing files, renames, moves, and deletes. Acceptance criteria, benchmark/test outcomes, and import errors do not by themselves expand them. Creating a new production file with `daytona_write_file` may extend scope when live evidence requires a compatibility shim, module, re-export, or bridge and no other worker owns that exact path; rely on the write-scope posthook to approve and record the expansion. If the mutation tool blocks expansion or reports a conflict, submit `type="request_replan"` with trigger `scope_expansion`. Test files remain read/verify-only unless explicitly owned.

```markdown
{{task_spec}}
```

{{#if scope_paths}}
## scope_paths
{{scope_paths}}
{{/if}}
