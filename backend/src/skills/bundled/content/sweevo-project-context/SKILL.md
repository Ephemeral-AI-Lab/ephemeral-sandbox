---
name: sweevo-project-context
description: Stable SWE-EVO benchmark rules shared by planner, developer, validator, and replanner agents.
---

# SWE-EVO Project Context

Use this skill only for stable benchmark policy. Treat the prompt, payload, live checkout, and named tests as the contract for the current run.

## Shared rules

- Treat the live sandbox checkout as the source of truth.
- Treat named `FAIL_TO_PASS`, `PASS_TO_PASS`, and grading commands as authoritative.
- Report a missing named test or node as `benchmark_surface_mismatch`. Never guess a replacement node, file, or symbol.
- Keep commands repo-root-relative. Never prepend guessed `cd /workspace`, `cd /home/user`, or similar wrappers.
- Fix repository code, not the ambient environment. Never rely on ad hoc package installs as the benchmark fix.
- Keep roles separate, use the upgraded CI toolkit (`ci_status`, `ci_workspace_structure`, `ci_query_symbols`, `ci_query_references`, `ci_hover`, `ci_diagnostics`) for live ownership evidence, and trust live file state over cached briefs or old reasoning.
- Preserve exact file paths and exact pytest node ids when they are known.
- Treat benchmark test files as failure evidence first, not default implementation ownership.
- Treat collection or import failures before the named target loads as still-red verification, not as a reason to trim the scope.

## Benchmark planning rules

- Fresh benchmark roots must stay live-first. Start with a narrow owner-surface pass before broad exploration.
- Any plan JSON drafted before one production anchor and one explorer wave is invalid, even if the JSON shape looks plausible.
- Planners must load `team-planner-playbook/exploration-script` before the first non-reference planning tool call on a fresh root.
- After the root anchor, planners must execute at least one explorer wave on unresolved production-owner slices before loading final-plan references or emitting the root DAG; explorers must `post_note` findings and planners must `read_notes` before decomposition or duplicate scouting.
- If `ci_status()` reports `initialized=false` or the first anchor is empty, planners must stop exact-file guessing and launch the first wave on stable production boundaries that explorers can confirm live.
- Child or scoped benchmark planning must load `team-planner-playbook/non-root-context-reuse` before fresh exploration.
- Must not draft placeholder scout lanes, `plan-anchor-*` work items, or `developer_override` escape hatches into the submitted DAG.
- Must anchor `scope_paths` on exact live owner paths and keep verification text on exact benchmark paths. Never keep guessed aliases once the live owner is known.
- Stop planning once ownership is clear enough to emit the next plan layer.

## Benchmark execution rules

- Developers must start from the exact failing command or exact named retry target.
- Developers must keep product-code fixes on the real owner surface first, use the current role-appropriate read tool, `read_notes` before widening into a shared chain, and `post_note` blockers that siblings or replanners need.
- If a scope-change warning or listener notification arrives mid-flight, developers, validators, and replanners must treat it as a freshness signal: refresh with `read_notes(...)`, then use `context_changed_since()` before committing or re-verifying.
- Developers and validators must treat `shell(...)["exit_code"]` as the command verdict. Wrapper success or manifest output does not turn a killed or timed-out shell run green.
- Validators must start with the exact retry target. After one broader same-surface check, they must stop.
- Validators must report exact failing ids and exact snippets. Never explain failures away.
- Replanners must `read_notes` on the failing scope before new archaeology, try `check_exploration_memory(paths=[...])` before duplicate recovery exploration, and treat validator evidence plus one live owner confirmation as enough to act.

## Mandatory benchmark references

- Replanners on resume or corrective turns where the validator packet already names exact failing pytest ids plus exact existing owner files must load `team-replanner-playbook/corrective-fast-path` before deeper analysis.

## Cross-surface guardrails

- When a change affects public serialization, schema shape, or docs-visible output, developers and validators must run one nearby same-surface guardrail in addition to the originally failing test.

## Observability note

- When debugging runtime, coordination, retry, or checkpoint behavior, benchmark logs under `.ephemeralos/benchmark-logs/` are supporting evidence only. Live workspace state and current command output still win.
- Prefer structured events that name prompt/completion/total tokens, tool usage and limits, final context size, compactions, checkpoint id/label/parent, resume source, retries, and replans when those logs exist.
