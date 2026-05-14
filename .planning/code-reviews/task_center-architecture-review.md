# Harsh architectural review — `backend/src/task_center`

**Scope:** all 50 Python files (~6,000 LOC) under `backend/src/task_center/`.
**Lens:** naming, folder/file structure, import-graph cleanliness, extensibility,
inheritance/interface use, future flexibility. Not behavioural correctness.

> Bottom line: this is a working state machine wearing the costume of a layered
> architecture. The package boundaries are decorative, the interface seams are
> threadbare, and the names tell two stories about what it is. Most of the
> "extension points" are hand-wired callbacks that pretend to be plug-and-play.

---

## 1. Naming conventions — graded D

### 1.1 The package can't decide what it's called

The codebase uses **two competing names for the same concept**:

| "TaskCenter" flavor | "Harness" flavor |
| --- | --- |
| `TaskCenterInvariantViolation` | `HarnessLifecycleConfig` |
| `TaskCenterStore` (sibling pkg) | `HarnessTaskRole` |
| `TaskCenterSandboxBridge` | `HarnessTaskStatus` |
| `TaskCenterEntryHandle` | `EphemeralAttemptAgentLauncher` ("harness agent") |
| `TaskCenterAuditEmitter` | persisted column `is_harness` (implied) |
| `task_center_run_id` (column) | "harness attempt" (docstrings) |

`MissionHandler` docstring (`mission/handler.py:1`) says "mission boundary
lifecycle service". `AttemptOrchestrator` docstring (`orchestrator.py:1`) calls
the same flow a "harness attempt". `MissionStarter.start` docstring talks about
"executor → delegated mission start" — three different vocabularies in three
adjacent files. Pick **one** name and use it consistently.

### 1.2 The package name itself is a tell

`task_center` — like `service`, `manager`, `engine`, `controller` — is one of
those generic suffix-words that means nothing. A reader meeting this directory
cannot guess that it owns mission/episode/attempt orchestration. Honest names:
`mission_runtime/`, `agent_orchestrator/`, `harness/`, `lifecycle/`. As-is the
name leaks across the schema (`task_center_run_id`, `task_center_attempt_id`,
`task_center_task_id`) so renaming costs more than it should.

### 1.3 The `-or/-er/-Manager/-Handler/-Composer/-Engine` zoo

Seventeen end-state collaborators in a tightly coupled pipeline:

```
TaskCenterEntryCoordinator → EphemeralAttemptAgentLauncher → MissionStarter
       → MissionHandler → EpisodeManager → AttemptOrchestrator → AttemptDispatcher
                       → MissionCloseReportRouter → EntryTaskController
ContextComposer → ContextEngine → RuleBasedAgentResolver → MarkdownPromptRenderer
TaskCenterAuditEmitter   AttemptOrchestratorRegistry   EpisodeManagerRegistry
TaskCenterSandboxBridge
```

Three suffixes (`Manager`, `Handler`, `Controller`) used for the same role:
"single lifecycle owner of one X". Pick a convention — `EntityLifecycle` works
and stops the suffix bingo.

### 1.4 Specific naming smells

- **`MissionHandler` vs `MissionStarter`.** Both touch missions.
  `MissionHandler.create_mission` and `MissionStarter.start` overlap in scope;
  no name tells you which owns what. Reader has to read both files
  (`handler.py`, `starter.py`) to find out that Starter is the
  use-case-orchestrator and Handler is the boundary CRUD.

- **`AttemptRuntime` is not a runtime.** It's a frozen `@dataclass` carrying 8
  stores + 2 registries + an optional composer + an optional controller + an
  audit_sink (`runtime.py:46-65`). It's a service-locator bag. Rename
  `AttemptDeps` / `AttemptContext`. "Runtime" implies a process / scheduler /
  VM — none of which this is.

- **`AttemptStage` mixes verb tenses**: `PLANNING / GENERATING / EVALUATING /
  CLOSED` — three present-progressive verbs and one past-tense state
  (`attempt/state.py:10-15`). Either `IN_PLANNING / … / CLOSED` or `PLAN /
  GENERATE / EVALUATE / CLOSED`.

- **`HarnessTaskRole` is a lie for the entry executor.** The enum contains
  `{PLANNER, GENERATOR, EVALUATOR}` but the entry-mode task is persisted as
  `role=GENERATOR` (`coordinator.py:260`). The role then has to be branched on
  via `attempt_id is None` (`launcher.py:187-198`,
  `mission/close_report_delivery.py:67-86`). Either expose `ENTRY_EXECUTOR` or
  stop reusing `GENERATOR`.

- **`HarnessTaskStatus.WAITING_MISSION`** is a status that only makes sense for
  one role in one situation. It's not in `TERMINAL_GENERATOR_STATUSES` but it
  also isn't `RUNNING`. The enum needs role-specific carving or a
  `task_center_blocked_by_mission` flag instead of an extra enum value.

- **`MissionCloseReport` vs `EpisodeClosureReport`.** Different word
  (`Close` vs `Closure`) for the same concept across two adjacent files.

- **`task_input`** is a pre-rendered markdown prompt. "Input" is too generic.
  `rendered_prompt` would be honest. Same field name appears in `AgentLaunch`
  (`runtime.py:33`), `LaunchBundle` (`composer.py:36`), task store rows.

- **`PLANNER_V1`, `PLANNER_V1_RECIPE`, `_planner_v1_build`** — three names for
  one recipe. `_v1` is in the recipe id, the constant, and the function. There
  is no `_v2` anywhere. Speculative versioning is just noise.

- **`task_center_run_id_for_attempt`** (`runtime.py:67`). Two `_for_` chains
  in one identifier is a smell.

- **`spawn_reason`** is a free-form string at every `upsert_task` call:
  `"attempt_planner"`, `"attempt_generator"`, `"attempt_evaluator"`,
  `"entry_executor"` (`orchestrator.py:104`, `dispatcher.py:273`,
  `coordinator.py:267`). No enum, no constants, typo-prone.

- **`PredicateRegistry` / `RecipeRegistry`** are class-state singletons named
  like classes (`predicates.py:41`, `recipes_registry.py:37`). Two near-identical
  implementations; could be one generic `Registry[T]` or just a module-level
  dict.

---

## 2. Folder/file structure — graded C-

### 2.1 Three half-facades doing the job of one

```
task_center/__init__.py    — 10-line docstring, no exports
task_center/api.py         — 22 re-exports + lazy __getattr__ for one
task_center/domain.py      — 11 read-only DTO re-exports
```

Three different surfaces with overlapping content (`Episode`, `Attempt`,
`Mission` are exported from both `api.py` AND `domain.py`). External callers
must remember which "facade" to use. The `__init__.py` says "external callers
should import `task_center.api`" — but Python convention is for
`task_center/__init__.py` itself to be the public surface. The current split
gives you the worst of both: longer import paths AND voluntary discipline that
nothing enforces.

**Fix:** collapse `api.py` into `__init__.py`; delete `domain.py`; let internal
modules import from canonical paths and external callers from the package root.

### 2.2 The public surface is too wide

`api.py` re-exports **22 names** including the lazy-loaded
`start_task_center_entry_run`. A clean facade exposes 3-5 entry points; this
one exposes the internal class hierarchy basically wholesale, which is why every
refactor is a breaking change for callers.

### 2.3 Stuttering subpackage names

```
task_center/mission/mission.py     — DTO file
task_center/episode/episode.py     — DTO file
task_center/attempt/state.py       — DTO file (different convention!)
task_center/entry/controller.py    — no DTO file (different again!)
task_center/task/models.py         — DTO file (yet another convention)
```

Five subpackages, four naming conventions for the data-class file. Pick one:
either `<pkg>/state.py` everywhere or `<pkg>/models.py` everywhere. The
`mission/mission.py` and `episode/episode.py` stutter is the worst offender.

### 2.4 Sub-package surcharge

- **`task/`** (3 files: `__init__.py`, `ids.py`, `models.py`) — undersized.
  `task/ids.py` is a few helpers. Flatten to top-level
  `task_center/task_models.py` and `task_center/task_ids.py`.

- **`audit/`** (2 files: `emitter.py` 126 lines, `events.py` 13 lines).
  Five lines of constants and a thin emitter facade — collapse to
  `task_center/audit.py`.

- **`config.py`** is 17 lines for a single integer field
  (`default_attempt_budget=2`). `exceptions.py` is 12 lines for a single
  exception. Both are textbook over-engineering — could be inlined into the
  modules that use them.

### 2.5 Underscore-prefixed files that aren't private

```
task_center/context_engine/recipes/_summaries.py       — imported by 2 modules
task_center/context_engine/recipes/_mission_episode.py — imported by 3 modules
```

The leading `_` says "private" but Python doesn't enforce module privacy and
they're imported across modules. Drop the underscore — it's a lie.

### 2.6 Validation files don't pull their weight

`attempt/validation.py` (93 lines), `mission/validation.py` (49 lines),
`episode/validation.py` (33 lines). All three are bare `assert_X(args) -> None`
free-function modules with no shared protocol, no validator class, no compose
machinery. Their parallel structure suggests a shared abstraction, but no
shared abstraction exists.

**Fix:** one `task_center/invariants.py` (or per-domain protocol) — current
layout creates the appearance of architecture without providing it.

### 2.7 `agent_launch/` is four unrelated concerns under one roof

```
agent_launch/composer.py     — orchestrates resolver + engine + renderer
agent_launch/launcher.py     — asyncio runtime that runs agents
agent_launch/predicates.py   — variant routing predicates registry
agent_launch/resolver.py     — variant routing resolver
```

`predicates` and `resolver` belong with the agent definition layer (or
`agent_routing/`). `composer` belongs with `context_engine/` (it's the engine's
public entry point). `launcher` is the asyncio process runtime — own top-level
concern. Coalescing them under `agent_launch/` because they all touch a launch
is artificial.

### 2.8 `mission/handler.py` is a god file (281 lines)

Owns: mission CRUD, initial episode creation, continuation episode creation,
episode-closed routing across THREE outcome types, mission closure with close
report delivery, manager spawning, AND a synthetic-failure recovery branch in
`_start_continuation_episode`. Hidden behind the modest "Handler" name. Split
along the four verbs.

### 2.9 Cross-package coupling does not match the on-disk layering

The on-disk layout says "we have layers". The import graph says otherwise:

- `attempt/orchestrator.py` imports from `mission/`, `episode/`,
  `context_engine/`, `task/`, `audit/` (via dispatcher).
- `episode/manager.py` imports from `attempt/`.
- `mission/handler.py` imports from `attempt/` AND `episode/`.
- `mission/starter.py` imports from `attempt/`, `episode/`, `entry/`.
- `entry/coordinator.py` imports from `attempt/`, `episode/`, `mission/`,
  `agent_launch/`, `context_engine/`.

Every leaf imports from every other leaf. The package boundaries are
decorative — the actual graph is a complete bipartite mess.

---

## 3. Import-dependency chains — graded D

### 3.1 Cycles broken by `TYPE_CHECKING` and lazy `__getattr__`

- `api.py:32-39` — lazy `__getattr__` to break a cycle on
  `start_task_center_entry_run`. Confession that the dependency graph is
  acyclic by runtime trickery, not by design.
- `attempt/runtime.py:19-24` — `TYPE_CHECKING` for `ContextComposer`,
  `EntryTaskController`, `AttemptOrchestratorRegistry`, all of which are
  *runtime collaborators*. If you can't import them at runtime, the layering
  is wrong.
- `episode/manager.py:41-43` — `TYPE_CHECKING` for `AttemptOrchestrator`. The
  manager calls back into the orchestrator's start path; orchestrator owns
  callback into manager. Circular by design, broken by trickery.
- `episode/registry.py:9-10` and `attempt/orchestrator_registry.py:9-10` —
  `TYPE_CHECKING` for the only thing the registry stores. Registries should
  not be downstream of the things they hold.

### 3.2 Long import paths add friction

```python
from task_center.context_engine.recipes._mission_episode import mission_episode_blocks
from task_center.attempt.orchestrator_registry import AttemptOrchestratorRegistry
from task_center.mission.close_report_delivery import MissionCloseReportRouter
```

Five-segment paths to reach a free function. Combined with the underscore
prefix lie (§2.5), reading any single import line is work.

### 3.3 Persistence is hard-imported, not abstracted

`attempt/runtime.py`, `mission/handler.py`, `episode/manager.py`,
`mission/ancestry.py`, `entry/controller.py`, `entry/coordinator.py` all
import the **concrete** `MissionStore`, `AttemptStore`, `EpisodeStore`,
`TaskCenterStore`, `ContextPacketStore` from `db.stores.*`.

There is exactly **one** Protocol abstraction over a store in the entire
package: `ContextPacketStoreProtocol` in `engine.py:24` (2 methods). The other
five stores are concrete imports. So:

- You cannot substitute an in-memory store without monkey-patching `db.stores.*`.
- You cannot test `MissionStarter` against a fake `MissionStore`.
- You cannot version the store contract independently of the implementation.

`db.stores.*` is the only seam, and it's nailed shut.

### 3.4 Top-of-stack module pretending to be a service

`task_center` reaches into:

- `agents` — agent definition registry
- `db.stores.*` — persistence
- `audit.base` — audit sink
- `engine.api` — agent runner (`from engine.api import run_ephemeral_agent` in
  `launcher.py:103`, deferred to dodge a cycle)
- `runtime.app_factory` — `RuntimeConfig` (TYPE_CHECKING only, fine)
- `tools` — `ExecutionMetadata`
- `message.stream_events` — `StreamEvent`
- `sandbox.api` — sandbox lifecycle (`sandbox_bridge.py:23`, deferred import
  inside the function)

Six other top-level packages in the runtime path. A "lifecycle service" should
expose interfaces and let composition happen at the app-factory layer. As-is,
`task_center` IS the app factory.

### 3.5 Recipe registration is hardcoded, not discoverable

`context_engine/recipes/__init__.py:28-35` declares `_BUILTIN_RECIPES` as a
hardcoded tuple of six imports. `register_builtin_recipes()` iterates them.
Adding a recipe requires editing `__init__.py` AND the new recipe module — the
"two-step" claim in the docstring is true only if you ignore that the second
step *is the bootstrap edit*. There's no entry-point discovery, no decorator
auto-registration, no manifest. The system isn't pluggable, it's hand-wired.

---

## 4. Extensibility / inheritance / interfaces — graded D-

### 4.1 The four real Protocols, and the silence around everything else

These are all the abstract seams in the package:

| Protocol | Methods | Used by |
| --- | --- | --- |
| `AttemptAgentLauncher` (`runtime.py:40`) | 1 | only `EphemeralAttemptAgentLauncher` |
| `AgentResolver` (`resolver.py:42`) | 1 | only `RuleBasedAgentResolver` |
| `PromptRenderer` (`renderer.py:31`) | 1 | only `MarkdownPromptRenderer` |
| `ContextPacketStoreProtocol` (`engine.py:24`) | 2 | only the concrete store |

That's it. Every other lifecycle class — `AttemptOrchestrator`,
`AttemptDispatcher`, `MissionHandler`, `MissionStarter`, `EpisodeManager`,
`EntryTaskController`, `MissionCloseReportRouter`,
`EphemeralAttemptAgentLauncher`, `ContextComposer`, `TaskCenterEntryCoordinator`
— is a fully concrete class with no interface above it. To substitute any of
them in tests you must monkey-patch a method or a constructor.

### 4.2 `AttemptOrchestrator` is a 455-line god class

Eleven private methods. Constructor takes attempt + callback + runtime.
Direct knowledge of planner / generator / evaluator state machines via
`_mark_generator`, `_mark_evaluator`, `apply_planner_failure`,
`apply_plan_submission`, `apply_generator_submission`,
`apply_evaluator_submission`, `apply_mission_close_report`. There is no
strategy hierarchy (`PlannerStage`, `GeneratorStage`, `EvaluatorStage`); the
state machine is hand-rolled in `if attempt.stage == ... elif ...` chains
inside the orchestrator and dispatcher.

### 4.3 The state machine is not a state machine

`AttemptStage` is just an enum. Transition logic is scattered:

- `orchestrator.apply_plan_submission` — PLANNING → GENERATING
  (`orchestrator.py:183`)
- `dispatcher._dispatch_generating` — decides GENERATING → EVALUATING
  (`dispatcher.py:124-125`)
- `dispatcher._spawn_evaluator` — sets stage EVALUATING (`dispatcher.py:279`)
- `orchestrator._mark_evaluator` then triggers close-attempt
- `orchestrator._close_attempt` — any → CLOSED (`orchestrator.py:381`)

Adding a stage (e.g. `REVIEWING` between EVALUATING and CLOSED) means editing
four files. A real state-table or `Stage` strategy would localize transitions.

### 4.4 Adding a new harness role costs 6 file edits

Adding a fourth role today requires:

1. New `HarnessTaskRole` enum value.
2. New `*Submission` DTO in `task/models.py`.
3. New `_mark_*` method on `AttemptOrchestrator`.
4. New `_dispatch_*` branch in `AttemptDispatcher`.
5. New `_report_*_exhaustion` method in `EphemeralAttemptAgentLauncher`
   (lines 263-322).
6. New branch in `_fail_reason_for_role` (`launcher.py:325`).

The `# pragma: no cover - exhaustive over HarnessTaskRole` comments on the
exhaustive switches (e.g. `launcher.py:211`) show the codebase **noticed** the
problem and chose pragma-suppression over polymorphism.

### 4.5 Four near-identical `_build_*_launch` methods

```
orchestrator._build_planner_launch       (orchestrator.py:113)
dispatcher._build_generator_launch       (dispatcher.py:316)
dispatcher._build_evaluator_launch       (dispatcher.py:352)
coordinator._build_entry_launch          (coordinator.py:295)
```

Each one is:

```python
composer = runtime.require_composer()
episode = runtime.episode_store.get(...)   # except entry
bundle = composer.compose(base_agent_name=X, scope=...)
return AgentLaunch(...)
```

The only differences are `base_agent_name` and which `ContextScope` fields
are populated. A single `LaunchBuilder.build(role, attempt|None, task_id)`
collapses all four. As-is, refactoring `AgentLaunch` means touching four files.

### 4.6 Entry-mode vs attempt-mode branching is duplicated four times

The `if attempt_id is None: do entry path else: do attempt path` branch
appears in:

1. `MissionStarter._mark_parent_waiting` (lines 187-218) — `controller is not
   None` then route, else CAS.
2. `MissionStarter._compensate_failed_start` (lines 252-263) — controller-or-CAS.
3. `MissionCloseReportRouter.deliver` (lines 67-94) — controller-or-orchestrator.
4. `EphemeralAttemptAgentLauncher._report_unfinished_running_task` (lines
   187-198) — controller-or-orchestrator.

`EntryTaskController` and `AttemptOrchestrator` both expose
`apply_mission_close_report` with identical signatures. They have no shared
interface. A `CloseReportTarget` Protocol with one polymorphic dispatch site
would eliminate all four branches.

### 4.7 `OrchestratorFactory` is a typedef, not a real factory

```python
OrchestratorFactory = Callable[[Attempt, AttemptClosedCallback], "AttemptOrchestrator"]
```

(`episode/manager.py:49-51`). The only construction site is a hardcoded lambda
in `MissionStarter._build_handler` (`starter.py:139-143`). For a system that
claims pluggable variants, there is no way to inject a different orchestrator
type — you have to rewrite the lambda.

### 4.8 Recipes are functions when they should be classes

`ContextRecipe` is a frozen dataclass holding `id` + `required_scope_fields` +
`build` callable. That's fine for trivial projections, but the recipes here
are 100+ line builders that read multiple stores and conditionally construct
blocks. Recipe inheritance is hand-rolled in `helper.py:75-91` instead of being
a base-class method. Worse, the parent-block demotion logic
(`helper.py:35-44`) is duplicated nowhere because there are only two helpers,
but if a third recipe ever needs to demote-and-inherit, it gets copy-pasted.

A `class Recipe` base with `build(scope, deps)`, `inherit_parent_blocks(...)`,
`required_scope_fields` (class attr) would express the contract that
`ContextRecipe` only fakes via a callable.

### 4.9 `PredicateRegistry` and `RecipeRegistry` are duplicated singletons

`predicates.py:41-66` and `recipes_registry.py:37-66` are two near-identical
classes. Both are class-state singletons (no instance, no DI). Tests have to
manually call `clear()` in teardown — brittle. Either:

- A generic `Registry[T]` class with `register / get / has / list / clear`.
- Or just a module-level `dict` with helper functions.

The current shape is the worst of both: object-oriented overhead with no
instances.

### 4.10 `AttemptRuntime` is a service-locator anti-pattern

`runtime.py:46-65` — frozen dataclass with 8 stores + 2 registries + optional
composer + optional controller + audit_sink. Most call sites use 2-3 fields
(e.g. `MissionStarter._compensate_failed_start` uses `attempt_store`,
`episode_store`, `mission_store`, `task_store`, `manager_registry`,
`entry_task_controller_for`).

Splitting into role-narrow contexts (`PlannerCtx`, `GeneratorCtx`,
`EpisodeLifecycleCtx`) reduces coupling and makes "what does this method
actually need" obvious from the constructor signature. Today every method has
the entire universe wired in.

### 4.11 The lazy `runtime: Callable[[], AttemptRuntime | None]` bootstrap

`coordinator.py:200-222` — builds an `EphemeralAttemptAgentLauncher` with
`runtime=lambda: runtime_ref` where `runtime_ref` is mutated **after**
construction. This circular bootstrap (launcher needs runtime, runtime needs
launcher) is a hint that the launcher belongs *outside* the runtime, not as a
field on it. As-is, the launcher must defensively `_require_runtime()` on
every call (`launcher.py:164`) and other consumers see a maybe-None field.

### 4.12 Three different lifecycle-callback shapes

```python
on_attempt_closed: Callable[[str], None]                   # episode/manager.py:48
ClosureReportSink = Callable[[EpisodeClosureReport], None] # episode/manager.py:47
CloseReportSink   = Callable[[MissionCloseReport], None]   # mission/handler.py:46
```

Three different ad-hoc sinks for the same publish/subscribe pattern. A typed
event bus would unify them and let new subscribers (metrics, persistence,
replay) attach without changing constructors. As-is, every new subscriber is
a constructor change up the call chain.

### 4.13 Audit is a write-only stringly-typed channel

`audit/events.py` — three `str` constants. `audit/emitter.py` — three
hand-coded helpers. No `AuditEvent` subclass per kind, no schema validation.
Adding a new event type means: add a string in `events.py`, add a method in
`emitter.py`, hope every consumer parses `payload` correctly. The audit sink
type is `Mapping[str, Any]`. Schema evolution is on a wing and a prayer.

### 4.14 `ContextScope` is a flat dataclass with 6 optional fields

`context_engine/scope.py:17` — 6 optional `str | None` fields, validated at
runtime via `assert_fields(frozenset)`. There's no per-recipe scope type, so
every recipe redundantly checks its required subset (cf.
`recipes/planner.py:43-51`, `recipes/helper.py:56-64`). The redundant check is
even *necessary* — the `# python -O strips assert` comment in both recipes
explains why. A discriminated-union scope (`PlannerScope`, `HelperScope`)
would catch missing fields at the type level; a dataclass with all-optional
fields catches them at runtime, in production.

### 4.15 Compensation logic is duplicated four times

| Compensator | File | Scope |
| --- | --- | --- |
| `_compensate_failed_start` | `mission/starter.py:220` | mission-start failure |
| `_compensate_startup_failure` | `entry/coordinator.py:321` | entry-launch failure |
| `_close_attempt_after_startup_failure` | `episode/manager.py:172` | retry-start failure |
| `_mark_startup_failed` | `attempt/orchestrator.py:402` | planner-launch failure |

Four overlapping compensation routines with `try/except logger.exception(...)`
patterns. The harshest case — `MissionStarter._deliver_synthetic_failure_close_report`
(lines 282-317) — admits in the docstring that this is a "last-resort" path
that may still leave the parent in `WAITING_MISSION` requiring **manual
recovery**. There's no shared `Compensator` / `Saga` abstraction.

---

## 5. Future flexibility — graded C-

### 5.1 Hardcoded knobs

- `MAX_HANDOFF_DEPTH = 2` is a module constant in `predicates.py:27`. Making
  it config-driven requires reaching into the predicates module from
  `HarnessLifecycleConfig`, which is currently impossible because predicates
  are class-level singletons. The depth threshold drives whole agent variant
  selection — it should be config.
- `default_attempt_budget = 2` lives on `HarnessLifecycleConfig` but
  per-mission overrides have no path. Making episode budgets per-mission means
  editing `MissionHandler.create_initial_episode_with_manager`,
  `MissionHandler.create_continuation_episode_with_manager`, AND threading
  through `MissionStarter`.
- Token budget compression policy in `MarkdownPromptRenderer._compress`
  (`renderer.py:202-243`) is hardcoded `(LOW, MEDIUM)` drop order. Not
  pluggable.

### 5.2 No way to roll out a planner v2

`base_agent_name="planner"` and `base_agent_name="evaluator"` are hardcoded
strings at three call sites (`orchestrator.py:128`, `dispatcher.py:332`, 366).
Resolution flows through process-global `get_definition`. Switching to a
per-mission planner version requires surgery in three places. The cosmetic
`_v1` suffix on recipes does not buy you any actual versioning machinery.

### 5.3 No replay / dry-run / inspection mode

The state machine is deeply entangled with stores. `AttemptOrchestrator.start`
writes to `task_store`, `attempt_store`, calls `agent_launcher.launch`, all in
one method. There is no seam for "compute the next intended state transition
without committing it." That makes time-travel debugging, what-if simulation,
and audit replay impossible.

### 5.4 Untyped `payload` fields

`PlannerSubmission`, `GeneratorSubmission`, `EvaluatorSubmission` carry
`payload: dict[str, Any]` (`task/models.py:73-87`). Future code can't trust
the shape. You have to grep every callsite to know what's in there.

### 5.5 Rigid agent-launch shape

`AgentLaunch` (`runtime.py:27-37`) has fixed fields: `task_id`,
`task_center_run_id`, `attempt_id`, `role`, `agent_name`, `task_input`,
`needs`, `context_packet_id`, `mission_id`. No metadata bag, no extension
point. Adding a per-launch knob (priority, latency budget, retry policy)
requires editing the dataclass and every construction site (4 places — see §4.5).

---

## 6. The five highest-leverage fixes (in priority order)

1. **Pick one name** — TaskCenter or Harness. The dual vocabulary is taxing
   every reader and every column rename. Schema migrations come later; the
   class/file names should converge first.

2. **Introduce a `LifecycleTarget` Protocol** with `apply_mission_close_report`,
   `mark_waiting_mission`, `restore_running_after_failed_mission_start`. Make
   `EntryTaskController` and `AttemptOrchestrator` implement it. Delete the
   four `if attempt_id is None: ... else: ...` branches. (§4.6)

3. **Collapse `api.py` into `__init__.py`, delete `domain.py`.** Make the
   public surface ~5 names. Internal modules import from canonical paths;
   external callers from the package root. (§2.1, §2.2)

4. **Replace the four `_build_*_launch` methods with a single `LaunchBuilder`
   keyed on role + scope.** This unblocks every future change to `AgentLaunch`.
   (§4.5)

5. **Introduce `Store` Protocols** at the `task_center` boundary for the four
   stores `task_center` actually uses. Stop importing concrete store classes.
   This is the prerequisite for any real test seam, in-memory mode, or
   alternative persistence backend. (§3.3)

Mid-term:

- Split `mission/handler.py` into `MissionRepository` (CRUD) +
  `EpisodeFactory` (initial + continuation creation) +
  `EpisodeClosureRouter` (closed-event routing).
- Replace the three ad-hoc lifecycle callbacks with a typed event bus.
- Introduce a `Compensator` / `Saga` for the four overlapping compensation
  routines.
- Per-recipe `Scope` types (`PlannerScope`, `GeneratorScope`, `HelperScope`)
  to move `assert_fields` from runtime to type system.
- Drop `_v1` cosmetic versioning until you actually need v2.

---

## 7. What's actually good (so the review isn't all teeth)

- The DTOs (`Mission`, `Episode`, `Attempt`, `ContextPacket`, `ContextBlock`)
  are immutable, frozen, slotted dataclasses or Pydantic models. Solid baseline.
- `EpisodeClosureReport.outcome` is a discriminated union
  (`TerminalSuccess | SuccessContinue | AttemptPlanFailed`) — properly typed,
  exhaustively dispatched in `MissionHandler.handle_episode_closed`. This is
  the best part of the codebase.
- `assert_*` invariant functions are consistent and read well at the call
  site, even if their packaging is over-broken-up.
- `ContextScope.assert_fields` runtime check, while not type-safe, at least
  fails fast and clearly.
- `MissionCloseReportRouter` correctly identifies "active orchestrator
  required" and refuses to silently degrade. Good defensive posture.
- `ordered_generator_tasks` cycle/dup detection is tight and well-tested in
  shape (`generator_dag.py:17-49`).
- The `# python -O strips assert` comments showing awareness of why
  `assert_fields` doesn't replace explicit guards. Good production thinking.
- Audit emitter doesn't lie about what it knows — `_text` strips and
  None-coerces consistently.
