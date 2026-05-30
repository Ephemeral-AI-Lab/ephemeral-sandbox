# Reducers + Unified Outcomes Redesign — Implementation Plan

**Status:** REVIEWED (consensus pass 2 applied — Architect SOUND-WITH-CONCERNS +
Critic ITERATE feedback folded in). This is both a *simplification* and a
*refactor* plan: it replaces the planner → generator → **evaluator** triad with
planner → generator → **reducer**, makes reducers DAG nodes (collapsing the
dedicated EVALUATE stage into one RUN stage), unifies every agent result under
one recursive **outcomes** vocabulary, unifies every DAG edge under **needs**,
and pays down the state / naming / chained-import debt that accreted around the
evaluator.

**Note on "goal":** there is **no `Goal` class** to remove — it was collapsed
into `Workflow` by the earlier Goal→Workflow rename. `goal` is a `str` field on
`Workflow`/`Iteration` (+ `deferred_goal_for_next_iteration`) and **stays.** The
two planner tools keep their `..._closes_goal` / `..._defers_goal` names and the
defer handoff is unchanged.

---

## RALPLAN-DR Summary

**Principles**
1. **One vocabulary, enforced by construction.** `needs`, `outcomes`,
   `reducer`, `node` — and no surviving synonyms (`deps`, `dependency`,
   `summary`, `task_summary`, `evaluation_criteria`, `evaluator_summary` all
   retire). The context layer's value is omission-control, so the words it
   renders must be singular. Where the redesign moves an *enforced* guarantee
   (e.g. "≥1 acceptance criterion"), it must move the enforcement too, not just
   the data (see D6).
2. **Reducers are just nodes for scheduling — special for the gate + algebra.**
   They schedule, gate, and quiesce through the *existing* generator-DAG
   machinery (no new coordination layer, no peer-to-peer comms); the dedicated
   EVALUATE stage and singular `evaluator_task_id` disappear. They are
   privileged in exactly two ways, stated honestly: ≥1 is mandatory (the exit
   gate) and only *their* outcomes propagate to `attempt.outcomes`.
3. **Minimal context of highest value.** Each recipe assembles only what its
   role consumes. A reducer is a *generator-shaped* recipe over its `needs`; the
   evaluator's bespoke "see every generator + criteria" assembly is deleted and
   recovered, when needed, by a convergent reducer that `needs` all generators.
4. **Two projections, one rendering.** Feed-forward relay (prior iterations'
   *canonical, reducer-only* outcomes) and retry feedback (the current
   iteration's *failed-node* outcomes + `fail_reason`) are different *data
   sources* rendered through one element. Do not collapse them into one source —
   that silently drops generator-failure diagnostics (§6, Correction-1).
5. **Surgical blast radius.** Tier-1 renames are mandatory and load-bearing;
   Tier-2 renames are optional coherence, deferred, because each multiplies the
   mock string-match churn in a worktree shared with concurrent agents.

**Decision drivers (top 3)**
1. The user's explicit asks: `needs` over `dependency`; exactly three recipes
   with the specified inputs; one `outcomes` vocabulary; coherent semantics; pay
   down state/naming/import debt.
2. The `task_center_runner` mock **string-matches** rendered context vocabulary
   (`scenario_loop_runner.py:274,284,303` match `<task `, `<assigned_task`,
   `<evaluation_criteria>`) and dispatches on role names — it is the integration
   harness, so every rename carries a test-vocab cost that must move in lockstep.
3. The worktree is shared with concurrent agents — favor net-negative, staged,
   path-scoped change over a wide cosmetic sweep.

**Viable options**
- **A — Full redesign (RECOMMENDED).** Reducer role + node-DAG gate + 3 recipes
  (generator/reducer sharing one block helper) + Tier-1 renames; Tier-2 deferred.
  Delivers every ask; bounded churn. The GENERATE+EVALUATE→RUN collapse is sound
  because the evaluator's `needs` is *already* the full generator set
  (`launch.py:367`), so the dedicated stage encodes no timing guarantee the DAG
  can't already express.
- **B — Minimal rename.** `evaluator→reducer` + `deps→needs` only; keep the
  EVALUATE stage and the flat evaluator recipe. *Rejected:* no gate
  simplification, no recipe symmetry, and a reducer still cannot synthesize from
  a sub-DAG (a reducer that `needs` other reducers) — fails asks #3 and #5. Not a
  strawman: B is coherent, it just cannot express sub-DAG synthesis.
- **C — Maximal sweep.** A plus all Tier-2 renames (`<task>`→`<outcome>`,
  `summaries`→`outcomes` column + log keys, `tasks`→`generators`) in the same
  pass. *Rejected for now:* triples mock-coupling churn (the `verifier`
  string-match surface alone is ~24 files) for coherence the user did not
  require; offered as a follow-up once A lands.

---

## 1. Target model (end state)

**Roles:** `planner`, `generator` (only profile: `executor`), `reducer`,
`helper` (advisor), `subagent` (explorer). **Gone:** the `evaluator` role and
the `verifier` profile.

**A plan is a DAG of nodes in two colors, edges are `needs`.**
- **generator node** — `{local_id, agent_name, needs, task_spec}`, produces work
  (role `GENERATOR`).
- **reducer node** — `{local_id, needs, prompt}`, digests/gates (role `REDUCER`);
  the planner authors each `prompt` (required, nonblank — D6); binary
  success/failure terminal. Reducers may `needs` generator ids *and* other
  reducer ids (reducers form a sub-DAG; a convergent reducer that `needs` the
  rest is how you synthesize one outcome).

**Gate (the simplification).** Reducers are *mandatory DAG nodes*. The attempt
PASSES iff **every node reaches DONE**; it FAILS if any node failed/blocked.
That is exactly today's `summarize_generator_dag` quiescence applied to the
combined gen+reducer node set — so `AttemptStage.EVALUATE` and the singular
`evaluator_task_id` **disappear**, and stages collapse to **`PLAN → RUN →
CLOSED`**.

Two structural rules keep "every attempt has an exit condition AND all work is
judged" true **by construction** (both enforced in `ordered_dag_nodes`, both
regression-tested):
- **≥1 reducer** — a plan with zero reducers is rejected (the exit gate).
- **Reachability (Architect F2)** — every generator must be transitively
  required by ≥1 reducer (follow `needs` edges backward from the reducers; a
  generator not in that closure is unjudged work and rejects). Without this, a
  generator no reducer needs still reaches DONE, the attempt PASSES, and that
  work is never judged and never in `attempt.outcomes`.

**Outcomes algebra (single vocabulary, recursive).**
- `generator.outcomes` / `reducer.outcomes` = the agent's `submit_*` result, a
  `list[Outcome]` (singleton normally; the child-workflow rollup for a handoff
  generator).
- `attempt.outcomes` = ⋃ its **reducers'** outcomes (generators reach the
  attempt only through the reducer that `needs` them).
- `iteration.outcomes` (**canonical, persisted**) = the **passing** attempt's
  reducer outcomes. Full attempt history is retained for audit + retry feedback
  but does not propagate.
- `workflow.outcomes` (**derived, not stored**) = the final iteration's
  `outcomes`. A read/report-time projection off `final_iteration_id`, *not* a
  repurposing of the existing `final_outcome` closure dict (§6, Correction-3).
- A workflow-handoff generator's outcomes = its child workflow's outcomes
  (`Outcome.children`).

**Off-spine agents (unchanged role, excluded from the algebra).** `planner` is
control-plane (its `submit_plan_*` configures the attempt; a control signal, not
an Outcome). `advisor` + `explorer` are callee-returns (their result goes only to
the agent that spawned them). Only generators + reducers (+ handoff workflows)
are on the outcomes spine.

**Retry (reshaped, no memoization).** A failed attempt re-plans from scratch
(attempts stay immutable). What crosses the retry is the failed attempt's
**failed-node outcomes + `fail_reason`** (feedback) — a generalization of
today's `failed_attempt_blocks`, **not** "failed reducers' outcomes" (empty when
a generator fails; §6, Correction-1). Memoizing passed reducers is **rejected**
(unsound when reducers share upstream work).

**Cross-iteration relay (repointed).** The next iteration's planner reads prior
iterations' **reducer** outcomes (`iteration.outcomes`), not generator
summaries: relay the digest, not the raw work.

---

## 2. Unified vocabulary (the naming spine)

Every rename below is justified by an existing inconsistency. **Tier 1** is
mandatory (load-bearing). **Tier 2** is optional coherence — deferred to a
follow-up because each crosses the mock string-match surface.

### Tier 1 — mandatory

| Concept | Today (inconsistent) | Unified | Rationale |
|---|---|---|---|
| Gate / digest role | `evaluator` | **`reducer`** | "evaluate" → "evaluate *or* synthesize"; a general fold over `needs` |
| DAG edge | `deps` (schema + DTO), `dependency` (recipe tags/kind) — but `needs` (DB column + task row + launch) | **`needs`** everywhere | the persisted truth is already `needs`; collapse the 3-way split toward it |
| Needs **wrapper tag** | `<dependency>` group (`generator.py:128`) | **`<needs>`** group | forced Tier-1: the shared `needs_outcome_blocks` (§4) emits one wrapper for *both* colors, so it cannot stay `<dependency>`. The mock *harness* does not match `<dependency`, but ~7 context-engine **unit tests** assert the rendered wrapper — inventoried in WS6 |
| Result unit | `TaskOutcome` | **`Outcome`** | it is no longer generator-specific |
| Result content field | `summary` (on `Outcome`, submissions, `Outcome.to_record`) | **`text`** | retire `summary`; free it from the status meaning. (`Outcome.raw_status` is kept — it drives `is_terminal`.) |
| Result collection | `task_summary` / `generator_summaries` projections | **`outcomes`** (`list[Outcome]`) | one recursive collection name across attempt/iteration/workflow |
| Submission status field | `outcome: Literal[...]` | **`status`** | stop colliding the status with the `outcomes` list |
| Per-attempt nodes | `generator_task_ids` (tuple) **+** `evaluator_task_id` (singular) | **`node_task_ids`** (one tuple, role-tagged) | reducers are just nodes; one scheduler input (D4) |
| Iteration achieved record | `Iteration.task_summary` | **`Iteration.outcomes`** | the canonical algebra projection |
| Attempt stages | `PLAN → GENERATE → EVALUATE → CLOSED` | **`PLAN → RUN → CLOSED`** | one DAG run-to-quiescence; no bespoke evaluate stage |
| Attempt fail reason | `GENERATOR_FAILED` + `EVALUATOR_FAILED` | **`NODE_FAILED`** | the gate fails on any node; the culprit is in the node outcomes (keep `PLANNER_FAILED`, `STARTUP_FAILED`) |
| Reducer module / recipe | `recipes/evaluator.py`, `EVALUATOR_RECIPE`, `for_evaluator` | `recipes/reducer.py`, `REDUCER_RECIPE`, `for_reducer` | follows the role |
| Reducer terminals | `submit_evaluation_success/failure` | `submit_reduction_success/failure` | mirrors retired pairs, one-terminal routing (D1) |
| Planner plan field | `evaluation_criteria: list[str]` | **removed** → per-reducer `prompt` (D6) | criteria become each reducer's authored prompt |
| Outcomes module | `_core/generator_summaries.py` | `_core/outcomes.py` | it owns the `Outcome` type + algebra, not "generator summaries" |
| DAG module | `attempt/generator_dag.py`, `ordered_generator_tasks` | `attempt/node_dag.py`, `ordered_dag_nodes` | validates/schedules gen+reducer nodes |

### Tier 2 — optional coherence (DEFERRED; do not fold into the Tier-1 sweep)

| Concept | Today | Optional unified | Why deferred |
|---|---|---|---|
| Result XML **child element** | `<task id status>` (in needs, prior-iteration, failed-attempt blocks) | `<outcome id status>` | total coherence, but `<task ` is string-matched at `scenario_loop_runner.py:274,302` |
| Task append-log column **+ its dict keys** | `task_center_tasks.summaries` (JSON), entries keyed `{"outcome","summary"}` | column→`outcomes`, keys→`{"status","text"}` | highest-churn; the log keys are written at `orchestrator.py:348` and read by `latest_task_summary` — move column + keys together (D7) |
| Planner generators field | `PlannerSubmission.tasks` | `generators` (symmetry with `reducers`) | deeply embedded; symmetry is nice-to-have |

> **Wrapper vs child (resolves the prior draft's self-contradiction):** Tier-1
> renames the **concept** (`deps`→`needs`), the recipe block kind
> (`DEPENDENCY_SUMMARY`→`NEEDS_OUTCOME`), **and the wrapper tag**
> (`<dependency>`→`<needs>`) — the last is forced because one shared helper emits
> the wrapper for both colors. The **child element stays `<task>`** (Tier-2) to
> avoid the confirmed `<task `-matching in the mock. There is exactly one
> instruction here: wrapper `<needs>` now, child `<task>` until the Tier-2
> follow-up.

---

## 3. End-state class / field diagram

```
ROLES & PROFILES
  AgentRole / TaskCenterTaskRole : PLANNER | GENERATOR | REDUCER         (was …EVALUATOR)
  SpawnReason                    : ATTEMPT_{PLANNER,GENERATOR,REDUCER}
  Profiles                       : planner, executor, reducer            (gone: verifier, evaluator)

PLAN  (what the planner submits) — a DAG of two node colors; edges = needs
  generator node : { local_id, agent_name, needs: list[str], task_spec }
  reducer  node  : { local_id,              needs: list[str], prompt }     (prompt required+nonblank; ≥1 reducer; D6)
  · validation (ordered_dag_nodes): unique ids across colors, known needs, no cycles,
    ≥1 reducer, AND every generator transitively needed by ≥1 reducer (reachability, F2)

OUTCOMES ALGEBRA  (_core/outcomes.py)  — recursive
  Outcome : { local_id, status, text, children: tuple[Outcome,...], failure: str|None, raw_status: str|None }
              (was TaskOutcome; summary→text; raw_status KEPT — drives is_terminal)
  generator.outcomes / reducer.outcomes = submit_* result   (singleton; list on handoff)
  attempt.outcomes    = ⋃ reducers' outcomes                (derived)
  iteration.outcomes  = passing attempt's reducer outcomes   (PERSISTED, canonical)
  workflow.outcomes   = final iteration's outcomes           (DERIVED at report time, not stored)

STATE   (§3 lists FIELD names; DB columns differ where noted — names are not 1:1 with columns)
  Workflow  : id, task_center_run_id, goal, status, iteration_ids,
              final_outcome{outcome, final_iteration_id, final_attempt_id},   (UNCHANGED closure dict)
              origin_kind, requested_by_task_id, created/updated/closed_at
              · workflow.outcomes is a derived projection, NOT a new column.

  Iteration : id, workflow_id, sequence_no, creation_reason, goal, attempt_budget,
              status, attempt_ids,
              deferred_goal_for_next_iteration (DB col: deferred_goal), plan_spec,
              outcomes (json list[Outcome-record]),                            (was task_summary)
              created/updated/closed_at

  Attempt   : id, iteration_id, attempt_sequence_no,
              stage: {PLAN, RUN, CLOSED},                                      (was PLAN/GENERATE/EVALUATE/CLOSED)
              status, planner_task_id, plan_spec,
              node_task_ids: tuple[str,...],                                   (was generator_task_ids + evaluator_task_id)
              deferred_goal_for_next_iteration (DB col: deferred_goal),
              fail_reason, created/updated/closed_at
              · REMOVED: evaluation_criteria, evaluator_task_id
  AttemptFailReason : PLANNER_FAILED | NODE_FAILED | STARTUP_FAILED           (GENERATOR_/EVALUATOR_FAILED → NODE_FAILED)

SUBMISSIONS  (tools/submission)
  PlannerSubmission   : attempt_id, planner_task_id, kind, plan_spec,
                        tasks: tuple[PlannedNode-generator],                   (Tier-2 rename → "generators")
                        reducers: tuple[PlannedNode-reducer],                  (NEW; ≥1; prompt nonblank)
                        deferred_goal_for_next_iteration, text                 (REMOVED evaluation_criteria; summary→text)
  GeneratorSubmission : attempt_id, task_id, status, outcomes: list[Outcome], payload   (outcome→status; summary→outcomes)
  ReducerSubmission   : attempt_id, task_id, status:Literal["success","failure"], outcomes, payload   (was EvaluatorSubmission; BINARY — D7/F5)
  Task row            : role{planner,generator,reducer}, agent_name, context_message,
                        status, needs: list[str], summaries(json log)          (log dict keys stay "outcome"/"summary" in Tier-1 — D7)

CONTEXT RECIPES  (still three; the change is the reducer's internal assembly + a shared helper)
  planner  : <goal> + <iteration_goal> + prior-attempt outcomes (if any)
               ├ relay   : prior iterations' canonical (reducer-only) outcomes        [feed-forward]
               └ retry   : current iteration's failed attempts → failed-node outcomes + <failure>  [feedback]
  generator: <plan_spec> + <needs>(outcomes) + <assigned_task>
  reducer  : <needs>(outcomes) + <assigned_prompt>
  shared   : needs_outcome_blocks(node)  (generator & reducer; emits <needs> wrapper) ; outcome XML in _task_xml.py
  DELETED  : recipes/evaluator.py, current_attempt_flat_blocks,
             <evaluation_criteria> + <evaluator_summary> tags, _EVALUATION_CRITERIA_KIND
```

---

## 4. The 3-recipe builder redesign (item 5, the heart)

The context engine builds packets through one protocol — `ContextRecipe{id,
required_scope_fields, build(scope, deps) -> ContextPacket}` — auto-discovered by
the `*_RECIPE` suffix. There are already three `*_RECIPE` objects; **this
redesign keeps three recipes and collapses the evaluator recipe's bespoke
assembly into a shared helper** so generator and reducer render identically over
their `needs`. That is the substance: today there are *three different
result-rendering shapes* (the generator's `_dependency_blocks`, the evaluator's
`current_attempt_flat_blocks`, the planner's `_prior_iteration_blocks` +
`failed_attempt_blocks`); after, there is **one** result helper reused across
generator + reducer, and the planner's two history paths render through one
element.

### 4.1 `generator` recipe — `<plan_spec>` + `<needs>` + `<assigned_task>`
Unchanged in spirit; `_dependency_blocks` → shared `needs_outcome_blocks` (block
kind `DEPENDENCY_SUMMARY`→`NEEDS_OUTCOME`; wrapper tag `<dependency>`→`<needs>`,
child stays `<task>`; the task-row field is already `needs`). Reads
`attempt.plan_spec`, the node's `needs` outcomes, and the node's
`context_message` (`<assigned_task>`).

### 4.2 `reducer` recipe — `<needs>` + `<assigned_prompt>`  (replaces evaluator)
A **generator-shaped** recipe minus `<plan_spec>`:
- `needs_outcome_blocks(reducer_node)` — the outcomes of exactly this reducer's
  `needs` (generator and/or reducer ids), same `<needs>`/`<task>` rendering as
  the generator.
- `<assigned_prompt>` — the planner-authored `prompt` (parallel to the
  generator's `<assigned_task>`), carrying whatever criteria that reducer checks.

This **deletes** the evaluator's "see every generator + flat criteria" view. A
reducer that must judge the whole attempt is authored as a **convergent reducer
that `needs` every generator** — identical information, expressed through
`needs`; and the planner can now author narrower/intermediate reducers (the
sub-DAG synthesis case). Cover the subset case with a dedicated scenario (a
reducer whose `needs` is a strict subset of the generators), and see §9 for its
concurrency note (Architect F3).

Scope: `ContextScope.for_reducer(workflow_id, iteration_id, attempt_id,
task_id)` — it now requires `task_id` (the reducer is a node), whereas
`for_evaluator(workflow_id, iteration_id, attempt_id)` (`scope.py:88-101`) took
the attempt without a task. Required fields mirror the generator.

### 4.3 `planner` recipe — `<goal>` + `<iteration_goal>` + prior-attempt outcomes
Two history paths render through one **prior-attempt-outcomes** element, but over
**two data sources** (Principle 4; do not merge the sources):
- **Relay (feed-forward).** Prior iterations' **canonical** outcomes =
  `iteration.outcomes` (the passing attempt's reducer outcomes). Reducer-only is
  correct: the iteration passed, so its reducers ran. Depth-priority HIGH/MEDIUM
  unchanged.
- **Retry (feedback).** The current iteration's **failed** attempts → each
  attempt's **failed-node outcomes (any color)** + its `<failure>`
  (`fail_reason`) line. This **preserves and generalizes** `attempt_failure_line`
  / `_render_failed_attempt_body` (terminal generator `<task>`s + the failure
  line; the old `<evaluator_summary>` becomes just another reducer node's
  outcome). It must **not** become "failed reducers' outcomes" — when a generator
  fails the reducer never runs, so that set is empty exactly when feedback
  matters most (§6, Correction-1).

`<goal>`, `<iteration_goal>`, and the deferred-goal handoff are **unchanged.**

### 4.4 Module / import shape after the redesign
- `recipes/reducer.py` (≈25 lines) replaces `recipes/evaluator.py`; it imports
  the **shared** `needs_outcome_blocks` — it does **not** import `attempts.py`.
- `recipes/attempts.py` loses `current_attempt_flat_blocks`,
  `_task_outcome_block`, `_TASK_OUTCOME_KIND`, `_EVALUATION_CRITERIA_KIND`, and
  `_evaluator_summary_if_ran`; it keeps the generalized `failed_attempt_blocks` /
  `_render_failed_attempt_body` for the planner.
- `_core/generator_summaries.py` → `_core/outcomes.py`; `TaskOutcome`→`Outcome`
  (`summary`→`text`, `raw_status` kept), `generator_outcomes`→
  `attempt_node_outcomes` (role-filterable), `parse_achieved_record`→
  `parse_outcomes`, `child_outcomes_for_workflow` reads `iteration.outcomes`.

---

## 5. Workstreams

Each lists the change, principal seams (file:line from the blast-radius maps),
and verification. Files are not listed exhaustively — the removal maps are the
inventory; **additions** (notably the new `reducers` field) are called out
because they are not in any removal map.

### WS1 — Reducer role replaces evaluator (rename + generalize)
- **Roles:** `AgentRole.EVALUATOR`→`REDUCER` (`agents/definition/model.py:38`);
  `TaskCenterTaskRole.EVALUATOR`→`REDUCER` (`_core/task_state.py:16`);
  `SpawnReason.ATTEMPT_EVALUATOR`→`ATTEMPT_REDUCER` (`:24`); loader role
  validation (`agents/definition/loader.py:65-69`).
- **Profile:** `agents/profile/main/evaluator.md`→`reducer.md` (role `reducer`,
  terminals per D1, recipe `reducer`, generalized skill "evaluate *or*
  synthesize", prompt authored per-node by the planner).
- **Terminals:** `tools/submission/evaluator/*`→`tools/submission/reducer/*`
  (`submit_reduction_success/failure`); registry descriptors
  (`tools/_terminals/registry.py:112-140`); `_factory.py` wiring.
- **Recipe:** `recipes/evaluator.py`→`reducer.py` per §4.2;
  `ContextScope.for_evaluator`→`for_reducer` (now takes `task_id`)
  (`scope.py:88-101`); auto-discovery keeps working (`*_RECIPE`).
- **Launch:** `EVALUATOR_AGENT_NAME`→`REDUCER_AGENT_NAME`; `for_evaluator`→
  `for_reducer` (`attempt/launch.py:303,354-369`); `_ROLE_FAIL_REASONS`
  (`:197-200`) maps reducer→`NODE_FAILED`.
- **Directive/tags:** `agent_directives.py:21`; the `evaluator_summary` /
  `evaluation_criteria` tags retire (`tag_dictionary.py:77-80`, `_task_xml.py:32`,
  `recipes/attempts.py`).
- **Verify:** unit tests for the reducer recipe + role resolution; `ruff` +
  type-check green.

### WS2 — Reducers as DAG nodes + gate + stage collapse (the lifecycle change)
- **Schema (`tools/submission/planner/_schemas.py`):**
  - replace `evaluation_criteria: Field(min_length=1)` (`:61`) with
    `reducers: list[ReducerInput] (min 1)`, `ReducerInput = {id, needs, prompt}`
    where **`prompt` is required and nonblank** — reuse the existing nonblank
    validator at `:70-75` so a content floor stays enforced by construction (D6,
    closes Architect F1).
  - rename `PlanTaskInput.deps`→`needs` (`:31`).
  - the two planner tool **names** and `deferred_goal_for_next_iteration` are
    **unchanged**.
  - generalize `ordered_generator_tasks`→`ordered_dag_nodes` to validate the
    **combined** gen+reducer DAG: unique ids across colors, unknown deps spanning
    colors, cycles, **≥1 reducer**, **and reachability — every generator
    transitively required by ≥1 reducer** (F2; reject unjudged generators).
- **DTOs:** `PlannerSubmission.evaluation_criteria`→`reducers`;
  `PlannedGeneratorTask.deps`→`needs` (`submissions.py`); `EvaluatorSubmission`→
  `ReducerSubmission` (status **binary** `Literal["success","failure"]` — keep it
  binary so the shared `_write_submission_status` blocker→BLOCKED branch
  (`orchestrator.py:341`) stays unreachable for reducers; F5).
- **Persistence:** replace `Attempt.generator_task_ids` + `evaluator_task_id`
  with `node_task_ids` (`attempt/state.py:41-43`); persist reducer nodes like
  generator tasks (role `REDUCER`). DB (`db/engine.py`): **add** an `attempts`
  entry to `_DROPPED_COLUMNS` (`:40-69`, no `attempts` key today) for
  `{evaluation_criteria, evaluator_task_id}`; **add**
  `generator_task_ids→node_task_ids` under the existing `attempts` key in
  `_RENAMED_COLUMNS` (`:71-78`). Migration note: `_RENAMED_COLUMNS` preserves old
  data, so a pre-migration `attempts` row would carry only generator ids under
  `node_task_ids`. Per project memory (durable dev DBs are empty; real rows live
  only in disposable `task_center_runner/*.db` + `.sweevo_runs/` scratch) this is
  effectively inert — state it rather than assume it.
- **Stage machine:** delete `AttemptStage.EVALUATE`, `_start_evaluator_stage`,
  `_advance_evaluator_stage` (`stage_advancer.py:101-119,203-265`); rename
  `_advance_generator_stage`→`_advance_run_stage` over the full node set.
  Reducers schedule via the existing `ready_pending_*`/`summarize_*_dag` path
  (`generator_dag.py`→`node_dag.py`). Close: **all nodes DONE→PASSED; any
  failed/blocked→FAILED (`NODE_FAILED`)**.
- **Orchestrator:** `apply_evaluator_submission`→`apply_reducer_submission`
  (`orchestrator.py:161-164,307-325`, `orchestrator_registry.py:39`); invariant
  `assert_evaluator_task_for_submission`→reducer (`_core/invariants.py:135-138`);
  a reducer submission marks its node DONE/FAILED through the *same* path as a
  generator (no singleton branch).
- **Scenario impact (additions — not in any removal map; Critic MAJOR 2):** the
  new `reducers (min 1)` makes **every plan-submitting scenario** (~31 files with
  inline plan dicts, not just verifier ones) fail validation until it adds a
  `reducers=[{id, prompt, needs:[…generators]}]`. Update the inline dicts **and**
  the 3 `plan_shapes.py` helpers (`minimal_full_plan` and callers) to take/emit
  `reducers`.
- **Verify:** the pipeline mock scenarios (attempt pass/fail/retry, DAG
  diamond/parallel/serial) pass under reducers; **add**:
  - a multi-reducer-gate scenario (some reducers pass, one fails → retry);
  - a generator-fails-before-reducer scenario (Correction-1: the retry packet
    shows the failed *generator* outcome + `fail_reason`, not an empty set);
  - a subset-reducer scenario (Correction-2 / F3: reducer `needs` ⊊ generators);
  - **a `no_reducers` planner-validation scenario** mirroring
    `scenarios/planner_validation/empty_tasks.py` (Critic MAJOR 3 — the ≥1-reducer
    invariant is the whole justification for the gate collapse and must have a
    regression guard; `min_length=1` provides the enforcement). Update
    `empty_tasks.py` itself: a plan with tasks but zero reducers must now reject.
  - a reachability-rejection scenario (a generator no reducer needs → rejected).

### WS3 — Remove the verifier profile
- Delete `agents/profile/main/generator_verifier.md` and
  `tools/submission/verifier/*` (`submit_verification_success/failure`); drop
  their `_factory.py:16-35` wiring + registry descriptors
  (`tools/_terminals/registry.py:141-169`).
- Drop `"verifier"` from `_REQUIRED_AGENT_NAMES`
  (`task_center_runner/core/bootstrap.py:28`), audit role sets
  (`audit/recorder.py:87,94`, `audit/node_id.py:15`), the directive
  (`agent_directives.py:20`) + task-guidance dispatch
  (`agent_launch/task_guidance_dispatch.py:26-36`).
- Rework verifier-spawning scenarios to `executor` generators + `reducer` gates:
  `full_case_user_input.py`, `full_stack_adversarial.py`,
  `pipeline/nested_workflow.py`,
  `pipeline/deferred_parent_planner_terminal_routing.py`, their
  `verifier_response` hooks (`scenarios/base.py:52,73`,
  `mock/scenario_adapter.py:142-145,296-297`, `scenario_loop_runner.py:239,290`),
  the `verifier_checkpoint_script`s, and the asserting tests
  (`tests/mock/task_center/test_full_case_user_input.py` verifier
  helpers/asserts). **Result:** `executor` is the only generator profile.
- Update planner prompts that say "executor/verifier"
  (`planner/submit_plan_*/prompt.py:32`, `planner.md:90,93`).
- **Verify:** full mock suite green; no remaining `verifier`/`verification`
  references (`grep`).

### WS4 — Unified `outcomes` vocabulary
- **Type:** `_core/generator_summaries.py`→`_core/outcomes.py`; `TaskOutcome`→
  `Outcome`; `summary`→`text` (keep `raw_status`). `Outcome.to_record`/
  `from_record` keys move `summary`→`text`; `from_record` keeps a back-compat
  read of legacy `"summary"` for pre-migration achieved-records. **Rename the
  status field** on `Generator/ReducerSubmission` `outcome`→`status`.
- **The `summaries`-log key contract (Critic MAJOR 4 — separate from
  `Outcome.to_record`):** the submit path writes an inline dict into the task-row
  `summaries` log at `orchestrator.py:348`
  (`{"outcome": …, "summary": …, "payload": …}`, via `_write_submission_status`
  `:327`), read back by `latest_task_summary` (`generator_summaries.py:72-84`,
  `last.get("summary") or last.get("outcome")`). **Decision (D7):** the in-memory
  DTO/`Outcome` fields rename (`outcome`→`status`, `summary`→`text`), but the
  **persisted log dict keys stay `"outcome"`/`"summary"`** in Tier-1 — the
  `summaries` *column* is Tier-2-deferred, and the log keys move with it. So the
  write site at `:348` maps the renamed field values onto the stable keys and
  `latest_task_summary` is unchanged. Add both sites to the WS4 inventory so the
  executor does not rename the keys ad hoc and silently blank the needs/relay
  summaries.
- **submit_* shape:** generator + reducer tools return `outcomes: list[Outcome]`
  (singleton normally; handoff generator returns the child workflow's list).
- **Aggregation:** implement the §1 algebra. `_achieved_record_for`
  (`iteration/attempt_coordinator.py:215-223`)→`_iteration_outcomes_for`,
  projecting **reducer** outcomes. Add derived `attempt.outcomes` (reducers),
  persisted `iteration.outcomes`, derived `workflow.outcomes` (final iteration —
  §6, Correction-3); `child_outcomes_for_workflow` reads `iteration.outcomes`.
- **Persistence:** `Iteration.task_summary`→`outcomes` (JSON list)
  (`iteration/state.py`, `db/models/iteration.py`, stores); add to
  `_RENAMED_COLUMNS`. Surface `workflow.outcomes` in the run report
  (`task_center_runner/core/runner.py:133`) **alongside** the unchanged
  `final_outcome`.
- **Verify:** round-trip tests for `Outcome` (`to_record`/`from_record` incl.
  legacy `"summary"` key / `parse_outcomes`); a test that needs/relay rendering
  still shows non-empty summaries (guards the D7 key contract); run-report shows
  derived `workflow.outcomes`.

### WS5 — Relay + retry repoint (two projections)
- **Relay:** `_prior_iteration_blocks` (`recipes/iterations.py:113-164`) renders
  prior iterations' **reducer** outcomes from `iteration.outcomes`.
  `<goal>`/`<iteration_goal>` and the deferred-goal handoff unchanged.
- **Retry feedback:** `failed_attempt_blocks` (`recipes/attempts.py:67-98`)
  carries each failed attempt's **failed-node outcomes + `fail_reason`** to the
  retry planner (generalizes the single `<evaluator_summary>` + `<failure>`).
  Attempts immutable; no memoization. **Correction-1 verified here.**
- **Verify:** deferral scenario (`pipeline/iterative_deferral.py`) shows iter N+1
  planner context = iter N reducer outcomes; retry scenarios show failed-node
  feedback for **both** a failed-reducer case *and* a
  failed-generator-before-reducer case.

### WS6 — Mock runner, audit, scenarios, docs (lockstep, not optional)
- **Mock vocab coupling:** the mock runner string-matches planner context
  (`scenario_loop_runner.py:303` keys on `<evaluation_criteria>`; role dispatch
  on `"verifier"`/`"evaluator"`) and the initial-messages test asserts
  `<evaluation_criteria>` + `Load skill: evaluator`
  (`tests/mock/task_center/test_initial_messages_capture.py:235-251`). Update in
  lockstep with WS1/WS2 (the reducer recipe emits `<assigned_prompt>` +
  `<needs>`, not `<evaluation_criteria>`).
- **`evaluation_criteria` removal reaches the response-builders, not just the
  recipe (Architect F4):** scenario evaluator-response builders read
  `ctx.attempt.evaluation_criteria` — `full_case_user_input.py:117`,
  `pipeline/nested_workflow.py:118,161`, `full_stack_adversarial.py:155`,
  `pipeline/attempt_budget_exhausted.py:71`, `pipeline/dependency_dag_diamond.py:51`,
  `pipeline/dependency_blocked_descendants.py:56`. Each becomes a reducer-response
  builder; add them to the WS6 inventory so they are not discovered mid-sweep.
- **Audit:** role enums/sets, drop `evaluator_task_id`, `summaries` projections
  (`audit/recorder.py`, `events.py`, `node_id.py`).
- **Docs:** refresh `docs/architecture/task_center/{agent-roles,lifecycle,
  context-engine,terminal-tools}.html` + the runner pages; follow each page's
  `data-last-reviewed-commit`/`data-evidence-paths`.

### WS7 — Import-graph flattening (item 6, tech debt — concrete, not vague)
**Chains the redesign genuinely flattens (deletions, not renames):**
- `recipes/evaluator.py → recipes/attempts.py(current_attempt_flat_blocks) →
  {iterations.py, generator_summaries.py, _task_xml.py}` — deleting `evaluator.py`
  + `current_attempt_flat_blocks` removes the evaluator's pull on the
  planner-only `attempts.py` flat path and on `iterations.py` group helpers.
- **New cross-import prevented:** extracting the **shared** `needs_outcome_blocks`
  (used by generator + reducer) keeps `reducer.py` and `generator.py` siblings
  over a shared helper, not a chain.
- `_core/generator_summaries.py`→`_core/outcomes.py` makes importers (recipes,
  `attempt_coordinator`, `orchestrator`) depend on a correctly-named module that
  already owns the whole algebra.

**Renames that force import churn but reduce no coupling (mechanical):**
`TaskOutcome`→`Outcome`, `generator_summaries`→`outcomes`,
`generator_dag`→`node_dag`. Touch every importer; coherence, not decoupling.

- **Verify:** `ruff`/type-check green; grep finds no remaining
  `current_attempt_flat_blocks`, `generator_summaries`, `evaluation_criteria`,
  `evaluator_task_id`; and confirm the only surviving `"outcome"`/`"summary"`
  dict-key literals are the intentional `summaries`-log sites (`orchestrator.py:348`,
  `latest_task_summary`) per D7.

---

## 6. Corrections carried from review (rationale, auditable)

- **Correction-1 (load-bearing). Retry feedback is failed-*node* outcomes +
  `fail_reason`, not "failed reducers' outcomes."** A generator G that fails
  leaves its reducer `unreachable_pending` (`ready_pending_generator_ids` needs
  all `needs` DONE; `_unreachable_pending_ids`/`summarize_generator_dag`,
  `generator_dag.py:79-89,114-175`), so the attempt fails with **no reducer
  outcome produced**. The relay projection stays reducer-only (canonical, passing
  attempt); the retry-feedback projection generalizes `attempt_failure_line` over
  failed nodes of any color. One rendering, two sources (§4.3).
- **Correction-2. The reducer's context is its `needs`, not "all generators."**
  Deleting `current_attempt_flat_blocks` (`recipes/attempts.py:101-150`) removes
  the evaluator's global view; "judge the whole attempt" is recovered by a
  convergent reducer that `needs` every generator. A behavior change — covered by
  the subset-reducer scenario (WS2).
- **Correction-3. `workflow.outcomes` is derived, `final_outcome` is untouched.**
  `Workflow.final_outcome` is the closure dict `{outcome, final_iteration_id,
  final_attempt_id}` (`workflow/state.py:96-101`), consumed by the run report
  (`runner.py:133`), audit, and store — closure *status + pointers*, not result
  content. `workflow.outcomes` is a **derived** projection off
  `final_iteration_id → iteration.outcomes`; **no** dict→list column migration,
  **no** change to `final_outcome`'s three consumers.

---

## 7. Decisions

- **D1 — Reducer terminal tool names.** `submit_reduction_success` /
  `submit_reduction_failure` (mirrors the retired pairs + one-terminal routing).
  **Recommend the pair.**
- **D2 — Off-spine submissions stay distinct.** `submit_advisor_feedback` /
  `submit_exploration_result` are **not** renamed to `outcomes`. **Recommend keep
  distinct.**
- **D3 — `iteration.outcomes` projection.** Canonical = passing attempt's reducer
  outcomes; history kept for audit/retry only. **Confirmed.**
- **D4 — One `node_task_ids` vs `generator_task_ids` + `reducer_task_ids`.**
  **Recommend one `node_task_ids`** (role-tagged): one scheduler input, one
  quiescence pass, matches "reducers are just nodes."
- **D5 — Tier-2 renames.** **Recommend defer** (`<task>`→`<outcome>`,
  `summaries` column + log keys, `tasks`→`generators`); land as a follow-up.
- **D6 — Reducer prompt content guarantee (Architect F1).** `ReducerInput.prompt`
  is **required + nonblank** (reuse the `_schemas.py:70-75` nonblank validator).
  This preserves an enforced content floor by construction; it is a *documented
  trade* — weaker than the old `evaluation_criteria` min-1 *list*, in exchange
  for planner-authored flexibility and the "assigned prompt" shape the user
  asked for. If stronger structure is wanted later, add an optional
  `criteria: list[str]` alongside `prompt`. **Recommend nonblank prompt now.**
- **D7 — `summaries`-log dict keys stay literal in Tier-1 (Critic MAJOR 4).**
  The in-memory DTO/`Outcome` fields rename, but the persisted task-row
  `summaries`-log keys (`"outcome"`/`"summary"` at `orchestrator.py:348`, read by
  `latest_task_summary`) stay until the Tier-2 `summaries`-column rename, when
  keys + column move together. **Recommend keep literal now;** documents the
  field-name↔log-key split instead of leaving a silent half-rename.

---

## 8. Sequencing & couplings

1. **D1–D7 resolved** (blocks WS2/WS4).
2. **WS1** (reducer rename/generalize) — foundation.
3. **WS2** (DAG gate + stage collapse + reachability) — depends on WS1.
4. **WS4** (outcomes) — couples tightly with WS2; land WS2→WS4 back-to-back.
5. **WS5** (relay/retry, two projections) — depends on WS4.
6. **WS3** (verifier removal) — mostly independent; can run parallel to WS1/WS2
   but its scenario rework must target the WS2 reducer schema.
7. **WS6** (mock/audit/docs) — continuous; mock-runner vocab + the
   `evaluation_criteria` response-builders move with WS1/WS2, the scenario
   `reducers`-field additions move with WS2.
8. **WS7** (import flattening) — lands as deletions occur; verify at the end.

**Parallel-agent note:** the worktree is shared. Stage with explicit file paths;
verify at HEAD before declaring a workstream done.

---

## 9. Invariants to preserve

- Every attempt has an **exit gate** (≥1 reducer; reject zero) **and all work is
  judged** (every generator transitively needed by ≥1 reducer; reject unjudged —
  F2). Both enforced in `ordered_dag_nodes`, both regression-tested.
- **Attempt immutability** — retries re-plan; no cross-attempt memoization.
- TaskCenter stays the control plane — **no peer-to-peer agent comms**, no global
  orchestrator. A reducer that `needs` another reducer reads it through the
  context engine off persisted state (blackboard-mediated), not agent-to-agent.
- Terminal tools called **alone**; reducer success/failure remain terminal and
  **binary** (D7/F5).
- DAG validation (no cycles, known deps, ≥1 reducer, reachability) spans
  gen+reducer nodes.
- **Concurrency/OCC unchanged for *convergent* reducers** (the default, which —
  like the old evaluator — runs only after all generators are DONE). A **subset**
  reducer (new capability) can go ready while out-of-`needs` sibling generators
  still run; its needs-outcomes are always final (its needs are DONE), but if it
  performs sandbox I/O beyond its needs it can observe the shared workspace
  mid-flight — an interleaving the old design prevented. Exercise this in the
  subset-reducer scenario (F3).
- Failed attempts still surface *why* — the retry planner sees failed-node
  outcomes + `fail_reason` (Correction-1).

## 10. Verification strategy

- Use `.venv/bin/pytest` (never global) and `.venv/bin/ruff`.
- The `task_center_runner` **mock scenarios are the integration harness** — every
  workstream's "verify" routes through them (pipeline, dependency-DAG, deferral,
  recursion, full_stack). New scenarios to add (consolidated): multi-reducer-gate
  pass/partial-fail/retry; generator-fails-before-reducer retry (Correction-1);
  subset-reducer with sandbox I/O (Correction-2 / F3); `no_reducers` rejection
  (MAJOR 3); reachability rejection (F2).
- Unit-level (not only scenario): `Outcome` round-trip incl. legacy `"summary"`
  key; `ordered_dag_nodes` reachability + ≥1-reducer + cycle rejection;
  `latest_task_summary` non-empty after the D7 key decision.
- Heavy IWS/background + docker scenarios run on this host (~25s each); only
  `live_e2e_test/**` skips.
- Per workstream: failing scenario/repro first where practical, then the change,
  then green.

---

## ADR — Architecture Decision Record

**Decision.** Replace the `evaluator` role/stage with a general `reducer` that is
a DAG node; gate the attempt on full gen+reducer quiescence (`PLAN→RUN→CLOSED`);
unify results under a recursive `Outcome`/`outcomes` algebra and edges under
`needs`; rename for one vocabulary (Tier-1) and defer cosmetic renames (Tier-2).

**Drivers.** The user's five asks (needs/3-recipes/unified-outcomes/coherent-
semantics/debt-paydown); the mock string-match coupling that prices every rename;
a shared worktree that prices wide churn.

**Alternatives considered.** B (minimal rename) — rejected: cannot express
sub-DAG synthesis, fails asks #3/#5. C (maximal sweep) — rejected for now:
triples mock churn for unrequested coherence; deferred as a follow-up.

**Why chosen.** A delivers all asks with bounded churn; the GENERATE+EVALUATE→RUN
collapse is provably sound (evaluator `needs` is already the full generator set,
`launch.py:367`); the reducer-as-generator-shaped-node insight gives the 3-recipe
symmetry the user asked for; reachability + nonblank-prompt keep the gate
guarantees by construction.

**Consequences.** A reducer sees only its `needs` (not all generators) — a
behavior change recovered by a convergent reducer; subset reducers introduce a
new mid-attempt sandbox-read shape (scoped, tested); retry feedback must read
failed *nodes* not failed *reducers* (Correction-1); `final_outcome` stays,
`workflow.outcomes` is derived (Correction-3).

**Follow-ups (Tier-2).** `<task>`→`<outcome>` child element; `summaries` column +
log keys → `outcomes`/`text` (D7); `PlannerSubmission.tasks`→`generators`;
optional structured `criteria` on `ReducerInput` (D6) if a stronger content floor
than nonblank prompt is later wanted.
