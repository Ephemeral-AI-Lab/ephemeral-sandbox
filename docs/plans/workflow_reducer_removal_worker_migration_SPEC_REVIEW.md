# Review — Workflow Reducer Removal & Worker Migration SPEC

Reviewer pass date: 2026-06-09. Grounded against the live tree (eos-types,
eos-workflow, eos-tool, eos-db, eos-agent-run, eos-engine) with file:line
evidence. Focus areas requested: Rust folder/SRP/boundary quality, aggressive
binary→singular folding, the resulting file/class/field shape, and naming.

---

## DECISION UPDATE (supersedes §4) — the planner KEEPS its TaskStore row

After review, the spec's decision *"planner has no TaskId, no TaskStore row"*
(spec §2/§5, acceptance §14) is **reversed**. The planner keeps a task row.

**Why.** A task row does two independent jobs: (a) **DAG membership** (`needs`,
readiness, the worker wave) and (b) **record anchor** (the folder under which the
agent run's `messages.jsonl`/`events.jsonl` + nested `workflows`/`subagents`/
`advisors` live). The planner needs (b), not (a). The recorder is task-rooted:
`records/writer.rs:51` + `handle.rs:110-111` write `{messages,events}.jsonl` into
the `format_record_dir` path, and `records/kind.rs:32-41` records the planner as a
`WorkflowTask` node (`workflow_planner`) keyed by `task_id`. Every `AgentType::Agent`
run (root, planner, worker) is task-anchored; even subagents/advisors carry a
`ParentedRun.task_id` (`task.rs:177`). Removing the planner's row makes it the lone
homeless agent run — which is exactly the `format_record_dir` / `finish_task_run`
break this review's §4 flagged as the #1 blocker. Keeping the row means that
blocker **never exists**.

**The spec was half-right:** killing `PlannerId` is correct (one planner per
attempt → derive `planner_task_id(attempt_id)`); killing the *row* was the
overreach. The planner is task-owned for recording, but is **never** a member of
the worker DAG (`plan_task_records` enumerates only worker ids — `orchestrator.rs:553-569`).

**Corrected deltas (these override the relevant rows in §2, §3, §4, §7, §8 below):**

| Surface | §4 (planner-no-row) said | Corrected |
|---|---|---|
| `TaskRole` | `Root \| Worker` (4→2) | **`Root \| Planner \| Worker` (4→3)**; `TASK_AGENT_ROLES [;3]` |
| `WorkflowTaskRole` | delete | **keep at `{ Planner, Worker }`** (node_type + path prefix) |
| `TaskAgentRunKind::Workflow { role }` | split → `WorkflowPlanner`/`WorkflowWorker` | **unchanged** (no split) |
| `task_runs` CHECK | `role IN ('worker')` | **`role IN ('planner','worker')`** |
| `AttemptState::Planning` | `Planning {}` | **`Planning { planner_task_id: Option<TaskId> }` unchanged** — the `None`→`Some` transition is `start()`'s double-start guard (`orchestrator.rs:62-70`) |
| `WorkflowNodeId` | delete | may still die, **but** the spawn target must then carry `WorkflowTaskRole` + worker `work_item_id` (don't orphan `agent_run.rs:127`) |
| `AgentLaunch` | `task_id` in `Worker` kind only | **`task_id` in common fields** (both have it); `kind = Planner \| Worker { work_item_id, needs }`; `task_id()` infallible |
| planner id | — | drop `PlannerId`; add `planner_task_id(attempt_id) -> TaskId` |
| record path / finish / `settle_planner` | breaks (the seam) | **all unchanged** — the planner machinery is preserved |

Net: **Axis B (planner↔worker) is no longer a "widening"** — both stay task-owned.
The only planner-side change is dropping the redundant `PlannerId`. Everything on
**Axis A (generator+reducer→worker)** is unchanged from this review. Read §4 below
as historical analysis of *why* the planner-no-row option fails, not as the
recommendation.

---

## 0. Verdict

The spec is **directionally correct and well-disciplined** — the no-duplicate-status
principle, the typed-id posture, and the per-tool-file split are all good Rust
instincts. But it **under-exploits its own central simplification** and
**under-specifies the one genuinely new design problem** it creates.

Two sentences capture the whole review:

1. **The fold is bigger than the spec commits to.** Removing reducer doesn't just
   delete a role — it reveals that the "binary" generator/reducer machinery was
   *already a single code path wearing two labels*. The spec renames files but
   leaves several structs (`MaterializedPlan`, `AttemptState::Planning`'s field,
   `ids.rs`, `state/projections.rs`) standing that have no reason to exist
   post-fold.
2. **Planner-without-a-task-row breaks the recording model — so don't do it.**
   The spec makes the planner attempt-owned, which breaks the record-path and
   finish-path plumbing. **Resolution: keep the planner's task row** (see the
   DECISION UPDATE above). The planner is task-owned for *recording* but is never a
   member of the worker DAG. Drop only the redundant `PlannerId`.

Everything below is in service of those two points.

---

## 1. The single most important framing: there are TWO axes, not one

The spec's prose ("convert generator terminology to worker", "remove the binary
generator/reducer model") blurs two changes that behave **oppositely**. Getting
this wrong is the main risk in implementation.

```
                         BEFORE                          AFTER
   ┌───────────────────────────────────┐   ┌───────────────────────────────┐
   │ Planner ── task row (attempt-keyed)│   │ Planner ── NO task row          │
   │   │                                │   │   │        (attempt-owned)      │
   │   ▼  authors                       │   │   ▼  authors                    │
   │ Generator[] ──► Reducer[] (gate)   │   │ Worker[]  (leaves ARE the gate) │
   │   task rows       task rows        │   │   task rows                     │
   └───────────────────────────────────┘   └───────────────────────────────┘

   AXIS A  generator + reducer → worker     COLLAPSE HARD  (binary → singular)
   AXIS B  planner ↔ worker                 WIDENS 3-var enums → 2 DIVERGENT
```

| | Axis A — generator+reducer → worker | Axis B — planner ↔ worker |
|---|---|---|
| Nature | True binary→singular **merge** | A **divergence**, not a fold |
| Direction | 3 variants → 1 (or 2-arm → 1-arm) | 3 variants → **2 dissimilar** |
| Why | The two were already identical code | Planner loses its task row; worker keeps + gains `work_item_id` |
| Posture | Delete the redundant arm aggressively | Keep planner task-owned; only drop the redundant `PlannerId` |
| Risk | Low (deletion) | High (new persistence/record seam) |

**Evidence that Axis A is already degenerate today** (so the fold is mostly
deletion, not merging two different things):

- `build_execution_context` serves Generator and Reducer through the *same*
  function, differing only by the `ContextRole` tag passed in
  (`context/engine.rs:151-185`, called at `:75` vs `:83`). No reducer-specific
  rendering exists.
- `GeneratorSubmission` and `ReducerSubmission` are **byte-identical** 5-field
  structs (`state/tools/submissions.rs:46` vs `:61`).
- `record_generator_submission` and `record_reducer_submission` both funnel
  through one `mark_execution_task` + `ExecutionMark`
  (`attempt/orchestrator.rs:387-496`).
- `dag_resolution` / `ready_pending_plan_ids` are **role-agnostic** already
  (`attempt/plan_dag.rs:21-58`).

So "collapse the binary to singular" = **delete the second label**, not write a
merge.

**Why conflating the axes is dangerous:** if you treat the planner like the
reducer (delete it), you erase the planner *record subject* — its task-anchored
`workflow_planner` transcript node. If you treat the planner like the worker
(merge into one launch type), you blur the work-item identity onto a role that has
none. Keep them separate: the planner stays a task-owned record node (DECISION
UPDATE) but is never a worker-DAG member; only its redundant `PlannerId` is
dropped (derive `planner_task_id(attempt_id)`).

---

## 2. Binary → singular fold inventory (Axis A — collapse aggressively)

Every collapse the fold enables, with the current site and the aggressive target.
This is the answer to "dig hard into binary→singular folding."

| # | Surface (file:line) | Current | → Target | LOC | Risk |
|---|---|---|---|---|---|
| 1 | `TaskRole` (`request_task/task.rs:52`) + `TASK_AGENT_ROLES` | `Root/Planner/Generator/Reducer`, `[;4]` | `Root/Worker`, `[;2]` | −2 variants | med (serde/DB token) |
| 2 | `ExecutionRole`, `ExecutionTaskOutcome` (`outcomes.rs:42,63`) | gen/reducer evidence DTO | **delete**; worker `TaskStatus` + `WorkItemOutcome` | −60 | med |
| 3 | `GeneratorSubmission`+`ReducerSubmission` (`submissions.rs:46,61`) | two identical structs | one `WorkerOutcomeSubmission` (task_id **+** work_item_id) | −30 | low |
| 4 | `PlannerFailReason`, `PlannerFailureSubmission` (`submissions.rs:18,35`) | planner-failure DTOs | **delete** — planner failure = attempt-state transition | −20 | low |
| 5 | `AgentLaunch` enum + ~15 accessors (`attempt/launch.rs:58-278`) | 3 variants, every accessor a 3-arm match | 2 (see §4 for shape) | −200 | med |
| 6 | `AgentLaunchFactory::for_{planner,generator,reducer}` (`launch.rs:428-520`) | 3 near-identical builders | `for_planner` + `for_worker` | −80 | low |
| 7 | `record_{generator,reducer}_submission` + `mark_execution_task` (`orchestrator.rs:387-496`) | 2 recorders + `ExecutionMark` shim | one `record_worker_submission` | −90 | low |
| 8 | `synthesize_failure` 4-arm match (`run_stage.rs:287-327`) + `build_launch` reducer branch (`:138-159`) | Planner/Gen/Reducer/Root | planner / worker | −40 | low |
| 9 | `ContextScope`/`ContextRole` (`context/scope.rs`, `section.rs`) | 3 variants | 2 (Planner / Worker) | −40 | low |
| 10 | `WorkflowNodeId` (`contracts/record.rs:90`) + `workflow_task_id` 3 arms (`task_agent_run.rs:120`) | planner/gen/reducer id formatter | **delete**; `worker_task_id(attempt_id, work_item_id)` | −85 | low |
| 11 | `ids.rs` 6 helpers (`eos-workflow/src/ids.rs:14-95`) | planner/gen/reducer id derivation | **dissolve file** (see §5.3) | −83 | low |
| 12 | `PlannerId`/`GeneratorId`/`ReducerId` (`plan.rs:69-85`) | 3 macro newtypes | 1 `WorkItemId` | −2 macros | low |
| 13 | `validate_plan_shape` reducer rules + dangling-leaf reject (`plan_dag.rs:153-229`) | ≥1 reducer, reducer-needs, **reject dangling generator** | unique ids + needs-known + acyclic + ≥1 item; **leaf is valid** | −50 | low |
| 14 | `validate_plan_agents` fixed-reducer-profile check (`plan_dag.rs:259-268`) | requires registered `reducer` profile | delete | −12 | low |
| 15 | `project_iteration_outcomes` reducer-gate filter (`state/projections.rs:50-73`) | role-filtered pass/fail | **delete the whole file** (see §5.4) | −73 | med |
| 16 | `MaterializedPlan` (`plan.rs:259-278`) + `PlanDisposition` (`:142-188`) | planner_task_id + disposition + 2 id lists | **delete both** (see §5.1) | −90 | med |
| 17 | `TerminalTool` (`tools/terminal.rs:17-41`), `ToolName::Submit*` (`model.rs:305-371`) | 6 terminals | 5 (drop Reducer; Generator→Worker) | −1 variant ×3 sites | low |
| 18 | `tools/submission.rs` 6 inline submodules (1095 LOC) | one mega-file | 5 per-tool files + `support.rs` (see §5.5) | restructure | low |
| 19 | eos-db `attempts` columns `planner_task_id`,`generator_task_ids`,`reducer_task_ids` (`0001_initial.sql:128-130`) + `task_runs` CHECK `role IN ('planner','generator','reducer')` (`:75-78`) | 3 id columns + 3-role CHECK | drop the 2 worker id-lists (keep `planner_task_id`) → `work_items` JSON; CHECK `role IN ('planner','worker')` | net − | high |

Aggregate: this is a **strongly net-negative** migration. The fold removes well
over 800 LOC of branching/duplication across the workspace before any new code is
written.

---

## 3. What survives UNCHANGED (honest scope — don't churn it)

| Survives | Evidence | Note |
|---|---|---|
| `dag_resolution` Passed = "all Done" | `plan_dag.rs:40-42` | Already matches spec §11 "every required worker Done" — and it's *stronger* than "all leaves Done" (it's whole-DAG-Done). No behavioral change. |
| `ready_pending_plan_ids`, quiescence, `unreachable_pending_ids` | `plan_dag.rs:21-151` | Zero `ExecutionRole` references; reads only `needs` + `TaskStatus`. |
| `assert_acyclic` | `plan_dag.rs:272-321` | Unchanged modulo `GeneratorId`→`WorkItemId`. |
| Single-writer `advance_run_stage` JoinSet scheduler | `run_stage.rs:41-136` | Concurrency/cap logic intact. |
| `IterationOutcome` enum | `iteration.rs:39-54` | Already lifecycle-derived (`Complete`/`Continue{deferred_goal}`/`Failed`/`Cancelled`) — it already *embodies* the spec's "no status booleans" rule. |
| `AttemptBudget`, `DeferredGoal` newtypes | `plan.rs:88-256` | Keep; the fold should *use* `DeferredGoal` more, not less (§6). |
| `composer.rs` skill/terminal-block plumbing | `context/composer.rs:117-178` | Only the role arm in `render_task_guidance` (`:95-99`) collapses. |

The only label residue inside the surviving scheduler is `TaskStatus::is_terminal_generator`
(`task.rs:39`) — a status-set predicate misnamed after a role. Rename to
`is_terminal` (§6).

---

## 4. The under-specified seam: planner without a task row (Axis B) — SUPERSEDED

> **Superseded by the DECISION UPDATE at the top: keep the planner's task row.**
> The three breakages below are precisely *why* planner-no-row fails; they are the
> evidence for the reversal, not a list of problems to solve. With the row kept,
> none of these occur.

This is the part the spec leaves dangerous. The planner record is currently
**entirely a function of its `task_runs` row**; removing the row breaks three
concrete things.

### 4.1 Record path breaks structurally

`format_record_dir` builds the workflow leaf as `prefixed(role.task_segment_prefix(), index.task_id)`
(`contracts/record.rs:255,263-271`), and `AgentRunRecordIndex.task_id` is **non-optional**
(`:166`). The planner leaf today is literally `planner-task-<attempt>:planner:planner`.
With no `task_id` there is no value to interpolate. The engine layout also hard-requires it
(`eos-engine .../layout.rs:13-16`).

### 4.2 Finish path silently fails

`record_index_for_agent_run` → `finish_task_run` routes `Root|Workflow` to a
`task_runs` UPDATE keyed by `agent_run_id` (`eos-agent-run/src/persistence.rs:64-83`).
A planner with no `task_runs` row updates **0 rows** → the "row not updated" error
path (`:85-91`). **The spec never says where the planner's workflow coordinates
are read back from.** This is the #1 blocker to resolve.

### 4.3 Lifecycle signal disappears

`settle_planner` decides PLAN→RUN by reading the planner task row's status
(`orchestrator.rs:169-184`: `Done`→advance, `Failed`→stop, else synthesize).
With no row, that signal is gone.

### Recommended resolution (concrete)

| Question | Recommendation |
|---|---|
| Where do the planner record's wf-coords live? | **Carry them on `AgentRunRecordTarget` at spawn** so the finish path never re-resolves from `task_runs`. Smaller blast radius than adding `agent_runs` columns or a `planner_runs` table; the spawn already holds the coords. |
| `TaskAgentRunKind::Workflow{role}` shape | **Split into two variants:** `WorkflowPlanner { workflow }` (no `task_id`) and `WorkflowWorker { workflow }` (task-owned). Then `format_record_dir` never reads `task_id` on the planner arm. |
| `WorkflowTaskRole` (`record.rs:54-85`) | **Delete it.** Its 3 jobs all become structural after the split: path prefix → per-variant literal (`planner` / `worker-task`), `node_type` → variant match (`workflow_planner`/`workflow_worker`), DB round-trip → planner has no row so only `Worker` maps. (record-paths probe verdict: *refuted* that it's still needed.) |
| `AttemptState::Planning { planner_task_id: Option<TaskId> }` | **→ `Planning {}`** (no field). The planner's lifecycle signal becomes *"is `PlanOutcome` present on the attempt?"* — which the attempt state already knows. `settle_planner` checks `attempt.plan_outcome().is_some()` instead of the task row. Cleaner than the current row-status read. |
| `AgentRunRecordIndex.task_id` | Keep `TaskId` but ensure the `WorkflowPlanner` format arm ignores it, **or** make it `Option<TaskId>` (more honest, but touches `layout.rs` + the fake-store in `eos-agent-run/tests`). Pick the ignore-route for smaller blast radius; call it out. |

### 4.4 `AgentLaunch` shape — present as a real tradeoff (the verifier's core question)

The 3-variant enum with ~15 three-arm accessors (`launch.rs:58-278`) must become 2.
**Most of the accessor savings come from deleting the reducer arm (Axis A), not
from merging planner into worker.** Two viable targets:

| Option | Shape | Wins when |
|---|---|---|
| (a) Two structs behind a 2-variant enum | `enum AgentLaunch { Planner(PlannerLaunch), Worker(WorkerLaunch) }` | Conventional; each launch's fields stay flat and visible. |
| (b) **One struct + kind (recommended)** | `struct AgentLaunch { request_id, attempt_id, workflow_id, iteration_id, agent_name, context, agent_def, task_guidance, skill, kind: LaunchKind }` where `enum LaunchKind { Planner, Worker { task_id, work_item_id, needs } }` | 9 of ~11 fields are shared → centralizing them removes ~200 LOC of match boilerplate. Only `task_id()/work_item_id()/needs()` match on `kind`. |

Recommend **(b)**. Critically, putting `task_id` *inside* `LaunchKind::Worker`
makes "ask a planner launch for its task_id" a **type error** rather than a
fabricated id — a correctness win over today's `<attempt>:planner:planner` hack.
The repo's own guidance ("enums for closed sets, but a 2-variant enum whose
variants share 9 fields is a smell") points the same way.

---

## 5. Aggressive simplifications the spec misses or under-commits

The user asked to "simplify eos-workflow very aggressively." These are the
deletions the spec gestures at but doesn't actually commit to.

### 5.1 Delete `MaterializedPlan` outright (spec only deletes its fields)
Every field dies: `planner_task_id` (no planner row), `disposition` (was never even
a column — derived from `deferred_goal`, `rows.rs:417`), `generator_task_ids` /
`reducer_task_ids` (replaced by deterministic `worker_task_id`). Nothing survives,
so **the struct should be deleted**, and `AttemptState::Running` should hold
`PlanOutcome` directly (eos-db probe verdict: *confirmed*). The spec's §4 table
deletes the fields one-by-one but never states the struct dies — say it.

**Enumeration mechanism (the crux that makes deletion safe):** today the runtime
enumerates an attempt's plan tasks by chaining the stored id lists —
`plan_task_records` walks `generator_task_ids().chain(reducer_task_ids())`
(`orchestrator.rs:553-569`), and `cancel_attempt` does the same (`service.rs:259-265`).
With the lists gone, the runtime **enumerates workers by mapping
`worker_task_id(attempt_id, w.id)` over `PlanOutcome.work_items` and loading those
rows.** That single rewrite of `plan_task_records` is what lets the id-list columns
and `MaterializedPlan` disappear without losing the ability to find the attempt's
worker tasks — turning "deletable" from an assertion into a mechanism.

### 5.2 `AttemptState::Planning { planner_task_id }` is KEPT (see DECISION UPDATE)
The planner keeps its task row, so `Planning` keeps `planner_task_id: Option<TaskId>` —
the `None`→`Some` transition is `start()`'s double-start guard (`orchestrator.rs:62-70`).
The only attempt-state simplification is `Running`/`Closed` holding plan *contents*
instead of `MaterializedPlan` (§5.1).

### 5.3 Delete `eos-workflow/src/ids.rs` entirely (spec keeps it implicitly)
The whole id-helper block (`:14-95`) deletes; `worker_task_id` is born in
eos-types `state/workflow/work_item.rs` per §3 of the spec. The only survivor is
`WorkflowLifecycleConfig` (`:8-12`) — which **isn't about ids at all** and is
mis-homed; move it to `config.rs`. Net: the file disappears rather than lingering
as a ~12-LOC near-empty module.

### 5.4 `state/projections.rs` *dissolves* — it is not relocated
Its only non-trivial work is the reducer-gate filter (`:50-73`), which is
**deleted**, not moved. The residual task-row walk becomes worker-outcome
collection inside `attempt/work_items_run.rs` settlement; the rendering side goes
to `context/render.rs`. There is **no** new eos-workflow `outcome.rs`
(lifecycle probe verdict: *refuted* that it survives). Good — it removes a generic
"projections" middle layer between stores and rendering.

### 5.5 Per-tool split of `submission.rs` is well-motivated and right-sized
`submission.rs` already contains 6 inline submodules + a shared header
(`SubmissionStatus`, `OutcomeInput`, `is_blank`, `meta_obj`, `submission_ack_result`).
The spec's split into `submission/{mod,support,submit_root_task_outcome,
submit_plan_outcome,submit_worker_outcome,submit_advisor_outcome,
submit_subagent_outcome}.rs` maps 1:1 — the shared header → `support.rs`. After the
reducer-module deletion, ~7 files average ~120 LOC. This matches the repo's
"per-tool files named after wire tools" rule. **Approve.**

### 5.6 Context: 5 files → **3** (the spec over-splits the planner)
The spec proposes `planner_first_attempt.rs` + `planner_retry.rs` +
`planner_continuation.rs`. But all three emit the *same* `<workflow><goal>` +
`<current_iteration><goal>` scaffolding and differ only by which single evidence
block they attach — and the spec itself says planner context includes **"exactly
one of"** `<previous_attempts>` or `<latest_iteration>` (spec lines 633-639). That
is one mutually-exclusive `match`, not three renderers. Splitting it scatters the
"exactly one of" invariant across files and duplicates the scaffolding.
**Recommend `planner_context.rs` + `worker_context.rs` + `render.rs`** (render
absorbs the recipe-validation + `xml.rs` helpers). Note `dependency_sections`
(`engine.rs:258-292`) is worker-side and travels with `worker_context.rs`.

> ⚠️ The spec's §3 context list also **omits** `composer.rs`, `scope.rs`,
> `section.rs`, `xml.rs`. `composer.rs` (315 LOC, orthogonal) survives;
> `scope.rs` survives at 2 variants; `render.rs` should absorb `xml.rs` + the DTOs
> in `section.rs`. State this so files aren't deleted by omission.

### 5.7 `launch.rs` has no home in the spec's `attempt/` layout — gap
Spec §3 lists `attempt_run / active_attempt_runs / planner_run / work_items /
work_items_run`. It **omits** where `AgentLaunch`, `AttemptResources` (the DI
bundle), `AgentRunner` (the runner seam trait), and `AgentLaunchFactory` go — today
all in `launch.rs` (610 LOC, substantial). Recommend keeping `attempt/launch.rs`
(launch types + factory) and optionally `attempt/resources.rs` (the
`AttemptResources` bundle). Don't let 600 LOC vanish from the target tree by
omission.

### 5.8 `SubmissionStatus` vs `TaskOutcomeStatus` — a duplication the spec's own principle forbids
`SubmissionStatus` exists today as a **tool-private** enum that maps **1:1** onto
`TaskOutcomeStatus` (`submission.rs:15,29-31`). The current internal
`GeneratorSubmission.status` deliberately *reuses* `TaskOutcomeStatus` "(DRY, spec
§6.10)" (`submissions.rs:4`). The new spec puts `SubmissionStatus` on the
**internal** `WorkerOutcomeSubmission` — abandoning that DRY choice and creating a
parallel status enum at the eos-types boundary.

**Recommendation:** `SubmissionStatus` is legitimate as the *model-facing wire*
input enum (`SubmitWorkerOutcomeInput.status`). But the **internal**
`WorkerOutcomeSubmission.status` should stay `TaskOutcomeStatus`, with the
wire→internal map at the tool edge (exactly as today). Don't promote the wire enum
into the internal contract. If you do promote it, you must justify why two 1:1
enums coexist at the same boundary — which the spec's own no-duplicate principle
argues against.

### 5.9 `submission_kind` must be deleted at 4 emission sites
Forbidden by the spec, currently emitted in terminal metadata at
`submission.rs:149/156` (planner), `:458` (root), `:706` (generator), `:804`
(reducer), plus `tools/workflow.rs:112`. Phase 2/6 must remove all.

---

## 6. Naming verdict (file/folder, type, field)

Overall: **good conventions, with a handful of stringly regressions to fix.**

### 6.1 Field/type naming — fixes required

| Surface | Current / spec | Fix | Reason |
|---|---|---|---|
| `PlanOutcome.deferred_goal_for_next_iteration` (spec §6) | `Option<String>` | **`Option<DeferredGoal>`** | `DeferredGoal` newtype exists (`plan.rs:88`) **and** the sibling `Iteration.deferred_goal_for_next_iteration` is *already* `Option<DeferredGoal>` (`iteration.rs:76`). The spec is inconsistent with its own crate. Strongest-grounds stringly regression. |
| `WorkItemSpec.agent_name` | `AgentName` ✓ (spec §6) | keep | Correct — uses the existing newtype (`agent.rs:31`). |
| `*Input` wire DTOs (`SubmitPlanOutcomeInput`, `WorkItemSpecInput`) | `String` | keep `String` | Acceptable at the serde/wire boundary; convert to newtypes on ingest. |
| `TaskStatus::is_terminal_generator` (`task.rs:39`) + `TERMINAL_GENERATOR_STATUSES` | role-named status helper | **`is_terminal`** | It's a status-set predicate, not a role concept; "generator" is stale. |
| surviving `Task.agent_name` (`task.rs:108`) | `Option<String>` | consider `AgentName` | Avoid a stringly residue on the one surviving workflow task row. |
| `AdvisorVerdict` (spec §7) | new type | **good** | Today the verdict is a bare string `"approve"/"reject"` (`terminal.rs:112`); typing it is a genuine improvement. |

### 6.2 File/folder naming — verdict

| Spec name | Verdict |
|---|---|
| `work_items.rs` (validation/materialization) vs `work_items_run.rs` (waves/settlement) | **OK but thin distinction.** Acceptable — one owns plan→tasks, the other owns execution. Consider `work_items.rs` → `work_plan.rs` to sharpen "shape vs run". |
| `active_attempt_runs.rs` (rename of `orchestrator_registry.rs`) | **Good** — names ownership (active attempt/abort handles), not mechanism. |
| `attempt_run.rs` / `planner_run.rs` / `iteration_run.rs` / `workflow_run.rs` | **Good** — consistent `_run` ownership suffix; drops the "orchestrator/stage/dag" mechanism names per the spec's own rule. |
| planner context → 3 files | **Over-split** (§5.6). |
| context list omits `composer/scope/section/xml` | **Incomplete** (§5.6). |
| `attempt/` list omits `launch.rs` contents | **Gap** (§5.7). |
| eos-db given no target list | Fine — it's a column-narrowing, not a restructure; keep `rows.rs` + `repositories/attempt.rs`. Edit `0001_initial.sql` **in place** — *contingent on no deployed DB needing migration*; the schema header declares a no-legacy-migration posture (`0001_initial.sql:1-5`), so this is defensible but must be confirmed, not assumed. |

### 6.3 The cleanup-gate grep (spec §13 Phase 6) is realistic but skewed
Only **three** no-go names exist as real code today: `disposition`
(`plan.rs`, `contracts/workflow.rs:42`, `rows.rs:416`, callers), `submission_kind`
(`submission.rs`, `workflow.rs:112`), and `WorkflowNodeId` (`record.rs:90` +
consumers). The `answer` hits are prose strings (false positives). All other
no-go names have **zero** code matches — they're pre-emptive guardrails. So the
gate is real but will be dominated by `disposition` + `submission_kind` +
`WorkflowNodeId`; don't expect the long list to light up.

---

## 7. Resulting structure (annotated target, with my deltas vs the spec)

```text
agent-core/crates/eos-types/src/
  contracts/
    record.rs        # TaskAgentRunKind::Workflow UNCHANGED; WorkflowTaskRole kept {Planner,Worker};
                     #   WorkflowNodeId may die (spawn target then carries role + work_item_id)
    workflow.rs      # WorkflowAttemptSubmissionApi: 2 methods; PlanTask→WorkItemSpec
  state/
    request_task/task.rs   # TaskRole::{Root, Planner, Worker}; is_terminal (renamed)
    tools/submissions.rs   # WorkerOutcomeSubmission + PlanOutcomeSubmission (2, was 5)
                           #   internal status stays TaskOutcomeStatus (§5.8)
    workflow/
      attempt.rs     # AttemptState::Planning { planner_task_id } KEPT; Running holds plan contents
                     #   DELETE MaterializedPlan, PlanDisposition
      iteration.rs   # unchanged (IterationOutcome already lifecycle-derived)
      work_item.rs   # NEW: WorkItemId, WorkItemSpec, worker_task_id()  ← spec §3
      outcome.rs     # NEW: PlanOutcome, WorkItemOutcome, AttemptOutcome ← spec §3
                     #   (DELETE ExecutionRole, ExecutionTaskOutcome from outcomes.rs)

agent-core/crates/eos-workflow/src/
  attempt/
    attempt_run.rs        # thin coordinator: start/close/asserts
    active_attempt_runs.rs# abort handles + OpenIterationCoordinatorRegistry home (§ below)
    planner_run.rs        # planner launch + settle (task-owned; signal = planner task status, unchanged)
    work_items.rs         # plan validation + task materialization + readiness
    work_items_run.rs     # worker waves + settlement + missing-terminal synth + outcome collect
    launch.rs             # KEEP: AgentLaunch (struct+kind), AgentLaunchFactory, AgentRunner
    resources.rs          # optional: AttemptResources DI bundle
  context/
    planner_context.rs    # 3 spec files → 1 (exactly-one-of is a match)
    worker_context.rs     # + dependency_sections
    render.rs             # recipe dispatch + absorbs xml.rs + section DTOs
    composer.rs           # KEEP (orthogonal); collapse role arm only
    scope.rs              # ContextScope::{Planner, Worker} (2 variants)
  workflow_run.rs         # WorkflowApi start/check/cancel + create/close_workflow
  iteration_run.rs        # coordinator + retry + handle_iteration_closed + continuation
  attempt_submission.rs   # submit_plan_outcome + submit_worker_outcome adapter
  config.rs               # gains WorkflowLifecycleConfig (from the deleted ids.rs)
  # DELETED: ids.rs, state/projections.rs, attempt/{orchestrator,run_stage,plan_dag}.rs

agent-core/crates/eos-tool/src/tools/
  terminal.rs             # TerminalTool::{RootTask, Plan, Worker, Advisor, Subagent} (5)
  submission/{mod, support, submit_root_task_outcome, submit_plan_outcome,
              submit_worker_outcome, submit_advisor_outcome, submit_subagent_outcome}.rs

agent-core/crates/eos-db/   # edit 0001_initial.sql in place:
  # attempts: KEEP planner_task_id; DROP generator_task_ids, reducer_task_ids; ADD work_items (+plan_spec?)
  # task_runs CHECK: role IN ('planner','worker'); rows.rs: drop MaterializedPlan reconstruction + 'generator'/'reducer' parsing
```

**Open item flagged by the lifecycle probe:** `OpenIterationCoordinatorRegistry`
(`iteration.rs:309-357`) has no named home in the spec's 2-file lifecycle scheme.
It fits `active_attempt_runs.rs` (the spec already designates that file for
"in-process active attempt handles") or `iteration_run.rs`. **Name it explicitly.**

---

## 8. Resulting classes & fields (the concrete target type set)

```rust
// eos-types — IDs & plan shape
pub struct WorkItemId(String);                       // was GeneratorId (PlannerId/ReducerId deleted)
pub fn worker_task_id(attempt_id: &AttemptId, work_item_id: &WorkItemId) -> TaskId;

pub struct WorkItemSpec {
    pub id: WorkItemId,
    pub agent_name: AgentName,                        // typed ✓
    pub work_spec: String,                            // genuinely free-text ✓
    pub needs: Vec<WorkItemId>,
}

// eos-types — outcomes (NO status booleans; success from lifecycle enums)
pub struct PlanOutcome {
    pub attempt_id: AttemptId,
    pub plan_spec: String,
    pub work_items: Vec<WorkItemSpec>,
    pub deferred_goal_for_next_iteration: Option<DeferredGoal>,  // FIX: was Option<String>
}
pub struct WorkItemOutcome { pub attempt_id, pub task_id, pub work_item_id: WorkItemId, pub outcome: String }
pub struct AttemptOutcome { pub attempt_id, pub plan_outcome: Option<PlanOutcome>, pub work_item_outcomes: Vec<WorkItemOutcome> }

// eos-types — lifecycle (DELETE MaterializedPlan, PlanDisposition)
pub enum AttemptState {
    Planning { planner_task_id: Option<TaskId> },     // UNCHANGED — guards double-start
    // NOTE: PlanOutcome carries attempt_id (spec §6), but here the Attempt already
    // owns the id — storing the whole PlanOutcome duplicates it. Apply the spec's own
    // no-duplicate rule: hold the plan CONTENTS, not the self-referential outcome DTO.
    Running { planner_task_id: TaskId, plan_spec: String, work_items: Vec<WorkItemSpec>,
              deferred_goal_for_next_iteration: Option<DeferredGoal> },  // FIX: was MaterializedPlan
    Closed  { closure: AttemptClosure, planner_task_id: Option<TaskId>,
              plan: Option<(/* plan_spec, work_items, deferred_goal */)> },
}
pub enum TaskRole { Root, Planner, Worker }           // was 4 (Reducer removed; Generator→Worker)

// eos-types — record subjects: planner KEEPS its task row, so Workflow stays whole
pub enum TaskAgentRunKind {
    Root,
    Workflow { workflow: WorkflowCoordinates, role: WorkflowTaskRole },  // role = Planner | Worker
    Parented { parent_agent_run_id: AgentRunId, kind: ParentedAgentRunKind },
}
pub enum WorkflowTaskRole { Planner, Worker }         // kept (node_type + path prefix); was 3
// WorkflowNodeId may still be deleted, but then SpawnAgentTarget must carry
// WorkflowTaskRole + the worker's work_item_id (don't orphan agent_run.rs:127).

// eos-types — submissions (5 → 2; internal status stays TaskOutcomeStatus)
pub struct WorkerOutcomeSubmission { pub attempt_id, pub task_id, pub work_item_id: WorkItemId,
                                     pub status: TaskOutcomeStatus, pub outcome: String, pub terminal_payload: JsonObject }
pub struct PlanOutcomeSubmission   { pub attempt_id, pub plan_spec: String, pub work_items: Vec<WorkItemSpec>,
                                     pub deferred_goal_for_next_iteration: Option<DeferredGoal>, pub terminal_payload: JsonObject }

// eos-workflow — launch (struct+kind; task_id is SHARED since planner keeps its row)
pub struct AgentLaunch { /* 9 shared fields */ pub task_id: TaskId, pub kind: LaunchKind }
pub enum   LaunchKind  { Planner, Worker { work_item_id: WorkItemId, needs: Vec<TaskId> } }
// task_id() is now infallible (both planner and worker have one).

// eos-workflow — context (3 variants → 2)
pub enum ContextScope {
    Planner { workflow_id, iteration_id, attempt_id },
    Worker  { workflow_id, iteration_id, attempt_id, task_id, work_item_id },
}
```

---

## 9. Prioritized fix list (do before implementing)

1. **Keep the planner's TaskStore row (DECISION UPDATE).** Reverse spec §2/§5/§14;
   drop only `PlannerId` and derive `planner_task_id(attempt_id)`. This *removes*
   the former #1 blocker (record/finish seam) rather than solving it. `TaskRole`
   becomes `Root \| Planner \| Worker`; `WorkflowTaskRole` stays `{ Planner, Worker }`.
2. **Keep `AttemptState::Planning { planner_task_id }`; `Running` holds plan
   contents (§5.1)**; declare `MaterializedPlan` + `PlanDisposition` deleted, not
   just their fields. (Do **not** make `Planning` field-less — it guards double-start.)
3. **Keep `TaskAgentRunKind::Workflow` whole and `WorkflowTaskRole` at `{ Planner, Worker }`**
   (DECISION UPDATE) — the planner stays task-owned, so the record/finish path is unchanged.
4. **Fix `PlanOutcome.deferred_goal_for_next_iteration` to `Option<DeferredGoal>` (§6.1).**
5. **Keep internal submission `status: TaskOutcomeStatus`; don't promote the wire
   `SubmissionStatus` into the internal DTO (§5.8).**
6. **Reduce context to 3 files; add `composer/scope/render` to the target list (§5.6).**
7. **Add `launch.rs` (+ optional `resources.rs`) to the `attempt/` target list (§5.7).**
8. **Name the `OpenIterationCoordinatorRegistry` home (§7).**
9. **Rename `is_terminal_generator` → `is_terminal` (§6.1).**
10. **Decide the persisted `Iteration.outcomes`/`Workflow.outcomes` column shape**
    post-fold (`Vec<WorkItemOutcome>`? `AttemptOutcome`? nothing?) — it's a
    serde/persisted-state contract change needing golden coverage (lifecycle probe Q).

---

## Appendix — methodology & confidence

Grounded by 6 parallel probe agents (eos-db, record-paths, context, lifecycle,
types-ids each completed with file:line evidence; eos-tool filled by direct
reading after its agent hit a socket drop) plus 3 adversarial verifiers. The
verifier agents were lost to repeated `socket connection closed` errors before
emitting structured output, so their adversarial role (MaterializedPlan deletion,
planner-without-task ripple, naming/duplication audit) was performed here directly
from the probes' own `verifications` arrays — which independently returned
*confirmed* on MaterializedPlan deletability, *confirmed* on the record-path break,
and *refuted* on `WorkflowTaskRole` survival — and from a prior reviewer-model
pass. Confidence is high on §1–§3 and §5–§6 (direct code evidence), and high on the
*existence* of the §4 problems with the §4 *resolutions* offered as the
recommended (not the only) design.
