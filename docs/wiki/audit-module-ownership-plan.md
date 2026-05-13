# Audit Module Ownership Plan

## Goal

Move audit facts to the modules that own the behavior, while keeping
`live_e2e.audit` as the collector, assertion layer, and artifact writer.

The current live E2E audit path mixes several responsibilities:

- TaskCenter lifecycle and graph state are inferred from SQLAlchemy listeners.
- Engine/tool execution facts are translated from stream events.
- Sandbox/OCC/overlay/layer-stack facts are inferred from tool-result metadata.
- `.sweevo_runs` artifacts are written by the same package that defines the
  event vocabulary.

The target shape separates ownership:

- `task_center.audit` owns lifecycle, graph, dependency, retry, and status facts.
- `engine.audit` owns agent and generic tool execution facts.
- `sandbox.audit` owns sandbox operation facts and subsystem timing breakdowns.
- `live_e2e.audit` collects those facts and renders test artifacts.

## Target Module Layout

```text
backend/src/audit/
  base.py
  bus.py

backend/src/task_center/audit/
  __init__.py
  events.py
  emitter.py

backend/src/engine/audit/
  __init__.py
  events.py
  stream.py

backend/src/sandbox/audit/
  __init__.py
  events.py
  operation.py
  translation.py

backend/src/live_e2e/audit/
  collector.py
  recorder.py
  metrics.py
  legacy.py
```

## Shared Base Types

`backend/src/audit/base.py` should be dependency-light and safe for
`task_center`, `engine`, `sandbox`, and `live_e2e` to import.

```python
from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import Any, Literal, Protocol

JsonValue = Any
AuditSource = Literal["task_center", "engine", "sandbox", "live_e2e"]


@dataclass(frozen=True, slots=True)
class AuditNode:
    task_center_run_id: str | None = None
    request_id: str | None = None
    mission_id: str | None = None
    episode_id: str | None = None
    attempt_id: str | None = None
    task_center_task_id: str | None = None
    agent_name: str | None = None
    agent_run_id: str | None = None
    sandbox_id: str | None = None
    tool_name: str | None = None
    tool_id: str | None = None


@dataclass(frozen=True, slots=True)
class AuditEvent:
    source: AuditSource
    type: str
    node: AuditNode
    payload: Mapping[str, JsonValue] = field(default_factory=dict)
    correlation_id: str | None = None
    ts: datetime = field(default_factory=lambda: datetime.now(UTC))


class AuditSink(Protocol):
    def publish(self, event: AuditEvent) -> None: ...


class NoopAuditSink:
    def publish(self, event: AuditEvent) -> None:
        return None
```

`backend/src/audit/bus.py` can provide the in-memory implementation used by the
live E2E runner:

```python
from collections.abc import Callable

from audit.base import AuditEvent, AuditSink


class AuditEventBus(AuditSink):
    def publish(self, event: AuditEvent) -> None: ...
    def subscribe(self, handler: Callable[[AuditEvent], None]) -> Callable[[], None]: ...
```

Production emitters should accept `AuditSink` and should not import
`live_e2e.audit`. Collectors and test hooks can subscribe to the shared bus.

Use namespaced string event types instead of one global enum. Each owner module
keeps its own event constants.

Examples:

```text
task_center.run.started
task_center.attempt.failed
engine.tool.completed
sandbox.occ.committed
sandbox.layer_stack.auto_squashed
```

## Correlation Contract

`AuditNode` is the common correlation envelope. Producers should populate every
field they already know and leave unknown fields as `None`; collectors must not
guess missing identifiers from unrelated payload text.

Correlation source of truth:

- TaskCenter emits lifecycle events with `task_center_run_id`, `request_id`,
  `mission_id`, `episode_id`, `attempt_id`, and `task_center_task_id`.
- Engine emits agent/tool events from `ExecutionMetadata`, including
  `sandbox_id`, `agent_run_id`, `agent_name`, and `tool_id`.
- Sandbox emits operation events from public sandbox API boundaries. Before
  Phase 2 emits from those boundaries, extend `SandboxCaller` or add an adjacent
  `SandboxAuditContext` so sandbox APIs receive the TaskCenter and engine
  correlation fields already present in `ExecutionMetadata`.
- Live E2E direct probes and squad scripts that call `sandbox.api` directly must
  pass the same audit context when they want per-task assertions.

The sandbox daemon, provider adapters, OCC internals, overlay internals, and
layer-stack internals should not import `audit`. They may return domain result
objects and timing dictionaries; the public sandbox API wrapper translates those
results into `sandbox.audit` events.

## Sandbox Audit

### Current Facts Already Available

Sandbox public operation results already carry `timings: dict[str, float]`.
The dedicated tools pass those timings into `ToolResult.metadata`.

Covered operations today:

- `read_file`
- `write_file`
- `edit_file`
- `shell`
- plugin operations that return timing metadata

Current timing families already include:

- `api.read.*`, `api.write.*`, `api.edit.*`, `api.shell.*`
- `occ.prepare.*`
- `occ.commit.*`
- `occ.apply.*`
- `overlay.*`
- `command_exec.*`
- `layer_stack.*`
- `workspace_base.*`

Important current subsystem stats:

- OCC prepare, route, base-hash, commit, publish, queue wait, resume wait.
- OCC gated/direct path counts and per-path aggregate timings.
- Overlay mount, command run, capture, invoker, and total timings.
- Command exec mount/run/capture/apply/release timings.
- Layer-stack materialization, transaction lock wait/held, publish, and
  auto-squash timings.
- Auto-squash depth before/after, max depth, race/recheck/backpressure flags.

### Target Events

`sandbox.audit` should emit one operation event for each public sandbox
operation, plus optional derived subsystem events for high-value boundaries.

```text
sandbox.operation.started
sandbox.operation.completed
sandbox.operation.failed
sandbox.operation.conflicted

sandbox.occ.prepared
sandbox.occ.committed
sandbox.occ.conflicted

sandbox.overlay.executed

sandbox.layer_stack.lease_acquired
sandbox.layer_stack.layer_published
sandbox.layer_stack.auto_squashed
```

### Emission Boundary

Public sandbox API wrappers emit `sandbox.operation.started` immediately before
the daemon/provider call and emit one terminal operation event after converting
the domain result:

- `sandbox.operation.completed` for successful operations.
- `sandbox.operation.conflicted` for guarded-operation conflicts.
- `sandbox.operation.failed` for provider, daemon, validation, or unexpected
  operation failures.

Subsystem events such as `sandbox.occ.committed` and
`sandbox.layer_stack.auto_squashed` are derived once from the sandbox result
object and its raw timing dictionary. They should no longer be independently
derived from engine stream-event metadata after Phase 2 starts emitting
`sandbox.audit` events.

### Operation Payload

Every sandbox operation event should include:

```text
operation: read_file | write_file | edit_file | shell | raw_exec | plugin
status: ok | error | conflict
changed_paths
conflict_reason
warnings
timings
```

`timings` remains the raw, detailed timing dictionary. The collector derives
aggregates from it.

Example payload:

```json
{
  "operation": "edit_file",
  "status": "conflict",
  "changed_paths": ["src/foo.py"],
  "conflict_reason": "anchor not found",
  "timings": {
    "api.edit.lease_acquire_s": 0.012,
    "occ.prepare.total_s": 0.004,
    "occ.apply.commit_queue_wait_s": 0.001,
    "occ.apply.commit_resume_wait_s": 0.0,
    "occ.apply.total_s": 0.021
  }
}
```

### Ownership Rule

Sandbox should emit per-operation facts. It should not aggregate scenario-level
performance reports. Aggregation belongs to `live_e2e.audit` or a benchmark
reporter.

## Engine and Tool Audit

`engine.audit` owns generic agent and tool execution facts. Tool audit should be
framed as an operation lifecycle with timing, policy, validation, and result
shape, not just as "a stream event happened." It should not know about OCC,
layer-stack, or TaskCenter policy.

### Current Facts Already Available

The stream path already exposes:

- tool call started
- tool call completed
- tool call error
- `tool_name`
- `tool_id`
- `tool_input`
- output
- `is_error`
- `does_terminate`
- full tool metadata
- agent name and run id

The tool execution framework also has explicit internal stages:

- budget check
- tool lookup
- input parsing and validation
- pre-hook execution
- tool body execution
- output validation
- post-hook execution
- result finalization
- terminal-tool marking

`ExecutionMetadata` already carries correlation fields:

- `sandbox_id`
- `agent_run_id`
- `agent_name`
- `task_center_run_id`
- `task_center_task_id`
- `task_center_attempt_id`
- `task_center_mission_id`
- `task_center_request_id`
- `tool_id`

### Target Events

```text
engine.agent.started
engine.agent.completed
engine.agent.failed

engine.tool.requested
engine.tool.started
engine.tool.rejected
engine.tool.completed
engine.tool.failed
```

`engine.tool.requested` is emitted when the model/tool-use block asks for a
tool. `engine.tool.started` is emitted after the tool exists, input validates,
and pre-hooks allow execution. `engine.tool.rejected` is used for budget,
unknown-tool, validation, and pre-hook rejection paths where the tool body never
runs. `engine.tool.completed` and `engine.tool.failed` cover body execution
outcomes.

### Tool Payload

```text
tool_name
tool_id
status: ok | error | rejected
rejection_reason
error_kind
input_shape
input_redacted
input_digest
input_bytes
output_shape
output_digest
output_bytes
is_error
does_terminate
is_terminal_tool
metadata
timings
```

The generic tool audit envelope should include full correlation through
`AuditNode`. Domain-specific metadata stays in the producing domain. For example,
sandbox timing metadata stays under `sandbox.audit`; LSP-specific details can be
owned by plugin/LSP audit later.

### Tool Timings

Tool timing keys should be normalized by `engine.audit`. Domain timing
dictionaries may be copied through as opaque metadata during the transition, but
the authoritative source for sandbox/OCC/overlay/layer-stack timings is
`sandbox.audit`.

```text
engine.tool.total_s
engine.tool.budget_check_s
engine.tool.lookup_s
engine.tool.input_validation_s
engine.tool.pre_hooks_s
engine.tool.body_s
engine.tool.output_validation_s
engine.tool.post_hooks_s
engine.tool.finalize_s
engine.tool.emit_started_s
engine.tool.emit_completed_s
```

The raw `metadata.timings` from a tool should be preserved under
`metadata.domain_timings` when useful for compatibility. It should not be mixed
into the normalized `engine.tool.*` timing namespace:

```json
{
  "tool_name": "edit_file",
  "tool_id": "toolu_123",
  "status": "error",
  "error_kind": "tool_result_error",
  "is_error": true,
  "does_terminate": false,
  "input_shape": {"file_path": "str", "old_text": "str", "new_text": "str"},
  "input_digest": "sha256:...",
  "output_shape": {"status": "str", "changed_paths": "list", "conflict_reason": "str"},
  "output_digest": "sha256:...",
  "timings": {
    "engine.tool.total_s": 0.044,
    "engine.tool.pre_hooks_s": 0.002,
    "engine.tool.body_s": 0.039,
    "engine.tool.output_validation_s": 0.001
  },
  "metadata": {
    "domain_timings": {
      "api.edit.total_s": 0.036,
      "occ.apply.total_s": 0.021
    }
  }
}
```

### Tool Error Framing

Tool audit should classify failures by where they happened:

```text
budget_rejected
unknown_tool
input_validation_failed
pre_hook_rejected
tool_body_error
tool_result_error
output_validation_failed
post_hook_error
terminal_tool_completed
```

This gives the collector enough information to answer whether a scenario failed
because the agent selected a bad tool, a guardrail rejected the call, the tool
body failed, or a domain operation such as sandbox/OCC rejected the mutation.

### Metrics Derived by Collector

`live_e2e.audit` can derive:

- calls by tool
- errors by tool
- rejections by reason
- p50/p95/max/total duration by tool
- p50/p95/max/total duration by tool stage
- body time versus hook/validation overhead
- terminating tool calls
- output/error distribution
- tool sequence per agent run
- largest inputs and outputs by byte size
- domain timing rollups from preserved metadata, such as sandbox/OCC timings

## TaskCenter Audit

`task_center.audit` owns lifecycle, graph, dependency, retry, and status
transition facts. These should be emitted directly at mutation points, not
reconstructed from SQLAlchemy listeners in `live_e2e`.

### Current Facts Already Available

TaskCenter persistence already records:

- request id, cwd, sandbox id, request prompt
- run id, run status, started/finished timestamps
- task id, run id, role, agent name, task input, task status
- task summaries
- task dependencies
- attempt id
- context packet id
- system/user prompts
- fix target and spawn reason

Mission, episode, and attempt stores carry the higher-level lifecycle state used
by `live_e2e.runner` to reconstruct graph summaries.

### Target Events

```text
task_center.run.started
task_center.run.completed
task_center.run.failed

task_center.entry_task.created
task_center.entry_task.launched
task_center.entry_task.completed
task_center.entry_task.failed

task_center.mission.requested
task_center.mission.started
task_center.mission.completed
task_center.mission.failed

task_center.episode.started
task_center.episode.continued
task_center.episode.completed
task_center.episode.failed

task_center.attempt.started
task_center.attempt.passed
task_center.attempt.failed

task_center.task.created
task_center.task.ready
task_center.task.launched
task_center.task.completed
task_center.task.failed
task_center.task.blocked
```

`task_center.task.ready` is a dispatcher fact, not a persisted task status. It is
emitted after dependency readiness calculation finds a pending task schedulable
and before the task is marked `running` / `launched`. The payload should include
the satisfied dependency ids and should keep `status_from` and `status_to` as
`pending` when no store status changed.

### TaskCenter Payloads

Common fields:

```text
run_id
request_id
mission_id
episode_id
attempt_id
task_center_task_id
requested_by_task_id
role
agent_name
status_from
status_to
needs
fail_reason
summary
outcome
context_packet_id
attempt_sequence_no
episode_sequence_no
```

### Stats Derived by Collector

`live_e2e.audit` can derive:

- total run duration
- mission, episode, attempt, and task durations
- attempt retry count
- failed attempt count and fail reasons
- task counts by role and status
- DAG breadth/depth
- dependency wait time
- pending to running to done time
- planner/executor/verifier/evaluator counts
- context packet and prompt sizes, if TaskCenter emits composition facts

## live_e2e Audit Collector

`live_e2e.audit` should become a downstream collector. It should not own the
source-of-truth event vocabulary for TaskCenter, engine, or sandbox.

`live_e2e.audit` may own only live-harness facts:

```text
live_e2e.scenario.started
live_e2e.scenario.completed
live_e2e.scenario.failed
live_e2e.hook.asserted
live_e2e.hook.injected_failure
live_e2e.artifact.write_failed
```

Those events support scenario assertions and artifact diagnostics. They must not
stand in for domain lifecycle, tool, or sandbox facts.

Responsibilities:

- Subscribe to `AuditEvent`s through the shared audit bus.
- Preserve event order.
- Feed hooks and expected-event assertions.
- Write `.sweevo_runs/scenario_logs/...`.
- Write `run.json`, `metrics.json`, `message.jsonl`, and `sandbox_events.jsonl`.
- Derive per-tool, per-role, TaskCenter lifecycle, and sandbox subsystem
  summaries.
- Fail scenarios when audit collection or artifact writing fails.

It may keep a temporary compatibility layer:

```text
task_center.task.launched(role=planner) -> planner_invoked
engine.tool.completed               -> tool_call_completed
sandbox.occ.committed               -> sandbox_occ_changes_committed
sandbox.layer_stack.auto_squashed   -> sandbox_layer_stack_layers_squashed
```

That layer should be the only producer of legacy event names after the matching
domain audit events exist. For sandbox events specifically, disable the current
`live_e2e.audit.sandbox_events` metadata derivation when `sandbox.audit` events
are active so `sandbox_events.jsonl` and scenario counters cannot double-count.
The compatibility layer should disappear after scenario `expected_event_sequence`
values move to namespaced event types.

## Migration Phases

### Phase 1: Shared Base Types

- Add `backend/src/audit/base.py`.
- Add `backend/src/audit/bus.py`.
- Add `AuditSink` plumbing with `NoopAuditSink` and shared bus subscription.
- Do not change runtime behavior yet.
- Add import-boundary tests to ensure `audit.base` does not depend on
  TaskCenter, engine, sandbox, or live_e2e.
- Add import-boundary tests to ensure `audit.bus` depends only on `audit.base`
  and standard-library types.

### Phase 2: Sandbox Audit Extraction

- Move sandbox event derivation out of `live_e2e.audit`.
- Add `sandbox.audit.operation` and `sandbox.audit.translation`.
- Extend `SandboxCaller` or add `SandboxAuditContext` so public sandbox APIs
  receive TaskCenter and engine correlation fields from `ExecutionMetadata`.
- Emit per-operation sandbox events from public sandbox API result points.
- Derive subsystem events exactly once from sandbox result objects and timing
  dictionaries.
- Preserve current tool metadata and `.sweevo_runs` output.
- Keep live_e2e compatibility mapping during the transition, but make it map
  `sandbox.audit` namespaced events to legacy names rather than deriving a second
  event stream from tool-result metadata.
- Add an exactly-once compatibility test for `sandbox_events.jsonl`.

### Phase 3: Engine Tool Audit

- Add `engine.audit.stream` for stream event to `AuditEvent` translation.
- Emit `engine.tool.started/completed/failed`.
- Populate `AuditNode` from `ExecutionMetadata` and stamped stream-event fields.
- Preserve domain timing metadata as opaque compatibility metadata; do not make
  engine audit the source of truth for sandbox subsystem timing.
- Emit `engine.agent.started/completed/failed` around agent runs.
- Keep `on_agent_event` for UI streaming; audit is a structured side channel.

### Phase 4: TaskCenter Lifecycle Audit

- Thread `audit_sink` through TaskCenter entry coordinator and `AttemptRuntime`.
- Emit lifecycle facts after successful store mutations.
- Emit `task_center.task.ready` from the dispatcher after dependency readiness
  calculation and before launch/status mutation.
- Start with shadow-mode emission while SQLAlchemy listener recording remains.
- Compare emitted events against the current reconstructed graph summary.

### Phase 5: Collector Cutover

- Replace SQLAlchemy listener dependency in `live_e2e.audit.recorder`.
- Build artifacts from collected audit events plus explicit store lookups.
- Publish `live_e2e.*` scenario, hook, and artifact diagnostic events from the
  live harness only.
- Make collector errors fail the live E2E scenario.
- Keep artifact paths stable.

### Phase 6: Remove Legacy Event Vocabulary

- Migrate scenarios to namespaced event types.
- Delete legacy `EventType` mapping once tests are green.
- Delete `live_e2e.audit.sandbox_events` metadata derivation after all
  compatibility consumers read the namespaced sandbox events.
- Keep `sandbox_events.jsonl` for compatibility, but populate it from
  `sandbox.audit` events.

## Verification Plan

Focused tests:

- `audit.base` import fence and serialization tests.
- `audit.bus` subscription, handler error capture, and import-fence tests.
- correlation tests proving sandbox tool calls carry TaskCenter and engine ids
  from `ExecutionMetadata` into `sandbox.audit` events.
- `sandbox.audit` tests for read/write/edit/shell success, error, conflict, and
  timing preservation.
- `sandbox.audit` compatibility tests proving legacy sandbox events are produced
  once and only once.
- `engine.audit` tests for tool started/completed/failed events and duration
  calculation.
- `task_center.audit` tests for run, mission, episode, attempt, and task
  lifecycle event order, including dispatcher-owned `task.ready`.
- `live_e2e.audit` collector tests for metrics and artifact compatibility.
- `live_e2e.audit` tests for `live_e2e.*` scenario/hook/artifact events without
  treating them as TaskCenter, engine, or sandbox facts.

Live checks:

- Run focused pipeline scenarios to validate TaskCenter lifecycle event order.
- Run sandbox scenarios to validate OCC/overlay/layer-stack events.
- Run capacity scenario to validate cross-domain collection.
- Inspect `.sweevo_runs/scenario_logs/.../metrics.json`,
  `sandbox_events.jsonl`, and per-task `message.jsonl`.

## Final Ownership Rule

```text
task_center.audit:
  lifecycle, graph, dependency, retry, and status transition facts

engine.audit:
  agent execution and generic tool-call envelopes

sandbox.audit:
  sandbox operation facts and OCC/overlay/layer-stack timing breakdowns

live_e2e.audit:
  collection, assertions, aggregation, artifact writing, and live_e2e.*
  scenario/hook/artifact diagnostics
```
