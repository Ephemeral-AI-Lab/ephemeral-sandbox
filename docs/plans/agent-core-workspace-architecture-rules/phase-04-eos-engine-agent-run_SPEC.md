# Phase 04 - eos-engine and eos-agent-run Spec

Status: Implementation complete
Date: 2026-06-09
Owner: eos-engine / eos-agent-run

Revision 2026-06-09 (naming convention pass): aligned the target vocabulary with
Phase 03B and the current agent-core naming rules. The loop-launch contract lives
in `eos-types::agent_loop`, not an internal `*ports` crate; record writes consume
`AgentRunRecordTarget`; run creation uses `spawn.rs` / `spawn_agent`
vocabulary; provider streaming uses `ProviderStreamSource`; event observation
uses `EngineEventSink`; printing uses `EngineEventPrinter`; and the names-to-avoid
table gives concrete replacements for stale `*Service`, callback, hook, and
dependency-bag names.

Revision 2026-06-09 (completion contract pass): renamed the engine-to-run
lifecycle handoff to `StartedAgentLoop::completion` / `AgentLoopCompletion`.
The architecture must not expose channel implementation names such as `oneshot`
or receiver-shaped field names. Engine loop completion, active-run finalization,
and caller wait/poll publication are separate lifecycle steps.

Revision 2026-06-09 (closeout naming pass): removed stale `inner`,
`BackgroundSessionInputs`, `metadata_reader`, `event_sink`, and `record_writer`
targets from the spec. The target vocabulary is `terminal_outcome`,
`BackgroundSessionRuntimeFactory`, `execution_metadata_reader`,
`live_event_sink`, `run_record_writer`, and `EngineEventOutputs`.

## Scope

This phase makes `eos-engine` execution-only and `eos-agent-run` lifecycle-only.

The engine keeps the agent loop, turn execution, event emission, engine event
printing, records, and background accounting. The run crate keeps
spawn/wait/poll/cancel/finalization and durable agent-run state updates.

`eos-agent-run` must not depend directly on `eos-engine`. The shared loop launch
contract lives in `eos-types::agent_loop`; `eos-engine` implements that contract
with a concrete launcher, and backend composition wires the concrete launcher
into `AgentRunService` before handing the run service to
`eos-agent-core-server::AgentCoreService`. Do not create a new internal port
crate for this contract; `eos-sandbox-port` remains the explicit port-crate
exception.

Prerequisite: Phase 03B must define and implement the durable
request/task/workflow/agent-run lineage contract before this phase moves
record writing into `eos-engine` or splits run lifecycle from loop
execution. Phase 04 consumes `AgentRunRecordIndex` and
`AgentRunRecordTarget`; it does not redesign DB materialization.

## Local Architecture

### eos-engine

`eos-engine` owns:

- full agent loop execution,
- assistant turn execution,
- provider stream consumption,
- batch tool dispatch,
- concrete `AgentLoopLauncher` implementation,
- engine events,
- engine event printing,
- record writing for loop-visible events,
- background session accounting and notifications.

`eos-engine` does not own:

- concrete tool families,
- tool registry definitions,
- loop launch contract traits or DTOs consumed by `eos-agent-run`,
- agent-run lifecycle rows,
- request runtime wiring,
- external API facades.

### eos-agent-run

`eos-agent-run` owns:

- starting an agent run,
- process-local active-run map,
- waiting for run completion,
- polling run completion,
- cancellation,
- final lifecycle handoff from engine outcome,
- agent-run persistence updates.

`eos-agent-run` does not own:

- engine turn execution,
- direct `eos-engine` imports,
- tool behavior,
- model-facing `ToolResult` rendering,
- message event interpretation,
- request runtime wiring.

## Dependency Shape

The target dependency shape for this phase is:

```text
eos-agent-run   -> eos-types
eos-engine      -> eos-types, eos-tool, eos-llm-client, eos-sandbox-port
eos-agent-core-server -> eos-agent-run, eos-sandbox-port, eos-types
```

`eos-agent-run` consumes `dyn AgentLoopLauncher`; it does not name
`TokioAgentLoopLauncher`, `AgentLoopExecutor`, or any other concrete engine type.
Backend composition constructs the concrete engine launcher, passes it into
`AgentRunService`, and gives that service to `eos-agent-core-server`.

## Diff Table

| Area | Current | Target |
| --- | --- | --- |
| run service file | `agent_run_service.rs` owns spawn, wait, poll, cancel, completion forwarding, finalization | split into `service.rs`, `spawn.rs`, `completion.rs`, `cancellation.rs`, `persistence.rs` |
| loop request file | `agent_loop_request.rs` | move spawn-to-loop mapping into `spawn.rs` |
| persistence file | `agent_run_persistence.rs` | `persistence.rs` |
| service field | `agent_loop_launcher` | `loop_launcher` |
| runtime hooks | `runtime_state_recorder`, `runtime_state_remover` | removed; durable state transitions stay inside stores and `AgentRunService` |
| active handle type | `ActiveAgentRun` | `ActiveAgentRunHandle` |
| active handle field | no `agent_run_id` | add `agent_run_id` |
| active handle field | `cancel_handle` | `loop_cancellation` |
| active handle field | `outcome_tx` | `completion_tx` |
| completion output | `Option<AgentLoopOutcome>` | `AgentLoopOutcome` |
| completion field | `inner` | `terminal_outcome` |
| record writer type | `AgentRecordWriter` | `AgentRunRecordWriter` |
| record error name | `MessageRecordError` | `AgentRunRecordError` |
| live event field | `event_sink` | `live_event_sink` |
| durable record field | `record_writer` | `run_record_writer` |
| merged output object | separate `event_sink`, `record_writer`, optional printer | `event_outputs: EngineEventOutputs` |
| background dependency bundle | `BackgroundSessionInputs` | `BackgroundSessionRuntimeFactory` |
| metadata reader field | `metadata_reader` | `execution_metadata_reader` |
| hook stores | `ToolCallHookStores` | keep; rename only if it stops being tool-call hook state |
| engine executor file | `agent_loop/agent_loop_executor.rs` | `agent_loop/executor.rs` |
| engine state file | `agent_loop/agent_loop_state.rs` | `agent_loop/state.rs` |
| tool execution file | `tool_call/execution.rs` | `tool_call/execute.rs` |

## Resulting File Structure

Phase 04 optimizes for the cleanest end state instead of the smallest
rename/move. This target keeps provider-stream concerns, engine events,
tool-call scheduling, records, and background lifecycle accounting as separate
ownership groups.

```text
agent-core/crates/eos-engine/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── agent_loop.rs
│   ├── agent_loop/
│   │   ├── contracts.rs
│   │   ├── launcher.rs
│   │   ├── executor.rs
│   │   └── state.rs
│   ├── provider_stream.rs
│   ├── provider_stream/
│   │   ├── source.rs
│   │   └── messages.rs
│   ├── tool_call.rs
│   ├── tool_call/
│   │   ├── batch.rs
│   │   ├── execute.rs
│   │   └── hooks/
│   ├── event.rs
│   ├── event/
│   │   ├── event.rs
│   │   ├── sink.rs
│   │   ├── printer.rs
│   │   └── outputs.rs
│   ├── records.rs
│   ├── records/
│   │   ├── error.rs
│   │   ├── handle.rs
│   │   ├── io.rs
│   │   ├── kind.rs
│   │   ├── layout.rs
│   │   ├── record.rs
│   │   ├── writer.rs
│   ├── background.rs
│   └── background/
│       ├── session_runtime.rs
│       ├── command_session.rs
│       ├── subagent_session.rs
│       ├── workflow_session.rs
│       └── notification.rs
└── tests/
    ├── agent_loop/
    ├── provider_stream/
    ├── tool_call/
    ├── event/
    ├── records/
    └── background/
```

```text
agent-core/crates/eos-agent-run/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── service.rs
│   ├── spawn.rs
│   ├── active_agent_runs.rs
│   ├── completion.rs
│   ├── cancellation.rs
│   └── persistence.rs
```

Target struct field shape:

```rust
pub struct AgentRunService {
    agent_registry: Arc<AgentRegistry>,
    agent_run_store: Arc<dyn AgentRunStore>,
    task_agent_run_store: Arc<dyn TaskAgentRunStore>,
    loop_launcher: Arc<dyn AgentLoopLauncher>,
    active_agent_runs: ActiveAgentRunRegistry,
}

struct ActiveAgentRunHandle {
    agent_run_id: AgentRunId,
    loop_cancellation: AgentLoopCancellationHandle,
    completion_tx: watch::Sender<Option<AgentRunOutcome>>,
}

pub struct TokioAgentLoopLauncher {
    provider_stream_source: AgentLoopProviderStream,
    tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
    execution_metadata_reader: Arc<dyn ToolExecutionMetadataReader>,
    background_sessions: Option<BackgroundSessionRuntimeFactory>,
    hook_stores: Option<ToolCallHookStores>,
    event_outputs: EngineEventOutputs,
}

pub struct EngineEventOutputs {
    live_event_sink: Option<EngineEventSink>,
    event_printer: Option<EngineEventPrinter>,
    run_record_writer: Option<AgentRunRecordWriter>,
}
```

Target naming rules:

| Existing or weaker name | Target name | Reason |
| --- | --- | --- |
| `query/` for provider streaming | `provider_stream/` | names the model-stream boundary directly |
| `tool_dispatch/` | `tool_call/` | matches model-visible tool-call vocabulary |
| `events.rs` plus `printer.rs` | `event/{event,sink,printer,outputs}.rs` | keeps event data, live observation, rendering, and output fan-out separate |
| `agent_loop_request.rs` | `spawn.rs` | run crate maps spawn input into `StartAgentLoopRequest` at the spawn boundary |
| `BackgroundManagers` | `BackgroundSessionRuntime` | aggregate root is session lifecycle accounting, not a bag of managers |
| `BackgroundSessionInputs` | `BackgroundSessionRuntimeFactory` | runtime composition factory, not an opaque input bag |
| `ActiveAgentRuns` | `ActiveAgentRunRegistry` | registry owns active-run wait/cancel/publication invariants |
| `agent_run_service` field for `dyn AgentRunApi` | `agent_run_api` | names the trait contract rather than a concrete service |

## File Ownership Contract

The target is ownership-first, not module-count-first. Use a folder when a
concept has multiple cohesive implementation files. If a file needs another
responsibility, Phase 04 must be amended before implementation spreads that
logic.

### eos-engine files

| File | Owns | Must not own |
| --- | --- | --- |
| `lib.rs` | narrow public exports | implementation logic or compatibility re-export maze |
| `support/error.rs` | engine error type and conversions | lifecycle persistence or tool-family errors |
| `event.rs` | event module routing and narrow exports | loop execution or record persistence |
| `event/event.rs` | engine event enum, event severity, and event sink input shape | printing, persistence, run finalization |
| `event/sink.rs` | `EngineEventSink` and event observation delivery | durable finalization or record layout |
| `event/printer.rs` | engine event printing behavior | durable record writes |
| `event/outputs.rs` | `EngineEventOutputs` fan-out across live sink, printer, and durable writer | run finalization or event data definitions |
| `agent_loop.rs` | loop module routing and public loop-internal exports | full loop implementation |
| `agent_loop/contracts.rs` | engine composition contracts for tool registry, metadata, hooks, and background factory | lifecycle launch DTOs owned by `eos-types` |
| `agent_loop/launcher.rs` | concrete `AgentLoopLauncher` implementation and Tokio task launch | run spawning, wait/poll/cancel, durable finalization |
| `agent_loop/executor.rs` | full loop state machine, provider stream consumption, loop exit decisions | run lifecycle persistence |
| `agent_loop/state.rs` | in-memory state for one active loop | DB writes, active-run registry |
| `provider_stream.rs` | provider-stream module routing | tool dispatch, records, or lifecycle finalization |
| `provider_stream/source.rs` | provider stream source and factory contracts | completion, wait/poll notification, or run persistence |
| `provider_stream/messages.rs` | provider request/message normalization | tool execution or record writing |
| `tool_call.rs` | engine-side tool-call routing | concrete tool families, tool registry definitions |
| `tool_call/batch.rs` | batch rejection and bounded fan-out/fan-in policy | one-tool execution internals |
| `tool_call/execute.rs` | one registered-tool execution glue | tool registry construction |
| `tool_call/hooks/` | engine-owned pre-tool policy helpers | concrete tool family behavior or run lifecycle persistence |
| `records.rs` | record module routing and engine-local record exports | final agent-run state transitions |
| `records/writer.rs` | loop-visible record writes against a resolved record target | DB lineage lookup or run finalization |
| `records/handle.rs` | append/read handle for one resolved record node | DB lineage lookup or final run status |
| `records/record.rs` | stable record row DTOs and byte-range return values | event printing or lifecycle persistence |
| `background.rs` | background module routing and aggregate exports | concrete family protocol details |
| `background/session_runtime.rs` | `BackgroundSessionRuntime` aggregate, cross-family counts, cancel, list, and completion polling | concrete family-specific protocol details |
| `background/command_session.rs` | command-session registration, active IDs, counts, cancel, completion polling | workflow/subagent behavior |
| `background/subagent_session.rs` | subagent registration, active IDs, counts, cancel, completion polling | command/workflow behavior |
| `background/workflow_session.rs` | workflow registration, active IDs, counts, cancel, completion polling | command/subagent behavior |
| `background/notification.rs` | background completion event rendering and enqueueing | session storage or polling |

The only background-session vocabulary in this phase is the flat
`background/{session_runtime,command_session,subagent_session,workflow_session}.rs`
layout above. Do not reintroduce nested `session_managers/<kind>/...` folders or
generic `lane`, `recorder`, `driver`, or internal `*_port` names.

### eos-agent-run files

| File | Owns | Must not own |
| --- | --- | --- |
| `lib.rs` | narrow lifecycle exports | engine or tool implementation exports |
| `service.rs` | `AgentRunService`, lifecycle orchestration, active-run registry ownership | turn execution or concrete engine types |
| `spawn.rs` | `spawn_agent` orchestration, request validation, launch input mapping, and task-agent-run creation handoff | provider streaming |
| `active_agent_runs.rs` | `ActiveAgentRunRegistry`, process-local `ActiveAgentRunHandle`, wait subscriptions, cancellation lookup, and final publication | durable DB state or engine execution |
| `persistence.rs` | durable run state transitions | engine event interpretation |
| `completion.rs` | exactly-once engine outcome handoff and final-state mapping | event-by-event loop handling |
| `cancellation.rs` | run cancellation orchestration | concrete tool or sandbox family behavior |

Target `AgentRunService` field shape:

```rust
pub struct AgentRunService {
    agent_registry: Arc<AgentRegistry>,
    agent_run_store: Arc<dyn AgentRunStore>,
    task_agent_run_store: Arc<dyn TaskAgentRunStore>,
    loop_launcher: Arc<dyn AgentLoopLauncher>,
    active_agent_runs: ActiveAgentRunRegistry,
}
```

Use `agent_registry` / `AgentRegistry` vocabulary, not `agent_catalog`. The
target type comes from the Phase 02 agent-definition disposition; the concrete
registry DTO now lives in `eos-types`, so Phase 04 must not recreate an
agent-definition crate edge.

Target active-run handle value:

```rust
struct ActiveAgentRunHandle {
    agent_run_id: AgentRunId,
    loop_cancellation: AgentLoopCancellationHandle,
    completion_tx: watch::Sender<Option<AgentRunOutcome>>,
}
```

`AgentRunService` owns the active-run registry, but the map and watch-channel
mechanics stay inside `ActiveAgentRunRegistry`. Keep `agent_run_id` inside
`ActiveAgentRunHandle` even though it duplicates the map key, so the handle
remains self-identifying when moved into completion or cancellation helpers.

`ActiveAgentRunRegistry` owns:

- active-run insertion after engine startup returns `StartedAgentLoop`,
- subscription for `wait_for_agent_outcome`,
- cancellation-handle lookup/removal,
- exactly-once final outcome publication to in-process waiters.

It must not own durable run finalization, engine-loop execution, or DB fallback
polling for already-completed runs.

## Loop Launch Contract and Engine Surface

`eos-types::agent_loop` owns the shared launch contract consumed by
`eos-agent-run`:

```text
AgentLoopLauncher
StartAgentLoopRequest
StartedAgentLoop
AgentLoopCompletion
AgentLoopOutcome
AgentLoopCancellationHandle
AgentLoopCancelSignal
```

`eos-engine` implements this contract and exports only concrete engine
composition types. Its engine-owned composition contracts include:

```text
TokioAgentLoopLauncher
AgentLoopToolRegistryFactory
AgentLoopToolRegistryBuildInput
ToolExecutionMetadataReader
ExecutionMetadataBuildInput
BackgroundSessionRuntimeFactory
ToolCallHookStores
EngineEventOutputs
EngineEventSink
EngineEventPrinter
```

It must not re-export every internal engine helper.
There is no target `services.rs` file and no first-target `services/` folder;
execution internals stay in `agent_loop/`, `provider_stream/`, `tool_call/`,
`event/`, `records/`, and `background/`.

The loop module is named `agent_loop` (not `loop`): `loop` is a reserved Rust
keyword, so `mod loop;` does not compile.

Allowed exported surface:

```text
TokioAgentLoopLauncher
AgentLoopToolRegistryFactory
AgentLoopToolRegistryBuildInput
BackgroundSessionRuntimeFactory
ToolExecutionMetadataReader
ExecutionMetadataBuildInput
ToolCallHookStores
EngineEventOutputs
EngineEventSink
EngineEventPrinter
```

Contract:

| Type | Consumer | Rule |
| --- | --- | --- |
| `AgentLoopLauncher` | `eos-agent-run`, test harnesses | lives in `eos-types::agent_loop`; starts an async loop only through the lifecycle boundary |
| `StartAgentLoopRequest` | `eos-agent-run` | lives in `eos-types::agent_loop`; carries run correlation, record target, initial messages, model key, and tool/token limits |
| `StartedAgentLoop` | `eos-agent-run` | lives in `eos-types::agent_loop`; carries `completion` and the loop cancel handle |
| `AgentLoopCompletion` | `eos-agent-run` | lives in `eos-types::agent_loop`; resolves once when the engine loop publishes its terminal outcome; hides channel/future implementation details |
| `AgentLoopOutcome` | `eos-agent-run` | lives in `eos-types::agent_loop`; contains terminal status, passive submission facts, record summary, and background-session closure status |
| `TokioAgentLoopLauncher` | backend composition, tests | concrete engine implementation of `AgentLoopLauncher` |
| `BackgroundSessionRuntimeFactory` | backend composition | builds per-loop background session runtime from runtime-owned sandbox/workflow dependencies |
| `ToolExecutionMetadataReader` | backend composition | reads current runtime facts and builds per-tool execution metadata |
| `EngineEventOutputs` | backend composition, `eos-engine` loop execution | fans out non-fatal live observation, printing, and durable record writes |
| `EngineEventSink` | backend composition, tests | receives stream/tool/system events without owning finalization |

The engine may receive a run/correlation ID for records and events. It must not
own the active-run registry, spawn state, or durable lifecycle row.

Completion and event vocabulary:

| Name | Owns | Must not own |
| --- | --- | --- |
| `StartedAgentLoop::completion` | lifecycle completion signal from engine task to run service | stream/tool event delivery, active-run waiter publication, or durable finalization |
| `AgentLoopCompletion` | the awaitable/observable completion contract for one started loop | channel implementation vocabulary such as `oneshot`, receiver naming, or caller wait/poll semantics |
| `AgentLoopOutcome` | terminal loop data returned through `AgentLoopCompletion` | wait/poll publication or durable state mutation |
| `ProviderStreamSource` | provider stream input for one assistant turn | lifecycle completion or run finalization |
| `ProviderStreamSourceFactory` | choosing a `ProviderStreamSource` per loop request and agent state | run persistence or wait/poll state |
| `EngineEventOutputs` | aggregate output fan-out for one loop | lifecycle finalization or active-run waiter publication |
| `EngineEventSink` | stream/tool/system event observation during loop execution | final run-state persistence |
| `EngineEventPrinter` | rendering engine events for users/logs | durable record writes or lifecycle finalization |
| `AgentRunRecordWriter` | durable record writes to `messages.jsonl` and `events.jsonl` | live observation or final run-state persistence |

Do not use "event hook", "callback", "oneshot", or "receiver" to describe
agent-run completion. Completion is `StartedAgentLoop::completion`, a lifecycle
contract. Events are stream/tool/system observations inside engine execution.

Names to avoid:

```text
NotificationService       # engine-internal queue; target name is EngineNotificationQueue
BackgroundTeardownService # engine-internal finalizer; target name is BackgroundSessionTeardown
RecordService             # avoid for private internals; use AgentRunRecordWriter
EventPrinterService       # target name is EngineEventPrinter
EventCallback             # too generic; target name is EngineEventSink
AgentLoopHooks            # remove if no-op; if needed, use AgentLoopObserver for engine-only observation
AgentLoopBackgroundDependencies # target name is BackgroundSessionRuntimeFactory
AgentLoopHookDependencies # target name is WorkflowAncestryStores or ToolCallHookStores
```

## Execution Invariants

The engine is execution-only, but execution is not vague. The implementation
must preserve these behaviors:

| Behavior | Rule |
| --- | --- |
| provider stream | consumed inside `agent_loop/executor.rs`; stream deltas produce engine events before final outcome |
| foreground tool batch | dispatched with bounded fan-out/fan-in, not sequential execution by accident |
| terminal tool result | in-band terminal-tool errors stay non-terminal so the model can retry |
| terminal batch rejection | does not fabricate a successful terminal completion |
| event order | stream/tool/record/print events preserve loop order for a single run |
| cancellation | cancellation token is checked between stream consumption, tool dispatch, and background polling |
| background closure | terminal outcome reports whether command/subagent/workflow background sessions remain active |
| lock scope | no lock is held across provider stream await, tool execution await, or background polling await |

## Background Session Contract

`background.rs` is the routing/export surface. The aggregate root lives in
`background/session_runtime.rs`. The family session modules keep implementation details
local, but the aggregate owns cross-family policy.

| Capability | Owner | Required behavior |
| --- | --- | --- |
| register active background work | family module | records typed active ID and source family |
| count active work | `background/session_runtime.rs` | returns command/subagent/workflow counts in one snapshot |
| list active IDs | `background/session_runtime.rs` | preserves family identity; no stringly mixed ID list |
| cancel by reason | `background/session_runtime.rs` | forwards `cancel(reason)` to every family and reports partial failures |
| poll completions | `background/session_runtime.rs` | drains family completions and emits engine events |
| terminal gate | `background/session_runtime.rs` plus hooks in `eos-tool` | terminal submission/isolated-workspace gates can prove no background sessions remain |

The background runtime is allowed to depend on sandbox, workflow, and subagent
runtime handles. It must not depend on concrete tool family modules or on
`eos-agent-run` active-run internals. If it needs to spawn/wait/poll subagent
runs, it consumes `dyn AgentRunApi` from `eos-types`, never the concrete
run crate.

## Lifecycle Handoff

Completion flow:

```text
backend composition
  -> eos-agent-run::spawn_agent(request)
  -> eos-types::agent_loop::AgentLoopLauncher::start_agent_loop(request)
  -> eos-engine::TokioAgentLoopLauncher starts AgentLoopExecutor
  -> eos-engine executes stream/tool/background loop work
  -> eos-engine writes loop-visible records and prints engine events
  -> StartedAgentLoop::completion resolves to AgentLoopOutcome
  -> eos-agent-run finalizer persists final agent-run state
  -> eos-agent-run publishes AgentRunOutcome to active-run waiters
  -> caller receives AgentRunOutcome
```

There are two event-driven lifecycle paths, and they must stay separate:

```text
Engine completion path:

AgentLoopExecutor finishes
  -> StartedAgentLoop::completion resolves once with AgentLoopOutcome
  -> AgentRunService-owned finalizer consumes the completion
  -> finalizer removes ActiveAgentRunHandle from ActiveAgentRunRegistry
  -> finalizer persists durable run status
  -> finalizer publishes AgentRunOutcome to the active-run registry
```

```text
Caller wait/poll path:

wait_for_agent_outcome(agent_run_id)
  -> first checks durable terminal state through poll_agent_run_outcome
  -> if active in this process, subscribes to ActiveAgentRunRegistry
  -> waits for the finalizer to publish AgentRunOutcome
  -> returns AgentRunOutcome

poll_agent_run_outcome(agent_run_id)
  -> checks in-process active-run publication first
  -> falls back to durable terminal state
  -> never waits on engine execution directly
```

The engine does not send directly to `wait_for_agent_outcome`. The only engine
to-runner lifecycle signal is `StartedAgentLoop::completion`; runner-owned
finalization is the only path that publishes outcomes to waiters.

Handoff rules:

| Rule | Owner |
| --- | --- |
| engine produces exactly one terminal `AgentLoopOutcome` | `eos-engine` |
| run crate consumes `StartedAgentLoop::completion` and performs exactly one durable finalization | `eos-agent-run` |
| cancellation can win before, during, or after engine startup | `eos-agent-run` orchestrates; `eos-engine` observes token |
| failed engine startup creates a failed run outcome, not a dangling active run | `eos-agent-run` |
| background sessions are cancelled or reported before final state is persisted | `eos-engine` reports; `eos-agent-run` persists |
| final outcome is visible to waiters and pollers after persistence succeeds | `eos-agent-run` |
| `ActiveAgentRunHandle` is removed from the map before final publication | `eos-agent-run` |
| `wait_for_agent_outcome` subscribes to runner publication, not engine completion | `eos-agent-run` |
| engine event sinks cannot finalize or publish run outcomes | `eos-engine` / backend composition |

## Records and Engine Event Printing

Target ownership:

| Behavior | Owner |
| --- | --- |
| event emission during loop | `eos-engine` |
| engine event printing | `eos-engine/event/printer.rs` |
| record row DTOs and byte ranges | `eos-engine/records/record.rs` |
| record writing | `eos-engine/records/writer.rs` |
| durable run finalization | `eos-agent-run` |
| external record contract | `eos-types`, if externally exposed |

Reason: the engine sees stream events, tool calls, assistant messages, and
terminal transitions as they happen. The runner only sees the final outcome.

Record and print rules:

| Rule | Owner |
| --- | --- |
| every model-visible stream/tool event can be printed during execution | `event/printer.rs` |
| every durable loop-visible event is appended once through the active record handle | `records/handle.rs`, `records/writer.rs` |
| printing failure cannot corrupt loop state | `event/printer.rs` reports non-fatal sink errors |
| record write failure is an engine error and appears in `AgentLoopOutcome` | `records/writer.rs`, `agent_loop/executor.rs` |
| externally exposed record DTOs are re-exported from `eos-types` only if needed | `eos-types` |

`eos-agent-run` resolves and passes a passive `AgentRunRecordTarget` into
`StartAgentLoopRequest`. `eos-engine` writes loop-visible records against that
target. It must not derive lineage from DB state or perform final run-status
transitions while writing records.

Naming rule: target API names must not combine `Message` and `Record`. The
engine/run surface receives `AgentRunRecordTarget`; record layout classification
stays behind `AgentRunRecordIndex` / `TaskAgentRunKind` and the
`eos-types::format_record_dir` formatter. Do not pass `AgentRunRecordKind`,
`AgentRunMessageRecordKind`, `MessageRecordService`, `RecordService`,
`message_records`, or `record_kind` through the target engine/run surface. The
literal file name `messages.jsonl` is unchanged.

## Progress Tracker

| Item | Status |
| --- | --- |
| Move loop launch contract target to `eos-types::agent_loop` | Done |
| Export concrete engine launcher only from `eos-engine` | Done |
| Add exact engine file ownership contracts | Done |
| Add exact run file ownership contracts | Done |
| Define `StartedAgentLoop::completion` / `AgentLoopCompletion` as the lifecycle handoff | Done |
| Keep lifecycle completion separate from engine event sinks and caller wait/poll publication | Done |
| Rename current `EventCallback` target to `EngineEventSink` | Done |
| Remove no-op `AgentLoopHooks` or rename/load it as engine-only `AgentLoopObserver` | Done |
| Rename private active-run wrapper to `ActiveAgentRunRegistry` and keep map/watch mechanics encapsulated | Done |
| Add execution invariants for stream/tool/terminal behavior | Done |
| Add `BackgroundSessionRuntime` aggregate contract | Done |
| Move records into engine internals | Done |
| Add engine event printer/sink | Done |
| Remove concrete tool ownership from engine | Done |
| Rename private `*Service` internals where needed | Done |
| Rename `eos-agent-runner` to `eos-agent-run` | Done |
| Keep active run map in run crate | Done |
| Keep finalization persistence in run crate | Done |
| Add exactly-once completion handoff tests | Done |
| Add cancellation race tests | Done |
| Add background-session accounting tests | Done |
| Retire `eos-agent-core` runtime wiring in favor of backend composition plus `eos-agent-core-server` | Done |
| Replace stale `BackgroundSessionInputs` target with `BackgroundSessionRuntimeFactory` | Done |
| Replace stale `event_sink` / `record_writer` fields with `EngineEventOutputs` fan-out | Done |
| Replace `AgentRecordWriter` / `MessageRecordError` naming with agent-run record names | Done |
| Update `index.md` Progress Tracker with Phase 04 result and exit artifact | Done |

Latest verification:

- `cargo fmt --all`
- `cargo check -p eos-agent-run --all-targets`
- `cargo check -p eos-engine --all-targets`
- `cargo check -p eos-agent-core-server --all-targets`
- `cargo test -p eos-agent-run --all-targets`
- `cargo test -p eos-engine --all-targets`
- `cargo test -p eos-agent-core-server --all-targets`
- `cargo clippy -p eos-agent-run -p eos-engine -p eos-agent-core-server --all-targets -- -D warnings`
- `cargo test -p eos-db --all-targets`

## Acceptance Criteria

- `eos-engine` has no `tools/` concrete tool family folder.
- `eos-engine` does not own tool registry definitions or hook contracts.
- `AgentLoopLauncher`, `StartAgentLoopRequest`, and `AgentLoopOutcome` are
  consumed from `eos-types::agent_loop`, not from `eos-engine`.
- `StartedAgentLoop::completion` is the only engine-to-run lifecycle completion
  signal.
- `AgentLoopCompletion` hides the concrete channel/future implementation; target
  architecture text and public field names do not use `oneshot`, `receiver`, or
  callback vocabulary for lifecycle completion.
- `eos-engine` exports the concrete `TokioAgentLoopLauncher` and engine
  composition helpers, not a broad service facade.
- `ProviderStreamSource` is provider input only; it is not used for completion,
  finalization, or wait/poll notification.
- `EngineEventSink` is the target name for stream/tool/system events during loop
  execution;
  there is no target `EventCallback` API.
- There is no no-op target `AgentLoopHooks`; if engine-only lifecycle
  observation is needed, it is named `AgentLoopObserver` and must not be used by
  `eos-agent-run` for finalization.
- `eos-engine` has no target `services.rs` file and no first-target
  `services/` folder.
- Engine records and engine event printing work during loop execution.
- `eos-agent-run` has no normal dependency edge to `eos-engine`.
- `eos-agent-run` does not import concrete tool modules.
- `eos-agent-run` has no dependency on `eos-tool` or `ToolResult`; model-facing
  rendering happens above the lifecycle layer.
- `eos-agent-run` owns `active_agent_runs: ActiveAgentRunRegistry`; the registry
  hides the map/watch mechanics while durable finalization stays in the run
  service/finalizer.
- `eos-agent-run` owns spawn/wait/poll/cancel/finalization.
- `eos-agent-run` does not interpret stream/tool events.
- `eos-engine/src/background` keeps concrete command, workflow, and subagent
  managers under
  `background/{session_runtime,command_session,subagent_session,workflow_session}.rs`;
  there are no target `background/*_sessions.rs` files and no nested
  `session_managers/<kind>/...` folders.
- Engine completion returns to run lifecycle through
  `StartedAgentLoop::completion`; `wait_for_agent_outcome` observes only the
  runner-published `AgentRunOutcome`, not the engine loop directly.
- Engine startup failure cannot leave an active run without a terminal state.
- Engine cancellation produces one terminal outcome and one durable finalization.
- Foreground multi-tool batches are proven to execute with bounded fan-out/fan-in.
- Terminal-tool in-band errors remain retryable by the model.
- Background session counts, list, cancel, and completion polling are tested per
  family and through the aggregate.
- Midflight printing is tested separately from durable record writing.
- `cargo tree -p eos-agent-run --edges normal --depth 1` does not show
  `eos-engine` or `eos-tool`.
- `cargo tree -p eos-engine --edges normal --depth 1` does not show
  `eos-agent-run`.
- `cargo test -p eos-engine` passes.
- `cargo test -p eos-agent-run` passes.
- Focused tests cover loop outcome handoff, cancellation races, background
  accounting, records, and engine event printing.
- Final file layout follows the resulting file structure above; there is no
  standalone module-count cap for this phase.
