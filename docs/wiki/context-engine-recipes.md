---
title: "Context Engine Recipes"
tags: ["context-engine", "recipes", "planner", "generator", "evaluator", "context-packet", "context-scope", "live-e2e", "see-also"]
created: 2026-05-10T11:27:34.362Z
updated: 2026-05-10T11:58:11.495Z
sources: []
links: ["live-e2e-testing-framework-design.md", "task-center-pipeline.md", "engine-query-loop-llm-seam.md"]
category: architecture
confidence: medium
schemaVersion: 1
---

# Context Engine Recipes

_Source: explore agent draft, 2026-05-10. See `.omc/wiki-draft/context-engine.md`._

## Pipeline

```
ContextScope
  → ContextEngine.build(recipe_id, scope)      # engine.py:60-63
      → RecipeRegistry.get(recipe_id)           # recipes_registry.py:47-54
      → scope.assert_fields(required_fields)    # scope.py:32-37
      → recipe.build(scope, deps) → ContextPacket
  → composer appends required_context_blocks   # composer.py:72-73
  → context_packet_store.insert(packet)        # composer.py:74, 85-89
  → MarkdownPromptRenderer.render(packet)      # composer.py:75 → renderer.py:125-134
  → LaunchBundle.task_input (str)              # composer.py:76-81
```

`ContextComposer.compose` (`composer.py:61-81`) is the single call site: `engine.build` → `renderer.render` → `LaunchBundle`. `LaunchBundle.agent_def` carries the system prompt; `LaunchBundle.task_input` is the rendered user prompt.

## ContextScope

**`context_engine/scope.py:16-38`** — frozen dataclass.

| Field | Required by |
|---|---|
| `mission_id` | planner, generator, evaluator, helpers |
| `episode_id` | planner (required); evaluator/generator fall back to `attempt.episode_id` |
| `attempt_id` | planner, generator, evaluator |
| `task_id` | generator, entry_executor, helpers |
| `parent_packet_id` | advisor_v1 / resolver_v1 only |
| `parent_task_id` | advisor_v1 / resolver_v1 only |

`scope.assert_fields(required)` raises `RecipeScopeError` (`errors.py:14`) on `None` required field; called by `engine.build` before recipe dispatch (`engine.py:62`).

## ContextPacket

**`context_engine/packet.py:82-93`** — immutable Pydantic model.

```
ContextPacket
  id: str                      # UUID auto-generated
  target_role: str             # "planner"|"generator"|"evaluator"|"executor"
  target_id: str | None        # attempt_id or task_id
  canonical_refs: ContextRefs  # mission/episode/attempt/task ids
  blocks: list[ContextBlock]
  metadata: dict[str, str]     # optional "token_budget"
  source_ids: list[str]
```

**ContextBlock** (`packet.py:59-79`): `kind: str`, `priority: ContextPriority`, `text: str`, `source_id`, `source_kind`, `metadata`. `required` priority blocks must have non-blank text.

**ContextPriority** (`packet.py:18-24`): `required | high | medium | low`. Compression drops `low` first, then `medium`; `required`/`high` never truncated (`renderer.py:215-235`).

**ContextBlockKind** constants (`packet.py:31-46`): `mission_goal`, `episode_goal`, `prior_episode_specification`, `prior_episode_summary`, `failed_attempt_landscape`, `partial_plan_boundary`, `planned_task_spec`, `task_specification`, `evaluation_criteria`, `dependency_summary`, `completed_task_summary`, `artifact_reference`, `entry_request`.

**ContextRefs** (`packet.py:48-56`): mission/episode/attempt/task ids.

Inherited blocks (`metadata["inherited_from_parent"] == "true"`) render under `"# Parent context"` heading (`renderer.py:131-133`).

## Recipe per role

### entry_executor_v1
**`recipes/entry_executor.py`** | Required scope: `{task_id}`. Reads `deps.task_store.get_task(task_id)`. Emits one `entry_request` block (`priority=required`, text = `task["task_input"]`). No mission/episode/attempt context.

### planner_v1
**`recipes/planner.py`** | Required scope: `{mission_id, episode_id, attempt_id}`.

Calls `mission_episode_blocks(...)` (`_mission_episode.py:20-40`) then `failed_attempt_landscape_blocks(...)` (`attempt_landscape.py:15-77`).

**Episode frame branch** (`_mission_episode.py:27-40`):
- `episode.sequence_no == 1` → single `episode_goal` block, heading `"# Mission / Current Episode"`.
- `sequence_no > 1` → `mission_goal` + N prior-episode pairs (`prior_episode_specification` + `prior_episode_summary` per closed episode, sorted by `sequence_no`) + `episode_goal`. Immediate prior `priority=HIGH`; older `priority=MEDIUM` (`_mission_episode.py:84-87`). Missing prior fields → `ContextEngineError`.

**Retry branch** (`attempt_landscape.py`): failed attempts = `status==FAILED AND id != current_attempt_id`. Zero failed -> no blocks. Each failed attempt renders one `failed_attempt_landscape` block (`priority=HIGH`) under `"# Prior Failed Attempts"` with `Accepted Plan`, `Generator Outcomes`, and, only when all generators completed and an evaluator task exists, `Evaluator Judgment`. The generator section includes status for every planned generator task and detailed subsections only for useful stored summaries. The recipe does not render a separate failure-reason section.

### generator_v1
**`recipes/generator.py`** | Required scope: `{mission_id, attempt_id, task_id}`.

Block order: `task_specification` (attempt plan) → `dependency_summary` blocks → `planned_task_spec` (always last, `priority=REQUIRED`).

**Plan presence** (`generator.py:50-59`): `attempt.task_specification` truthy → prepend `task_specification` block (`priority=HIGH`).

**Partial-plan boundary is not rendered here.** Even when `attempt.continuation_goal` is set, `generator_v1` does not emit `partial_plan_boundary`; generators should execute only their local task and dependency summaries, not reason about deferred episode scope.

**Dependency presence** (`generator.py:61-65`, `_dependency_summary_blocks` at `91-115`): iterates `task["needs"]`. Each resolved dep → `dependency_summary` block (`priority=MEDIUM`) with `latest_summary_text(dep["summaries"])` (`_summaries.py:14-20`), grouped under `"# Dependency Results"`. Missing dep rows raise `ContextEngineError` because dependency edges are accepted planner DAG invariants.

### evaluator_v1
**`recipes/evaluator.py`** | Required scope: `{mission_id, attempt_id}`.

Block order: `mission_episode_blocks(...)` → `task_specification` → optional `partial_plan_boundary` → `completed_task_summary` per generator task → `evaluation_criteria`.

**Episode frame**: same `_mission_episode.py` logic as planner.

**Plan presence** (`evaluator.py:55-64`): `attempt.task_specification` truthy → `task_specification` block at `priority=REQUIRED` (stronger than generator's `HIGH`).

**Partial-plan presence**: `attempt.continuation_goal` truthy → `partial_plan_boundary` block (`priority=REQUIRED`) with `plan_kind: partial` and the deferred `continuation_goal`, rendered before dependency results. This tells the evaluator not to fail the current attempt for intentionally deferred work.

**Generator task summaries** (`evaluator.py:66-83`): iterates `attempt.generator_task_ids`; each existing task → `completed_task_summary` block (`priority=HIGH`), grouped under `"# Dependency Results"`.

**Criteria presence** (`evaluator.py:84-94`): `attempt.evaluation_criteria` non-empty → single `evaluation_criteria` block (`priority=REQUIRED`), bullet-formatted.

Note: evaluator does **not** call `failed_attempt_landscape_blocks`; prior failure history is planner-only.

## Per-state matrix

| Scenario | Recipe | What changes | Key conditional |
|---|---|---|---|
| Initial mission (ep 1, attempt 1) | planner_v1 | Single `episode_goal` block, combined heading; no failed-attempts | `_mission_episode.py:27-28` |
| Attempt retry after failure (ep 1, attempt N>1) | planner_v1 | Adds N-1 `failed_attempt_landscape` under `"# Prior Failed Attempts"` with accepted plan, generator outcome statuses, useful generator summaries, and evaluator judgment when present | `attempt_landscape.py` |
| Episodic continuation (ep 2+) | planner_v1, evaluator_v1 | Adds `mission_goal` + prior-episode pairs; immediate prior `HIGH`, older `MEDIUM` | `_mission_episode.py:30-40`, `84-87` |
| Generator — with dependency outputs | generator_v1 | Adds `dependency_summary` blocks under `"# Dependency Results"` | `generator.py:61-65`, `91-115` |
| Generator — no dependencies | generator_v1 | No `dependency_summary` blocks | `generator.py:62`: `needs` empty |
| Attempt retry on generator failure | generator_v1 | New attempt = new `task_specification` from re-plan | `generator.py:50-59` |
| Attempt retry on evaluator failure | evaluator_v1 | New attempt → new `task_specification` + revised `evaluation_criteria` | `evaluator.py:55-64`, `84-94` |
| Nested mission (child helper) | advisor_v1 / resolver_v1 | Parent blocks copied with priority demoted + `inherited_from_parent=true`; rendered under `"# Parent context"` | `helper.py:35-40`, `66-92`; `renderer.py:131-133` |

## What the live-e2e framework needs

### Public surface

| Symbol | File:line |
|---|---|
| `ContextEngine` | `engine.py:47-63` |
| `ContextEngineDeps` | `engine.py:30-44` |
| `register_builtin_recipes` | `recipes/__init__.py:38-41` |
| `ContextScope` | `scope.py:16-38` |
| `MarkdownPromptRenderer` | `renderer.py:106-246` |
| `ContextComposer` | `agent_launch/composer.py:43-89` |
| `LaunchBundle.task_input` | `agent_launch/composer.py:37` |
| `RecipeRegistry.clear` | `recipes_registry.py:64-66` |

### Recommended hook point

**Capture the `LaunchBundle` returned by `ContextComposer.compose`** (`composer.py:61-81`). At that point:
- `bundle.task_input` = fully rendered user prompt (post `renderer.render`)
- `bundle.agent_def` = `AgentDefinition` carrying the system prompt
- `bundle.packet` = structured `ContextPacket` for block-level assertions

Wrap or subclass `ContextComposer.compose`, capture, assert, forward to launcher. After `renderer.render(packet)` (`composer.py:75`) and before the LLM call.

### Real vs fake

| Component | Status |
|---|---|
| `ContextEngine.build` + all recipes | REAL |
| `mission_episode_blocks`, `failed_attempt_landscape_blocks`, `_dependency_summary_blocks` | REAL |
| `MarkdownPromptRenderer.render` | REAL |
| `ContextComposer.compose` | REAL |
| `AgentResolver.resolve` | REAL |
| `context_packet_store.insert` | REAL or in-memory stub |
| LLM API call | FAKE — replay only |

### What to test

- **Planner — initial mission**: `task_input` contains `"# Mission / Current Episode"`; no `"# Prior Failed Attempts"`; `packet.blocks` has one block (`episode_goal`).
- **Planner — attempt retry**: `"# Prior Failed Attempts"` present; each prior attempt has `Accepted Plan` and `Generator Outcomes`; evaluator judgment appears only when an evaluator ran; block count matches failed-attempt count.
- **Planner/evaluator — episodic continuation**: `"# Mission"` block present; `"# Previous Episode Results"` group present; immediate-prior `priority=HIGH`.
- **Generator — with dependencies**: `"# Dependency Results"` present; each dep summary text appears.
- **Generator — no dependencies**: no `"# Dependency Results"`.
- **Evaluator — criteria present**: `"# Evaluation Criteria"` with bulleted items matching `attempt.evaluation_criteria`.
- **Nested mission (helper)**: `"# Parent context"` heading; parent blocks demoted; `packet.metadata["inherits_from"]` matches parent packet id.
- **Entry executor**: `task_input` contains only `entry_request` content.

### Error surface

- `RecipeScopeError` (`errors.py:14`) — scope missing required field.
- `ContextEngineError` (`errors.py:10`) — missing store row.
- `MissingContextRecipeError` (`errors.py:18`) — agent definition has no `context_recipe`.

---

## Update (2026-05-10T11:58:11.495Z)

## See also

- [[role-planner]], [[role-generator]], [[role-evaluator]] — per-role deep dives on what each recipe's blocks mean to its consumer
- [[task-center-pipeline]] — the data the recipes read from
- [[engine-query-loop-llm-seam]] — what consumes the rendered prompt
