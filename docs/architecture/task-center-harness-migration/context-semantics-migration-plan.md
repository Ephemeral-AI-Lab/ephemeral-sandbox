# TaskCenter Context Semantics and Filesystem Naming Migration Plan

**Status:** In progress - prompt semantics and auxiliary block cleanup are
implemented; documentation path and runtime package/file renames are pending.

**Goal:** migrate the LLM-facing context language and filesystem names from
`ComplexTaskRequest -> TaskSegment -> HarnessGraph -> Task` to the clearer
semantic frame `Mission -> Episode -> Attempt -> Task`, without forcing an
immediate persistence, database table, or DTO rename.

This plan is about context semantics: headings, summaries, recipe inputs,
renderer ordering, documentation paths, Python package/file paths, and tests.
Durable class/table/DTO renames belong to the broader naming refactor and are
not required for this migration.

## 1. Decisions

Use these terms in rendered prompts:

| Runtime source | LLM-facing term | Meaning |
| -------------- | --------------- | ------- |
| `ComplexTaskRequest` | Mission | The full goal of the current complex-task request. |
| `TaskSegment` | Episode | One self-contained continuation slice of the mission. |
| `HarnessGraph` | Attempt | One planner -> generator -> evaluator try within an episode. |
| `TaskCenterTaskRecord` | Task | One atomic planner, generator, evaluator, or helper task. |

`Mission` is request-local. Parent executor or original user context may appear
as background evidence, but it is not the Mission contract for this
`ComplexTaskRequest`.

`Episode` is preferred over `segment` because segment implies the smallest unit.
That is wrong here: the smallest executable unit is the task. `Episode` also
fits the partial-planning lifecycle: an episode may complete the mission or
produce a continuation goal for the next episode.

Do not use `Current Episode Task`. `Task` already names the bottom layer, so
putting it into the episode heading blurs the hierarchy. Use `Current Episode`.

Filesystem names should follow the same semantic frame where they describe these
domain layers:

| Current filesystem name | Target filesystem name | Notes |
| ----------------------- | ---------------------- | ----- |
| `complex_task` | `mission` | Request-local delegated goal layer. |
| `segment` | `episode` | Vertical continuation slice layer. |
| `harness_graph` | `attempt` | One planner -> generator DAG -> evaluator try. |
| `task` | `task` | Already matches the bottom-layer term. |

Do not rename service-oriented folders merely because they touch these layers.
`context_engine` stays `context_engine`: it composes context packets, not a
Mission/Episode/Attempt lifecycle object.

## 2. Episode 1 Special Case

For episode 1, the mission and current episode are equivalent:

```text
mission_goal == episode_goal
```

No previous episode has narrowed or advanced the mission yet, so the prompt
must not render duplicate mission and episode sections. Render one merged
section:

```md
# Mission / Current Episode

<full mission goal>
```

For episode 2+, render the layers separately:

```md
# Mission

<full mission goal>

# Previous Episode Results

<accepted prior episode projection>

# Current Episode

<current continuation goal>
```

The correct claim is narrow: "episode 1 is equivalent to the mission." Do not
generalize that to "an episode is the mission."

## 3. Prompt Sections

The storage block kinds can remain mechanical, but renderer output should use
these headings:

| Current block kind | Rendered section | Notes |
| ------------------ | ---------------- | ----- |
| `complex_task_goal` | `# Mission` | Omitted as a separate section for episode 1. |
| `segment_goal` | `# Mission / Current Episode` or `# Current Episode` | Merged for episode 1; separate for episode 2+. |
| `prior_segment_specification`, `prior_segment_summary` | `# Previous Episode Results` | Accepted prior episode projection in sequence order. |
| `failed_graph_landscape` | `# Failed Attempts` | Failed attempts inside the current episode. |
| `task_specification` | `# Attempt Plan` | Planner-emitted plan for the current attempt. |
| `dependency_summary`, `completed_task_summary` | `# Dependency Results` | Grouped heading with task subsections. |
| `planned_task_spec` | `# Assigned Task` | Generator's local assignment; always last for generator. |
| `evaluation_criteria` | `# Evaluation Criteria` | Authoritative pass/fail criteria for the current attempt; always last for evaluator. |

Use `Evaluation Criteria` because it mirrors the `submit_full_plan` and
`submit_partial_plan` field name. Do not use `Evaluation Goal`: `goal` is already
the Mission/Episode/continuation vocabulary, and evaluator inputs must be
falsifiable criteria rather than an aspirational target.

Dependency results must render as one grouped section:

```md
# Dependency Results

## <task name or id>

<summary/result content>

## <task name or id>

<summary/result content>
```

Internally, generator inputs can still use `dependency_summary` and evaluator
inputs can still use `completed_task_summary`. The LLM-facing vocabulary should
be unified as `Dependency Results`.

## 4. Role Context Contracts

### Planner

Episode 1:

```text
Mission / Current Episode -> Failed Attempts (if any)
```

Episode 2+:

```text
Mission -> Previous Episode Results -> Current Episode -> Failed Attempts (if any)
```

Planner context defines the next attempt. Failed attempts are retry evidence
inside the same episode, not previous episode results.

### Generator

```text
Attempt Plan -> Dependency Results -> Assigned Task
```

The generator should not receive the full mission or episode history by
default. If the planner wants a generator to account for wider mission context,
that requirement belongs in the assigned task.

`Assigned Task` must be the final section so the generator ends on its concrete
local obligation.

### Evaluator

Episode 1:

```text
Mission / Current Episode -> Attempt Plan -> Dependency Results -> Evaluation Criteria
```

Episode 2+:

```text
Mission -> Previous Episode Results -> Current Episode -> Attempt Plan -> Dependency Results -> Evaluation Criteria
```

The evaluator receives the mission and episode frame as orientation. It still
judges the current attempt against the current attempt plan and evaluation
criteria. Mission, previous episode, and current episode frames are
non-authoritative for pass/fail; they must not broaden what the evaluator accepts
or rejects.

`Evaluation Criteria` must be the final section so the evaluator ends on the
pass/fail contract.

## 5. Summary Semantics

The target LLM-facing summary vocabulary is:

| Heading | Content source today | Target meaning |
| ------- | -------------------- | -------------- |
| `Previous Episode Results` | `TaskSegment.task_specification` and `TaskSegment.task_summary` from closed prior segments | Accepted prior episode projection available today: the accepted attempt plan plus the closed episode summary. |
| `Dependency Results` | Upstream task summaries for generator; completed generator/verifier summaries for evaluator | Results produced by prerequisite tasks inside the current attempt. |
| `Failed Attempts` | Failed graph landscape blocks | Retry evidence from failed attempts inside the current episode. |

Current storage only has a partial episode-result projection:
`TaskSegment.task_summary` comes from the passing evaluator summary and
`TaskSegment.task_specification` comes from the passing graph. That is enough
for the prompt-heading migration, but it is not a full episode-result model.

A later summary phase should introduce richer episode results with:

- completed work,
- continuation goal,
- artifact references,
- residual risks,
- attempted-plan history.

Until that richer model exists, do not claim `Previous Episode Results` contains
artifacts, residual risks, or a continuation handoff beyond what the current
closed episode summary actually records.

Do not block the prompt semantics migration on that richer model.

## 6. Migration Phases

Implementation state:

- Phases 1-7 are implemented for the live context-engine, helper-agent, planner
  variant, and main-agent prompt surfaces.
- Phases 8-9 are intentionally pending because they move documentation and
  runtime filesystem paths.

### Phase 1 - Renderer Contract

- Recipes must emit blocks in role-specific semantic presentation order, either
  directly or through generic block metadata such as `heading`, `group`, or
  `section_order`.
- The markdown renderer must stay role-agnostic and preserve packet order after
  compression.
- Keep priority for compression only.
- Preserve required blocks under token pressure, but do not let priority sorting
  override semantic order.
- Add grouped rendering for `Dependency Results`.
- Support per-block headings through metadata where that keeps recipes simple.
- Remove `parent_question` and `capability_note` from the renderer's first-class
  heading templates. They are not Mission / Episode / Attempt / Task concepts.

Verification:

- A mixed-priority packet renders in semantic packet order, not priority order.
- Compression preserves the order of blocks that remain after truncation.
- Dependency summaries render as `# Dependency Results` with `## ...`
  subsections.
- Rendered prompts contain no `# Parent question` or `# Capability note`
  sections.

### Phase 2 - Planner Recipe

- For episode 1, emit/render one `Mission / Current Episode` frame.
- For episode 2+, render separate `Mission`, `Previous Episode Results`, and
  `Current Episode` frames.
- Continue using failed attempt landscape only for attempts inside the current
  episode.

Verification:

- Episode 1 planner context has no duplicate mission/current-episode content.
- Episode 2+ planner context includes prior accepted episode results before the
  current episode.
- Failed attempts remain separate from previous episode results.

### Phase 3 - Generator Recipe

- Render the current attempt plan first.
- Render dependency task results under grouped `Dependency Results`.
- Render the assigned local task last.
- Do not add mission or episode history by default.

Verification:

- Dependency results include all ready upstream dependency summaries.
- `Assigned Task` is the last top-level section.
- Mission/episode headings are absent unless explicitly added through a task
  assignment.

### Phase 4 - Evaluator Recipe

- Add mission and episode framing to evaluator context.
- Use the episode 1 merged-frame special case.
- Render current attempt plan before dependency results.
- Render completed generator/verifier summaries as `Dependency Results`.
- Render evaluation criteria last.

Verification:

- Episode 1 evaluator context order is:
  `Mission / Current Episode -> Attempt Plan -> Dependency Results -> Evaluation Criteria`.
- Episode 2+ evaluator context order is:
  `Mission -> Previous Episode Results -> Current Episode -> Attempt Plan -> Dependency Results -> Evaluation Criteria`.
- Evaluator tests assert that mission/episode context is framing only, while
  pass/fail authority still comes from current attempt evaluation criteria.

### Phase 5 - Tests and Fixtures

Add focused tests near the context engine:

- planner episode 1 merged heading,
- planner episode 2+ split headings,
- generator dependency grouping and assigned-task-last order,
- evaluator episode 1 order,
- evaluator episode 2+ order,
- compression preserves required blocks while retaining semantic order,
- `dependency_summary` and `completed_task_summary` both render as dependency
  result subsections,
- no recipe or renderer test depends on `parent_question`,
- no launch/composer test injects `capability_note` as a context block.

Use a representative benchmark prompt, such as the first PR description from
`backend/config/benchmarks/sweevo_gpt5_2025_08_07_pr_descriptions.csv`, as a
manual demonstration fixture for Mission -> Episode -> Attempt -> Task
semantics. The CSV must be parsed with a real CSV parser because the
`pr_description` field is multiline.

### Phase 6 - Later Summary Model

After prompt semantics are stable, design a first-class episode-result summary.
That phase can replace the current `TaskSegment.task_summary` projection with a
shape that explicitly records continuation goal, artifacts, residual risks, and
attempted-plan history.

### Phase 7 - Legacy Auxiliary Block Cleanup

Delete auxiliary context-block concepts that do not belong to the new semantic
frame:

- Remove `ContextBlockKind.PARENT_QUESTION`.
- Remove `ContextBlockKind.CAPABILITY_NOTE`.
- Remove renderer headings for `parent_question` and `capability_note`.
- Replace helper-agent parent-task framing with existing parent-context
  inheritance or a role-local assigned-task section; do not introduce a new
  top-level context kind for it.
- Remove planner variant `required_context_blocks` that only emit
  `capability_note`. The selected variant's terminal-tool surface is the hard
  gate; do not duplicate it as a prompt block.
- Update tests and agent markdown that reference `parent_question` or
  `capability_note`.

Verification:

- `rg "parent_question|capability_note" backend/src backend/tests` returns no
  live code or test references.
- Planner full-only selection still hides `submit_partial_plan` through the
  selected variant's terminals.
- Helper recipes still preserve parent context without a `parent_question`
  block kind.

### Phase 8 - Documentation Filesystem Rename

Rename the architecture-doc package and old-object filenames so durable docs
match the Mission / Episode / Attempt / Task vocabulary.

Recommended doc path map:

| Current path | Target path |
| ------------ | ----------- |
| `docs/architecture/task-center-harness-migration.md` | `docs/architecture/task-center-mission-episode-attempt.md` |
| `docs/architecture/task-center-harness-migration/` | `docs/architecture/task-center-mission-episode-attempt/` |
| `complex-task-workflow-overview.md` | `mission-episode-attempt-workflow-overview.md` |
| `context-semantics-migration-plan.md` | `mission-episode-attempt-context-migration-plan.md` |
| `phase-01-graph-and-attempt-model.md` | `phase-01-mission-episode-attempt-model.md` |
| `phase-02-harness-graph-orchestrator-lifecycle.md` | `phase-02-attempt-orchestrator-lifecycle.md` |
| `phase-04-complex-task-spawning.md` | `phase-04-mission-spawning.md` |

Leave phase implementation-plan and implementation-report filenames in place
unless their basename contains an old domain term. Their phase number already
gives the durable navigation key.

Verification:

- The phase index links resolve after the folder rename.
- `rg "task-center-harness-migration|complex-task-workflow-overview|phase-01-graph-and-attempt-model|phase-02-harness-graph-orchestrator-lifecycle|phase-04-complex-task-spawning" docs README.md`
  returns no stale links except historical notes that intentionally describe the
  old path.
- `git diff --name-status` shows the doc moves as renames rather than delete/add
  churn where possible.

### Phase 9 - Runtime Package/File Rename

Rename Python package and file paths that encode the old domain nouns while
leaving persistent schemas and DTO/class names unchanged until the durable
naming refactor.

Recommended runtime path map:

| Current path | Target path |
| ------------ | ----------- |
| `backend/src/task_center/complex_task/` | `backend/src/task_center/mission/` |
| `backend/src/task_center/complex_task/request.py` | `backend/src/task_center/mission/mission.py` |
| `backend/src/task_center/complex_task/handler.py` | `backend/src/task_center/mission/handler.py` |
| `backend/src/task_center/complex_task/close_report_delivery.py` | `backend/src/task_center/mission/close_report_delivery.py` |
| `backend/src/task_center/complex_task/ancestry.py` | `backend/src/task_center/mission/ancestry.py` |
| `backend/src/task_center/segment/` | `backend/src/task_center/episode/` |
| `backend/src/task_center/segment/segment.py` | `backend/src/task_center/episode/episode.py` |
| `backend/src/task_center/segment/manager.py` | `backend/src/task_center/episode/manager.py` |
| `backend/src/task_center/segment/closure_report.py` | `backend/src/task_center/episode/closure_report.py` |
| `backend/src/task_center/harness_graph/` | `backend/src/task_center/attempt/` |
| `backend/src/task_center/harness_graph/orchestrator.py` | `backend/src/task_center/attempt/orchestrator.py` |
| `backend/src/task_center/harness_graph/runtime.py` | `backend/src/task_center/attempt/runtime.py` |
| `backend/src/task_center/harness_graph/state.py` | `backend/src/task_center/attempt/state.py` |
| `backend/src/task_center/harness_graph/generator_dag.py` | `backend/src/task_center/attempt/generator_dag.py` |

Keep `backend/src/task_center/task/` unchanged because `Task` remains the
bottom-layer concept. Keep database model files and persisted column/table names
unchanged in this pass; compatibility is more important than cosmetic schema
alignment.

Update imports directly and avoid long-lived compatibility wrappers. A temporary
old-path wrapper is acceptable only when it keeps the rename reviewable, and the
same migration must include a deletion step for those wrappers.

Verification:

- `rg "task_center\\.complex_task|task_center\\.segment|task_center\\.harness_graph" backend/src backend/tests`
  returns no live imports after the move.
- `rg "complex_task/|segment/|harness_graph/" backend/src backend/tests docs`
  returns no stale path references except historical notes.
- Focused TaskCenter tests and static checks pass after import rewrites:
  `uv run pytest backend/tests/task_center -q`,
  `uv run pytest backend/tests/test_tools -q`,
  `uv run ruff check backend/src/task_center backend/tests/task_center`.

## 7. Implementation Touchpoints

Expected code touchpoints:

- `docs/architecture/task-center-mission-episode-attempt.md`
- `docs/architecture/task-center-mission-episode-attempt/`
- `backend/src/task_center/context_engine/renderer.py`
- `backend/src/task_center/context_engine/packet.py`
- `backend/src/task_center/context_engine/recipes/planner.py`
- `backend/src/task_center/context_engine/recipes/generator.py`
- `backend/src/task_center/context_engine/recipes/evaluator.py`
- `backend/src/task_center/context_engine/recipes/graph_landscape.py`
- `backend/src/task_center/context_engine/recipes/helper.py`
- `backend/src/task_center/agent_launch/resolver.py`
- `backend/src/task_center/mission/`
- `backend/src/task_center/episode/`
- `backend/src/task_center/attempt/`
- `backend/src/agents/main_agent/planner/agent.md`
- `backend/tests/task_center/context_engine/`
- `backend/tests/task_center/`

Do not rename persistence models, database tables, or DTO/class names as part of
this plan. Filesystem names may move to Mission / Episode / Attempt first.

## 8. Migration Exit Criteria

- Rendered prompts use Mission / Episode / Attempt / Task semantics for planner,
  generator, and evaluator roles.
- Episode 1 renders `# Mission / Current Episode` and does not duplicate the
  same goal under two headings.
- Episode 2+ renders `# Mission`, `# Previous Episode Results`, and
  `# Current Episode` separately.
- Generator context contains `# Dependency Results` when upstream dependency
  summaries exist and always ends with `# Assigned Task`.
- Evaluator context contains `# Dependency Results` and always ends with
  `# Evaluation Criteria`.
- Priority remains a compression policy, not a presentation-order policy.
- Evaluator mission/episode framing does not change the pass/fail policy for the
  current attempt.
- `parent_question` and `capability_note` are removed from live context-engine
  code, tests, renderer headings, and planner variant context blocks.
- Architecture docs and runtime package/file names use Mission / Episode /
  Attempt / Task filesystem vocabulary where those paths name lifecycle layers.
- Old `complex_task`, `segment`, and `harness_graph` import paths are removed or
  explicitly temporary wrappers with a same-phase deletion step.

## 9. Non-goals

- No table, DTO, persisted column, or durable class rename.
- No retry-budget policy change.
- No planner partial-plan gate change.
- No new multi-episode look-ahead system.
- No full episode-result persistence model in this pass.
