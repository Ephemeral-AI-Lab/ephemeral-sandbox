# Review — phase-03b-execution-lineage-materialization_SPEC.md

Reviewer pass: 2026-06-09. Grounded against the real `eos-db`, `eos-agent-run`
(+ retired `eos-agent-runner`), `eos-workflow`, and `eos-engine/records` source,
and cross-checked against phase-00 (lock), phase-02 (DAG/contract floor),
phase-04 (engine/run split), and `index.md`. Claims about current behaviour were
adversarially verified against the code; two of my own first-pass claims were
corrected by that pass and are flagged inline.

## Verdict (one position per question)

| Q | Position |
| --- | --- |
| **Q1 — healthier/cleaner shape?** | **Yes, net materially healthier** — and on the highest-value axis (the durable data model), close to *much* healthier. It deletes real current mess: column duplication, a flat 13-optional spawn bag, the forbidden `AgentRunMessageRecordKind` + generic `Agent` run kind, the `instruction`/`initial_messages` dual-intent, and — the biggest correctness win — the filesystem-scan + `parents-missing/` placement hack. Held back from "much healthier" by four self-inflicted issues below. |
| **Q2 — boundaries / SRP / Rust shape?** | **Mostly right, two concrete boundary bugs.** The table split, the closed `SpawnTarget`, and *moving record resolution out of the engine* are correct boundary moves. But (a) `SpawnTarget`/`SpawnAgentTaskArgs` are assigned to the wrong crate for the port architecture, and (b) "record-dir resolution in `eos-db`" fuses a legitimate lineage *query* with path *formatting* that is not persistence. |
| **Q3 — refactor more aggressively?** | **One flagship + one scope-cut.** Flagship: collapse `AgentRunRecordIndex`'s 9-of-11 optional fields into a closed `Task{..}|Parented{..}` enum (the spec's own `SpawnTarget` pattern, applied to fix its own inconsistency). Scope-cut: the 6 nested `*ExecutionTree` read-model types are the heaviest net-new surface in a plan whose headline is 291→150 modules; most are speculative for v1. |

**Resolve the open fork now (keep the escape hatch).** The spec gates Migration
Step 2 on "task/run merge vs retry-split." Current behaviour settles it —
**merge** (§1.1) — so stop gating the migration on the decision. Don't *delete*
the analysis: retain it as a one-paragraph contingency, not a blocker — *if*
single-run-per-task resume is ever prioritized, the split back to `tasks` +
`agent_runs` is localized to the `task_runs` section and admission atomicity, and
every other contract here is identical. Render the call; keep the documented
fallback.

## 1. Healthier shape? (Q1)

### 1.1 The data-model wins are real, and grounded

| Dimension | Today (verified) | 03B target | Verdict |
| --- | --- | --- | --- |
| Run tables | `tasks` + `agent_runs`; `agent_runs.task_id` is **`UNIQUE` but `NULLABLE`** (`0001_initial.sql:94-105`) → overloaded: 0..1 run per task **plus** task-less subagent/advisor runs stored as `NULL task_id` | `task_runs` (task-bound, total) + `parented_runs` (total) — task-backed per **fix #6**, see Appendix | **Strong win.** The split is a *normalization* of the overloaded table, not just "fold run into task." |
| Column duplication | `terminal_tool_result` **and** `agent_name` defined on **both** `tasks` and `agent_runs` (`rows.rs:50/112`, `47/111`) | one owning row | **Win** — removes dual-write/dual-parse (`row_to_task` + `row_to_agent_run` both decode `terminal_tool_result`) |
| Model-visible intent | `tasks.instruction TEXT NOT NULL` **and** `agent_runs.initial_messages` both carry intent | `initial_messages` only; `messages.jsonl` is the audit | **Win** — kills the dual source of truth |
| Spawn input | `SpawnAgentRequest` = **flat bag of 13 optionals** (`eos-types/contracts.rs`) | closed `SpawnTarget` / `SpawnAgentTaskArgs` | **Strong win** — both-set/neither-set become unrepresentable |
| Spawn classification | `AgentRunMessageRecordKind` incl. a generic `Agent` fallback variant (a name 03B/index explicitly forbid) | `TaskRole` + `ParentedRunKind`; no generic `Agent` run | **Win** — removes the banned name and the task-less generic run |
| Spawn return | bare `AgentRunId` | `SpawnAgentResult { agent_run_id, task_id }` | **Win** — callers stop re-deriving the task id |
| Parent lineage | **not a durable column** — lives only in the in-memory `AgentRunRecordKind` enum + JSON event payload; placement uses a recursive `std::fs::read_dir` scan with a `parents-missing/` fallback (`eos-engine/records/layout.rs:103-111,149-181`) | durable `parented_runs.parent_agent_run_id`; path derived from lineage | **Strongest win** — replaces a filesystem race/scan with a queryable column |
| outstanding workflow query | previously accepted an agent-run id without using it in the removed workflow service path | durable `launched_by_agent_run_id` column; exact query | **Win** — closed by deleting the stale service path and keeping the exact query at the `WorkflowApi` implementation boundary |
| Plan materialization | `MaterializedPlan` stores only resolved task ids (`plan.rs:236-256`) | stores planned spawn *inputs* + reserved ids; rows created at admission | **Win** — no early "pending" rows |

The merge is **safe**, and the verification settles the fork:
`agent_runs.task_id` is `UNIQUE` (≤1 run/task) with an integration test proving a
second run on a live `task_id` errors (`eos-db/tests/integration.rs:400-409`).
Retries are **attempt-level**: a failed attempt calls `create_attempt(...)` →
fresh `AttemptId::new_v4` + next `attempt_sequence_no`, spawning **new**
planner/generator/reducer tasks whose ids are namespaced by the new attempt
(`eos-workflow/ids.rs:12-25`). There is no in-task re-run path. So
multi-run-per-task is not latent → **merge, don't keep the split branch alive.**

> Note for the spec text: describe retries via `attempt_sequence_no` — there is
> **no** `reattempt` creation reason. `IterationCreationReason` is `{Initial,
> DeferredGoalContinuation}`; retry identity lives in the attempt sequence under
> `uq_attempt_iteration_sequence`. Don't anchor the merge rationale on a
> non-existent enum variant.

### 1.2 What keeps it from *much* healthier

Four issues, all fixable inside this spec: the `AgentRunRecordIndex` flat-bag
self-contradiction (§3.1), the `SpawnTarget` crate misplacement (§2.1), the
`eos-db` resolution conflation (§2.2), and the read-model over-build (§3.2).

## 2. Boundaries / SRP (Q2)

### 2.1 Bug: `SpawnTarget`/`SpawnAgentTaskArgs` are assigned to the wrong crate

The crate-ownership table (spec §Crate Ownership) gives **`eos-agent-run`** the
`SpawnTarget` / `SpawnAgentTaskArgs` types. This would **move them out of where
they already live**: today the spawn argument (`SpawnAgentRequest`) and the
`AgentRunApi` trait both sit in **`eos-types/src/contracts.rs`** — phase-02 already
sank that contract floor (its tracker marks "AgentRunApi contracts now live in
`eos-types`" as Done). So the table isn't an open placement choice; it's a
*regression* that relocates a contract-floor type down into a behaviour crate.

That regression breaks the port. Phase-02 is a hard rule (`phase-02:42-46,383`):
**`eos-workflow` must not depend on `eos-agent-run`**; it spawns
planner/generator/reducer runs **only through the injected `eos-types::AgentRunApi`
port**. `spawn_agent`'s argument *is* `SpawnTarget`, so for `eos-workflow` to
construct `SpawnTarget::Planner{..}` and call the port, the type must be reachable
from `eos-workflow` — i.e. it must stay in **`eos-types`** on the `AgentRunApi`
contract, alongside `AgentRunRecordIndex` and friends. Placing it in
`eos-agent-run` forces the forbidden edge and trips the `dependency_dag` guard.

03B's creation flow shows `Workflow ->> Run: spawn_agent(...)` and "`eos-agent-run`
called by `eos-workflow`" **without naming the port** — and an exploratory pass of
the current code literally inferred the fix as "add `eos-agent-runner` to
`eos-workflow`'s `Cargo.toml`," which is exactly the forbidden edge. This is the
one boundary error most likely to be implemented wrong.

**Fix:** (1) put `SpawnTarget`/`SpawnAgentTaskArgs`/`SpawnAgentResult` in
`eos-types` (the `AgentRunApi` argument/return contract); leave only the
*admission behaviour* (`task_runs`/`parented_runs` row writes, `AgentType`
validation) in `eos-agent-run`. (2) State in the creation flow and acceptance
criteria that workflow→run spawn crosses `dyn AgentRunApi`, never a crate edge.

### 2.2 Smell: "record-dir resolution in `eos-db`" fuses query + formatting

Verified: **all** record path-string construction (`requests/`, `root-task-`,
`subagents/`, `advisors/`, `workflows/`, the planner/generator/reducer `-task`
prefixes) lives in `eos-engine/records/layout.rs` + `kind.rs`; `eos-db` contains
**zero** path logic (its only `"requests"`/`"workflows"` literals are SQL
table-name labels). Moving resolution out of the engine is the **right** call —
the engine should write into a pre-resolved dir. But the spec then lands *two
jobs* in `eos-db`:

| Job | Correct owner |
| --- | --- |
| walk durable lineage → coordinates (root task id, workflow coords, parent run id) | **`eos-db`** (a real query) |
| format coordinates → `requests/<id>/.../agent-run-<id>` string | **pure function in `eos-types`** (record layout is a cross-crate contract, not persistence) — or the `eos-agent-core` facade |

Burying `format!("{prefix}-{id}")` inside the SQL repository layer is the same
category error as putting layout in the engine, moved one crate over. Keep the
`AgentRunRecordDir` formatter pure and co-located with the layout contract.

**Bonus (same principle):** the spec's "no filesystem scan" rule names only the
*parent* case. The current code also fs-scans for the **workflow root**
(`find_root_agent_dir`) and for **read-side** `read_messages`/`read_events`
(`resolve_agent_run`, `layout.rs:8-14`). A durable `parent_agent_run_id` does not
remove those. State that the read path and workflow-root path resolve from
lineage too, or they silently remain scans after 03B "removes" scanning.

### 2.3 Acknowledged tension (lower severity): cross-aggregate binding in spawn

`Task(Root)` binds `Request.root_task_id` and `Task(Planner)` binds
`Attempt.planner_task_id` — mutations to aggregates owned by `eos-agent-core`
(request) and `eos-workflow` (attempt) — from inside `eos-agent-run::spawn_agent`.
The spec defends this as *structural* atomicity ("one store owns both rows"),
which is reasonable.

> Correction to my own first pass: I initially called the "removes cross-store
> atomic admission" justification a strawman. **It is not.** Today `tasks` and
> `agent_runs` are written through **two separate repository objects**
> (`SqlRequestTaskStore` / `SqlAgentRunStore`, `composition.rs:23-87`) via two
> non-transactional `INSERT`s with no shared transaction handle — atomic task+run
> admission is genuinely *unexpressible*. The merge removes a **real** problem.
> The only precision nit: "cross-**store**" overstates it — it is
> cross-*repository* within one SQLite pool, not cross-*database*. Keep the
> justification; soften the word.

The residual tension is ownership, not atomicity: writing `Attempt.planner_task_id`
from the run crate couples run-spawn to an attempt-internal column. The spec's own
rule "callers pass workflow decisions; `eos-agent-run` persists run rows" mostly
contains it; just confirm the planner-binding is an `eos-workflow`-supplied id, not
an attempt-policy decision leaking into the run crate.

### 2.4 What is correctly bounded (don't second-guess these)

- Engine receives only `AgentRunRecordTarget` (resolved dir + 4 anchors), never
  the lineage coordinate set — correct narrowing.
- `eos-db` doing flat lineage reads while the `eos-agent-core` facade composes the
  nested tree — correct query/composition split.
- `ParentedRunKind` on the parent-owned row rather than as an engine-readable
  flag — keeps classification out of execution. Defensible even though it
  denormalizes a `tool_use_id`-derivable fact.
- The `messages.jsonl` / `events.jsonl` row contracts (base anchor fields + the
  closed `event_type` list) were reviewed and are sound and mechanical — the
  per-row `request_id`/`agent_run_id`/`task_id` anchors line up with the two-table
  model, and the closed event enum is the right shape. No change requested.

## 3. Refactor more aggressively (Q3)

### 3.1 Flagship: make `AgentRunRecordIndex` a closed enum (fixes a self-contradiction)

The spec forbids flat optional-id bags for spawn ("`SpawnTarget` makes task-owned
vs parent-owned a closed choice; the illegal both-set and neither-set states … are
unrepresentable") and then, **one section later**, ships `AgentRunRecordIndex` as
exactly that bag: **9 of its 11 fields are `Option`**, with the task-owned /
parent-owned XOR pushed into prose ("a task-owned index sets `task_id` and
`task_role`; a parent-owned index sets `parent_task_id`, `parent_agent_run_id`,
`parented_kind`"). Prose-encoded mutual exclusion is the textbook tell for a
missing enum.

Adversarial check settled the exact move:

- **Full elimination (resolver takes the row) — rejected.** The field-subset test
  passes (every coordinate the resolver needs is on `TaskRun`/`ParentedRun`), but
  handing the resolver the whole row drags `initial_messages`, `message_history`,
  `status`, `outcomes`, `terminal_tool_result`, `token_count`, `error` into a
  *placement-string* function — trading a flat-bag smell for a fat-input-with-
  prompt-content smell, and violating the spec's own content/coordinate layering
  (rule "`SpawnAgentTaskArgs` carries record-index/admission facts only"). A
  content-free coordinate projection *should* exist.
- **Enum-collapse — adopt.** Keep the Index as the projection, fix its shape:

```rust
pub struct AgentRunRecordIndex {
    pub request_id: RequestId,        // shared, outside the enum
    pub agent_run_id: AgentRunId,
    pub locus: AgentRunRecordLocus,
}

pub enum AgentRunRecordLocus {
    Task {
        task_id: TaskId,
        task_role: TaskRole,
        workflow: Option<WorkflowCoords>,   // {workflow_id, iteration_id, attempt_id} move together
    },
    Parented {
        parent_task_id: TaskId,
        parent_agent_run_id: AgentRunId,
        kind: ParentedRunKind,
    },
}
```

One field further: **`tool_use_id` does not belong on this DTO at all.** The
Index's stated job is "input to record-dir resolution," but **no** record-layout
segment encodes a tool-use id (`root-task-`, `…-task-`, `subagent-run-`,
`advisor-run-`, `agent-run-` — verified against `eos-engine/records/layout.rs`).
It is an *admission* fact, not a *placement* fact: it belongs on
`SpawnAgentTaskArgs`, from which it populates the durable `parented_runs.tool_use_id`
/ `workflows.tool_use_id` columns. Carrying it on a resolution-input DTO is the
same content/coordinate bleed the §2.4 layering rule forbids — drop it here, keep
it on the spawn args.

That makes the collapse a **net reduction** (9 optionals → 2 closed arms, minus a
field, plus one `WorkflowCoords` newtype that also kills the "set all three
workflow ids together" invariant on `task_runs`); it mirrors `SpawnTarget`
exactly, and it removes the spec's only internal contradiction — squarely
on-theme for a 291→150-module plan. It belongs in `eos-types` next to
`SpawnTarget` (see §2.1).

### 3.2 Scope-cut: the nested read model is over-built for v1

The materialized read model adds the heaviest net-new surface in the spec:
`RequestExecutionTree`, `TaskExecutionNode`, `WorkflowsHydration`,
`WorkflowExecutionTree`, `IterationExecutionTree`, `AttemptExecutionTree`,
`PlanNodeView` (+ `PlannedNode`). The spec itself says the v1 requirement is "one
workflow level plus subagents and advisors," and makes `WorkflowsHydration::Ids`
the **default** — which means the four deep `*ExecutionTree` types only feed the
opt-in `Hydrated` arm that has **no stated v1 consumer**.

**Aggressive cut:** ship `TaskExecutionIndex` (the flat, load-bearing child-id
surface — keep it, it also drives path generation) + a single `TaskExecutionNode`
(task_run, index, subagents, advisors, `workflow_ids`). Defer
`WorkflowExecutionTree`/`IterationExecutionTree`/`AttemptExecutionTree`/`PlanNodeView`
until a reader needs the deep walk. That removes ~5 DTOs and the
`Planned`/`Spawned` view enum from the v1 contract without losing any stated
capability; the `eos-db` lineage query already returns enough to reconstruct them
on demand later.

### 3.3 Considered and *rejected*: unifying the three discriminators

`AgentType` (profile) / `TaskRole` (task_runs) / `ParentedRunKind` (parented_runs)
look collapsible — `ParentedRunKind` is `tool_use_id`-derivable and maps 1:1 to
`AgentType::Subagent|Advisor`. A single `RunRole {Root,Planner,Generator,Reducer,
Subagent,Advisor}` keyed to the table split is *tempting*.

**Don't.** It breaks the per-table totality the spec deliberately prizes: a
6-variant `RunRole` on `task_runs` would have 2 illegal variants, reintroducing
exactly the "this column is meaningless for this row" class the two-total-tables
design exists to remove. This is *bolder*, not *better* — the opposite of §3.1,
which is strictly better with no downside. Keep the spec's three-discriminator
split; it earns its keep.

## 4. Cross-phase consistency risks (carry into phase-04, not blockers for 03B)

| Drift | Detail | Action |
| --- | --- | --- |
| `AgentRunRecordTargetFile` is undefined | phase-04 (line 24) says it "consumes `AgentRunRecordIndex` and `AgentRunRecordTargetFile`"; 03B defines `AgentRunRecordTarget` + `AgentRunRecordDir`, no `…File`. | Reconcile names; 03B's are canonical. |
| "engine consumes the index" reads as a contradiction | phase-04 says it consumes `AgentRunRecordIndex`; 03B's *Forbidden engine input* bans handing the engine the full index. Most likely phase-04 means the *phase* consumes it, not the engine struct — soft, but the undefined name above makes it look harder than it is. | Clarify phase-04 wording. |
| `eos-agent-ports` still named live in phase-04 | `index.md` retires it (→ `eos-types`); 03B agrees (puts contracts in `eos-types`); only phase-04 still calls it the launch-contract owner. | 03B is consistent; fix is in phase-04/index, noted here so 03B's successor doesn't inherit the stale edge. |

## 5. Minor (footnote)

- `TaskRun.initial_messages: Option<Vec<JsonObject>>` while the spec requires
  "every spawn provides non-empty `initial_messages`." If always-present at
  admission, drop the `Option`. Also note the typed `Vec<Message>` (spawn input)
  is downgraded to untyped `Vec<JsonObject>` on the row — inherited from current
  code, but worth a typed `Message` if the row is being redesigned anyway.
- Three terminal-state representations co-exist on the merged row (`outcomes`,
  `terminal_tool_result`, `message_history`). Inherited from today; out of 03B's
  scope, but a candidate for a later pass once the merge lands.

## Bottom line

03B is a **good spec doing the right things in the right order** — nailing the
durable model before phase-04 moves files is correct sequencing, and the model it
defines is genuinely cleaner than the code it replaces. Land it, but first: (1)
**commit to the merge** and drop the fork, (2) move `SpawnTarget` to `eos-types`
and name the `AgentRunApi` port on the workflow→run edge, (3) split the `eos-db`
lineage *query* from the pure path *formatter*, (4) collapse `AgentRunRecordIndex`
to a closed enum, and (5) trim the speculative deep read-model tree to v1 scope.
Items (2) and (4) are the two that, left as written, will be implemented wrong.

---

# Appendix — Post-fix target shape (folders, types, fields)

The end-state once 03B lands **with the five fixes applied**, sitting on the
phase-02 DAG and phase-04 file trees. Verified for DAG-acyclicity and
field-fidelity against the spec. Only the crates 03B touches are shown.

> **Directive (fix #6): every run is task-backed.** Subagent and advisor runs now
> own their **own** `task_id` (not just a `parent_task_id`), so `task_id` is
> **non-optional** everywhere a run is identified. This reverses the spec's
> Non-Goal "do not turn every subagent or advisor into a task run" — but only in
> the *identity/lifecycle* sense: a parented run gets a `task_id` + `status` +
> outcome like any task, while staying **parent-launched and not
> workflow-schedulable**, which was that Non-Goal's real intent. The two-table
> split **survives and is strengthened**: both tables are now "(task, run) in one
> row," differing only by *workflow-scheduled* (`task_runs`) vs *parent-launched*
> (`parented_runs`) — every column stays total, no nullable union. Bonus: `task_id`
> lifts out of `AgentRunRecordLocus` into the shared header, and the
> `messages.jsonl`/`events.jsonl` `task_id` anchor becomes always-present.

> **Directive (fix #7): the only `task_id` *input* is `Parented.parent_task_id`.**
> `spawn_agent` never receives the spawned run's own `task_id` (always
> admission-output), and the four `Task(..)` launches pass **zero** `task_id`s.
> `needs` is **removed from the spawn input** and from `task_runs`: it is plan +
> context-engineering data on `MaterializedPlan`/`PlannedNode`, used by the
> workflow for (a) DAG readiness gating in `run_stage` and (b) composing the
> generator/reducer `initial_messages` from dependency **outcomes** *before* the
> call. By the time `spawn_agent` runs, the node is already ready and its context
> is already baked into `initial_messages`. `local_id: PlanNodeId` stays (node
> identity, not a `TaskId`).

## A. Ownership / resolution flow

```mermaid
flowchart TD
    WF["eos-workflow\nplanner/generator/reducer decisions"] -->|dyn AgentRunApi (eos-types)| RUN
    CORE["eos-agent-core (facade)\ncreate Request, wire dyn stores"] -->|spawn_agent Task(Root)| RUN
    RUN["eos-agent-run\nadmission + AgentType validation"] -->|dyn TaskRunStore / ParentedRunStore| DB
    RUN -->|dyn AgentRunRecordTargetResolver| DB
    DB["eos-db\nlineage WALK -> AgentRunRecordIndex"] -->|format_record_dir pure| TYPES
    TYPES["eos-types\nDTOs + ports + pure path formatter"]
    RUN -->|AgentLoopExecutionRequest + AgentRunRecordTarget| ENG["eos-engine/records\nwrite into pre-resolved record_dir"]
    ENG --> TYPES
    DB --> TYPES
```

Key edge corrections vs a naive reading: `eos-workflow` and `eos-agent-run`
**never** name `eos-db` or each other — row writes cross `dyn *Store` ports in
`eos-types`; record-dir resolution crosses `dyn AgentRunRecordTargetResolver`;
the path-string **format** is a pure `eos-types` function (precedent:
`parse_markdown_frontmatter` already lives there). `eos-agent-core` is the only
crate that wires concrete `eos-db` impls into those ports.

## B. Folder tree (● new · ◑ changed · ✗ removed · → moved)

```text
agent-core/crates/
├── eos-types/                         # contract floor (passive only)
│   └── src/
│       ├── contracts.rs               ◑ AgentRunApi + SpawnTarget/SpawnAgentTaskArgs/
│       │                                SpawnAgentRequest/SpawnAgentResult; WorkflowApi
│       │                                ✗ AgentRunMessageRecordKind, WorkflowTaskRole
│       ├── record.rs                  ● AgentRunRecordIndex(enum)+Locus+WorkflowCoords,
│       │                                AgentRunRecordTarget, AgentRunRecordDir,
│       │                                pure fn format_record_dir()   (path segments live here)
│       ├── stores.rs                  ◑ TaskRunStore, ParentedRunStore (replace TaskStore+
│       │                                AgentRunStore), RequestStore, AgentRunRecordTargetResolver,
│       │                                Workflow/Iteration/AttemptStore
│       ├── agent.rs                     AgentType{Agent,Subagent,Advisor} (no AgentRole)
│       └── state/
│           ├── lineage.rs            ● TaskRun, ParentedRun, TaskRole, ParentedRunKind,
│           │                            TaskStatus, WorkflowCoords, TaskExecutionIndex
│           ├── runtime/task.rs       ✗ (Task DTO folded into TaskRun)
│           ├── engine/agent_run.rs   ✗ (AgentRun DTO folded into TaskRun/ParentedRun)
│           └── workflow/
│               ├── workflow.rs       ◑ + launched_by_agent_run_id, tool_use_id
│               └── plan.rs           ◑ MaterializedPlan + PlannedNode spawn inputs
├── eos-db/                            # persistence; -> eos-types only
│   ├── migrations/
│   │   └── 0002_execution_lineage.sql ● CREATE task_runs, parented_runs; DROP tasks,
│   │                                    agent_runs; ALTER workflows ADD launch cols
│   └── src/
│       ├── rows.rs                    ◑ TaskRunRow, ParentedRunRow (replace TaskRow, AgentRunRow)
│       └── repositories/
│           ├── task_run.rs           ● (from agent_run.rs + request_task.rs task half)
│           ├── parented_run.rs       ● subagent/advisor rows
│           ├── request.rs            ◑ (request half of old request_task.rs)
│           ├── lineage.rs            ● TaskExecutionIndex query + record-dir lineage WALK
│           │                            (returns AgentRunRecordIndex; calls eos-types formatter)
│           └── workflow.rs|iteration.rs|attempt.rs
├── eos-agent-run/                    # lifecycle; -> eos-types, eos-engine (NO eos-db edge)
│   └── src/
│       ├── service.rs                 ◑ AgentRunService impl eos-types::AgentRunApi;
│       │                                spawn_agent(SpawnAgentRequest)->SpawnAgentResult
│       ├── admission.rs              ● SpawnTarget -> dyn *Store writes; AgentType validation;
│       │                                root/planner one-tx binding
│       └── records.rs                ◑ resolve AgentRunRecordTarget via dyn resolver before engine
├── eos-engine/                       # execution only; -> eos-types, eos-tool, ...
│   └── src/records/
│       ├── service.rs                 ◑ start_agent_run(AgentRunRecordTarget) -> write into record_dir
│       ├── layout.rs                 ✗ fs-scan + parents-missing + node_dir formatting (moved to eos-types)
│       └── kind.rs                   ✗ AgentRunRecordKind, WorkflowTaskRole
├── eos-workflow/                     # -> eos-types, eos-tool (NO eos-agent-run edge)
│   └── src/
│       ├── (WorkflowApi impl)          ◑ list_open_delegated_workflows_for_agent_run queries launched_by_agent_run_id
│       └── attempt/orchestrator.rs    ◑ task_store.insert_task(..)  ->  dyn AgentRunApi.spawn_agent(Task(..))
└── eos-agent-core/                   # facade; -> all
    └── src/
        ├── request.rs                 ◑ create Request + spawn_agent(Task(Root))
        └── (read facade)             ● TaskExecutionNode (single, v1)
                                         DEFERRED: RequestExecutionTree, Workflow/Iteration/
                                         AttemptExecutionTree, PlanNodeView, WorkflowsHydration
```

## C. Types & fields (post-fix)

```rust
// ─── eos-types/src/contracts.rs ───────────────────────────────────────────
pub trait AgentRunApi: Send + Sync {                 // consumed by workflow + agent-core
    async fn spawn_agent(&self, req: SpawnAgentRequest) -> Result<SpawnAgentResult, AgentRunError>;
    // wait_for / poll / cancel …
}
pub enum SpawnTarget {
    Task(SpawnAgentTaskArgs),
    Parented { parent_task_id: TaskId, parent_agent_run_id: AgentRunId, kind: ParentedRunKind },
}
pub enum SpawnAgentTaskArgs {                         // NO task_id input; own task_id is admission-output
    Root     { request_id: RequestId },
    Planner  { request_id: RequestId, workflow_id: WorkflowId, iteration_id: IterationId, attempt_id: AttemptId },
    Generator{ request_id: RequestId, workflow_id: WorkflowId, iteration_id: IterationId, attempt_id: AttemptId,
               local_id: PlanNodeId },                // fix #7: `needs` is plan/context, not a spawn input
    Reducer  { /* same as Generator */ },
}
pub struct SpawnAgentRequest {
    pub agent_run_id: Option<AgentRunId>, pub agent_name: AgentName,
    pub target: SpawnTarget, pub initial_messages: Vec<Message>,
    pub tool_use_id: Option<ToolUseId>,              // durable launch fact; NOT a record-dir input
    // sandbox / workspace / model / cancellation inputs
}
pub struct SpawnAgentResult { pub agent_run_id: AgentRunId, pub task_id: TaskId }  // fix #6: never None

// ─── eos-types/src/record.rs  (NEW — fix #3 + #4 + #6) ────────────────────
pub struct AgentRunRecordIndex {                     // task_id lifted to shared header (fix #6)
    pub request_id: RequestId, pub agent_run_id: AgentRunId, pub task_id: TaskId,
    pub locus: AgentRunRecordLocus,
}
pub enum AgentRunRecordLocus {                       // closed; replaces the 9-of-11 optional bag
    Task     { role: TaskRole, workflow: Option<WorkflowCoords> },          // task_id now shared above
    Parented { parent_task_id: TaskId, parent_agent_run_id: AgentRunId, kind: ParentedRunKind },
}                                                    // tool_use_id intentionally absent
pub struct AgentRunRecordTarget { pub request_id: RequestId, pub agent_run_id: AgentRunId,
                                  pub task_id: TaskId, pub record_dir: AgentRunRecordDir }  // fix #6: non-opt
pub struct AgentRunRecordDir(/* normalized request-rooted path */);
pub fn format_record_dir(index: &AgentRunRecordIndex) -> AgentRunRecordDir;  // pure; owns path segments

// ─── eos-types/src/state/lineage.rs  (NEW — the merge) ────────────────────
pub struct TaskRun {                                 // task AND its 1:1 main run, one row
    pub task_id: TaskId, pub agent_run_id: AgentRunId, pub request_id: RequestId,
    pub role: TaskRole, pub status: TaskStatus,
    pub workflow_id: Option<WorkflowId>, pub iteration_id: Option<IterationId>, pub attempt_id: Option<AttemptId>,
    pub agent_name: AgentName,                        // `needs` removed -> MaterializedPlan/PlannedNode (fix #7)
    pub initial_messages: Option<Vec<JsonObject>>,   // (footnote: could be non-Option — always set at spawn)
    pub message_history: Option<Vec<JsonObject>>,
    pub outcomes: Vec<ExecutionTaskOutcome>, pub terminal_tool_result: Option<JsonObject>,
    pub token_count: i64, pub error: Option<String>,
    pub created_at: UtcDateTime, pub updated_at: UtcDateTime, pub finished_at: Option<UtcDateTime>,
}
pub struct ParentedRun {                             // subagent/advisor; task-backed, parent-launched (fix #6)
    pub task_id: TaskId,                             // OWN task id (≠ parent_task_id below)
    pub agent_run_id: AgentRunId, pub request_id: RequestId, pub status: TaskStatus,  // lifecycle, like any task
    pub parent_task_id: TaskId, pub parent_agent_run_id: AgentRunId, pub kind: ParentedRunKind,
    pub tool_use_id: Option<ToolUseId>,              // durable launch fact lives HERE, not on the index
    pub agent_name: AgentName,
    pub initial_messages: Option<Vec<JsonObject>>, pub message_history: Option<Vec<JsonObject>>,
    pub terminal_tool_result: Option<JsonObject>, pub token_count: i64, pub error: Option<String>,
    pub created_at: UtcDateTime, pub finished_at: Option<UtcDateTime>,
}
pub enum TaskRole { Root, Planner, Generator, Reducer }
pub enum ParentedRunKind { Subagent, Advisor }
pub enum TaskStatus { Pending, Running, Done, Failed, Blocked, Cancelled }
pub struct WorkflowCoords { pub workflow_id: WorkflowId, pub iteration_id: IterationId, pub attempt_id: AttemptId }
pub struct TaskExecutionIndex {                      // flat read surface; drives record paths
    pub task_id: TaskId, pub agent_run_id: AgentRunId,
    pub workflow_ids: Vec<WorkflowId>, pub subagent_ids: Vec<AgentRunId>, pub advisor_ids: Vec<AgentRunId>,
}

// ─── eos-types/src/stores.rs  (ports; impl in eos-db, dyn-consumed) ───────
pub trait TaskRunStore { /* admit/finish task_runs; bind Request.root_task_id, Attempt.planner_task_id */ }
pub trait ParentedRunStore { /* insert/finish parented_runs */ }
pub trait AgentRunRecordTargetResolver { async fn resolve(&self, agent_run_id: &AgentRunId) -> AgentRunRecordTarget; }

// ─── eos-agent-core facade (v1 read model — trimmed, fix #5) ──────────────
pub struct TaskExecutionNode {
    pub task_run: TaskRun, pub index: TaskExecutionIndex,
    pub subagents: Vec<ParentedRun>, pub advisors: Vec<ParentedRun>,
}   // workflow ids come from index.workflow_ids; deep *ExecutionTree types deferred
```

## D. DB schema (eos-db migration)

| `task_runs` | `parented_runs` | `workflows` (+cols) |
| --- | --- | --- |
| `task_id` PK | `task_id` PK ● (own task, fix #6) | `id` PK |
| `agent_run_id` UNIQUE | `agent_run_id` UNIQUE | `request_id` |
| `request_id` (idx) | `request_id` (idx) | `parent_task_id` |
| `role`, `status` | `kind`, `status` ● | `launched_by_agent_run_id` ● |
| `workflow_id`/`iteration_id`/`attempt_id` (idx) | `parent_task_id`+`parent_agent_run_id` (idx) | `tool_use_id` ● |
| `agent_name` | `tool_use_id`, `agent_name` | lifecycle fields |
| `initial_messages`, `message_history` | `initial_messages`, `message_history` | |
| `outcomes`, `terminal_tool_result` | `terminal_tool_result` | |
| `token_count`, `error` | `token_count`, `error` | |
| `created_at`/`updated_at`/`finished_at` | `created_at`/`finished_at` | |
| invariant: planner/gen/red rows require workflow+iter+attempt; root row = `Request.root_task_id` | invariant: every column total; own `task_id` ≠ `parent_task_id` | |

Dropped: `tasks`, `agent_runs` (incl. their duplicated `terminal_tool_result` /
`agent_name`); no `instruction`, no `AgentRunKind`/`record_kind`/`parents-missing/`.

## E. What moves, vs today

| Today | Post-fix | Driver |
| --- | --- | --- |
| `tasks` + `agent_runs` (1:1, + NULL-task runs) | `task_runs` + `parented_runs` (two total tables) | merge + normalize |
| `SpawnAgentRequest` 13-optional bag + `AgentRunMessageRecordKind` | closed `SpawnTarget`/`SpawnAgentTaskArgs` in `eos-types` | fix #2 |
| record paths formatted in `eos-engine/records/layout.rs` (+ fs-scan, `parents-missing/`) | lineage WALK in `eos-db/lineage.rs` + pure `format_record_dir` in `eos-types` | fix #3 |
| `AgentRunRecordIndex` 9-optional bag (in spec) | closed `Locus` enum, `tool_use_id` dropped | fix #4 |
| 6 nested `*ExecutionTree` DTOs | flat `TaskExecutionIndex` + one `TaskExecutionNode` | fix #5 |
| subagent/advisor = task-**less** (`NULL task_id`), `task_id: Option` in DTOs | every run task-**backed**: `parented_runs.task_id` (own), `task_id: TaskId` non-optional, lifted to shared record header | fix #6 (directive) |
| `needs` on `task_runs` + in `SpawnAgentTaskArgs::Generator/Reducer` | `needs` only on `MaterializedPlan`/`PlannedNode`; not a spawn input, not a run-row column | fix #7 (directive) |

### spawn_agent `task_id` rules (post fix #6 + #7)

| Launch | `task_id` passed **in** | own `task_id` |
| --- | --- | --- |
| root / planner / generator / reducer | **none** | admission-output (returned) |
| subagent / advisor | `parent_task_id` only (parent linkage) | admission-output (returned) |

Invariant: the **only** `task_id` input to `spawn_agent` is `Parented.parent_task_id`.
A run never names its own `task_id`; dependency edges (`needs`) are resolved into
`initial_messages` by the workflow before the call. `local_id` (Generator/Reducer)
is a `PlanNodeId`, not a `TaskId`.

---

# Round 2 — Redundant-field audit

Focused pass on duplicate / derivable fields across the spec's DTOs and tables,
taking **fix #6** into account: every run is now strictly 1:1 `task_id ↔
agent_run_id` on **both** tables — which removed `Option<TaskId>` but also made
several stored id-pairs **provably derivable**. Grounded in spec line numbers.

### Tier 1 — pure duplication, free deletion

| # | Redundant field | Duplicates | Evidence | Action |
| --- | --- | --- | --- | --- |
| R1 | `AgentLoopExecutionRequest.agent_run_id` | `record_target.agent_run_id` | spec 540 vs 547 — same id twice in one struct | Drop the top-level field; read `req.record_target.agent_run_id`. Ironic: the spec proudly "does not hand both the index and a near-duplicate target" (561-564), then hands `agent_run_id` twice. |
| R2 | `TaskExecutionNode.index` | `task_run` ids + `subagents`/`advisors` lists + `workflows` | spec 697-703: `index.{task_id,agent_run_id}` = `task_run`'s; `index.subagent_ids` = `subagents.map(agent_run_id)`; `index.advisor_ids` = `advisors`; `index.workflow_ids` = `workflows` | Drop `index` from the hydrated node. Keep `TaskExecutionIndex` ONLY as the standalone cheap-query type (its stated purpose, 763); a node that already expands the children doesn't also need the id surface. |

### Tier 2 — derivable via the 1:1 invariant (the fix-#6 dividend)

| # | Redundant field | Derivable from | Evidence | Action |
| --- | --- | --- | --- | --- |
| R3 | `parented_runs.parent_task_id` | `parent_agent_run_id` (+ 1:1 lookup) | spec 168-169 store BOTH, 184 indexes BOTH; placement uses `parent_agent_run_id` (620), materialization uses `parent_task_id` (749) | Store one (keep `parent_agent_run_id` — placement needs it), derive the other; or keep both but label `parent_task_id` a *denormalized index*, not "required." The redundancy rule at 813 ("store both… the pair is the anchor") is now one-too-many. |
| R4 | `workflows.parent_task_id` | `launched_by_agent_run_id` (+ 1:1) | spec 204-205; `find_outstanding` now queries `launched_by_agent_run_id` (209-211), so `parent_task_id`'s query role is supplanted; only `workflow_ids` materialization (748) still reads it | Same as R3 — keep as a labeled denormalized materialization index, or derive it. |

### Tier 3 — record-file vs row duplication (pick one source of truth)

| # | Redundant field | Duplicates | Evidence | Action |
| --- | --- | --- | --- | --- |
| R5 | `message_history` (both tables) | `messages.jsonl` (the audit transcript) | spec 137, 174 ("terminal/replay history"); the row "messages.jsonl is the audit" rule | Transcript already lives in `messages.jsonl`. Pick ONE: drop `message_history` and read the file for replay, or declare the file a projection of the column. Carrying both is the dual-source the merge removes elsewhere. |
| R6 | `outcomes` + `terminal_tool_result` (+ `message_history` tail) | each other / the transcript | task_runs 137-138; parented 175 | Three terminal-state reps on one row. `terminal_tool_result` is the last tool message in the transcript; `outcomes` is a normalized projection of it + role/status. Verify whether `outcomes` is derivable from `terminal_tool_result` + role; if so, store one and derive. |

### Tier 4 — acknowledged / conventional (spec already defends; no action unless minimizing)

- `parented_runs.kind` — spec admits derivable from `tool_use_id` (191-194); post-fix-#6 also equals the profile `AgentType`. Kept for placement query-locality. Redundant but defensible.
- `request_id` on `task_runs`/`parented_runs`/`workflows` — derivable, denormalized as the audit/query anchor (rule 814). Acceptable.
- `AgentRunRecordTarget.{request_id, agent_run_id, task_id}` vs `record_dir` — the dir path encodes all three; carrying them typed avoids path-parsing per row write. Justified.
- messages/events per-row `request_id`/`agent_run_id`/`task_id` — constant within a file (the path encodes them); redundant per-row but standard self-describing JSONL.
- events.jsonl `payload` announce-ids duplicate the durable row — spec states the row is source of truth, the event is audit (668). Intentional.

**Bottom line.** **R1, R2 are free deletions** (pure duplication). **R3, R4 are the
fix-#6 dividend** — the same 1:1 invariant that justified the merge now makes the
stored parent-id pairs derivable; keep at most one of each pair, explicitly
labeled as a denormalized index if retained. **R5, R6 are pre-existing
terminal-state / transcript triplication** the merge left untouched — pick one
source of truth. None block the spec; all shrink the row/DTO surface, on-theme
for 291→150.
| `eos-workflow` writes `task_store.insert_task()` | `eos-workflow` calls `dyn AgentRunApi.spawn_agent` | merge + port |
