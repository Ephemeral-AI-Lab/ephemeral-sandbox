# Phase 2 Command Service Implementation Record

This document is the handoff log for
`phase_2_command_service_IMPLEMENTATION_PLAN.md`.

Rules:

- Keep entries append-friendly; do not delete prior milestone evidence.
- Before starting Milestones 2-8, read earlier entries and carry forward
  unresolved notes.
- Before marking any milestone complete, update that milestone's entry with files
  changed, verification commands/results, deviations from the plan, unresolved
  issues, and next-milestone handoff notes.

## Milestone 1: Operation-Service Scaffolding And Contracts

- Status: Complete.
- Files changed:
  - `Cargo.lock`
  - `crates/daemon/operation_service/Cargo.toml`
  - `crates/daemon/operation_service/src/lib.rs`
  - `crates/daemon/operation_service/src/error.rs`
  - `crates/daemon/operation_service/src/services.rs`
  - `crates/daemon/operation_service/src/workspace_manager/mod.rs`
  - `crates/daemon/operation_service/src/workspace_manager/error.rs`
  - `crates/daemon/operation_service/src/workspace_manager/service.rs`
  - `crates/daemon/operation_service/src/workspace_manager/session_manager.rs`
  - `crates/daemon/operation_service/src/command/mod.rs`
  - `crates/daemon/operation_service/src/command/contract.rs`
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/registry.rs`
  - `crates/daemon/operation_service/src/command/process_store.rs`
  - `crates/daemon/operation_service/src/command/service.rs`
  - `crates/daemon/operation_service/src/command/exec.rs`
  - `crates/daemon/operation_service/src/command/remount.rs`
  - `crates/daemon/operation_service/src/workspace_remount/mod.rs`
  - `crates/daemon/operation_service/src/workspace_remount/error.rs`
  - `crates/daemon/operation_service/src/workspace_remount/service.rs`
  - `crates/daemon/operation_service/tests/service_graph.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p command`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service --test service_graph`:
    passed, 4 tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service --test workspace_manager`:
    passed, 9 tests.
  - `cargo fmt --check`: passed.
  - `git diff --check`: passed.
- Deviations: None. Behavior remains stubbed/constructor-only; daemon dispatch,
  old `operation::command` wrappers, command execution, remount, capture, and
  finalization behavior were not migrated.
- Unresolved issues: None for Milestone 1.
- Handoff notes: Milestone 2 should replace the skeleton registry/store with the
  target command-id keyed registry and active/completed process-store behavior.
  The workspace manager module is named `workspace_manager` so it is parallel to
  `workspace_remount` and distinct from the low-level `workspace` crate.
  `OperationTraceContext` is currently an empty command-contract placeholder
  because no existing operation-service trace context type was present in the
  checkout; it does not expose request or trace identifiers.

## Milestone 2: Command Service Registry/Process-Store Split

- Status: Complete.
- Files changed:
  - `crates/daemon/operation_service/src/command/mod.rs`
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/registry.rs`
  - `crates/daemon/operation_service/src/command/process_store.rs`
  - `crates/daemon/operation_service/tests/command_registry.rs`
  - `crates/daemon/operation_service/tests/command_process_store.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_registry`:
    passed, 3 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_process_store`:
    passed, 6 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service`:
    passed, 28 tests.
  - `cargo fmt --check`: passed.
  - `git diff --check`: passed.
- Deviations: `CommandRegistry` wraps its single
  `HashMap<CommandId, WorkspaceId>` in a mutex for shared service access. It has
  no caller index, no workspace index, no process handles, and no completed
  command storage.
- Unresolved issues: None for Milestone 2.
- Handoff notes: Milestone 3 should use `CommandProcessStore::allocate_command_id`
  and `try_reserve`, bind commands in `CommandRegistry`, then call
  `CommandProcessStore::insert_active(reservation, record)` so the store consumes
  the reservation only after active insert succeeds. Terminal state should move
  through `CommandProcessStore::complete_active(record)` so completed retention is
  recorded before the active slot is released. `CommandCompletionStore` now
  retains `caller_id` and `workspace_id`, but read/poll/stdin/cancel
  authorization remains a Milestone 3 behavior. Command execution, finalization,
  remount quiesce, daemon dispatch migration, and `operation::command` wrapper
  changes remain untouched.

### Post-Milestone 2 Cleanup Review

- Status: Complete.
- Files changed:
  - `crates/daemon/operation_service/src/command/service.rs`
  - `crates/daemon/operation_service/src/command/registry.rs`
  - `crates/daemon/operation_service/src/workspace_remount/service.rs`
  - `crates/daemon/operation_service/src/workspace_remount/error.rs`
  - `crates/daemon/operation_service/src/workspace_manager/session_manager.rs`
  - `crates/daemon/operation_service/tests/service_graph.rs`
  - `crates/daemon/operation_service/tests/workspace_manager.rs`
  - `crates/daemon/layerstack/src/lease_aware.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo clippy -p operation_service --all-targets --no-deps -- -D warnings`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service`:
    passed, 28 tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_registry`:
    passed, 3 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_process_store`:
    passed, 6 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p layerstack lease_aware`:
    passed, 16 matching tests.
- Cleanup notes: Removed scaffold `dead_code` allowances from command/remount
  service dependencies by adding explicit accessors and test coverage. Derived
  `CommandFinalizationOptions::default`, rewrote the registry workspace scan to
  satisfy clippy, removed stale milestone-specific placeholder error wording,
  replaced operation-service test unwraps with expectations, and removed a
  redundant `#[must_use]` from a layerstack iterator accessor.
  Private session-manager caller/lease lookup helpers remain covered by unit
  tests but are now compiled only for tests until live service behavior needs
  them.
- Unresolved issues: Full
  `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo clippy -p operation_service --all-targets -- -D warnings`
  still stops in the `workspace` dependency on pre-existing lints
  (`workspace/src/model.rs:176` derivable default and
  `workspace/src/network_mode/host.rs:178` too many arguments). Those were left
  untouched because the second item is an API-shape refactor outside this
  command-service cleanup.

### Post-Adversarial Review Fixes

- Status: Complete.
- Files changed:
  - `crates/daemon/operation_service/src/command/contract.rs`
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/registry.rs`
  - `crates/daemon/operation_service/src/command/process_store.rs`
  - `crates/daemon/operation_service/tests/command_process_store.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_IMPLEMENTATION_PLAN.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `cargo fmt --check`: passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_registry`:
    passed, 4 matching tests including the unit structural test.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_process_store`:
    passed, 8 matching tests including the unit duplicate-completion test.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service`:
    passed, 31 tests.
  - `rg -n "WorkspaceRuntime|CommandOps|ExecTarget|InternalHostOneShot|StartCommand|CollectCompleted|layer_stack_root|request_id|trace_id|invocation_id|remountable|trace::TraceId|trace::RequestId|RequestId|TraceId|collect_completed|count_by_caller|count_commands|advance_active_commands_once|by_workspace|by_caller|workspace_index|caller_index" crates/daemon/operation_service/src/command crates/daemon/operation_service/tests/command_registry.rs crates/daemon/operation_service/tests/command_process_store.rs crates/daemon/operation_service/tests/service_graph.rs`:
    no matches.
- Fix notes: `OperationTraceContext` remains as an empty placeholder and no
  longer exposes request or trace identifier types. `CommandProcessStore` now
  consumes a `CommandReservation` in `insert_active`, rejects reservations from a
  different process store, and exposes `complete_active` for the active-to-completed
  transition so completed retention is recorded before active capacity is released.
  `CommandRegistry` has a unit structural test that destructures the type without
  `..`, so adding a secondary index now fails the test build.
- Unresolved issues: None for the adversarial review findings.

## Milestone 3: Exec Some/None Flows And Caller Ownership

- Status: Partial. Ownership/admission and Some/None mode selection are
  implemented; real process launch and yield waiting remain open.
- Files changed:
  - `crates/daemon/operation_service/src/command/contract.rs`
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/exec.rs`
  - `crates/daemon/operation_service/src/command/mod.rs`
  - `crates/daemon/operation_service/src/command/process_store.rs`
  - `crates/daemon/operation_service/src/command/remount.rs`
  - `crates/daemon/operation_service/src/command/service.rs`
  - `crates/daemon/operation_service/src/services.rs`
  - `crates/daemon/operation_service/src/workspace_manager/service.rs`
  - `crates/daemon/operation_service/tests/command_exec.rs`
  - `crates/daemon/operation_service/tests/command_ownership.rs`
  - `crates/daemon/operation_service/tests/command_process_store.rs`
  - `crates/daemon/operation_service/tests/service_graph.rs`
  - `crates/daemon/operation_service/tests/support/mod.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_IMPLEMENTATION_PLAN.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_exec`:
    passed, 8 matching tests including rollback/root-mismatch unit coverage.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_ownership`:
    passed, 6 matching tests across active and completed ownership paths.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service`:
    passed, 48 tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo clippy -p operation_service --all-targets --no-deps -- -D warnings`:
    passed.
  - `cargo fmt --check`: passed.
  - `git diff --check`: passed.
  - `rg -n "WorkspaceRuntime|CommandOps|ExecTarget|InternalHostOneShot|StartCommand|CollectCompleted|layer_stack_root|request_id|trace_id|invocation_id|remountable|trace::TraceId|trace::RequestId|RequestId|TraceId|collect_completed|count_by_caller|count_commands|advance_active_commands_once|by_workspace|by_caller|workspace_index|caller_index" crates/daemon/operation_service/src/command crates/daemon/operation_service/tests/command_exec.rs crates/daemon/operation_service/tests/command_ownership.rs`:
    no matches.
- Deviations: Milestone 3 active records still use `command::CommandProcess::new`
  instead of real process spawning. The current `WorkspaceSessionHandler` only
  exposes the resource-facing workspace handle and snapshot paths, not the
  policy-free namespace/overlay launch material needed to spawn through the
  low-level command crate without importing old `operation::command` routing
  policy. The M3 plan is no longer marked complete until a policy-free launch
  context and `command::yield_wait_loop` integration are added.
- Unresolved issues: Real launch/yield behavior is still open. Ownership,
  rollback, active/completed authorization, and private one-shot id exposure
  findings from the M3 adversarial review have been remediated.
- Cleanup notes: Replaced the host-specific
  `WorkspaceManagerService::create_private_host_workspace` helper with generic
  `create_private_workspace(caller_id, workspace_root, network)` so the
  workspace manager does not imply a missing isolated twin. Removed the unused
  local exec yield-time binding left over from the not-yet-implemented real
  spawn/yield wait path. Updated the Phase 2 implementation-plan checklist for
  completed Milestone 3 items while keeping the milestone itself open for launch
  preparation/yield waiting. Added start-failure one-shot cleanup, direct
  command-service root-mismatch validation, active-to-completed ownership
  validation, service-owned completion/unbind coordination, and removed public
  service registry/process-store accessors so one-shot workspace ids are not
  recoverable through the command service.
- Handoff notes: Milestone 4 should replace the process-free active record path
  with the scoped finalization-aware launch/finalization flow once the service
  has a policy-free launch adapter. The completed-record authorization path is
  ready for read/poll behavior: service methods authorize active records first,
  then retained completed records by `CompletedCommandRecord.caller_id`, and
  completion now validates caller/workspace ownership against the active record
  before retaining the completed record.
  One-shot commands call `WorkspaceManagerService::create_private_workspace`
  with `NetworkMode::Host`, which keeps the temporary workspace-create adapter
  out of command-service contracts without adding a host-specific workspace
  manager API.

## Milestone 4: One-Shot Finalization And Persistent-Session Semantics

- Status: Not started
- Files changed:
- Verification:
- Deviations:
- Unresolved issues:
- Handoff notes:

## Milestone 5: Local OS Row Projection

- Status: Not started
- Files changed:
- Verification:
- Deviations:
- Unresolved issues:
- Handoff notes:

## Milestone 6: WorkspaceRemountService And Remount-Pending State

- Status: Not started
- Files changed:
- Verification:
- Deviations:
- Unresolved issues:
- Handoff notes:

## Milestone 7: Daemon Dispatch Migration Away From WorkspaceRuntime

- Status: Not started
- Files changed:
- Verification:
- Deviations:
- Unresolved issues:
- Handoff notes:

## Milestone 8: Compatibility Wrapper Cleanup And Final Gates

- Status: Not started
- Files changed:
- Verification:
- Deviations:
- Unresolved issues:
- Handoff notes:
