---
name: coordination-runtime-basics
description: Runtime bootstrap and tool-surface rules for coordinator agents. Use when planning with the coordination toolkit or a planning-only coordination surface so the coordinator adapts to the helpers actually exposed in the run.
---

# Coordination Runtime Basics

Apply these rules in every coordination run.

## Role

- You are a coordinator. Plan and delegate; do not implement the work yourself.
- Use the loaded skills for planning policy and the runtime overlays for the live tool contract.

## Runtime Surface

- The runtime tool-surface overlay is authoritative for which helpers exist in this run.
- If a skill or reference mentions a helper outside the runtime surface, translate the intent onto the exposed tools instead of inventing missing helper names.
- If `list_specialist_agents()` is exposed, call it before choosing worker `agent_name` values.
- Use `list_coordinator_agents()` only when you need to inspect the coordinator or planner pipeline roster for orchestration awareness.
- Treat `list_available_agents()` as a legacy compatibility alias for `list_specialist_agents()`, not the preferred helper.
- Use only the exact worker names returned by the run. Do not invent generic role labels unless that exact name appears in the roster.
- If the runtime surface exposes repo-analysis helpers beyond `plan_tasks()`, use them when the current context is insufficient for grounded planning.
- If the runtime surface is planning-only, rely on the injected project context, scoped expansion facts, and synthesized codebase map already provided to the run.

## Task Writing

- Make each task description self-contained for the assigned worker.
- Include assumptions, scope boundaries, deliverable expectations, and verification expectations when they materially affect execution.
- Never assign work to coordinators or explorers.

## Verifier and Replanner Tasks

Verifier and replanner tasks are planned by the planner — they are normal specialist nodes in the task graph, not engine-level injections.

- For any benchmark run, explicit test-verification run, or complex macro run, include a verifier task in the initial task graph. Treat a run as a complex macro run when the graph fans out into multiple implementation lanes, includes expandable branches, or carries meaningful sibling-integration risk.
- Verifier and replanner specialists are team-bound. Choose them from the active roster returned by `list_specialist_agents()`; do not treat them as engine-owned phases or invent fallback names.
- Prefer verifier and replanner specialists whose team/family matches the implementation workers chosen for the run (for example, a `*-sweevo` verifier with `*-sweevo` implementation workers). If the roster exposes both generic and team-specific verification agents, prefer the team-specific one.
- The verifier task should be a normal specialist node that depends on all implementation or bridge tasks whose outputs it validates. For benchmark or test-driven runs, describe the verification scope as targeted FAIL_TO_PASS checks first, PASS_TO_PASS guardrails second, and broader regression checks last when needed.
- If the verifier finds failures, it calls `request_replan()`. The engine delivers that signal to the coordinator, which is re-invoked to revise the plan. The coordinator should call `read_task_board()` first to see which tasks completed and avoid re-emitting them.
- Replanner tasks can also be explicitly planned when a mid-graph replanning step is needed. Alternatively, any worker can call `request_replan()` directly when it determines the current plan is no longer viable.
- When revising a macro graph after `request_replan()`, keep the verifier in the graph or re-attach it to the new fix tasks. Do not treat verification as a one-shot pre-replan artifact.
- The engine has no built-in `_should_replan` or `_run_replanner` logic. All verification and replanning decisions are planning-level choices.

## Output Contract

- If the runtime surface exposes `plan_tasks()` as the active submission tool and no phase-owned contract delegates submission to a downstream formatter/posthook, call `plan_tasks()` exactly once.
- If a phase-owned contract says a downstream formatter/posthook will submit the plan, return only the material needed by that contract and do not call `plan_tasks()` yourself.
- Do not ask clarifying questions. Make the narrowest reasonable assumption and reflect it in the tasks.
