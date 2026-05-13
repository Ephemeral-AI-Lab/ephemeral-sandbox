---
title: "Role: Evaluator"
tags: ["task-center", "evaluator", "role", "context-recipe", "attempt", "verdict", "submission", "see-also"]
created: 2026-05-13T00:00:00.000Z
updated: 2026-05-13T00:00:00.000Z
sources: []
links: ["task-center-pipeline.md", "context-engine-recipes.md", "role-planner.md", "role-generator.md"]
category: architecture
confidence: high
schemaVersion: 1
---

# Role: Evaluator

The evaluator is the **judge** of one Attempt. Spawned by the dispatcher only after every generator in the DAG has reached `DONE`, it issues a single binary verdict against the planner's evaluation criteria. Its PASS closes the attempt and either terminates the episode or triggers a continuation; its FAILURE feeds the next planner's failure landscape (if budget remains) or terminates the episode.

## Identity in one sentence

> The evaluator inherits the planner's rubric and applies it ruthlessly to the DAG's summaries — without seeing the broader retry history and without authority to rewrite the criteria.

## Place in the pipeline

```
Attempt
  ├── planner
  ├── generator × N    (all DONE)
  └── evaluator        ← (you are here — singleton, last)
```

Exactly one evaluator per Attempt. Deterministic id `{attempt_id}:evaluator` (`task_center/task/ids.py`). Spawned by `AttemptDispatcher._spawn_evaluator` (`task_center/attempt/dispatcher.py:214`) when:

1. The attempt is in stage `GENERATING`, and
2. `all_generators_done(task_records)` is true (every generator status is `DONE`, none `FAILED`/`BLOCKED`).

If _any_ generator FAILED or BLOCKED, the evaluator is **not** spawned — the dispatcher short-circuits to attempt close `FAILED/generator_failed`. The evaluator only runs when there is genuine work to judge.

## Lifecycle

| Event | Effect |
|---|---|
| Last generator becomes `DONE`, none failed | `_spawn_evaluator` upserts evaluator task row (status `RUNNING`), sets `attempt.evaluator_task_id`, transitions attempt stage to `EVALUATING`, composes `evaluator_v1`, launches agent. |
| Agent calls `submit_evaluation_success` | `apply_evaluator_submission(outcome="success")` → evaluator task `DONE`; attempt closes `PASSED`; `EpisodeManager.handle_attempt_closed` runs. |
| Agent calls `submit_evaluation_failure` | Same path with `outcome="failure"` → evaluator task `FAILED`; attempt closes `FAILED/evaluator_failed`. |
| Agent run ends without terminal | Launcher synthesises `apply_evaluator_submission(outcome="failure")`. |
| Evaluator launcher exception | `_launch_evaluator` exception path closes attempt `FAILED/evaluator_failed`. |

The evaluator's lifetime is one agent run. There is no evaluator retry within an attempt — an evaluator failure closes the attempt, and the retry (if any) is at the attempt level.

## Responsibilities

What the evaluator MUST do:

1. **Apply the planner's criteria.** `evaluation_criteria[]` is the verdict basis. The block is REQUIRED priority and rendered last in the prompt so the agent's reading ends on the contract.
2. **Read task summaries, not artifacts.** The evaluator sees `completed_task_summary` blocks (HIGH priority) — `latest_summary_text(task.summaries)` for each generator task. It can use `read_file` and `shell` to inspect the sandbox, but the structured judgment surface is the summaries.
3. **Issue a binary verdict.** Success or failure — no partial credit. The planner's narrowing happens upstream; the evaluator's job is the dichotomy.
4. **Use `ask_resolver` for adjustment, not litigation.** If a criterion is _almost_ met and a small fix would close the gap, dispatch a resolver to make it, then re-check. The `resolver_limit` notification reminder nudges the evaluator toward submitting failure when the resolver loop is running away without resolution.
5. **Exit via exactly one terminal.**

What the evaluator MUST NOT do:

- **Rewrite the criteria.** They are inherited. The submission shape carries `passed_criteria[]` / `failed_criteria[]` for transparency, not for redefinition.
- **Judge against the mission or episode goal.** Mission/Episode framing is in the prompt as REQUIRED context, but `attempt.task_specification` and `evaluation_criteria` are the verdict basis. Promoting the episode goal to a criterion is the planner's failure, not something the evaluator can correct.
- **Look at retry history.** The failed-attempt landscape is _not_ in `evaluator_v1`. The evaluator judges only the present attempt; it has no memory of what prior attempts attempted.
- **Mutate the plan.** No tool exists to do so. The plan is the planner's frozen output.

## Context recipe — `evaluator_v1`

Source: `task_center/context_engine/recipes/evaluator.py:30-106`. Required scope: `{mission_id, attempt_id}`. `episode_id` is optional — falls back to `attempt.episode_id`.

**Block order (rendered top-to-bottom):**

| Position | Block kind | Priority | When present |
|---|---|---|---|
| 1 | `episode_goal` (under `# Mission / Current Episode`) | REQUIRED | episode `sequence_no == 1` |
| 1 | `mission_goal` (under `# Mission`) | REQUIRED | `sequence_no > 1` |
| 2..N | `prior_episode_specification` + `prior_episode_summary` pairs (under `# Previous Episode Results`) | HIGH for immediate prior, MEDIUM for older | `sequence_no > 1` |
| N+1 | `episode_goal` (under `# Current Episode`) | REQUIRED | `sequence_no > 1` |
| N+2 | `task_specification` | **REQUIRED** | `attempt.task_specification` non-empty |
| N+3 | `partial_plan_boundary` | **REQUIRED** | `attempt.continuation_goal` non-empty |
| ... | `completed_task_summary` × N (under `# Dependency Results`) | HIGH | One per id in `attempt.generator_task_ids`; rendered via `latest_summary_text` |
| last | `evaluation_criteria` | **REQUIRED** | `attempt.evaluation_criteria` non-empty, bullet-formatted |

**Why this order:** The prompt opens on contextual framing and closes on the verdict basis. The mission/episode framing is REQUIRED because it grounds the evaluator's reading, but the operative blocks — `task_specification` (REQUIRED), the partial-plan boundary when present, task summaries (HIGH), and `evaluation_criteria` (REQUIRED) — render in the bottom half.

**Notable comparison to `planner_v1`:**

| | `planner_v1` | `evaluator_v1` |
|---|---|---|
| Mission/episode framing | Yes (REQUIRED) | Yes (REQUIRED) |
| Prior episode results | Yes | Yes |
| `failed_attempt_landscape` | **Yes** | **No** |
| `task_specification` | No (planner _writes_ it) | Yes (REQUIRED) |
| `partial_plan_boundary` | No | Yes, when the current attempt is partial |
| Generator task summaries | Yes, for failed prior attempts only | Yes (HIGH, all current-attempt generators) |
| `evaluation_criteria` | No (planner _writes_ it) | Yes (REQUIRED, last) |

The planner writes; the evaluator reads. The planner sees the past; the evaluator sees the present.

**Notable comparison to `generator_v1`:**

| | `generator_v1` | `evaluator_v1` |
|---|---|---|
| Mission/episode framing | **No** | Yes |
| `task_specification` | HIGH (framing) | REQUIRED (verdict basis context) |
| Dependency summaries | One per `task.needs[]` only (subset) | All `attempt.generator_task_ids` (full DAG) |
| `evaluation_criteria` | No | Yes (REQUIRED) |
| Local `planned_task_spec` | Yes (REQUIRED, last) | No |

The generator is per-node; the evaluator is per-attempt. The generator's view is local; the evaluator's view is global.

**Truncation:** `evaluator_v1` has no compression beyond the renderer's standard priority drop (low → medium under budget pressure). REQUIRED and HIGH blocks are never truncated (`renderer.py:215-235`).

## Submission contract

Two terminal tools (`tools/submission/main_agent/evaluator/`):

### `submit_evaluation_success(summary, passed_criteria)`

`submit_evaluation_success.py:22`. `summary` is non-blank prose; `passed_criteria` is an open list (defaults to empty). Calls `apply_evaluator_submission(EvaluatorSubmission(outcome="success", payload={"passed_criteria": ...}))`.

### `submit_evaluation_failure(summary, failed_criteria)`

`submit_evaluation_failure.py:22`. Same shape with `outcome="failure"` and `failed_criteria` payload.

### Verdict consequences

`AttemptOrchestrator.apply_evaluator_submission` drives `EpisodeManager.handle_attempt_closed` (`task_center/episode/manager.py:122`):

| Verdict + attempt state | Episode result |
|---|---|
| PASS, `continuation_goal is None` | `EpisodeClosureReport(outcome=TerminalSuccess)` → episode closes; mission can succeed. |
| PASS, `continuation_goal is not None` | `EpisodeClosureReport(outcome=SuccessContinue(goal=...))` → new continuation episode created with `creation_reason=PARTIAL_CONTINUATION`. |
| FAIL, episode has budget remaining | New attempt created via `create_next_attempt` → new planner spawn with this attempt added to the failed landscape. |
| FAIL, episode budget exhausted | `_close_episode_failed` → `AttemptPlanFailed(failure_summary, attempted_plan_history)` propagates up. |

The evaluator's verdict is the only point in the pipeline where a Mission can succeed.

## Constraints (pre-hooks)

| Gate | Tool | Effect |
|---|---|---|
| `resolve_attempt_submission_context` | both terminals | Caller is the attempt's evaluator task; attempt not closed. Raises `AttemptSubmissionContextError` (not a pre-hook). |

The asymmetry is intentional: the failure path is always reachable, but success requires the resolver loop to have closed cleanly.

## Key insights

**1. The evaluator inherits authority, it does not create it.** The planner submits criteria; the evaluator binds to them. There is no path by which an evaluator can decide "this attempt produced something different but also good" — that judgment is outside its contract. If the planner wrote bad criteria, the evaluator's accurate enforcement still produces a bad verdict. This is why the planner doc emphasizes _criteria are the planner's auto-handcuffs_.

**2. Binary verdict, by design.** `apply_evaluator_submission` accepts only `outcome ∈ {"success", "failure"}`. There is no partial pass, no conditional pass, no "needs revision" verdict. The space of outcomes is dichotomy. Three consequences:

- The planner must scope criteria to what the DAG can actually produce, or risk false negatives.
- The evaluator must not invent middle ground; the only nuanced surface is `passed_criteria[]` / `failed_criteria[]` in the payload, which is informational not authoritative.
- A subjective criterion ("looks good") is structurally problematic — there is no way to render a "looks _kind of_ good" outcome.

**3. The evaluator does not see retry history.** `failed_attempt_landscape` is planner-only. The evaluator judges the present attempt; it does not learn from prior verdicts. This is the right factoring: retrospection lives where action follows it (the planner, who can replan). The evaluator's job is local correctness.

**4. The evaluator sees summaries, not work.** The `completed_task_summary` blocks render `latest_summary_text(task.summaries)` — the last summary the generator wrote on submission. Not the diff. Not the tool calls. Not the conversation. This forces a contract: **generators must write summaries that capture what they did and what is now true**, because that's the surface the judge consults.

The evaluator _can_ use `read_file`/`shell` to inspect the sandbox directly. But the prompt-level rubric application is summary-driven; the sandbox tools exist for adjudicating gaps the summaries don't fully resolve.

**5. PASS is irreversible; FAIL is retryable.** A PASS closes the attempt and may close the entire episode (if no `continuation_goal`). A FAIL closes the attempt but the episode lives on while budget remains. This asymmetry shapes evaluator caution: passing too easily is harder to recover from than failing too strictly.

**6. The evaluator is the only role with a binary closing authority over an episode.** A planner failure or a generator failure also closes the attempt as FAILED, but the episode merely retries. Only the evaluator's PASS can deliver `TerminalSuccess` or `SuccessContinue`. Only its FAILED + exhausted budget delivers `AttemptPlanFailed`. The lifecycle's terminal outcomes flow through this one role.

**7. The resolver-loop signal is asymmetric.** The `resolver_limit` notification reminder fires regardless of whether the evaluator intends to pass or fail. The intent is to keep the evaluator honest about unresolved fixes when considering success — submitting failure honestly is always allowed, but waving through a success when the resolver loop has not closed should be a deliberate decision the evaluator owns.

**8. The episode-frame blocks in `evaluator_v1` are framing only.** Despite being REQUIRED priority, they are not verdict basis. The criteria block — REQUIRED, last — is what the evaluator judges against. The episode goal is in the prompt to help the evaluator interpret the criteria in context, not to provide an alternative rubric.

**9. The evaluator does not run if there's nothing to judge.** The dispatcher's quiescence check (`_dispatch_generating`, `dispatcher.py:117`) closes the attempt as `FAILED/generator_failed` if any generator FAILED or BLOCKED — the evaluator is never spawned. This means an evaluator is a positive signal in itself: someone thought there was a coherent DAG result worth judging.

## Context building workflow

This section traces — end-to-end — how the evaluator's `task_input` string is constructed. The evaluator's recipe shares the mission/episode framing with the planner but **adds** the present attempt's plan plus per-generator summaries, and **omits** the failed-attempt landscape.

### The seven-stage pipeline

```
┌─────────────────────────────────────────────────────────────────────────┐
│  AttemptDispatcher._spawn_evaluator   (attempt/dispatcher.py:214)       │
│      all_generators_done(task_records) == True                          │
│      → upsert evaluator task row (status=RUNNING)                       │
│      → attempt.evaluator_task_id = task.id                              │
│      → attempt stage → EVALUATING                                       │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  composer.compose(
                                   │      recipe_id="evaluator_v1",
                                   │      scope=ContextScope(mission_id, attempt_id))
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  ContextComposer.compose                                                │
│  (task_center/agent_launch/composer.py)                                 │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  engine.build(recipe_id, scope)
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  ContextEngine.build  (engine.py:60)                                    │
│      recipe = RecipeRegistry.get("evaluator_v1")                        │
│      scope.assert_fields({mission_id, attempt_id})                      │
│      (episode_id is *not* required; the recipe derives it from attempt) │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  _evaluator_v1_build  (recipes/evaluator.py:30)                         │
│      attempt   = attempt_store.get(attempt_id)                          │
│      mission   = mission_store.get(mission_id)                          │
│      eid       = scope.episode_id or attempt.episode_id                 │
│      episode   = episode_store.get(eid)                                 │
│                                                                         │
│      blocks  = mission_episode_blocks(mission, episode, episodes)       │
│      if attempt.task_specification:                                     │
│          blocks += [ task_specification block REQUIRED ]                │
│      if attempt.continuation_goal:                                      │
│          blocks += [ partial_plan_boundary block REQUIRED ]             │
│      for task_id in attempt.generator_task_ids:                         │
│          task = task_store.get_task(task_id)                            │
│          blocks += [ completed_task_summary block HIGH ]                │
│      if attempt.evaluation_criteria:                                    │
│          blocks += [ evaluation_criteria block REQUIRED ]               │
│                                                                         │
│      return ContextPacket(target_role="evaluator", blocks=blocks, ...)  │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  ContextPacket
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  MarkdownPromptRenderer.render  (renderer.py:125)                       │
│      group_heading="# Dependency Results" causes all                    │
│      completed_task_summary blocks to render under one group with       │
│      per-task ## subheadings.                                           │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  task_input: str
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  EphemeralAttemptAgentLauncher                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

A subtle but important detail: the evaluator scope **does not require** `episode_id`. The recipe falls back to `attempt.episode_id` (`evaluator.py:45`). This is why the evaluator can be spawned from any context with just `{mission_id, attempt_id}` in scope — the dispatcher does not need to thread episode through.

### The four cases the evaluator sees

The cross-product is again on episode sequence number and on whether the present attempt has a non-empty plan. (Attempts always have evaluation criteria post-planning, so we don't enumerate that dimension.)

| Case | Episode | task_specification | Builders invoked |
|---|---|---|---|
| **A — first episode, normal attempt** | `seq=1` | non-empty | `mission_episode_blocks` (1 block) + plan + N task summaries + criteria |
| **B — continuation episode, normal attempt** | `seq≥2` | non-empty | `mission_episode_blocks` (3+ blocks) + plan + N task summaries + criteria |
| **C — degenerate: empty plan** | any | empty | Plan block elided; summaries + criteria still emitted |
| **D — degenerate: no generators** | any | non-empty | No `completed_task_summary` blocks (only happens for plans whose DAG is empty) |

Cases C and D are corner cases — well-formed planner submissions enforce both, but the recipe is defensive.

### Walk-through: Case A — typical first-episode evaluation

The evaluator for attempt #1 of episode #1, judging a plan with three generator tasks (`gen-1`, `gen-2`, `gen-3`).

```
Stores read:
  attempt_store.get(attempt_id)            → Attempt(
                                                generator_task_ids=["gen-1","gen-2","gen-3"],
                                                task_specification="Build the X with...",
                                                evaluation_criteria=["X exists","tests green","..."],
                                                episode_id=ep-1)
  mission_store.get(mission_id)            → Mission(goal="...")
  episode_store.get(ep-1)                  → Episode(sequence_no=1, goal="...")
  episode_store.list_for_mission(...)      → [Episode#1]
  task_store.get_task("gen-1")             → {summaries:[{summary:"Created module Y..."}], ...}
  task_store.get_task("gen-2")             → {summaries:[{summary:"Added tests Z..."}], ...}
  task_store.get_task("gen-3")             → {summaries:[{summary:"Verified all..."}], ...}

Builder decisions:
  mission_episode_blocks: sequence_no == 1
    → [ episode_goal(heading="# Mission / Current Episode") ]

  attempt.task_specification truthy
    → append task_specification block REQUIRED

  attempt.continuation_goal truthy
    → append partial_plan_boundary block REQUIRED
      text starts with:
        plan_kind: partial
        continuation_goal: ...

  for task_id in ["gen-1","gen-2","gen-3"]:
    latest_summary_text(summaries):
      summaries non-empty → last entry → prefer "summary" → fall back "outcome" → "(empty)"
    → append completed_task_summary block HIGH (group="# Dependency Results")

  evaluation_criteria non-empty
    → join with "- " bullets
    → append evaluation_criteria block REQUIRED

Final block sequence:
  [0] episode_goal                                    REQUIRED
  [1] task_specification                              REQUIRED
  [2] partial_plan_boundary (if partial)              REQUIRED
  [3] completed_task_summary (gen-1)                  HIGH    group=# Dependency Results
  [4] completed_task_summary (gen-2)                  HIGH    group=# Dependency Results
  [5] completed_task_summary (gen-3)                  HIGH    group=# Dependency Results
  [6] evaluation_criteria                             REQUIRED
```

Rendered `task_input` (Case A):

```
# Mission / Current Episode

Build a CSV-import endpoint that streams uploaded files into the warehouse
without buffering more than 8MB in memory, and document the format.

# Attempt Plan

Deliver the streaming CSV importer end-to-end: a POST /import/csv endpoint
that consumes multipart/form-data and pipes each row into the warehouse
writer; a smoke test that loads a 1GB synthetic file under the memory cap;
and a README section documenting the row schema and error reporting.

# Dependency Results

## gen-1

Created routes/import_csv.py with a chunked SpooledTemporaryFile reader.
Streams rows into WarehouseWriter.write_row; respects MAX_BUFFER=8MB
(asserted with resource.getrusage in the route). No new deps added.

## gen-2

Added tests/test_import_csv_stream.py: generates a 1GB file via
hypothesis, asserts RSS stays under 16MB during the upload, and that
all rows reach the warehouse. CI green; runtime ~38s.

## gen-3

Verified routes/import_csv.py against the criteria: endpoint exists,
streaming confirmed, README section "CSV Import" added with row schema
and error format. Two minor wording fixes via resolver; both resolved.

# Evaluation Criteria

- POST /import/csv exists and accepts multipart uploads
- Peak resident memory < 16MB during a 1GB upload
- Row schema documented in README
- Smoke test runs in CI
```

The renderer (`renderer.py:163-179`) groups the three `completed_task_summary` blocks under the single `# Dependency Results` heading because they all carry `metadata["group_heading"] = "# Dependency Results"`. Each block's `metadata["subheading"]` (set to the task id) becomes the `## gen-1` subhead.

### Walk-through: Case B — continuation evaluation

The evaluator for attempt #1 of episode #2; episode #1 already closed successfully via a partial plan.

```
mission_episode_blocks: sequence_no == 2  (>1)
  priors = [Episode#1]    (must have task_specification AND task_summary or ContextEngineError)
  emit:
    - mission_goal(heading="# Mission")
    - prior_episode_specification(Episode#1, HIGH)
    - prior_episode_summary(Episode#1, HIGH)
    - episode_goal(Episode#2, heading="# Current Episode")
  → 4 framing blocks

Then the recipe adds (same as Case A):
  - task_specification (REQUIRED)
  - completed_task_summary × N (HIGH)
  - evaluation_criteria (REQUIRED)
```

Rendered shape:

```
# Mission
...
# Previous Episode Results
## Episode 1 accepted plan
...
## Episode 1 summary
...
# Current Episode
...
# Attempt Plan
...
# Dependency Results
## gen-1
...
## gen-2
...
# Evaluation Criteria
- ...
- ...
```

The framing front-loads context (mission → prior history → current goal), the attempt body sits in the middle, and the rubric closes the prompt. **The evaluator's reading ends on the verdict basis** — this ordering is the recipe's central design choice.

### Where each piece of information comes from

```
                ┌── mission.goal ────────────────► mission_goal block
                │
                │   ┌── episode.goal ────────────► episode_goal block
                │   │                              (heading varies by sequence_no)
                │   │
                │   ├── prior_ep.task_spec ──────► prior_episode_specification block
                │   ├── prior_ep.task_sum. ──────► prior_episode_summary block
                │   │
                │   ├── attempt.task_specification ───► task_specification block
                │   │                                    (REQUIRED)
                │   │
                │   ├── attempt.continuation_goal ────► partial_plan_boundary block
                │   │                                    (REQUIRED, partial attempts only)
                │   ├── attempt.evaluation_criteria ──► evaluation_criteria block
                │   │   "- "-joined into bullets        (REQUIRED, last position)
                │   │
                │   │   for tid in attempt.generator_task_ids:
                │   │     task_store.get_task(tid).summaries
                │   │       → latest_summary_text(...)
                │   │       → "summary" | "outcome" | "(empty)"
                │   │                                  ► completed_task_summary block
                │   │                                    (HIGH, group="# Dependency Results")
mission_store ──┘   │
episode_store ──────┘
attempt_store ──────────────────────► (the full attempt row — plan, criteria, gen ids)
task_store    ──────────────────────► (per-task summaries — latest only)
```

Compared to the planner's provenance map, two differences stand out:

1. The evaluator **reads `task_store`** — once per generator. The planner does not.
2. The evaluator **does not read** the `attempt_store.list_for_episode` collection. Failed-attempt history is invisible by design (see _Key insight #3_ above).

### The latest-summary-only contract

Per-task summary collection goes through `latest_summary_text(task["summaries"])` (`_summaries.py:14`):

```python
def latest_summary_text(summaries):
    if not summaries:          return "(no summary recorded)"
    last = summaries[-1]
    if not isinstance(last, dict):  return str(last)
    return str(last.get("summary") or last.get("outcome") or "(empty)")
```

That means:

- **Only the last entry** of `task["summaries"]` is surfaced. If a generator wrote three summary updates, only the third reaches the evaluator.
- **Field priority** inside the entry: `"summary"` wins; falls back to `"outcome"`; falls back to the literal `"(empty)"` string.
- **No structural inspection of artifacts**: the evaluator does not see `artifacts[]` or `unresolved_issues[]` in this block. Those payload fields are persisted on the task row but the prompt-level surface is the prose summary alone.

The implication for generator authors is in [[role-generator]] — the prose summary must be self-contained because it is the only structured handoff the evaluator gets.

### Empty / corner cases

| Condition | Result |
|---|---|
| `attempt.task_specification` is empty/None | The `task_specification` block is **not** emitted. The evaluator still receives framing + summaries + criteria. (Well-formed planners never produce this.) |
| `attempt.evaluation_criteria` is empty list | The `evaluation_criteria` block is **not** emitted. The evaluator must judge against framing alone — a structurally broken state; the planner schema rejects this. |
| `attempt.generator_task_ids` empty | No `completed_task_summary` blocks. The evaluator has nothing to judge — the dispatcher path that leads here is already pathological. |
| A `task_id` in `generator_task_ids` returns `None` from `task_store.get_task` | `ContextEngineError`; evaluator context refuses to assemble a partial dependency-results frame. |

### Truncation

`evaluator_v1` has no recipe-level cap on summary count. If a DAG produced 50 generator tasks, all 50 `completed_task_summary` blocks render. Compression falls to the renderer:

- All `evaluator_v1` block kinds are REQUIRED or HIGH.
- `MarkdownPromptRenderer._compress` (`renderer.py:201-235`) only truncates LOW and MEDIUM.
- **No block in `evaluator_v1` is currently truncatable.** Under budget pressure the prompt overruns; the renderer does not enforce a hard ceiling.

This is intentional: every piece of context the evaluator has is judgment-load-bearing. If the DAG is wide enough to overflow, the planner is producing a plan the evaluator structurally cannot judge in one prompt — a planning-side problem, not a rendering-side one.

### Failure shapes inside the recipe

| Where it fails | Trigger | Effect |
|---|---|---|
| `ContextEngine.build` | scope missing `mission_id` or `attempt_id` | `AssertionError` from `scope.assert_fields` → evaluator launch fails → attempt closes `FAILED/evaluator_failed`. |
| `attempt_store.get` | attempt row missing | `ContextEngineError("Attempt ... not found")` |
| `mission_store.get` | mission row missing | `ContextEngineError("Mission ... not found")` |
| `episode_store.get` | episode row missing for derived id | `ContextEngineError("Episode ... not found")` |
| `_previous_episode_result_blocks` | a closed prior episode has `task_specification is None` or `task_summary is None` | `ContextEngineError` — same chain-integrity invariant as planner. |
| `task_store.get_task` returns None | task id in `generator_task_ids` not found | `ContextEngineError`; evaluator launch fails rather than judging an incomplete frame. |

## Failure modes

| Mode | Cause | Effect |
|---|---|---|
| Submission failure | Agent submits explicit failure | Attempt closes `FAILED/evaluator_failed`; episode may retry. |
| Unfinished agent run | Run ends with no terminal | Launcher synthesises `apply_evaluator_submission(outcome="failure")`. |
| Resolver saturation | ≥4 unresolved resolver calls before success | `resolver_limit` notification reminder fires; evaluator is expected to submit failure with remaining gaps. |
| Launcher exception | Sandbox error, agent crash | `_launch_evaluator` exception path closes attempt `FAILED/evaluator_failed`. |
| Submission to closed attempt | Race between submission and attempt close | `resolve_attempt_submission_context` raises `AttemptSubmissionContextError` once the attempt is closed. |

## See also

- [[role-planner]] — writes the `evaluation_criteria` and `task_specification` the evaluator reads.
- [[role-generator]] — writes the summaries the evaluator judges.
- [[task-center-pipeline]] — Episode/Mission closure logic that consumes the evaluator's verdict.
- [[context-engine-recipes]] — `evaluator_v1` block builders.
