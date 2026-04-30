# Phase 06 - Context Engine

## Goal

Define the context engine that sits on top of the migrated
`ComplexTaskRequest` / `TaskSegment` / `HarnessGraph` lifecycle model.

The context engine composes role-specific launch context, stores durable
summaries, and produces detailed close-report payloads. It must not own
lifecycle policy. Request creation and close belong to
`ComplexTaskRequestHandler`. Per-segment retry and segment close belong to
`TaskSegmentManager`. Planner, generator, and evaluator execution belong to
`HarnessGraphOrchestrator`.

The design target is flexible and structured enough for three layers:

- complex task request handling,
- task segment management,
- harness graph orchestration.

Partial-plan continuation is removed. The context engine does not compose
segment-chain context or propagate planner-supplied continuation goals.

## Non-goals

The context engine does not:

- decide whether a complex task request should close,
- decide whether a failed graph should retry,
- mutate `ComplexTaskRequest`, `TaskSegment`, or `HarnessGraph` lifecycle fields
  except through explicit summary/evidence writes,
- replace canonical lifecycle fields with generated summaries.

Generated summaries are derived context. They can guide agents, but lifecycle
decisions still read the structural source-of-truth fields.

## Sources of truth

Different roles need different canonical inputs. The context engine should keep
these sources distinct instead of blending them into one global prompt.

| Scope | Canonical source | Notes |
| ----- | ---------------- | ----- |
| Entry executor | user request or assigned root task input | This is the executor's direct work contract. |
| Complex task request | `ComplexTaskRequest.goal` | Created from `request_complex_task_solution(goal)` and becomes the request-level goal. |
| Task segment | `TaskSegment.goal` | Equals the complex task request goal. |
| Harness graph | `HarnessGraph.task_specification` and `HarnessGraph.evaluation_criteria` | Emitted by the planner through `submit_full_plan`. |
| Generator task | planned task specification plus dependency summaries | Generators should not need the full complex-task history unless their local task spec explicitly requires it. |
| Evaluator | graph task specification, completed task summaries, and evaluation criteria | The evaluator judges the current harness graph. |

The `goal` passed to `request_complex_task_solution(goal)` is the direct
request-level contract. Harness graph execution is governed by the planner's
`task_specification` and `evaluation_criteria`. Parent executor context can be
included as background evidence, but it must not override those canonical
contracts.

## Context engine contract

Expose a single service with typed read and write operations:

```python
class ContextEngine:
    async def build_planner_context(
        self, harness_graph_id: str
    ) -> ContextPacket: ...

    async def build_generator_context(
        self, task_id: str
    ) -> ContextPacket: ...

    async def build_evaluator_context(
        self, harness_graph_id: str
    ) -> ContextPacket: ...

    async def build_request_close_context(
        self, complex_task_request_id: str
    ) -> ContextPacket: ...

    async def record_task_summary(
        self, task_id: str, summary: TaskSummary
    ) -> None: ...

    async def record_harness_graph_summary(
        self, harness_graph_id: str
    ) -> HarnessGraphSummary: ...

    async def record_task_segment_summary(
        self, task_segment_id: str
    ) -> TaskSegmentSummary: ...

    async def record_complex_task_summary(
        self, complex_task_request_id: str
    ) -> ComplexTaskSummary: ...
```

`build_*_context` methods return structured packets. Prompt rendering should be
a downstream formatting step so tests can assert on context blocks without
parsing prose.

## Context packet model

```python
class ContextPacket(BaseModel):
    target_role: ContextRole
    target_id: str
    canonical_refs: ContextRefs
    blocks: list[ContextBlock]
    source_ids: list[str]
    token_budget: int | None = None
```

```python
class ContextBlock(BaseModel):
    kind: ContextBlockKind
    title: str
    text: str
    priority: ContextPriority
    source_id: str | None = None
    source_kind: str | None = None
```

Suggested block priorities:

| Priority | Meaning |
| -------- | ------- |
| `required` | Must be included. Canonical goals, task specs, evaluation criteria, and hard constraints. |
| `high` | Include unless impossible. Failure landscape and dependency summaries. |
| `medium` | Useful history. Parent executor background, resolver notes. |
| `low` | Optional background. Verbose evidence lists, exploratory notes, long logs. |

Suggested block kinds:

- `entry_request`,
- `complex_task_goal`,
- `segment_goal`,
- `task_specification`,
- `evaluation_criteria`,
- `planned_task_spec`,
- `dependency_summary`,
- `completed_task_summary`,
- `prior_harness_graph_summary`,
- `failed_graph_landscape`,
- `resolver_summary`,
- `artifact_reference`,
- `close_report`.

## Summary model

The context engine persists summaries at four levels.

### Task summary

Produced when a planner, executor, verifier, evaluator, resolver, advisor, or
explorer run returns useful terminal information.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `task_id` | Owning task or helper run id. |
| `role` | Planner, executor, verifier, evaluator, resolver, advisor, or explorer. |
| `outcome` | Success, failure, blocked, or informational. |
| `summary` | Human-readable result. |
| `evidence_refs` | Artifact, file, log, test, or external references. |
| `residual_risks` | Known risks or follow-ups. |
| `created_at` | Timestamp. |

Planner task summaries should include the submitted full plan and enough context
to explain the decomposition, but the canonical segment contract remains on
`HarnessGraph.task_specification` and `HarnessGraph.evaluation_criteria`.

### Harness graph summary

Produced when a harness graph closes.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `harness_graph_id` | Owning harness graph. |
| `segment_id` | Owning segment. |
| `graph_sequence_no` | 1-based graph order inside the segment. |
| `status` | Passed or failed. |
| `fail_reason` | Planner exhaustion, generator failure, evaluator failure, or null. |
| `task_specification` | Planner-emitted graph contract. |
| `evaluation_criteria` | Planner-emitted evaluation criteria. |
| `task_summaries` | Ordered summaries for generator, verifier, and evaluator tasks. |
| `failure_landscape` | Structured failed, blocked, unresolved, and skipped work. |
| `artifact_refs` | Durable evidence references. |

For failed graphs, `failure_landscape` is the most important input if
`TaskSegmentManager` retries and launches a later graph. It should distinguish
planner exhaustion, failed generators, blocked dependents, evaluator failures,
unresolved resolver calls, and independent work that still completed
successfully.

### Task segment summary

Produced when a segment closes.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `task_segment_id` | Closed segment. |
| `complex_task_request_id` | Owning request. |
| `sequence_no` | Always `1` in the no-continuation model. |
| `goal` | Segment goal. |
| `outcome` | Succeeded or failed. |
| `completed_work` | What this segment accomplished. |
| `final_harness_graph_summary_id` | Summary for the harness graph that produced the final segment outcome: the passing graph for success, or the final attempted failed graph for `attempt_plan_failed`. |
| `attempted_plan_history` | Ordered digest of every harness graph attempted in the segment. Each entry is derived from its harness graph summary and includes `task_specification`, `evaluation_criteria`, `fail_reason`, and `failure_landscape` for failed graphs. |

For `attempt_plan_failed`, `attempted_plan_history` is the primary failure
payload. It must include every attempted harness graph, not only the final
failed graph, so the requester can see what plans were tried and why each
attempt failed.

### Complex task summary

Produced when a complex task request closes.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `complex_task_request_id` | Closed request. |
| `requested_by_task_id` | Executor task receiving the final report. |
| `goal` | Request goal from `request_complex_task_solution(goal)`. |
| `outcome` | Succeeded, failed, or cancelled. |
| `final_segment_id` | Segment that produced the final outcome. |
| `final_harness_graph_id` | Graph that produced the final outcome. |
| `segment_summary` | Digest of the single segment result. |
| `final_result` | The payload returned for the requesting executor task. |
| `artifact_refs` | Final durable evidence references. |
| `residual_risks` | Follow-ups the requesting executor task must know. |

This summary powers the close packet delivered to `requested_by_task_id`.

## Role-specific context recipes

### Entry executor context

Entry executor context is the user request or assigned task input plus runtime
environment details.

It should not include complex-task summaries until a close report has been
attached for the executor task.

### Planner context

Planner context is built for one `HarnessGraph`.

Required blocks:

- `complex_task_goal`: `ComplexTaskRequest.goal`.
- `segment_goal`: current `TaskSegment.goal`.
- `failed_graph_landscape`: present when this graph follows an earlier failed
  graph in the same segment and derived from those earlier failed harness graphs.

The planner may receive parent executor background, but it must be marked as
background. It must not override the complex task request goal.

The planner of a graph launched after a `TaskSegmentManager` retry decision
submits a fresh full plan. The failure landscape is the relevant retry input.

### Generator executor context

Generator executor context is built for one planned generator task.

Required blocks:

- `planned_task_spec`: the exact task assigned by the planner.
- `task_specification`: the current graph contract, included as framing.
- `dependency_summary`: summaries of completed dependency tasks.
- `artifact_reference`: artifacts from dependencies that the generator may need.

Generators should not receive the whole request history by default. Their job is
local execution. If the planner wants a generator to account for the larger
goal, that requirement should appear in the planned task spec.

### Generator verifier context

Verifier context is built like executor context, but with verification-specific
framing:

- the planned verification task,
- the generator summary being verified,
- relevant dependency summaries,
- artifact references,
- local pass/fail expectations.

The verifier may call resolver helpers when it finds issues it cannot resolve
through read-only checks. Resolver outputs become `resolver_summary` blocks for
the verifier and later evaluator.

### Evaluator context

Evaluator context is built for one closed generator DAG inside a
`HarnessGraph`.

Required blocks:

- `task_specification`: the current graph contract.
- `evaluation_criteria`: exact pass/fail criteria.
- `completed_task_summary`: all completed generator and verifier summaries.
- `resolver_summary`: resolver outputs relevant to completed tasks.
- `artifact_reference`: final evidence and artifacts.

The evaluator should judge the current harness graph. Request-level background
can be included only when the current graph contract or evaluation criteria
explicitly depends on it.

### Request close context

When a complex task request closes, `ComplexTaskRequestHandler` asks the context
engine for a close packet for `requested_by_task_id`.

Required blocks:

- `close_report`: succeeded, failed, or cancelled.
- `complex_task_goal`: the original request goal.
- `complex_task_summary`: final result and segment digest.
- `artifact_reference`: artifacts attached to the final report.
- `residual_risks`: risks and follow-ups.

This packet becomes the final report for the executor task that requested the
complex task.

## Segment retry context rules

Retry is horizontal inside one `TaskSegment` and only follows a failed graph.
A planner launched after `TaskSegmentManager` retries receives the same request
goal and segment goal as the prior graph, plus structured failure history. It
submits a fresh full plan.

For retry context, include:

- every prior failed graph summary in graph sequence order,
- failed task summaries,
- blocked dependents,
- completed independent work that may be reused,
- evaluator failure details when applicable,
- planner exhaustion details when applicable,
- residual risks and unresolved criteria.

## Integration points

`ComplexTaskRequestHandler`:

- records request origin context when creating a request,
- asks the context engine to record a complex-task summary when the request
  closes,
- asks for `build_request_close_context` when closing a request,
- stores the final complex task summary before reporting to the requesting
  executor task.

`TaskSegmentManager`:

- asks the context engine to record a segment summary when its owned segment
  closes,
- passes segment and graph ids to context recipes rather than assembling
  prompts.

`HarnessGraphOrchestrator`:

- asks for planner context before spawning the planner,
- records planner, generator, verifier, and evaluator task summaries as tasks
  complete,
- asks for generator and evaluator context before spawning those roles,
- records the harness graph summary when the graph closes.

Tool prehooks:

- may read context packets for soft reminders,
- must enforce hard gates from structural state, role state, and conversation
  history rather than generated prose.

## Token and compression policy

The context engine should compose packets in priority order:

1. required canonical fields,
2. high-priority summaries and failure landscape,
3. medium-priority failed-graph history,
4. low-priority evidence and verbose notes.

If a packet exceeds its token budget, compress low-priority blocks first. Never
compress canonical goals, task specifications, evaluation criteria, or hard
constraints into ambiguous prose.

Evidence references should be preferred over pasted logs. Include concise
summaries inline and attach durable refs for detailed inspection.

## Persistence

The context engine needs store helpers for:

- inserting and loading task summaries,
- inserting and loading harness graph summaries,
- inserting and loading task segment summaries,
- inserting and loading complex task summaries,
- listing harness graphs by `segment_id` and `graph_sequence_no`,
- loading task dependency summaries,
- loading artifacts and evidence refs.

Summary writes should be idempotent by owner id. Re-recording a summary for the
same closed owner should update the existing summary or no-op predictably.

## Implementation tasks

1. Add typed schemas for `ContextPacket`, `ContextBlock`, context refs, and
   summary types.
2. Add summary persistence for task, harness graph, task segment, and complex
   task summaries.
3. Add context walks for request origin, failed graph history, task dependencies,
   and completed generator DAG tasks.
4. Implement `build_planner_context` for initial graph and later graph after
   failure cases.
5. Implement `build_generator_context` for executor and verifier tasks.
6. Implement `build_evaluator_context` from graph contract, evaluation criteria,
   completed task summaries, resolver summaries, and artifacts.
7. Implement `build_request_close_context` for complex-task close reports.
8. Connect `HarnessGraphOrchestrator` role launches to context packets.
9. Connect terminal submissions and helper returns to summary recording.
10. Keep prompt rendering as a separate adapter from context packet composition.
11. Add token-budget compression without changing canonical required blocks.

## Test plan

Minimum coverage:

- Planner context for initial segment includes request goal and segment goal.
- Planner context for a graph launched after failure includes failure landscape
  and prior failed graph summaries.
- Generator context includes planned task spec and dependency summaries but does
  not include full request history by default.
- Evaluator context includes task specification, evaluation criteria, and all
  completed generator/verifier summaries.
- Complex-task close context returns final complex task summary to
  `requested_by_task_id`.
- Summary recording is idempotent per owner id.
- Token compression preserves canonical goals, task specifications, evaluation
  criteria, and hard constraints.

## Phase exit criteria

- Every planner, generator, verifier, evaluator, and request-close packet can be
  built from a structured `ContextPacket`.
- Summaries exist at task, harness graph, segment, and complex task levels.
- Segment retry context surfaces the failure landscape from prior failed graphs.
- Lifecycle services still make decisions from structural state, not generated
  summaries.
