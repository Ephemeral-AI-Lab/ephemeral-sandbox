# Phase 2 Command Service Implementation Plan

Date: 2026-06-18
Spec: `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_SPEC.md`

## Scope

This plan splits Phase 2 into narrow, reviewable milestones that migrate daemon
command lifecycle ownership into `operation_service::command` while preserving
the target boundaries from the spec:

- `OperationServices` exposes `workspace`, `command`, and `remount`.
- `WorkspaceRemountService` lives under `operation_service/src/workspace_remount`
  and owns cross-service remount orchestration.
- `WorkspaceManagerService` owns workspace session state, resource create,
  capture, remount, destroy, and remount-pending state transitions. It must not
  import or call command-service types.
- `CommandOperationService` owns command lifecycle, command-to-workspace
  binding, stdin/read/poll/cancel, finalization, command-side remount quiesce,
  and command-side process inspection.
- `crates/daemon/command` remains the low-level PTY/process/transcript substrate.
- `CommandRegistry` remains exactly one map:
  `HashMap<CommandId, WorkspaceId>`.
- After Milestone 6.5, no public or crate-public command-service exec boundary
  may receive `Option<WorkspaceSessionHandler>`; workspace resolution belongs
  inside `CommandOperationService::exec_command(input, context)`.
- Stdin/read/poll/cancel are command-id based and authorize through
  `CommandCallContext`.
- `ExecCommandInput` uses `workspace_root` as its only root path, has no request
  correlation identifiers, and has no per-command `remountable` flag.
- One-shot finalization is `OneShotPublishThenDestroy`: successful commands use
  `CommandFinalizationOptions { one_shot_capture, one_shot_publish }` to publish
  captured changes, non-success paths discard, and the temporary workspace is
  destroyed only after the publish/discard result is recorded.
- Persistent session command finalization does not publish, destroy the
  workspace, or update session snapshot/layer metadata. Optional changed-path
  metadata must come from a bounded non-mutating scan.

## Non-Goals

- Do not modify the Phase 2 spec while implementing these milestones.
- Do not move the low-level `command` crate into `operation_service`.
- Do not preserve `WorkspaceRuntime`, `daemon/core/runtime`, or
  `operation::command::contract` as target architecture.
- Do not expose `collect_completed`, `count_commands`/`count_by_caller`, or
  `advance_active_commands_once` as Phase 2 command-service APIs.
- Do not add request correlation identifiers to `ExecCommandInput`.
- Do not add per-command remount opt-in state.
- Do not make `WorkspaceManagerService` depend on `CommandOperationService`.
- Do not add caller/workspace secondary indexes to `CommandRegistry`.
- Do not implement daemon restart recovery or a persistent command/session store
  unless explicitly scoped after Phase 2 command-service migration.

## Review, Refactor, And Cleanup Discipline

Every milestone should end with a focused cleanup pass, not just a green compile.
The cleanup pass is still bounded by that milestone's scope:

- Remove unused scaffold fields, placeholder methods, stale imports, and
  temporary test helpers when the milestone no longer needs them.
- Remove old command/runtime compatibility only at the milestone that owns that
  migration. Do not delete legacy daemon dispatch, `operation::command`, or
  `WorkspaceRuntime` code before M7/M8 unless an earlier milestone explicitly
  moves that behavior.
- Do not keep `#[allow(dead_code)]` as a substitute for either deleting unused
  code or adding the real target API. If a placeholder must remain for a later
  milestone, record why in the implementation record.
- Prefer narrow refactors that make ownership boundaries clearer. Avoid broad
  formatting churn, module reshuffles, or behavior-preserving rewrites outside
  the changed milestone surface.
- After implementation, run static boundary searches for old target vocabulary
  in the touched operation-service files and update the implementation record
  with either a clean result or explicit false positives.

Adversarial review should be split by responsibility so findings are not
averaged away:

- Registry/binding lane: `CommandRegistry`, command/workspace binding shape,
  workspace scan behavior, and absence of secondary indexes.
- Process/completion lane: `CommandProcessStore`, active records, reservation
  rollback, completed-record retention, and caller ownership data.
- Contract/boundary lane: command inputs, service method signatures, error
  types, module exports, dependency direction, and forbidden legacy terms.
- Test/record lane: focused tests, static checks, implementation-record
  accuracy, unresolved issues, and handoff notes for the next milestone.

## Current-State Summary

This summary is grounded in the live tree at plan creation time:

- `operation_service` currently exposes only `OperationServices { workspace }`
  in `crates/daemon/operation_service/src/services.rs:6`. There are no
  `command/` or `workspace_remount/` folders under `operation_service/src`.
- `WorkspaceManagerService` exists in
  `crates/daemon/operation_service/src/workspace_manager/service.rs:13` and already
  owns `create`, `resolve`, `capture_changes`, `remount_workspace`, and
  `destroy` methods at lines 27, 57, 81, 99, and 116. The session manager has
  only `Active` and `Closing` lifecycle states at
  `workspace/session_manager.rs:11`.
- `operation_service` has focused tests in
  `crates/daemon/operation_service/tests/workspace_manager.rs` that use a fake
  `WorkspaceService` and assert ownership, rollback, stale handler rejection,
  capture refresh, remount id mismatch, and destroy retention behavior.
- The `command` crate is already a low-level substrate. `CommandProcess` and
  `CommandProcessExit` are in `crates/daemon/command/src/process.rs:29` and
  `:89`; spawning, stdin, exit taking, and final persistence are at lines 179,
  247, 300, and 334. The wait loop lives in
  `command/src/yield_wait_loop.rs:6`.
- The low-level `command::StartCommand` still carries `trace_id`, `request_id`,
  and `remountable` in `command/src/contract.rs:49-58`, and
  `CollectCompleted` is still exported at `:80`. These are migration evidence,
  not target operation-service contracts.
- The current higher-level command owner is `operation::command::CommandOps`.
  It still couples lifecycle to workspace target shape through
  `ExecTarget` at `crates/daemon/operation/src/command/service.rs:48` and owns
  config, commit/capture options, registry, resource samples, and trace buffers
  at `:134`.
- The current `operation::command::CommandRegistry` is caller-primary:
  `runs: Mutex<HashMap<String, HashMap<String, Arc<ActiveCommand>>>>` and a
  completed buffer are defined in
  `crates/daemon/operation/src/command/registry.rs:123-124`. This must be
  replaced by the target one-map registry plus a separate completion store.
- `operation::command` still exposes public command lifecycle helpers that must
  not survive as command-service APIs: `count_by_caller`,
  `collect_completed`, and `advance_active_commands_once` in
  `service/lifecycle.rs:39`, `:44`, and `:149`.
- Current remount quiesce is caller-based and per-command opt-in:
  `begin_live_remount_for_caller` is in
  `operation/src/command/service/remount.rs:105`, and the old
  `remountable` checks appear around lines 128, 142, 144, and 157.
- `workspace::WorkspaceService` is resource-facing in
  `crates/daemon/workspace/src/service.rs:8`. Current capture input and result
  are metadata-shaped in `workspace/src/model.rs:78` and `:146`, where
  `CapturedWorkspaceChanges` is just a `CaptureChangesResult` alias. Phase 2
  should extend this into one generic upperdir-delta capture result, not a
  command-specific mode enum.
- The old lower-level isolated workspace manager already has a remount state
  enum in `workspace/src/lifecycle/remount/state.rs:2` and caller-keyed
  `mark_remount_pending` plus `remount_with_layers` methods in
  `workspace/src/lifecycle/remount/apply.rs:10` and `:45`. Phase 2 should move
  session-level remount-pending ownership into `WorkspaceManagerService`.
- `daemon/core/src/op_adapter/command.rs:65` parses `sandbox.command.exec`,
  calls `WorkspaceRuntime::route_command_context` at `:135`, builds
  `command::StartCommand` at `:143`, and still maps collect/count/stdin/poll/
  cancel directly to `CommandOps` around lines 176, 193, 215, 235, and 244.
- `WorkspaceRuntime` lives in `daemon/core/src/runtime/workspace.rs:630`, owns
  `Arc<CommandOps>` at `:632`, routes commands at `:829`, marks remount pending
  and quiesces through `CommandOps` at `:1129-1145`, and protects idle
  workspaces through command counts at `:1292` and `:1433`.
- `RuntimeServices` still owns `Arc<CommandOps>` and `WorkspaceRuntime` in
  `daemon/core/src/runtime/services.rs:77-81`, and its background task calls
  `advance_active_commands_once` at `:34`.
- `protocol` still advertises command exec/write/poll/cancel plus
  collect/count in `crates/shared/protocol/src/catalog.rs:320-331`.
- The TypeScript `packages/coding-agent/src/tools/local_os/...` paths cited by
  the spec are not present in this checkout. Live local_os command evidence in
  this tree is currently the daemon E2E test
  `crates/e2e-test/tests/workspace-runtime-command/command_local_os_sandbox.rs`,
  which exercises the legacy status/stdout command surface rather than the
  target row projection.

## Target File And Folder Structure

Final target shape:

```text
crates/daemon/operation_service/src/
  lib.rs
  services.rs
  error.rs

  workspace_manager/
    mod.rs
    service.rs
    session_manager.rs
    error.rs

  command/
    mod.rs
    service.rs
    contract.rs
    registry.rs
    process_store.rs
    exec.rs
    finalize.rs
    transcript.rs
    remount.rs
    error.rs

  workspace_remount/
    mod.rs
    service.rs
    error.rs
```

Optional policy-free extractions from `operation_service/src/command` into the
existing `command` crate:

```text
crates/daemon/command/src/
  launch.rs          # generic runner request/artifact preparation
  process_store.rs   # only if it owns process handles and no workspace policy
  quiesce.rs         # process-group freeze/resume and /proc inspection helpers
  transcript_rows.rs # policy-free row windows, if kept below operation_service
```

Dependency direction after Phase 2:

```text
daemon request entrypoint -> operation_service
operation_service -> command
operation_service -> workspace
operation_service -> layerstack
operation_service -> trace
command -> no workspace, no layerstack, no operation, no operation_service
workspace -> no operation_service, no command-service types
layerstack -> no operation_service
```

## Milestone List

The suggested milestone order is mostly kept. The only sequencing adjustment is
the explicit Milestone 3.5 bridge: it isolates real command launch and initial
yield from the completed M3 ownership/admission surface and keeps M4 focused on
one-shot/session finalization. The generic upperdir-delta capture contract stays
grouped with one-shot/session finalization, because that finalization cannot be
reviewed independently without the new capture result shape.

- [x] Milestone 1: Operation-service scaffolding and contracts.
- [x] Milestone 2: Command service registry/process-store split.
- [x] Milestone 3: Exec Some/None flows and caller ownership.
- [x] Milestone 3.5: Policy-free command launch and initial yield.
- [x] Milestone 4: One-shot finalization and persistent-session finalization semantics.
- [x] Milestone 5: Local OS row projection.
- [x] Milestone 6: `WorkspaceRemountService` and remount-pending state.
- [x] Milestone 6.5: Exec command boundary migration to
  `CommandOperationService`.
- [ ] Milestone 6.6: Workspace profile symmetry.
- [ ] Milestone 7: Daemon dispatch migration away from `WorkspaceRuntime`.
- [ ] Milestone 8: Compatibility wrapper cleanup and final gates.

## Milestone 1: Operation-Service Scaffolding And Contracts

### Objective

Create the operation-service module skeleton, target top-level service wiring,
and command plus workspace-remount contract types without moving behavior yet.
This milestone should compile with stubbed or constructor-only service methods
and should not change daemon dispatch.

### Implementation Record Workflow

- [x] At start, create or append the Milestone 1 entry in
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`.
- [x] Before marking this milestone complete, update that entry with files
  changed, verification commands/results, design deviations, unresolved issues,
  and handoff notes for Milestone 2.

### Files And Modules Expected To Change

- `crates/daemon/operation_service/Cargo.toml`
- `crates/daemon/operation_service/src/lib.rs`
- `crates/daemon/operation_service/src/error.rs`
- `crates/daemon/operation_service/src/services.rs`
- New `crates/daemon/operation_service/src/command/*`
- New `crates/daemon/operation_service/src/workspace_remount/*`
- Test additions under `crates/daemon/operation_service/tests/`

### Expected Structure After This Milestone

```text
operation_service/src/
  command/
    mod.rs
    contract.rs
    registry.rs
    process_store.rs
    service.rs
    error.rs
  workspace_remount/
    mod.rs
    service.rs
    error.rs
  workspace_manager/
    existing files unchanged except exports if needed
  services.rs
  lib.rs
```

### Struct, Type, And Field Contracts

```rust
pub struct OperationServices {
    pub workspace: Arc<WorkspaceManagerService>,
    pub command: Arc<CommandOperationService>,
    pub remount: Arc<WorkspaceRemountService>,
}
```

- `workspace`: single owner-facing entry to session state and resource
  primitives.
- `command`: single owner-facing entry to command lifecycle and command process
  state.
- `remount`: single owner-facing entry to cross-service remount orchestration.

```rust
pub struct CommandOperationService {
    workspace: Arc<WorkspaceManagerService>,
    config: command::CommandConfig,
    registry: Arc<CommandRegistry>,
    process_store: Arc<CommandProcessStore>,
    finalization_options: CommandFinalizationOptions,
}
```

- `workspace`: used only by command lifecycle paths that must create/resolve/
  destroy/capture workspaces. It is not used by workspace remount orchestration.
- `config`: low-level command configuration copied from the old `CommandOps`
  configuration source.
- `registry`: command-id to workspace-id binding, no process ownership.
- `process_store`: active process and retained completion state, no registry
  identity ownership.
- `finalization_options`: one-shot capture/publish policy knobs only.

```rust
pub struct CommandFinalizationOptions {
    pub one_shot_capture: layerstack::service::BoundedCaptureOptions,
    pub one_shot_publish: layerstack::CommitOptions,
}
```

- `one_shot_capture`: bounded capture options for successful one-shot host
  commands.
- `one_shot_publish`: LayerStack/OCC publish options for successful one-shot
  host commands.
- These fields do not apply to persistent session command finalization.

```rust
pub struct WorkspaceRemountService {
    workspace: Arc<WorkspaceManagerService>,
    command: Arc<CommandOperationService>,
    options: WorkspaceRemountOptions,
}

pub struct WorkspaceRemountOptions {
    pub live_quiesce_timeout_ms: u64,
}
```

- `workspace`: owns session state/resource remount calls.
- `command`: owns command quiesce/resume/inspection calls.
- `options`: remount orchestration knobs. Start narrow; add production policy
  knobs only when needed.

```rust
pub struct CommandCallContext {
    pub caller_id: workspace::CallerId,
    pub trace: OperationTraceContext,
}
```

- `caller_id`: authoritative caller ownership for every command-id operation.
- `trace`: request trace context. It is not copied into `ExecCommandInput`.

```rust
pub struct ExecCommandInput {
    pub caller_id: workspace::CallerId,
    pub workspace_root: PathBuf,
    pub workspace_id: Option<workspace::WorkspaceId>,
    pub cmd: String,
    pub cwd: Option<PathBuf>,
    pub timeout_seconds: Option<f64>,
    pub yield_time_ms: Option<u64>,
}
```

- `workspace_root`: the only root path accepted by the command-service
  contract.
- `workspace_id`: `Some` means persistent session command, `None` means
  private one-shot host command.
- No `layer_stack_root`, `trace_id`, `request_id`, `invocation_id`, or
  `remountable` field.

### Service Method Contracts

| Method | Contract |
| --- | --- |
| `OperationServices::new(workspace, command, remount) -> Self` | Purpose: wire the three service domains. Inputs: three `Arc` services. Outputs/errors: infallible. Ownership rules: does not construct child dependencies or inline orchestration. Notes: tests assert all arcs are preserved. |
| `OperationServices::exec_command(input, trace) -> Result<CommandYield, CommandServiceError>` | Historical M1-M6 contract, superseded by Milestone 6.5. Purpose during early milestones: top-level dispatch helper that resolved an optional workspace and delegated to command service. Current boundary after Milestone 6.5: this method may remain only as a forwarding shim to `CommandOperationService::exec_command(input, context)` and must not resolve workspace ids or own command-start policy. |
| `CommandOperationService::new(workspace, config) -> Self` | Purpose: construct command service with default registry/store/finalization options. Inputs: `Arc<WorkspaceManagerService>`, `command::CommandConfig`. Outputs/errors: infallible. Ownership rules: creates its own `CommandRegistry` and `CommandProcessStore`. Tests: construction exposes no old `CommandOps` dependency. |
| `CommandOperationService::with_finalization_options(...) -> Self` | Purpose: test/config seam for one-shot capture/publish options. Inputs: workspace, config, options. Outputs/errors: infallible. Ownership rules: stores options but does not apply them until M4. Tests: options are retained. |
| `WorkspaceRemountService::new(workspace, command, options) -> Self` | Purpose: construct orchestration service. Inputs: workspace service, command service, options. Outputs/errors: infallible. Ownership rules: this is the only type allowed to hold both workspace and command services for remount. Tests: workspace module has no command-service imports. |

### Implementation Steps

- [x] Add `command` and `workspace_remount` modules with empty behavior and typed
   errors.
- [x] Add command contract types with the target fields.
- [x] Add service structs and constructors.
- [x] Update `OperationServices` to include all three service fields.
- [x] Add only the dependencies needed for the new type signatures.
- [x] Add compile-focused tests that construct the service graph with fake
   workspace service dependencies.

### Explicit Exclusions

- No command execution implementation.
- No daemon dispatch migration.
- No `operation::command` wrapper changes.
- No remount behavior.
- No finalization or capture changes.

### Tests And Verification Commands

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service --test service_graph
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service workspace_manager
cargo fmt --check
git diff --check
```

### Risks And Rollback Notes

- Risk: adding dependencies too early can create cycles. Roll back by keeping
  contracts in `operation_service` and deferring behavior dependencies until
  later milestones.
- Risk: `OperationServices::new` signature churn breaks current tests. Rollback
  is simple because no daemon dispatch should depend on the new service graph in
  this milestone.

### Acceptance Criteria

- [x] `operation_service/src/command` and `operation_service/src/workspace_remount`
  exist.
- [x] `OperationServices` has exactly `workspace`, `command`, and `remount` public
  fields.
- [x] `CommandOperationService` has fields for workspace, config, registry,
  process_store, and `CommandFinalizationOptions`.
- [x] `WorkspaceRemountService` has fields for workspace, command, and options.
- [x] `ExecCommandInput` contains `workspace_root` and optional `workspace_id`, and
  does not contain `layer_stack_root`, request correlation identifiers, or
  `remountable`.
- [x] `operation_service::workspace_manager` does not import command-service types.
- [x] No daemon behavior changes.

## Milestone 2: Command Service Registry/Process-Store Split

### Objective

Replace the caller-primary old registry model with the target registry and a
separate process/completion store. This milestone should be independently
reviewable through unit tests without real PTY process spawning.

### Implementation Record Workflow

- [x] Before starting, read
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  and carry forward unresolved notes from earlier milestones.
- [x] At start, create or append the Milestone 2 entry in the implementation
  record.
- [x] Before marking this milestone complete, update that entry with files
  changed, verification commands/results, design deviations, unresolved issues,
  and handoff notes for Milestone 3.

### Files And Modules Expected To Change

- `operation_service/src/command/registry.rs`
- `operation_service/src/command/process_store.rs`
- `operation_service/src/command/service.rs`
- `operation_service/src/command/error.rs`
- Tests under `operation_service/tests/command_registry.rs` and
  `operation_service/tests/command_process_store.rs`

### Expected Structure After This Milestone

```text
operation_service/src/command/
  registry.rs        # only command_id -> workspace_id binding
  process_store.rs   # active processes and completed records
  service.rs         # owns registry/store arcs
  contract.rs        # command ids, statuses, inputs/outputs
  error.rs
```

### Struct, Type, And Field Contracts

```rust
pub struct CommandRegistry {
    command_workspace: HashMap<CommandId, WorkspaceId>,
}
```

- `command_workspace`: the only field. It binds active command ids to
  workspace ids.
- Implementation may wrap this one map in synchronization for shared service
  access, but the wrapped map remains the only registry state.
- No caller index.
- No workspace index.
- No completed-command storage.
- No process handles.

```rust
pub struct CommandProcessStore {
    active: HashMap<CommandId, ActiveCommandProcess>,
    completed: CommandCompletionStore,
    next_id: AtomicU64,
    active_count: AtomicUsize,
    max_active: usize,
}
```

- `active`: command-id keyed active process records.
- `completed`: retained terminal records, not command registry entries.
- `next_id`: command id allocation.
- `active_count` and `max_active`: admission control.

```rust
pub struct ActiveCommandProcess {
    pub command_id: CommandId,
    pub caller_id: CallerId,
    pub workspace_id: WorkspaceId,
    pub process: command::CommandProcess,
    pub transcript: CommandTranscriptStore,
    pub finalize_policy: CommandFinalizePolicy,
    pub lifecycle_state: CommandLifecycleState,
    pub cancellation: CancellationState,
    pub finalization: FinalizationState,
    pub trace_origin: CommandTraceOrigin,
    pub started_at: Instant,
}
```

- `caller_id`: ownership authority for command-id operations while active.
- `workspace_id`: duplicated here for local finalization convenience, but the
  registry remains the coordination binding.
- `process`: low-level PTY/process substrate.
- `transcript`: retained output source used by poll/read/row projection.
- `finalize_policy`: session versus one-shot behavior.
- `lifecycle_state`, `cancellation`, `finalization`: internal state, not wire
  booleans.
- `trace_origin`: trace facts retained outside `ExecCommandInput`.
- `started_at`: lifecycle timing.

```rust
pub enum CommandFinalizePolicy {
    Session { workspace_id: WorkspaceId },
    OneShotPublishThenDestroy { workspace_id: WorkspaceId },
}
```

- `Session`: no implicit publish and no workspace destroy.
- `OneShotPublishThenDestroy`: success captures the generic upperdir delta and
  publishes through the command LayerStack/OCC policy; non-success, cancelled,
  and timed-out commands discard; the temporary workspace is destroyed only after
  the publish/discard result is recorded.

```rust
pub struct CommandCompletionStore {
    completed: HashMap<CommandId, CompletedCommandRecord>,
}

pub struct CompletedCommandRecord {
    pub command_id: CommandId,
    pub caller_id: CallerId,
    pub workspace_id: WorkspaceId,
    pub result: CommandTerminalResult,
    pub transcript: RetainedCommandTranscript,
    pub finalization: FinalizationState,
    pub completed_at: Instant,
}
```

- `completed`: command-id keyed only.
- `caller_id`: required for authorization after active registry removal.
- `workspace_id`: retained for telemetry and remount/finalization reporting.
- `transcript`: retained row/raw output reference.
- `finalization`: terminal or failed finalization state.

### Service Method Contracts

| Method | Contract |
| --- | --- |
| `CommandRegistry::new() -> Self` | Purpose: create empty binding map. Inputs: none. Outputs/errors: infallible. Ownership rules: owns only `HashMap<CommandId, WorkspaceId>`. Tests: `Debug` or helper asserts no extra maps. |
| `CommandRegistry::bind(command_id, workspace_id) -> Result<(), CommandServiceError>` | Purpose: add active binding. Inputs: owned command/workspace ids. Outputs/errors: duplicate command id error. Ownership rules: does not inspect caller or process state. Tests: duplicate rejected. |
| `CommandRegistry::workspace_for(&CommandId) -> Option<WorkspaceId>` | Purpose: resolve workspace coordination key for active command. Inputs: command id. Outputs/errors: cloned workspace id or none. Ownership rules: does not authorize caller. Tests: returns bound workspace. |
| `CommandRegistry::unbind(&CommandId) -> Option<WorkspaceId>` | Purpose: remove active binding after safe finalization. Inputs: command id. Outputs/errors: removed workspace id or none. Ownership rules: caller/completion state stays in process store. Tests: unbind removes only one map entry. |
| `CommandRegistry::commands_for_workspace(&WorkspaceId) -> Vec<CommandId>` | Purpose: remount scan by workspace id. Inputs: workspace id. Outputs/errors: list, possibly empty. Ownership rules: implemented by scanning `command_workspace`, not by adding an index. Tests: multiple command ids discovered by scan. |
| `CommandProcessStore::allocate_command_id() -> CommandId` | Purpose: allocate service-owned command ids. Inputs: none. Outputs/errors: new id. Ownership rules: id allocation belongs here, not low-level `command::StartCommand`. Tests: stable monotonic unique ids. |
| `CommandProcessStore::try_reserve() -> Result<CommandReservation, CommandServiceError>` | Purpose: enforce active admission. Inputs: none. Outputs/errors: admission error with active/max counts. Ownership rules: reservation does not bind registry until consumed by `insert_active`. Tests: dropped reservation releases count. |
| `CommandProcessStore::insert_active(reservation, record) -> Result<(), CommandServiceError>` | Purpose: install active process record after admission. Inputs: `CommandReservation`, `ActiveCommandProcess`. Outputs/errors: duplicate active id. Ownership rules: process store owns the process handle and consumes the reservation only after insert succeeds. Tests: active lookup returns caller/workspace/finalize policy and duplicate insert rolls reservation back. |
| `CommandProcessStore::active(&CommandId) -> Option<ActiveCommandRef>` | Purpose: read active state for stdin/read/poll/cancel. Inputs: command id. Outputs/errors: active record reference/clone guard. Ownership rules: no caller filtering. Tests: not-found distinct from unauthorized. |
| `CommandProcessStore::complete_active(record) -> Result<Option<ActiveCommandProcess>, CommandServiceError>` | Purpose: atomically move an active command into completed retention. Inputs: completed record. Outputs/errors: removed active record, none when not active, or duplicate completed id. Ownership rules: active state is retained if completed insertion would fail, and admission capacity is released only after completed state is recorded. Tests: completed lookup validates caller data is retained and duplicate completion preserves active state. |
| `CommandCompletionStore::insert(record)` | Purpose: retain terminal record. Inputs: completed record. Outputs/errors: duplicate or eviction result if bounded retention is added. Ownership rules: not part of `CommandRegistry`. Tests: completed lookup validates caller data is retained. |
| `CommandCompletionStore::get(&CommandId) -> Option<CompletedCommandRecord>` | Purpose: let read/poll return retained terminal results. Inputs: command id. Outputs/errors: record or none. Ownership rules: caller authorization happens at service method layer. Tests: missing active can still find completed. |

### Implementation Steps

- [x] Define `CommandId` in `operation_service::command::contract` or move it to
   the lowest existing shared crate that avoids an `operation -> operation_service`
   dependency.
- [x] Implement `CommandRegistry` as exactly one field.
- [x] Add process-store structs and lifecycle enums.
- [x] Add lightweight inactive-process test builders using
  `command::CommandProcess::inactive_for_test`.
- [x] Wire `CommandOperationService` to own `Arc<CommandRegistry>` and
   `Arc<CommandProcessStore>`.
- [x] Add unit tests proving registry shape, scan behavior, completion-store
   ownership retention, and reservation rollback.
- [x] Remove Milestone 1-only registry/process-store skeleton behavior.

### Explicit Exclusions

- No real command spawning.
- No finalization behavior.
- No daemon dispatch changes.
- No `collect_completed`, `count_by_caller`, or `advance_active_commands_once`
  methods on `CommandOperationService`.
- No caller/workspace secondary indexes.

### Tests And Verification Commands

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_registry
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_process_store
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service
cargo fmt --check
git diff --check
```

### Risks And Rollback Notes

- Risk: process store starts to duplicate registry responsibilities. Roll back
  by deleting any caller/workspace indexes and requiring scans.
- Risk: command id type placement creates dependency cycles. Roll back by
  keeping the new id type private to `operation_service::command` until a lower
  shared type is explicitly needed.

### Acceptance Criteria

- [x] `CommandRegistry` contains exactly one `HashMap<CommandId, WorkspaceId>`.
- [x] Active process state and completed records are not fields of
  `CommandRegistry`.
- [x] Completed records retain `caller_id` for authorization after active binding
  removal.
- [x] Workspace command scans are implemented by iterating the one binding map.
- [x] No public command-service count/collect/advance APIs exist.

## Milestone 3: Exec Some/None Flows And Caller Ownership

### Objective

Implement command admission and launch flow selection from
`Option<WorkspaceSessionHandler>`:

- `Some(handler)`: run in an existing persistent workspace session.
- `None`: create a temporary private one-shot host workspace.

Also implement command-id ownership validation for stdin/read/poll/cancel
against `CommandCallContext`.

### Implementation Record Workflow

- [x] Before starting, read
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  and carry forward unresolved notes from earlier milestones.
- [x] At start, create or append the Milestone 3 entry in the implementation
  record.
- [x] Update that entry with current files changed, verification
  commands/results, design deviations, unresolved issues, and handoff notes for
  Milestone 3.5.

### Files And Modules Expected To Change

- `operation_service/src/services.rs`
- `operation_service/src/command/contract.rs`
- `operation_service/src/command/service.rs`
- `operation_service/src/command/exec.rs`
- `operation_service/src/command/process_store.rs`
- `operation_service/src/command/error.rs`
- `operation_service/tests/command_exec.rs`

### Expected Structure After This Milestone

```text
operation_service/src/command/
  exec.rs            # Some/None mode selection and command admission
  service.rs         # public command methods
  contract.rs        # exec/stdin/read/poll/cancel inputs
  registry.rs
  process_store.rs
```

### Struct, Type, And Field Contracts

Add command operation inputs:

```rust
pub struct WriteStdinInput {
    pub command_id: CommandId,
    pub chars: String,
    pub yield_time_ms: Option<u64>,
}

pub struct ReadCommandLinesInput {
    pub command_id: CommandId,
    pub offset: u64,
    pub limit: usize,
}

pub struct PollCommandInput {
    pub command_id: CommandId,
    pub last_n_lines: Option<usize>,
}

pub struct CancelCommandInput {
    pub command_id: CommandId,
}
```

- These inputs carry only `command_id` and operation arguments.
- They do not carry workspace handlers, workspace roots, or caller ids.
- Caller ownership is always from `CommandCallContext`.

Add yield/result shells:

```rust
pub struct CommandYield {
    pub command_id: Option<CommandId>,
    pub status: CommandStatus,
    pub exit_code: Option<i64>,
    pub output: CommandOutputSnapshot,
    pub finalized: Option<CommandFinalizedMetadata>,
}
```

- `command_id`: present for running commands and optionally retained in service
  domain outputs; daemon wire can still strip it for foreground terminal
  compatibility later.
- `output`: derived from one transcript source.
- `finalized`: absent while running.

### Service Method Contracts

| Method | Contract |
| --- | --- |
| `OperationServices::exec_command(input, trace)` | Historical M3 contract, superseded by Milestone 6.5. The retained method is now only a temporary forwarding shim that builds `CommandCallContext` from `input.caller_id` and `trace`, then calls `CommandOperationService::exec_command(input, context)`. |
| `CommandOperationService::exec_command(input, context)` | Current Milestone 6.5 public boundary. Purpose: validate exec input, resolve `workspace_id: Some` through the command service's `WorkspaceManagerService`, create private one-shot host workspaces for `workspace_id: None`, and run the existing launch/finalization flow. Inputs: `ExecCommandInput`, call context. Outputs/errors: command yield shell or typed error. Boundary rules: no public or crate-public handler-taking overload; validation happens before workspace creation or command allocation. |
| `CommandOperationService::write_stdin(input, context)` | Purpose: write to a running command and optionally wait for output. Inputs: command id, chars, yield time, call context. Outputs/errors: `CommandYield` or typed error. Boundary rules: authorize active or completed command owner against `context.caller_id`; do not accept workspace handlers. Notes: control-byte cancel behavior can be preserved, but cancellation still uses command-id ownership validation. Tests: wrong caller receives authorization error, not not-found or leaked output. |
| `CommandOperationService::read_lines(input, context)` | Purpose: read row window by offset/limit. Inputs: command id, offset, limit, context. Outputs/errors: `CommandLinesOutput` or typed error. Boundary rules: active lookup first, then completion store; both authorize caller. Notes: full row projection is completed in M5; M3 can return a minimal snapshot if the row store is stubbed. Tests: completed records remain readable by owner only. |
| `CommandOperationService::poll(input, context)` | Purpose: return current command status and finalize if completed. Inputs: command id, optional tail size, context. Outputs/errors: `CommandPollOutput` or typed error. Boundary rules: command id only; active/completed ownership validation. Notes: no `collect_completed` replacement. Tests: wrong caller cannot poll active or completed command. |
| `CommandOperationService::cancel(input, context)` | Purpose: request command cancellation. Inputs: command id and context. Outputs/errors: `CommandYield` or typed terminal/not-found/authorization error. Boundary rules: active/completed ownership validation. Notes: remount cancellation token logic is added in M6. Tests: wrong caller cannot cancel; owner cancellation marks cancellation state. |
| `pub(crate) CommandOperationService::active_workspace_for_command(command_id)` | Purpose: internal helper for ownership/remount checks. Inputs: command id. Outputs/errors: active workspace id or not found. Boundary rules: reads `CommandRegistry`; not public. Tests: unbound completed commands are not returned as active. |

### Implementation Steps

- [x] Implement the historical `OperationServices::exec_command` dispatch and root
  mismatch behavior; superseded by the Milestone 6.5 command-service boundary.
- [x] Add command service public methods with target signatures.
- [x] Implement `exec_command` mode selection:
   - `Some(handler)`: use handler workspace root, network mode, layer paths, and
     workspace id.
   - `None`: call `WorkspaceManagerService::create(NetworkMode::Host)` using
     `workspace_root` as the root input and temporary adapter path until the
     workspace create contract collapses duplicate roots.
- [x] Register command id in registry and active record in process store.
- [x] Add caller authorization helpers that check active records first and
   completed records second.
- [x] Add tests using fake workspace and inactive/fake command processes where
   possible.

### Explicit Exclusions

- No upperdir-delta capture changes.
- No full `OneShotPublishThenDestroy` finalization semantics beyond enough
  temporary-workspace cleanup to keep tests deterministic.
- No row projection beyond the method signature and minimal output shell.
- No real process spawn, launch artifact generation, or initial yield waiting;
  those are Milestone 3.5.
- No remount-pending behavior yet.
- No daemon dispatch migration.

### Tests And Verification Commands

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_exec
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_ownership
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p command
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
cargo fmt --check
git diff --check
```

### Risks And Rollback Notes

- Risk: M3 process-free active records are mistaken for final launch behavior.
  Roll back by keeping M3.5 as the explicit real-launch milestone and requiring
  `CommandProcess::spawn`/`yield_wait_loop` acceptance checks before daemon
  dispatch migration.
- Risk: temporary one-shot workspace ids leak as reusable session ids. Roll back
  by keeping the id private to active records and never exposing it in command
  outputs.
- Risk: session commands accidentally create/destroy workspaces. Tests must use
  fake workspace service call counts to catch this.

### Acceptance Criteria

- [x] Historical M3 state: only `CommandOperationService::exec_command` accepted
  `Option<WorkspaceSessionHandler>`. Superseded by Milestone 6.5: no public or
  crate-public command exec accepts an optional handler.
- [x] Historical M3 state: `OperationServices::exec_command` validated caller
  ownership and root match for `workspace_id: Some`. Superseded by Milestone 6.5:
  command service owns that resolution and validation.
- [x] `Some(handler)` exec does not create, destroy, or implicitly publish a
  workspace.
- [x] `None` exec creates a private one-shot host workspace and binds its workspace
  id to the command id without exposing it as a reusable session id.
- [x] Stdin/read/poll/cancel use command id plus `CommandCallContext` and reject
  caller mismatches for active and completed records.
- [x] No `ExecCommandInput` request correlation identifiers or `remountable` field
  are introduced.

M3 is closed for ownership/admission. The process-free active record scaffold is
intentionally carried forward to Milestone 3.5, which owns real spawn and initial
yield behavior.

## Milestone 3.5: Policy-Free Command Launch And Initial Yield

The completed implementation and verification details live in
`docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`.

### Objective

Replace the process-free M3 active-record scaffold with a real low-level command
spawn path while preserving the new operation-service ownership and mode-selection
boundaries. This milestone is only about launch preparation, `CommandProcess::spawn`,
and the first yield response; finalization, publish/discard, row projection, remount,
and daemon dispatch remain later milestones.

### Implementation Record Workflow

- [x] Before starting, read
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  and carry forward unresolved launch/yield notes from Milestone 3.
- [x] At start, create or append the Milestone 3.5 entry in the implementation
  record.
- [x] Before marking this milestone complete, update that entry with files
  changed, verification commands/results, design deviations, unresolved issues,
  and handoff notes for Milestone 4.

### Files And Modules Expected To Change

- `operation_service/src/command/exec.rs`
- `operation_service/src/command/service.rs`
- `operation_service/src/command/process_store.rs`
- `operation_service/src/command/contract.rs`
- `operation_service/src/command/error.rs`
- `operation_service/src/workspace_manager/session_manager.rs`
- Optional policy-free launch helpers in `crates/daemon/command`.
- Tests under `operation_service/tests/command_exec.rs` and command-service unit
  tests.

### Service Method Contracts

| Method | Contract |
| --- | --- |
| `CommandOperationService::prepare_launch_context(input, handler, command_id)` | Purpose: assemble policy-free launch material for the low-level command runner. Inputs: exec input, resolved workspace handler, command id. Outputs/errors: runner request/artifact paths or typed command-service error. Boundary rules: no `operation::command`, no `StartCommand`, no request/trace/invocation ids, no `remountable` policy. |
| `CommandOperationService::spawn_command_process(spec, launch)` | Purpose: call `command::CommandProcess::spawn` and map spawn/artifact errors. Inputs: `CommandProcessSpec` and `CommandProcessSpawn`. Outputs/errors: live `CommandProcess` or typed command-service error. Boundary rules: registry/process-store state is not committed until cleanup rollback paths are defined. |
| `CommandOperationService::initial_exec_yield(command_id, process, yield_time_ms)` | Purpose: wait for first command output or completion through `command::yield_wait_loop`. Inputs: active process and yield timeout. Outputs/errors: `CommandYield`. Boundary rules: no unconditional `Running` shell; completed outcome is retained for M4 finalization handoff without exposing collect/advance APIs. |

### Implementation Steps

- [x] Add a policy-free launch context that contains the runner request, request
  path, output path, final path, transcript path, transcript timezone, and output
  drain grace without importing `operation::command`.
- [x] Build `CommandProcessSpec` from operation-service command ids and caller ids
  while keeping `ExecCommandInput` free of request correlation fields.
- [x] Replace the process-free active-record scaffold in
  `CommandOperationService::exec_command` with `CommandProcess::spawn`.
- [x] Preserve cleanup on every launch failure: unbind registry entries, release
  admission reservations, and destroy one-shot workspaces while leaving session
  workspaces alive.
- [x] Insert the live process into `CommandProcessStore` only after spawn succeeds,
  with transcript paths matching the low-level command artifacts.
- [x] Use `command::yield_wait_loop` for the first exec response and map both
  running-output and completed outcomes into `CommandYield`.
- [x] Add tests proving spawn failure cleanup, active insert failure cleanup after
  spawn, first-yield running output, and completed first-yield behavior.

### Explicit Exclusions

- No one-shot publish/discard or persistent-session finalization behavior.
- No upperdir-delta capture changes.
- No row-window implementation beyond existing command output shells.
- No remount-pending behavior.
- No daemon dispatch migration.

### Tests And Verification Commands

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_exec
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_ownership
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p command
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
cargo fmt --check
git diff --check
rg -n "operation::command|StartCommand|request_id|trace_id|invocation_id|remountable|CommandProcess::new" crates/daemon/operation_service/src/command crates/daemon/operation_service/tests/command_exec.rs
```

### Risks And Rollback Notes

- Risk: launch preparation imports old `operation::command` policy. Roll back by
  moving only policy-free runner request construction into `command` or a new
  operation-service helper.
- Risk: one-shot workspaces leak on spawn/artifact failure. Roll back by routing
  all failures through the existing one-shot cleanup helper before returning.
- Risk: first-yield completion starts finalization work early. Roll back by
  retaining terminal process outcome for M4 without publishing, destroying, or
  exposing public collect/advance APIs.

### Acceptance Criteria

- [x] `CommandOperationService::exec_command` launches through a policy-free
  command launch context and `command::CommandProcess::spawn`, not the
  process-free active-record scaffold.
- [x] The first exec response is produced by `command::yield_wait_loop` instead
  of an unconditional running shell.
- [x] Spawn and initial-yield tests prove cleanup is correct for one-shot and
  persistent-session command starts.
- [x] Static boundary scan shows no `operation::command`, old command DTOs, request
  correlation ids, `remountable`, or process-free spawn scaffolds in the
  operation-service launch path.

## Milestone 4: One-Shot Finalization And Persistent-Session Semantics

### Objective

Move command finalization policy into `CommandOperationService` and split
one-shot host behavior from persistent session behavior. Add the generic
workspace upperdir-delta capture contract needed for successful one-shot command
publish.

### Implementation Record Workflow

- [x] Before starting, read
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  and carry forward unresolved notes from earlier milestones.
- [x] At start, create or append the Milestone 4 entry in the implementation
  record.
- [x] Before marking this milestone complete, update that entry with files
  changed, verification commands/results, design deviations, unresolved issues,
  and handoff notes for Milestone 5.

### Files And Modules Expected To Change

- `operation_service/src/command/finalize.rs`
- `operation_service/src/command/service.rs`
- `operation_service/src/command/process_store.rs`
- `operation_service/src/command/contract.rs`
- `operation_service/src/command/error.rs`
- `operation_service/src/workspace_manager/service.rs`
- `operation_service/src/workspace_manager/session_manager.rs`
- `crates/daemon/workspace/src/model.rs`
- `crates/daemon/workspace/src/service.rs`
- Tests under `operation_service/tests/command_finalize.rs` and workspace model
  tests as needed

### Expected Structure After This Milestone

```text
operation_service/src/command/
  finalize.rs        # session vs one-shot finalization
  exec.rs            # calls finalization when initial yield completes
  process_store.rs   # finalization state and completed record retention
workspace/src/model.rs
  CaptureChangesRequest
  CapturedWorkspaceChanges
```

### Struct, Type, And Field Contracts

```rust
pub struct CaptureChangesRequest {
    pub bounds: BoundedCaptureOptions,
    pub include_stats: bool,
}
```

- `CaptureChangesRequest` has one semantic meaning: capture the changes currently
  present in the overlay upperdir.
- No `CaptureChangesMode`, `MetadataOnly`, `PublishableCommandCapture`, or
  command-specific capture mode should be introduced.
- Capture must not mutate the overlay upperdir, publish to LayerStack, retarget
  leases, destroy the workspace, or decide keep/discard policy.
- If bounded capture creates a spool directory, that directory is a temporary
  artifact outside the upperdir and is cleaned up by the command finalizer or
  explicit publish/checkpoint consumer after publish/discard.

```rust
pub struct CapturedWorkspaceChanges {
    pub workspace_id: WorkspaceId,
    pub base_revision: BaseRevision,
    pub changed_paths: Vec<String>,
    pub changed_path_kinds: BTreeMap<String, ChangedPathKind>,
    pub protected_drops: Vec<ProtectedPathDrop>,
    pub stats: Option<TreeResourceStats>,
    pub changes: Vec<layerstack::LayerChange>,
    pub route_stats: layerstack::CaptureRouteStats,
    pub metadata_path_count: usize,
    pub spool_dir: Option<PathBuf>,
}
```

- `CapturedWorkspaceChanges` is one generic upperdir-delta result.
- Workspace/resource code produces capture artifacts but does not decide publish
  versus discard.
- Persistent-session command metadata, if returned, comes from a separate
  non-mutating scan rather than from a metadata-only capture mode.

```rust
pub enum FinalizationState {
    NotStarted,
    InProgress,
    ResponseBuffered {
        finalized: CommandFinalizedMetadata,
    },
    WorkspaceDestroyPending {
        finalized: CommandFinalizedMetadata,
    },
    Complete,
    Failed {
        error: String,
        finalized: Option<CommandFinalizedMetadata>,
    },
}
```

- State is retained so finalization failure does not silently drop command or
  workspace cleanup state. Intermediate and failed states keep already-decided
  publish/discard metadata so destroy failures remain reportable with the
  prior outcome.

### Service Method Contracts

| Method | Contract |
| --- | --- |
| `pub(crate) CommandOperationService::finalize_command(command_id, process_exit)` | Purpose: route finalization by `CommandFinalizePolicy`. Inputs: command id and `command::process::CommandProcessExit`. Outputs/errors: `CommandTerminalResult` or retained finalization failure. Boundary rules: removes active state only after finalization state is recorded; removes registry binding only when finalization no longer needs remount/lifecycle coordination. Tests: exit is finalized exactly once. |
| `fn CommandOperationService::finalize_session_command(record, exit)` | Purpose: finalize persistent session command. Inputs: active record and process exit. Outputs/errors: terminal result. Boundary rules: no publish, no workspace destroy, no session snapshot/layer update. Notes: optional changed paths come from a separate non-mutating bounded scan only, not `capture_changes`. Tests: fake workspace service sees no capture call, no destroy, no snapshot refresh. |
| `fn CommandOperationService::finalize_one_shot_command(record, exit)` | Purpose: implement `OneShotPublishThenDestroy` for a private one-shot host command. Inputs: active record and process exit. Outputs/errors: terminal result plus publish/discard and destroy result/failure metadata. Boundary rules: success captures the generic upperdir delta and publishes; non-success/cancel/timeout discards; destroy is attempted only after the publish/discard result is recorded; destroy failure retains state. Tests: success calls upperdir-delta capture, publish, then destroy; failure records discard and then destroys. |
| `WorkspaceManagerService::capture_changes(handler, request)` | Purpose: return the generic captured overlay upperdir delta from resource service. Inputs: handler and capture bounds. Outputs/errors: `CapturedWorkspaceChanges` or workspace manager error. Boundary rules: no command-specific mode, no publish/discard decision, no upperdir mutation, no workspace destroy, and no persistent-session command finalization scan. Tests: captured result passes through without workspace manager deciding publish. |
| `pub(crate) CommandOperationService::scan_session_changed_paths(handler, bounds)` | Purpose: optional non-mutating session metadata scan. Inputs: session handler and bounds. Outputs/errors: changed-path metadata or scan error. Boundary rules: must not materialize payloads, publish, retarget leases, destroy, or refresh session snapshot/layer metadata. Tests: fake scan proves no workspace manager capture/destroy calls. |

Finalizer supervision should be introduced only as a real process-exit
supervisor with the policy-free spawn/yield work. Do not keep a no-op
`start_finalizer_watch` placeholder solely to reserve the name.

### Implementation Steps

- [x] Extend `workspace` capture types into one generic upperdir-delta result.
- [x] Update `WorkspaceService::capture_changes` implementors and tests to return
   `CapturedWorkspaceChanges` with `LayerChange` payloads and route metadata.
- [x] Move or adapt host finalization publish-lane/OCC behavior from
   `operation::command::finalize` into `operation_service::command::finalize`.
- [x] Add session finalization path that never publishes, destroys, or updates
   session snapshot/layer metadata.
- [x] Add one-shot `OneShotPublishThenDestroy` finalization path that uses
   `CommandFinalizationOptions` for publish/capture policy.
- [x] Add crate-private finalization for completed initial-yield and poll paths;
   background watcher registration remains deferred in the implementation record.
- [x] Store completed records in `CommandCompletionStore` with caller/workspace
   metadata and transcript retention.
- [x] Add tests for success, non-success, cancellation, timeout, destroy failure,
   finalization failure, and retained ownership.

### Explicit Exclusions

- No public `advance_active_commands_once`.
- No persistent session implicit checkpoint or publish.
- No row-window implementation beyond retaining enough transcript metadata for
  M5.
- No remount quiesce/cancellation token.
- No daemon dispatch migration.

### Tests And Verification Commands

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p workspace capture
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_finalize
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_completion
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
cargo fmt --check
git diff --check
```

### Risks And Rollback Notes

- Risk: workspace capture starts owning publish decisions. Roll back by keeping
  publish decisions entirely in `CommandOperationService`.
- Risk: session command finalization refreshes session metadata by reusing the
  old capture path. Roll back by using a separate non-mutating scan helper.
- Risk: failed finalization drops active state too early. Roll back by recording
  `FinalizationState::Failed` before registry/process-store cleanup.

### Acceptance Criteria

- [x] `CommandFinalizationOptions { one_shot_capture, one_shot_publish }` is the
  only command-service publish/capture options bundle and is scoped to
  `OneShotPublishThenDestroy` finalization.
- [x] Successful one-shot commands use generic `CapturedWorkspaceChanges` upperdir
  deltas and current lane-aware publish/OCC behavior.
- [x] Non-success, cancelled, and timed-out one-shot commands do not publish.
- [x] One-shot workspace destroy is attempted only after the publish/discard result
  is recorded, and destroy failure is retained/reportable.
- [x] Persistent session commands do not publish, destroy, or update session
  snapshot/layer metadata during normal finalization.
- [x] Optional persistent-session changed-path metadata comes only from a
  non-mutating scan.
- [x] Completed records are retained outside `CommandRegistry` and authorize by
  retained `caller_id`.
- [x] No public `collect_completed`, `count_by_caller`, or
  `advance_active_commands_once` method exists on `CommandOperationService`.

## Milestone 5: Local OS Row Projection

Agent prompt:
`docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_5_agent_prompt.md`.

### Objective

Add a row-oriented transcript projection that can support local_os-style output:

```text
{ offset, next_offset, total_lines, output_truncated, output: rows }
```

This must be derived from the same command transcript source used by exec,
stdin, poll, and finalization, not a duplicate output store.

### Implementation Record Workflow

- [x] Before starting, read
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  and carry forward unresolved notes from earlier milestones.
- [x] At start, create or append the Milestone 5 entry in the implementation
  record.
- [x] Before marking this milestone complete, update that entry with files
  changed, verification commands/results, design deviations, unresolved issues,
  and handoff notes for Milestone 6.

### Files And Modules Expected To Change

- `operation_service/src/command/transcript.rs`
- `operation_service/src/command/contract.rs`
- `operation_service/src/command/service.rs`
- `operation_service/src/command/process_store.rs`
- Optionally `crates/daemon/command/src/transcript_rows.rs`
- Tests under `operation_service/tests/command_transcript_rows.rs`

### Expected Structure After This Milestone

```text
operation_service/src/command/
  transcript.rs      # row parsing/windowing and truncation metadata
  contract.rs        # CommandTranscriptRow and CommandLinesOutput
  service.rs         # read_lines, stdin/poll projections
```

### Struct, Type, And Field Contracts

```rust
pub enum CommandStream {
    Stdout,
    Stderr,
}

pub struct CommandTranscriptRow {
    pub offset: u64,
    pub stream: CommandStream,
    pub text: String,
}

pub struct CommandLinesOutput {
    pub command_id: CommandId,
    pub status: CommandStatus,
    pub exit_code: Option<i64>,
    pub offset: u64,
    pub next_offset: u64,
    pub total_lines: u64,
    pub truncated_before: u64,
    pub output_truncated: bool,
    pub output: Vec<CommandTranscriptRow>,
}
```

- `offset`: first requested row offset.
- `next_offset`: offset to use for the next read.
- `total_lines`: total retained/known row count.
- `truncated_before`: number of rows omitted before the returned window.
- `output_truncated`: true when requested output cannot fit configured bounds.
- `output`: row window with stable row offsets.

### Service Method Contracts

| Method | Contract |
| --- | --- |
| `CommandOperationService::read_lines(input, context)` | Purpose: return a row window by offset/limit. Inputs: command id, offset, limit, caller context. Outputs/errors: `CommandLinesOutput` or typed not-found/auth error. Boundary rules: authorize active or completed record before returning rows. Notes: row offsets are line offsets, not byte offsets. Tests: offset/limit/total/next/truncation behavior. |
| `pub(crate) CommandTranscriptStore::append_or_index(raw_chunk)` | Purpose: maintain row index from PTY transcript data. Inputs: raw transcript data or file path delta. Outputs/errors: row count or parse error. Boundary rules: no workspace or publish policy. Tests: stdout/stderr rows get stable offsets. |
| `pub(crate) CommandTranscriptStore::window(offset, limit, bounds)` | Purpose: produce bounded row window. Inputs: offset, limit, output bounds. Outputs/errors: `CommandLinesOutput` parts. Boundary rules: no caller authorization. Tests: truncation flags and `next_offset` are correct. |
| `CommandOperationService::write_stdin(input, context)` | Purpose: after writing stdin, return the same command yield plus row snapshot when requested by local_os-compatible surface. Inputs: stdin request and context. Outputs/errors: command yield/row projection. Boundary rules: authorization remains command-id based. Tests: stdin handoff returns rows from one transcript source. |
| `CommandOperationService::poll(input, context)` | Purpose: continue daemon-native status polling while allowing row projection from retained transcript. Inputs: command id and context. Outputs/errors: poll output. Boundary rules: no collect/completed side channel. Tests: completed poll and row read see same terminal status. |

### Implementation Steps

- [x] Decide whether row parsing stays in `operation_service::command` or moves to
   a policy-free `command` crate helper.
- [x] Add transcript row structs and windowing helper.
- [x] Ensure active and completed command records both retain enough transcript
   metadata for row reads.
- [x] Implement `read_lines`.
- [x] Update `write_stdin` and `poll` to derive output from the same transcript
   source.
- [x] Add unit tests for row offsets, limits, truncation, completed retention, and
   caller authorization.
- [ ] Add daemon E2E coverage later in M7/M8 when the wire surface is migrated.

### Explicit Exclusions

- No TypeScript local_os client migration in this milestone.
- No duplicated transcript source.
- No byte-offset semantics.
- No change to one-shot/session finalization policy.
- No remount behavior.

### Tests And Verification Commands

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_transcript_rows
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p command transcript
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
cargo fmt --check
git diff --check
```

### Risks And Rollback Notes

- Risk: parsing existing timestamped PTY logs into rows is lossy. Roll back by
  writing a structured row sidecar at command-service level while preserving raw
  transcript logs.
- Risk: row store becomes policy-heavy in the low-level command crate. Roll back
  by keeping workspace/command-status projection in operation_service.

### Acceptance Criteria

- [x] `CommandLinesOutput` exposes `offset`, `next_offset`, `total_lines`,
  `truncated_before`, `output_truncated`, and row output.
- [x] `read_lines` is command-id based and validates caller ownership through
  `CommandCallContext`.
- [x] Active and completed command rows come from one transcript source.
- [x] `poll` and `write_stdin` do not bypass authorization or duplicate transcript
  storage.
- [x] Existing legacy command output can continue during migration, but the row
  projection is implemented and tested.

## Milestone 6: WorkspaceRemountService And Remount-Pending State

Agent prompt:
`docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_6_agent_prompt.md`.

### Objective

Move live remount orchestration out of `WorkspaceRuntime` into
`WorkspaceRemountService`, with workspace state transitions owned by
`WorkspaceManagerService` and command quiesce/resume owned by
`CommandOperationService`.

### Implementation Record Workflow

- [ ] Before starting, read
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  and carry forward unresolved notes from earlier milestones.
- [ ] At start, create or append the Milestone 6 entry in the implementation
  record.
- [ ] Before marking this milestone complete, update that entry with files
  changed, verification commands/results, design deviations, unresolved issues,
  and handoff notes for Milestone 7.

### Files And Modules Expected To Change

- `operation_service/src/workspace_manager/session_manager.rs`
- `operation_service/src/workspace_manager/service.rs`
- `operation_service/src/workspace_manager/error.rs`
- New `operation_service/src/command/remount.rs` created with the Milestone 6
  command-side quiesce behavior, not as an empty placeholder.
- `operation_service/src/command/service.rs`
- `operation_service/src/workspace_remount/service.rs`
- New or changed `operation_service/src/workspace_remount/error.rs` only if the
  Milestone 6 behavior introduces remount-specific errors.
- Optional policy-free helpers in `crates/daemon/command/src/quiesce.rs`
- Tests under `operation_service/tests/workspace_remount.rs` and
  `operation_service/tests/command_remount.rs`

### Expected Structure After This Milestone

```text
operation_service/src/
  workspace_manager/
    session_manager.rs # RemountState plus begin/apply/finish support
    service.rs         # workspace-only remount state/resource methods
  command/
    remount.rs         # command quiesce/resume/inspection wrappers
  workspace_remount/
    service.rs         # cross-service orchestration
```

### Struct, Type, And Field Contracts

```rust
pub enum WorkspaceRemountState {
    Active,
    RemountPending,
    RemountBlocked { reason: String },
}

pub(crate) struct WorkspaceSession {
    pub remount_state: WorkspaceRemountState,
    // existing fields retained
}
```

- `RemountPending`: visible while a remount attempt is in progress.
- `RemountBlocked`: optional retained failure/pressure state if a blocked
  report must remain visible.
- Workspace session state remains in `WorkspaceManagerService`.

```rust
pub struct CommandRemountInspection {
    pub active_commands: usize,
    pub command_ids: Vec<CommandId>,
    pub process_group_ids: Vec<i32>,
    pub process_count: usize,
    pub quiesced_process_count: usize,
    pub pinned_cwd_count: usize,
    pub pinned_root_count: usize,
    pub pinned_fd_count: usize,
    pub pinned_mapped_file_count: usize,
    pub mountinfo_checked_count: usize,
    pub blocked_reason: Option<String>,
    pub inspected: bool,
    pub quiesce_attempted: bool,
    pub resumed: bool,
    pub detail: Option<String>,
}
```

- No `remountable_commands` field in the target API.
- Unknown inspection state blocks remount.

```rust
pub struct CommandRemountQuiesce {
    inspection: CommandRemountInspection,
    held_process_group_ids: Vec<i32>,
    cancellation: RemountCancellationToken,
}

pub struct RemountAttemptGuard {
    pub workspace_id: WorkspaceId,
    pub quiesce: CommandRemountQuiesce,
    pub cancellation: RemountCancellationToken,
    pub switch_state: RemountSwitchState,
}
```

- `CommandRemountQuiesce` resumes on finish/drop.
- `RemountAttemptGuard` owns cross-service critical-section state.

```rust
pub enum RemountSwitchState {
    Quiescing,
    ReadyToSwitch,
    CriticalSwitch,
    Resuming,
    Finished,
}
```

- Used by cancel to avoid killing stopped process groups before resume.

### Service Method Contracts

| Method | Contract |
| --- | --- |
| `WorkspaceManagerService::begin_remount(workspace_id) -> Result<WorkspaceSessionHandler, WorkspaceManagerError>` | Purpose: resolve active session and mark remount pending. Inputs: workspace id. Outputs/errors: handler or not-found/closing/remount-pending error. Boundary rules: no command-service imports. Tests: pending state visible and duplicate begin rejected. |
| `WorkspaceManagerService::apply_remount(handler, request) -> Result<WorkspaceSessionHandler, WorkspaceManagerError>` | Purpose: call resource remount and refresh handle metadata after verified remount. Inputs: handler and `RemountWorkspaceRequest`. Outputs/errors: updated handler. Boundary rules: workspace resource primitive only; no command quiesce. Tests: id mismatch and failure retain session. |
| `WorkspaceManagerService::finish_remount(workspace_id)` | Purpose: clear pending after success. Inputs: workspace id. Outputs/errors: updated state or not found. Boundary rules: no command-service imports. Tests: returns to active. |
| `WorkspaceManagerService::finish_or_block_remount(workspace_id, reason)` | Purpose: clear or mark blocked after failed/blocked attempt. Inputs: workspace id and reason. Outputs/errors: state update result. Boundary rules: no command-service imports. Tests: blocked reason retained if supported. |
| `WorkspaceManagerService::is_remount_pending(workspace_id) -> bool` | Purpose: command service guard for starts/stdin. Inputs: workspace id. Outputs/errors: bool. Boundary rules: read-only workspace state. Tests: start rejects while pending. |
| `pub(crate) CommandOperationService::begin_workspace_remount_quiesce(workspace_id)` | Purpose: freeze and inspect active commands for workspace. Inputs: workspace id. Outputs/errors: `CommandRemountQuiesce`. Boundary rules: scans `CommandRegistry.command_workspace`; every command is quiesce eligible; no per-command remount flag. Tests: workspace scan finds active commands and resumes on block. |
| `WorkspaceRemountService::compact_or_remount_session(workspace_id)` | Purpose: full mounted-snapshot compaction/remount orchestration. Inputs: workspace id. Outputs/errors: compacted or blocked report. Boundary rules: owns sequencing between workspace and command services. Notes: lease retarget after mount verification only; old lowerdirs deleted only after lease retarget success. Tests: no-active, live-success, live-blocked, failure-resume, cancel-race paths. |
| `CommandOperationService::cancel(input, context)` with remount token | Purpose: record deterministic cancellation during quiesce. Inputs: cancel request and context. Outputs/errors: terminal/yield result. Boundary rules: do not kill stopped process group before remount guard resumes it. Tests: cancel before critical switch aborts/remount resumes before termination; cancel during critical switch waits for resume. |

### Implementation Steps

- [x] Add remount state to `WorkspaceSession` and handler/status projection.
- [x] Add `begin_remount`, `apply_remount`, `finish_remount`, and blocked-state
   methods to `WorkspaceManagerService`.
- [x] Move command-side quiesce and `/proc` inspection from old
   `operation::command::service::remount` into `operation_service::command`.
- [x] Change quiesce lookup from caller based to workspace-id based registry scan.
- [x] Remove per-command `remountable` gating from the new command-side API.
- [x] Add remount cancellation token and switch-state guard.
- [x] Implement `WorkspaceRemountService::compact_or_remount_session` using the
   current mounted layer remount primitive; parent-prefix production compaction
   remains excluded from this milestone.
- [x] Make command starts and stdin reject with retryable
   `workspace_remount_pending` while pending.
- [x] Allow read/poll during pending and report pending/quiesced metadata where the
   response shape supports it.
- [x] Add unit tests for state transitions, quiesce/resume invariants, cancellation
    races, and blocked telemetry.

### Explicit Exclusions

- No queueing of commands during remount pending.
- No parent-prefix production compaction policy.
- No unsafe fallback that deletes mounted lowerdirs after blocked inspection.
- No command-service imports from `operation_service::workspace_manager`.
- No per-command remount opt-in.

### Tests And Verification Commands

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service workspace_remount
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_remount
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p command
cargo fmt --check
git diff --check
```

### Risks And Rollback Notes

- Risk: stopped process groups remain stopped on early return. Roll back by
  keeping quiesce guard `Drop`-based resume and testing every failure path.
- Risk: workspace service starts importing command service for remount. Roll
  back by moving orchestration into `workspace_remount`.
- Risk: remount deletes lowerdirs before lease retarget success. Roll back by
  splitting mount verification, lease retarget, and cleanup into explicit guard
  states.

### Acceptance Criteria

- [x] `WorkspaceRemountService` exists under
  `operation_service/src/workspace_remount/service.rs`.
- [x] `WorkspaceManagerService` owns remount-pending state and resource remount
  application and has no command-service imports.
- [x] `CommandOperationService` owns only command-side quiesce/resume/inspection.
- [x] `begin_workspace_remount_quiesce` scans `CommandRegistry` by workspace id.
- [x] Every active command is remount-quiesce eligible.
- [x] Unknown inspection state blocks remount.
- [x] Stopped process groups resume on success, failure, early return, and cancel.
- [x] Command starts and stdin reject while remount is pending; read/poll remain
  allowed.
- [x] Cancellation never kills a stopped process group before required resume.

## Milestone 6.5: Exec Command Boundary Migration

Spec:
`docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_6_5_exec_command_boundary_SPEC.md`

This narrow bridge milestone moves the public exec boundary from
`OperationServices::exec_command` to
`CommandOperationService::exec_command(input, context)` before daemon dispatch
migration. It keeps daemon dispatch itself out of scope and may retain
`OperationServices::exec_command` only as a temporary forwarding shim.

### Implementation Steps

- [x] Make `CommandOperationService::exec_command(input, context)` the public exec
  boundary.
- [x] Move `workspace_id: Some` resolution and workspace-root validation into
  command service.
- [x] Keep `workspace_id: None` one-shot host workspace creation inside command
  service.
- [x] Reduce `OperationServices::exec_command` to a forwarding shim only.
- [x] Preserve remount admission, pending-state rejection, one-shot finalization,
  and persistent-session finalization behavior.
- [x] Keep daemon dispatch migration assigned to Milestone 7.

### Milestone 7 Handoff

Milestone 6.6 must run before Milestone 7 so daemon dispatch does not migrate
onto a host-compatible versus isolated profile asymmetry. After Milestone 6.6,
Milestone 7 daemon exec dispatch should call:

```rust
RuntimeServices.operation.command.exec_command(exec_input, command_call_context)
```

## Milestone 6.6: Workspace Profile Symmetry

Spec:
`docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_6_6_workspace_profile_symmetry_SPEC.md`

### Objective

Make host-compatible and isolated workspaces symmetric for every concern except
network setup. The only allowed profile-specific difference is:

```text
HostCompatibleProfile
  host network access
  no isolated veth, DNS rewrite, or isolated net-ready setup

IsolatedProfile
  private network namespace
  veth, DNS rewrite, and isolated net-ready setup
```

Holder lifecycle, namespace FD ownership/projection, scratch lifecycle, cgroup
lifecycle, caller-owned lifetime, capture/publish policy, command lifecycle,
remountability, and file-operation routing must be common and profile-neutral.

### Implementation Record Workflow

- [ ] Before starting, read
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  and carry forward unresolved notes from Milestone 6.5 and the 6.6 adversarial
  review.
- [ ] At start, create or update the Milestone 6.6 entry in the implementation
  record.
- [ ] Before marking this milestone complete, update that entry with files
  changed, verification commands/results, design deviations, unresolved issues,
  and handoff notes for Milestone 7.

### Files And Modules Expected To Change

- `crates/daemon/workspace/src/model.rs`
- `crates/daemon/workspace/src/profile/common.rs`
- `crates/daemon/workspace/src/profile/host_compatible.rs`
- `crates/daemon/workspace/src/profile/isolated.rs`
- `crates/daemon/workspace/src/profile/host_workspace.rs`
- `crates/daemon/workspace/src/profile/handle.rs`
- `crates/daemon/workspace/src/profile/manager.rs`
- `crates/daemon/workspace/src/profile/resource_control.rs`
- `crates/daemon/workspace/src/lifecycle/create.rs`
- `crates/daemon/workspace/src/lifecycle/destroy.rs`
- `crates/daemon/workspace/src/lifecycle/recovery.rs`
- `crates/daemon/workspace/src/lifecycle/remount/*`
- `crates/daemon/operation_service/src/workspace_manager/*`
- `crates/daemon/operation_service/src/command/exec.rs`
- `crates/daemon/operation_service/src/command/remount.rs`
- `crates/daemon/operation_service/src/command/finalize.rs`
- `crates/daemon/core/src/op_adapter/files.rs`
- focused tests under `workspace`, `operation_service`, and `daemon/core`

### Target Ownership Table

| Concern | Owner | Profile must not own |
| --- | --- | --- |
| Holder lifecycle | common workspace lifecycle | spawn, kill, readiness, teardown policy |
| Namespace FD ownership/projection | common workspace lifecycle and launch projection | command/file routing policy |
| Scratch lifecycle | common workspace lifecycle | allocation, rollback, recovery cleanup |
| Cgroup lifecycle | common resource-control lifecycle | create, holder join, command join, remove, recovery |
| Caller-owned lifetime | `WorkspaceManagerService` / session manager | one-shot/session selection |
| Capture/publish policy | `CommandOperationService` and layerstack publish policy | publish/discard/snapshot refresh |
| Command lifecycle | `CommandOperationService` and command crate substrate | start/finalize/cancel policy |
| Remountability | `WorkspaceRemountService` and workspace remount primitives | remount eligibility by profile kind |
| File-operation routing | daemon/operation-service file routing owner | direct versus session routing |
| Network setup | `HostCompatibleProfile` / `IsolatedProfile` | non-network lifecycle behavior |

### Implementation Steps

- [ ] Add or adapt a profile-neutral workspace create path so host-compatible and
  isolated handles are created through the same managed lifecycle.
- [ ] Narrow `WorkspaceProfile` or profile hooks so profile implementations can
  mutate only profile-owned network state.
- [ ] Move cgroup creation, holder join, command join, teardown, and recovery
  cleanup out of `IsolatedProfile` into common lifecycle/resource-control code.
- [ ] Stop treating `HostWorkspace` as a permanent public target abstraction;
  keep it private/temporary or replace it with the common handle path.
- [ ] Route one-shot host-compatible workspaces through the same handle/context
  shape as persistent host-compatible workspaces. One-shot versus persistent
  remains command/workspace policy, not profile policy.
- [ ] Require holder namespace FDs for holder-backed workspace command launch;
  missing FDs are an error, not a silent fresh-namespace fallback.
- [ ] Keep remount eligibility and command quiesce decisions profile-neutral.
- [ ] Define file-operation routing so policy is outside profile implementations.
- [ ] Add focused host-compatible and isolated tests for the same lifecycle,
  command, remount, teardown, recovery, and file-routing contracts where platform
  support permits.

### Explicit Exclusions

- No daemon dispatch migration. That remains Milestone 7.
- No new publish mode or command lifecycle mode.
- No per-command remount opt-in.
- No fake `IsolatedWorkspace` adapter added only for naming symmetry.
- No permanent public `HostWorkspace` target abstraction.
- No new code that uses the compatibility `network_mode` module path.
- No encoding of one-shot/session lifetime, capture/publish, command lifecycle,
  remount eligibility, or file routing in `WorkspaceProfile`.

### Tests And Verification Commands

```text
rg -n "HostWorkspace|HostNamespaceWorkspaceRequest|WorkspaceModeContext|WorkspaceModeManager|ExecTarget::Host|ExecTarget::IsolatedNetwork|IsolatedNetworkError|network_mode" crates/daemon/workspace/src crates/daemon/operation/src crates/daemon/operation_service/src crates/daemon/core/src
rg -n "one.shot|one_shot|publish|published|remountable|cgroup|ResourcePolicy" crates/daemon/workspace/src/profile crates/daemon/operation/src/command crates/daemon/operation_service/src/command
rg -n "FreshNs|namespace_fds: None|NetworkMode::Host" crates/daemon/command/src crates/daemon/operation_service/src crates/daemon/core/src
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p workspace
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p workspace
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_exec
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_remount
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service workspace_remount
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p daemon
cargo fmt --check
git diff --check
```

The `rg` commands are evidence scans. Every remaining match must be classified as
target code, temporary compatibility, test fixture, or bug before acceptance.

### Risks And Rollback Notes

- Risk: moving cgroup behavior out of `IsolatedProfile` changes launch ordering.
  Roll back by preserving the existing ordering while extracting ownership into a
  common helper with identical behavior.
- Risk: removing host-only lifecycle shortcuts breaks one-shot command creation.
  Roll back by keeping a private compatibility adapter that returns the common
  handle/context shape and records its removal criteria.
- Risk: file-operation routing is too large for this milestone. Roll back by
  defining the profile-neutral routing invariant and adding a Milestone 7/M8
  blocker; do not leave routing as an implicit profile asymmetry.

### Acceptance Criteria

- [ ] Host-compatible and isolated workspaces share one create/setup/teardown
  sequence.
- [ ] Both profiles produce one handle/context shape.
- [ ] Cgroup create, holder join, command join, teardown, and recovery cleanup
  are common and profile-neutral.
- [ ] `WorkspaceProfile` or profile hooks cannot mutate common lifecycle policy
  directly.
- [ ] `HostWorkspace` is not a permanent public target abstraction.
- [ ] One-shot versus persistent lifetime is owned outside profiles.
- [ ] Capture/publish policy is owned outside profiles.
- [ ] Command lifecycle is owned outside profiles.
- [ ] Remount eligibility is owned outside profiles.
- [ ] File-operation routing policy is owned outside profiles.
- [ ] The only accepted profile-specific difference is host network access versus
  isolated network namespace, veth, DNS rewrite, and isolated net-ready setup.

### Milestone 7 Handoff

Milestone 7 daemon dispatch migration must not depend on `HostWorkspace`,
`operation::command::ExecTarget`, or `WorkspaceRuntime` profile routing as target
architecture. It should call operation-service command/file/remount owners with
profile-neutral workspace session handles.

## Milestone 7: Daemon Dispatch Migration Away From WorkspaceRuntime

### Objective

Move command request dispatch from `WorkspaceRuntime` plus `operation::command`
to `operation_service::OperationServices`. The daemon request entrypoint should
parse wire requests, build operation-service inputs and call contexts, call
operation services, and shape protocol responses.

### Implementation Record Workflow

- [ ] Before starting, read
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  and carry forward unresolved notes from earlier milestones.
- [ ] At start, create or append the Milestone 7 entry in the implementation
  record.
- [ ] Before marking this milestone complete, update that entry with files
  changed, verification commands/results, design deviations, unresolved issues,
  and handoff notes for Milestone 8.

### Files And Modules Expected To Change

- `crates/daemon/core/src/op_adapter/command.rs`
- `crates/daemon/core/src/runtime/services.rs`
- `crates/daemon/core/src/runtime/context.rs`
- `crates/daemon/core/src/transport/server.rs`
- `crates/shared/protocol/src/catalog.rs`
- `crates/daemon/core/tests/unit/command/*`
- `crates/e2e-test/tests/workspace-runtime-command/*` or replacement command
  E2E tests
- Temporary compatibility files in `crates/daemon/operation/src/command` only
  if needed

### Expected Structure After This Milestone

```text
daemon/core/src/op_adapter/command.rs
  parse wire args
  build operation_service::command inputs
  build CommandCallContext
  call RuntimeServices.operation.command/remount dispatch
  shape wire responses

daemon/core/src/runtime/services.rs
  owns operation_service::OperationServices
  no command request lifecycle ownership
```

### Struct, Type, And Field Contracts

`RuntimeServices` target shape during this milestone:

```rust
pub struct RuntimeServices {
    pub operation: operation_service::OperationServices,
    pub commit_options: CommitOptions,
    pub plugin: PluginRuntime,
}
```

- `operation`: owns workspace, command, and remount services.
- `commit_options`: retained only if other daemon code still needs it during
  migration.
- `plugin`: unchanged.
- `command: Arc<CommandOps>` and `workspace: WorkspaceRuntime` are removed or
  quarantined behind temporary compatibility only until M8.

Daemon command parser target:

```rust
struct WireExecCommandRequest {
    caller_id: CallerId,
    workspace_root: PathBuf,
    workspace_id: Option<WorkspaceId>,
    cmd: String,
    cwd: Option<PathBuf>,
    timeout_seconds: Option<f64>,
    yield_time_ms: Option<u64>,
}
```

- Wire parsing may temporarily accept legacy root fields outside the command
  service, but the operation-service input must contain only `workspace_root`.
- Trace/request ids stay in `CommandCallContext`/trace context, not
  `ExecCommandInput`.

### Service Method Contracts

| Method | Contract |
| --- | --- |
| `op_exec_command(input, context)` | Purpose: parse wire exec request and call `RuntimeServices.operation.command.exec_command(exec_input, command_call_context)`. Inputs: wire input plus dispatch context. Outputs/errors: protocol JSON response or daemon error. Boundary rules: no `WorkspaceRuntime::route_command_context`, no `command::StartCommand`, no `operation::command::ExecTarget`, and no call through the temporary `OperationServices::exec_command` shim. Tests: exec with `workspace_id: Some` resolves through command service; `None` one-shot works. |
| `command_write_stdin(input, context)` | Purpose: call `CommandOperationService::write_stdin`. Inputs: wire command id/chars/yield. Outputs/errors: response JSON. Boundary rules: build `CommandCallContext` from dispatch context caller; no workspace handler. Tests: unauthorized caller rejected. |
| `command_read_progress` / `command_read_lines` | Purpose: map legacy poll and row-read surfaces to command service. Inputs: command id plus poll/read args. Outputs/errors: legacy or row response. Boundary rules: no collect-completed side channel. Tests: existing poll behavior and new row behavior. |
| `command_cancel(input, context)` | Purpose: call command service cancel. Inputs: command id. Outputs/errors: response JSON. Boundary rules: command service handles remount cancellation token. Tests: cancel during running and remount-pending states. |
| `RuntimeServices::new(...)` | Purpose: build workspace resource service, workspace manager, command service, remount service, and operation services. Inputs: daemon config. Outputs/errors: service graph. Boundary rules: does not construct `WorkspaceRuntime` in target. Tests: service graph fields are present. |
| Background finalizer task | Purpose: drive internal finalizer supervisor if it needs daemon runtime scheduling. Inputs: service handle. Outputs/errors: pushes trace records if applicable. Boundary rules: not a public `advance_active_commands_once` API. Tests: completed command finalizes without public collect/advance. |

### Implementation Steps

- [ ] Introduce `operation_service` into daemon core service wiring.
- [ ] Update command op adapter to build `ExecCommandInput` with only
   `workspace_root`.
- [ ] Remove `WorkspaceRuntime::route_command_context` from command dispatch.
- [ ] Replace `command::StartCommand`, `WriteStdin`, `ReadCommandProgress`, and
   `CancelCommand` use at the daemon command adapter with operation-service
   contracts.
- [ ] Route poll/read/cancel/write through `CommandOperationService`.
- [ ] Remove daemon command op paths for collect/count or mark them unavailable
   according to the protocol migration decision.
- [ ] Update protocol catalog schemas from `operation.command.*` to
   `operation_service.command.*` once wire contracts are migrated.
- [ ] Move command finalizer scheduling into command service or daemon background
   task without exposing `advance_active_commands_once`.
- [ ] Add focused daemon unit tests and, after packaging, live command E2E if
   daemon behavior changed.

### Explicit Exclusions

- No new `WorkspaceRuntime` compatibility layer.
- No command-service API that mirrors old daemon op adapter names.
- No request correlation fields in operation-service exec input.
- No collect/count public command-service APIs.
- No broad file/plugin/checkpoint migration beyond what is needed to remove
  command dispatch from `WorkspaceRuntime`.

### Tests And Verification Commands

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p daemon command
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_exec
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p daemon
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
cargo fmt --check
git diff --check
```

Conditional live E2E after packaging:

```text
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p e2e-test workspace-runtime-command
```

### Risks And Rollback Notes

- Risk: wire compatibility for existing `layer_stack_root` callers breaks before
  clients migrate. Roll back by keeping legacy parsing in the daemon adapter,
  but convert to `workspace_root` before constructing `ExecCommandInput`.
- Risk: background finalization loses trace records previously emitted by
  `advance_active_commands_once`. Roll back by adding an internal finalizer
  trace sink, not a public command-service method.
- Risk: `WorkspaceRuntime` is still needed by non-command adapters. Keep command
  dispatch independent in M7 and finish runtime cleanup in M8.

### Acceptance Criteria

- [ ] `daemon/core/src/op_adapter/command.rs` no longer imports
  `operation::command::CommandOps`, `command::StartCommand`, or
  `WorkspaceRuntime` for command execution.
- [ ] Command exec/write/read/poll/cancel route through operation-service methods.
- [ ] The daemon adapter builds `CommandCallContext` for every command operation.
- [ ] `ExecCommandInput` created by daemon dispatch contains only `workspace_root`
  as root path.
- [ ] Collect/count are no longer command-service-backed public operations.
- [ ] The internal finalizer does not require a public
  `advance_active_commands_once`.

## Milestone 8: Compatibility Wrapper Cleanup And Final Gates

### Objective

Delete or collapse old command/runtime architecture after daemon dispatch has
moved. This is the final review gate that proves Phase 2 target boundaries are
real rather than parallel to the old system. This milestone also removes
temporary scaffold left by earlier milestones once the real target behavior has
landed.

### Implementation Record Workflow

- [ ] Before starting, read
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  and carry forward unresolved notes from earlier milestones.
- [ ] At start, create or append the Milestone 8 entry in the implementation
  record.
- [ ] Before marking this milestone complete, update that entry with files
  changed, verification commands/results, design deviations, unresolved issues,
  and final Phase 2 readiness notes.

### Files And Modules Expected To Change

- `crates/daemon/operation/src/command/*`
- `crates/daemon/core/src/runtime/workspace.rs`
- `crates/daemon/core/src/runtime/*`
- `crates/daemon/core/src/lib.rs`
- `crates/daemon/core/src/op_adapter/*`
- `crates/shared/protocol/src/catalog.rs`
- Old tests under `crates/daemon/operation/tests/command/*` and
  `crates/daemon/core/tests/unit/workspace_runtime.rs`
- New/updated tests under `operation_service`, daemon core, and E2E

### Expected Structure After This Milestone

```text
crates/daemon/operation/src/command/
  removed, or only temporary re-export shims with no policy ownership

crates/daemon/core/src/runtime/
  no WorkspaceRuntime command routing
  no public command lifecycle background advance API

crates/daemon/operation_service/src/
  command/
  workspace_manager/
  workspace_remount/
```

### Struct, Type, And Field Contracts

Removed target-incompatible contracts:

- `operation::command::CommandOps`
- `operation::command::ExecTarget`
- `operation::command::HostCommandWorkspace`
- old caller-primary `operation::command::CommandRegistry`
- old `operation::command::contract::ExecCommandInput`
- old `command::StartCommand` as operation-service contract
- `command::CollectCompleted` as daemon command-service API
- `WorkspaceRuntime` as command routing/remount owner

Retained target contracts:

- `operation_service::command::CommandOperationService`
- `operation_service::workspace_manager::WorkspaceManagerService`
- `operation_service::workspace_remount::WorkspaceRemountService`
- `command::CommandProcess` and other policy-free substrate types

### Service Method Contracts

| Method | Contract |
| --- | --- |
| Removed `CommandOps::collect_completed` | Purpose: none in target. Inputs/outputs: removed. Boundary rule: completed command polling/reads use command id and retained completion store. Tests: static search and compile fail if called. |
| Removed `CommandOps::count_by_caller` / `count_commands` | Purpose: none as public command service API. Inputs/outputs: removed. Boundary rule: internal pressure counts may be private scans but not public API. Tests: protocol/daemon no longer call command count for command-service behavior. |
| Removed `CommandOps::advance_active_commands_once` | Purpose: replaced by internal finalization supervisor. Inputs/outputs: removed. Boundary rule: no public command-service finalization tick. Tests: running command finalizes after exit without public advance. |
| Removed `WorkspaceRuntime::route_command_context` | Purpose: replaced by `RuntimeServices.operation.command.exec_command(...)` plus command-service workspace resolve/create. Inputs/outputs: removed. Boundary rule: daemon dispatch does no workspace command routing. Tests: static search and daemon command tests. |
| Removed `WorkspaceRuntime` remount test hooks | Purpose: replaced by `WorkspaceRemountService` test hooks. Inputs/outputs: removed. Boundary rule: remount orchestration owned by operation service. Tests: remount unit/E2E tests use new service. |

### Implementation Steps

- [ ] Remove old `operation::command` implementation modules or reduce them to
   temporary re-exports that do not own policy.
- [ ] Remove temporary operation-service scaffold that is no longer needed:
   placeholder `NotImplemented` variants, stale `#[allow(dead_code)]` field
   suppressions, unused constructor-only helpers, and tests that only proved
   earlier compile scaffolding.
- [ ] Remove old daemon command adapter imports and tests that assert old contract
   details.
- [ ] Remove `WorkspaceRuntime` command routing and remount orchestration.
- [ ] Migrate any remaining file/isolation references that block deletion of
   `daemon/core/runtime` to operation-service workspace/remount surfaces or a
   separate non-command owner, as required by the Phase 2 hard target.
- [ ] Update protocol catalog to remove or replace collect/count entries and
   operation schema names.
- [ ] Add static boundary checks using `rg` in the final PR checklist.
- [ ] Run the four adversarial review lanes from the cross-milestone cleanup
   discipline: registry/binding, process/completion, contract/boundary, and
   tests/record.
- [ ] Run focused package checks, then live daemon E2E if behavior changed.

### Explicit Exclusions

- No broad unrelated refactors.
- No preserving `WorkspaceRuntime` under a new name.
- No old command API compatibility if it keeps policy ownership outside
  `operation_service::command`.
- No reintroduction of request ids or `remountable` in target command input.

### Tests And Verification Commands

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p command
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p workspace
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p daemon
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service workspace_remount
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p daemon command
cargo fmt --check
git diff --check
```

Static boundary checks:

```text
rg -n "collect_completed|count_by_caller|advance_active_commands_once" crates/daemon/operation_service crates/daemon/core
rg -n "WorkspaceRuntime|route_command_context|ExecTarget|HostCommandWorkspace" crates/daemon/core crates/daemon/operation_service
rg -n "request_id|trace_id|remountable|layer_stack_root" crates/daemon/operation_service/src/command
rg -n "operation::command" crates/daemon/operation_service crates/daemon/core/src/op_adapter/command.rs
rg -n "allow\\(dead_code\\)|NotImplemented" crates/daemon/operation_service/src
```

Conditional live E2E:

```text
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p e2e-test workspace-runtime-command
```

### Risks And Rollback Notes

- Risk: deleting `WorkspaceRuntime` exposes unrelated file/isolation migration
  gaps. Roll back by splitting non-command cleanup into a clearly documented
  compatibility shim that does not own command lifecycle, then remove it before
  final Phase 2 acceptance.
- Risk: protocol catalog removal breaks clients still calling collect/count.
  Roll back by returning explicit unsupported/deprecated daemon errors outside
  command service, not by restoring command-service APIs.
- Risk: old tests mask target behavior by continuing to use `operation::command`.
  Roll back by moving tests to `operation_service` and deleting old imports.

### Acceptance Criteria

- [ ] `CommandOperationService`, `WorkspaceManagerService`, and
  `WorkspaceRemountService` are the target operation-service owners.
- [ ] `WorkspaceRuntime` is not preserved as command routing or remount
  compatibility architecture.
- [ ] `operation_service::command` does not depend on `operation::command`.
- [ ] `command` crate does not depend on `workspace`, `layerstack`, `operation`, or
  `operation_service`.
- [ ] `workspace` and `layerstack` do not depend on `operation_service`.
- [ ] `CommandOperationService` does not expose collect/count/advance APIs.
- [ ] `ExecCommandInput` still has only `workspace_root` as root path and no request
  correlation or `remountable` fields.
- [ ] Daemon command dispatch calls operation-service methods directly.
- [ ] Earlier milestone scaffold has either been removed or documented as
  intentionally retained target surface.
- [ ] No stale `#[allow(dead_code)]` suppressions or placeholder
  `NotImplemented` errors remain in operation-service command or remount code.
- [ ] Final static boundary checks pass or have only documented false positives in
  migration notes.

## Cross-Milestone Dependency Map

```text
M1 scaffolding
  -> M2 registry/store
  -> M3 exec Some/None and ownership
  -> M4 finalization
  -> M5 row projection
  -> M6 remount orchestration
  -> M6.5 exec command boundary
  -> M6.6 workspace profile symmetry
  -> M7 daemon dispatch migration
  -> M8 cleanup/final gates
```

Detailed dependencies:

- M2 depends on M1 service/module structure.
- M3 depends on M2 registry/store because exec must bind command id to
  workspace id and validate command-id ownership.
- M4 depends on M3 exec records and M2 completion store.
- M5 depends on M2/M4 transcript retention and completed record ownership.
- M6 depends on M2 registry workspace scans and M3 command-id lifecycle state.
- M6.5 depends on M6 remount admission and command-service state.
- M6.6 depends on M6.5 command-service ownership and the workspace profile
  lifecycle surface.
- M7 depends on M3/M4 command behavior, M6 remount behavior, and M6.6 profile
  symmetry if workspace sessions are visible through daemon commands.
- M8 depends on M7 dispatch migration and all target behavior passing focused
  tests.

## Final Integration Gates

Architecture gates:

- `OperationServices` exposes `workspace`, `command`, and `remount`.
- `WorkspaceRemountService` owns cross-service remount orchestration.
- `WorkspaceManagerService` has no command-service imports.
- `CommandOperationService` owns command lifecycle and command-side remount
  quiesce.
- `CommandRegistry` has exactly one binding map:
  `HashMap<CommandId, WorkspaceId>`.
- No public or crate-public command exec accepts `Option<WorkspaceSessionHandler>`.
- Stdin/read/poll/cancel are command-id based and authorize through
  `CommandCallContext`.
- `ExecCommandInput` has `workspace_root` as its only root path.
- `ExecCommandInput` has no request correlation ids and no `remountable`.
- Persistent session command finalization does not publish, destroy, or update
  session snapshot/layer metadata.
- One-shot finalization is `OneShotPublishThenDestroy` and uses
  `CommandFinalizationOptions` only for publish/capture policy.
- Completed command state is retained in `CommandCompletionStore`, not
  `CommandRegistry`, and retained reads/polls authorize by
  `CompletedCommandRecord.caller_id`.
- Successful host one-shot commands publish generic `CapturedWorkspaceChanges`
  upperdir deltas; persistent session changed-path metadata, if returned, comes
  only from a bounded non-mutating scan.
- Phase 2 remount uses full mounted-snapshot compaction and
  `RemountCancellationToken` prevents cancellation from killing a stopped
  process group before the remount guard resumes it.
- Host-compatible and isolated workspace profiles differ only by network setup:
  host network access versus isolated network namespace, veth, DNS rewrite, and
  isolated net-ready setup.
- Holder, namespace FD, scratch, cgroup, caller-owned lifetime,
  capture/publish, command lifecycle, remountability, and file routing concerns
  are common and profile-neutral.
- `collect_completed`, `count_commands`/`count_by_caller`, and
  `advance_active_commands_once` are not public Phase 2 command-service APIs.
- `WorkspaceRuntime`, `daemon/core/runtime`, and
  `operation::command::contract` are not target architecture.

Compile/test gates:

```text
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p command
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p workspace
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p daemon
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service workspace_remount
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p daemon command
cargo fmt --check
git diff --check
```

Conditional live E2E gate:

```text
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p e2e-test workspace-runtime-command
```

## Open Questions And Blockers

1. Where should the durable `CommandId` type live?
   Recommendation: keep it in `operation_service::command` for Phase 2 unless
   a lower shared protocol/common crate is needed by daemon wire schemas. Do not
   depend on `operation` from `operation_service`.

2. What is the exact wire migration path from legacy `layer_stack_root` command
   exec requests to target `workspace_root` requests?
   Recommendation: keep any legacy parsing in daemon dispatch only, translate to
   `workspace_root` before constructing `ExecCommandInput`, and never add
   `layer_stack_root` to operation-service command contracts.

3. Should row-oriented command output be exposed through the existing
   `sandbox.command.poll` response or a sibling local_os-specific command read
   operation?
   Recommendation: implement `read_lines` internally first, preserve legacy poll
   while daemon/client migration is decided, then add/update the wire surface in
   M7.

4. What exact typed daemon error should represent
   `workspace_remount_pending`?
   Recommendation: add a stable command-service error code and let daemon
   response shaping map it to retryable invalid-state/error metadata.

5. Should the finalizer supervisor run inside `CommandOperationService` or as a
   daemon background task over a private method?
   Recommendation: prefer service-owned registration where possible. If daemon
   scheduling is required, keep the callable `pub(crate)` and do not expose a
   public `advance_active_commands_once` equivalent.

6. The referenced TypeScript local_os tool files are not present in this
   checkout. The live repo has daemon E2E coverage for legacy command stdout and
   command count behavior instead.
   Recommendation: add Rust operation-service row-projection tests first, then
   add or update client/E2E coverage once the local_os source surface is present.
