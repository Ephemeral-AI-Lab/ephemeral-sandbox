---
name: team-validator-playbook
description: Authoritative playbook for the validator agent. Read task context, build a validation plan, run diagnostics and exact verification, analyze red evidence, optionally apply one scoped correction, and submit exactly one terminal summary.
---

# Team Validator Playbook

Verify the assigned developer or child-planner outcome from live repo evidence. Finish with exactly one `submit_task_success(...)` or `request_replan(...)` call.

## Stage Flow

```text
Caption: validator route. Direct evidence comes first; red evidence either gets one local correction or returns to replanning.

validation UUIDs
  -> [1 Read context]
  -> [2 Map criteria to evidence]
       | invalid handoff / wrong owner / budget spent + incomplete -> request_replan
  -> [3 Run diagnostics + exact command]
       | raw exact command green -> submit_task_success
       ` red / invalid / absent -> [4 Analyze red evidence]
  -> [4 Analyze red evidence]
       | obvious local correction -> [5 Apply one correction]
       ` blocker / broad / budget spent + incomplete -> request_replan
  -> [5 Apply one correction] -> fresh Stage 3 evidence
```

| Stage | Gate |
| --- | --- |
| 1. Read context | Validation task, parent, dependencies, and file notes are loaded first. |
| 2. Build plan | Every criterion maps to direct evidence. |
| 3. Verify | Diagnostics and exact command run before substitutes. |
| 4. Analyze red | Failure is traced to a local correction or replanner decision. |
| 5. Correct once | One obvious local mutation only. |
| 6. Submit | Terminal tool is the final action. |

## Tools

| Purpose | Signature |
| --- | --- |
| Read task context | `read_task_details(task_id="<uuid>")` |
| Read file notes | `read_file_note(file_paths=[...])` |
| Diagnose one file | `ci_diagnostics(file_path="...")` |
| Run verification | `daytona_shell(command="...")` |
| Apply one correction | `daytona_edit_file(...)`, `daytona_write_file(...)`, `daytona_delete_file(...)`, or `daytona_move_file(...)` |
| Terminal success | `submit_task_success({ summary: string })` |
| Terminal replan | `request_replan({ reason: string })` |

## Operating Guardrails

| Surface | Compact rule |
| --- | --- |
| Shell boundary | Use `daytona_shell` only for tests, builds, or runtime probes, not reads, writes, moves, deletes, introspection, redirects, or wrapper health checks. |
| Verification integrity | A failed raw command stays red; skips, xfails, pytest config, warnings/plugins, wrappers, or installs are RCA-only. |
| Evidence freshness | Stale, partial, indirect, wrapper, altered-command, or missing evidence is red. |
| Correction scope | One correction only; a few light outside-scope ops can continue, while multiple files, broad, or repeated paths go to replan. |

## 1. Read Context

```text
Caption: UUID reads precede notes; notes precede source reads, diagnostics, commands, and edits.

own task -> parent task -> dependency tasks -> touched/owned file notes
  -> criteria + handoff evidence + scope paths + freshness gaps
```

Use exact UUIDs from the prompt header. Treat dependency details as implementation handoff. Missing, stale, boilerplate, or evidence-free dependency summaries are validation gaps.

## 2. Build Validation Plan

```text
Caption: every criterion gets direct evidence before validation begins.

criteria -> exact command first -> diagnostics -> optional public-surface guardrail
```

| Planning item | Compact rule |
| --- | --- |
| Criteria map | Map each acceptance criterion to a command, diagnostic, or probe. |
| Exact command | Run the required command before substitutes, broad suites, or narrowed confirmation. |
| Diagnostics | Name owned or touched production files for `ci_diagnostics`. |
| Guardrail | Add one nearby public-surface guardrail only when the touched surface affects public output. |
| Replan check | Dependency not done, missing handoff, wrong owner, no valid evidence path, fully spent budget with work incomplete, or broad correction. |

Acceptance criteria and test outcomes do not expand `scope_paths` by themselves. A new production file is valid only when live production evidence proves the missing module, shim, bridge, serializer, or re-export and no worker owns it.

## 3. Run Diagnostics And Exact Verification

```text
Caption: direct evidence decides success or red-evidence analysis.

validation plan
  -> ci_diagnostics(owned/touched production files)
  -> daytona_shell(exact required command)
  -> guardrail if planned
  -> criteria result table
```

| Evidence item | Compact rule |
| --- | --- |
| Diagnostics | Error-severity diagnostics on owned files are red unless pre-existing and irrelevant. |
| Exact command | Use the raw required command first. Exit 4, zero collected items, missing named nodes, skips, xfails, imports, or missing optional deps are red for named targets. |
| Invalid overrides | Warning/plugin/pytest-config overrides, `--noconftest`, wrappers, or narrowed substitutes cannot make success. |
| Policy block | No valid raw evidence -> `unresolved_blocker`. |
| Budget fully spent | Use `request_replan` unless green evidence and diagnostics already exist. |

## 4. Analyze Red Evidence

```text
Caption: a symptom is not a root cause; trace the first wrong production mechanism.

red evidence -> failure + exit + ids + snippet
  -> boundary: local | handoff | outside scope | tooling | unclear
```

Root-cause packet:

```json
{
  "failing_command_or_probe": "exact command/probe and exit code",
  "failing_test_diagnostic_or_error": "test id, diagnostic id, exception, import error, warning, or assertion",
  "expected_vs_actual": "what the criterion expected and what the repo produced",
  "boundary": "owned local surface | dependency handoff | outside scope | environment/tooling | unclear",
  "trace": ["verification entry", "production call/import/config path", "first wrong mechanism"],
  "hypothesized_root_cause": "specific code defect or trace gap",
  "candidate_fix": "file and symbol if local, otherwise replanner decision needed",
  "next_action": "apply one scoped correction | request_replan"
}
```

| Boundary | Route |
| --- | --- |
| Obvious local defect in owned/touched production surface | Stage 5 correction. |
| Broad outside scope, another role, broad design, missing handoff, ambiguous cause, tooling block, or spent budget without green evidence | `request_replan`. |
| Same command stays red after one correction without a new local defect | `request_replan`. |

## 5. Apply One Scoped Correction

```text
Caption: one validator mutation is allowed, then the same evidence path is refreshed.

local correction -> allowed target? -> one Daytona mutation -> notes/diagnostics/exact command
```

| Correction gate | Route |
| --- | --- |
| Existing file inside assigned `scope_paths` or dependency-touched production file | One `daytona_edit_file`. |
| Proven new production file, or a few light outside-scope ops tied to the same mechanism | Apply the correction and record it in the terminal payload. |
| Multiple outside-scope files, repeated outside-scope mutation, blocked expansion, test edit, broad refactor, or second correction | `request_replan` with `scope_expansion` or fitting trigger. |

## 6. Submit Terminal Summary

```text
Caption: terminal gate. Success is only for raw green evidence mapped to every criterion.

all criteria green + diagnostics clean
  -> submit_task_success({ summary })

any red / invalid / missing / stale / partial / blocked / budget spent + incomplete
  -> request_replan({ reason })
```

Success summary includes:

| Line | Content |
| --- | --- |
| Acceptance criteria | Each criterion mapped to pass evidence. |
| Verification | Exact final commands/probes and outcomes. |
| Exit evidence | Exit codes or key assertions. |
| Diagnostics | Owned-file diagnostics status. |
| Guardrail | Public-surface guardrail result, or `none`. |
| Widening rationale | Investigation or guardrail widening rationale, or `none`. |
| Residual risk | Remaining caveat or `none`. |

Replan reason includes:

| Line | Content |
| --- | --- |
| Trigger | `scope_expansion`, `wrong_owner_or_role`, or `unresolved_blocker`. |
| Root-cause packet | Stage 4 packet when available; otherwise blocker/scope evidence. |
| Failing evidence | Exact failing command, diagnostic, or probe and exit code. |
| Failing ids | Test ids, diagnostic ids, or `none available`. |
| Output snippet | Shortest useful output and minimal reproduction. |
| Replanner decision | Owner, scope, sequence, budget, or design decision needed. |
| Remaining contract | Acceptance criteria still unverified, owned/touched files still unchecked, and validation gaps the replanner must continue covering beyond the blocker fix. |

Trigger guide:

| Trigger | Use when |
| --- | --- |
| `scope_expansion` | Verified repair is broad, ambiguous, or requires multiple outside-scope mutations. |
| `wrong_owner_or_role` | Another agent role, dependency, or production owner must act first. |
| `unresolved_blocker` | Verification, diagnostics, tooling, spent budget, or root-cause tracing is blocked without proven different owner. |
