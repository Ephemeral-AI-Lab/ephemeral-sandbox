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
  - `crates/daemon/operation_service/src/workspace_remount/mod.rs`
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

- Status: Complete. Ownership/admission and Some/None mode selection are
  implemented; real process launch and yield waiting are split into Milestone
  3.5.
- Files changed:
  - `crates/daemon/operation_service/src/command/contract.rs`
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/exec.rs`
  - `crates/daemon/operation_service/src/command/mod.rs`
  - `crates/daemon/operation_service/src/command/process_store.rs`
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
- Deviations: At Milestone 3 completion, active records still used a process-free
  command scaffold because Milestone 3.5 owned the policy-free real spawn and
  initial-yield slice. Milestone 3.5 later replaced that launch path with
  `command::CommandProcess::spawn`.
- Unresolved issues: None for Milestone 3 ownership/admission. Real launch/yield
  behavior is tracked by Milestone 3.5. Ownership, rollback, active/completed
  authorization, and private one-shot id exposure findings from the M3
  adversarial review have been remediated.
- Cleanup notes: Replaced the host-specific
  `WorkspaceManagerService::create_private_host_workspace` helper with generic
  `create_private_workspace(caller_id, workspace_root, network)` so the
  workspace manager does not imply a missing isolated twin. Removed the unused
  local exec yield-time binding left over from the not-yet-implemented real
  spawn/yield wait path. Updated the Phase 2 implementation-plan checklist for
  completed Milestone 3 ownership/admission items and split real launch/yield
  work into Milestone 3.5. Added start-failure one-shot cleanup, direct
  command-service root-mismatch validation, process-store active-to-completed
  ownership validation, and removed public service registry/process-store
  accessors so one-shot workspace ids are not recoverable through the command
  service.
- Handoff notes: Milestone 3.5 should replace the process-free active record path
  with policy-free spawn and initial yield; that later happened in the Milestone
  3.5 entry below. The completed-record authorization path is ready for read/poll
  behavior: service methods authorize active records
  first, then retained completed records by `CompletedCommandRecord.caller_id`.
  Process-store completion validates caller/workspace ownership against the
  active record before retaining the completed record; Milestone 4 still needs
  to add the live service-owned finalizer transition.
  One-shot commands call `WorkspaceManagerService::create_private_workspace`
  with `NetworkMode::Host`, which keeps the temporary workspace-create adapter
  out of command-service contracts without adding a host-specific workspace
  manager API.

### Post-Milestone 3 Cleanup Review

- Status: Complete.
- Files changed:
  - `crates/daemon/operation_service/src/command/mod.rs`
  - `crates/daemon/operation_service/src/command/remount.rs`
  - `crates/daemon/operation_service/src/error.rs`
  - `crates/daemon/operation_service/src/workspace_remount/mod.rs`
  - `crates/daemon/operation_service/src/workspace_remount/error.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_IMPLEMENTATION_PLAN.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-operation-service-target cargo check -p command`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-operation-service-target cargo check -p operation_service`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-operation-service-target cargo test -p operation_service command_ownership`:
    passed, 6 matching tests.
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-operation-service-target cargo test -p operation_service command_process_store`:
    passed, 10 matching tests.
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-operation-service-target cargo test -p operation_service`:
    passed, 47 tests.
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-operation-service-target cargo clippy -p operation_service --all-targets --no-deps -- -D warnings`:
    passed.
  - `cargo fmt --check`: passed.
  - `rg -n "allow\\(dead_code\\)|expect\\(\\s*dead_code|NotImplemented|WorkspaceRemountError|CommandBindingMissing|complete_active_command|command::remount|mod remount" crates/daemon/operation_service/src crates/daemon/operation_service/tests`:
    no matches.
  - `rg -n "WorkspaceRuntime|CommandOps|ExecTarget|InternalHostOneShot|StartCommand|CollectCompleted|layer_stack_root|request_id|trace_id|invocation_id|remountable|trace::TraceId|trace::RequestId|RequestId|TraceId|collect_completed|count_by_caller|count_commands|advance_active_commands_once|by_workspace|by_caller|workspace_index|caller_index" crates/daemon/operation_service/src/command crates/daemon/operation_service/tests/command_exec.rs crates/daemon/operation_service/tests/command_ownership.rs`:
    no matches.
- Cleanup notes: Removed the empty command-side remount placeholder module and
  its `mod` declaration. Removed the unused test-only
  `complete_active_command` helper rather than keeping production dead-code
  scaffolding for the future finalizer. Removed the unused `WorkspaceRemountError`
  `NotImplemented` scaffold and the top-level conversion variant because no
  workspace-remount service method returns it yet. Milestone 6 should create
  `command/remount.rs` and any remount-specific errors only when command-side
  quiesce behavior is implemented.
- Unresolved issues: Milestone 4 still needs to add the real service-owned
  finalizer transition and registry unbind when live command finalization is
  implemented.

## Milestone 3.5: Policy-Free Command Launch And Initial Yield

- Status: Complete.
- Files changed:
  - `Cargo.lock`
  - `crates/daemon/command/Cargo.toml`
  - `crates/daemon/command/src/launch.rs`
  - `crates/daemon/command/src/lib.rs`
  - `crates/daemon/command/src/process.rs`
  - `crates/daemon/command/src/pty.rs`
  - `crates/daemon/command/tests/unit/process.rs`
  - `crates/daemon/operation_service/Cargo.toml`
  - `crates/daemon/operation_service/src/command/exec.rs`
  - `crates/daemon/operation_service/src/command/finalize_tests.rs`
  - `crates/daemon/operation_service/src/command/launch.rs`
  - `crates/daemon/operation_service/src/command/mod.rs`
  - `crates/daemon/operation_service/src/command/process_store.rs`
  - `crates/daemon/operation_service/src/command/service.rs`
  - `crates/daemon/operation_service/src/workspace_manager/session_manager.rs`
  - `crates/daemon/operation_service/tests/command_exec.rs`
  - `crates/daemon/operation_service/tests/command_ownership.rs`
  - `crates/daemon/operation_service/tests/command_process_store.rs`
  - `crates/daemon/operation_service/tests/support/mod.rs`
  - `crates/daemon/operation_service/tests/workspace_manager.rs`
  - `crates/daemon/workspace/src/lib.rs`
  - `crates/daemon/workspace/src/model.rs`
  - `crates/daemon/workspace/tests/unit/model.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_IMPLEMENTATION_PLAN.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_3_5_agent_prompt.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_exec`:
    passed, 7 matching unit tests and 7 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_ownership`:
    passed, 2 matching unit tests and 4 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p command`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
    passed.
  - `cargo fmt --check`: passed.
  - `git diff --check`: passed.
  - `rg -n "operation::command|StartCommand|request_id|trace_id|invocation_id|remountable|CommandProcess::new|WorkspaceRuntime|CommandOps|ExecTarget|InternalHostOneShot" crates/daemon/operation_service/src/command crates/daemon/operation_service/tests/command_exec.rs`:
    no matches; `rg` exited 1 as expected for an empty result set.
  - Supplemental focused checks:
    `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p command`
    passed, 18 unit tests;
    `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_finalize`
    passed, 10 matching unit tests;
    `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_process_store`
    passed, 1 matching unit test and 9 matching integration tests;
    `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p workspace model`
    passed, 4 matching unit tests.
- Cleanup notes:
  - Removed the now-unused `operation_service` dependency on `trace`; `cargo
    machete --with-metadata` reports no unused dependencies.
  - Removed the public process-free `command::CommandProcess::new` scaffold.
    Tests use the explicit `CommandProcess::inactive_for_test` helper, while
    production launch paths go through `CommandProcess::spawn`.
  - Removed the stale Milestone 3.5 agent prompt after the milestone was
    completed and updated the implementation plan to point at this record
    instead of the obsolete prompt.
  - Post-cleanup verification:
    `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p command`
    passed, 18 tests;
    `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service`
    passed, 60 tests;
    `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo clippy -p command --all-targets --no-deps -- -D warnings`
    passed;
    `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo clippy -p operation_service --all-targets --no-deps -- -D warnings`
    passed;
    source-only static scans for `CommandProcess::new`, the old command policy
    terms, and `trace.workspace` returned no matches.
- Deviations:
  - Added `command::launch` as a policy-free helper for typed runner request
    construction. It uses the low-level runner protocol internally, but
    operation-service command contracts still carry no request, trace, or
    invocation identifiers and do not carry the old remount policy flag.
  - Added optional `WorkspaceLaunchContext` material to `WorkspaceHandle`
    because `WorkspaceSessionHandler` previously exposed snapshot/layer facts
    but not the `upperdir`, `workdir`, namespace fd, or cgroup facts required to
    spawn through `command::CommandProcess::spawn`.
  - Added a hidden command launch driver hook for tests. Production
    `CommandOperationService` uses `RealCommandLaunchDriver`, which calls
    `CommandProcess::spawn` and `command::yield_wait_loop::wait_for_yield`; tests
    use a fake driver so operation-service integration tests do not spawn the
    Rust test harness as `ns-runner`.
  - No background finalizer watcher was added. Completed initial yields and
    `poll` use the existing crate-private finalization path.
- Unresolved issues: None for the M3.5 launch/yield slice. Background
  finalizer supervision remains future work; no public collect, advance, row
  window, remount-pending, or daemon dispatch API was added.
- Handoff notes: M3.5 now provides live process insertion with transcript
  artifact paths, policy-free runner request construction, cleanup on
  admission/artifact/spawn/bind/active-insert failures, and first exec responses
  from the command wait loop. The existing M4 finalization path can consume
  completed initial yields and later `poll` exits; future background supervision
  should reuse the same live-process/finalization surface without reopening old
  command policy.

## Milestone 4: One-Shot Finalization And Persistent-Session Semantics

- Status: Complete.
- Files changed:
  - `crates/daemon/operation_service/Cargo.toml`
  - `crates/daemon/operation_service/src/command/contract.rs`
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/exec.rs`
  - `crates/daemon/operation_service/src/command/finalize.rs`
  - `crates/daemon/operation_service/src/command/finalize_tests.rs`
  - `crates/daemon/operation_service/src/command/mod.rs`
  - `crates/daemon/operation_service/src/command/process_store.rs`
  - `crates/daemon/operation_service/src/command/remount.rs`
  - `crates/daemon/operation_service/src/command/service.rs`
  - `crates/daemon/operation_service/src/error.rs`
  - `crates/daemon/operation_service/src/workspace_manager/session_manager.rs`
  - `crates/daemon/operation_service/src/workspace_remount/error.rs`
  - `crates/daemon/operation_service/src/workspace_remount/mod.rs`
  - `crates/daemon/operation_service/tests/command_exec.rs`
  - `crates/daemon/operation_service/tests/command_finalize.rs`
  - `crates/daemon/operation_service/tests/command_ownership.rs`
  - `crates/daemon/operation_service/tests/command_process_store.rs`
  - `crates/daemon/operation_service/tests/support/mod.rs`
  - `crates/daemon/operation_service/tests/workspace_manager.rs`
  - `crates/daemon/workspace/src/model.rs`
  - `crates/daemon/workspace/tests/unit/model.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_IMPLEMENTATION_PLAN.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_SPEC.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p workspace capture`:
    passed, 1 matching test.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_finalize`:
    passed, 10 matching unit tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_completion`:
    passed, 1 matching unit test.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service`:
    passed, 60 tests.
  - `cargo fmt --check`: passed.
  - `git diff --check`: passed.
  - `rg -n "WorkspaceRuntime|CommandOps|ExecTarget|InternalHostOneShot|StartCommand|CollectCompleted|layer_stack_root|request_id|trace_id|invocation_id|remountable|trace::TraceId|trace::RequestId|RequestId|TraceId|collect_completed|count_by_caller|count_commands|advance_active_commands_once|by_workspace|by_caller|workspace_index|caller_index" crates/daemon/operation_service/src/command`:
    no matches. A broader workspace DTO search still finds pre-existing
    `layer_stack_root` fields in `CreateWorkspaceRequest` and
    `owner_request_id` fields in `LatestSnapshotRequest`; those were not added
    by this milestone and are not command-service contract fields.
- Deviations:
  - Background finalizer supervision is still not a daemon thread. Initial exec
    yield and `poll` can finalize completed process exits through the
    crate-private supervisor path, but no public advance, collect, or count API
    was added.
  - `CommandOperationService::finalize_command` is a crate-private supervisor
    entrypoint rather than a daemon request API. It exists so process-exit
    finalization can be tested inside the crate and so service-owned process
    paths can finalize without exposing daemon request surface.
- Unresolved issues:
  - No background finalizer watcher thread is started. The placeholder
    `start_finalizer_watch` no-op was removed in the cleanup pass after M4;
    until background supervision is added, initial exec yield and `poll`
    opportunistically finalize completed process exits.
- Cleanup notes:
  - Removed the no-op finalizer-watch placeholder and tightened direct
    command-service exec validation so `workspace_id` and the resolved session
    handler cannot disagree. Moved root/mode validation into
    `CommandOperationService::exec_command` and removed the duplicate wrapper
    check from `OperationServices::exec_command`.
  - Made `finalize_session_command` and `finalize_one_shot_command` private
    helpers because only the supervisor entrypoint should route finalization.
  - Post-review remediation made `finalize_command` crate-private, moved direct
    finalizer coverage from the integration-test API boundary to
    `crates/daemon/operation_service/src/command/finalize_tests.rs`, authorized
    active failed-finalization reads before returning retained failure details,
    and made `FinalizationState::{ResponseBuffered, WorkspaceDestroyPending,
    Failed}` retain already-decided publish/discard metadata so destroy failures
    report the prior outcome instead of losing it. The owner-visible error
    payload boxes optional finalized metadata so `CommandServiceError` remains
    small enough for clippy's `result_large_err` lint.
  - Post-remediation launch-path fixture cleanup added direct
    `operation_service` ownership of `serde_json`, launch-driver coverage for
    spawn-failure cleanup and initial completed session yield, and local fake
    launch drivers in unit tests that need manual process/finalization control.
  - Cleanup verification:
    - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_exec`:
      passed, 7 matching unit tests and 5 matching integration tests.
    - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_finalize`:
      passed, 10 matching unit tests.
    - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service`:
      passed, 60 tests.
    - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo clippy -p operation_service --all-targets --no-deps -- -D warnings`:
      passed.
    - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
      passed.
    - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p workspace capture`:
      passed, 1 matching test.
    - `cargo fmt --check`: passed.
    - `git diff --check`: passed.
    - Static cleanup/legacy searches over touched command-service code:
      no matches for old command policy terms, public advance/collect/count
      APIs, request/trace/invocation ids, `remountable`, or
      command-service caller/workspace indexes. A broader touched-file search
      still finds expected false positives for workspace-manager
      `layer_stack_root` session fields and `find_by_workspace_id` /
      test-only `find_by_caller_id` helpers; those are not command-service
      contracts, `CommandRegistry` indexes, or completion-store indexes.
- Handoff notes:
  - Milestone 5 can build row projection on the retained
    `CompletedCommandRecord` path. Completed records now keep caller/workspace
    ownership plus finalization metadata, and `poll`/`read_lines` authorize
    retained completed records by `caller_id`.
  - One-shot success finalization captures the generic upperdir delta, publishes
    through `layerstack::service::publish_command_capture_lane_aware`, cleans any
    capture spool directory, records publish metadata, then destroys the private
    workspace. Non-success/cancel/timeout one-shot exits skip capture/publish and
    still record discard plus destroy behavior. Capture/publish/destroy failures
    mark active state as `FinalizationState::Failed` instead of dropping cleanup
    state; destroy failures retain the already-decided Published/Discarded
    metadata for owner-visible reporting.
  - Persistent session finalization records terminal output and metadata without
    calling `WorkspaceManagerService::capture_changes`, publishing, destroying,
    or refreshing session snapshot/layer metadata.

## Milestone 5: Local OS Row Projection

- Status: Complete.
- Files changed:
  - `crates/daemon/operation_service/src/command/contract.rs`
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/mod.rs`
  - `crates/daemon/operation_service/src/command/service.rs`
  - `crates/daemon/operation_service/src/command/transcript.rs`
  - `crates/daemon/operation_service/src/command/exec.rs`
  - `crates/daemon/operation_service/tests/command_transcript_rows.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-review-target cargo test -p operation_service command_transcript_rows`:
    passed, 7 matching integration tests plus 32 filtered unit tests after the
    bounded transcript-window remediation.
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-review-target cargo test -p operation_service command_ownership`:
    passed, 2 matching unit tests and 4 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-review-target cargo test -p operation_service command_finalize`:
    passed, 10 matching unit tests.
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-review-target cargo test -p operation_service`:
    passed, 76 tests.
  - `CARGO_TARGET_DIR=/tmp/eos-cleanup-review-target cargo check -p operation_service`:
    passed.
  - `cargo fmt --all --check`: passed.
  - `git diff --check`: passed.
  - `rg -n "operation::command|StartCommand|request_id|trace_id|invocation_id|remountable|WorkspaceRuntime|CommandOps|ExecTarget|InternalHostOneShot|advance_active_commands_once|collect_completed|count_commands|count_by_caller" crates/daemon/operation_service/src/command crates/daemon/operation_service/tests`:
    one documented false positive in
    `crates/daemon/operation_service/tests/command_exec.rs`, where the test
    asserts the low-level `command` crate runner payload field
    `tool_call.invocation_id`; this is not an operation-service command
    contract field and was pre-existing outside the Milestone 5 row projection
    scope.
  - `rg -n "tool_call|ToolCall|tool call" crates/daemon/linux-namespace-subprocess/src crates/daemon/command/src crates/daemon/workspace/src/namespace crates/daemon/operation/src/command crates/daemon/operation_service/tests docs/daemon/workspace_migration`:
    intentionally not clean. It records sandbox protocol debt in
    `docs/daemon/workspace_migration/tool_call_sandbox_protocol_FINDINGS.md`,
    low-level command/workspace producers, ns-runner consumers, and tests that
    still assert the serialized runner request shape.
- Deviations:
  - Row projection is implemented in
    `operation_service::command::transcript` rather than in the low-level
    `command` crate, so status/authorization/window policy stays with the
    operation-service owner.
  - Current PTY transcript logs are merged raw output and do not retain a native
    stderr marker. Raw PTY rows are therefore projected as
    `CommandStream::Stdout`; the parser also accepts structured JSONL rows with
    explicit `stdout`/`stderr` streams for any future command-service row
    sidecar without changing the public row contract.
  - Active commands may report an empty pending row window before the transcript
    file exists. Retained completed command reads are stricter: a missing or
    unreadable retained transcript path is reported as a command transcript
    error instead of being collapsed into empty output.
  - Superseded by the post-review remediation below: direct command-service exec
    is now crate-internal and treats a supplied session handler only as a
    session marker before re-resolving canonical workspace state.
- Unresolved issues:
  - True stderr fidelity cannot be recovered from existing merged PTY transcript
    logs. A later substrate or row-sidecar change must write structured stream
    rows at capture time if daemon-dispatch local_os parity requires separate
    stderr rows.
- Handoff notes:
  - `CommandLinesOutput` now carries command status, exit code, row offset
    metadata, `truncated_before`, `output_truncated`, and
    `Vec<CommandTranscriptRow>`.
  - `CommandOperationService::read_lines` authorizes active records first, then
    retained completed records by caller id, and reads rows from the retained
    transcript path instead of `read_output_since(0)` or completed stdout.
    Retained completed reads fail if that transcript path is unavailable, so
    lost retained output is distinguishable from true empty output.
  - `poll` and `write_stdin` remain daemon-native and continue to use the
    command process transcript source; no duplicate output store or daemon
    dispatch migration was added.
  - Milestone 6 should keep remount work separate from row projection and can
    rely on completed command records retaining transcript metadata after
    finalization.

### Post-Milestone 5 Adversarial Review Remediation

- Status: Complete.
- Issues fixed:
  - Direct command exec is no longer an external API surface:
    `CommandOperationService::exec_command` is `pub(crate)`, while
    `OperationServices::exec_command` remains public. The internal session path
    now re-resolves the canonical `WorkspaceSessionHandler` from
    `WorkspaceManagerService` and does not trust caller-provided handler launch
    material.
  - Initial yield no longer holds the active-process-store mutex while waiting.
    Active records store `Arc<CommandProcess>`, and `initial_exec_yield` clones
    the process handle before calling the launch driver's wait loop.
  - Active-insert rollback now cancels the spawned process before unbind and
    cleanup. Start-failure cleanup also removes the command artifact directory
    after launch preparation failures that reach spawn/bind/insert handling.
  - `WorkspaceLaunchContext` and `WorkspaceLaunchNamespaceFds` use custom
    `Debug` output that masks launch paths, cgroup paths, and namespace fd
    numbers.
  - Launch-path coverage now includes forged/stale handler launch material,
    missing launch material, artifact-directory setup failure, spawn-part
    propagation, spawn-failure artifact cleanup, direct debug masking, and the
    active-process clone/no-lock behavior.
  - `read_lines` no longer materializes the full command transcript for every
    row-window request. Transcript row projection now retains at most a bounded
    suffix, aligns it to a row boundary, counts unavailable prior rows, and
    reports `truncated_before` / `output_truncated` through the public row
    contract.
- Files changed:
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/exec.rs`
  - `crates/daemon/operation_service/src/command/process_store.rs`
  - `crates/daemon/operation_service/src/command/service.rs`
  - `crates/daemon/operation_service/src/command/transcript.rs`
  - `crates/daemon/operation_service/tests/command_exec.rs`
  - `crates/daemon/operation_service/tests/command_process_store.rs`
  - `crates/daemon/operation_service/tests/command_transcript_rows.rs`
  - `crates/daemon/operation_service/tests/support/mod.rs`
  - `crates/daemon/workspace/src/model.rs`
  - `crates/daemon/workspace/tests/unit/model.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-review-fix-target cargo test -p operation_service`:
    passed, 76 tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-review-fix-target cargo test -p workspace`:
    passed, 23 tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-review-fix-target cargo check -p operation_service`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-review-fix-target cargo check -p workspace`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-review-fix-target cargo clippy -p operation_service --all-targets --no-deps -- -D warnings`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-review-fix-target cargo clippy -p workspace --all-targets --no-deps -- -D warnings`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/ephemeral-os-fix-transcript-all cargo test -p operation_service --all-targets`:
    passed, 78 tests.
  - `CARGO_TARGET_DIR=/tmp/ephemeral-os-fix-transcript-clippy cargo clippy -p operation_service --all-targets --no-deps -- -D warnings`:
    passed.
  - `cargo fmt --check`: passed.
  - `git diff --check`: passed.
  - Static API/debug scans confirmed only
    `OperationServices::exec_command` is public, `CommandOperationService` exec
    is crate-internal, and `WorkspaceLaunchContext` / namespace fds use custom
    debug implementations instead of derived debug.
- Deviations:
  - The live-child rollback behavior is fixed in production by cancelling the
    owned `CommandProcess` before rollback cleanup. The focused service tests
    keep using fake launch drivers because the real command runner requires the
    namespace-runner artifact protocol and is covered below the operation
    service boundary.
- Unresolved issues: None for this remediation slice.
- Handoff notes:
  - Superseded by Milestone 6.5: the stable public exec boundary moves to
    `CommandOperationService::exec_command(input, context)`, while
    `OperationServices::exec_command` is retained only as a temporary forwarding
    shim.

### Post-Milestone 5 Row Projection Review Fixes

- Status: Complete.
- Issues fixed:
  - Retained completed transcript reads no longer silently report missing or
    unreadable transcript files as empty output. Active commands still allow an
    empty pending row window before any transcript has been created.
  - Structured JSONL row candidates with malformed JSON or invalid stream/text/
    offset fields are skipped instead of being synthesized as stdout rows. Raw
    PTY transcript lines that are not structured row candidates still project as
    stdout after timestamp-prefix stripping.
  - The `tool_call` sandbox-protocol note now records that `RunRequest` is a
    serialized daemon-to-namespace-runner wire DTO, that cleanup must preserve
    command/plugin/setup/setns/unknown verb compatibility, and that the rename is
    future work rather than part of Milestone 5.
  - The static-search record now distinguishes the narrow operation-service
    forbidden-term false positive from the broader `tool_call` inventory that is
    intentionally documenting low-level protocol debt.
- Files changed:
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/service.rs`
  - `crates/daemon/operation_service/src/command/transcript.rs`
  - `crates/daemon/operation_service/tests/command_transcript_rows.rs`
  - `docs/daemon/workspace_migration/tool_call_sandbox_protocol_FINDINGS.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-fix-target cargo test -p operation_service command_transcript_rows`:
    passed, 9 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-fix-target cargo test -p operation_service command_ownership`:
    passed, 2 matching unit tests and 4 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-fix-target cargo test -p operation_service command_finalize`:
    passed, 10 matching unit tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-fix-target cargo test -p operation_service`:
    passed, 81 tests total across unit, integration, and doc-test binaries.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-fix-target cargo check -p operation_service`:
    passed.
  - `cargo fmt --check`: passed.
  - `git diff --check`: passed.
  - `rg -n "operation::command|StartCommand|request_id|trace_id|invocation_id|remountable|WorkspaceRuntime|CommandOps|ExecTarget|InternalHostOneShot|advance_active_commands_once|collect_completed|count_commands|count_by_caller" crates/daemon/operation_service/src/command crates/daemon/operation_service/tests`:
    one expected false positive remains in
    `crates/daemon/operation_service/tests/command_exec.rs`, where the test
    asserts the low-level runner JSON field `tool_call.invocation_id`.
  - `rg -n "tool_call|ToolCall|tool call" crates/daemon/linux-namespace-subprocess/src crates/daemon/command/src crates/daemon/workspace/src/namespace crates/daemon/operation/src/command crates/daemon/operation_service/tests docs/daemon/workspace_migration`:
    intentionally returns the documented sandbox protocol producers, consumers,
    tests, and migration notes.
- Deviations:
  - Operation-service row projection tests still use fake launch drivers for the
    command process boundary. This layer now proves the command runner payload
    includes the expected transcript path and that retained reads fail when that
    retained path is missing. A full live ns-runner transcript creation proof
    belongs to daemon/live E2E or the lower-level command substrate.
- Unresolved issues:
  - True stderr fidelity remains unavailable from existing merged PTY transcript
    logs until a future structured sidecar or substrate change writes stream rows
    at capture time.

## Milestone 6: WorkspaceRemountService And Remount-Pending State

- Status: Complete.
- Files changed:
  - `Cargo.lock`
  - `crates/daemon/command/src/process.rs`
  - `crates/daemon/command/src/pty.rs`
  - `crates/daemon/operation_service/Cargo.toml`
  - `crates/daemon/operation_service/src/error.rs`
  - `crates/daemon/operation_service/src/command/mod.rs`
  - `crates/daemon/operation_service/src/command/error.rs`
  - `crates/daemon/operation_service/src/command/exec.rs`
  - `crates/daemon/operation_service/src/command/process_store.rs`
  - `crates/daemon/operation_service/src/command/service.rs`
  - `crates/daemon/operation_service/src/command/remount.rs`
  - `crates/daemon/operation_service/src/workspace_manager/mod.rs`
  - `crates/daemon/operation_service/src/workspace_manager/session_manager.rs`
  - `crates/daemon/operation_service/src/workspace_manager/service.rs`
  - `crates/daemon/operation_service/src/workspace_manager/error.rs`
  - `crates/daemon/operation_service/src/workspace_remount/mod.rs`
  - `crates/daemon/operation_service/src/workspace_remount/error.rs`
  - `crates/daemon/operation_service/src/workspace_remount/service.rs`
  - `crates/daemon/operation_service/tests/command_process_store.rs`
  - `crates/daemon/operation_service/tests/workspace_manager.rs`
  - `crates/daemon/operation_service/tests/command_remount.rs`
  - `crates/daemon/operation_service/tests/workspace_remount.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_IMPLEMENTATION_PLAN.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_6_agent_prompt.md`
- Carried-forward notes:
  - From Milestone 4: command finalization is service-owned but still driven by
    initial yield and `poll`; no public collect/count/advance APIs should be
    added while implementing remount.
  - From Milestone 4: persistent session finalization must not publish, destroy,
    or refresh session snapshot/layer metadata.
  - From Milestone 5: row projection and retained transcript reads remain
    separate from remount; `read_lines` and `poll` must stay allowed while a
    workspace remount is pending.
  - From Milestone 5, superseded by Milestone 6.5: direct command-service exec
    was crate-internal before this bridge milestone. Milestone 6.5 promotes
    `CommandOperationService::exec_command(input, context)` to the public
    command-service boundary and keeps `OperationServices::exec_command` only as
    a temporary shim.
  - Daemon dispatch migration away from `WorkspaceRuntime` remains Milestone 7
    and is intentionally out of scope for this milestone.
- Post-completion cleanup:
  - Removed the unused public `CommandOperationService::inspect_workspace_remount`
    convenience method. Tests now finish the remount quiesce guard directly, so
    the command service keeps only the orchestration primitive needed by
    `WorkspaceRemountService`.
  - Split the new remount integration tests away from the broad shared test
    support and removed remount-only shared fake state, keeping
    `operation_service` all-target clippy clean under `-D warnings`.
  - Post-review fix: serialized persistent exec admission with workspace-remount
    quiesce scans so an exec that passed the pending guard cannot spawn while
    remaining invisible to remount inspection.
  - Post-review fix: command records receive the remount cancellation token
    before process-group inspection can stop a group, so cancellation is
    deferred until resume instead of killing a stopped group.
  - Post-review fix: unknown `/proc` cwd/root/fd/maps/mountinfo reads now block
    live remount instead of being treated as safe.
  - Post-review fix: `WorkspaceRemountService` records finish/block workspace
    state before explicit process-group resume, and cancellation after
    `CriticalSwitch` no longer aborts the resource remount.
  - Post-review fix: `WorkspaceManagerService::apply_remount` blocks the remount
    state on resource or refresh failure, and the old direct manager
    `remount_workspace` bypass was removed.
  - Follow-up cleanup: removed the private
    `begin_workspace_remount_quiesce_with_controller` test seam now that
    command remount tests exercise the service-level injected process-group
    controller.
  - Follow-up cleanup: removed the unused `_workspace_handle` unit-test helper
    and its obsolete workspace metadata imports from command remount tests.
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service workspace_remount`:
    passed, 5 matching integration tests plus the existing matching service-graph
    test.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_remount`:
    passed, 7 matching command-remount unit tests and 5 matching integration
    tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_ownership`:
    passed, 2 matching unit tests and 4 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_exec`:
    passed, 7 matching unit tests and 10 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service`:
    passed, 104 total tests across unit, integration, and doc-test binaries.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service --test workspace_manager`:
    passed, 15 integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p command`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p command`:
    passed, 18 total tests across unit, integration, and doc-test binaries.
  - `cargo clippy -p operation_service --all-targets --no-deps -- -D warnings`:
    passed.
  - `cargo test -p operation_service`:
    passed, 104 total tests across unit, integration, and doc-test binaries.
  - `cargo fmt --check`: passed.
  - `git diff --check`: passed.
  - `rg -n -e 'begin_workspace_remount_quiesce_with_controller' -e '_workspace_handle\(' -e 'inspect_workspace_remount' -e 'begin_live_remount_for_caller' -e 'inspect_live_remount_for_caller' -e '\bremountable\b' -e 'remountable_commands' -e 'session_not_marked_remountable' -e 'WorkspaceRuntime' -e 'CommandOps' -e 'operation::command' -e 'collect_completed' -e 'count_by_caller' -e 'advance_active_commands_once' crates/daemon/operation_service/src crates/daemon/operation_service/tests`:
    no matches.
  - `rg -n "begin_live_remount_for_caller|inspect_live_remount_for_caller|remountable|remountable_commands|session_not_marked_remountable|WorkspaceRuntime|CommandOps|operation::command|collect_completed|count_by_caller|advance_active_commands_once" crates/daemon/operation_service/src crates/daemon/operation_service/tests`:
    no matches.
- Deviations:
  - Added `command::CommandProcess::inactive_with_process_group_for_test` and a
    policy-free inactive PTY helper so operation-service remount tests can prove
    process-group resume/drop/cancel behavior without spawning the namespace
    runner.
  - `compact_or_remount_session` does not add a production compaction policy; it
    applies the current mounted layer paths through the workspace resource
    remount primitive, preserving the Milestone 6 exclusion on parent-prefix
    compaction.
- Unresolved issues:
  - No daemon dispatch migration was done; mapping the new
    `WorkspaceRemountPending` command error into daemon retryable response
    metadata remains Milestone 7.
  - Command remount tests use an injected process-group controller for
    deterministic quiesce/resume/cancel races. The real Linux `/proc` helper
    retains parser coverage, but live signal-level E2E remains outside this
    milestone.
- Handoff notes:
  - Milestone 7 should route command exec/stdin/read/poll/cancel through
    operation-service APIs and preserve the pending-state behavior landed here:
    starts and stdin reject during pending, while read/poll remain allowed and
    wrong callers still receive authorization errors.
  - `WorkspaceManagerService` now owns remount state transitions and resource
    remount application. `WorkspaceRemountService` is the only owner that holds
    both workspace and command services for remount orchestration.
  - `CommandOperationService` now scans `CommandRegistry` by workspace id for
    remount quiesce, treats every active command as quiesce eligible, blocks on
    unknown process group or `/proc` state, and resumes held process groups on
    success, block, error, drop, and deferred cancel paths.

## Milestone 6.5: Exec Command Boundary Migration

- Status: Complete.
- Spec:
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_6_5_exec_command_boundary_SPEC.md`
- Files changed:
  - `crates/daemon/operation_service/src/command/exec.rs`
  - `crates/daemon/operation_service/src/services.rs`
  - `crates/daemon/operation_service/tests/command_exec.rs`
  - `crates/daemon/operation_service/tests/command_remount.rs`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_IMPLEMENTATION_PLAN.md`
  - `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- Verification:
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_exec`:
    passed, 7 matching unit tests and 11 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_ownership`:
    passed, 2 matching unit tests and 4 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service command_remount`:
    passed, 7 matching unit tests and 5 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service workspace_remount`:
    passed, 1 matching service-graph test and 5 matching integration tests.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo test -p operation_service`:
    passed, 105 total tests across unit, integration, and doc-test targets.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo clippy -p operation_service --all-targets --no-deps -- -D warnings`:
    passed.
  - `CARGO_TARGET_DIR=/tmp/eos-phase2-command-service-target cargo check -p operation_service`:
    passed.
  - `cargo fmt --check`: passed.
  - `git diff --check`: passed.
  - `rg -n "pub\\(crate\\) fn exec_command\\(|Option<WorkspaceSessionHandler>" crates/daemon/operation_service/src/command`:
    no matches; `rg` exited 1 as expected for an empty result set.
  - `rg -n "self\\.workspace\\.resolve|WorkspaceManagerService::resolve" crates/daemon/operation_service/src/services.rs`:
    no matches; `rg` exited 1 as expected for an empty result set.
  - Static stale-doc scan for the two phrases named in the Milestone 6.5 spec
    around retained-public-shim wording and external-caller handoff wording:
    expected false positives only in
    `phase_2_milestone_6_5_exec_command_boundary_SPEC.md` and
    `phase_2_milestone_6_5_agent_prompt.md`, where those files quote the static
    check command itself; no stale narrative handoff text remains.
  - `rg -n "request_id|trace_id|invocation_id|remountable|layer_stack_root" crates/daemon/operation_service/src/command`:
    no matches; `rg` exited 1 as expected for an empty result set.
- Deviations:
  - `OperationServices::exec_command` was retained as the planned temporary
    forwarding shim to avoid daemon-dispatch call-site churn before Milestone 7.
  - The private `ExecCommandMode::Session` stores its resolved
    `WorkspaceSessionHandler` behind `Box` to keep the mode enum clippy-clean
    under `-D warnings`; this does not change the public API.
- Unresolved issues: None for Milestone 6.5.
- Handoff notes: Daemon dispatch migration remains Milestone 7 and should call
  `RuntimeServices.operation.command.exec_command(...)` directly. The temporary
  `OperationServices::exec_command` shim should be removed or explicitly
  justified during Milestone 8 cleanup.

## Milestone 6.6: Workspace Profile Symmetry

- Status: Spec added; implementation not started.
- Spec:
  `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_6_6_workspace_profile_symmetry_SPEC.md`
- Files changed:
- Verification:
- Deviations:
- Unresolved issues:
  - Current code must still prove that cgroup lifecycle is common and
    profile-neutral instead of owned by isolated profile setup/teardown.
  - Current code must still remove or quarantine `HostWorkspace` as a permanent
    public target abstraction.
  - Current code must still ensure holder-backed workspace command launch does
    not silently fall back to fresh namespace launch when namespace FDs are
    missing.
  - Current code must still define file-operation routing outside profile
    implementations.
- Handoff notes:
  - Milestone 7 daemon dispatch migration should not start from a target
    architecture where host-compatible and isolated profile behavior differs by
    holder, namespace FD, scratch, cgroup, caller-owned lifetime, capture/publish,
    command lifecycle, remountability, or file-routing policy.

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
