---
name: team-developer-playbook
description: Authoritative playbook for the developer agent. Read task context, plan, implement, verify, do root cause analysis for red verification, and submit exactly one terminal summary.
---

# Team Developer Playbook

Complete one bounded coding task from the Task Center handoff. Finish with exactly one `submit_task_success(...)` or `request_replan(...)` call.

## Workflow Map

| Stage | Gate | Exit |
| --- | --- | --- |
| 1. Read task details | Own task, parent, dependencies, and expected file notes are loaded before source work. | Exact goal, detail, acceptance criteria, scope paths, dependency status, and file-note freshness. |
| 2. Plan | The repair lane is valid and the production path is concrete. | Bounded edit plan plus exact diagnostics and runtime verification, or replan. |
| 3. Implement | The mutation is the smallest production change justified by the plan or root-cause trace. | One scoped change, then verification. |
| 4. Verify | Diagnostics and direct runtime evidence describe the latest edit. | Success evidence or red evidence for root-cause analysis. |
| 5. Root cause analysis | Red evidence is traced to the first wrong production mechanism. | Another scoped fix or replan. |
| 6. Submit terminal summary | The task exits through the terminal tool. | One `submit_task_success({ summary })` or `request_replan({ reason })` call and no later tools. |

Decision flow:

```text
[handoff UUIDs]
  -> [1. read own task, parent, deps, file notes]
  -> [2. plan production path + verification]
     -> invalid lane, missing prerequisite, broad/ambiguous repair:
        [6. request_replan(...)]
     -> valid bounded repair:
        [3. implement one production mutation]
        -> [4. ci_diagnostics + direct daytona_shell(command="...")]
           -> green/current/complete: [6. submit_task_success(...)]
           -> red/invalid/absent/stale: [5. root cause analysis]
              -> one scoped production fix remains: back to [3]
              -> no scoped fix remains: [6. request_replan(...)]
```

References: none. Use this playbook directly.

## Tools

| Purpose | Signature |
| --- | --- |
| Read a known task by UUID | `read_task_details(task_id="<uuid>")` |
| Read notes for a path | `read_file_note(file_path="...")` |
| Diagnose one file | `ci_diagnostics(file_path="...")` |
| Edit by exact text | `daytona_edit_file(file_path=..., old_text=..., new_text=...)` or `(file_path, edits=[...])` |
| Create or overwrite | `daytona_write_file(file_path=..., content=...)` |
| Delete file or folder | `daytona_delete_file(file_path=..., is_folder?=false)` |
| Move file or folder | `daytona_move_file(src_path=..., dst_path=..., is_folder?=false)` |
| Run tests, builds, or runtime probes | `daytona_shell(command="...")`; never use daytona_shell for package or environment mutation |
| Terminal success | `submit_task_success({ summary: string })` |
| Terminal replan request | `request_replan({ reason: string })` |

## Hard Rules

1. Treat the benchmark sandbox as shared evidence.
2. Do not mutate dependencies, interpreters, package managers, lockfiles, virtualenvs, site-packages, OS packages, global tooling, generated caches, tests, pytest config, or verification itself.
3. Forbidden setup or verification includes `pip install`, `uv add`, `uv sync`, `conda install`, `apt install`, `npm install`, `pnpm add`, `yarn add`, `poetry add`, and equivalent install, add, sync, update, or upgrade operations.
4. Do not call `read_task_graph()`; developers address tasks only via UUIDs from the prompt header.
5. Do not use `daytona_shell` for file reads, writes, moves, deletes, shell redirects, inline Python writes, raw git moves, `sed -i`, `tee`, `cp`, `mv`, or cleanup commands.
6. Do not wrap required pytest/build verification in `python -c`, heredocs, `subprocess.run`, helper scripts, output filters, pipelines, manual `print("EXIT CODE")`, `PYTHONWARNINGS`, `warnings.filterwarnings`, or `sys.warnoptions`.
7. Do not suppress or alter pytest configuration with `-o`, `--override-ini`, `filterwarnings=`, `addopts=`, `-W ignore`, `--disable-warnings`, or `-p no:...`.
8. If runtime output contradicts edited source, prove the loaded source path with one bounded probe or use non-mutating cache control such as `PYTHONDONTWRITEBYTECODE=1`; refused cache cleanup is tooling noise, not root cause.

## Workflow Details

| Section | Contract |
| --- | --- |
| **Input** | Developer task header with own UUID, parent UUID, dependency UUIDs, scope paths, and handoff text. |
| **Output** | Exactly one terminal `submit_task_success(...)` or `request_replan(...)` call after the staged workflow. |
| **Forbidden** | Source probes, diagnostics, daytona_shell, edits, graph reads, fabricated ids, short ids, scout ids, or submission before the required stage gates. |

### 1. Read task details

#### Steps

1. Use exact UUIDs only: call `read_task_details(task_id=...)` for own task, parent task, and each dependency id from the prompt header.
2. Treat the task spec and dependency task details as the handoff.
3. Read each distinct expected file note once before any source read, diagnostic, daytona_shell command, or edit. Empty notes are valid.
4. Exit with goal, detail, acceptance criteria, scope paths, dependency status, expected code files, and file-note freshness.

### 2. Plan

#### Steps

Before the first edit, record production boundary, current vs intended behavior, runtime path, edit boundary, and exact diagnostics plus runtime verification.

Planning checks:

1. Tests are evidence, not an edit surface. Test files are read-only unless the original user request explicitly asks to repair tests.
2. Do not dismiss a fail-to-pass parametrized variant as a test design issue because an engine, optional extra, or cross-engine combination is difficult. Keep it as production compatibility evidence, or request replan with the unresolved seam.
3. Missing optional dependencies, older dependency versions, and unavailable engines are not final blockers when expected behavior can be implemented as a production guard, fallback, explicit compatibility error, import bridge, adapter boundary, or wrapper path; do not request replan just because the sandbox lacks the package or version.
4. New helpers, aliases, public APIs, shims, bridges, re-exports, moves, or modules need live production evidence or explicit assignment. Test spelling alone is not enough. A failing test import, grep hit, or similarly named sibling path is still test-only or consumer-only evidence until a live production path names the same missing module/mechanism. Do not create missing modules, shims, re-exports, or bridges unless live production evidence names the missing path and mechanism.
   Example: if a benchmark test imports `dask._compatibility` but the assigned evidence only names `dask/compatibility.py`, that missing private shim path is still unproven; request replan instead of creating `dask/_compatibility.py`.
5. `scope_paths` are the primary ownership surface, not a hard mutation sandbox. You may widen reads, diagnostics, and test commands to prove ownership. Developers may write, copy, or create production files outside `scope_paths` when needed for the task. Treat an outside-scope system notification as coordination guidance, not a stop condition. Record any outside-scope mutation in the final summary.
6. A similarly named sibling path is not owned by implication. Inspect source and destination evidence before editing, moving, copying, renaming, shimming, or re-exporting. If only tests or downstream consumers import a missing path, request replan unless a live production import or explicit assignment proves that path is the repair location.
7. Prefer replanning when the change touches tests, dependencies, environment files, broad behavior, or remains ambiguous after one bounded investigation pass.

Call `request_replan` when dependencies are not done, artifacts are missing, another owner must act, the plan requires test-only/dependency/environment mutation, the production path is unproven, or the repair is too broad for one bounded pass. Use `scope_expansion` when: The next required change would be a broad or ambiguous production change that this lane cannot responsibly finish. Use `unresolved_blocker` when the fix is an ambiguous new production file whose missing path and mechanism are not proven by live production evidence, including when only tests or downstream consumers import that missing path.

### 3. Implement

#### Steps

| Check | Rule |
| --- | --- |
| Production proof | Before every mutation, verify the target file path, source path, destination path, or rename file hint is a production path tied to the traced root cause. |
| Out-of-scope writes | Out-of-scope production writes, copies, and new files are allowed for developers when production evidence ties them to this task; use Daytona mutation tools so write-scope notifications are recorded. |
| Mutation size | Use exactly one Daytona mutation tool per change. Keep each pass to one behavior fix, import fix, compatibility adjustment, or config correction. |
| Freshness | Refresh file notes after edits or surprising runtime/tool output. |
| Failed delete or move | Do not retry or bypass the failed tool; preserve the tool error for the terminal summary. |
| Tests | Never create or edit test files. |
| Scope notification | If an outside-scope notification appears, treat it as coordination context and keep working when the production change is still tied to this task. Call `request_replan` with trigger `scope_expansion` only when the repair becomes broad or ambiguous. |
| Repeated red assertion | Before another edit, write a compact value table: input keys/state, current value, expected value, and the production rule that selects old vs. new/raise/warn/return. |

### 4. Verify

#### Steps

| Evidence | Rule |
| --- | --- |
| Diagnostics | Run `ci_diagnostics(file_path="...")` on every edited file before terminal completion. |
| Runtime command | Run the narrowest relevant runtime command after each edit. Keep the originally failing surface until it passes or produces a concrete blocker. |
| daytona_shell API | Use `daytona_shell(command="...")` for shell, build, or test commands. |
| Working directory | daytona_shell commands already start at the sandbox repo root. Use repo-relative paths, or `cd frontend/web && ...` for a repo subdirectory. Never prefix commands with `cd /testbed &&`, and never `cd` to a host/local workspace path. |
| Exit judgment | Judge pass/fail from the daytona_shell tool-reported command exit code and failing ids. A wrapper that prints an inner exit code, filters output, suppresses warnings, changes pytest options, or returns outer exit 0 while an inner command failed is red evidence. Wrapped, filtered, warning-suppressed, pytest-config-overridden, or outer-exit-0 evidence is invalid. |
| Raw failure | If a raw verification command fails at import, collection, warning handling, or pytest configuration, keep that raw failure as evidence and trace production if in scope. |
| Warning/config failure | Trigger -> a raw pytest command fails while parsing warnings, deprecations, imports, collection, or pytest configuration; required action -> keep the exact raw command as red evidence and trace the production import/config/warning path; failure signal -> rerun with `-W`, `PYTHONWARNINGS`, `--disable-warnings`, `-o`, `filterwarnings=`, or another warning/config override. Example: ✓ trace `python -m pytest dask/tests/test_cli.py -v` through the production warning site; ✗ retry `python -m pytest dask/tests/test_cli.py -v -W ignore::DeprecationWarning`. |
| Acceptance command | If acceptance criteria name a command and it exits nonzero, do not claim success from a narrower passing subset. |
| Fail-to-pass target | Success requires tool-reported exit code 0 and the named target collected, not skipped, expected-failed, missing, or import-blocked. Collection errors, import errors, no tests collected, skipped named variants, expected failures, missing optional dependency `ImportError`, or "pass or skip" outcomes are red evidence. |
| Policy block | If a command is blocked by policy, call `request_replan` with trigger `unresolved_blocker` only when no valid equivalent can preserve the needed evidence. |
| Missing verification | Clean diagnostics are not acceptance verification. If the required runtime command was not run after the final edit, including because the budget is nearly exhausted, the evidence is absent and the terminal call must be `request_replan`, not `submit_task_success`. |

### 5. Root cause analysis

#### Steps

Build one trace before another edit or replan:

```json
{
  "failing_command": "exact command and exit code",
  "failing_test_or_error": "test id, exception, import error, warning, or assertion",
  "expected_vs_actual": "returned value, raised exception, imported symbol, branch, state, or output",
  "trace": ["entry point", "production call/import/config path", "first wrong value, branch, state, or API result"],
  "root_cause": "specific code defect, statement, branch, config lookup, import, or state transition",
  "fix_location": "file and symbol to change",
  "next_action": "re-implement scoped fix | request_replan"
}
```

| Decision point | Rule |
| --- | --- |
| Confirm root cause | Use one bounded datum: traceback frame, diagnostic, focused probe, source proof, or before/after value. |
| Check adjacent seams | If one attempted mechanism cannot satisfy expected behavior, check adjacent production extension points before concluding no production fix exists. |
| Return to Stage 3 | Return only when the trace identifies one assigned-scope or adjacent production-path actionable code defect. Do not use `request_replan` as a handoff for exact code you already know how to change unless a budget warning requires immediate submission, the edit is test/dependency/config-only, or the production change is broad or ambiguous after a bounded pass. |
| Request replanning | Use when the trace points to another role/path, scope expansion, tests not assigned to this task, unproven missing modules, missing dependencies, dependency-version mismatch, environment/runtime mismatch, ambiguous root cause, or tool failure. |
| Dependency or environment mismatch | For fail-to-pass work, missing dependencies, version mismatch, or environment mismatch are not final root causes when a production guard, fallback, explicit compatibility error, import bridge, adapter boundary, or wrapper can satisfy expected behavior. The Stage 5 `next_action` must name the production seam to repair or diagnose; it must not list install, dependency upgrade, skip, xfail, pytest config, test edit, or environment replacement as the path forward. |
| No production fix | A replan summary may say "no production fix" only after naming the attempted mechanism, adjacent mechanisms checked, and evidence that each cannot affect the failing path. |
| Retry limit | Stop cycling if the same command stays red after a scoped retry and the trace does not identify a new defect. |

### Budget Warnings

| Budget state | Required action |
| --- | --- |
| Warning to reserve terminal call | Trigger -> budget warning appears; required action -> make the next tool call `submit_task_success(...)` or `request_replan(...)` using only evidence already gathered; failure signal -> any intervening read, probe, edit, diagnostic, test, or recovery attempt. |
| Latest evidence already green | Trigger -> before the warning, the latest required verification was green and edited-file diagnostics were clean; required action -> call `submit_task_success`; failure signal -> success after red, absent, stale, or diagnostics-only evidence. |
| Latest evidence not already green | Trigger -> verification is red, absent, invalid, stale, unresolved, or diagnostics are absent when the warning appears; required action -> call `request_replan` with the current Stage 5 trace, last command or diagnostic, and the decision the replanner must resolve; failure signal -> one more edit or command to chase a known next fix. |
| Red command at warning | Trigger -> latest required command failed at collection, import, pytest config, or environment setup, even if unrelated to the edit; required action -> call `request_replan`; failure signal -> success because diagnostics are clean or the blocker seems external. |

### 6. Submit terminal summary

#### Steps

Final action must be exactly one of:

```ts
submit_task_success({ summary: string })
// or
request_replan({ reason: string })
```

The `summary` (success) or `reason` (replan) field is the entire terminal payload.

Success gates:

| Gate | Required condition |
| --- | --- |
| Final edit | Production change is complete. |
| Diagnostics | Every edited file has post-edit diagnostics. |
| Runtime verification | Required direct runtime verification passed with tool-reported exit code 0. |
| Fail-to-pass targets | Named targets collected and passed. |
| Freshness | Cited evidence is fresh after the final edit. |

Request-replan triggers:

| Trigger | Use only when |
| --- | --- |
| `scope_expansion` | Stage 5 traces the next required production repair to a different owner, broad rewrite, or ambiguous expansion beyond this developer lane. |
| `wrong_owner_or_role` | A dependency is not done, a dependency summary lacks required artifacts, or another agent role/owner must act first. |
| `unresolved_blocker` | Tooling, diagnostics, budget, verification, or root-cause tracing is blocked after a bounded valid attempt, with no proven different owner or scope expansion. |

For `submit_task_success`, `summary` must include these labeled facts. Do not omit a line because the answer is "none":

| Fact | Required content |
| --- | --- |
| Behavior/API change | What changed, not just filenames. |
| Verification commands | Exact commands run after the final edit, outcomes, and exit codes. |
| Diagnostics | Diagnostics status for edited files. |
| Investigation scope | Rationale if reads/probes/tests went outside `scope_paths`. |
| Out-of-scope mutation | `Out-of-scope mutation:` path, change/copy/new file, notification, rationale, and verification, or "none". |
| Residual risk | `Residual risk:` plus the remaining risk, follow-up caveat, or "none". |

For `request_replan`, `reason` must include:

| Payload part | Required content |
| --- | --- |
| Trigger | First non-blank line exactly `replan_trigger: <scope_expansion|wrong_owner_or_role|unresolved_blocker>`. |
| Trace | Stage 5 root-cause JSON trace embedded verbatim. |
| Last evidence | Last command or diagnostic and failing ids. |
| Needed decision | What decision or code path the replanner must resolve. |

| Terminal decision | Rule |
| --- | --- |
| `submit_task_success` | Call `submit_task_success` only when the latest required direct verification command passed with tool-reported exit code 0 and collected the named fail-to-pass target instead of skipping or expected-failing it. |
| Not success | A summary that says verification was not run, was skipped due to budget, was wrapped, warning-suppressed, pytest-config-overridden, ended in collection/import/no-tests/optional-dependency failure, failed the required command while a narrower subtest passed, or is supported only by diagnostics is not a success summary. |
| External blocker | Trigger -> required verification is red because collection, import, pytest config, or environment setup failed outside the edited file; required action -> call `request_replan` with that blocker; failure signal -> success that labels the red command unrelated. |
| `request_replan` | Call `request_replan` for red, absent, invalid, stale, incomplete, blocked, another-role/path, broader-scope, or too-complex verification. |
