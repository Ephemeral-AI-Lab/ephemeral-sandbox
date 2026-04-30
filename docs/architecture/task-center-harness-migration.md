# Task Center Harness — Recursive Harness-Graph Migration

> Migration plan that reshapes the harness around **two axes of progression**:
> a **vertical** axis of nested HarnessGraphs (delegation + partial-plan
> continuation), and a **horizontal** axis of `Attempt`s within each graph
> (graph-level retry on failure). Each HarnessGraph has its own in-process
> orchestrator that drives both axes locally.
>
> Scope: harness-graph hierarchy, orchestrator locality, agent roles, terminal
> tools, retry mechanic, partial-plan continuation, and runtime tool gating.

***

## §1. Architecture (recursive new world)

The harness is a **tree of HarnessGraphs**. Each node owns its agents and a
local orchestrator. Inside each graph, the orchestrator runs a sequence of
`Attempt`s — fresh `planner → DAG → evaluator` passes — until one succeeds
or the graph's retry budget is exhausted.

```
                                    ┌── USER QUERY ──┐
                                    ▼                ▼
                       ╔════════════════════════════════════════╗
                       ║  HARNESS GRAPH  G_root                  ║
                       ║  (no parent; bound to the session)      ║
                       ║                                         ║
                       ║   Orchestrator_root                     ║
                       ║      │ init → spawn root executor       ║
                       ║      ▼                                  ║
                       ║   [root generator/executor]             ║
                       ║      │  submit_request_plan(note)       ║
                       ║      ▼                                  ║
                       ║   spawn_child_graph(REQUEST_PLAN)       ║
                       ╚═══════════════╪═════════════════════════╝
                                       │
                                       ▼
            ╔══════════════════════════════════════════════════════════╗
            ║  HARNESS GRAPH  G1   (child of G_root via REQUEST_PLAN)   ║
            ║  retry_budget = N    attempts_used ≤ N                    ║
            ║                                                           ║
            ║   Orchestrator_G1                                         ║
            ║                                                           ║
            ║   ┌────── Attempt 1 ──────┐                               ║
            ║   │ planner → DAG → eval  │ — failed (some generator)     ║
            ║   └───────────────────────┘                               ║
            ║   ┌────── Attempt 2 ──────┐                               ║
            ║   │ planner → DAG → eval  │ — failed (evaluator)          ║
            ║   └───────────────────────┘                               ║
            ║   ┌────── Attempt 3 ──────┐                               ║
            ║   │ planner → DAG → eval  │ — passed                      ║
            ║   └───────────────────────┘                               ║
            ║                       │                                   ║
            ║                       ▼                                   ║
            ║   plan_shape == 'partial'?                                ║
            ║      ├── yes → spawn child G_next (CONTINUE_AFTER…)       ║
            ║      └── no  → close G1 success; bubble up to G_root      ║
            ╚═══════════╪═══════════════════════════════════════════════╝
                        │ partial-plan only
                        ▼
            ╔══════════════════════════════════════════════════════════╗
            ║  HARNESS GRAPH  G_next  (child of G1 via CONTINUE…)       ║
            ║  retry_budget = M    its own attempts                     ║
            ║                                                           ║
            ║   Orchestrator_G_next                                     ║
            ║   ┌────── Attempt 1 ──────┐                               ║
            ║   │ planner → DAG → eval  │ — passed                      ║
            ║   └───────────────────────┘                               ║
            ║                       │                                   ║
            ║   close G_next success → bubble up through G1 → root      ║
            ╚═══════════════════════════════════════════════════════════╝

   ─── Subagent (NOT in TaskCenter; non-blocking) ───────────────────────
       explorer  ── run_subagent(name="explorer", prompt) → future result

   ─── Helpers (NOT in TaskCenter; blocking ask_* calls) ────────────────
       advisor   ── ask_advisor(tool_name, tool_payloads, prompt)
                    → {verdict, reason}                         no edits
       resolver  ── ask_resolver(issues_to_resolve)
                    → {resolved, summaries}                      can edit
```

***

## §2. Two axes of progression

The harness moves along two distinct axes. Conflating them is what made the
prior single-spawn-tree model awkward.

| Axis        | Entity         | What changes              | Triggered by                                             | Tree shape effect                            |
| ----------- | -------------- | ------------------------- | -------------------------------------------------------- | -------------------------------------------- |
| Vertical    | `HarnessGraph` | new scope                 | `REQUEST_PLAN`, `CONTINUE_AFTER_PARTIAL_PLAN`            | depth — pipeline gets longer                 |
| Horizontal  | `Attempt`      | same scope, fresh try     | retry on Attempt failure (under graph's retry budget)    | breadth inside a single graph — no new depth |

### §2.1 Vertical (HarnessGraph)

A HarnessGraph represents **a scoped goal**. New graphs are created when the
pipeline needs to do new work:

- `REQUEST_PLAN` — an executor delegates a subgoal. The executor's enclosing
  graph spawns a child graph for that subgoal.
- `CONTINUE_AFTER_PARTIAL_PLAN` — a planner finished a partial plan and left
  forward instructions. The completing graph spawns a child graph that picks
  up from those instructions.

In both cases the tree gets one level deeper.

### §2.2 Horizontal (Attempt)

An `Attempt` is **one full `planner → DAG → evaluator` pass at the graph's
goal**. A graph holds an ordered list of Attempts; only the latest one is
running. When an Attempt fails and the graph still has retry budget, the
orchestrator spawns the next Attempt **inside the same graph**. Retry never
deepens the tree; the graph itself is the unit of retry.

### §2.3 Why this asymmetry matters

- Tree depth now reflects only *real* pipeline depth (delegation +
  segmentation). A 3-deep tree means three layers of scope, not two retries.
- `prior_graph_id` chain is reserved for partial-plan segments — its original
  purpose. Retry history lives on `Attempt` rows inside a graph.
- The retry budget is graph-local, which is what it semantically is. No
  cross-graph inheritance or shared counters.
- Bubble-up only happens when a child graph closes — never on a retry — so
  the parent graph's view of its child is monotonic (one final outcome, not
  a sequence of failed attempts).

***

## §3. Components

| Component                | Owner / scope                  | Responsibility                                                                                                                                                                                                                                                                       |
| ------------------------ | ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `HarnessGraph`           | TaskCenter                     | Container for one scoped goal. Holds `parent_harness_graph_id`, `prior_graph_id` (set only on `CONTINUE_AFTER_PARTIAL_PLAN`), `spawn_reason`, `retry_budget`, ordered list of `Attempt`s.                                                                                              |
| `Attempt`                | TaskCenter, owned by HarnessGraph | One pass at the graph's goal: `planner_task_id`, DAG edges, generator task ids, `evaluator_task_id`, `status`, `fail_reason`, `attempt_index`, `prior_attempt_id` (within the same graph).                                                                                       |
| `Orchestrator`           | one per `HarnessGraph`         | Drives the local graph: spawns each Attempt's planner / DAG / evaluator, observes terminal transitions, decides next-Attempt vs close-graph, spawns child graphs when needed. **In-process class instance** keyed off `HarnessGraph.id`; durable state lives on the row, the object is ephemeral and looked up on demand by terminal handlers. |
| `RootHarnessGraph`       | TaskCenter (singleton/session) | Special graph wrapping only the root executor. No Attempts (no planner/DAG/evaluator). Closing it ends the session.                                                                                                                                                                  |
| Tasks                    | per `Attempt`                  | Agent runs (planner, executor, verifier, evaluator) scoped to a single Attempt of a single graph.                                                                                                                                                                                   |

### §3.1 Root harness graph is special

- `G_root` exists for the lifetime of the session.
- It contains **only one task**: the root generator/executor. It has no
  Attempt list (the root executor isn't a `planner → DAG → evaluator` pass).
- Its orchestrator's only jobs are:
  - **Init**: spawn the root executor.
  - **Catch root executor terminals**:
    - `submit_request_plan(note)` → spawn child graph (`REQUEST_PLAN`), wait
      for child close, deliver child summary back to the root executor.
    - `submit_execution_success` / `submit_execution_failure` → close
      `G_root` → session ends.
- The root executor's tool-gating rules are identical to any other executor
  (e.g. `submit_request_plan` is disabled after the first edit).
- `G_root.retry_budget = 0` — root executor has no retry mechanism.

***

## §4. Vertical motion: child HarnessGraphs

A child graph is spawned by the parent graph's orchestrator. Two reasons,
each with its own lineage rules.

| Spawn reason                  | Trigger                                                              | Spawned by                       | `parent_harness_graph_id`         | `prior_graph_id`         |
| ----------------------------- | -------------------------------------------------------------------- | -------------------------------- | --------------------------------- | ------------------------ |
| `REQUEST_PLAN`                | An executor task calls `submit_request_plan(note)`                   | Orchestrator of executor's graph | the graph containing the executor | — (chain reset)          |
| `CONTINUE_AFTER_PARTIAL_PLAN` | Parent graph's last Attempt passed and `plan_shape == 'partial'`     | Orchestrator of completing graph | the completed graph               | the completed graph      |

> Planner launch context (the `note`, continuation instructions, prior-attempt
> evidence) is composed by the **context engine** — discussed separately. The
> harness only owns the structural lineage edges shown above.

### §4.1 Visual: vertical spawn paths

```
        ┌──────────────────── parent harness graph G_p ────────────────────┐
        │                                                                  │
        │  (a) REQUEST_PLAN                                                 │
        │      executor in G_p ── submit_request_plan(note) ──► Orch_p      │
        │                                                       └─► spawn G_c
        │      G_p stays open, awaiting G_c's outcome                      │
        │                                                                  │
        │  (b) CONTINUE_AFTER_PARTIAL_PLAN                                  │
        │      G_p's last Attempt passed with plan_shape='partial'          │
        │      Orch_p reads continuation instructions                       │
        │                                                       └─► spawn G_c
        │      G_p stays open, awaiting G_c's outcome                      │
        └──────────────────────────────────────────────────────────────────┘
                                            │
                                            ▼
                                  new harness graph G_c
                                  parent_harness_graph_id = G_p
                                  spawn_reason            = (a|b)
                                  prior_graph_id          = G_p when (b),
                                                            null when (a)
```

### §4.2 Lineage chain semantics

Two distinct upward walks coexist on the tree:

- `parent_harness_graph_id` ↑ → **containment hierarchy**. Used by
  orchestrator ownership and by bubble-up of close reports.
- `prior_graph_id` ↑ → **partial-plan segment chain only**. Used by
  tool-gating predicates (recursive partial-plan check) and as input to the
  context engine when building a continuation graph's planner context.

The chains overlap on `CONTINUE_AFTER_PARTIAL_PLAN` edges and diverge at
every `REQUEST_PLAN` boundary, where `prior_graph_id` resets while
`parent_harness_graph_id` continues. **Retry no longer extends
`prior_graph_id`** — retry attempts live inside the graph.

***

## §5. Horizontal motion: `Attempt`s within a graph

An Attempt is **one full `planner → DAG → evaluator` pass at the graph's
goal**. The HarnessGraph holds an ordered list of Attempts; the orchestrator
drives them one at a time and is the only thing that creates them.

### §5.1 Attempt structure

```
Attempt {
    attempt_index:        N
    prior_attempt_id:     id of Attempt N-1 (null on first)
    stage:                planning | generating | evaluating | closed
    planner_task_id:      uuid
    dag_edges:            [(from, to), ...]              # set after plan submission
    generator_task_ids:   [executor_1, verifier, ...]    # set after plan submission
    evaluator_task_id:    uuid                           # set when evaluator spawns
    status:               running | passed | failed
    fail_reason:          null
                        | planner_step_budget_exhausted
                        | generator_failed
                        | evaluator_failed
}

# Per-attempt evidence (per-task summaries, planner scratchpad, etc.)
# is owned by the context engine — not stored here.
```

### §5.2 Failure escape valves (whole harness)

```
Failure escape valves:
  ├─ Tool-call-level error (any agent)
  │     prehook returns is_error=True / handler rejects payload
  │     → agent sees ToolResult(is_error=True), retries within its own run
  │     (no Attempt-level escalation)
  │
  ├─ Generator/Verifier submit_*_failure
  │     → after generators quiescent, mark Attempt failed (generator_failed)
  │
  ├─ Evaluator submit_evaluation_failure
  │     → mark Attempt failed (evaluator_failed) immediately
  │
  └─ Planner agent ends without a successful submit_*_plan
        → runtime marks Attempt failed (planner_step_budget_exhausted)
```

The planner has **no failure terminal**. Its only soft-fail channel is
inline tool-call rejection (gating prehook returns `is_error=True`, or the
plan-materialization handler rejects a malformed DAG); those stay inside the
planner agent's own loop. Only when the planner agent ends without a
successful submission does the runtime synthesise
`planner_step_budget_exhausted` and escalate to the orchestrator.

### §5.3 Attempt failure modes (orchestrator-visible)

| Failure mode                       | Detected by                                                         | Wait point                                                                                  |
| ---------------------------------- | ------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| `planner_step_budget_exhausted`    | runtime ends planner agent without a valid plan submission          | immediate (planner is the only running task in the `planning` stage)                        |
| `generator_failed`                 | any generator submitted `*_failure` (executor or verifier)          | **quiescence** — wait until every generator ∈ `{DONE, FAILED, BLOCKED}`                     |
| `evaluator_failed`                 | evaluator submitted `submit_evaluation_failure`                     | immediate (generators already DONE by definition before evaluator was spawned)              |

#### Generator-failure detail

- When a generator submits `*_failure`, its dependents in the DAG transition
  to `BLOCKED`.
- Sibling generators that don't depend on the failed one **keep running**.
- The orchestrator does **not** initiate retry mid-flight. It waits until
  every generator is in `{DONE, FAILED, BLOCKED}`, then makes one decision.
- The next Attempt's planner sees the full landscape: which generators
  passed, which failed, which were blocked.

#### Evaluator-failure detail

- The evaluator is only spawned after all generators DONE, so quiescence is
  trivially satisfied.
- Evaluator failure triggers retry immediately.

### §5.4 Retry budget (graph-local)

`HarnessGraph.retry_budget` is set at graph creation and **never inherited
across graphs**. `attempts_used` is just `len(graph.attempts)`.

| Graph creation                | `retry_budget`                                  |
| ----------------------------- | ----------------------------------------------- |
| `G_root` (session start)      | `0` — root executor has no retry                |
| `REQUEST_PLAN` child          | freshly configured (defaults or note override)  |
| `CONTINUE_AFTER_PARTIAL_PLAN` child | freshly configured (defaults or note override)  |

Partial-plan continuation does not inherit prior segments' attempt counts.
Each segment is its own graph with its own budget.

### §5.5 `try_spawn_next_attempt` (per local orchestrator)

```
try_spawn_next_attempt(G, A_failed, fail_reason):
    A_failed.status      = failed
    A_failed.fail_reason = fail_reason

    if G.attempts_used < G.retry_budget:
        A_next = G.create_attempt(
            attempt_index    = G.attempts_used + 1,
            prior_attempt_id = A_failed.id,
        )
        G.attempts.append(A_next)
        spawn planner for A_next
        # planner's launch context is composed by the context engine (TBD)
        # G stays open; A_next is the new "current" Attempt

    else:
        close_harness_graph_failed(
            G,
            source_task_id = A_failed.evaluator_task_id
                          or A_failed.last_failed_generator_task_id
                          or A_failed.planner_task_id,
        )
        # G's parent orchestrator delivers child_failure to whatever requested G
```

***

## §6. Agent roles, helper semantics, and state policy

### §6.1 Role model

TaskCenter owns three main agent roles, all scoped to a single Attempt of a
single HarnessGraph:

- **Planner**: decomposes the graph's goal into either a full plan or a
  gated partial plan. Has only `submit_full_plan` / `submit_partial_plan`;
  **no failure terminal**.
- **Generator**: performs generation work. Two kinds — **executor** for
  direct work and **verifier** for checking generator output. Verifiers are
  still generator tasks, not evaluator sinks.
- **Evaluator**: runs only after every generator in the Attempt has passed.
  Provides Attempt-level acceptance.

One subagent role:

- **Explorer**: launched with `run_subagent(name="explorer", prompt)`.
  Non-blocking, parallel-safe, read-only.

Two helper roles:

- **Advisor**: `ask_advisor(tool_name, tool_payloads, prompt)` before
  terminal submission. Blocking, inline, no edits.
- **Resolver**: `ask_resolver(issues_to_resolve)` when a verifier or
  evaluator finds issues it cannot resolve through read-only checks.
  Blocking; one helper call at a time. Resolver can edit and must return
  `resolved` plus summaries.

Failure authority is role-based. Only roles that own a `submit_*_failure`
terminal may declare failure: executor, verifier, evaluator. **Planner has
no failure terminal**.

### §6.2 State-dependent tool policy

Tool availability depends on graph depth, lineage chain, task role, and
tool-step history. The runtime composes two layers — neither mutates the
system prompt nor changes tool registration:

- **Soft layer**: system reminders inject which terminal tools are currently
  disabled or required.
- **Hard layer**: terminal-tool prehooks enforce the same policy before the
  terminal handler runs.

***

## §7. Workflows

### §7.1 Happy path (full plan, single Attempt)

```
G_root.Orch: init → spawn root executor
     │
     ▼
[root executor] ── submit_request_plan(note)
     │
G_root.Orch catches the terminal; spawns child G1 (REQUEST_PLAN)
     │
     ▼
G1.Orch: init Attempt 1 → spawn planner
     │
     ▼
[planner G1.A1] ── submit_full_plan ──► materialize DAG
     │
G1.Orch transitions A1 to "generating"
     │
     ├── [generator/executor 1] ── submit_execution_success
     ├── [generator/executor 2] ── submit_execution_success
     │                                  │
     │                                  ▼
     └── [generator/verifier]  ── submit_verification_success
                                  │
                       all generators DONE
                                  │
G1.Orch transitions A1 to "evaluating"; spawns evaluator
                                  │
                                  ▼
[evaluator G1.A1] ── submit_evaluation_success
                                  │
G1.Orch marks A1 passed, plan_shape == 'full'
                                  │
G1.Orch closes G1 success
                                  │
                                  ▼
G_root.Orch delivers child_success summary to root executor
                                  │
                                  ▼
[root executor] resumes — may submit_request_plan again or finish
                                  │
                                  ▼
G_root closes → session ends
```

### §7.2 Resolver loop inside a verifier or evaluator

(Behaviour unchanged; runs entirely within one Attempt.)

```
[verifier or evaluator] ── ask_resolver(issues)  [BLOCK] ──► resolver runs
                                                              │ may edit
                                                              ▼
                                          submit_resolver_result(resolved, summaries)
                                                              │
                       ┌──────────────────────────────────────┘
                       ▼
              read {resolved, summaries}
                       │
              ┌────────┴────────┐
       resolved=True       resolved=False
              │                 │
              ▼                 ▼
        re-check &           counter++
        decide               another ask_resolver?
                                   │
                       at counter=5 with resolved=False:
                       prehook BLOCKS submit_*_success;
                       agent must submit_*_failure
```

### §7.3 Generator failure → quiescence → next Attempt (horizontal)

```
[generator/executor 2 in G1.A1] ── submit_execution_failure(...)
                                       │
G1.Orch marks executor_2 FAILED
                                       │
                          dependent generators → BLOCKED
                                       │
                  remaining non-blocked generators keep running
                                       │
                                       ▼
                          generators quiescent in A1
                          (every generator ∈ {DONE, FAILED, BLOCKED})
                                       │
                          any FAILED or BLOCKED?
                                       │
                                  yes ─┘
                                       │
                                       ▼
                          A1.status      = failed
                          A1.fail_reason = generator_failed
                                       │
                          G1.attempts_used = 1
                          G1.attempts_used < G1.retry_budget?
                                       │
                       ┌───────────────┴───────────────┐
                       ▼ yes                           ▼ no
   G1.Orch spawns Attempt 2 in G1            G1.Orch closes G1 failed
   attempt_index    = 2                      bubble up to parent (G_root)
   prior_attempt_id = A1.id                  → root executor sees
                                              child_failure
```

`G1` itself doesn't move in the tree. The new Attempt lives inside `G1`.
The parent (`G_root`) sees nothing during retry — only the eventual close
summary.

### §7.4 Evaluator failure → next Attempt (horizontal)

```
[evaluator G1.A1] ── submit_evaluation_failure(...)
                       │
                       ▼
A1.status      = failed
A1.fail_reason = evaluator_failed
       │
       ▼
G1.attempts_used = 1
G1.attempts_used < G1.retry_budget?

If yes:
  G1.Orch spawns Attempt 2 in G1
Otherwise:
  G1.Orch closes G1 failed → bubble up
```

### §7.5 Planner step-budget exhaustion → Attempt failed

```
[planner G1.A1]  ... agent step budget runs out without successful submission ...
                       │
runtime ends planner agent run
                       │
                       ▼
A1.status      = failed
A1.fail_reason = planner_step_budget_exhausted
                       │
G1.attempts_used = 1
                       │
       ┌───────────────┴───────────────┐
       ▼ under budget                  ▼ at budget
  G1.Orch spawns Attempt 2        G1.Orch closes G1 failed
                                  bubble up
```

### §7.6 Partial plan → spawn `CONTINUE_AFTER_PARTIAL_PLAN` child (vertical)

```
[planner G1.A_k] ── submit_partial_plan(
                        dag,
                        details,
                        instructions_on_what_to_do_after_completion_of_partial_plan
                    )
                       │
                       ▼
G1.A_k runs the partial DAG (executors → verifiers → evaluator)
                       │
                       ▼
              evaluator submits success
                       │
                       ▼
G1.Orch: A_k passed, plan_shape == 'partial'
                       │
                       ▼
            spawn child G_next (CONTINUE_AFTER_PARTIAL_PLAN)
            parent_harness_graph_id = G1
            prior_graph_id          = G1
                       │
                       ▼
G_next.Orch: init Attempt 1 → spawn planner
                       │
            planner G_next.A1: prior_graph_id chain already contains
            plan_shape='partial' ⇒ submit_partial_plan is GATED
            (soft + hard); must use submit_full_plan
```

`G1` stays open while `G_next` runs. The closure of `G_next` bubbles
through `G1` (which then closes itself with the same outcome) up to whoever
requested `G1`.

### §7.7 Nested `REQUEST_PLAN` (executor inside an Attempt requests a subplan)

```
G1.A_current has [generator/executor 7] in its DAG
[generator/executor 7] ── submit_request_plan(note)
                       │
G1.Orch catches the terminal; spawns child G1.X (REQUEST_PLAN)
parent_harness_graph_id = G1
prior_graph_id          = null   ◄── chain RESET (fresh attempt)
spawn_reason            = REQUEST_PLAN
                       │
                       ▼
G1.X runs its own Attempts to completion
                       │
G1.Orch delivers child_success/failure summary
back to executor 7 inside G1.A_current
                       │
executor 7 resumes inside G1.A_current's DAG
```

`REQUEST_PLAN` may happen at any depth and during any Attempt. Tool-gating
predicates that walk `prior_graph_id` (recursive partial-plan check) reset
at this boundary.

### §7.8 Closure decision tree (per local orchestrator)

#### Stage 1: decide the current Attempt's outcome

```
Orchestrator_G observes a terminal-transition in current Attempt A
        │
        ▼
   A.stage at moment of transition:
        │
   ┌────┴────────┬─────────────────────┐
   ▼             ▼                     ▼
 planning     generating           evaluating
   │             │                     │
   ▼             ▼                     ▼
 planner agent  failure made A's      evaluator submitted
 ended without   generators            *_success?
 a valid plan?   quiescent?            ┌──┴──┐
   │           ┌──┴──┐                 yes   no
   │          no    yes                 │     │
   │           │     │                  ▼     ▼
   │           ▼     ▼                  A     A
   │         keep   any FAILED          passed failed
   │         running or BLOCKED?               (evaluator_failed)
   │                ┌──┴──┐
   │               yes    no
   │                │      │
   ▼                ▼      ▼
 A failed         A      spawn evaluator
 (planner_       failed  (transition A to evaluating)
  step_…)        (generator_failed)
```

#### Stage 2: react to the Attempt's outcome at graph level

```
A.status:
   ├─ passed
   │   └─ A.plan_shape == 'partial'?
   │       ├─ yes → spawn child G_next (CONTINUE_AFTER_PARTIAL_PLAN);
   │       │       wait; adopt G_next's outcome; close G with that outcome
   │       └─ no  → close G success; bubble up to parent
   │
   └─ failed
       └─ G.attempts_used < G.retry_budget?
           ├─ yes → spawn Attempt N+1 in G
           └─ no  → close G failed; bubble up to parent
```

***

## §8. Tool gating matrix

State lives on the harness graph, the current Attempt, and agent message
history, evaluated by the local orchestrator. Reminder layer is advisory;
prehook layer is authoritative.

| Terminal                                                                         | Block when                                                                  | State source                                                                                                       | Soft (notification)                                                                                              | Hard (prehook)                                                                                              |
| -------------------------------------------------------------------------------- | --------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `submit_partial_plan`                                                            | planner's `prior_graph_id` chain already contains `plan_shape='partial'`    | local graph + walk `prior_graph_id` upward (partial-plan segments only; does **not** cross `REQUEST_PLAN` resets)  | opening reminder injects "this is a continuation graph; only `submit_full_plan` is permitted" when chain says so | prehook walks `prior_graph_id` chain; returns block on recursive partial                                    |
| `submit_full_plan` / `submit_partial_plan` (malformed DAG)                       | DAG fails materialization (cycle, dangling edge, unknown task ref)          | handler-level validation                                                                                           | n/a (planner sees the rejection and tries again)                                                                 | handler returns `ToolResult(is_error=True, output=reason)`; planner agent retries within its run            |
| `submit_request_plan`                                                            | this generator/executor has called any tool ∈ EDIT_TOOLS ≥ 1                | agent message history                                                                                              | inject after first edit: "edits made; `submit_request_plan` is now disabled"                                     | prehook counts EDIT_TOOLS calls; block if ≥1                                                                |
| `submit_evaluation_success`                                                      | this evaluator has ≥5 `ask_resolver` calls returning `resolved=False`       | agent message history                                                                                              | warn at 4: "4/5 resolver calls used; next outcome must be `submit_evaluation_failure`"                           | prehook counts qualifying ask_resolver calls; block if ≥5                                                   |
| `submit_verification_success`                                                    | this verifier has ≥5 `ask_resolver` calls returning `resolved=False`        | agent message history                                                                                              | warn at 4: "4/5 resolver calls used; next outcome must be `submit_verification_failure`"                         | prehook counts qualifying ask_resolver calls; block if ≥5                                                   |
| (evaluator spawn — orchestrator-internal, not a terminal)                        | any generator task in current Attempt is not DONE                           | local Attempt's task statuses                                                                                      | n/a — structural                                                                                                 | local orchestrator only spawns the evaluator after every generator in the current Attempt has passed (DONE) |
| (next-Attempt spawn — orchestrator-internal, not a terminal)                     | `attempts_used >= retry_budget`                                             | local `HarnessGraph` state                                                                                         | n/a — structural                                                                                                 | local orchestrator closes G failed instead of spawning next Attempt                                         |
| `submit_evaluation_failure`, `submit_verification_failure`, `submit_execution_*` | never blocked for roles that own those terminals                            | —                                                                                                                  | —                                                                                                                | —                                                                                                           |

### §8.1 Gate enforcement runtime

```
agent decides → calls submit_<terminal>(input)
                     │
                     ▼
        ┌──────────────────────────────────────────┐
        │ prehook(tool_input, tool_context)         │
        │                                           │
        │   tool_context.task_center      ──┐       │
        │   tool_context.harness_graph    ──┤       │
        │   tool_context.attempt          ──┤       │
        │   conversation_messages         ──┤       │
        │                                   ▼       │
        │            evaluate gate condition        │
        │                       │                   │
        │              ┌────────┴────────┐          │
        │              ▼                 ▼          │
        │            ALLOW            BLOCK         │
        └──────────────┬──────────────────┬────────┘
                       │                  │
                       ▼                  ▼
            run terminal handler    ToolResult(
            (local orchestrator       output=reason,
             picks up the             is_error=True)
             transition)            → agent sees error,
                                      chooses different terminal
```

Soft layer (per-turn notification rules) examples:
- first-edit-detected → "submit_request_plan disabled"
- resolver_count == 4 → "1 resolver call left; plan to fail"
- in `prior_graph_id` chain that contains `partial` → "only submit_full_plan permitted"

The two layers compose:
- **Notification** = the agent *sees* the constraint in-context, on the turn it matters.
- **Prehook** = the harness *enforces* the constraint even if the agent ignores the notification.

***

## §9. Bubble-up and close summary

A graph closes exactly once. Its outcome is the outcome of either:

- the latest Attempt that passed (graph success), or
- the last Attempt at the retry budget (graph failure).

When a child graph `G_c` closes, its orchestrator hands a **close report**
to its parent's orchestrator. The harness owns only the structural facts:
`harness_graph_id`, `spawn_reason`, `outcome` (`success | failed`), and
`plan_shape` of the final Attempt (`full | partial`). The detailed payload
(per-task summaries, planner scratchpads, evidence) is composed by the
**context engine** — discussed separately.

Routing of the close report depends on `spawn_reason`:

| Spawn reason                  | Where the close report lands                                                                                                                              |
| ----------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `REQUEST_PLAN`                | back to the executor task that called `submit_request_plan`; that executor resumes inside its own Attempt                                                 |
| `CONTINUE_AFTER_PARTIAL_PLAN` | the parent graph adopts the child's outcome verbatim and closes itself with the same outcome (cascading bubble-up across the partial-plan segment chain)  |

Retry **never** bubbles up. It's purely internal motion within a single
graph; from the parent's perspective each child closes exactly once with
exactly one outcome.

***

## §10. Open questions / migration sequencing

1. **`REQUEST_PLAN` / `CONTINUE_AFTER_PARTIAL_PLAN` retry-budget defaults** —
   what's the default budget for child graphs? Configurable per-graph via
   the executor's note / continuation instructions, or fixed in the runtime?
2. **Concurrency model for parent-while-child-runs** — the parent graph
   stays open while its `CONTINUE_AFTER_PARTIAL_PLAN` child runs, and a
   `REQUEST_PLAN`-spawning executor stays paused inside its own Attempt.
   Confirm the Attempt's `stage` handles "paused waiting on child graph"
   without confusion with the three named stages.
3. **Planner step-budget mechanism** — confirm how the runtime detects
   "planner agent ended without a successful submission" (turn count cap?
   wall-clock? abort signal?), and how that bubbles up to the orchestrator
   as `planner_step_budget_exhausted` rather than a normal terminal.
4. **Context engine boundary** — out of scope for this doc: planner launch
   context composition, per-Attempt evidence storage, close-report payload
   schema, cross-graph visibility of prior-segment work. All deferred to
   the context-engine spec.
