# Review — Workflow Reducer Removal & Worker Migration SPEC

Reviewed: `docs/plans/workflow_reducer_removal_worker_migration_SPEC.md` (rev 2)
Method: deep read of the current code in `eos-types`, `eos-tool`, `eos-db`,
`eos-agent-run`, `eos-workflow` + a 13-agent fan-out review (inventory →
aggressive-proposal → adversarial-verify) grounded at `file:line`.

---

## Verdict

The SPEC is **strong, genuinely aggressive, and pointed in the right direction.**
Approve the thesis; ship it after fixing the gaps below.

- The binary→singular ("generator + reducer → worker") thesis is correct, and the
  *"every leaf worker is already a reducer for its own branch"* insight (§10) is the
  right structural key. There are **more folds to claim** than the SPEC enumerates
  (§2 below).
- The naming posture is **ban-clean** (the SPEC's own §3/§6 target introduces no
  name from its own "do not introduce" list) and the mechanism→ownership file
  renames are the single best naming win.
- The user's sharpest worry — *"the SPEC explodes `orchestrator.rs` into 6 files
  while the mandate is to simplify"* — **rests on a false premise** (§1). It does
  not; it dissolves a 639-LOC three-owner god-file and re-cuts a ~1,975-LOC subtree.
- **But the SPEC has 7 real internal contradictions and several orphan/under-spec
  gaps** (§5) that will mislead an implementer or fail to compile as written. These
  are fixable with text edits; none invalidate the design.

---

## 1. Rust file/folder management, SRP, boundaries (Q1)

### The "exploding files" premise is false — and the repo already legislated this

`orchestrator.rs` (639 LOC) is **one file mixing three runtime owners** — attempt
lifecycle + planner run + worker recording — which is exactly the CLAUDE.md
"split when the file mixes lifecycle phases" smell. The SPEC's six `attempt/` files
are the home for the **whole ~1,975-LOC, 5-module subtree**, not a decomposition of
one file:

| Current `attempt/` source | LOC | Owners mixed |
| --- | ---: | --- |
| `orchestrator.rs` | 639 | attempt lifecycle **+** planner run **+** worker recording |
| `run_stage.rs` | 344 | RUN-stage scheduler + worker settlement |
| `plan_dag.rs` | 325 | plan validation + DAG topology/readiness |
| `launch.rs` | 610 | `AgentLaunch` + factory |
| inline `orchestrator_registry` (in `attempt.rs`) | ~57 | process-global liveness map |
| **subtree** | **~1,975** | |

`orchestrator.rs`'s 639 LOC redistribute into **four** destinations, **three of
which already exist as files** (`plan_dag`→`work_items`, `run_stage`→`work_items_run`,
the inline registry→`active_attempt_runs`). The module count goes **5 → 6 (+1)**,
solely because the inline registry is promoted to its own file. No file explodes;
behavior shrinks ~900+ LOC.

This is sanctioned by the repo's own **Phase-06 module-budget SPEC** (verified at
`docs/plans/agent-core-workspace-architecture-rules/phase-06-verification-module-budget_SPEC.md`):

- "**Cohesion outranks file count**… No merge may create a new god-file… Keep
  `eos-workflow/src/attempt/` **split by ownership boundary**" (lines 113–129).
- The **only strict gate is the 170 workspace-total** module budget (lines 88–90);
  per-crate caps (`eos-workflow 18 / <=10`) are **advisory and explicitly deferred**:
  "keep `attempt/` split unless real behavior can be deleted" (line 158).

> ⚠️ **Budget caveat to confirm.** The workspace is at **167 modules** today (Phase-06
> line 211). The most-aggressive-deletion shape still nets **+2** `eos-workflow`
> modules → ~169/170. Satisfiable, but only ~1 module of headroom. This is itself a
> good reason to take the `attempt_run`/`planner_run` consolidation below.

### Refinement over the SPEC: where the async materialization lives

The SPEC puts "plan validation, worker materialization, readiness helpers" all in
`work_items.rs`. That over-folds one boundary: `materialize_plan_tasks` is **async**
(`task_store.insert_task(...).await` in a loop), while the DAG core
(`ready_pending_plan_ids`, `dag_resolution`, `unreachable_pending_ids`,
`assert_acyclic`) is **pure sync over `&[Task]`**. Co-homing them is the same
"different lifecycle in one file" smell the split is trying to avoid.

**Cleaner cut:** move worker materialization into `planner_run.rs` (the plan
*producer* — it already owns `record_plan`), leaving `work_items.rs` as a pure sync
validation+scheduler owner. This *also* earns the `attempt_run`/`planner_run` split
on **ownership** rather than the SPEC's "PLAN-vs-RUN symmetry" (which the adversarial
pass flagged as aesthetic).

```text
attempt_run.rs      ~140  PURE lifecycle: start (insert planner row + handoff),
                          close_attempt, assert_stage, fresh_attempt,
                          validate_planner_submission, concurrency asserts
planner_run.rs      ~210  PLAN production: planner launch + settle, record_plan,
                          worker-row materialization, RUN handoff   ◄ async writes
work_items.rs       ~210  PURE sync: plan-shape validation residual + DAG topology/
                          readiness (ready/dag_resolution/unreachable/acyclic) +
                          deterministic worker_task_id mapping      ◄ LOAD-BEARING, no I/O
work_items_run.rs   ~300  ASYNC run: worker waves, settlement, missing-terminal
                          synthesis, worker-outcome collection
active_attempt_runs ~110  cross-attempt process liveness: attempt-run registry +
                          OpenIterationCoordinatorRegistry home
launch.rs           ~320  AgentLaunch (struct+kind), factory, AgentRunner, resources
```

Alternative if you want a module back for budget headroom: **merge
`attempt_run`+`planner_run` into one `attempt_run.rs` (~330, in-band)** — both own the
same `AttemptOrchestrator` lifecycle. Either is defensible; pick one and state the
ownership reason. Do **not** keep two ~165-LOC files justified only by symmetry.

### Three structural gaps in §3 (files left unhomed)

1. **`starter.rs` (183 LOC, `WorkflowStarter`) is absent from the §3 tree.** Fold it
   into `workflow_run.rs` alongside `lifecycle.rs` (`WorkflowStarter.start()` already
   constructs the `WorkflowLifecycle`). Merged ~360 LOC, in band.
2. **`render.rs` must explicitly home `AgentContext`, `ContextSection`, `ContextRole`**
   (today inline `mod section`/`mod xml` in `context.rs:104–194`). §3 routes only
   `ContextScope`→`scope.rs` and leaves these unhomed when `context.rs` dissolves.
3. **eos-types §3 tree omits survivors:** `DeferredGoal` and `AttemptBudget` (kept
   newtypes) have no listed home (suggest `AttemptBudget`→`attempt.rs`,
   `DeferredGoal`→`work_item.rs`), and `present_status` / `execution_outcome_for_submission`
   / `NO_OUTCOME` are public re-exports that **orphan** when `outcomes.rs` is deleted —
   flag them as conscious deletions, not silent drops. (`present_status` encodes a real
   `"done"→Success` vs normalize `"done"→Failed` invariant being retired.)

---

## 2. Where to fold *harder* — the binary→singular catalog (Q2, the headline)

### Folds the SPEC already plans (confirmed correct, biggest wins)

| Construct | Current | Target | Net |
| --- | --- | --- | --- |
| `AgentLaunch` enum + 3 launch structs | `Planner`/`Generator`/`Reducer`; `GeneratorLaunch`≡`ReducerLaunch` field-for-field | one struct + `kind = Planner \| Worker{work_item_id, needs}` | huge |
| `record_generator_submission` + `record_reducer_submission` | twin wrappers around role-agnostic `mark_execution_task` | one `submit_worker_outcome` | ~52→26 |
| `materialize_plan_tasks` | 2 maps + 2 insert loops (gen + reducer) | 1 map + 1 worker loop | ~115→~50 |
| `validate_plan_shape` | unique-ids + needs + acyclic **+** ≥1-reducer + reducer-needs + dangling-leaf reject | unique-ids + needs + acyclic + ≥1-item | ~76→~25 |
| `submit_generator_outcome.rs` + `submit_reducer_outcome.rs` | twin ~97-LOC tool modules | one `submit_worker_outcome.rs` | ~194→~97 |
| eos-db `rows.rs` outcome normalizer + `MaterializedPlan` build + parity tests | `execution_role`/`normalize_*`/`MaterializedPlan` reconstruction | deleted | **~250–300 LOC gone** |
| `GeneratorSubmission` ≡ `ReducerSubmission` | byte-identical DTOs | `WorkerOutcomeSubmission` | -1 type |

### Folds the SPEC **missed** — adopt these (your explicit ask)

1. **`AgentLaunch`'s ~11 role-keyed accessor methods → plain field reads.**
   `launch.rs:152–278` is ~130 LOC of `match self { Planner|Generator|Reducer }`
   boilerplate (`task_id`, `request_id`, `attempt_id`, `iteration_id`, `workflow_id`,
   `agent_name`, `context`, `task_guidance`, `agent_def`, `skill`, …). The moment the
   three structs become one, **every accessor collapses to a field read**; only
   `role()` survives as a 2-arm match. This is the single largest net-negative in the
   file and the cleanest binary→singular collapse — the SPEC names the struct+kind
   fold but never the accessor evaporation.

2. **`build_execution_context`'s `role: ContextRole` parameter is dead.** Generator
   and reducer context **already differ by exactly one label string today** —
   `composer.rs:95` *already* merges `Generator|Reducer` into one render arm, and the
   two context tests assert the identical `<dependencies>+<assigned_task>` shape. The
   worker recipe needs **no role discriminant**; drop it rather than carry a
   `ContextRole` param that flows only into a literal attribute.

3. **`TaskOutcomeStatus` is a third redundant `{Success, Failed}` enum** bridged to
   `SubmissionStatus` via `outcome_status()`. Once `ExecutionTaskOutcome` /
   `GeneratorSubmission` / `ReducerSubmission` (its only consumers) are deleted and
   "pass/fail is never on the outcome" holds, `TaskOutcomeStatus` + the bridge are
   orphaned. **The SPEC's Phase-1 delete list omits it** — it should die, leaving the
   wire-only `SubmissionStatus` (mapping onto `TaskStatus`) as the sole survivor.

4. **`LaunchBuildArgs` carries both `role: TaskRole` *and* `workflow_node_id`**
   (which already encodes the role) **plus `needs` even for the planner** (always
   empty). After `WorkflowNodeId` deletion, carry the `kind` only.

5. **`for_generator` / `for_reducer` factory methods → one `for_worker`**, and the
   reverse parsers **`generator_id_from_task_id` / `reducer_id_from_task_id`**
   (`ids.rs:55,76`, consumed at `launch.rs:482,511`) must be deleted or replaced —
   the SPEC dissolves `ids.rs` but never addresses these *consumed* parsers. Prefer
   threading `work_item_id` forward on the `Worker` kind so no reverse parse is needed.

6. **Mechanical residue the §13 `rg` sweep will hit but the edit list omits:**
   `TASK_AGENT_ROLES: [TaskRole; 4] → [;3]`; `terminal.rs` module doc ("4 of 6",
   "all six") + the `descriptors_total` test count 6→5; doc comments hardcoding the
   "generator/reducer"/"reducer gate" model in `TaskRun`, `AttemptStage/Status`,
   `config.rs:17`, the `composer` planner bullet.

### Guardrails — folds to **NOT** make (the over-fold trap)

The mandate is to fold *within* the workflow-task family (generator+reducer→worker).
It is **not** license to cross these axes:

| Keep distinct | Why it is load-bearing |
| --- | --- |
| `TaskOutcome {Root,Planner,Worker}` **vs** `ParentedOutcome {Advisor,Subagent}` | Different **row types** (`Task` vs `ParentedRun`) and role enums (`TaskRole` vs `ParentedAgentRunKind`). Both serialize into the *same* `terminal_payload: Option<JsonObject>` column → surface pressure to merge; resist it. Merging breaks the `TaskRole↔TaskOutcome` 1:1. |
| DAG scheduler: `ready_pending_plan_ids` / `dag_resolution` / `unreachable_pending_ids` / `assert_acyclic` | Pure, sync, **role-agnostic** (reads `task.needs`/`task.status`, never the binary). Workers still form a DAG; this is the correctness heart. Only `validate_plan_shape`'s reducer/dangling checks delete. |
| `finish_task_run` **vs** `finish_parented_run` | Target **different tables** (`task_runs` vs `parented_runs`) = the same two-family axis. |
| root inline store-write **vs** worker/plan `WorkflowAttemptSubmissionApi` | Root closes a `Request` and has no attempt; do **not** put a `submit_root` method on the 2-method trait. |
| planner `request_id` store-walk **vs** worker `task.request_id` read | The planner is launched **before** its Task row exists (`orchestrator.rs:73` precedes the insert at `:80`), so `for_planner` *cannot* read `task.request_id`. Don't naively unify the two request_id sources in the struct+kind fold. |
| attempt **lifecycle-coherence** validation in `rows.rs` | Only the plan-BUILD half (`MaterializedPlan`/gen/reducer) folds; the `Passed/Failed/Cancelled` stage↔status↔closed_at coherence guards must survive (round-tripped in `integration.rs`). |
| `planner_context` **vs** `worker_context` renderers | Read different state (workflow/iteration history vs plan_spec/needs); not the binary. |
| ⚠️ `Worker { needs }` on the launch kind | **Verify it's consumed.** `launch.needs` is **write-only/dead today** — readiness and events read `task.needs` off the persisted row. If worker context resolves `<needs>` from `ContextScope::Worker{work_item_id}` + the sibling outcome walk (per §12), `needs` on the launch kind is dead again. Don't carry a dead field into the new struct. |

---

## 3. Resulting structure + class/field catalog (Q3)

### Aggressive type-model re-think — "do we still need this?"

Beyond the mechanical folds, the reducer removal invites four honest re-think
questions about the type model. Conclusion: **mostly keep, with two real wins the
SPEC's §6 shape misses.**

| Question | Answer | Why |
| --- | --- | --- |
| Does **`AttemptStage {Plan,Run,Closed}`** still earn its keep? | **Candidate cut → derived view.** | `AttemptStage` is *already* a pure 1:1 projection of the `AttemptState` discriminant (`AttemptState::stage()`), as is `AttemptStatus` (`::status()`). Post-fold it carries zero independent information. **Evaluate** demoting it from a stored/peer enum to a `matches!` helper — *caveat:* `rows.rs::attempt_state_from_columns` may use a persisted `stage` column for lifecycle-coherence checks, so confirm it isn't load-bearing for the DB representation before deleting. |
| Is **`planner_task_id` worth threading through all 3 `AttemptState` variants?** | **Real win — stop storing it.** | The SPEC makes the planner id **deterministic** (`planner_task_id(attempt_id)`, §5). So storing it in `Running{planner_task_id}` / `Closed{…}` is redundant with a pure function. The only genuine state is *"has planning started?"* — which is exactly what the `Planning{planner_task_id: Option}` None→Some guard encodes (a boolean wearing an `Option<TaskId>`). Cleaner: `AttemptState::Planning { started: bool }` (or gate on the planner row's existence) and **derive** the id everywhere else. The SPEC's §6 `AttemptState` still threads the redundant id through every variant. |
| Is **`SubmissionStatus` needed at all** post-fold? | **Keep (it's the survivor).** | A worker genuinely reports success vs blocker/failure, and the model supplies it; that maps onto `TaskStatus` Done/Failed. The *internal* `TaskOutcomeStatus` dies (§2); the *wire* `SubmissionStatus` stays. `submit_plan_outcome` correctly carries no status (success-by-construction). |
| Is **`Attempt` still distinct from `Iteration`** without a reducer gate? | **Keep — distinct axes.** | The reducer gate was never what made `Attempt` distinct; the **retry budget** is (`retry_or_close_failed`, `AttemptBudget`, per-iteration `attempt_sequence_no`). `Attempt` = horizontal retry axis; `Iteration` = vertical deferred-goal continuation. Both survive. |
| Where does **`TaskRole::Root`** belong? | **Keep in `TaskRole`.** | `Root`, `Planner`, `Worker` is the *persisted-task-role* axis; `Root` (workflow_id=None, closes a `Request`) and `Planner` are both task-owned non-DAG members, `Worker` is the DAG member. The 1:1 with `TaskOutcome {Root,Planner,Worker}` is the point. |

### Target `eos-workflow/src/` (incorporating the §1 refinement)

```text
eos-workflow/src/
  attempt/
    attempt_run.rs          one attempt's lifecycle (start/close/asserts)
    planner_run.rs          planner launch+settle + plan recording + worker materialization + RUN handoff
    work_items.rs           PURE sync: plan validation residual + DAG topology/readiness + worker_task_id mapping
    work_items_run.rs       ASYNC: worker waves, settlement, missing-terminal synthesis, collection
    active_attempt_runs.rs  attempt-run registry + OpenIterationCoordinatorRegistry home
    launch.rs               AgentLaunch (struct+kind), AgentLaunchFactory, AgentRunner, AttemptResources
  context/
    planner_context.rs      all planner cases (one "exactly one of" match)
    worker_context.rs       plan_spec + needs + work_item rendering (no role discriminant)
    render.rs               recipe dispatch + xml/section; HOMES AgentContext, ContextSection, ContextRole
    composer.rs             AgentEntryComposer
    scope.rs                ContextScope::{Planner, Worker}
  workflow_run.rs           WorkflowApi + create/close_workflow  (ABSORBS starter.rs + lifecycle.rs)
  iteration_run.rs          coordinator + retry + continuation + handle_iteration_closed
  attempt_submission.rs     submit_plan_outcome + submit_worker_outcome adapter
  config.rs                 WorkflowLifecycleConfig (from deleted ids.rs)
  error.rs / util.rs / lib.rs
  # DELETED: ids.rs, state.rs (the inline `mod projections` — NOT a file named state/projections.rs)
  #          attempt/{orchestrator,run_stage,plan_dag}.rs, context/engine.rs
```

### Type / field catalog (current → target)

| File | Current | Target |
| --- | --- | --- |
| `task.rs` | `TaskRole {Root,Planner,Generator,Reducer}` + `TASK_AGENT_ROLES[4]` | `{Root,Planner,Worker}` + `[3]` |
| `task.rs` | `TaskStatus::is_terminal_generator` | `is_terminal` (same body) |
| `task.rs` | `Task.outcomes: Vec<ExecutionTaskOutcome>` + `terminal_payload: Option<JsonObject>` | drop the Vec; `terminal_payload` only (stays `Option<JsonObject>`) |
| `outcomes.rs`→`outcome.rs` | `ExecutionRole`, `ExecutionTaskOutcome`, `TaskOutcomeStatus`, `present_status`, `execution_outcome_for_submission`, `NO_OUTCOME` | **delete all**; add `TaskOutcome {Root,Planner,Worker}` + `ParentedOutcome {Advisor,Subagent}` + `AdvisorVerdict` |
| `plan.rs`→`work_item.rs` | `PlannerId`/`GeneratorId`/`ReducerId` (macro ×3) | `WorkItemId` (+ derived `planner_task_id`/`worker_task_id`) |
| `plan.rs` | `PlanDisposition {Complete, Defer}` + 4 methods | delete → `Option<DeferredGoal>` |
| `plan.rs` | `MaterializedPlan {planner_task_id, disposition, generator_task_ids, reducer_task_ids}` | delete (plan lives in `TaskOutcome::Planner`) |
| `plan.rs` | `DeferredGoal`, `AttemptBudget` | **keep** (home `DeferredGoal`→`work_item.rs`, `AttemptBudget`→`attempt.rs`) |
| `attempt.rs` | `AttemptClosure::{Passed,Failed,Cancelled}` each `outcomes: Vec<ExecutionTaskOutcome>` | drop the Vec from every variant; keep `reason`+`closed_at` (outcomes become read-side) |
| `attempt.rs` | `AttemptState::Running{plan: MaterializedPlan}` / `Closed{…, plan}` | `Running{planner_task_id: TaskId}` / `Closed{closure, planner_task_id: Option}` (§6 shape) |
| `attempt.rs` | `Attempt::generator_task_ids()` / `reducer_task_ids()` | delete; enumerate via `worker_task_id` or `(attempt_id, role=Worker)` |
| `contracts.rs` | `WorkflowTaskRole {Planner,Generator,Reducer}` | `{Planner,Worker}` |
| `contracts.rs` | `WorkflowNodeId {Planner,Generator,Reducer}` | **delete** — but reshape `SpawnAgentTarget::Workflow` (see §5) |
| `contracts.rs` | `PlanTask` / `PlanReducer` / `PlannerPlan{disposition,tasks,task_specs,reducers}` | `WorkItemSpec{id,agent_name:AgentName,work_spec,needs}`; `PlanReducer` delete; `PlannerPlan`→`TaskOutcome::Planner` |
| `contracts.rs` | `WorkflowAttemptSubmissionApi` (3 methods) | 2 methods: `submit_plan_outcome` + `submit_worker_outcome` |
| `submissions.rs` | `GeneratorSubmission`≡`ReducerSubmission`; `PlannerSubmission`/`PlannerFailureSubmission`/`PlannerFailReason` | `WorkerOutcomeSubmission{…,work_item_id,status:SubmissionStatus,outcome}`; `PlanOutcomeSubmission`; planner-failure DTOs delete |
| eos-tool `terminal.rs` | `TerminalTool {Root,Generator,Reducer,Planner,AdvisorFeedback,SubagentResult}` (6) | `{RootTask,Plan,Worker,Advisor,Subagent}` (5) |
| eos-tool `model.rs` | `ToolName::ALL [22]` w/ 6 terminals | `[21]` w/ 5 terminals |

---

## 4. Naming conventions (Q4)

- **Mechanism→ownership file renames are sound and the best naming improvement:**
  `orchestrator`→`attempt_run`/`planner_run`, `run_stage`→`work_items_run`,
  `plan_dag`→`work_items`, `engine`→`render`+role renderers, `submission`→`attempt_submission`.
- **`work_items.rs` vs `work_items_run.rs` is a clear distinction, not a near-duplicate**
  — *provided you pin the boundary* (sync data+validation vs async wave+settlement) in
  the file headers. It is the only owner carrying both a bare and a `_run` file, so it's
  the one seam at drift risk.
- **Single `outcome` field name: good and essentially non-lossy.** Pass/fail lives in
  `TaskStatus`, role in the typed variant, so per-role names were redundant; no struct
  carries two of the renamed fields, so the blanket rename can't silently merge two.
  Two honest caveats: (a) subagent `findings`+`references` → free-text `outcome` is a
  **conscious de-typing** (the only field-*shape* loss); (b) `AgentRunReport.failure_summary`
  → `outcome` is subtly lossy because `AgentRunReport` has **no status field** — failure
  now rides `Option`-presence (`None`=clean, `Some`=fault), not a lifecycle status.

**Three naming fixes the SPEC needs:**

| Issue | Fix |
| --- | --- |
| **`AttemptOrchestratorRegistry`** (type) + `orchestrator_registry` field + `with_orchestrator_registry` builder keep the **banned "orchestrator" word** even though the file is renamed to `active_attempt_runs.rs` | rename the **type** → `ActiveAttemptRuns` (+ field/builder). As written you'd ship `active_attempt_runs.rs` *defining* `AttemptOrchestratorRegistry`. |
| **`AdvisorVerdict`** is used by `ParentedOutcome::Advisor` / `SubmitAdvisorOutcomeInput` but **does not exist** (only tool-private `Verdict {Approve,Reject}`), and the SPEC never homes it | promote the private enum to public `eos-types` `outcome.rs` as `AdvisorVerdict` |
| §9 calls `TerminalTool {RootTask,Plan,Worker,Advisor,Subagent}` **"1:1 with the `TaskOutcome` variants"** — false (`TaskOutcome` has 3) | it's 1:1 with `TaskOutcome ∪ ParentedOutcome` (5); also note names diverge (`RootTask`≠`Root`, `Plan`≠`Planner`). Correct the wording. |

(`OpenIterationCoordinatorRegistry` is **not** a miss — "coordinator" is unbanned and §3 keeps it.)

---

## 5. SPEC issues to fix before implementation

The consistency audit labeled the SPEC "contradictory," but the items are not equal.
Split by what they cost an implementer:

**A. Correctness bugs — read/write of deleted state (must fix):**

| # | Bug | Fix |
| --- | --- | --- |
| B1 | **`deferred_goal` dropped-but-read:** §13 drops the attempt `deferred_goal` cache + `MaterializedPlan`, but `iteration.rs:187,204` still calls `attempt.deferred_goal_for_next_iteration()` (backed by `MaterializedPlan`). This is a read of deleted state. | Specify how the iteration re-reads the deferred goal from the planner task's `TaskOutcome::Planner` after `MaterializedPlan` is gone. |
| B2 | **`WorkflowNodeId` deleted but `SpawnAgentTarget::Workflow` never reshaped.** It still carries `workflow_node_id`; no phase names the edit; the **planner-has-no-`work_item_id` / worker-has** asymmetry is unspecified (touches `eos-agent-run/src/spawn.rs:85–100`, which the SPEC wrongly implies is untouched). | Write the concrete replacement: `SpawnAgentTarget::Workflow { role: WorkflowTaskRole, work_item_id: Option<WorkItemId> }`; enumerate the `spawn.rs` edit. |

**B. Mechanical gaps — correct intent, but a step is unenumerated (will fail to build):**

| # | Gap | Fix |
| --- | --- | --- |
| B3 | **`TaskOutcomeStatus` deletion omitted** from the Phase-1 delete list (the third redundant status enum, §2). | Add `TaskOutcomeStatus` + the `outcome_status()` bridge to the delete list. |
| B4 | **`state/projections.rs` is a phantom file** — §3/§13/AC say delete it, but the code is an **inline `mod projections` inside `state.rs`**. AC is trivially "satisfied" while the real `state.rs` is never named. | Target `eos-workflow/src/state.rs` for dissolution explicitly. |
| B5 | **Phase-3+ verify ladders can't compile** — `ids.rs` dissolution + `MaterializedPlan`/`generator_task_ids` deletion strand **four test trees** (`tests/attempt/{orchestrator,run_stage,plan_dag}/mod.rs`, `tests/context/engine/mod.rs`) and `eos-db/tests/integration.rs`. No phase rewrites them. | Add test-tree rewrites to each phase before its `cargo test`. |

**C. Imprecise wording — an implementer resolves it correctly anyway, but tighten it:**

| # | Wording | Fix |
| --- | --- | --- |
| B6 | **"three files" AC (line 848) vs §3's FIVE context files** (`planner_context`, `worker_context`, `render` **+ `composer` + `scope`**). §12's "three not five" is really an anti-over-split guard for the *recipe/render* files only. | Reword the AC to "the context recipe layer is three render files"; `composer`/`scope` are separate owners. |
| B7 | **`Task.terminal_payload: TaskOutcome` typing** (§4 line 289, §13 line 730) reads as contradicting the two-family split — the column is a **shared `Option<JsonObject>`** across `Task`/`TaskRun`/**`ParentedRun`** (which holds `ParentedOutcome`). | State the field stays `Option<JsonObject>`; only the *producer/reader* changes. The eos-db `terminal_payload` change is a near-no-op (columns stay TEXT-of-JSON). |

**Orphan set the "dissolve `state.rs`/`ids.rs`" steps must re-home** (grounded
consumers): `project_attempt_outcomes` (`orchestrator.rs:509`),
`attempt_execution_outcomes` (`context/engine.rs:239`), `project_iteration_outcomes`
(`iteration.rs:305` — **and its stored write-path** via `IterationStore::close_succeeded`/
`set_status`), `WorkflowLifecycleConfig` (`lifecycle.rs:11`, `launch.rs:298/340/368`),
`planner_id`/`generator_task_id`/`reducer_task_id` (production + 4 test trees).

**Under-specified asymmetry to resolve:** §11 says outcomes are "read-side
projections, never stored," but §13/eos-db drop **only** the *attempt* outcomes cache
and are silent on the `Iteration.outcomes` / `Workflow.outcomes` stored-String caches
(`iteration.rs:84`, `entity.rs:73`), which `project_iteration_outcomes` currently
feeds. Decide explicitly: drop them for symmetry, or state they're intentionally kept.

**Low-risk but real:** `0001_initial.sql` edit-in-place is safe for this repo (no
committed DB, every test uses a temp dir), but any out-of-tree long-lived SQLite file
that already applied `0001` would hit a **sqlx checksum mismatch** (no `0002`). The
SPEC's "verify no deployed DB" contingency covers the repo; note the residual operator risk.

---

## 6. Prioritized punch-list

1. **Fix the correctness bugs first** (§5.A): B1 deferred-goal read-of-deleted-state and
   B2 `SpawnAgentTarget`/`WorkflowNodeId` reshape. Then the mechanical gaps (§5.B) and
   wording (§5.C).
2. **Adopt the missed folds** (§2): `AgentLaunch` accessor evaporation, drop
   `build_execution_context`'s `ContextRole` param, delete `TaskOutcomeStatus`.
3. **Take the structure refinement** (§1): materialization → `planner_run.rs`,
   `work_items.rs` stays pure sync; confirm the 170-module workspace budget.
4. **Apply the 3 naming fixes** (§4): rename the `AttemptOrchestratorRegistry` *type*,
   home `AdvisorVerdict`, correct the "1:1 with `TaskOutcome`" wording.
5. **Home the §1 orphans** (`starter.rs`, render-types, `DeferredGoal`/`AttemptBudget`,
   the three `outcomes.rs` free fns).
6. **Verify `Worker{needs}` is actually consumed** before carrying it into the new struct.

---

## Appendix — concrete resulting structure (files + types)

### Full file/folder tree (SPEC §3 + this review's refinements)

```text
agent-core/crates/eos-types/src/
  contracts/
    record.rs          TaskAgentRunKind, WorkflowTaskRole{Planner,Worker},
                       SpawnAgentTarget (Workflow arm reshaped)
    workflow.rs        WorkflowApi, WorkflowAttemptSubmissionApi (2 methods)
  state/
    request_task/task.rs   TaskRole{Root,Planner,Worker}, TaskStatus(is_terminal), Task, TaskRun, ParentedRun
    tools/submissions.rs   PlanOutcomeSubmission, WorkerOutcomeSubmission, SubmissionStatus
    workflow/
      workflow.rs      Workflow, WorkflowStatus, WorkflowOutcome      (was entity.rs)
      iteration.rs     Iteration, IterationStatus, IterationOutcome
      attempt.rs       AttemptState, AttemptClosure, AttemptStatus, Attempt, AttemptBudget
      work_item.rs     WorkItemId, WorkItemSpec, DeferredGoal, planner_task_id(), worker_task_id()
      outcome.rs       TaskOutcome{Root,Planner,Worker}, ParentedOutcome{Advisor,Subagent}, AdvisorVerdict
  # DELETED: state/workflow/plan.rs (→ work_item.rs + attempt.rs),
  #          state/workflow/outcomes.rs (binary ExecutionRole/ExecutionTaskOutcome/TaskOutcomeStatus)

agent-core/crates/eos-tool/src/
  model.rs             ToolName: 5 terminals (was 6), ALL[21]
  tools/
    terminal.rs        TerminalTool{RootTask,Plan,Worker,Advisor,Subagent}
    submission/
      mod.rs · support.rs (SubmissionStatus, OutcomeInput, helpers)
      submit_root_task_outcome.rs · submit_plan_outcome.rs
      submit_worker_outcome.rs   (submit_generator + submit_reducer twins folded)
      submit_advisor_outcome.rs · submit_subagent_outcome.rs

agent-core/crates/eos-workflow/src/
  attempt/
    attempt_run.rs        PURE lifecycle: start/close/asserts
    planner_run.rs        planner launch+settle + record_plan + worker materialization  ◄ async
    work_items.rs         PURE sync: validation residual + DAG readiness + worker_task_id map
    work_items_run.rs     ASYNC: worker waves, settlement, missing-terminal synthesis, collection
    active_attempt_runs.rs  ActiveAttemptRuns + OpenIterationCoordinatorRegistry
    launch.rs             AgentLaunch (struct+kind), AgentLaunchFactory, AgentRunner, AttemptResources
  context/
    planner_context.rs · worker_context.rs · render.rs (homes AgentContext/ContextSection/ContextRole)
    composer.rs · scope.rs (ContextScope{Planner,Worker})
  workflow_run.rs         WorkflowApi + create/close_workflow  (absorbs starter.rs + lifecycle.rs)
  iteration_run.rs · attempt_submission.rs · config.rs · error.rs · util.rs · lib.rs
  # DELETED: ids.rs, state.rs, attempt/{orchestrator,run_stage,plan_dag}.rs,
  #          context/engine.rs, starter.rs, lifecycle.rs, submission.rs

agent-core/crates/eos-db/        # no new files
  migrations/0001_initial.sql    attempts: keep planner_task_id; drop generator_task_ids,
                                 reducer_task_ids, outcomes, deferred_goal. tasks: drop outcomes.
                                 task_runs CHECK: role IN ('planner','worker')
  src/rows.rs                    -250..300 LOC (normalizer + MaterializedPlan build + parity tests gone)
agent-core/crates/eos-agent-run/src/spawn.rs   reshape SpawnAgentTarget::Workflow arm

.eos-agents/
  profile/main/{root,planner,executor}.md   # reducer.md deleted
  profile/helper/advisor.md · profile/subagent/subagent.md · skills/ (reducer/ deleted)
  tools/{submit_root_task_outcome, submit_plan_outcome, submit_worker_outcome,
         submit_advisor_outcome, submit_subagent_outcome}.md
```

### Key target type signatures

```rust
// ── eos-types task.rs ──
pub enum TaskRole { Root, Planner, Worker }                 // was {Root,Planner,Generator,Reducer}
pub const TASK_AGENT_ROLES: [TaskRole; 3] = [Root, Planner, Worker];
impl TaskStatus { pub const fn is_terminal(self) -> bool {/* Done|Failed|Blocked|Cancelled */} }
pub struct Task {                                           // `outcomes: Vec<…>` field REMOVED
    pub id: TaskId, pub request_id: RequestId, pub role: TaskRole,
    pub instruction: String, pub status: TaskStatus,
    pub workflow_id: Option<WorkflowId>, pub iteration_id: Option<IterationId>,
    pub attempt_id: Option<AttemptId>, pub agent_name: Option<String>,
    pub needs: Vec<TaskId>,
    pub terminal_payload: Option<JsonObject>,               // stays Option<JsonObject>; holds a TaskOutcome
}   // ParentedRun unchanged; its terminal_payload holds a ParentedOutcome (separate family)

// ── eos-types work_item.rs ──
pub struct WorkItemId(String);                              // was GeneratorId
pub struct DeferredGoal(String);                            // kept (PlanDisposition deleted)
pub struct WorkItemSpec { pub id: WorkItemId, pub agent_name: AgentName,
                          pub work_spec: String, pub needs: Vec<WorkItemId> }
pub fn planner_task_id(attempt_id: &AttemptId) -> TaskId;                       // deterministic
pub fn worker_task_id(attempt_id: &AttemptId, work_item_id: &WorkItemId) -> TaskId;

// ── eos-types outcome.rs ──  (DELETED: ExecutionRole, ExecutionTaskOutcome, TaskOutcomeStatus,
//                                       present_status, execution_outcome_for_submission)
#[serde(tag="kind", rename_all="snake_case")]
pub enum TaskOutcome {                                       // workflow-task family (Task rows)
    Root    { outcome: String },
    Planner { plan_spec: String, work_items: Vec<WorkItemSpec>,
              deferred_goal_for_next_iteration: Option<DeferredGoal> },
    Worker  { outcome: String },
}
#[serde(tag="kind", rename_all="snake_case")]
pub enum ParentedOutcome {                                  // parented family (ParentedRun) — DO NOT merge
    Advisor  { verdict: AdvisorVerdict, outcome: String },
    Subagent { outcome: String },
}
pub enum AdvisorVerdict { Approve, Reject }                 // promoted from tool-private Verdict

// ── eos-types attempt.rs ──  (SPEC §6 literal)
pub enum AttemptState {
    Planning { planner_task_id: Option<TaskId> },           // None→Some guards double-start
    Running  { planner_task_id: TaskId },                   // no MaterializedPlan field
    Closed   { closure: AttemptClosure, planner_task_id: Option<TaskId> },
}
//  ▶ RECOMMENDED tightening (review §3): id is deterministic, so track only "started":
//        enum AttemptState { Planning { started: bool }, Running, Closed { closure } }
//    and demote AttemptStage to a derived `matches!` helper.
pub enum AttemptClosure {                                    // `outcomes: Vec<…>` REMOVED from each variant
    Passed    { closed_at: UtcDateTime },
    Failed    { reason: AttemptFailReason, closed_at: UtcDateTime },
    Cancelled { reason: String, closed_at: UtcDateTime },
}
// Attempt::generator_task_ids()/reducer_task_ids()/materialized_plan() DELETED;
// enumerate workers via worker_task_id(attempt_id, w.id) over the planner's work_items.

// ── eos-types contracts ──
pub enum WorkflowTaskRole { Planner, Worker }               // was {Planner,Generator,Reducer}
// WorkflowNodeId DELETED. SpawnAgentTarget::Workflow reshaped:
pub enum SpawnAgentTarget { /* … */ Workflow { role: WorkflowTaskRole, work_item_id: Option<WorkItemId> } }
#[async_trait] pub trait WorkflowAttemptSubmissionApi {     // 3 methods → 2
    async fn submit_plan_outcome(&self, s: PlanOutcomeSubmission)   -> Result<SubmissionAck, CoreError>;
    async fn submit_worker_outcome(&self, s: WorkerOutcomeSubmission) -> Result<SubmissionAck, CoreError>;
}

// ── eos-types submissions.rs ──
pub enum SubmissionStatus { Success, Failed }               // wire-only survivor; maps onto TaskStatus
pub struct PlanOutcomeSubmission { pub attempt_id: AttemptId, pub plan_spec: String,
    pub work_items: Vec<WorkItemSpec>, pub deferred_goal_for_next_iteration: Option<DeferredGoal> }
pub struct WorkerOutcomeSubmission { pub attempt_id: AttemptId, pub task_id: TaskId,
    pub work_item_id: WorkItemId, pub status: SubmissionStatus, pub outcome: String }
// DELETED: PlannerFailureSubmission, PlannerFailReason (planner failure = attempt transition)

// ── eos-workflow attempt/launch.rs ──  3 structs → 1; ~11 accessor matches → field reads
pub struct AgentLaunch {
    pub task_id: TaskId, pub request_id: RequestId, pub attempt_id: AttemptId,
    pub iteration_id: IterationId, pub workflow_id: WorkflowId, pub agent_name: String,
    pub context: String, pub task_guidance: Option<String>,
    pub agent_def: AgentDefinition, pub skill: Option<String>, pub kind: AgentLaunchKind,
}
pub enum AgentLaunchKind { Planner, Worker { work_item_id: WorkItemId } }  // add `needs` ONLY if proven consumed
impl AgentLaunch { pub fn role(&self) -> TaskRole {/* Planner | Worker */} }  // only surviving match

// ── eos-tool terminals ──
pub enum TerminalTool { RootTask, Plan, Worker, Advisor, Subagent }   // 6 → 5
// ToolName terminals: SubmitRootTaskOutcome, SubmitPlanOutcome, SubmitWorkerOutcome,
//                     SubmitAdvisorOutcome, SubmitSubagentOutcome   (ALL: 22 → 21)
```
```
