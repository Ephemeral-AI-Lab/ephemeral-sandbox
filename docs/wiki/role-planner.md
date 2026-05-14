---
title: "Role: Planner"
tags: ["task-center", "planner", "role", "context-recipe", "attempt", "episode", "submission", "see-also"]
created: 2026-05-13T00:00:00.000Z
updated: 2026-05-13T00:00:00.000Z
sources: []
links: ["task-center-pipeline.md", "context-engine-recipes.md", "role-generator.md", "role-evaluator.md", "engine-query-loop-llm-seam.md"]
category: architecture
confidence: high
schemaVersion: 1
---

# Role: Planner

The planner is the **rubric author** for one Attempt. It designs a single executable plan — a generator DAG, a prose contract for that DAG, and a set of falsifiable evaluation criteria — and commits it via exactly one terminal call. After submission, the planner has no further influence: dispatcher, generators, and evaluator carry the plan forward until the Attempt closes.

## Identity in one sentence

> The planner converts an Episode goal into a frozen plan-contract that downstream roles must produce against and judge against.

## Place in the pipeline

```
Mission → Episode → Attempt
                      └── planning  ← (you are here)
                          generating
                          evaluating
                          closed
```

A planner task is created when an Attempt enters stage `planning` (`AttemptStage.PLANNING`, `task_center/attempt/state.py:11`). Exactly one planner per Attempt; deterministic id `{attempt_id}:planner` (`task_center/task/ids.py`).

The planner is **not** a Mission. It is a single TaskCenter task with role `planner`. Its lifetime is one agent run that ends on a single terminal submission. If a partial plan is committed in any ancestor mission, the planner_full_only variant is selected for descendants (see _Variants_).

## Lifecycle

| Event | Effect |
|---|---|
| `AttemptOrchestrator.start()` | Creates planner task row (status `RUNNING`), composes `planner_v1` context, launches agent. |
| Planner agent emits `submit_full_plan` | Validates, calls `apply_plan_submission(kind="full")` → planner task `DONE`, attempt stage→`generating`, generator rows inserted `PENDING`. |
| Planner agent emits `submit_partial_plan` | Same as full + records `continuation_goal`; episode chain will branch on evaluator PASS. |
| Agent run ends without terminal | `EphemeralAttemptAgentLauncher._report_unfinished_running_task` synthesizes `apply_planner_failure` → attempt closes `FAILED`/`planner_failed`. |
| Validity error inside `apply_plan_submission` | `TaskCenterInvariantViolation` returned to tool result; agent may retry submission within the same turn. |

The planner has **no read access to live attempt state during planning**. It reasons only from what `planner_v1` placed in its prompt.

## Responsibilities

What the planner MUST do:

1. **Read the episode contract.** For episode 1, the mission goal and episode goal collapse into one block. For episode 2+, the planner sees the mission goal, every prior closed episode's spec+summary, and the current episode goal as three distinct sections.
2. **Read the failure landscape.** If `Failed Attempts` is present, prior plans in this episode failed. The planner is the only role that sees this — it must use it to diagnose what to drop, narrow, or restructure.
3. **Author four contracts in one submission:**
   - `task_specification` — prose for the **evaluator** describing what this DAG delivers as a whole.
   - `evaluation_criteria[]` — falsifiable conditions the **evaluator** will judge against. Binary; no partial credit downstream.
   - `tasks[]` — the **dispatcher's** DAG: ids, agent names, dependency edges.
   - `task_specs{id: str}` — per-task instruction read by the **generator** for that node.
4. **Choose full vs partial** (when both terminals are available). Partial is appropriate when the attempt delivers a coherent, bounded slice and the remainder is large enough to deserve a fresh episode.
5. **Commit once.** No iterating mid-attempt. Plain text emitted before the terminal call is reasoning, not a plan.

What the planner MUST NOT do:

- **Run the work.** It has no `write_file`, `edit_file`, or `shell` tool. By design, the planner cannot empirically test its plan — it must commit on reasoning alone. (`allowed_tools: read_file, run_subagent, ask_advisor`.)
- **Skip lifecycle stages.** No tool exists to close an attempt, episode, or mission directly.
- **Replan after submission.** The plan is frozen at `apply_plan_submission`. Replanning happens only through an attempt retry, which spawns a new planner task with the previous attempt added to the failed landscape.
- **Re-derive the episode goal in task_specs.** Inlining the broader contract into each task's local instruction is the antipattern — generators receive the attempt's `task_specification` as a separate framing block.

## Context recipe — `planner_v1`

Source: `task_center/context_engine/recipes/planner.py:37-77`. Required scope: `{mission_id, episode_id, attempt_id}`.

**Block order (rendered top-to-bottom):**

| Position | Block kind | Priority | When present | Source |
|---|---|---|---|---|
| 1 | `episode_goal` (under `# Mission / Current Episode`) | REQUIRED | episode `sequence_no == 1` | `episode.goal` |
| 1 | `mission_goal` (under `# Mission`) | REQUIRED | `sequence_no > 1` | `mission.goal` |
| 2..N | `prior_episode_specification` + `prior_episode_summary` pairs (under `# Previous Episode Results`) | HIGH for immediate prior, MEDIUM for older | `sequence_no > 1`, one pair per closed predecessor | `episode.task_specification` + `episode.task_summary` |
| N+1 | `episode_goal` (under `# Current Episode`) | REQUIRED | `sequence_no > 1` | `episode.goal` |
| last | `failed_attempt_landscape[]` (under `# Prior Failed Attempts`) | HIGH | any failed attempt in current episode | accepted plan, generator outcome status summary, useful generator summaries, and evaluator criteria/summary only when an evaluator ran |

**Why this order:** The episode contract anchors the front of the prompt; the failure landscape closes it. The planner ends its read on retry evidence so the most-recent failure shapes its planning choices.

**What is deliberately absent:**

- No generator results from this attempt (none exist yet — planner runs before generating).
- No evaluator output (none exists yet).
- No current-attempt generator results (none exist yet — planner runs before generating).
- No sandbox state. The planner cannot inspect the filesystem.
- No nested-mission context. A planner planning an attempt inside a child Mission still sees its own Mission/Episode framing, not the parent's.

**Retry landscape size.** Every failed attempt in the current episode is
rendered. The recipe does not collapse older failures, cap generator status
entries, or truncate useful generator summaries inside a failed-attempt block.

**Failure modes of the recipe itself:**

- Missing prior episode's `task_specification` or `task_summary` → `ContextEngineError` (`_mission_episode.py:78-82`). The episode chain's integrity is the recipe's invariant.
- Missing mission or episode store row → `ContextEngineError`.

## Submission contract

Two terminal tools, sharing `PlannerSubmissionBaseInput` (`tools/submission/main_agent/planner/_schemas.py:44`):

### `submit_full_plan(task_specification, evaluation_criteria, tasks, task_specs)`

The DAG covers the entire `Current Episode`. Evaluator PASS closes the episode terminally; mission can succeed.

### `submit_partial_plan(...same..., continuation_goal)`

The DAG covers a bounded slice; `continuation_goal` is the verbatim contract for the next episode. Evaluator PASS triggers `EpisodeManager._close_episode_passed` → `SuccessContinue(goal=continuation_goal)` → `MissionHandler.create_continuation_episode_with_manager` → new Episode with `creation_reason=PARTIAL_CONTINUATION`.

### Hard validity (rejected pre-orchestrator)

Built into `build_planner_submission` (`_schemas.py:86-152`):

- Unique task ids.
- `task_specs` keys exactly equal task ids (no missing, no extra).
- Every `deps` entry refers to an id in `tasks`.
- DAG is acyclic — enforced by `ordered_generator_tasks` topological sort (`task_center/attempt/generator_dag.py:17`).
- `task_specification`, every criterion, every task spec, and `continuation_goal` (when present) are non-blank.
- Every `agent_name` is registered and has `role ∈ {executor, verifier}` (`_schemas.py:79-83`).
- Caller is the attempt's own planner task (`task_id == attempt.planner_task_id`).

A rejection returns an error tool result; the agent can correct and call again within the same turn — but the agent run ends on the first accepted terminal.

## Variants

`agents/profile/main/` defines two planner variants:

| Variant | When selected | Difference |
|---|---|---|
| `planner` | default | Both `submit_full_plan` and `submit_partial_plan` are available. |
| `planner_full_only` | when the mission ancestry is nested under another attempt (`when: nested_mission_depth_gt_1`) | Only `submit_full_plan` is exposed. System prompt explicitly forbids deferring remainder work. |

**Why `planner_full_only` exists.** Partial planning creates an episodic continuation _on top of_ the current episode. Allowing a descendant planner to _also_ partial-plan would make the continuation chain ambiguous: whose `continuation_goal` extends the parent's mission? The depth rule eliminates the question — any planner running inside a nested mission (`nested_mission_depth > 1`) must fully cover its scope. The depth helper lives at `task_center/mission/ancestry.py:nested_mission_depth`; the predicate is registered in `task_center/agent_launch/predicates.py`.

## Constraints

Planner submission has no pre-hook. Structural checks happen inside `resolve_attempt_submission_context`:

- A live `AttemptRuntime` is bound to the call.
- The current TaskCenter task id matches a task row with `role == "planner"`.
- The attempt that owns the planner task is not already closed.

Failures surface as `AttemptSubmissionContextError`, which the tool converts into a failed `ToolResult`. Beyond that, planner submission is gated only by its own schema validators.

## Key insights

**1. The planner is fire-and-forget.** Submission is its only state mutation, and after the call the planner cannot influence what happens to its plan. This is intentional: it forces the planner to write _durable_ artifacts. Every field (`task_specification`, `evaluation_criteria`, `task_specs`, `continuation_goal`) will be read by some other role that does not have the planner's context. Write them so a fresh agent picking them up cold can act without reconstructing what the planner was thinking.

**2. Four audiences, one submission.** The four submission fields target four distinct readers:

| Field | Read by | Render context |
|---|---|---|
| `task_specification` | Evaluator (framing), Generator (framing) | `evaluator_v1` REQUIRED block; `generator_v1` HIGH block |
| `evaluation_criteria[]` | Evaluator (verdict basis) | `evaluator_v1` REQUIRED block, bullet-formatted |
| `tasks[]` | Dispatcher (not an LLM); next planner via `failed_attempt_landscape` | Persisted as DAG rows |
| `task_specs[id]` | The single generator with that id | `generator_v1` REQUIRED `planned_task_spec` block (last position) |

Leakage between audiences is a planning bug: criteria-language in a task_spec, task-level detail in the global task_specification, or rubric in the continuation_goal all confuse the wrong reader.

**3. Criteria are the planner's auto-handcuffs.** The evaluator returns binary verdicts (`submit_evaluation_success`/`submit_evaluation_failure`). Over-broad criteria mean partial progress becomes total failure; over-narrow criteria let trivially-passing plans through. The planner's only defense against an unforgiving evaluator is to write criteria it is _confident_ the planned DAG will satisfy. If coverage is uncertain, a partial plan with a tighter criterion set and an explicit `continuation_goal` outperforms a brittle full plan.

**4. The planner is the only role that sees retry history.** `failed_attempt_landscape_blocks` is unique to `planner_v1`. It carries the previous attempt's plan kind (`unsubmitted`, `full`, or `partial`), continuation goal, criteria, latest generator summaries, and fail reason. For evaluator failures, the fail reason includes the evaluator's latest summary when recorded. The evaluator and generators operate context-free with respect to retry — they judge and execute the present attempt. This places retrospection where it can act: the planner can drop a failing slice, narrow scope, preserve achieved work, or restructure dependencies. Neither the evaluator nor the generator has the authority to do any of those.

**5. Wide-flat DAGs are normal; deep chains compound risk.** A generator failure blocks all transitive descendants (`blocked_descendant_ids`, `generator_dag.py:90`); the attempt then closes `FAILED/generator_failed`. A deep chain turns one stuck task into a whole-attempt loss. A wide flat DAG with independent siblings parallelizes throughput and isolates failures.

**6. Partial planning is mission-ancestral, irreversible.** Once a partial plan exists anywhere in the mission's calling lineage, the `planner_full_only` variant is selected for every descendant planner in that lineage. The decision to commit to incremental closure (vs. atomic closure) is a global property, not a local one.

**7. The planner cannot run code.** No `shell`, no `write_file`, no `edit_file`. This is a deliberate capability restriction, not an oversight. A planner that could test its plan would either (a) waste budget on speculative execution before committing, or (b) blur the planner/generator boundary by doing the work itself. The plan-vs-execute split is structural.

**8. Continuation goals are written for a stranger.** The next episode's planner does not see this attempt's task contents — only its `task_summary` aggregation. `continuation_goal` must read like a fresh episode goal, not like a diff against this attempt's plan.

## Context building workflow

This section traces — end-to-end — how the planner's `task_input` string is actually constructed, with every store fetch, every branch, and a rendered example for the major cases. The same pipeline applies to all roles; the differences are isolated to the recipe builder.

### The seven-stage pipeline

```
┌─────────────────────────────────────────────────────────────────────────┐
│  AttemptOrchestrator.start()                                            │
│      attempt stage → PLANNING                                           │
│      planner task row INSERT (status=RUNNING)                           │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  composer.compose(
                                   │      recipe_id="planner_v1",
                                   │      scope=ContextScope(mission_id, episode_id, attempt_id))
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  ContextComposer.compose                                                │
│  (task_center/agent_launch/composer.py)                                 │
│      forwards to engine, then renderer, returns bundle                  │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  engine.build(recipe_id, scope)
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  ContextEngine.build  (engine.py:60)                                    │
│      recipe = RecipeRegistry.get("planner_v1")                          │
│      scope.assert_fields({mission_id, episode_id, attempt_id})          │
│      return recipe.build(scope, self._deps)                             │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  _planner_v1_build  (recipes/planner.py:37)                             │
│      mission   = mission_store.get(mission_id)                          │
│      episode   = episode_store.get(episode_id)                          │
│      episodes  = episode_store.list_for_mission(mission_id)             │
│      attempts  = attempt_store.list_for_episode(episode_id)             │
│                                                                         │
│      blocks  = mission_episode_blocks(...)                              │
│      blocks += failed_attempt_landscape_blocks(...)                     │
│                                                                         │
│      return ContextPacket(target_role="planner", blocks=blocks, ...)    │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  ContextPacket
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  MarkdownPromptRenderer.render  (renderer.py:125)                       │
│      kept = compress(blocks, budget=packet.metadata["token_budget"])    │
│      owned, inherited = split_inherited(kept)                           │
│      sections = render_blocks(owned)                                    │
│      if inherited: sections += ["# Parent context", *render(inherited)] │
│      return "\n\n".join(sections) + "\n"                                │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  task_input: str
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  EphemeralAttemptAgentLauncher                                          │
│      starts agent run with task_input as the first user turn            │
└─────────────────────────────────────────────────────────────────────────┘
```

The recipe builder is a **pure function over stores**. It performs no mutations, no I/O outside the four stores in `ContextEngineDeps`, and emits an immutable `ContextPacket`. Everything that varies between the four planner cases below happens inside `_planner_v1_build`.

### The four cases the planner sees

Two independent dimensions branch the recipe — episode sequence number, and whether failed attempts exist in this episode. The cross-product gives four cases. The table maps each to which builders fire and which blocks land in the packet:

| Case | Episode | Failed attempts | Builders invoked | Block kinds emitted (in order) |
|---|---|---|---|---|
| **A — first attempt, first episode** | `seq=1` | none | `mission_episode_blocks` → 1 block | `episode_goal` |
| **B — retry, first episode** | `seq=1` | ≥1 | `mission_episode_blocks` + `failed_attempt_landscape_blocks` | `episode_goal`, `failed_attempt_landscape*` |
| **C — first attempt, continuation** | `seq≥2` | none | `mission_episode_blocks` → 3+ blocks | `mission_goal`, (`prior_episode_specification` + `prior_episode_summary`)×K, `episode_goal` |
| **D — retry, continuation** | `seq≥2` | ≥1 | both | `mission_goal`, (prior_episode pairs)×K, `episode_goal`, `failed_attempt_landscape*` |

### Walk-through: Case A — the simplest packet

The planner of attempt #1 in episode #1 of a freshly-started mission.

```
Stores read:
  mission_store.get(mission_id)                    → Mission(goal="Add OAuth2 login")
  episode_store.get(episode_id)                    → Episode(sequence_no=1, goal="...")
  episode_store.list_for_mission(mission_id)       → [Episode#1]
  attempt_store.list_for_episode(episode_id)       → [Attempt#1 (current, RUNNING)]

Builder decisions:
  mission_episode_blocks:
    current_episode.sequence_no == 1
      → return [ episode_goal_block(heading="# Mission / Current Episode") ]

  failed_attempt_landscape_blocks:
    failed = [ a for a in attempts if a.status == FAILED and a.id != current ]
    failed == []  → return []

Blocks emitted:
  [0] kind=episode_goal       priority=REQUIRED   text=episode.goal
```

Rendered `task_input` (Case A):

```
# Mission / Current Episode

Add OAuth2 login with Google provider support, expose /auth/google/callback,
and update README with setup instructions.
```

That is the entire planner prompt body for a clean first attempt. Anything else the planner needs comes from its system prompt (its profile markdown).

### Walk-through: Case D — the worst case

The planner of attempt #3 in episode #2 of a mission whose episode #1 succeeded. Episode #2 has two failed attempts behind it.

```
Stores read:
  mission_store.get(mission_id)         → Mission(goal="...")
  episode_store.get(episode_id)         → Episode#2 (sequence_no=2, goal="...")
  episode_store.list_for_mission(...)   → [Episode#1 (closed, has task_summary),
                                           Episode#2 (current)]
  attempt_store.list_for_episode(eid)   → [Attempt#1 (FAILED, fail_reason=...),
                                           Attempt#2 (FAILED, fail_reason=...),
                                           Attempt#3 (current, RUNNING)]

Builder decisions:
  mission_episode_blocks:
    current_episode.sequence_no == 2  (>1)
      priors = [ Episode#1 ]
      immediate_prior_sequence = 1
      Episode#1.sequence_no == immediate_prior  → priority=HIGH
      emit:
        - mission_goal_block(heading="# Mission")
        - prior_episode_specification(Episode#1, priority=HIGH)     [group=# Previous Episode Results]
        - prior_episode_summary(Episode#1, priority=HIGH)            [group=# Previous Episode Results]
        - episode_goal_block(Episode#2, heading="# Current Episode")

  failed_attempt_landscape_blocks:
    failed = [Attempt#1, Attempt#2]    sorted by attempt_sequence_no
    emit:
        - failed_attempt_landscape(Attempt#1, priority=HIGH)         [group=# Prior Failed Attempts]
        - failed_attempt_landscape(Attempt#2, priority=HIGH)         [group=# Prior Failed Attempts]

Final block sequence (in packet order):
  [0] mission_goal                                    REQUIRED
  [1] prior_episode_specification (Ep#1)              HIGH      group=# Previous Episode Results
  [2] prior_episode_summary       (Ep#1)              HIGH      group=# Previous Episode Results
  [3] episode_goal                (Ep#2, current)     REQUIRED
  [4] failed_attempt_landscape    (Attempt#1)         HIGH      group=# Prior Failed Attempts
  [5] failed_attempt_landscape    (Attempt#2)         HIGH      group=# Prior Failed Attempts
```

Rendered `task_input` (Case D):

```
# Mission

Add an offline-capable transaction ledger to the wallet app, including
conflict resolution on reconnect.

# Previous Episode Results

## Episode 1 accepted plan

Build the local sqlite-backed ledger schema and the read API. Defer
sync/conflict-resolution to a follow-up episode.

## Episode 1 summary

Created Ledger + LedgerEntry sqlite tables, exposed list_entries and
get_balance APIs. Sync layer intentionally stubbed; conflict resolution
out of scope per partial-plan contract.

# Current Episode

Add the sync layer: push local entries to /ledger/sync on reconnect,
reconcile server-side conflicts via last-write-wins per entry id, and
surface unresolved conflicts via a new GET /ledger/conflicts endpoint.

# Prior Failed Attempts

## Attempt 1

### Accepted Plan

Plan type: full

Specification:
Add sync push + conflict resolution. The DAG covers the HTTP client, the
local-to-remote diff, and a smoke test against a mocked server.

### Generator Outcomes

Status summary:
- gen-sync-client: done

#### gen-sync-client

Implemented the sync client, but the mock-server smoke test still failed.

### Evaluator Judgment

Evaluation criteria:
  - POST /ledger/sync called with diff of unsynced entries
  - server conflict resolved by entry_id last-write-wins
  - smoke test green

Evaluator summary:
Mock-server smoke test failed after the sync client change.

## Attempt 2

### Accepted Plan

Plan type: full

Specification:
Same scope; route sync through a queue worker instead of a direct call, so
retries on disconnect are not lost.

### Generator Outcomes

Status summary:
- gen-queue-worker: done

#### gen-queue-worker

Added the queue worker; verifier found reconnect did not drain within 5s.

### Evaluator Judgment

Evaluation criteria:
  - queue worker created with idempotent pop
  - reconnect triggers worker drain within 5s
  - smoke test green

Evaluator summary:
Reconnect did not drain the queue worker within 5s.
```

Note how the rendered grouping is driven by `metadata["group_heading"]` in each block, not by an outer template — the renderer (`renderer.py:163-179`) walks blocks linearly, collects consecutive blocks sharing the same `group_heading`, and emits one `## subheading` per block inside that group.

### Where each piece of information comes from

The planner prompt is assembled by reading **four stores** and one constant. The provenance map:

```
                ┌─── mission.goal ──────────────► mission_goal block
                │
                │   ┌── episode.goal ────────────► episode_goal block
                │   │                              (heading varies by sequence_no)
                │   │
                │   ├── prior_episode.task_spec ─► prior_episode_specification block
                │   ├── prior_episode.task_sum. ─► prior_episode_summary block
                │   │   (one pair per closed predecessor; HIGH for immediate prior,
                │   │    MEDIUM for older)
                │   │
                │   │           ┌── attempt.task_specification ───┐
                │   │           ├── attempt.evaluation_criteria ──┼─► failed_attempt_landscape
                │   │           ├── attempt.generator_task_ids ───┤   block (one per failed attempt
                │   │           └── attempt.evaluator_task_id ─────┘   except current)
mission_store ──┘   │
episode_store ──────┘
attempt_store ──────────────────────────────────►
task_store    ──────────────────────────────────► generator statuses/summaries and evaluator summaries
```

Notable: the planner recipe reads `task_store` only for prior failed attempts,
using `attempt.generator_task_ids` to render every generator status and useful
latest generator summaries. For evaluator failures, it also uses
`attempt.evaluator_task_id` to render evaluator criteria and the evaluator's
latest summary in `Evaluator Judgment`. It still does not read current-attempt
task results because the current attempt has not generated anything yet.

### Large retry landscape in practice

Imagine an unusually retry-heavy episode with 9 prior failed attempts. The path through `failed_attempt_landscape_blocks`:

```
failed = [F1, F2, F3, F4, F5, F6, F7, F8, F9]   # sorted by attempt_sequence_no

Final blocks:
  failed_attempt_landscape(F1..F9)            HIGH     × 9 blocks
```

If the resulting packet still exceeds `metadata["token_budget"]`, `MarkdownPromptRenderer._compress` (`renderer.py:201-235`) drops or truncates blocks by priority:

```
budget exceeded?
  → drop LOW first (longest-first within priority)
  → then drop MEDIUM
  → never touch HIGH or REQUIRED
```

For the planner, this means failed-attempt projections are preserved as HIGH-priority retry evidence. Under budget pressure, LOW blocks and older MEDIUM prior-episode summaries are the only planner context the renderer may drop or truncate before returning the best-effort prompt.

### Failure shapes inside the recipe

| Where it fails | Trigger | Effect |
|---|---|---|
| `ContextEngine.build` | scope missing a required field | `AssertionError` from `scope.assert_fields` — surfaces as a planner-launch exception → attempt closes `FAILED/startup_failed`. |
| `mission_store.get` | mission row missing | `ContextEngineError("Mission ... not found")` |
| `episode_store.get` | episode row missing | `ContextEngineError("Episode ... not found")` |
| `_previous_episode_result_blocks` | a closed prior episode has `task_specification is None` or `task_summary is None` | `ContextEngineError("Prior episode ... is missing task_specification or task_summary; chain integrity violated.")` — this is the planner's hardest invariant: every closed predecessor in the chain must have been summarized. |
| Renderer (`render`) | budget set but no compression brings it under | renderer still returns a string; budget is best-effort, not a hard ceiling. |

The chain-integrity invariant exists because the planner of episode N depends on every prior episode's spec/summary being present. A continuation episode created from a partial plan must have those fields written before the next planner spawns; the recipe refuses to silently render incomplete history.

## Failure modes

| Mode | Source | What happens |
|---|---|---|
| Schema rejection (duplicate id, missing spec, blank field) | `build_planner_submission` | Tool returns error result; agent may retry within turn. |
| DAG invariant rejection (cycle, unknown dep, bad agent role) | `ordered_generator_tasks` / `_is_generator_capable_agent` | Same — tool result error, agent can retry. |
| Orchestrator invariant rejection (caller not this attempt's planner; attempt closed) | `apply_plan_submission` | `TaskCenterInvariantViolation`; tool result error. |
| Agent run ends without terminal | Launcher synthesises `apply_planner_failure` | Attempt closes `FAILED/planner_failed`. Failed attempt joins the next planner's landscape. |
| Agent crashes / launcher exception | `_launch_*` exception handlers | Attempt closes `FAILED/startup_failed` if pre-agent; otherwise treated as unfinished-running. |

## See also

- [[task-center-pipeline]] — Mission/Episode/Attempt state machine; what a planner submission triggers downstream.
- [[context-engine-recipes]] — recipe mechanics, renderer, priority/compression.
- [[role-generator]] — what the planner's `task_specs` are consumed by.
- [[role-evaluator]] — what the planner's `evaluation_criteria` are consumed by.
- [[engine-query-loop-llm-seam]] — the agent run that executes a planner turn.
