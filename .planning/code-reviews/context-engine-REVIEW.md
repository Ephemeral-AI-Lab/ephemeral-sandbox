# `task_center/context_engine` — Harsh Review & Recommended Changes

**Date:** 2026-05-14
**Branch:** codex/fix-dot-path-normalization-tests
**Reviewer:** Claude (Opus 4.7) + four parallel sub-reviewers (architect, critic, code-reviewer)
**Scope:** `backend/src/task_center/context_engine/` (16 files, ~1500 LoC)

## Verdict

Internally coherent, builds cleanly, six recipes hold to a uniform shape. That's the compliment. Every flexibility seam advertised in docstrings is **promised in prose and unfunded in code**: `_v1` doesn't version, `PromptRenderer` Protocol doesn't pluralize, `extra="forbid"` doesn't migrate, `ContextBlockKind` is a vanity enum, `RecipeRegistry` is global state with admitted test-isolation debt, and recipes own presentation strings that the renderer pretends to control. There is also **one real data-attribution bug** (helper `canonical_refs` info loss). Healthy for v1 launch; not healthy for a second contributor to add a recipe without three "where is X defined?" questions.

---

## Recommended Changes (landing order)

### 1. Fix the bug: helper packet drops attempt/episode refs — **HIGH (real bug)**

**File:** `recipes/helper.py:92-102`

`_build_helper_packet` copies every parent block (text, source_id, demoted priority, metadata + `inherited_from_parent=true`) but rebuilds `canonical_refs` from `scope` only — keeping `mission_id` and `task_id`, **dropping `episode_id` and `attempt_id`**. The blocks contain attempt-scoped content (failed attempts, attempt's task_specification, evaluation_criteria) but the refs say "no episode, no attempt." Any audit / trace / projection that filters packets by `canonical_refs.attempt_id` will silently exclude helper packets that *are* about that attempt.

**Fix (2 lines):**
```python
canonical_refs=parent_packet.canonical_refs.model_copy(
    update={"task_id": scope.task_id}
),
```

---

### 2. Delete dead `if X is None: raise` checks in all five recipes — **HIGH (dead code, ~30 LoC)**

**Files:** `recipes/planner.py:43-51`, `evaluator.py:36-40`, `generator.py:38-46`, `helper.py:56-64`, `entry_executor.py:32-34`

Every recipe declares `_REQUIRED_FIELDS = frozenset({...})` and then re-checks the same fields in `_build`. The comments claim "explicit guard makes the recipe self-defending under `python -O` where `assert` would be stripped." **The comments are factually wrong.** Neither `ContextScope.assert_fields` nor the in-recipe checks use `assert` — both use `if ... raise`. `-O` strips nothing. The engine always calls `assert_fields` before `recipe.build` (`engine.py:62`). The in-recipe checks can never fire in production.

**Fix:** Delete the in-recipe `if scope.X is None: raise` blocks and the misleading `# Engine pre-validates...python -O` comments. Trust `engine.build → scope.assert_fields`.

---

### 3. Add facade `__init__.py` — **MED (cuts rename blast radius 10×)**

**File:** `context_engine/__init__.py` (currently one-line docstring)

Every external caller reaches into deep submodules. Even `task_center/api.py` itself imports from three submodules (`api.py:13-15`). Renaming `ContextScope` is a 10+ file edit today.

**Fix:** Re-export public surface:
```python
from task_center.context_engine.engine import ContextEngine, ContextEngineDeps
from task_center.context_engine.scope import ContextScope
from task_center.context_engine.packet import (
    ContextPacket, ContextBlock, ContextBlockKind, ContextPriority, ContextRefs,
)
from task_center.context_engine.recipes_registry import RecipeRegistry, ContextRecipe
from task_center.context_engine.recipes import register_builtin_recipes
from task_center.context_engine.errors import (
    ContextEngineError, RecipeScopeError,
    MissingContextRecipeError, AgentDefinitionValidationError,
)

__all__ = [...]
```

Update external callers (`task_center/api.py`, `task_center/entry/coordinator.py`, `task_center/domain.py`, `agents/definition/resolved_validation.py`, and 4 other discovered call sites) to import from `task_center.context_engine` only.

---

### 4. Break the engine ↔ recipes_registry cycle — **MED**

**Files:** all 5 recipe modules (`recipes/*.py`) + `recipes_registry.py`

The cycle is real, not "static" as the comment claims. `engine.py:14` imports `RecipeRegistry` at runtime; `recipes_registry.py:21` imports `ContextEngineDeps` via `TYPE_CHECKING`. Every recipe imports `ContextEngineDeps` at runtime for parameter annotations. Reverse the import order in any test or REPL and you get `ImportError`.

**Fix:** Each recipe module already has `from __future__ import annotations`. Move `from task_center.context_engine.engine import ContextEngineDeps` into a `TYPE_CHECKING` block. Same treatment for `Mission`, `Episode` in `_mission_episode.py:3,12`, and `Attempt` (annotation-only) in `attempt_landscape.py:14`. Keep `AttemptStatus` runtime (used in enum comparison).

---

### 5. Schema-version `ContextPacket` — **HIGH (prevents the first breaking-change incident)**

**File:** `packet.py`, `db/stores/context_packet_store.py`

`BaseModel(extra="forbid")` (`packet.py:94`). Persisted via `context_packet_store.insert`. Helper recipes **read back** persisted packets at spawn time (`helper.py:70`). No `schema_version` field, no migration hook, no read-time tolerance. Rename a field tomorrow and every in-flight helper spawn gets `ValidationError`.

**Fix:**
```python
class ContextPacket(BaseModel):
    schema_version: int = 1
    # existing fields…
    model_config = ConfigDict(extra="forbid")  # on write
```
On read in `context_packet_store.get`: deserialize with `extra="ignore"`, route through `_migrate(raw_dict)` based on `schema_version`. Bump version when a field is renamed/removed.

---

### 6. Collapse `advisor_v1` + `resolver_v1` into one parameterized `helper_v1` — **MED**

**Files:** `recipes/helper.py`, `recipes/__init__.py`, `scope.py`

`helper.py:47-118` has two ~3-line recipes differing only in `target_role="advisor"` vs `"resolver"`. Adding a "critic" helper today = copy 6 lines + edit the central `_BUILTIN_RECIPES` tuple in `recipes/__init__.py:28-35`.

**Fix:**
- Add `helper_role: str | None = None` to `ContextScope`.
- Replace `_advisor_v1_build` / `_resolver_v1_build` with one `_helper_v1_build(scope, deps)` that reads `scope.helper_role` to set `target_role`.
- Register a single `HELPER_V1_RECIPE`. Drop the two existing entries from `_BUILTIN_RECIPES`.
- Update `agent_launch/composer.py` and tool spawn paths (`tools/ask_helper/...`, `tools/subagent/run_subagent.py`) to pass `helper_role` into `ContextScope`.

This both removes the duplicate-pair smell and prepares for `_BUILTIN_RECIPES` becoming discovery-based.

---

### 7. Promote contract fields off `metadata` onto typed model — **MED (unblocks #8, #9)**

**Files:** `packet.py`, all `recipes/*.py`, `renderer.py`

`metadata: dict[str, str]` everywhere forces stringly-typed contracts:
- `"token_budget": "8000"` → `int(raw)` in `renderer.py:144-148`
- `"plan_kind": "partial"` (closed enum as free string)
- `_is_inherited(block)` comparing to literal `"true"` (`renderer.py:104`) — helper inheritance hinges on a string comparison

**Fix:** Add typed fields directly on `ContextBlock` / `ContextPacket`:
```python
class ContextBlock(BaseModel):
    kind: ContextBlockKind  # see #10
    priority: ContextPriority
    text: str
    source_id: str | None = None
    source_kind: str | None = None
    inherited_from_parent: bool = False
    group_key: str | None = None
    subheading: str | None = None
    subtitle: str | None = None
    heading_override: str | None = None
    metadata: dict[str, str] = Field(default_factory=dict)  # opaque provenance only

class ContextPacket(BaseModel):
    schema_version: int = 1
    # …
    token_budget: int | None = None
    inherits_from: str | None = None
    metadata: dict[str, str] = Field(default_factory=dict)
```
Sweep every `metadata["..."]` reader/writer. Document remaining allowed `metadata` keys.

---

### 8. Move compression and token estimation out of `MarkdownPromptRenderer` — **MED**

**Files:** `renderer.py`, new `packet.py` or `compression.py`, `engine.py`

`_compress` and `_estimate_tokens` are packet-shape and cross-cutting concerns marooned inside the markdown renderer. A second renderer would copy-paste; a pre-render budgeter has to instantiate a markdown renderer.

**Fix:**
- Extract `compress(blocks, *, budget, estimator) -> list[ContextBlock]` as a free function.
- Add `TokenEstimator` Protocol with `estimate(text: str) -> int`. Default impl = the 4-char heuristic.
- Place the estimator on `ContextEngineDeps` (or a small `RenderConfig` passed to renderers).
- `MarkdownPromptRenderer.render` calls `compress(...)` with the estimator; it no longer owns either.

Additionally, fix the **silent budget overrun**: `_compress` only iterates `(LOW, MEDIUM)`. Ten HIGH `failed_attempt_landscape` blocks blow the budget today. Add a structured log when `running > budget` after the drop pass; optionally a `strict_budget` mode that demotes HIGH→MEDIUM as a final fallback.

---

### 9. Centralize headings in `default_heading_template()` and fix dead-letter defaults — **MED**

**Files:** `renderer.py`, `recipes/_mission_episode.py`, `recipes/evaluator.py`, `recipes/generator.py`, `recipes/attempt_landscape.py`

`HeadingTemplate.heading_for` short-circuits on `metadata["heading"]`. `_mission_episode.py:50,61` **always** sets it, so `default_heading_template()` entries for `mission_goal`, `episode_goal`, `prior_episode_specification`, `prior_episode_summary` are dead code. The recipe-side `MISSION_HEADING`/`CURRENT_EPISODE_HEADING`/`MISSION_EPISODE_HEADING`/`PREVIOUS_EPISODE_RESULTS_HEADING` constants live in the wrong layer (presentation in a recipe helper). Same for `"# Dependency Results"` repeated in `evaluator.py:111` and `generator.py:126`, and `"# Prior Failed Attempts"` repeated in `attempt_landscape.py:55` and `renderer.py:84`.

**Fix:**
- Move all heading text into `renderer.default_heading_template()`.
- Delete the 4 heading-text constants from `_mission_episode.py`.
- Stop recipes from setting `metadata["heading"]` / `metadata["group_heading"]` literal strings; use semantic keys (e.g. `block.group_key = "previous_episode_results"`) and let the renderer map key → display string.
- Register heading templates as `(format_string, required_keys: frozenset[str])`. On `KeyError`, only fall back if `block.inherited_from_parent`; raise `ContextEngineError` otherwise so heading drift surfaces in tests.

---

### 10. Make `ContextBlockKind` enforced — **MED**

**File:** `packet.py:31-47, 60-80`

Today: `StrEnum` declared, but `ContextBlock.kind: str`. Open-string benefit forfeited (no exhaustiveness, no rename), closed-enum benefit also forfeited. Worst of both.

**Fix:** Type `kind: ContextBlockKind`. Force every new kind to land in the enum (and through the heading template registry at the same time). If the open-extension policy must survive, replace the enum with module-level string constants and document the convention; but pick one.

---

### 11. Replace `_BUILTIN_RECIPES` tuple + make `RecipeRegistry` instance-based — **MED**

**Files:** `recipes_registry.py`, `recipes/__init__.py`, `engine.py`, `task_center/entry/coordinator.py`, `agents/definition/resolved_validation.py`

`_BUILTIN_RECIPES` is a central tuple — every new recipe edits it. `RecipeRegistry` is `ClassVar`-backed singleton with admitted test-isolation debt. `agents/definition/resolved_validation.py:37` reaches into the global registry to validate agent definitions.

**Fix:**
- Convert `RecipeRegistry` to a regular class. Engine accepts an instance via `ContextEngineDeps.recipe_registry` (or constructor argument).
- Move `register_builtin_recipes(registry)` to accept a registry argument.
- `coordinator.py` constructs one registry per engine, calls `register_builtin_recipes(registry)`.
- `resolved_validation.py` accepts a `recipe_registry` parameter from its caller.
- Optional: replace the tuple with module discovery (walk `recipes/`, look for module-level `__recipe__`) so new recipes plug in without editing `__init__.py`.

---

## Items intentionally deferred (do NOT do yet)

| Item | Why defer |
|---|---|
| Convert `ContextRecipe` to ABC with template-method `build` | Speculative until helper-as-one-recipe lands (#6) and you can see the residual duplication |
| Extract `CompressionPolicy` Strategy | Only one policy exists; project's own `CLAUDE.md §2 "Simplicity First"` warns against this |
| Per-recipe `ContextEngineDeps` slicing / typed provider lookup | Bag of 4 stores is fine; reshape at recipe seven |
| Per-role / per-model token budgets (`TokenPolicy`) | Solve when a real model migration forces it |
| `order_index: int | None` on `ContextBlock` for explicit ordering | Insertion order works today; add only when packet aggregation appears |
| Plain-text / JSON / HTML renderer | Add a second concrete renderer; THEN re-litigate `PromptRenderer` Protocol surface |

---

## Reference: Severity-ranked findings

| # | Severity | Axis | Finding | File:line |
|---|---|---|---|---|
| 1 | **HIGH (bug)** | correctness | Helper packet drops `episode_id` / `attempt_id` from canonical_refs | `helper.py:92-102` |
| 2 | **HIGH** | dead code | `if X is None: raise` duplicates `assert_fields`; `-O` justification is factually wrong | all 5 recipes |
| 3 | **HIGH** | flexibility | `_v1` versioning is theatrical, no `name`+`version` contract | `recipes/__init__.py:28-35` |
| 4 | **HIGH** | flexibility | `ContextPacket` schema unversioned; `extra="forbid"` + persistence = breaking-change timebomb | `packet.py:94`, store |
| 5 | **HIGH** | flexibility | `_compress` cannot touch HIGH/REQUIRED; silent budget overrun | `renderer.py:224-227` |
| 6 | **HIGH** | extensibility | `_BUILTIN_RECIPES` central tuple closes for extension | `recipes/__init__.py:28-35` |
| 7 | **HIGH** | pipeline | Heading constants in recipes shadow renderer defaults; 6/12 defaults dead | `_mission_episode.py:14-17, 50-61` |
| 8 | **HIGH** | pipeline | Presentation policy leaks through metadata strings | recipes + renderer |
| 9 | **HIGH** | coupling | `engine ⇄ recipes_registry` cycle is real, not static | `engine.py:14`, `recipes_registry.py:21` |
| 10 | **HIGH** | coupling | Domain-model runtime imports are annotation-only | `_mission_episode.py:3,12`; `attempt_landscape.py:14` |
| 11 | **MED** | design | `ContextRecipe` dataclass-with-callable lacks composition / caching / decoration hooks | `recipes_registry.py:28-34` |
| 12 | **MED** | design | Helper-as-two-recipes; role discrimination via string parameter | `helper.py:47-118` |
| 13 | **MED** | design | `ContextBlockKind` bimodal (closed enum advertised, open string enforced) | `packet.py:31-47, 60-80` |
| 14 | **MED** | design | `RecipeRegistry` ClassVar singleton; admitted test-isolation debt | `recipes_registry.py:37-66` |
| 15 | **MED** | design | `ContextEngineDeps` won't scale past 4 stores | `engine.py:30-44` |
| 16 | **MED** | pipeline | `_compress` married to `MarkdownPromptRenderer` | `renderer.py:202-243` |
| 17 | **MED** | pipeline | `_estimate_tokens` (`_CHARS_PER_TOKEN`) in wrong home | `renderer.py:26-28, 99-100` |
| 18 | **MED** | pipeline | `HeadingTemplate.heading_for` swallows KeyError silently | `renderer.py:59-69` |
| 19 | **MED** | pipeline | `_render_block` vs `_render_group` — two grammars; contiguity-implicit grouping | `renderer.py:158-200` |
| 20 | **MED** | flexibility | `metadata: dict[str, str]` everywhere; stringly-typed contracts | `packet.py:68, 91` |
| 21 | **MED** | flexibility | Helper demotion is frozen dict, not per-role policy | `helper.py:35-40` |
| 22 | **MED** | flexibility | Group heading mechanic is adjacency-only — silent footgun if interleaved | `renderer.py:165-176` |
| 23 | **MED** | structure | No facade `__init__.py`; 10+ files break on `ContextScope` rename | `context_engine/__init__.py` |
| 24 | **MED** | structure | `renderer.py` lives inside engine package; composer is in `agent_launch/` (layering smear) | `agent_launch/composer.py` |
| 25 | **MED** | coupling | String-contract drift between recipes and renderer (`"# Dependency Results"`, `"inherited_from_parent"`, etc.) | recipes + renderer |
| 26 | **LOW** | naming | `recipes_registry.py` shadows the `recipes/` package | top-level |
| 27 | **LOW** | naming | `ContextRecipe` lives in the registry module, not in `recipes/` | `recipes_registry.py:28` |
| 28 | **LOW** | naming | `_mission_episode.py` / `_summaries.py` underscore-private but `attempt_landscape.py` isn't | `recipes/` |
| 29 | **LOW** | naming | Per-recipe `ENTRY_EXECUTOR_V1` + `_RECIPE` constants are duplication | every `recipes/*.py` |
| 30 | **LOW** | naming | `target_role="executor"` for `entry_executor_v1` — id and role disagree | `recipes/entry_executor.py:50` |
| 31 | **LOW** | flexibility | Episode-1 special case is a business rule in a shared block helper | `_mission_episode.py:27` |
| 32 | **LOW** | structure | `helper.py` packs two recipes; every other recipe gets its own file | `recipes/helper.py` |
| 33 | **LOW** | structure | `recipes/__init__.py` is both eager barrel and side-effecting registrar | `recipes/__init__.py` |
| 34 | **LOW** | pipeline | `PromptRenderer` Protocol speculative — one impl, one consumer | `renderer.py:31-34` |
| 35 | **LOW** | pipeline | `ContextPacketStoreProtocol.insert` return-id contract is wishy-washy | `engine.py:24-27`, store |

---

## Files referenced

**Module under review:**
- `backend/src/task_center/context_engine/__init__.py`
- `backend/src/task_center/context_engine/engine.py`
- `backend/src/task_center/context_engine/errors.py`
- `backend/src/task_center/context_engine/packet.py`
- `backend/src/task_center/context_engine/recipes_registry.py`
- `backend/src/task_center/context_engine/renderer.py`
- `backend/src/task_center/context_engine/scope.py`
- `backend/src/task_center/context_engine/recipes/__init__.py`
- `backend/src/task_center/context_engine/recipes/_mission_episode.py`
- `backend/src/task_center/context_engine/recipes/_summaries.py`
- `backend/src/task_center/context_engine/recipes/attempt_landscape.py`
- `backend/src/task_center/context_engine/recipes/entry_executor.py`
- `backend/src/task_center/context_engine/recipes/evaluator.py`
- `backend/src/task_center/context_engine/recipes/generator.py`
- `backend/src/task_center/context_engine/recipes/helper.py`
- `backend/src/task_center/context_engine/recipes/planner.py`

**External call sites:**
- `backend/src/task_center/api.py`
- `backend/src/task_center/domain.py`
- `backend/src/task_center/entry/coordinator.py`
- `backend/src/task_center/agent_launch/composer.py` (gluing site; lives outside `context_engine/`)
- `backend/src/task_center/agent_launch/predicates.py`
- `backend/src/task_center/agent_launch/resolver.py`
- `backend/src/task_center/attempt/dispatcher.py`
- `backend/src/task_center/attempt/orchestrator.py`
- `backend/src/tools/ask_helper/_lib/_compose.py`
- `backend/src/agents/definition/resolved_validation.py`
- `backend/src/db/stores/context_packet_store.py`
