---
name: sweevo-project-context
description: Stable SWE-EVO benchmark rules shared by planner, developer, validator, and replanner agents.
---

# SWE-EVO Project Context

Use this skill only for stable benchmark policy. Must treat the prompt, payload, live checkout, and named tests as the contract for the current run.

## Shared rules

- Must treat the live sandbox checkout as the source of truth.
- Must treat named `FAIL_TO_PASS`, `PASS_TO_PASS`, and grading commands as authoritative.
- Must report a missing named test or node as `benchmark_surface_mismatch`. Never guess a replacement node, file, or symbol.
- Must not label a missing transitive import, helper, or adjacent production module as `benchmark_surface_mismatch` when the prompt-named benchmark files still exist live; that is fixable runtime evidence on the current repository surface.
- Must keep commands repo-root-relative. Never prepend guessed `cd /workspace`, `cd /home/user`, or similar wrappers.
- Must fix repository code, not the ambient environment. Never rely on ad hoc package installs as the benchmark fix.
- Must keep roles separate, use the upgraded CI toolkit (`ci_workspace_structure`, `ci_query_symbols`, `ci_query_references`, `ci_hover`, `ci_diagnostics`) for live ownership evidence, and trust live file state over cached briefs or old reasoning.
- Must preserve exact file paths and exact pytest node ids when they are known.
- Must treat benchmark test files as failure evidence first, not default implementation ownership.
- Must treat collection or import failures before the named target loads as still-red verification, not as a reason to trim the scope.
- If benchmark-path validation fails, must rebuild a prompt-surface ledger from the exact `FAIL_TO_PASS`/`PASS_TO_PASS` text and copy from that ledger verbatim; keep named nodes as-is or downgrade only to that same prompt file path. Never repair benchmark paths by filename intuition or by swapping in a same-family sibling node.

## Benchmark planning rules

- Fresh benchmark roots must stay live-first. Must start with a narrow owner-surface pass before broad exploration.
- On a fresh benchmark root, any plan JSON drafted before one production anchor and one scout wave is invalid, even if the JSON shape looks plausible.
- Planners must load `team-planner-playbook/exploration-script` before the first non-reference planning tool call on a fresh benchmark root when `load_skill_reference` is available; no `ci_workspace_structure(...)` or test-side scan is valid first.
- After the root anchor, planners must execute at least one scout wave on unresolved production-owner slices before loading final-plan references or emitting the root DAG; scouts must `post_note` findings and planners must `read_notes` before decomposition or duplicate scouting.
- Planners must load `team-planner-playbook/task-planning-decomposition` immediately before finalizing the root DAG when `load_skill_reference` is available.
- Child or scoped benchmark planning must load `team-planner-playbook/non-root-context-reuse` before fresh exploration when `load_skill_reference` is available.
- Must treat skipped references, early test-file census, optional-dependency guessing, and repeated source-symbol queries before the first scout wave as planning drift. Reset to `team-planner-playbook/exploration-script`, then one production anchor and one scout wave.
- Must not draft placeholder scout lanes, `plan-anchor-*` work items, or `developer_override` escape hatches into the submitted DAG. Scouts happen through tools; the plan names only real `developer`, `validator`, or expandable `team_planner` lanes.
- When one dominant production subtree and several scattered sibling families coexist, anchor inside the dominant subtree first, then branch to sibling production modules. Never open a second sibling anchor or a test-side status packet before the scout wave.
- Must anchor `scope_paths` on exact live owner paths and keep verification text on exact benchmark paths. Never keep guessed aliases such as `compat.py` when live structure shows `compatibility.py`, or shorten `pkg/dataframe/utils.py` to `pkg/utils.py` once the live owner is known.
- Must stop planning once ownership is clear enough to emit the next plan layer. Never keep scouting after sufficiency.

## Benchmark execution rules

- Developers must start from the exact failing command or exact named retry target. If the payload owns only one or a few exact pytest nodes, reproduce and re-verify those exact nodes before any broader same-file sweep.
- Developers must keep product-code fixes on the real owner surface first, use the current role-appropriate read tool (`daytona_read_file` for worker file reads), `read_notes` before widening into a shared chain, and `post_note` blockers that siblings or replanners need. Never patch unowned tests or runner config just because they fail first.
- Developers and validators must treat `shell(...)["exit_code"]` as the command verdict. Wrapper success or manifest output does not turn a killed or timed-out shell run green.
- Validators must start with the exact retry target. After one broader same-surface check, they must stop.
- Validators must report exact failing ids and exact snippets. Never explain failures away.
- Replanners must `read_notes` on the failing scope before new archaeology, then treat validator evidence plus one live owner confirmation as enough to act. Never reopen broad source archaeology once the corrective owner is clear.

## Mandatory benchmark references

- Replanners on resume or corrective turns where the validator packet already names exact failing pytest ids plus exact existing owner files must load `team-replanner-playbook/corrective-fast-path` before deeper analysis when `load_skill_reference` is available.

## Cross-surface guardrails

- When a change affects public serialization, schema shape, or docs-visible output, developers and validators must run one nearby same-surface guardrail in addition to the originally failing test.

## Observability note

- When debugging runtime, coordination, retry, or checkpoint behavior, benchmark logs under `.ephemeralos/benchmark-logs/` are supporting evidence only. Live workspace state and current command output still win.
- Prefer structured events that name tokens, tool usage, context, compactions, checkpoint ids, resume source, retries, and replans when those logs exist.
