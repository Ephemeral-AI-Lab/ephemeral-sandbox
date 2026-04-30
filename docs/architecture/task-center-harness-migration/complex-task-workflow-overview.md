# Complex Task Segmentation and Harness Graph Workflow

This document summarizes how a complex executor task is routed through the
harness graph runtime and reported back to the task that requested it.

The migration separates three concepts that were previously overloaded:

- `ComplexTaskRequest`: the delegated complex goal requested by an executor.
- `TaskSegment`: the single request-local execution segment that owns retry
  budget.
- `HarnessGraph`: one concrete planner-produced graph execution inside the
  segment.

There is no partial-plan continuation feature in the target model. A
`ComplexTaskRequest` has exactly one `TaskSegment`. Retry creates another
`HarnessGraph` inside that segment; it never creates another segment.

## Mental model

Complex task progression has two axes:

| Axis | Entity | Trigger | Meaning |
| ---- | ------ | ------- | ------- |
| Request origin | `ComplexTaskRequest` | Executor calls `request_complex_task_solution(goal)` | A delegated complex goal starts for the requesting task. |
| Horizontal retry | `HarnessGraph` | A graph fails and segment retry budget remains | The same segment receives a fresh planner-produced graph. |

```mermaid
flowchart TD
    E["Executor task"] -->|"request_complex_task_solution(goal)"| C["ComplexTaskRequest"]
    C --> S1["TaskSegment S1"]
    S1 --> H11["HarnessGraph S1.H1"]
    H11 -->|"failed, retry budget remains"| H12["HarnessGraph S1.H2"]
    H12 -->|"passed"| Close["Close ComplexTaskRequest"]
    Close -->|"final report"| E
```

The key rule is:

- A `ComplexTaskRequest` has one `TaskSegment`.
- A new `HarnessGraph` inside that segment means retry after failure.
- The complex-task close report supplies the final result for
  `requested_by_task_id`; the original executor agent run ends at the handoff.

## Layer responsibilities

| Layer | Owns | Does not own |
| ----- | ---- | ------------ |
| `ComplexTaskRequestHandler` | Request creation and close, initial segment creation, final close report to `requested_by_task_id`. | Per-segment retry policy or graph execution. |
| `TaskSegmentManager` | One segment's retry budget, next harness graph creation after failed graphs, segment close, `TaskSegmentClosureReport`. | Request creation or planner/generator/evaluator execution. |
| `HarnessGraphOrchestrator` | One `planner -> generator DAG -> evaluator` execution and graph pass/fail outcome. | Retry or request close. |
| Agent roles | Planner, generator executor, verifier, and evaluator terminal submissions inside a graph. | Structural lifecycle decisions. |
| Context engine | Role-specific launch context, durable summaries, and detailed close-report payloads. | Lifecycle policy or source-of-truth state transitions. |

## End-to-end flow

```mermaid
sequenceDiagram
    participant E as Executor
    participant R as ComplexTaskRequestHandler
    participant S as TaskSegmentManager
    participant H as HarnessGraphOrchestrator
    participant A as Agents

    E->>R: request_complex_task_solution(goal)
    R->>R: create ComplexTaskRequest
    R->>R: create TaskSegment S1
    R->>S: spawn manager for S1
    S->>H: create HarnessGraph S1.H1
    H->>A: spawn planner
    A->>H: submit_full_plan
    H->>A: spawn generator DAG
    A->>H: generator/verifier terminal submissions
    H->>A: spawn evaluator after all generators pass
    A->>H: submit_evaluation_success or submit_evaluation_failure
    H->>S: report graph passed or failed
    S->>R: emit TaskSegmentClosureReport when segment closes
    R->>E: deliver final complex-task report
```

## Harness graph lifecycle

`HarnessGraphOrchestrator` owns exactly one graph run. It does not inspect
retry budget and does not create sibling graphs.

```mermaid
flowchart TD
    Start["HarnessGraph starts"] --> Plan["planning: run planner"]
    Plan -->|"valid full plan submitted"| Gen["generating: run executor/verifier DAG"]
    Plan -->|"planner ends without valid plan"| PlannerFail["close graph failed: planner_step_budget_exhausted"]
    Gen -->|"any generator failed or blocked after quiescence"| GenFail["close graph failed: generator_failed"]
    Gen -->|"all generators done"| Eval["evaluating: run evaluator"]
    Eval -->|"submit_evaluation_success"| Passed["close graph passed"]
    Eval -->|"submit_evaluation_failure"| EvalFail["close graph failed: evaluator_failed"]
    PlannerFail --> Segment["report graph outcome to TaskSegmentManager"]
    GenFail --> Segment
    Passed --> Segment
    EvalFail --> Segment
```

Generator failure waits for quiescence: failed generators block dependents,
independent siblings may finish, and the graph closes only after all generator
nodes are terminal.

## Segment decision flow

`TaskSegmentManager` reacts to the closed graph. It is the only layer that can
spend segment retry budget.

```mermaid
flowchart TD
    HClose["HarnessGraph closes"] --> Passed{"Graph passed?"}

    Passed -->|"no"| Retry{"Retry budget remains?"}
    Retry -->|"yes"| NextH["Create next HarnessGraph in same TaskSegment"]
    NextH --> RunNext["Run next graph through HarnessGraphOrchestrator"]
    Retry -->|"no"| FailSeg["Close TaskSegment failed"]
    FailSeg --> FailReport["Emit TaskSegmentClosureReport: attempt_plan_failed(attempted_plan_history)"]

    Passed -->|"yes"| SuccessReport["Emit TaskSegmentClosureReport: terminal_success"]
```

A passed graph always closes its segment. There is no retry after a passing
graph; graph quality is enforced by the evaluator.

## Request decision flow

`ComplexTaskRequestHandler` reacts only to `TaskSegmentClosureReport`.

```mermaid
flowchart TD
    Report["TaskSegmentClosureReport"] --> Outcome{"Outcome"}
    Outcome -->|"terminal_success"| Success["Close ComplexTaskRequest succeeded"]
    Outcome -->|"attempt_plan_failed(attempted_plan_history)"| Failed["Close ComplexTaskRequest failed"]
    Success --> Return["Deliver final report to requested_by_task_id"]
    Failed --> Return
```

## Happy path

```mermaid
flowchart TD
    E["Executor decides task is non-atomic"] --> Request["request_complex_task_solution(goal)"]
    Request --> C["Create ComplexTaskRequest C1"]
    C --> S1["Create TaskSegment S1"]
    S1 --> H1["Create HarnessGraph S1.H1"]
    H1 --> P["Planner submits submit_full_plan"]
    P --> G["Generator DAG completes successfully"]
    G --> V["Evaluator submits success"]
    V --> HP["HarnessGraph passes"]
    HP --> SC["TaskSegment closes successfully"]
    SC --> RC["ComplexTaskRequest closes success"]
    RC --> Report["Final complex-task report is attached to requested_by_task_id"]
```

## Retry-then-pass path

```mermaid
flowchart TD
    H1["S1.H1 runs"] --> Fail["S1.H1 fails"]
    Fail --> Budget{"Retry budget remains?"}
    Budget -->|"yes"| H2["TaskSegmentManager creates S1.H2"]
    H2 --> Fresh["S1.H2 planner submits a fresh full plan"]
    Fresh --> Pass["S1.H2 passes"]
    Pass --> Close["S1 closes successfully"]
    Budget -->|"no"| Exhaust["S1 closes failed and request fails"]
```

Retry history is horizontal inside the segment. The next planner receives the
failure landscape as context, but lifecycle state does not inherit anything from
prior failed graphs beyond ordered retry history.

## Recursive complex task request

Any generator executor can request its own complex task before it edits. That
creates a new request, not a child segment in the outer request.

```mermaid
flowchart TD
    C1["Outer ComplexTaskRequest C1"] --> S1["TaskSegment S1"]
    S1 --> H1["HarnessGraph S1.H1"]
    H1 --> E7["Generator executor E7"]
    E7 -->|"request_complex_task_solution(goal)"| C2["Nested ComplexTaskRequest C2"]
    C2 --> C2S1["C2 TaskSegment S1"]
    C2S1 --> C2H1["C2 HarnessGraph S1.H1"]
    C2H1 --> C2Close["C2 closes"]
    C2Close -->|"final report"| E7
    E7 --> OuterContinue["Outer graph consumes E7's final report"]
```

The nested request has its own segment and retry history. The outer request sees
only the close report associated with the executor that requested it.

## Tool and role boundaries

| Role | Scope | Main terminals |
| ---- | ----- | -------------- |
| Planner | One `HarnessGraph` | `submit_full_plan` |
| Generator executor | One graph DAG node | `submit_execution_success`, `submit_execution_failure`, `request_complex_task_solution` |
| Generator verifier | One graph DAG node | `submit_verification_success`, `submit_verification_failure` |
| Evaluator | Sink for one graph | `submit_evaluation_success`, `submit_evaluation_failure` |

Important gates:

- malformed planner DAG submissions fail inline without marking the graph
  failed.
- `request_complex_task_solution` is blocked after the executor has edited.
- evaluator spawn is blocked until every generator in the current graph is
  `DONE`.
- next graph creation is blocked once the segment retry budget is exhausted.

## Context engine boundary

The context engine composes structured context packets and summaries for each
role, but lifecycle decisions read structural state:

- planner context includes request goal, segment goal, and retry failure
  landscape when applicable;
- generator context includes the planned task spec and dependency summaries;
- evaluator context includes the graph task specification, evaluation criteria,
  and completed generator/verifier summaries;
- request close context includes the final complex task summary and close report
  for `requested_by_task_id`.

Generated summaries are evidence. They do not decide whether to retry or close
the request.
