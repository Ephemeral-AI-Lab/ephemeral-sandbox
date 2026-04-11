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
- Must keep roles separate: planner plans, developer edits, validator verifies, replanner reshapes work.
- Must trust live CI and live file state over cached briefs or old reasoning.
- Must preserve exact file paths and exact pytest node ids when they are known.
- Must treat benchmark test files as failure evidence first, not default implementation ownership.
- Must treat collection or import failures before the named target loads as still-red verification, not as a reason to trim the scope.

## Benchmark planning rules

- Fresh benchmark roots must stay live-first. Must start with a narrow owner-surface pass before broad exploration.
- Planners must load `team-planner-playbook/exploration-script` before the first non-reference planning tool call on a fresh benchmark root when `load_skill_reference` is available.
- After the root anchor, planners must execute at least one scout wave on unresolved production-owner slices before loading final-plan references or emitting the root DAG.
- Planners must load `team-planner-playbook/task-planning-decomposition` immediately before finalizing the root DAG when `load_skill_reference` is available.
- Child or scoped benchmark planning must load `team-planner-playbook/non-root-context-reuse` before fresh exploration when `load_skill_reference` is available.
- Must anchor `owned_files`, `owned_failures`, and verification commands on exact live paths. Never keep guessed aliases such as `compat.py` when live structure shows `compatibility.py`.
- Must stop planning once ownership is clear enough to emit the next plan layer. Never keep scouting after sufficiency.

## Benchmark execution rules

- Developers must start from the exact failing command or exact named retry target.
- Developers must keep product-code fixes on the real owner surface first. Never patch unowned tests or runner config just because they fail first.
- Validators must start with the exact retry target. After one broader same-surface check, they must stop.
- Validators must report exact failing ids and exact snippets. Never explain failures away.
- Replanners must treat validator evidence plus one live owner confirmation as enough to act. Never reopen broad source archaeology once the corrective owner is clear.

## Mandatory benchmark references

- Replanners on resume or corrective turns where the validator packet already names exact failing pytest ids plus exact existing owner files must load `team-replanner-playbook/corrective-fast-path` before deeper analysis when `load_skill_reference` is available.

## Cross-surface guardrails

- When a change affects public serialization, schema shape, or docs-visible output, developers and validators must run one nearby same-surface guardrail in addition to the originally failing test.

## Observability note

- When debugging runtime, coordination, retry, or checkpoint behavior, benchmark logs under `.ephemeralos/benchmark-logs/` are supporting evidence only. Live workspace state and current command output still win.
