# impl-eos-workflow — delegated workflow lifecycle: starter, per-attempt orchestration, run-stage scheduler, context engine

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §9 (also §"SRP, Naming, and Prompt Gaps to Close").

## 1. Purpose & Responsibility (SRP)

`eos-workflow` is the **delegated-workflow lifecycle layer**. From a running parent
`Task` it mints a `Workflow -> Iteration -> Attempt` tree and drives each Attempt's
planner-authored generator/reducer DAG to a terminal outcome. It owns: `WorkflowStarter`
(validate parent, create workflow + first iteration + first attempt; **leave the parent
task running**); the per-Attempt `AttemptOrchestrator` state machine and its RUN-stage
scheduler (`AttemptStageAdvancer`); `WorkflowLifecycle` (iteration-chain extension + close
projection); `IterationAttemptCoordinator` + `OpenIterationCoordinatorRegistry` (attempt
retry budget); `AttemptOrchestratorRegistry`; the agent-launch surface (`AgentLaunch`,
`AgentLaunchFactory`, `AttemptDeps`, the `AgentRunner` seam); and the
**context-builder** (`ContextEngine`, `AgentContext`, `ContextSection`, `ContextScope`,
`AgentEntryComposer`) plus the run-stage DAG status (`DagStatus`).

This crate **must NOT**: add a global agent orchestrator (orchestration is per-Attempt only,
anchor §2); create a synthetic root workflow (the root request is a root `Task`); mutate the
parent `Task` at workflow close (the parent owns its own terminal submission); own lifecycle
*policy* inside the context engine (the `ContextEngine` only assembles `AgentContext` from
store state); open a DB, build SQL, run agent inference, or call any provider directly. It
also does **not** own the structural planner-DAG validator (`ordered_plan_tasks`) — see §5/§10
and GC-eos-workflow-04; that lives in `eos-tools` by the anchor §5a DAG tie-breaker.

## 2. Dependencies

- **Upstream crates (depends on):** `eos-types` (newtype IDs, `UtcDateTime`, `Clock`,
  `CoreError` — anchor §5); `eos-state` (`Workflow`/`Iteration`/`Attempt` DTOs + all
  status/stage/reason enums, the four submission DTOs, `ExecutionTaskOutcome` projections,
  the per-entity `Store` traits — impl-eos-state.md); `eos-tools` (`ToolName`, terminal
  descriptors, and the structural plan validator `ordered_plan_tasks` — impl-eos-tools.md);
  `eos-agent-def` (`AgentDefinition`, `AgentRole`, `AgentRegistry`, `context_recipe`
  frontmatter — impl-eos-agent-def.md); `eos-audit` (`AuditSink`, workflow audit events —
  impl-eos-audit.md). Matches overview line 102: `eos-workflow -> state, tools, agent-def,
  audit`. **No edge to `eos-engine`** in either direction (both are referenced only by
  `eos-runtime`); the agent-run call and the terminal-submission result are runtime-wired,
  not direct calls (see §3, §7, §8, GC-eos-workflow-03).
- **Downstream consumers (used by):** `eos-runtime` only (composition root wires the runner,
  the agent registry, stores, and audit sink; it also hosts the `delegate_workflow` tool's
  call into `WorkflowStarter`).
- **Implements (downstream-state ports owned by `eos-tools`, anchor §6b):**
  `WorkflowControlPort` (backs `delegate_workflow` / `check_workflow_status` /
  `cancel_workflow` through `WorkflowStarter` + status/cancel) and
  `PlanSubmissionPort` (backs planner/generator/reducer submission through the
  `AttemptOrchestrator`). These are `eos-tools`-owned traits (impl-eos-tools.md
  §5.6); `eos-workflow` supplies the concrete impls, injected into tool
  `ExecutionMetadata` at the composition root. This rides the existing
  `eos-workflow -> eos-tools` edge — no new dependency — and is distinct from the
  `eos-workflow`-**owned** `AgentRunner` seam (§6a; §5/§7 below).
- **External crates** (pinned via `[workspace.dependencies]`, inherited with
  `foo = { workspace = true }`, `proj-workspace-deps`):

  | Crate | Use | Justification / rust-skills |
  |---|---|---|
  | `tokio` | multi-thread runtime handle (`spawn`, `JoinSet`, `sync`); the run-stage scheduler is a single-writer async loop | `async-tokio-runtime`, `async-joinset-structured` |
  | `tokio-util` | `CancellationToken` for parent-exit / workflow-cancel of the attempt scheduler | `async-cancellation-token` |
  | `async-trait` | `dyn`-safe `async fn` on the injected `AgentRunner` trait + audit sink used behind `Arc<dyn ...>` | anchor §6 object-safety note |
  | `parking_lot` | `Mutex` for the small orchestrator/coordinator registries — synchronous insert/remove (no await held), `!Send` guard, no poison under `panic=unwind` | `own-mutex-interior`, anchor §7 |
  | `futures` | stream combinators in the scheduler | anchor §7 |
  | `serde` (derive) | `AgentContext`/`ContextSection`/`AgentLaunch`/`StartedWorkflow` (de)serialize for audit + parity snapshots | `api-common-traits` |
  | `schemars` | `JsonSchema` on `AgentContext`/`ContextSection` for the Phase-0 context-snapshot parity test | anchor §11 |
  | `thiserror` | the single `WorkflowError` enum (§8) replacing `WorkflowInvariantViolation` | `err-thiserror-lib`, `err-custom-type` |
  | `tracing` | structured launch/quiescence/compensation logs (Python uses `logging.exception`) | — |

  No `serde_json`: JSON (de)serialization of the persisted `outcomes` string is an `eos-db`
  boundary concern (impl-eos-state.md §2); this crate passes typed `ExecutionTaskOutcome`
  values to the store traits.

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped |
|---|---|---|
| `workflow/starter.py` | `starter.rs` | `StartedWorkflow` DTO + `WorkflowStarter`. `_assert_parent_running_and_no_open_child`, `_compensate_failed_start` (saga rollback) preserved. **Parent task NOT mutated** beyond reading status/request_id. |
| `workflow/attempt/orchestrator.py` | `attempt/orchestrator.rs` | `AttemptOrchestrator` PLAN→RUN→CLOSED state machine. `apply_plan_submission`/`apply_planner_failure`/`apply_generator_submission`/`apply_reducer_submission`/`_close_attempt`/`start`. |
| `workflow/attempt/plan_dag.py` | `attempt/plan_dag.rs` | **Split (GC-eos-workflow-04).** `DagStatus`, `dag_status`, `ready_pending_plan_ids`, `_validate_persisted_needs`, `_unreachable_pending_ids`, `_topo_order` move here (owned). `ordered_plan_tasks` + `_assert_lane_shape`/`_assert_acyclic` move to **eos-tools** (referenced); sole caller is the planner submission tool. |
| `workflow/attempt/run_stage.py` | `attempt/run_stage.rs` | `AttemptStageAdvancer` → the single-writer `JoinSet` scheduler loop (§7). Python self-recursion → loop iterations. `_launch_ready_plan_task`, `_mark_launch_failed`, `_advance_run_stage`. |
| `workflow/attempt/launch.py` | `attempt/launch.rs` | `AgentLaunch`, `AttemptDeps`, `AgentLaunchFactory`, the `AgentRunner` trait. **`EphemeralAttemptAgentLauncher` is dropped:** its spawn job moves to the run-stage `JoinSet` scheduler (run_stage.rs), its exhaustion synthesis (`_report_exhaustion`) moves to the scheduler's no-terminal-report mapping (§8.4), and its `_fail_unowned_attempt` orchestrator-callback path is eliminated (the single writer makes an unowned attempt impossible; GC-eos-workflow-03). `runner` becomes an **injected `AgentRunner`** on `AttemptDeps` (the Python `AttemptAgentRunner` default `from engine.api import run_ephemeral_agent` is removed; the runtime supplies the concrete value). |
| `workflow/attempt/orchestrator_registry.py` | `attempt/orchestrator_registry.rs` | `AttemptOrchestratorRegistry` + `RegisteredAttemptOrchestrator` slice → an internal liveness map (`pub(crate)`), no longer a cross-crate seam (GC-eos-workflow-03). |
| `workflow/context_engine/context.py` | `context/section.rs` | `ContextSection {tag, attrs, text, children}`, `AgentContext {role, sections, directive, context_limits}`. |
| `workflow/context_engine/engine.py` | `context/engine.rs` | `ContextEngine`, `ContextEngineDeps`, `build_planner_context`/`build_generator_context`/`build_reducer_context`, `validate_context_recipe`. |
| `workflow/context_engine/scope.py` | `context/scope.rs` | `ContextScope` + `for_planner`/`for_generator`/`for_reducer`; `Literal` role + optional-field shape → enum + `Option`. |
| `workflow/agent_launch/composer.py` + `entry_messages.py` | `composer.rs` | `AgentEntryComposer`, `AgentEntryMessages`; XML render + skill/task-guidance wrapping. |
| `workflow/iteration/attempt_coordinator.py` | `iteration/coordinator.rs` | `IterationAttemptCoordinator`, `OpenIterationCoordinatorRegistry`, `OrchestratorFactory` alias. Retry-budget loop preserved. |
| `workflow/lifecycle.py` | `lifecycle.rs` | `WorkflowLifecycle` (create workflow/iteration, `handle_iteration_closed`, `close_workflow`, deferred-goal continuation). |
| `workflow/_core/primitives.py` | `ids.rs` | `planner_task_id`/`generator_task_id`/`reducer_task_id` stable-id builders; `WorkflowLifecycleConfig`. `WorkflowInvariantViolation` → `WorkflowError` (§8). |
| `workflow/submissions.py` | — | **Dropped here**; the four submission DTOs are owned by `eos-state` (impl-eos-state.md §3). Referenced. |

**In scope:** starter, per-attempt orchestration + RUN scheduler, iteration coordination,
lifecycle, context builder + composer, agent launch surface, run-stage DAG status.
**Out of scope (non-goals, anchor §2):** global orchestrator; synthetic root workflow;
parent-task mutation at close; structural planner-DAG validator; the agent inference call;
JSON persistence; provider/sandbox access; submission DTO definitions.

## 4. File & Module Layout

```
src/
  lib.rs                     // pub use re-exports of the public surface (proj-pub-use-reexport)
  error.rs                   // WorkflowError (single thiserror enum), Result alias
  ids.rs                     // planner/generator/reducer task-id builders; WorkflowLifecycleConfig
  starter.rs                 // StartedWorkflow, WorkflowStarter (+ compensation saga)
  lifecycle.rs               // WorkflowLifecycle
  iteration/
    coordinator.rs           // IterationAttemptCoordinator, OpenIterationCoordinatorRegistry, OrchestratorFactory
  attempt/
    orchestrator.rs          // AttemptOrchestrator state machine
    orchestrator_registry.rs // AttemptOrchestratorRegistry (pub(crate) liveness map)
    run_stage.rs             // AttemptStageAdvancer = single-writer JoinSet scheduler
    plan_dag.rs              // DagStatus + persisted-needs/reachability/quiescence (owned half)
    launch.rs                // AgentLaunch, AttemptDeps, AgentLaunchFactory, AgentRunner trait
  context/
    section.rs               // ContextSection, AgentContext
    scope.rs                 // ContextScope
    engine.rs                // ContextEngine, ContextEngineDeps, role builders
    composer.rs              // AgentEntryComposer, AgentEntryMessages
    xml.rs                   // render_context_xml / render_task_guidance (pub(crate))
```

`lib.rs` re-exports the public types (`pub use`); `orchestrator_registry`, `xml`, and the
`_topo_order`-style helpers are `pub(crate)` (`proj-pub-crate-internal`). Module-by-feature
(`proj-mod-by-feature`).

## 5. Contracts Owned Here

Per the Ownership Map (anchor §5), this crate owns the workflow lifecycle machinery and the
context builder; everything below is fully specified here. Contracts **referenced** (defined
elsewhere, never re-specified):

- `Workflow`, `Iteration`, `Attempt`, `WorkflowStatus`, `IterationStatus`,
  `IterationCreationReason`, `AttemptStage`, `AttemptStatus`, `AttemptFailReason`,
  `ExecutionTaskOutcome` + projections, `PlannerSubmission`/`PlannerFailureSubmission`/
  `GeneratorSubmission`/`ReducerSubmission`, and the `WorkflowStore`/`IterationStore`/
  `AttemptStore`/`TaskStore`/`RequestStore` traits + `Task` DTO — **eos-state**
  (impl-eos-state.md §5/§6).
- `ordered_plan_tasks` (structural planner-DAG validator) — **eos-tools** (impl-eos-tools.md;
  GC-eos-workflow-04).
- `AgentDefinition`, `AgentRole`, `AgentRegistry`, `context_recipe` — **eos-agent-def**.
- `AuditSink`, workflow audit events — **eos-audit**.
- `TaskId`, `WorkflowId`, `IterationId`, `AttemptId`, `RequestId`, `UtcDateTime`, `Clock`,
  `CoreError` — **eos-types**.

**Owned, fully specified here:** `WorkflowStarter` + `StartedWorkflow`; `AttemptOrchestrator`
+ `AttemptOrchestratorRegistry`/`RegisteredAttemptOrchestrator`; `AttemptStageAdvancer`;
`DagStatus` + run-stage DAG-status functions; `AgentLaunch`, `AttemptDeps`, the `AgentRunner`
trait (the injected runner seam), `AgentLaunchFactory`;
`WorkflowLifecycle`; `IterationAttemptCoordinator`, `OpenIterationCoordinatorRegistry`,
`OrchestratorFactory`; `WorkflowLifecycleConfig`; `ContextSection`, `AgentContext`,
`ContextScope`, `ContextEngine`, `ContextEngineDeps`, `AgentEntryComposer`,
`AgentEntryMessages`. The only trait seam is the **injected `AgentRunner`** (§7), a named
`#[async_trait]` trait recorded on the anchor §6 seam map (§6a), implemented by the `eos-runtime`
adapter (and a test double); the data structs above are not `dyn`-implemented externally and need
no sealing (`api-sealed-trait` N/A).

## 6. Types, Fields & Schemas

All field names/types are taken from the Python source except where noted (the `AttemptDeps.runner`
field is a Rust-only DI addition — see §6a / overview); `str` ids become eos-types
newtypes; `dict[str, Any]` task rows become the typed `eos_state::Task` (`type-no-stringly`,
GC-eos-workflow-05); `Literal[...]` become enums (`type-enum-states`). Public structs that may
grow carry `#[non_exhaustive]` (`api-non-exhaustive`); all derive `Debug, Clone, PartialEq`
(`api-common-traits`).

### `StartedWorkflow` (starter.rs)

| Field | Rust type | Notes | source |
|---|---|---|---|
| `parent_task_id` | `TaskId` | the launching running task | starter.py:33 |
| `parent_attempt_id` | `Option<AttemptId>` | parent's own attempt, if any | starter.py:34 |
| `workflow_id` | `WorkflowId` | | starter.py:35 |
| `iteration_id` | `IterationId` | first iteration | starter.py:36 |
| `attempt_id` | `AttemptId` | first attempt | starter.py:37 |

### `AgentLaunch` (attempt/launch.rs) — launch descriptor for one harness agent run

| Field | Rust type | Notes | source |
|---|---|---|---|
| `task_id` | `TaskId` | | launch.py:76 |
| `request_id` | `RequestId` | | launch.py:77 |
| `attempt_id` | `Option<AttemptId>` | | launch.py:78 |
| `role` | `AgentRole` | referenced from eos-agent-def | launch.py:79 |
| `agent_name` | `String` (profile name) | resolved by composer | launch.py:80 |
| `context` | `String` | `<context>...</context>` envelope, persisted on the task | launch.py:81 |
| `task_guidance` | `Option<String>` | `<Task Guidance>` prose | launch.py:82 |
| `needs` | `Vec<TaskId>` | dependency ids | launch.py:83 |
| `agent_def` | `Option<AgentDefinition>` | resolved definition | launch.py:84 |
| `workflow_id` | `Option<WorkflowId>` | | launch.py:85 |
| `skill` | `Option<String>` | row-4 `Load skill:` body | launch.py:86 |

### `ContextSection` / `AgentContext` (context/section.rs)

| `ContextSection` field | Rust type | Notes | source |
|---|---|---|---|
| `tag` | `String` | XML tag | context.py:11 |
| `attrs` | `Vec<(String, String)>` | insertion order preserved (matches Python dict) for golden parity | context.py:12 |
| `text` | `Option<String>` | | context.py:13 |
| `children` | `Vec<ContextSection>` | recursive | context.py:14 |

| `AgentContext` field | Rust type | Notes | source |
|---|---|---|---|
| `role` | `ContextRole` (`Planner`/`Generator`/`Reducer`) | enum; was `Literal[...]` | context.py:19 (Rust-only name; Literal → named enum) |
| `sections` | `Vec<ContextSection>` | | context.py:20 |
| `directive` | `String` | | context.py:21 |
| `context_limits` | `Vec<String>` | default empty | context.py:22 |

### `DagStatus` (attempt/plan_dag.rs) — single-pass plan-task summary

| Field | Rust type | Notes | source |
|---|---|---|---|
| `all_quiescent` | `bool` | every task terminal, or pending-but-unreachable | plan_dag.py:174 |
| `all_done` | `bool` | every task `Done` | plan_dag.py:175 |
| `any_failed_or_blocked` | `bool` | any `Failed`/`Blocked` | plan_dag.py:176 |

`dag_status(&[Task]) -> Result<DagStatus, WorkflowError>` and
`ready_pending_plan_ids(&[Task]) -> Result<Vec<TaskId>, WorkflowError>` are pure over a typed
task slice (`own-slice-over-vec`); `_validate_persisted_needs`/`_unreachable_pending_ids`
become `pub(crate)` helpers returning `WorkflowError` on unknown persisted needs / a
persisted dependency cycle.

### `AttemptDeps` (attempt/launch.rs) — the per-attempt DI bundle

| Field | Rust type | Notes |
|---|---|---|
| `workflow_store` | `Arc<dyn WorkflowStore>` | eos-state trait, shared immutable handle (`own-arc-shared`) |
| `iteration_store` | `Arc<dyn IterationStore>` | |
| `attempt_store` | `Arc<dyn AttemptStore>` | |
| `task_store` | `Arc<dyn TaskStore>` | typed `Task` rows |
| `orchestrator_registry` | `Arc<AttemptOrchestratorRegistry>` | liveness map |
| `iteration_coordinators` | `Option<Arc<OpenIterationCoordinatorRegistry>>` | |
| `lifecycle_config` | `WorkflowLifecycleConfig` | `default_attempt_budget = 2` |
| `composer` | `Option<Arc<AgentEntryComposer>>` | |
| `audit_sink` | `Arc<dyn AuditSink>` | eos-audit; default noop |
| `runner` | `Arc<dyn AgentRunner>` | **Rust-only DI addition** (promotes the Python `EphemeralAttemptAgentLauncher._runner` param); the injected `AgentRunner` trait seam (§7, anchor §6a), wired by `eos-runtime`, no eos-engine edge |

Representative snippet (the run-stage scheduler core — §7 single-writer loop, store as truth):

```rust
/// Drive one attempt's RUN stage to quiescence. Single writer; reads plan
/// state from the store every tick so no in-memory DAG can diverge (anchor §7).
async fn advance_run_stage(&self, cancel: &CancellationToken) -> Result<(), WorkflowError> {
    let mut set: JoinSet<TerminalReport> = JoinSet::new();          // async-joinset-structured
    loop {
        let tasks = self.plan_task_records().await?;                 // typed Vec<Task> from store
        for task_id in ready_pending_plan_ids(&tasks)? {            // re-derived each tick
            self.mark_running_and_audit(&task_id).await?;
            let launch = self.build_launch(&task_id, &tasks).await?;
            let run = self.deps.runner.run(launch.clone());         // injected trait; no engine edge
            set.spawn(async move { run.await.into_report(launch) });
        }
        let status = dag_status(&tasks)?;
        if status.all_quiescent {
            return self.close_on_quiescence(status).await;          // reducer is exit gate
        }
        tokio::select! {
            _ = cancel.cancelled() => { set.abort_all(); return Ok(()); } // async-cancellation-token
            Some(joined) = set.join_next() => self.apply_report(joined?).await?,
        }
    }
}
```

`AgentRunner` is a named `#[async_trait]` trait (the seam recorded on the anchor §6 map, §6a),
not a type-erased `Arc<dyn Fn -> BoxFuture>` (`anti-type-erasure`):

```rust
#[async_trait]
pub trait AgentRunner: Send + Sync {
    async fn run(&self, launch: AgentLaunch) -> Result<AgentRunReport, WorkflowError>;
}
```

The concrete implementor (an adapter calling `run_ephemeral_agent`) is wired by `eos-runtime`;
tests inject a double (AC-eos-workflow-08). `AgentRunReport` carries the terminal submission (or
its absence → the scheduler synthesizes the matching failure, §8.4).

## 7. Concurrency & State Ownership

- **Runtime:** single Tokio multi-thread runtime created in `eos-runtime`; this crate never
  builds a runtime. All entry points are `&self` async methods (`async-tokio-runtime`).
- **Shared immutable state** (`AttemptDeps`, stores, composer, registries, agent registry):
  `Arc<T>` / `Arc<dyn Trait>`, cloned cheaply (`own-arc-shared`).
- **Run-stage scheduler:** `tokio::task::JoinSet` over the spawned agent-run futures
  (`async-joinset-structured`); `JoinSet::abort_all()` on drop/cancel. `CancellationToken`
  (child token per workflow) for parent-exit / `cancel_workflow` graceful stop
  (`async-cancellation-token`). **The store is the single source of truth**: readiness and
  quiescence are re-derived from `task_store` each tick — there is no in-memory DAG mutation
  that could diverge from persistence (anchor §7 last bullet).
- **Single-writer rule:** exactly one scheduler task advances a given attempt; terminal
  reports are applied by that task only. This replaces Python's re-entrant
  `advance_ready_tasks` recursion and the tool→orchestrator callback. No lock is held across
  `.await` (`async-no-lock-await`, `anti-lock-across-await`): the scheduler owns its plan-task
  vector by value and re-reads it from the store, so there is no shared mutable in-memory DAG
  to lock.
- **Registries:** `AttemptOrchestratorRegistry` and `OpenIterationCoordinatorRegistry` are
  small `HashMap`s mutated only by synchronous insert/remove (never across an `.await`). They
  sit behind a **`parking_lot::Mutex`** (anchor §7): the critical section never awaits, so the
  async `tokio::sync::Mutex` would add scheduler overhead for nothing; `parking_lot`'s `!Send`
  guard makes a hold-across-await a **compile error** in the spawned scheduler tasks (and
  enables clippy `await_holding_lock`), and it does not poison under `panic=unwind`. If read
  contention ever shows up, switch to `parking_lot::RwLock` (`own-rwlock-readers`) — not before
  (`anti-premature-optimize`).
- **Workflow↔engine seam:** the injected `AgentRunner` trait (`Arc<dyn AgentRunner>`) is the
  only crossing into engine territory; `AgentRunner::run` returns a `'static + Send` future so the
  `JoinSet` can own it. No compile-time dependency on `eos-engine` (GC-eos-workflow-03).
- **CPU-bound work:** none in this crate (context XML rendering is small string work, kept on
  the async path); no `spawn_blocking`.

## 8. Behavior & Invariants

State machine and ordering semantics preserved from the Python source (plan §9):

1. **Starter leaves the parent running (anchor §3, GC-eos-workflow-02).** `WorkflowStarter::start`
   trims/validates a nonblank prompt, asserts the parent `Task` is `running` and has no open
   delegated child workflow (`list_for_parent_task` filtered by `is_open`), reads its
   `request_id`/`attempt_id`, then creates Workflow → first Iteration (sequence 1, INITIAL
   reason, `iteration_goal = workflow_goal`) → first Attempt. It **never writes the parent task**.
   On a failure after partial creation, the compensation saga rolls back attempt → iteration →
   workflow (close FAILED/CANCELLED) and deregisters the coordinator (starter.py:135-181).
2. **Attempt stages PLAN → RUN → CLOSED; reducer is the exit gate.** `start()` registers the
   orchestrator, launches the planner task, advances. `apply_plan_submission` validates the
   submission belongs to this attempt and planner, enforces the full/partial plan rule
   (`completes` must not set `deferred_goal_for_next_iteration`; `defers` must), persists the
   plan (generator + reducer task ids, deferred goal), sets stage RUN, and advances. The RUN
   scheduler runs the generator∪reducer set to **quiescence**; `all_done` → close PASSED;
   `any_failed_or_blocked` (with no remaining ready/runnable work) → close FAILED `TASK_FAILED`.
   A passing attempt closes the iteration immediately (orchestrator.py:106-160, run_stage.py).
3. **Plan reaching RUN is already structurally valid (precondition).** The structural
   invariants — no duplicate local ids, known `needs`, ≥1 reducer, lane shape (no generator
   needs a reducer; every reducer has ≥1 need and no reducer dependency, so its needs are
   generators by construction; no dangling generator),
   acyclic — are enforced at the **eos-tools planner-submission boundary** by `ordered_plan_tasks`
   (`api-parse-dont-validate`; rejecting a bad plan in-tool lets the planner retry within its
   run instead of burning an attempt). eos-workflow's run-stage still **validates the persisted
   plan**: `_validate_persisted_needs` (unknown persisted `needs` → `WorkflowError`) and
   `_unreachable_pending_ids` (persisted dependency cycle → `WorkflowError`; pending tasks whose
   chain hit FAILED/BLOCKED are quiescent-but-not-done). This is the workflow-side half of
   "preserve planner DAG invariants" (GC-eos-workflow-06, GC-eos-workflow-04).
4. **Liveness: a dead agent never hangs the DAG.** When an agent-run future resolves **without**
   a terminal submission (crash / `None` / ended), the matching failed submission is synthesized
   (`PlannerFailureSubmission` with `run_exhausted`, or a failed `Generator`/`Reducer` submission)
   so the scheduler always advances. In Python this lived in the launcher's `_report_exhaustion`,
   with a `_fail_unowned_attempt` fallback when the orchestrator was missing (launch.py:284-331).
   In the Rust model the spawned future resolves to an `AgentRunReport` and the **single-writer
   scheduler** maps a no-terminal report to the synthesized failure before advancing; the
   single-writer rule makes the "unowned attempt" case impossible, so the `_fail_unowned_attempt`
   fallback is dropped (GC-eos-workflow-03).
5. **Submission path is store-mediated, not a back-edge (GC-eos-workflow-03 — biggest departure
   from Python).** Python's terminal tools call back into the orchestrator via
   `orchestrator_registry` + `ExecutionMetadata.attempt_runtime` (an eos-tools→eos-workflow
   edge the DAG forbids). In Rust the spawned agent-run future **resolves to the agent's terminal
   result**, and the single-writer scheduler applies it: write task status + outcomes via the
   store traits, then re-derive readiness/quiescence. The registry becomes an internal liveness
   detail (`pub(crate)`), not a public seam. How a terminal-submission result is surfaced from a
   run is owned by impl-eos-tools.md / impl-eos-engine.md — referenced, not redefined here.
6. **Iteration retry budget + deferred-goal continuation.** `IterationAttemptCoordinator` is the
   sole creator of attempts in its iteration: first attempt is sequence 1 (rejected if any
   attempt exists); a retry requires `previous_attempt_id == latest_attempt_id` and remaining
   budget. On a passing attempt it writes the iteration's canonical outcomes + deferred goal and
   signals close-succeeded; on failure with budget it retries, else closes the iteration failed
   (coordinator.rs from attempt_coordinator.py). `WorkflowLifecycle.handle_iteration_closed`:
   succeeded + deferred_goal → create + start next iteration (DEFERRED_GOAL_CONTINUATION,
   sequence+1); otherwise close the workflow SUCCEEDED/FAILED.
7. **Close never mutates the parent task (GC-eos-workflow-01).** `WorkflowLifecycle.close_workflow`
   persists the final projection (`workflow_outcomes`) on the Workflow row and sets status; the
   parent `Task` is untouched and owns its own terminal submission (lifecycle.py:149-166).
8. **Context engine builds, never decides (anchor §3).** `ContextEngine::build(recipe_id, scope)`
   validates `recipe_id == scope.role ∈ {planner, generator, reducer}` then assembles an
   `AgentContext` from store reads only: planner gets workflow/iteration goals + prior-iteration
   and previous-attempt outcome history; generator/reducer get dependency outcomes + the assigned
   task instruction. A missing dependency outcome on a non-done dependency is a hard
   `WorkflowError` (engine.py:250-281). No token budgeting, terminal routing, or recipe registry
   (engine.py docstring).

**Single error type:** `WorkflowError` (`thiserror`) replaces `WorkflowInvariantViolation`;
variants cover invariant breaches, not-found entities, recipe/scope errors, and persisted-plan
errors, with `#[from] CoreError` for store-trait failures (`err-from-impl`, `err-thiserror-lib`).
Messages lowercase, no trailing punctuation (`err-lowercase-msg`). No `.unwrap()` in non-test
code (`err-no-unwrap-prod`).

## 9. SOLID & Principles Applied

- **SRP:** crate boundary = delegated-workflow lifecycle. The `ContextEngine` builds packets and
  does **not** own lifecycle policy (anchor §1/§3); lifecycle policy lives in `WorkflowLifecycle`/
  `IterationAttemptCoordinator`/`AttemptOrchestrator`. The structural planner validator is *not*
  here (§5, GC-eos-workflow-04).
- **DIP:** depends on `eos-state` `Store` *traits* (not `eos-db`), `eos-audit` `AuditSink`,
  `eos-agent-def` `AgentRegistry`, and the injected `AgentRunner` trait — all wired by `eos-runtime`.
  This is how the crate reaches the agent loop **without** an `eos-engine` edge (anchor §5,
  GC-eos-workflow-03). `AgentRunner` is the one extensibility seam this crate adds; it is recorded
  on the anchor §6 seam map as a deliberate addition (anchor §6a, overview dependency-topology
  note) — the Python `AttemptAgentRunner` DI param promoted to a named trait rather than
  introduced silently or as a type-erased `Fn` alias.
- **OCP:** new agent roles/profiles extend through the `AgentRegistry` + `context_recipe`
  frontmatter (eos-agent-def), not by editing a dispatch `match`. Context builders are a fixed
  3-way `match` on `ContextRole` (planner/generator/reducer) — total and intentionally closed
  (`anti-over-abstraction`; no generic recipe registry, matching the Python non-goal).
- **ISP:** consumes the narrow per-entity `Store` traits; the `RegisteredAttemptOrchestrator`
  slice (renamed internal) exposes only the methods collaborators need.
- **LSP:** `AttemptStatus`/`AttemptStage`/`ContextRole` are exhaustive enums → exhaustive
  matches; in-memory test stores substitute for `eos-db` repos behind the `Store` traits
  (`test-mock-traits`).
- **KISS/YAGNI/DRY:** no global orchestrator, no synthetic workflow, no recipe registry, no
  speculative config beyond `WorkflowLifecycleConfig.default_attempt_budget`. The scheduler is a
  single loop, not an actor framework. **Non-goals respected:** per-attempt orchestration only;
  root request stays a root `Task`; parent not mutated at close (anchor §2).

## 10. Gap Closeouts (tracked requirements)

- **GC-eos-workflow-01 — Do not mutate parent task at workflow close.** `WorkflowLifecycle::close_workflow`
  writes only the Workflow row (status + outcomes projection); no `TaskStore` write touches the
  parent. Proven by AC-eos-workflow-05.
- **GC-eos-workflow-02 — Starter leaves the parent task running.** `WorkflowStarter::start` reads
  the parent's status/request_id but issues no parent-task mutation; only workflow/iteration/
  attempt rows are written. Proven by AC-eos-workflow-01.
- **GC-eos-workflow-03 — Keep per-attempt orchestration; no global orchestrator, no
  eos-workflow→eos-engine edge.** Orchestration is one `AttemptOrchestrator` per Attempt in a
  `pub(crate)` liveness registry. The agent-run call is the injected `AgentRunner` and the
  terminal result is store-mediated (returned by the spawned future), eliminating the Python
  tool→orchestrator back-edge. Proven by AC-eos-workflow-06, AC-eos-workflow-07.
- **GC-eos-workflow-04 — `plan_dag.py` split by call site (anchor §5a precedent).** Usage analysis
  (`grep`: `tools/submission/planner/_schemas.py:200` is the sole caller of `ordered_plan_tasks`;
  `run_stage.py` imports only `dag_status`/`ready_pending_plan_ids`) shows the literal placement
  under eos-workflow would force a forbidden eos-tools→eos-workflow cycle. Resolution: structural
  validator `ordered_plan_tasks` + lane/acyclic helpers → **eos-tools** (referenced); `DagStatus`
  + persisted-needs/reachability/quiescence → **eos-workflow** (owned). Recorded in overview.md's
  deliberate-refinement section. Proven by AC-eos-workflow-04 (workflow half) + the eos-tools
  `planner_validation` ports (structural half).
- **GC-eos-workflow-05 — Typed tasks, not `dict[str,Any]`.** `TaskRow = dict[str,Any]` is dropped;
  `ready_pending_plan_ids`/`dag_status`/context builders take/return typed `eos_state::Task`
  (`type-no-stringly`). Proven by AC-eos-workflow-04.
- **GC-eos-workflow-06 — Preserve planner DAG invariants (first-class persisted state).** Workflow
  lifecycle changes happen only through `Store` updates and terminal submissions; the run-stage
  re-validates the persisted plan (unknown needs, persisted cycle, unreachable-pending) on every
  tick. Proven by AC-eos-workflow-04, AC-eos-workflow-08.

## 11. Acceptance Criteria

TDD: write each test first, confirm it fails for the right reason, then implement. Maps to the
plan "Tests to Port First" row for eos-workflow (anchor §11): workflow DAG / orchestrator /
context tests under `backend/tests`; planner-DAG invariants; reducer exit gate.

- **AC-eos-workflow-01** — `WorkflowStarter::start` on a running parent creates workflow + iteration
  (seq 1, goal = prompt) + first attempt and returns the matching `StartedWorkflow`; the parent
  task row is byte-identical before/after (GC-02). *Test:* `starter::tests::start_leaves_parent_running`
  (ports `test_lifecycle/test_*` starter behavior).
- **AC-eos-workflow-02** — `start` rejects a blank prompt, a non-running parent, a parent with an
  open delegated child, and a parent with no request id, each as a distinct `WorkflowError`.
  *Test:* `starter::tests::start_rejects_invalid_parent`.
- **AC-eos-workflow-03** — a failure during first-attempt creation runs the compensation saga
  (attempt FAILED `STARTUP_FAILED`, iteration + workflow CANCELLED, coordinator deregistered).
  *Test:* `starter::tests::compensation_rolls_back` (ports `test_saga_inline_equivalence.py`).
- **AC-eos-workflow-04** — `dag_status`/`ready_pending_plan_ids` over a typed `&[Task]`: serial,
  fan-out/fan-in, and mixed DAGs yield correct ready sets and quiescence; an unknown persisted
  `need` and a persisted cycle each raise `WorkflowError` (GC-04/05/06). *Test:*
  `plan_dag::tests::dag_status_*` (ports `dependency_dag_serial`/`dependency_dag_mixed` scenarios).
- **AC-eos-workflow-05** — `WorkflowLifecycle::close_workflow` sets the Workflow status + outcomes
  and performs zero `TaskStore` writes (GC-01). *Test:* `lifecycle::tests::close_does_not_touch_parent`.
- **AC-eos-workflow-06 (reducer exit gate)** — when all generators are DONE and all reducers DONE,
  the attempt closes PASSED and the iteration closes SUCCEEDED; with a FAILED reducer the attempt
  closes FAILED `TASK_FAILED`. *Test:* `orchestrator::tests::reducer_is_exit_gate`.
- **AC-eos-workflow-07 (liveness)** — an agent-run future resolving without a terminal submission
  is mapped to the synthesized failed submission and the scheduler advances to a terminal attempt
  state; no hang. *Test:* `run_stage::tests::dead_agent_synthesizes_failure` (ports
  `test_launcher_exhaustion_parametrized.py`).
- **AC-eos-workflow-08 (no eos-engine edge)** — the crate compiles with `eos-engine` absent from
  `[dependencies]`; the runner is exercised via an injected test double. *Test:* `cargo` build +
  `run_stage::tests::injected_runner_double`.
- **AC-eos-workflow-09 (context builder)** — planner/generator/reducer `build` produces the
  expected `AgentContext` section tree from store state (insertion-ordered `Vec<(String, String)>` attrs matching the Python dict), and a
  recipe≠role mismatch raises `WorkflowError`. *Test:* `context::tests::build_*` + a Phase-0
  golden snapshot (ports `test_context_engine/test_agent_context.py`).
- **AC-eos-workflow-10 (iteration budget + continuation)** — failed attempt with budget retries;
  budget-exhausted closes the iteration failed; passing attempt with a deferred goal starts the
  next iteration (seq+1, DEFERRED_GOAL_CONTINUATION). *Test:* `iteration::tests::retry_and_continue`
  (ports `test_iteration_attempt_coordinator.py`).

## 12. Implementation Checklist

1. `error.rs`: `WorkflowError` enum + `Result` alias → AC-04 compiles. *(verify: `cargo check`)*
2. `ids.rs`: task-id builders + `WorkflowLifecycleConfig`. *(verify: unit round-trip)*
3. `context/section.rs` + `context/scope.rs`: `ContextSection`, `AgentContext`, `ContextScope`. *(verify: serde + schema snapshot)*
4. `context/engine.rs` + `xml.rs`: role builders + recipe validation → AC-09.
5. `context/composer.rs`: `AgentEntryComposer` over `AgentRegistry` → composer test.
6. `attempt/plan_dag.rs`: `DagStatus` + persisted-needs/reachability/quiescence (owned half) → AC-04.
7. `attempt/launch.rs`: `AgentLaunch`, `AttemptDeps`, the `AgentRunner` trait, `AgentLaunchFactory` → AC-08.
8. `attempt/orchestrator_registry.rs` (`pub(crate)`) + `attempt/orchestrator.rs` state machine → AC-06.
9. `attempt/run_stage.rs`: single-writer `JoinSet` scheduler with `CancellationToken`, including
   the no-terminal-report → synthesized-failure mapping (§8.4) → AC-06, AC-07.
10. `iteration/coordinator.rs`: retry-budget loop + close routing → AC-10.
11. `lifecycle.rs`: `WorkflowLifecycle` create/close/continuation → AC-05, AC-10.
12. `starter.rs`: `WorkflowStarter` + compensation saga → AC-01, AC-02, AC-03.
13. `lib.rs` re-exports; wire `cargo clippy -D warnings` + `fmt --check` (anchor §14).
14. Update overview.md: eos-workflow row → IN REVIEW; add the GC-04 split note to the
    deliberate-refinement section (`ordered_plan_tasks` → eos-tools).

---
**On completion:** update the Progress Tracker in `./overview.md` for row `eos-workflow` per
spec-conventions.md §13. Do not edit other crates' rows.
