# EOS Agent Core Rust to TypeScript Migration - Phase 05 Workflow Orchestration

Status: Proposed
Date: 2026-06-11
Owner: eos-agent-core
Migration direction: Rust -> TypeScript
Project path: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`
Rust source boundary: `agent-core/crates/eos-workflow` (workflow/iteration/attempt
runs, planner/worker launch, XML context rendering, active-attempt registries),
`agent-core/crates/eos-tool/src/tools/workflow.rs` +
`tools/submission/submit_plan_outcome.rs` + `tools/submission/submit_worker_outcome.rs`,
`agent-core/crates/eos-types/src/state/workflow` + `contracts/workflow.rs`,
`agent-core/crates/eos-db` (workflow/iteration/attempt repositories, migration 0001)
Depends on: Phase 04.5 (`@eos/agent-runtime`), Phase 04 (`@eos/tool`, engine
inbox/supervisor), Phase 03 (`@eos/engine`), Phase 02 (`@eos/contracts`)
Companion spec: `docs/plans/workflow_context_projection_SPEC.md` (the entity,
rendering, and lifecycle contract this phase implements; amendments in §3)
Knowledge inputs: `knowledge/background-task-tracking.md`,
`knowledge/background-task-spawn-and-cancellation.md`,
`knowledge/agent-run-concurrency.md`

## 1. Intent

Phase 05 lands the workflow family reserved by Phase 04 decision 21: a new
`@eos/workflow` package (entities, markdown context projection, lifecycle
orchestration, launch scheduling), the first real content in `@eos/db`
(workflow tables behind `better-sqlite3` + `Kysely`), the workflow tool
family in `@eos/tool` (`delegate_workflow`, `query_workflow`), per-kind
payload schemas on the planner/worker submission tools (the seam reserved in
`tools/submission/index.ts`), and the `@eos/agent-runtime` wiring that makes
a delegated workflow one background session of the delegating run.

The model is the companion spec's aggregate:

```text
delegate_workflow
  -> Workflow -> Iteration[] -> Attempt[] -> Plan + WorkItem[]
```

The database owns workflow state. Context artifacts (`spec.md` / `brief.md`
per entity) are deterministic projections rendered from a fresh aggregate -
this phase renders them virtually (at agent launch and inside
`query_workflow`), with the physical file projector left as a named seam
(§2.2). Planner and worker agent runs are launched automatically by the
scheduler; their terminal submissions ride the run outcome back to the
scheduler, which mutates the aggregate, re-renders context, and launches
whatever became ready. No user-facing `launch_agent` step exists.

This phase is additive plus the owned `@eos/tool` changes (§9): the workflow
family folder, per-kind submission input schemas, and the
`cancel_background_session` type refinement gaining `"workflow"`. There are
no `@eos/engine` changes. The Rust implementation remains live; nothing
under `agent-core/` changes.

## 2. Design Decisions

Deliberate choices, recorded so later phases do not mistake them for
omissions:

1. **The companion spec is the rendering and lifecycle contract.** Entity
   shapes, status-gated brief/spec composition rules, folder layout, and
   the orchestration flows come from
   `workflow_context_projection_SPEC.md`. Where this phase diverges, the
   divergence is an explicit amendment in §3 - never a silent drift. The
   companion's acceptance criteria become this phase's renderer and
   lifecycle test tables (§14).
2. **Projection is virtual first; the physical writer is a seam.** The
   companion's invariant 3 (rendering never reads projected files) makes
   the files a pure cache: nothing consumes them except agents, and agents
   receive context either injected at launch or through `query_workflow`.
   Both render from a fresh DB-loaded aggregate on demand, so this phase
   writes no `spec.md`/`brief.md` files - eliminating write amplification,
   stale-file races, and atomic-rename concerns. The companion §5 folder
   layout survives as the *addressing scheme*: `query_workflow` resolves
   `workflow_<id>/iteration_<id>/.../brief.md` paths against the aggregate.
   A physical `WorkflowContextProjector` (for sandboxed workers that read
   real files, or debugging) is deferred (§11) and reuses the same
   renderers unchanged.
3. **One status enum, five states.** All five entities share
   `WorkflowEntityRunStatus = NotStarted | Running | Success | Failed |
   Cancelled`. `Cancelled` is added beyond the companion's four states
   because the runtime cancels: `cancel_background_session`, the caller's
   disposal cascade (Phase 04.5 §8), and workflow teardown all need a
   terminal state that is neither success nor failure. `Cancelled` renders
   like `Failed` (status line, then terminal reference). The Rust
   `Open/Succeeded/Passed/Cancelled` per-entity enums are not ported.
4. **Entity IDs are globally unique, branded, and minted.**
   `WorkflowId`, `IterationId`, `AttemptId`, `PlanId`, `WorkItemId` follow
   the Phase 02 `ids.ts` pattern (`brand` + `mint*`/`*From`). The
   companion's invariant 20 (scheduler keyed by `folder_path` because
   local attempt IDs repeat across iterations) is replaced: unique IDs make
   every entity ID a sufficient locator, and folder paths stay
   presentation-only.
5. **Plans and work items are rows, not an execution-tree JSON.** The Rust
   `attempts.execution_tree` JSON column is not ported. Per-entity
   rendering, denormalized back-references, and ready-work-item queries
   all address plans and work items individually, so they get their own
   tables (§6) - matching the companion's entity model exactly.
6. **Work items carry `agent_name`; plans and work items record
   `agent_run_id`.** The scheduler launches by profile name (the Phase
   04.5 runtime vocabulary), so the planner's payload names a worker
   profile per work item - this restores the Rust `WorkItemSpec.agent_name`
   the companion dropped. When the scheduler claims an entity it stamps
   the minted `agent_run_id` onto the row: the run-to-entity binding is
   audit data and the join point for transcripts, never resolved through
   an in-process registry.
7. **Submissions ride the run outcome; the scheduler is their only
   consumer.** Phase 04 decision 8 already routes a terminal tool's
   `content` into `outcome.submission`. The workflow scheduler launched
   the run, so it already holds `handle.outcome`; when the promise
   settles it reads the submission and drives the orchestrator. The
   submission tools never see a workflow port (they stay service-free, as
   shipped), and the Rust `ActiveAttemptRuns` registry plus
   `AttemptSubmissionAdapter` resolution layer are deleted, not ported.
   The same settlement uniformly covers death: a run that settles
   `completed` without a submission, `failed`, or `cancelled` is
   synthesized as a failed submission - no plan or work item can wedge in
   `Running` because its agent died without calling the terminal tool.
8. **Launch happens after commit, from claimed queue rows.** Inside the
   mutation transaction the scheduler claims launchable entities (plan or
   ready work item: status -> `Running`, launch-queue row -> `claimed`);
   the actual `AgentLaunchPort.launch` calls happen strictly after commit.
   An agent can never observe or submit against uncommitted state. The
   queue is a DB table for determinism and inspectability; replay-on-boot
   recovery is deferred (§11).
9. **One serial reconcile queue per workflow.** Concurrent worker
   settlements must not interleave their
   transaction -> reconcile -> launch pipelines. Every settlement
   continuation and every cancel request enqueues onto a per-workflow
   promise chain with a single logical consumer. There is no engine
   mailbox for this and none is wanted: `handle.outcome` is the only
   completion surface (Phase 04.5 decision 5), and the per-run
   `NotificationInbox` belongs to an agent's conversation, which the
   scheduler is not.
10. **A delegated workflow is one background session of the delegating
    run.** `delegate_workflow` registers
    `{ type: "workflow", id: workflowId }` with the caller's supervisor
    and returns the id immediately (Phase 04 decision 2). The
    `SessionHandle` maps the workflow terminal onto `SessionOutcome`
    (`Success -> completed`, `Failed -> failed`,
    `Cancelled -> cancelled`); `cancel` is the workflow API. The
    workflow's internal planner/worker runs are *not* sessions of the
    caller - the caller sees one session, one `session_settled`
    notification, and pulls detail through `query_workflow` (Phase 04
    decision 14). Auto-wait, the `openCount()` submission guard, and
    `list_background_sessions` all apply to workflows with zero engine
    change.
11. **Workflows die with the delegating run.** Child runs are launched
    with no caller signal (fresh abort root, the Phase 04.5 subagent
    rule); cancellation reaches them only through the workflow's own
    cancel cascade: interrupt every live child run, await their outcomes,
    mark all non-terminal entities `Cancelled` in one transaction, resolve
    the terminal. `supervisor.dispose` on caller finish therefore tears
    the whole workflow down depth-first through one `SessionHandle`.
    Detached workflows that outlive their caller are a different ownership
    model, deferred with restart recovery (§11).
12. **At most one open workflow per run.** The guard Phase 04 decision 5
    pre-committed lives inside `delegate_workflow` as plain code: if the
    caller's supervisor already lists a `workflow`-type session (running
    or undelivered), the call returns an error result. Sub-delegation by
    workers composes naturally: a worker that delegates cannot submit
    until its own workflow session settles and is delivered
    (`openCount()` guard, already shipped).
13. **Renderers are depth-parametric pure functions; two combinators own
    the cross-cutting rules.** Each entity gets `renderSpec` /
    `renderBrief` as plain functions over the frozen aggregate, receiving
    a heading depth and emitting correct headings directly - the
    companion §8 `nest()` / `shift_headings_down_by_one` string rewriting
    is not implemented (it breaks on `#` inside code fences and compounds
    across levels). The three repeated behaviors - status line,
    `NotStarted` short-circuit, terminal `Reference:` line - are written
    once as `brief(base, body)` / `spec(base, body)` combinators that all
    ten renderers compose. Renderers never read files, never mutate, and
    never see the store.
14. **Goals travel in the launch directive, not the briefs.** The
    companion keeps `workflow_goal`/`iteration_goal` out of the brief
    rollups (its invariants 10-11) while also feeding the planner only
    briefs (its §11) - composed, the planner would receive no goal text.
    Resolution: launch context is ordered user messages (Phase 04.5
    decision 9) - rendered briefs as evidence first, then an instruction
    message carrying the iteration goal (planner) or the work-item spec
    plus dependency briefs (worker). Briefs stay goal-free as specified.
15. **Orchestrators are one module of functions, not four classes.** The
    companion's `WorkflowOrchestrator -> IterationOrchestrator ->
    AttemptOrchestrator` nesting is Rust structure, not behavior. The
    top-down dispatch and upward reconcile survive as the call structure
    of plain functions over `(trx, aggregate)` (`delegateWorkflow`,
    `materializeWorkItems`, `recordWorkerOutcome`, `reconcileAttempt`).
    Transitions are idempotent by guard: mutating a terminal entity is a
    no-op, which is what makes late settlements after a cancel harmless.
    The scheduler cell (§8) is the only stateful object in the package.
16. **The launch boundary is a narrow owned port.** `@eos/workflow`
    declares `AgentLaunchPort` - `launch(agentName, initialMessages) ->
    { runId, outcome, interrupt }` with a settlement DTO it owns - and
    never imports `@eos/engine` or `@eos/agent-runtime`. The runtime
    implements the port as a bound `startRun` adapter. Workflow package
    tests script the port directly: the whole lifecycle suite runs
    without an engine.
17. **`query_workflow` is the read tool.** Phase 04 decision 14's
    symmetry (spawn / read / shared cancel) lands as
    `query_workflow(workflow_id, path?)`: render the addressed projection
    from a fresh aggregate (default `brief.md` at the workflow root).
    Notifications stay `{ ref, status, summary }`; full state never sits
    in conversation history. Search (`search_workflow_context`) is
    deferred (§11).
18. **Tools receive narrow bound functions, not the service.** Mirroring
    Phase 04.5 §5, `workflowTools` takes
    `{ delegate, cancel, query }` bound functions plus the per-run
    supervisor. No `WorkflowService` object crosses into `@eos/tool`, and
    the dependency graph stays acyclic with the runtime as the only
    package that holds both sides.
19. **Every projection is a revision-stamped snapshot.** `workflows`
    carries a monotonic `revision`, incremented in every mutation
    transaction. Every rendered projection - launch evidence and
    `query_workflow` output alike - opens with a stamp line
    (`Context: workflow <id> @ revision <n>`), so an agent holding
    launch-time context can tell it is reading a snapshot and compare it
    against a live `query_workflow` read. Staleness stays possible (the
    dependency semantics make it safe - `needs` are terminal before
    launch); ambiguity about which view the model holds does not. The
    stamp is an aggregate field, so renderers stay pure.
20. **Only the current iteration renders rollups in the workflow brief.**
    Retries multiply content: failed attempts x `max_attempts` x
    iterations would otherwise all roll up into every planner launch.
    Prior (terminal) iterations therefore collapse to their status line
    plus terminal reference; only the current iteration renders in full -
    which is exactly the working set: a retry planner needs the failed
    attempts of its own iteration (kept), and a deferred-goal planner
    needs to know prior iterations closed, not how. Prior-iteration
    detail stays one `query_workflow` read away. This amends the
    companion's content-plus-reference rule at those positions (§3).

## 3. Companion Spec Amendments

Recorded deltas against `workflow_context_projection_SPEC.md`; everything
not listed here is implemented as written there.

| Companion item | Amendment | Decision |
| --- | --- | --- |
| §3 four-state status enum | `Cancelled` added; renders like `Failed` | §2.3 |
| Invariant 20: scheduler keyed by `attempt.folder_path` | replaced by globally unique minted entity IDs; folder paths are presentation-only | §2.4 |
| §4 `WorkItem` schema | gains `agent_name` (Rust parity); `Plan` and `WorkItem` gain `agent_run_id` stamped at claim | §2.6 |
| §9.1 pseudocode dispatches the planner inside the transaction | claim in-transaction, launch strictly post-commit from queue rows | §2.8 |
| §9 covers only worker failure; no path for a planner failure or an agent that never submits | one uniform rule: any settlement without a valid submission is a synthesized failed submission through the same reconcile path | §2.7 |
| §8 `nest()` heading shifting | depth-parametric renderers; no markdown rewriting | §2.13 |
| §11 planner context (briefs only) vs invariants 10-11 (briefs are goal-free) | goals ride the launch directive message; briefs stay goal-free | §2.14 |
| §10 physical projector re-rendering the whole tree after every mutation | virtual projection this phase; physical projector + dirty-ancestor-path rendering deferred | §2.2, §11 |
| §4 `Iteration.max_try` default 3 | `max_attempts` default 2 (Rust `AttemptBudget` parity); `delegate_workflow` may override | §6 |
| §6 per-entity `render_spec()/render_brief()` methods | same contract as pure functions + combinators | §2.13, §2.15 |
| §11 `read_workflow_context` / `search_workflow_context` | `query_workflow` (decision-14 naming); search deferred | §2.17, §11 |
| §8 terminal briefs always render local content before their reference | prior (non-current) iterations inside `workflow/brief.md` collapse to status + reference only; their own files still render in full | §2.20 |
| (no stamping) | every rendered projection opens with a workflow revision stamp | §2.19 |

## 4. Scope

In scope:

- `@eos/contracts` additions: workflow entity IDs, `WorkflowEntityRunStatus`,
  planner/worker submission payload schemas,
- `@eos/db` first real content: `better-sqlite3` + `Kysely` database
  factory, the workflow schema migration, `WorkflowStore` (transactional
  mutations + aggregate loader + launch queue),
- `@eos/workflow` package: aggregate types, markdown renderer combinators
  and per-entity renderers, launch-context policy, lifecycle orchestration
  functions, the per-workflow scheduler cell, `WorkflowService`
  (`delegate` / `cancel` / `query`), `AgentLaunchPort`,
- `@eos/tool` owned changes: the workflow family
  (`tools/workflow/`: `delegate_workflow`, `query_workflow`), per-kind
  input schemas for `submit_planner_outcome` / `submit_worker_outcome`
  (the reserved seam), `cancel_background_session` type union gaining
  `"workflow"`,
- `@eos/agent-runtime` wiring: optional workflow store dependency, the
  launch-port adapter over its own `startRun`, per-run `workflowTools`
  assembly, profile validation covering the workflow tool names,
- tests per §14.

Out of scope (named seams in §11):

- physical context-file projection and dirty-subtree rendering,
- restart recovery (durable launch-queue replay, terminal re-attachment),
- detached workflows that outlive the delegating run,
- workflow context search, progress notifications, depth budgets,
- sandbox-family tools for workers (workers in this phase's suite are
  scripted; real worker toolsets arrive with the sandbox family),
- any edit under `agent-core/`, any `@eos/engine` change.

## 5. Rust Surface and TypeScript Target

| Rust source | TypeScript target | Carries |
| --- | --- | --- |
| `eos-workflow/src/workflow_run.rs`, `iteration_run.rs`, `attempt/attempt_run.rs`, `attempt/planner_run.rs`, `attempt/work_items_run.rs` | `packages/workflow/src/lifecycle.ts`, `scheduler.ts` | Redesigned: run/coordinator classes -> orchestration functions + one per-workflow cell; `advance()` -> reconcile jobs on the serial queue |
| `eos-workflow/src/context/render.rs`, `planner_context.rs`, `worker_context.rs` (XML `AgentContext`) | `packages/workflow/src/render/`, `context.ts` | Redesigned: XML sections -> companion-spec markdown brief/spec projections; launch context becomes ordered user messages |
| `eos-workflow/src/attempt/active_attempt_runs.rs`, `attempt_submission.rs` | (not ported) | Replaced by scheduler-held `handle.outcome` consumption (§2.7) |
| `eos-tool/src/tools/workflow.rs` (`delegate_workflow`) | `packages/tool/src/tools/workflow/` | `delegate_workflow` + `query_workflow` over bound functions; one-open-workflow guard (§2.12) |
| `eos-tool/src/tools/submission/submit_plan_outcome.rs`, `submit_worker_outcome.rs` | `packages/tool/src/tools/submission/` | Per-kind payload schemas fill the reserved seam; in-run Zod validation replaces `validate_plan_structure` |
| `eos-types/src/state/workflow/*`, `contracts/workflow.rs`, `state/tools/submissions.rs` | `packages/contracts/src/workflow.ts` | IDs, status enum, payload DTOs; `WorkflowApi`/`WorkflowAttemptSubmissionApi` traits collapse into the service + outcome consumption |
| `eos-db/src/repositories/{workflow,iteration,attempt}.rs`, `migrations/0001_initial.sql` (workflow tables) | `packages/db/src/` | Kysely schema; plans/work_items normalized out of `execution_tree` JSON (§2.5); launch queue table added (§2.8) |

## 6. Contracts and Store (`@eos/contracts`, `@eos/db`)

Contracts (`packages/contracts/src/workflow.ts`), following the `ids.ts`
brand/mint pattern and the snake_case rule for serialized DTOs:

```ts
const WorkflowEntityRunStatusSchema = z.enum([
  "NotStarted", "Running", "Success", "Failed", "Cancelled",
]);

// Branded IDs with mint* / *From factories:
// WorkflowId, IterationId, AttemptId, PlanId, WorkItemId

const PlannerOutcomePayloadSchema = z.object({
  plan_spec: z.string().min(1),
  summary: z.string().min(1),
  work_items: z.array(z.object({
    id: z.string().min(1),            // planner-local; service mints global
                                      // WorkItemIds and rewrites `needs`
    agent_name: z.string().min(1),    // worker profile to launch (§2.6)
    work_item_spec: z.string().min(1),
    needs: z.array(z.string()).default([]),
  })).min(1),
  deferred_goal_for_next_iteration: z.string().min(1).optional(),
});

const WorkerOutcomePayloadSchema = z.object({
  summary: z.string().min(1),
  is_pass: z.boolean(),
  outcome: z.string().min(1),
});
```

Structural payload validation happens in the submission tool (§9): unique
local ids, every `needs` entry references a declared id, no dependency
cycles. `agent_name` validity is checked at materialization (the tool is
service-free and has no profile registry); an unknown profile name fails
the attempt with a recorded `fail_reason`, and the retry planner sees that
failure in the attempt brief.

Store (`packages/db/`): a `createDatabase(path | ":memory:")` factory
(Kysely over `better-sqlite3`, both already Phase 00 baseline), one
migration, and a `WorkflowStore` owning every workflow query:

```text
workflows    id PK, parent_run_id, goal, status, revision, created_at,
             updated_at, closed_at
iterations   id PK, workflow_id, sequence, origin ('initial'|'deferred_goal'),
             goal, max_attempts, status, timestamps
attempts     id PK, workflow_id, iteration_id, sequence, status, fail_reason,
             timestamps
plans        id PK, workflow_id, iteration_id, attempt_id, agent_run_id,
             status, plan_spec, planner_summary, deferred_goal, timestamps
work_items   id PK, workflow_id, iteration_id, attempt_id, plan_id,
             agent_name, agent_run_id, status, work_item_spec, needs (JSON
             id array), worker_summary, worker_outcome, timestamps
launch_queue id PK, workflow_id, kind ('plan'|'work_item'), entity_id,
             state ('queued'|'claimed'), created_at
```

Back-references are denormalized exactly as the companion's §4 table;
`revision` increments in every mutation transaction and feeds the §2.19
projection stamp.
`WorkflowStore` exposes `transaction(fn)`, the per-entity mutations the
orchestrator needs, `claimLaunchable(trx, workflowId)` (queued rows whose
entity is launchable -> entity `Running` + row `claimed`, returned to the
caller), and `loadAggregate(workflowId)` -> one frozen
`Workflow -> Iteration[] -> Attempt[] -> Plan/WorkItem[]` value ordered by
sequence. The aggregate is the only read shape the workflow package
consumes; row types stay inside `@eos/db`.

## 7. Renderers and Context Projection (`@eos/workflow`)

`render/md.ts` is the whole template engine: an `md` tagged template
(dedent, `false`/`null`/`undefined` skipping, array joining), `h(depth)`,
and the two combinators that own the companion's cross-cutting invariants
(status line first; `NotStarted` renders only the status line; `Success`,
`Failed`, and `Cancelled` briefs append their own inline
`Reference: <folder>/spec.md`; `Running` briefs never do; references stay
where the child brief is rendered):

```ts
type Renderer = (depth: number) => string;

const brief = (base: EntityBase, body: Renderer): Renderer => (depth) =>
  base.status === "NotStarted"
    ? statusLine(base.status)
    : md`${statusLine(base.status)}
         ${body(depth)}
         ${isTerminal(base.status) && `Reference: ${base.folder_path}/spec.md`}`;

const spec = (base: EntityBase, body: Renderer): Renderer => (depth) =>
  md`${statusLine(base.status)}
     ${body(depth)}`;
```

`render/render.ts` composes the ten per-entity renderers from these,
following the companion §8 templates: parents call children with
`depth + 1`; `Plan` and `WorkItem` briefs are heading-free prose
(companion invariant 6); `attemptSpec` inlines the full plan spec plus all
work-item briefs, `attemptBrief` the plan brief plus leaf work-item briefs
only (`leafWorkItems`: items no other item `needs`); workflow and
iteration briefs render rollups without goals, and the workflow brief
collapses prior iterations to status + reference (§2.20). Every rendered
projection opens with the §2.19 revision stamp. `folder_path` values
follow the companion §5 layout and are computed from the aggregate, not
stored - folder paths are presentation-only (§2.4).

`context.ts` is the launch-context policy (§2.14), producing ordered user
messages:

```text
planner launch:
  1. workflow brief + current iteration brief        (evidence)
  2. directive: iteration goal, max_attempts; on retry, the failed
     attempt's spec path with an explicit instruction to read it via
     query_workflow before planning; "submit via submit_planner_outcome"

worker launch:
  1. current attempt brief + dependency work-item briefs   (evidence)
  2. directive: own work_item_spec, "submit via submit_worker_outcome"
```

The retry read instruction is deliberate, not advisory boilerplate: the
brief compresses a failed attempt to one-line summaries, models
under-escalate when injected context looks complete, and the failure
detail is exactly what the next plan depends on.

`query_workflow` resolves a companion-§5 relative path
(`iteration_<id>/attempt_<id>/plan_<id>/spec.md`, default root
`brief.md`) against a fresh `loadAggregate` and returns that one rendered
projection; unknown paths return an error result naming the valid children.

## 8. Lifecycle Orchestration and Scheduler (`@eos/workflow`)

`launch-port.ts` (§2.16):

```ts
interface LaunchSettlement {
  status: "completed" | "failed" | "cancelled";
  submission?: JsonValue;          // outcome.submission verbatim
}

interface LaunchedAgent {
  runId: AgentRunId;
  outcome: Promise<LaunchSettlement>;
  interrupt(reason: string): void;
}

interface AgentLaunchPort {
  launch(agentName: string,
         initialMessages: readonly Message[]): LaunchedAgent;
}
```

`scheduler.ts` keeps one cell per active workflow - the only in-process
state in the package:

```ts
interface WorkflowCell {
  liveRuns: Map<AgentRunId, LaunchedAgent>;  // cancel cascade walks this
  queue: Promise<void>;                      // serial reconcile chain (§2.9)
  terminal: PromiseWithResolvers<WorkflowTerminal>;
}

interface WorkflowTerminal {
  status: "Success" | "Failed" | "Cancelled";
  summary: string;                           // one line, from the closing
}                                            // entity's recorded summaries
```

Every mutation runs as one job on the cell's queue:

```text
reconcile job (serialized per workflow):
  store.transaction:
    apply orchestration mutation        delegate / materialize work items /
                                        record worker outcome / reconcile
                                        attempt -> iteration -> workflow
                                        (retry attempt, deferred iteration,
                                        terminal close - companion §9)
    claimed = claimLaunchable(trx)      plan or ready work items: status ->
                                        Running, queue row -> claimed (§2.8)
  commit
  render + launch each claimed entity   port.launch(agent_name, context);
                                        stamp agent_run_id; add to liveRuns;
                                        outcome.then(s => enqueue(
                                          onSettlement(entity_id, s)))
  if workflow turned terminal:          resolve cell.terminal LAST, after
    resolve + drop the cell             commit - a woken caller must read
                                        closed state
```

`onSettlement` removes the run from `liveRuns`, parses
`settlement.submission` with the entity's payload schema, and dispatches:
valid planner payload -> `materializeWorkItems`; valid worker payload ->
`recordWorkerOutcome` (`is_pass` decides success/failure); anything else -
`failed`, `cancelled`, `completed` without a submission, or a payload that
fails the parse - synthesizes a failed submission (§2.7). The orchestrator
ignores mutations against already-terminal entities (§2.15), which is the
entire cancel-race story: a natural settlement arriving after a cancel job
finds `Cancelled` rows and no-ops.

`service.ts` exposes the package API the runtime binds (§2.18):

```ts
interface WorkflowService {
  delegate(input: { goal: string; max_attempts?: number },
           parentRunId: AgentRunId): Promise<DelegatedWorkflow>;
  cancel(id: WorkflowId, reason: string): Promise<void>;
  query(id: WorkflowId, path?: string): Promise<string>;
}

interface DelegatedWorkflow {
  workflowId: WorkflowId;
  terminal: Promise<WorkflowTerminal>;   // the SessionHandle watch surface
  describe(): string;                    // goal one-liner
}
```

`delegate` creates workflow + first iteration + first attempt + first plan
and enqueues the planner launch in one transaction (companion §9.1), then
returns; the first reconcile job claims and launches the planner. `cancel`
enqueues a cancel job: interrupt every `liveRuns` entry, await their
outcomes, mark all non-terminal entities `Cancelled` in one transaction,
resolve the terminal as `Cancelled` (§2.11). `cancel` resolves only after
teardown completes - the Phase 04.5 subagent-cancel shape.

## 9. Tool Family and Submission Payloads (`@eos/tool` owned changes)

`tools/workflow/` (one folder per family, one file per tool; the factory
takes bound functions, §2.18):

```ts
function workflowTools(
  workflow: {
    delegate(input: DelegateWorkflowInput,
             parent: AgentRunId): Promise<DelegatedWorkflow>;
    cancel(id: WorkflowId, reason: string): Promise<void>;
    query(id: WorkflowId, path?: string): Promise<string>;
  },
  supervisor: BackgroundSupervisor,
): ToolDefinition[];
```

`delegate_workflow` (input `{ goal, max_attempts? }`):

```ts
execute: async (input, ctx) => {
  const open = supervisor.list().some((s) => s.type === "workflow");
  if (open) return { content: "a delegated workflow is already open …",
                     isError: true };                       // §2.12
  const wf = await workflow.delegate(input, ctx.meta.run.run_id);
  supervisor.register(
    { type: "workflow", id: wf.workflowId },
    ctx.meta.tool_use_id,
    {
      settled: wf.terminal.then((t) => ({
        status: t.status === "Success" ? "completed"
              : t.status === "Cancelled" ? "cancelled" : "failed",
        summary: t.summary,
      })),
      cancel: (reason) => workflow.cancel(wf.workflowId, reason),
      describe: () => wf.describe(),
    },
  );
  return { content: { workflow_id: wf.workflowId } };
},
```

Registration precedes the tool result, so `openCount()` covers the
workflow before the model's next token. The supervisor's existing
machinery does the rest: settlement publishes one `session_settled`
notification, auto-wait parks an idle caller, the submission guard blocks
the caller past an unseen settlement, and `dispose` on caller finish
cancels through the handle (§2.10-2.11).

`query_workflow` (input `{ workflow_id, path? }`) returns the rendered
projection (§7). `cancel_background_session`'s `type` refinement gains
`"workflow"` (the Phase 04 §2.3 narrowing, tool-side only).

`tools/submission/`: `submit_planner_outcome` and `submit_worker_outcome`
replace the shared `{ summary, payload? }` input with
`PlannerOutcomePayloadSchema` / `WorkerOutcomePayloadSchema` - filling the
"per-kind payload schemas are a later seam" comment shipped in Phase 04.
Validation is in-run: a planner that emits a malformed plan gets an error
result and can correct it before terminating. The tools remain
service-free and terminal; the open-sessions guard is unchanged - which is
what forces a sub-delegating worker to resolve its own workflow before
submitting. The other three submission tools keep the shared schema.

## 10. Runtime Wiring (`@eos/agent-runtime`)

`AgentRuntimeDependencies` gains `workflowDb?: string | Kysely<Database>`.
When present, `createAgentRuntime` builds the store and one
`WorkflowService` whose `AgentLaunchPort` is a bound adapter over the
runtime's own `startRun` - launches pass no caller signal (fresh abort
root, §2.11) and set `context.parent` to the delegating run. The service
is process-level (decision: one scheduler owns all workflows); the tool
family is assembled per run in `startRun`, after the agent-tools entry:

```ts
const availableDefinitions = [
  ...(dependencies.baseTools ?? []),
  ...agentTools(boundAgentCalls, supervisor),
  ...(workflowService
    ? workflowTools(boundWorkflowCalls(workflowService), supervisor)
    : []),
  ...backgroundTools(supervisor),
  ...terminalToolDefinitions(supervisor),
];
```

`WORKFLOW_TOOL_NAMES` joins the static name universe, so profile
validation covers `delegate_workflow`/`query_workflow` at startup; a
profile listing them in a runtime configured without `workflowDb` fails at
`createAgentRuntime`, never mid-run. Planner and worker profiles are
ordinary Phase 04.5 profiles (`agent_kind: planner|worker`,
`terminal_tool: submit_planner_outcome|submit_worker_outcome`,
`allowed_tools` typically including `query_workflow`); the scheduler
launches them by the `agent_name` recorded on the entity. Rollup quality
is profile policy, not framework: every ancestor brief is built from
planner/worker `summary` fields, so shipped profiles should keep the
Phase 04.5 worker pattern of routing `ask_advisor` at the exact terminal
payload - summary fidelity included - before submission.

Disposal needs no new wiring: the caller's engine-triggered
`supervisor.dispose` reaches the workflow `SessionHandle.cancel`, which is
`WorkflowService.cancel` - the §8 cascade. Scheduler-originated interrupts
use the fixed reason `workflow_cancelled`; child transcripts record it as
`interrupt_reason` (Phase 04.5 §8 parity).

## 11. Deferred (named seams)

| Deferred behavior | Seam left by this phase |
| --- | --- |
| Physical context files (`spec.md`/`brief.md` on disk) for sandboxed workers and debugging | renderers are pure `(aggregate, depth) -> string`; a `WorkflowContextProjector` is one walk calling them; dirty-ancestor-path rendering and content-hash skip apply there |
| Restart recovery | `launch_queue` rows and entity `agent_run_id` stamps are durable; replay-on-boot + terminal re-attachment need a runtime registry of workflow cells |
| Detached workflows outliving the caller | `DelegatedWorkflow` is the only handle shape; a runtime-owned registry (not supervisor registration) is the alternative ownership model |
| Workflow context search | `query_workflow` is the single read entry; search is a second tool over the same aggregate |
| Progress notifications (per-iteration transitions) | `inbox.publish` with `key = "workflow:<id>"` collapses stale progress by design; publishing site would be the reconcile job |
| Workflow depth budget for sub-delegation | `parent_run_id` chains exist on `workflows`; a depth count is one recursive query in `delegate` |
| Summary length caps | `max()` refinements on the payload schema `summary` fields |
| Unstructured per-entity notes (worker insight that fits no schema field; today it gets crammed into `outcome`) | one optional payload field + one column + one renderer heading - submission-gated and typed, never a writable context file |
| Worker/planner steering mid-run | `LaunchedAgent` could expose `steer`; nothing consumes it yet |

## 12. Workspace Changes

- `packages/workflow/` (new); package name `@eos/workflow`
  (`dependencies`: `@eos/contracts`, `@eos/db` via `workspace:*`). No
  dependency on `@eos/engine`, `@eos/tool`, or `@eos/agent-runtime`.
- `packages/db/`: first real content - `createDatabase`, the workflow
  migration, `WorkflowStore`. Dependencies `better-sqlite3` + `kysely`
  (Phase 00 baseline, already in the root manifest).
- `packages/contracts/`: `workflow.ts` (IDs, status, payload schemas).
- `packages/tool/`: `tools/workflow/` family, per-kind submission input
  schemas, `cancel_background_session` type union gains `"workflow"`.
- `packages/agent-runtime/`: `workflowDb` dependency, launch-port adapter,
  per-run `workflowTools` assembly, name-universe extension. Gains a
  `@eos/workflow` + `@eos/db` workspace dependency.
- `packages/engine/`: no changes.
- `packages/testkit/`: a scripted `AgentLaunchPort` helper if the workflow
  suite's local fake is wanted by the runtime suite too; otherwise local.
- No new third-party dependencies beyond activating the Phase 00 baseline
  pair (`better-sqlite3`, `kysely`).

Resulting layout:

```
packages/workflow/
├─ src/
│  ├─ aggregate.ts        frozen aggregate types + folder-path addressing
│  ├─ render/
│  │  ├─ md.ts            tagged template, h(depth), brief/spec combinators
│  │  └─ render.ts        ten per-entity renderers (companion §8)
│  ├─ context.ts          planner/worker launch-context policy (§7)
│  ├─ lifecycle.ts        orchestration functions over (trx, aggregate)
│  ├─ scheduler.ts        WorkflowCell, serial reconcile queue, claims
│  ├─ service.ts          WorkflowService: delegate / cancel / query
│  ├─ launch-port.ts      AgentLaunchPort, LaunchedAgent, LaunchSettlement
│  └─ index.ts
├─ tests/
└─ package.json           @eos/workflow; deps: @eos/contracts, @eos/db

packages/db/
├─ src/
│  ├─ database.ts          createDatabase (Kysely + better-sqlite3)
│  ├─ schema.ts            table types
│  ├─ migrations/0001-workflow.ts
│  ├─ workflow-store.ts    transactions, mutations, claims, loadAggregate
│  └─ index.ts
└─ package.json            @eos/db; deps: @eos/contracts
```

Dependency graph stays acyclic: `contracts <- db <- workflow`;
`contracts <- engine <- tool`; `agent-runtime` consumes all (composition
root on top).

## 13. Migration Steps and Progress

| # | Step | Verify | Status |
| --- | --- | --- | --- |
| 1 | Contracts: IDs, status enum, payload schemas | §14 case 1 | Planned |
| 2 | `@eos/db`: database factory, migration, `WorkflowStore` | §14 case 2 round-trips on `:memory:` | Planned |
| 3 | Renderers + combinators | §14 case 3 (the companion acceptance criteria as `it.each` tables) | Planned |
| 4 | Lifecycle functions + scheduler cell over a scripted `AgentLaunchPort` | §14 cases 4-9 with no engine in the suite | Planned |
| 5 | `WorkflowService` (delegate / cancel / query) | §14 cases 8-10 | Planned |
| 6 | `@eos/tool` owned changes: workflow family, per-kind submission schemas, type union | §14 cases 5, 11; existing submission suite updated for the two narrowed schemas | Planned |
| 7 | Runtime wiring: `workflowDb`, launch adapter, per-run assembly, profile name universe | §14 case 12 end-to-end over `MockLlmClient` | Planned |
| 8 | Workspace wiring | `pnpm run check` green; `git diff --stat -- agent-core` empty | Planned |
| 9 | Update the migration `index.md` row | Phase 05 row with status and verification | Planned |

## 14. Verification

Workflow package suite runs against a scripted `AgentLaunchPort` and
`:memory:` databases - no engine, no network. The runtime case (12) is the
only one that drives real `startRun` loops, over `MockLlmClient` scripts.

| # | Case | Asserts |
| --- | --- | --- |
| 1 | Contracts | payload schemas accept the documented shapes; reject empty work-item lists, dangling `needs` references handled at tool layer (case 11); IDs brand and mint |
| 2 | Store round-trip | migration applies; `delegate` rows persist; `loadAggregate` returns the frozen ordered aggregate; `claimLaunchable` flips entity status and claims rows inside the transaction |
| 3 | Renderers | the companion §12 rendering criteria as case tables: `NotStarted` briefs render only the status line; `Running` briefs render content without references; `Success`/`Failed`/`Cancelled` briefs append their own inline reference; attempt spec = plan spec + all item briefs; attempt brief = plan brief + leaf item briefs; workflow/iteration briefs omit goals; references never collect into a tail section; depth nesting emits correct heading levels with no rewriting; prior iterations collapse to status + reference in the workflow brief; every projection opens with the workflow revision stamp |
| 4 | Delegation launches the planner | `delegate` creates workflow/iteration/attempt/plan, commits, then launches: the scripted port records launch-after-commit ordering; the plan is `Running` with `agent_run_id` stamped; no manual launch step exists |
| 5 | Planner materialization | a scripted planner settlement with a valid payload creates `NotStarted` work items, launches every root whose `needs` are empty, leaves dependents unlaunched; goals appear in the directive message, not the briefs |
| 6 | Worker success cascade | a dependent becomes ready when its `needs` succeed and is launched automatically; all-success closes attempt and iteration; no deferred goal closes the workflow `Success`; a deferred goal creates the next iteration + attempt + plan and launches that planner |
| 7 | Failure and retry | a failed worker closes the attempt `Failed` and creates a retry attempt + plan while `attempts < max_attempts` (default 2); exhaustion closes iteration and workflow `Failed` with `fail_reason` recorded; the retry planner's directive names the failed attempt's spec path |
| 8 | Death synthesis | a planner settlement of `failed`, `cancelled`, or `completed`-without-submission synthesizes a failed submission through the same retry path; no entity stays `Running` |
| 9 | Reconcile serialization | two worker settlements enqueued in the same tick reconcile strictly serially (instrumented store sees no interleaved transactions); the terminal resolves exactly once, after the closing commit |
| 10 | Cancel cascade | `cancel` interrupts live child runs, awaits them, marks all non-terminal entities `Cancelled`, resolves the terminal `Cancelled`; a late natural settlement after cancel is a no-op (idempotent transitions) |
| 11 | Tool family | `delegate_workflow` registers the session before returning and rejects a second open delegation; `query_workflow` renders fresh state by path and errors on unknown paths naming valid children; `submit_planner_outcome` rejects duplicate local ids, dangling `needs`, and cycles in-run; `cancel_background_session` accepts `type: "workflow"` |
| 12 | Runtime end-to-end | a scripted main run delegates; scripted planner and worker profiles run through real engine loops; the caller idles -> auto-wait; `session_settled` arrives and is drained; the caller reads `query_workflow` then submits; caller interrupt mid-workflow cascades `workflow_cancelled` into child transcripts and the session settles `cancelled` |

Commands:

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core
pnpm install
pnpm run check
```

- Rust boundary hygiene: `git diff --stat -- agent-core` stays empty.
- Docs hygiene: `git diff --check -- docs/plans/agent-core-rust-to-typescript-migration eos-agent-core`.

## 15. Coexistence and Rollback

- Coexistence: the Rust implementation remains live; `@eos/workflow` and
  the `@eos/db` content have no consumer outside the runtime wiring and
  their suites. The companion spec remains the rendering contract for any
  later Rust-side work; §3 amendments apply to this TypeScript surface.
- Rollback: revert the §9 `@eos/tool` owned changes and the §10 runtime
  wiring, delete the `@eos/workflow` package and `@eos/db` contents, drop
  the index row. Phases 02-04.5 are unaffected.

## 16. Acceptance Criteria

Phase 05 is accepted when:

- `delegate_workflow` initializes workflow, first iteration, first attempt,
  and first plan, and the scheduler launches the planner automatically
  after commit - no user-facing launch step at any point in the lifecycle,
- planner and worker submissions are validated in-run by the per-kind
  payload schemas and consumed exclusively from `outcome.submission` by
  the scheduler; a settlement without a valid submission synthesizes a
  failed submission, and no plan or work item can remain `Running` after
  its run settles,
- the lifecycle matches the companion §9 flows with the §3 amendments:
  ready-work-item launch on materialization and on unblocking successes,
  retry attempts within `max_attempts`, deferred-goal iterations, terminal
  closes - all under the per-workflow serial reconcile queue,
- rendering satisfies the companion §12 criteria (as amended, including
  the §2.19 revision stamp and the §2.20 current-iteration-only workflow
  rollup) from pure depth-parametric renderers, with `query_workflow` and
  launch context as the two consumers and no projected files written,
- a delegated workflow is exactly one supervisor session of the delegating
  run: settlement notification, auto-wait, the submission guard, model
  cancellation, and the caller disposal cascade all work through the one
  registered `SessionHandle`, and child runs take no caller signal,
- the one-open-workflow guard holds inside `delegate_workflow`,
- the §14 suite passes under `pnpm run check` with the workflow package
  suite engine-free over the scripted launch port,
- the Rust `agent-core/` tree is byte-for-byte unchanged,
- and the migration `index.md` lists Phase 05 with status and verification.
