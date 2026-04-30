# Task Center Harness Migration

> Demonstrative migration plan for the next harness/workflow refactor.
> Scope: TaskCenter roles, terminal tools, recovery model, retry mechanic,
> and runtime tool gating.

***

## §1. Architecture (new world)

```
                                USER QUERY
                                    │
                                    ▼
                        ┌──────────────────────┐
                        │  ROOT GENERATOR      │  no harness graph
                        │  (executor agent)    │
                        │  (in TaskCenter)     │
                        └─────────┬────────────┘
                                  │  submit_request_plan
                                  ▼
   ╔════════════════════════════ HarnessGraph Gn ═══════════════════════════╗
   ║                                                                         ║
   ║       ┌─────────┐  submit_full_plan                                     ║
   ║       │ planner │ ───────────────────────────► materialize DAG          ║
   ║       └─────────┘  (or submit_partial_plan, gated)                      ║
   ║                                                                         ║
   ║       ┌──────────────────────────────────────────────────────────┐      ║
   ║       │ DAG (planner-emitted generator tasks):                   │      ║
   ║       │   generator/executor ──┐                                 │      ║
   ║       │   generator/executor ──┼─► generator/verifier            │      ║
   ║       │   generator/executor ──┘   (not a sink; verifier is      │      ║
   ║       │                             still part of generation)     │      ║
   ║       └──────────────────────────────────────────────────────────┘      ║
   ║                              │ all generator tasks passed (DONE)        ║
   ║                              ▼                                          ║
   ║       ┌─────────────────────────────────────┐                           ║
   ║       │ evaluator   (system-spawned;        │                           ║
   ║       │             sink; not in generator   │                           ║
   ║       │             DAG)                    │                           ║
   ║       └─────────────────────────────────────┘                           ║
   ║                              │ submit_evaluation_*                      ║
   ╚══════════════════════════════╪══════════════════════════════════════════╝
                                  │
                  ┌───────────────┴───────────────┐
                  ▼                               ▼
            close success                 retry continuation OR
            (root DONE)                   close failed (root FAILED)

   ─── Subagent (NOT in TaskCenter; non-blocking) ─────────────────────────
       explorer  ── run_subagent(name="explorer", prompt) → future result

   ─── Helpers (NOT in TaskCenter; blocking ask_* calls) ──────────────────
       advisor   ── ask_advisor(tool_name, tool_payloads, prompt)
                    → {verdict, reason}                         no edits
       resolver  ── ask_resolver(issues_to_resolve)
                    → {resolved, summaries}                      can edit
```

***

## §3. Agent roles, helper semantics, and state policy

### §3.1 Role model

TaskCenter owns three main agent roles:

- **Planner**: decomposes a request into either a full plan or a gated partial plan.
- **Generator**: performs generation work. This role has two kinds: **executor** for direct work and **verifier** for checking generator output. Verifiers are still generator tasks, not evaluator sinks.
- **Evaluator**: runs only after every generator task in the graph has passed. It provides the finahl graph-level acceptance decision.

There is one subagent role:

- **Explorer**: launched with `run_subagent(name="explorer", prompt)`. This call is non-blocking and multiple explorer runs may be launched in parallel. Explorer work is read-only and reports back through its subagent result path.

There are two helper roles:

- **Advisor**: called with `ask_advisor(tool_name, tool_payloads, prompt)` before terminal tool submission. It checks whether the agent is choosing the right terminal tool, whether the payload is shaped correctly, and whether the rationale is sufficient. Advisor calls are blocking, inline, and do not edit.
- **Resolver**: called with `ask_resolver(issues_to_resolve)` when a verifier or evaluator finds issues that it cannot resolve through its own read-only checks. Resolver calls are blocking, and only one helper call may run at a time. Resolver can edit and must return `resolved` plus summaries.

### §3.2 State-dependent tool policy

Agent tool availability is state-dependent. The relevant state includes graph
depth, graph lineage, task role, and tool step history. The runtime does not
enforce these changes by mutating the system prompt or dynamically changing tool
registration. Instead, it composes two layers:

- **Soft layer**: system reminders tell the agent which terminal tools are currently disabled or required.
- **Hard layer**: terminal tool prehooks enforce the same policy before the terminal handler runs.

Only roles that own a `submit_*_failure` terminal tool may declare failure. This
authority is role-based: planner has no failure terminal; executor, verifier,
and evaluator do.

***

## §4. Workflows

### §4.1 Happy path (full-plan)

```
[user]
   │
   ▼
[ROOT generator/executor] ──submit_request_plan(note)──► open G1; spawn planner
                                                 │
[planner G1] ──submit_full_plan(dag,details)────► build DAG; planner→HANDOFF
                                                 │
        ┌────────────────────────────────────────┘
        │
   ┌────┴───────────────────────────────────────┐
   ▼                                            ▼
[generator/executor 1]──submit_execution_success    [generator/executor 2]──submit_execution_success
   │                                            │
   └─────────────►[generator/verifier]◄─────────┘
                       │
                       │  may call ask_resolver(issue) 
                       │  if 5x resolved=False → forced submit_verification_failure
                       │
                       └──submit_verification_success──► verifier DONE
                                                              │
                                                  all generator tasks passed
                                                  (DONE)
                                                              │
                                                              ▼
                                              orchestrator spawns evaluator
                                              (READY, harness_graph_id=G1,
                                               graph.evaluator_task_id=eval.id)
                                                              │
                                                  [evaluator] reads DAG summaries
                                                              │
                                                       submit_evaluation_success
                                                              │
                                                              ▼
                                              close_harness_graph_success(G1)
                                              ROOT generator/executor gets
                                              child_success summary and resumes
```

### §4.2 Generator/verifier or evaluator with  resolver loop

```
                ┌─────────────────────────────────────────────────────┐
                │  generator/verifier (or evaluator) — running          │
                │                                                      │
                │  scans evidence / runs checks                        │
                │  finds issue                                         │
                │             │                                        │
                │             ▼                                        │
                │     ask_resolver(issues_to_resolve, ctx)   [BLOCK]   │
                │             │                                        │
                │   ┌─────────┘                                        │
                │   ▼                                                  │
                │  ┌─────────────────────────────────────────────────┐ │
                │  │ resolver —  ephemeral run (no Task)       │ │
                │  │   - reads files                                 │ │
                │  │   - edits files (DIRECT_WORK)                   │ │
                │  │   - submit_resolver_result(resolved, summaries) │ │
                │  └─────────────────────────────────────────────────┘ │
                │             │                                        │
                │             ▼                                        │
                │  read {resolved, summaries}                          │
                │             │                                        │
                │     ┌───────┴────────┐                               │
                │     ▼                ▼                               │
                │  resolved=True    resolved=False                     │
                │     │                │                               │
                │     ▼                ▼                               │
                │  re-check;       another ask_resolver?               │
                │  decide:           (counter++)                       │
                │  submit_*_success                                    │
                │  or submit_*_failure                                 │
                │                                                      │
                │  GATE: at 5 resolver calls with resolved=False,      │
                │  submit_*_success is BLOCKED by prehook;             │
                │  agent must submit_*_failure                         │
                └─────────────────────────────────────────────────────┘
```

### §4.3 Generator failure → generators quiescent → retry continuation

```
[generator/executor 2] ──submit_execution_failure(summary)──► executor FAILED
                                                  │
                                                  ▼
                                  dependent generator tasks become BLOCKED
                                  (dependency_blocked by failed upstream)
                                                  │
                                  remaining non-blocked generator tasks
                                  keep running
                                                  │
                                                  ▼
                                  generators quiescent
                                  (all ∈ {DONE, FAILED, BLOCKED})
                                                  │
                                                  ▼
                                  any generator FAILED or BLOCKED?
                                                  │
                                            yes ──┘
                                                  │
                                                  ▼
                                  mark G1 REQUESTING_RETRY
                                  fail_count = G1.fail_count + 1
                                                  │
                                  TaskCenter observes retry request
                                                  │
                                  G1.fail_count ≤ G1.retry_budget?
                                                  │
                              ┌───────────────────┴─────────────────────┐
                              ▼ yes                                     ▼ no
                      spawn retry continuation                close_harness_graph_failed
                      G2 = Orchestrator.spawn(                root_task FAILED,
                          root=G1.root,                        propagate up
                          prior_graph_id=G1,
                          fail_count=G1.fail_count,
                          request_plan_note=<retry note>)
                              │
                              ▼
                      planner G2 launches with context:
                        ROOT_GOAL: ...
                        PRIOR ATTEMPT (G1):
                          PLAN: <G1 dag + details>
                          OUTCOMES:
                            generator/executor 1: SUCCESS — <summary>
                            generator/executor 2: FAILURE — <summary>
                            generator/verifier: blocked by failed dependency
                        RETRY ATTEMPT 1/1
```

### §4.4 Evaluator failure → retry (or close)

```
[evaluator] ──submit_evaluation_failure(summaries)──► evaluator FAILED
                                                       │
                                                       ▼
                                       mark G1 REQUESTING_RETRY with
                                       evaluator failure summaries
                                                       │
                                              (same branch as §4.3)
                                                       │
                                  ┌────────────────────┴─────────────────────┐
                                  ▼ under budget                             ▼ at budget
                          spawn retry continuation                   close_harness_graph_failed
                          (planner sees all generators passed        root_task FAILED
                          but evaluator rejected → replan
                          accordingly)
```

### §4.5 Closure decision tree (single pivot point)

```
              ┌───────────────────────────────────────────┐
              │  Any generator terminal-transition fires    │
              └────────────────────┬──────────────────────┘
                                   ▼
                       are generator tasks quiescent?
                       (every generator ∈ {DONE, FAILED, BLOCKED})
                                   │
                          ┌────────┴────────┐
                         no                yes
                          │                 │
                          ▼                 ▼
                    keep running     any generator FAILED or BLOCKED?
                                            │
                                   ┌────────┴────────┐
                                  yes              no
                                   │                 │
                                   ▼                 ▼
                         mark REQUESTING_RETRY  spawn evaluator sink (READY)
                         then retry_or_fail
                                                       │
                                                       ▼
                                                [evaluator runs]
                                                       │
                                              submit_evaluation_*
                                                       │
                                            ┌──────────┴──────────┐
                                            ▼                     ▼
                                         success               failure
                                            │                     │
                                            ▼                     ▼
                                  close_harness_graph_success   request_retry_or_fail
```

***

## §5. Tool gating matrix

Terminal tools remain registered for the agent role, but availability is
state-dependent. The reminder layer is advisory and the prehook layer is the
source of truth.

| Terminal                                                                         | Block when                                                                 | State source                                                                              | Soft (notification)                                                                                              | Hard (prehook)                                                                                      |
| -------------------------------------------------------------------------------- | -------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `submit_partial_plan`                                                            | planner's parent/prior graph chain already contains `plan_shape='partial'` | TaskCenter graph (`ctx.task_center.graph.get_harness_graph(...)` + walk `prior_graph_id`) | opening reminder injects "this is a continuation graph; only `submit_full_plan` is permitted" when chain says so | prehook walks chain; returns block on recursive partial                                             |
| `submit_request_plan`                                                            | this generator/executor has called any tool ∈ EDIT\_TOOLS ≥ 1              | agent message history (`ToolExecutionContextService.get("conversation_messages")`)        | inject after first edit: "edits made; `submit_request_plan` is now disabled"                                     | prehook counts EDIT\_TOOLS calls; block if ≥1                                                       |
| `submit_evaluation_success`                                                      | this evaluator has ≥5 `ask_resolver` calls returning `resolved=False`      | agent message history                                                                     | warn at 4: "4/5 resolver calls used; next outcome must be `submit_evaluation_failure`"                           | prehook counts qualifying ask\_resolver calls; block if ≥5                                          |
| `submit_verification_success`                                                    | this verifier has ≥5 `ask_resolver` calls returning `resolved=False`       | agent message history                                                                     | warn at 4: "4/5 resolver calls used; next outcome must be `submit_verification_failure`"                         | prehook counts qualifying ask\_resolver calls; block if ≥5                                          |
| (evaluator spawn)                                                                | any generator task is not DONE                                             | TaskCenter graph                                                                          | n/a — structural                                                                                                 | not a terminal; orchestrator spawns the evaluator only after all generator tasks have passed (DONE) |
| `submit_evaluation_failure`, `submit_verification_failure`, `submit_execution_*` | never blocked for roles that own those terminals                           | —                                                                                         | —                                                                                                                | —                                                                                                   |

### Gate enforcement runtime

```
   agent decides → calls submit_<terminal>(input)
                            │
                            ▼
              ┌─────────────────────────────────────────┐
              │ prehook(tool_input, tool_context)       │
              │                                         │
              │   tool_context.task_center  ──┐         │
              │   conversation_messages     ──┤         │
              │                               ▼         │
              │            evaluate gate condition       │
              │                       │                  │
              │              ┌────────┴────────┐         │
              │              ▼                 ▼         │
              │           ALLOW              BLOCK       │
              └──────────────┬─────────────────┬────────┘
                             │                 │
                             ▼                 ▼
                  run terminal handler   tool returns ToolResult(
                                            output=reason,
                                            is_error=True)
                                          → agent sees error,
                                          chooses different terminal


   Soft layer (notification rules, fired each turn):
        if predicate(messages, query_context) → inject <system-reminder>
   Examples:
        - first-edit-detected → "submit_request_plan disabled"
        - resolver_count == 4  → "1 resolver call left; plan to fail"
        - in continuation chain → "only submit_full_plan permitted"
```

The two layers compose:

- **Notification** = the agent *sees* the constraint in-context, on the turn it matters.
- **Prehook** = the harness *enforces* the constraint even if the agent ignores the notification.

***

## §6. Retry mechanic — single mechanism

**Key insight:** retry = continuation graph with a failure-flavored launch
context. A generator or evaluator failure first marks the graph
`REQUESTING_RETRY`; TaskCenter then consumes that request and either spawns the
next planner attempt or closes the graph as failed when the retry budget is
exhausted.

The retry launch context uses the same `prior_graph_id` chain as partial-plan
continuation, with two branches in `build_continuation_note`:

```
build_continuation_note(graph G):
    walk prior_graph_id chain → [G_old_oldest ... G_old_newest, G]
    for each prior in chain:
        if prior was retry-trigger:
            # any generator FAILED, generator BLOCKED, or evaluator FAILED
            render as RETRY ATTEMPT block
              - prior's plan
              - per-task outcomes
              - failure summaries
        else (partial-plan success):
            render as SEGMENT block (existing behavior)
    render CURRENT REQUEST
```

`request_retry_or_fail(G, summaries)` is the single retry/close pivot:

```
request_retry_or_fail(G, summaries):
    G.status = REQUESTING_RETRY
    G.failure_summaries = summaries
    G.fail_count = G.fail_count + 1

    if G.fail_count ≤ G.retry_budget:
        Orchestrator.spawn(
            tc,
            root_task_id=G.root_task_id,
            request_plan_note=build_continuation_note(G with retry flavor),
            prior_graph_id=G.id,
        )
        new_graph.fail_count = G.fail_count
        new_graph.retry_budget = G.retry_budget
    else:
        close_harness_graph_failed(G, source_task_id=G.evaluator_task_id or last_failed_generator_task)
```

Failure-trigger routing table:

| Source terminal                                  | Wait point                                                           | Calls                                 |
| ------------------------------------------------ | -------------------------------------------------------------------- | ------------------------------------- |
| generator/executor `submit_execution_failure`    | generators quiescent (after dependent blocking + sibling completion) | `request_retry_or_fail(G, summaries)` |
| generator/verifier `submit_verification_failure` | generators quiescent                                                 | same                                  |
| evaluator `submit_evaluation_failure`            | immediate (generators already passed)                                | same                                  |

***

