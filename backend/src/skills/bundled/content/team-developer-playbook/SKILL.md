---
name: team-developer-playbook
description: Authoritative playbook for the developer agent. Read task context, plan, implement, verify, do root cause analysis for red verification, and submit exactly one terminal summary.
---

# Team Developer Playbook

Read the following sections to complete one bounded coding task, then finish with exactly one `submit_task_summary(...)` call.

## Tools

| Purpose | Signature |
|---|---|
| Read a known task by UUID | `read_task_details(task_id="<uuid>")` |
| Read notes for a path | `read_file_note(file_path="...")` |
| Diagnose one file | `ci_diagnostics(file_path="...")` |
| Edit by exact text | `daytona_edit_file(file_path=..., old_text=..., new_text=...)` or `(file_path, edits=[...])` |
| Create or overwrite | `daytona_write_file(file_path=..., content=...)` |
| Rename a Python symbol | `daytona_rename_symbol(old_name=..., new_name=..., kind?=..., file_hint?=...)` |
| Delete file or folder | `daytona_delete_file(file_path=..., is_folder?=false)` |
| Move file or folder | `daytona_move_file(src_path=..., dst_path=..., is_folder?=false)` |
| Run tests or shell | `daytona_codeact(command="...")`; use `code` only for Python source snippets |
| Terminal submission | `submit_task_summary({ type: "success" \| "request_replan", content: string })` |

## Never

1. Do not edit test files unless this task explicitly owns a test-only bug.
2. Do not use `daytona_codeact` for file-content reads, writes, moves, or deletes. Use the Daytona read, search, or mutation tools above.
3. Do not skip, xfail, rewrite verification, change pytest config, install packages, or patch around root/OS permission behavior to turn a command green.
4. Do not call `read_task_graph()`; developers address tasks only via UUIDs from the prompt header.
5. Do not edit through shell redirects, inline Python writes, raw git moves, `sed -i`, `tee`, `cp`, `mv`, or unprefixed file tools.

## Route

```mermaid
flowchart TD
    A["1. Read task details"] --> B["2. Plan"]
    B --> C{"In scope and deps done?"}
    C -- "No" --> R["Stage 6: request_replan"]
    C -- "Yes" --> D["3. Implement"]
    D --> E["4. Verify"]
    E --> F{"Verification green?"}
    F -- "Yes" --> S["Stage 6: success"]
    F -- "No" --> G["5. Root cause analysis"]
    G --> H{"One scoped fix remains?"}
    H -- "Yes" --> D
    H -- "No" --> R
```



## 1. Read task details

Do this before probes, file reads, diagnostics, CodeAct, or edits.

1. Call `read_task_details(task_id="<uuid>")` for your task, parent task, and each dependency id from the prompt header.
2. Use exact UUIDs only. Do not use slugs, short prefixes, scout ids, or fabricated ids. Only the UUIDs exposed in your prompt header and their dependencies are addressable.
3. Treat the task spec, `Initial Plan` / `Initial Replan`, and dependency summaries as the handoff.
4. After those required UUID reads, call `read_file_note(file_path="...")` for each file you expect to touch before any file read, diagnostic, CodeAct command, or edit. Empty notes are valid freshness checks. Do not batch file-note reads with source file reads.

Exit with: objective, acceptance criteria, scope paths, dependency status, expected code files, and file-note freshness.

## 2. Plan

Write a code-focused plan before the first edit:

1. Name the production files and symbols you expect to inspect or change.
2. State the current code behavior that must change.
3. State the intended code behavior after the change.
4. Name the control flow, data flow, import path, config path, or API path involved.
5. List the exact edit boundary: what will change and what will stay untouched.
6. List the exact verification command and diagnostics to run after the edit.

Planning checks:

1. Use failing tests as evidence, not permission to edit tests.
2. Test files are read-only unless the task explicitly owns a test-only bug.
3. New helpers, aliases, public APIs, shims, bridges, re-exports, moves, or modules need production evidence or an explicit assignment. Test spelling alone is not enough.
4. `scope_paths` are the assigned edit surface for existing files, moves, renames, and deletes. Acceptance criteria and test outcomes never expand `scope_paths` by themselves. You may widen reads, diagnostics, and test commands to prove ownership. A new production file may extend scope only through `daytona_write_file` when live evidence proves a missing module, shim, re-export, or bridge and no other worker owns that exact path.
5. For moves, renames, shims, and re-export bridges, check source and destination production evidence separately.
6. If you cannot point from the failing surface to a concrete production path, gather one bounded datum, then decide again.

Submit `type="request_replan"` now if any of these hold:

1. A dependency read in Stage 1 is not `done` or its summary does not hand off the code artifacts this task needs.
2. The next required edit belongs to another role or code path.
3. The next required change is an existing out-of-scope edit, move, rename, or delete.
4. The next required edit is test-only and this task does not explicitly own a test-only bug.
5. The plan requires an unproven missing module, shim, re-export, or helper.
6. The required fix is an out-of-scope test edit, an unproven missing compatibility module, or a new production file whose `daytona_write_file` scope expansion was blocked or conflicted.
7. The required change is too complex or ambiguous for one bounded pass.

Exit with: a concrete in-scope plan, or a terminal replan summary.

## 3. Implement

Make one minimal production change that matches the plan.

1. Before every mutation, verify the target file path, source path, destination path, or rename file hint is inside one assigned `scope_paths` entry. For a new production file required by live evidence, use `daytona_write_file` and let the write-scope posthook approve and record expansion. If an existing-file mutation is outside scope or the posthook blocks expansion, submit `type="request_replan"` with trigger `scope_expansion`.
2. Use exactly one Daytona mutation tool per change (see Tools).
3. Keep each pass small: one behavior fix, import fix, compatibility adjustment, or config correction.
4. Refresh file notes after edits or surprising tool/runtime results.
5. If a delete, move, or rename tool fails, do not retry or bypass it. Preserve the tool error for the terminal summary.
6. Do not create missing modules, shims, re-exports, or bridges unless live production evidence requires them and the file is created through `daytona_write_file`; never create or edit test files outside an explicit test-only task.
7. If a mutation reports an outside-scope or verification-surface warning, pause and re-check the scope and code path before continuing.

Exit with: the smallest scoped edit ready for verification.

## 4. Verify

Prove the latest edit. Do not claim success from stale or partial evidence.

1. Run `ci_diagnostics(file_path="...")` on every edited file before terminal completion.
2. Run the narrowest relevant runtime command after each edit. Keep the originally failing surface until it passes or produces a concrete blocker.
3. For `daytona_codeact(...)`, use `command` for every shell, build, or test command; never pass a shell command string in `code`.
4. Judge runtime pass/fail from the command exit code and failing ids. If pytest exits `4`, collects `0` items, or the named node is missing, treat that as red evidence.
5. Record command, exit code, failing ids, diagnostics, and the shortest useful output snippet. If a command is blocked by policy, submit `type="request_replan"` with trigger `unresolved_blocker` only when no valid equivalent can preserve the needed evidence.

Exit with: green evidence → Stage 6 (`type="success"`); any red, stale, or absent evidence → Stage 5.

## 5. Root cause analysis

Use this section every time verification stays red. The goal is to find the actual code defect, not just the failing symptom. Once the actual root cause is confirmed and in scope, go back to Stage 3 and implement the fix.

Build one trace:

```json
{
  "failing_command": "exact command and exit code",
  "failing_test_or_error": "test id, exception, import error, warning, or assertion",
  "expected_vs_actual": "what the test expected and what the code produced",
  "trace": ["test or command entry", "production call/import/config path", "first wrong value, branch, state, or API result"],
  "root_cause": "specific code defect, statement, branch, config lookup, import, or state transition that explains the failure",
  "fix_location": "file and symbol to change",
  "next_action": "re-implement scoped fix | request_replan"
}
```

Example:

```json
{
  "failing_command": "python -m pytest tests/test_config.py::test_env_override -q --tb=short, exit 1",
  "failing_test_or_error": "test_env_override assertion: expected env value to override default",
  "expected_vs_actual": "expected 'prod'; ConfigLoader returned 'dev'",
  "trace": ["test_env_override", "ConfigLoader.load()", "merge_defaults()", "env value ignored when defaults already contain key"],
  "root_cause": "merge_defaults keeps the default value before checking environment overrides",
  "fix_location": "pkg/config.py::merge_defaults",
  "next_action": "re-implement scoped fix"
}
```

Root-cause checklist — all must hold before you re-enter Stage 3:

1. Capture the exact red command, exit code, failing id, exception/assertion, and the relevant stack frame.
2. State expected vs. actual in code terms: returned value, raised exception, imported symbol, branch taken, persisted state, or emitted output.
3. Follow the stack, import chain, fixture/input path, API call, config lookup, or state transition from the test into production code.
4. Name the first production mechanism that creates the wrong result — exact statement, branch condition, transform, config key lookup, import target, state mutation, persistence write/read, or API contract mismatch. Symptoms ("test failed", "assertion mismatch"), broad areas ("config bug", "bad state"), and guesses ("probably race", "likely missing helper") do not satisfy this step.
5. Confirm the root cause with one bounded datum: traceback frame, diagnostic, focused runtime probe, local source proof, or a before/after value on the traced path.
6. Answer three questions: what value/state/import/branch first became wrong, which code made it wrong, and why that code is incorrect for the expected behavior.
7. Fill the JSON above. If any field is "unknown" or a guess, keep tracing or request replanning — do not re-enter Stage 3 from a shallow trace.

Decision:

1. If the trace identifies one assigned-scope, actionable code defect, return to Stage 3 and implement the smallest fix at `fix_location`.
2. Request replanning when the trace points to another role or code path, scope expansion, tests not assigned to this task, unproven missing modules, environment/runtime mismatch, ambiguous root cause, or tool failure.
3. Stop cycling if the same command stays red after a scoped retry and the trace does not identify a new code defect.

Exit with: actual root cause found and implementation started, or a terminal replan summary with the trace gap.

## 6. Submit terminal summary

Final action must be exactly one:

```ts
submit_task_summary({
  type: "success" | "request_replan",
  content: string
})
```

The `content` field is the entire terminal payload; there is no separate `summary` key.

For `type="success"`, `content` must include:

1. behavior/API change, not just filenames;
2. exact commands run after the final edit and observed outcomes;
3. diagnostics status for edited files;
4. investigation-scope rationale, if reads/probes/tests went outside `scope_paths`;
5. residual risk, if any.

For `type="request_replan"`, `content` must include:

1. replan trigger, exactly one of: `scope_expansion` | `wrong_owner_or_role` | `unresolved_blocker`;
2. the Stage 5 root-cause JSON trace, embedded verbatim inside `content`;
3. last command or diagnostic and failing ids;
4. what decision or code path the replanner must resolve.

Use `scope_expansion` when the required production repair is outside the assigned `scope_paths`. Use `wrong_owner_or_role` when another agent role, dependency, or production owner must act before this task can succeed. Use `unresolved_blocker` when verification, diagnostics, tooling, or root-cause tracing is still blocked but no different owner/scope is proven.

Use `type="success"` only when the latest required verification passed. Use `type="request_replan"` for red, absent, invalid, stale, incomplete, outside-scope, blocked, another-role/code-path, or too-complex verification.
