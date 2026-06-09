# Phase 04 - eos-engine and eos-agent-run Spec

Status: Draft
Date: 2026-06-09
Owner: eos-engine / eos-agent-run

## Scope

This phase makes `eos-engine` execution-only and `eos-agent-run` lifecycle-only.

The engine keeps the agent loop, turn execution, event emission, midflight
printing, records, and background accounting. The run crate keeps
spawn/wait/poll/cancel/finalization and durable agent-run state updates.

`eos-agent-run` must not depend directly on `eos-engine`. The shared loop launch
contract stays in `eos-agent-ports`; `eos-engine` implements that contract with a
concrete launcher, and `eos-agent-core` wires the concrete launcher into
`AgentRunService` at runtime composition.

Prerequisite: Phase 03B must define and implement the durable
request/task/workflow/agent-run lineage contract before this phase moves
record writing into `eos-engine` or splits run lifecycle from loop
execution. Phase 04 consumes `AgentRunRecordIndex` and
`AgentRunRecordTargetFile`; it does not redesign DB materialization.

## Local Architecture

### eos-engine

`eos-engine` owns:

- full agent loop execution,
- assistant turn execution,
- provider stream consumption,
- batch tool dispatch,
- concrete `AgentLoopLauncher` implementation,
- engine events,
- midflight event printing,
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
eos-agent-run   -> eos-types, eos-agent-ports
eos-engine      -> eos-types, eos-agent-ports, eos-tool-ports,
                   eos-llm-client, eos-sandbox-port
eos-agent-core  -> eos-agent-run, eos-engine, eos-tool, eos-workflow,
                   eos-db, eos-llm-client, eos-sandbox-port
```

`eos-agent-run` consumes `dyn AgentLoopLauncher`; it does not name
`TokioAgentLoopLauncher`, `AgentLoopExecutor`, or any other concrete engine type.
`eos-agent-core` is the composition root that constructs the concrete engine
launcher and passes it into `AgentRunService`.

## Resulting File Structure

```text
agent-core/crates/eos-engine/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── error.rs
│   ├── events.rs
│   ├── printer.rs
│   ├── notifications/
│   │   ├── mod.rs
│   │   └── rules.rs
│   ├── agent_loop/
│   │   ├── mod.rs
│   │   ├── launcher.rs
│   │   ├── executor.rs
│   │   ├── state.rs
│   │   ├── stream.rs
│   │   └── turn.rs
│   ├── tool_dispatch/
│   │   ├── mod.rs
│   │   ├── batch.rs
│   │   └── execution.rs
│   ├── records/
│   │   ├── mod.rs
│   │   ├── writer.rs
│   │   └── event_mapper.rs
│   └── background/
│       ├── mod.rs
│       ├── notification.rs
│       └── background_session_manager/
│           ├── mod.rs
│           ├── command_session_manager.rs
│           ├── subagent_session_manager.rs
│           └── workflow_session_manager.rs
└── tests/
    ├── agent_loop/
    ├── records/
    ├── background/
    └── notifications/
```

```text
agent-core/crates/eos-agent-run/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── error.rs
│   ├── service.rs
│   ├── admission.rs
│   ├── active_agent_run.rs
│   ├── persistence.rs
│   ├── completion.rs
│   ├── cancellation.rs
│   └── records.rs
└── tests/
    ├── lifecycle/
    ├── completion/
    └── cancellation/
```

## File Ownership Contract

The target is ownership-first, not module-count-first. Use a folder when a
concept has multiple cohesive implementation files. If a file needs another
responsibility, Phase 04 must be amended before implementation spreads that
logic.

### eos-engine files

| File | Owns | Must not own |
| --- | --- | --- |
| `lib.rs` | narrow public exports | implementation logic or compatibility re-export maze |
| `events.rs` | engine event enum, event severity, event sink input shape | printing, persistence, run finalization |
| `printer.rs` | midflight event printing/sink behavior | durable record writes |
| `notifications/mod.rs` | loop-local notification queue and rule evaluation | external notification facade or run finalization |
| `notifications/rules.rs` | notification rules that are local to loop execution | session storage, persistence, or tool behavior |
| `agent_loop/mod.rs` | loop module routing and public loop-internal exports | full loop implementation |
| `agent_loop/launcher.rs` | concrete `AgentLoopLauncher` implementation and Tokio task launch | run admission, wait/poll/cancel, durable finalization |
| `agent_loop/executor.rs` | full loop state machine, provider stream consumption, loop exit decisions | run lifecycle persistence |
| `agent_loop/state.rs` | in-memory state for one active loop | DB writes, active-run registry |
| `agent_loop/stream.rs` | provider stream normalization and loop event production | tool dispatch or run finalization |
| `agent_loop/turn.rs` | assistant turn execution and tool-call turn semantics | concrete tool families |
| `tool_dispatch/mod.rs` | engine-side tool-dispatch routing | concrete tool families, tool registry definitions |
| `tool_dispatch/batch.rs` | batch rejection/fan-out policy | concrete tool execution |
| `tool_dispatch/execution.rs` | one registered-tool execution glue | tool registry construction |
| `records/mod.rs` | record module routing and engine-local record exports | final agent-run state transitions |
| `records/writer.rs` | loop-visible record writes against a resolved record target | DB lineage lookup or run finalization |
| `records/event_mapper.rs` | engine-event to record-row mapping | printing or persistence finalization |
| `background/notification.rs` | background completion event rendering and enqueueing | session storage or polling |
| `background/background_session_manager/mod.rs` | `BackgroundSessionRuntime` aggregate, cross-family counts, cancel, list, and completion polling | concrete family-specific protocol details |
| `background/background_session_manager/command_session_manager.rs` | command-session registration, active IDs, counts, cancel, completion polling | workflow/subagent behavior |
| `background/background_session_manager/subagent_session_manager.rs` | subagent registration, active IDs, counts, cancel, completion polling | command/workflow behavior |
| `background/background_session_manager/workflow_session_manager.rs` | workflow registration, active IDs, counts, cancel, completion polling | command/subagent behavior |

The only session-manager vocabulary in this phase is the flat
`background_session_manager/*_session_manager.rs` layout above. Do not
reintroduce nested `session_managers/<kind>/...` folders or generic `lane`,
`recorder`, `driver`, or internal `*_port` names.

### eos-agent-run files

| File | Owns | Must not own |
| --- | --- | --- |
| `lib.rs` | narrow lifecycle exports | engine or tool implementation exports |
| `service.rs` | `AgentRunService`, lifecycle orchestration, active-run map ownership | turn execution or concrete engine types |
| `admission.rs` | run admission, request validation, launch input mapping | provider streaming |
| `active_agent_run.rs` | one process-local `ActiveAgentRun` value | durable DB state or map-wide orchestration |
| `persistence.rs` | durable run state transitions | engine event interpretation |
| `completion.rs` | exactly-once engine outcome handoff and final-state mapping | event-by-event loop handling |
| `cancellation.rs` | run cancellation orchestration | concrete tool or sandbox family behavior |
| `records.rs` | resolve/pass `AgentRunRecordTarget` for engine writes | loop-visible record interpretation |

Target `AgentRunService` field shape:

```rust
pub struct AgentRunService {
    agent_registry: Arc<AgentRegistry>,
    run_store: Arc<dyn AgentRunStore>,
    loop_launcher: Arc<dyn AgentLoopLauncher>,
    active_agent_runs: RwLock<HashMap<AgentRunId, ActiveAgentRun>>,
    record_targets: Arc<dyn AgentRunRecordTargetResolver>,
    runtime_state: Arc<dyn AgentRuntimeStateStore>,
}
```

Use `agent_registry` / `AgentRegistry` vocabulary, not `agent_catalog`. The
target type comes from the Phase 02 agent-definition disposition; the concrete
registry DTO now lives in `eos-types`, so Phase 04 must not recreate an
agent-definition crate edge.

Target active-run value:

```rust
struct ActiveAgentRun {
    agent_run_id: AgentRunId,
    cancel_handle: AgentLoopCancelHandle,
    outcome_tx: watch::Sender<Option<AgentRunOutcome>>,
}
```

There is no target `ActiveAgentRuns` wrapper. `AgentRunService` owns the
`active_agent_runs` map directly. Keep `agent_run_id` inside `ActiveAgentRun`
even though it duplicates the map key, so the value remains self-identifying
when moved into completion or cancellation helpers.

## Loop Launch Contract and Engine Surface

`eos-agent-ports` owns the shared launch contract consumed by
`eos-agent-run`:

```text
AgentLoopLauncher
StartAgentLoopRequest
StartedAgentLoop
AgentLoopOutcome
AgentLoopCancelHandle
AgentLoopCancelSignal
AgentExecutionMetadataService
```

`eos-engine` implements this contract and exports only concrete engine
composition types. It must not re-export every internal engine helper.
There is no target `services.rs` file and no first-target `services/` folder;
execution internals stay in `agent_loop/`, `tool_dispatch/`, `records/`,
`printer.rs`, and `background/`.

The loop module is named `agent_loop` (not `loop`): `loop` is a reserved Rust
keyword, so `mod loop;` does not compile.

Allowed exported surface:

```text
TokioAgentLoopLauncher
AgentLoopToolRegistryFactory
AgentLoopToolRegistryBuildInput
AgentLoopBackgroundDependencies
EngineEventSink
MidflightPrinter
```

Contract:

| Type | Consumer | Rule |
| --- | --- | --- |
| `AgentLoopLauncher` | `eos-agent-run`, test harnesses | lives in `eos-agent-ports`; starts an async loop only through the lifecycle boundary |
| `StartAgentLoopRequest` | `eos-agent-run` | lives in `eos-agent-ports`; carries run correlation, record target, cancellation, and runtime inputs |
| `StartedAgentLoop` | `eos-agent-run` | lives in `eos-agent-ports`; carries the only lifecycle completion receiver and the loop cancel handle |
| `AgentLoopOutcome` | `eos-agent-run` | lives in `eos-agent-ports`; contains terminal status, passive submission facts, record summary, and background-session closure status |
| `TokioAgentLoopLauncher` | `eos-agent-core` runtime wiring, tests | concrete engine implementation of `AgentLoopLauncher` |
| `EngineEventSink` | `eos-agent-core` runtime wiring, tests | receives midflight events without owning finalization |

The engine may receive a run/correlation ID for records and events. It must not
own the active-run registry, admission state, or durable lifecycle row.

Completion and event vocabulary:

| Name | Owns | Must not own |
| --- | --- | --- |
| `StartedAgentLoop::outcome_receiver` | lifecycle completion notification from engine task to run service | stream/tool event delivery |
| `EventSource` | provider stream input for one assistant turn | lifecycle completion or run finalization |
| `EventSourceFactory` | choosing an `EventSource` per loop request and agent state | run persistence or wait/poll state |
| `EngineEventSink` | midflight stream/tool/system event observation | final run-state persistence |
| `MidflightPrinter` | rendering engine events for users/logs | durable record writes or lifecycle finalization |

Do not use "event hook" to describe agent-run completion. Completion is the
`StartedAgentLoop::outcome_receiver` lifecycle channel. Events are stream/tool
observations inside engine execution.

Names to avoid:

```text
NotificationService       # engine-internal queue, rename if private
BackgroundTeardownService # engine-internal finalizer, rename if private
RecordService             # avoid for private internals; use only as sibling-consumed facade
EventPrinterService       # event printer type, not service unless sibling-consumed
EventCallback             # too generic; target name is EngineEventSink
AgentLoopHooks            # remove if no-op; if needed, use AgentLoopObserver for engine-only observation
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

`background/mod.rs` is the routing/export surface. The aggregate root lives in
`background/background_session_manager/mod.rs`. The family manager modules keep
implementation details local, but the aggregate owns cross-family policy.

| Capability | Owner | Required behavior |
| --- | --- | --- |
| register active background work | family module | records typed active ID and source family |
| count active work | `background_session_manager/mod.rs` | returns command/subagent/workflow counts in one snapshot |
| list active IDs | `background_session_manager/mod.rs` | preserves family identity; no stringly mixed ID list |
| cancel by reason | `background_session_manager/mod.rs` | forwards `cancel(reason)` to every family and reports partial failures |
| poll completions | `background_session_manager/mod.rs` | drains family completions and emits engine events |
| terminal gate | `background_session_manager/mod.rs` plus hooks in `eos-tool` | terminal submission/isolated-workspace gates can prove no background sessions remain |

The background runtime is allowed to depend on sandbox, workflow, and subagent
runtime handles. It must not depend on concrete tool family modules or on
`eos-agent-run` active-run internals. If it needs to spawn/wait/poll subagent
runs, it consumes `dyn AgentRunApi` from `eos-agent-ports`, never the concrete
run crate.

## Lifecycle Handoff

Completion flow:

```mermaid
sequenceDiagram
    participant AgentCore as eos-agent-core
    participant Run as eos-agent-run
    participant Port as eos-agent-ports::AgentLoopLauncher
    participant Engine as eos-engine
    participant Tool as eos-tool

    AgentCore->>Run: spawn_agent(request)
    Run->>Port: start_agent_loop(StartAgentLoopRequest)
    Port->>Engine: dynamic dispatch to TokioAgentLoopLauncher
    Engine->>Tool: execute model-callable tools
    Engine->>Engine: print midflight events and write records
    Engine-->>Run: AgentLoopOutcome through StartedAgentLoop.outcome_receiver
    Run->>Run: persist final agent-run state
    Run-->>AgentCore: AgentRunOutcome
```

This is a lifecycle handoff, not an event-driven callback into the runner.

Handoff rules:

| Rule | Owner |
| --- | --- |
| engine produces exactly one terminal `AgentLoopOutcome` | `eos-engine` |
| run crate receives outcome and performs exactly one durable finalization | `eos-agent-run` |
| cancellation can win before, during, or after engine startup | `eos-agent-run` orchestrates; `eos-engine` observes token |
| failed engine startup creates a failed run outcome, not a dangling active run | `eos-agent-run` |
| background sessions are cancelled or reported before final state is persisted | `eos-engine` reports; `eos-agent-run` persists |
| final outcome is visible to waiters and pollers after persistence succeeds | `eos-agent-run` |
| `ActiveAgentRun` is removed from the map before final publication | `eos-agent-run` |
| midflight event sinks cannot finalize or publish run outcomes | `eos-engine` / `eos-agent-core` |

## Records and Midflight Printing

Target ownership:

| Behavior | Owner |
| --- | --- |
| event emission during loop | `eos-engine` |
| midflight printing | `eos-engine/printer.rs` |
| record interpretation | `eos-engine/records/event_mapper.rs` |
| record writing | `eos-engine/records/writer.rs` |
| durable run finalization | `eos-agent-run` |
| external record contract | `eos-agent-core`, if externally exposed |

Reason: the engine sees stream events, tool calls, assistant messages, and
terminal transitions as they happen. The runner only sees the final outcome.

Record and print rules:

| Rule | Owner |
| --- | --- |
| every model-visible stream/tool event can be printed midflight | `printer.rs` |
| every durable loop-visible event is interpreted once into records | `records/event_mapper.rs` |
| printing failure cannot corrupt loop state | `printer.rs` reports non-fatal sink errors |
| record write failure is an engine error and appears in `AgentLoopOutcome` | `records/writer.rs`, `agent_loop/executor.rs` |
| externally exposed record DTOs are re-exported by `eos-agent-core` only if needed | `eos-agent-core` |

`eos-agent-run` resolves and passes a passive `AgentRunRecordTarget` into
`StartAgentLoopRequest`. `eos-engine` writes loop-visible records against that
target. It must not derive lineage from DB state or perform final run-status
transitions while writing records.

Naming rule: target API names must not combine `Message` and `Record`. Use
`AgentRunRecordKind`, sibling-facing `RecordService`, `records_root`, or
`record_kind` for temporary compatibility names. Do not introduce
`AgentRunMessageRecordKind`, `MessageRecordService`, or `message_records` in the
target engine/run surface. The literal file name `messages.jsonl` is unchanged.

## Progress Tracker

| Item | Status |
| --- | --- |
| Keep loop launch contract in `eos-agent-ports` | Not started |
| Export concrete engine launcher only from `eos-engine` | Not started |
| Add exact engine file ownership contracts | Not started |
| Add exact run file ownership contracts | Not started |
| Separate lifecycle completion channel from engine event sinks | Not started |
| Rename current `EventCallback` target to `EngineEventSink` | Not started |
| Remove no-op `AgentLoopHooks` or rename/load it as engine-only `AgentLoopObserver` | Not started |
| Replace `ActiveAgentRuns` wrapper target with `HashMap<AgentRunId, ActiveAgentRun>` | Not started |
| Add execution invariants for stream/tool/terminal behavior | Not started |
| Add `BackgroundSessionRuntime` aggregate contract | Not started |
| Move records into engine internals | Not started |
| Add engine midflight printer/sink | Not started |
| Remove concrete tool ownership from engine | Not started |
| Rename private `*Service` internals where needed | Not started |
| Rename `eos-agent-runner` to `eos-agent-run` | Not started |
| Keep active run map in run crate | Not started |
| Keep finalization persistence in run crate | Not started |
| Add exactly-once completion handoff tests | Not started |
| Add cancellation race tests | Not started |
| Add background-session accounting tests | Not started |
| Update `eos-agent-core` runtime wiring | Not started |
| Update `index.md` Progress Tracker with Phase 04 result and exit artifact | Not started |

## Acceptance Criteria

- `eos-engine` has no `tools/` concrete tool family folder.
- `eos-engine` does not own tool registry definitions or hook contracts.
- `AgentLoopLauncher`, `StartAgentLoopRequest`, and `AgentLoopOutcome` are
  consumed from `eos-agent-ports`, not from `eos-engine`.
- `StartedAgentLoop::outcome_receiver` is the only engine-to-run lifecycle
  completion notification.
- `eos-engine` exports the concrete `TokioAgentLoopLauncher` and engine
  composition helpers, not a broad service facade.
- `EventSource` is provider input only; it is not used for completion,
  finalization, or wait/poll notification.
- `EngineEventSink` is the target name for midflight stream/tool/system events;
  there is no target `EventCallback` API.
- There is no no-op target `AgentLoopHooks`; if engine-only lifecycle
  observation is needed, it is named `AgentLoopObserver` and must not be used by
  `eos-agent-run` for finalization.
- `eos-engine` has no target `services.rs` file and no first-target
  `services/` folder.
- Engine records and midflight printing work during loop execution.
- `eos-agent-run` has no normal dependency edge to `eos-engine`.
- `eos-agent-run` does not import concrete tool modules.
- `eos-agent-run` has no dependency on `eos-tool`, `eos-tool-ports`, or
  `ToolResult`; model-facing rendering happens above the lifecycle layer.
- `eos-agent-run` owns `active_agent_runs: HashMap<AgentRunId, ActiveAgentRun>`
  directly; there is no target `ActiveAgentRuns` wrapper.
- `eos-agent-run` owns spawn/wait/poll/cancel/finalization.
- `eos-agent-run` does not interpret stream/tool events.
- `eos-engine/src/background` keeps concrete command, workflow, and subagent
  managers under flat `background_session_manager/*_session_manager.rs` files;
  there are no target `background/*_sessions.rs` files and no nested
  `session_managers/<kind>/...` folders.
- Engine completion returns to run lifecycle through an outcome receiver or
  equivalent lifecycle handoff.
- Engine startup failure cannot leave an active run without a terminal state.
- Engine cancellation produces one terminal outcome and one durable finalization.
- Foreground multi-tool batches are proven to execute with bounded fan-out/fan-in.
- Terminal-tool in-band errors remain retryable by the model.
- Background session counts, list, cancel, and completion polling are tested per
  family and through the aggregate.
- Midflight printing is tested separately from durable record writing.
- `cargo tree -p eos-agent-run --edges normal --depth 1` does not show
  `eos-engine`, `eos-tool`, or `eos-tool-ports`.
- `cargo tree -p eos-engine --edges normal --depth 1` does not show
  `eos-agent-run`.
- `cargo test -p eos-engine` passes.
- `cargo test -p eos-agent-run` passes.
- Focused tests cover loop outcome handoff, cancellation races, background
  accounting, records, and midflight printing.
- Final file layout follows the file ownership table above; there is no
  standalone module-count cap for this phase.
