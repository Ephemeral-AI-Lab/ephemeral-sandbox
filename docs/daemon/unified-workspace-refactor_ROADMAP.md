# Unified Workspace Refactor Implementation Roadmap

Status: Phase 4 central routing done
Date: 2026-06-17
Owner: `crates/daemon`
Source spec: `docs/daemon/unified-workspace-refactor_SPEC.md`

## Progress Tracker

| Phase | Status | Primary Area | Exit Gate |
|---|---|---|---|
| 0. Baseline and guardrails | Done | repo/test setup | Current tests and contract gates recorded |
| 1. Public model scaffold | Done | `crates/daemon/workspace` | New DTOs compile beside legacy exports |
| 2. Workspace root resolution | Done | runtime/root binding | New code accepts `workspace_root`; legacy `layer_stack_root` stays compatibility-only |
| 3. Host lifecycle ownership | Done | `WorkspaceRuntime`, `CommandOps` | Host workspace create/destroy is explicit and lease-safe |
| 4. Central routing | Done | runtime adapters | Command/file route choice lives in `WorkspaceRuntime` |
| 5. Capture changes API | Not started | workspace/runtime | Host and isolated capture are explicit and non-publishing |
| 6. Target folder structure | Not started | `crates/daemon/workspace/src` | Shared lifecycle and isolated network setup are physically separated |
| 7. Holder/setns-only workspace execution | Not started | namespace subprocess, command prep | Workspace commands cannot use `FreshNs` |
| 8. Retire legacy names | Not started | public exports/wire compatibility | Legacy names are removed or compatibility-scoped |
| 9. Final verification | Not started | full daemon surface | Focused unit, contract, clippy, and live E2E gates pass |

Statuses: `Not started`, `In progress`, `Blocked`, `Done`.

## Global Invariants

- [ ] Caller-facing APIs use `workspace_root`, not `layer_stack_root`.
- [ ] Public workspace modes remain `NetworkMode::Host` and `NetworkMode::Isolated`.
- [ ] `network_mode/` contains only thin mode adapters.
- [ ] `isolated_network_setup/` contains only dedicated-network setup/cleanup mechanics.
- [ ] Shared holder lifecycle, recovery, cgroup, namespace entry, and remount logic do not live under `isolated_network_setup/`.
- [ ] `LayerStack` remains the only owner of manifests, publish, OCC, snapshot leases, and capture routing.
- [ ] `CommandOps` remains the owner of process registry, PTY/process wait, stdin/progress, and cancel mechanics.
- [ ] `WorkspaceRuntime` coordinates caller lifecycle, route decisions, mode gates, command cancel ordering, and lease custody without owning low-level overlay mount syscalls.
- [ ] `ns-holder` is the only namespace creator for workspace commands.
- [ ] `ns-runner` enters prepared workspace namespaces with `setns`.
- [ ] `run_command`, `capture_changes`, and `destroy` do not publish workspace changes.
- [ ] Any single new file above roughly 500 LOC is split before merge.

## Phase 0: Baseline And Guardrails

Goal: capture the current working baseline before changing contracts or moving files.

Tasks:

- [x] Record current branch and dirty tree state.
- [x] Run `cargo metadata --format-version 1 --no-deps`.
- [x] Run `cargo machete --with-metadata`.
- [x] Run `cargo test -p workspace`.
- [x] Run `cargo test -p operation file`.
- [x] Run `cargo test -p daemon workspace_runtime`.
- [x] Run `cargo run -p xtask -- check-contract`.
- [x] Run `cargo clippy -p daemon --all-targets --locked -- -D warnings`.
- [x] Record skipped gates and why, especially Linux/Docker-only gates.

Exit criteria:

- [x] Baseline pass/fail state is written into the evidence log.
- [x] Any pre-existing failures are classified before implementation begins.
- [x] No phase-1 code changes depend on unverified stale assumptions.

## Phase 1: Public Model Scaffold

Goal: add the unified public vocabulary beside existing workspace names.

Tasks:

- [x] Add `crates/daemon/workspace/src/model.rs`.
- [x] Add `WorkspaceId`, `CallerId`, `BaseRevision`, `WorkspaceHandle`, and `NetworkMode`.
- [x] Add `CreateWorkspaceRequest`, `RunCommandRequest`, `RunCommandResult`, `CaptureChangesRequest`, `CaptureChangesResult`, `DestroyWorkspaceRequest`, and `DestroyWorkspaceResult`.
- [x] Add `crates/daemon/workspace/src/error.rs` with `WorkspaceError`.
- [x] Add `crates/daemon/workspace/src/service.rs` with the `WorkspaceService` trait shape.
- [x] Keep current `EphemeralWorkspace`, `IsolatedManager`, and `IsolatedWorkspaceBinding` exports during migration.
- [x] Add conversions from current isolated handles to the new `WorkspaceHandle`.
- [x] Add unit coverage for type conversion and public DTO construction/derive behavior; serialization remains deferred because the Phase 1 DTOs are not wire-facing.

Exit criteria:

- [x] New names compile beside old names.
- [x] No caller-facing DTO exposes `layer_stack_root`, `upperdir`, `workdir`, namespace FDs, holder PID, cgroup path, or network device details.
- [x] Existing wire behavior remains unchanged.

## Phase 2: Workspace Root Resolution

Goal: make `workspace_root` the caller-facing input and resolve storage roots internally.

Tasks:

- [x] Add `ResolvedWorkspaceRoot`.
- [x] Implement `WorkspaceRuntime::resolve_workspace_root`.
- [x] Resolve `layer_stack_root` through the LayerStack workspace binding.
- [x] Preserve compatibility parsing for legacy `layer_stack_root`.
- [x] Update new isolation enter and test-remount parsing/resolution to accept `workspace_root`; enter trace details emit `workspace_root`, and legacy enter roots are compatibility-labeled.
- [x] Add coverage for `workspace_root` parsing and legacy compatibility parsing.

Exit criteria:

- [x] Phase 2-facing runtime/root paths can be driven from `workspace_root`; full create/run/capture/destroy lifecycle surfaces remain Phase 3+ work.
- [x] Phase 2 legacy `layer_stack_root` use is isolated to explicitly named compatibility adapters for isolation enter and test compact remount; command, file, checkpoint, and other legacy surfaces remain later-phase migration work.
- [x] LayerStack validation still keeps storage outside `workspace_root`.

Phase 2 is closed for root resolution. The future create/run/capture/destroy
lifecycle APIs remain Phase 3+ work and were intentionally not implemented.

## Phase 3: Host Lifecycle Ownership

Goal: move host workspace lifecycle ownership out of command start and into the workspace runtime.

Tasks:

- [x] Introduce `LeasedBaseRevision` as the internal leased snapshot value.
- [x] Move bounded snapshot acquisition from command start into `WorkspaceRuntime`.
- [x] Move host `EphemeralWorkspace::create` orchestration into explicit workspace create.
- [x] Keep command process spawning in `CommandOps`.
- [x] Ensure command finalization releases `LeasedBaseRevision` exactly once.
- [x] Add failure-path tests for create failure, command cleanup, and release.

Exit criteria:

- [x] Host mode has explicit `create` and `destroy`.
- [x] Host mode no longer creates a fresh one-off workspace as a hidden side effect of command start.
- [x] Lease release is exact-once across create failure, command cleanup, and explicit release.

Phase 3 is closed for host lifecycle ownership. `WorkspaceRuntime` now creates
host command workspaces by resolving the legacy `layer_stack_root`
compatibility binding, acquiring the bounded base lease, and allocating the
host `EphemeralWorkspace` before handing a command context to `CommandOps`.
`CommandOps` still owns process spawning, wait/progress/stdin/cancel mechanics,
transcripts, capture/finalization, and the active-command registry. Route
centralization, capture changes, holder/setns-only execution, target folder
moves, and legacy export retirement remain deferred to later phases.

## Phase 4: Central Routing

Goal: make `WorkspaceRuntime` the route authority for command and file operations.

Tasks:

- [x] Move command route decision logic from op adapters into `WorkspaceRuntime`.
- [x] Move file route decision logic from op adapters into `WorkspaceRuntime`.
- [x] Add route context types for host and isolated network modes.
- [x] Keep op adapters responsible for wire parsing, trace recording, and response shaping.
- [x] Keep operation crates responsible for concrete file/command behavior after route selection.
- [x] Add tests for host routes, isolated routes, missing workspace, and active handle routing.

Exit criteria:

- [x] Op adapters do not choose host vs isolated behavior directly.
- [x] Route metadata remains wire-stable.
- [x] Route decisions do not expose internal storage roots to callers.

Phase 4 is closed for central routing. `WorkspaceRuntime` now returns command
and file route contexts with stable route trace facts. Command adapters parse
wire args, record the returned route metadata, and call `CommandOps` with the
returned execution target while the runtime-owned mode gate remains held during
command start. File adapters parse wire args, record the returned route
metadata, invoke the selected direct or isolated backend, and ask the runtime to
complete isolated file-route liveness touches after successful isolated file
operations. Operation crates still own concrete command/file behavior. No Phase
5 capture API, Phase 6 file moves, Phase 7 holder/setns changes, Phase 8
legacy-name retirement, publish behavior changes, or `ops.json` regeneration
were implemented.

## Phase 5: Capture Changes API

Goal: add explicit non-publishing capture for both network modes.

Tasks:

- [ ] Implement host `capture_changes` over the per-workspace `upperdir`.
- [ ] Implement isolated `capture_changes` over the open handle `upperdir`.
- [ ] Reject or quiesce active commands before walking the upperdir.
- [ ] Return changed paths, changed path kinds, protected drops, and optional stats.
- [ ] Ensure `capture_changes` does not publish.
- [ ] Add tests for active-command rejection or quiescing.
- [ ] Add tests for protected path drops and tree stats.

Exit criteria:

- [ ] Capture works for Host and Isolated.
- [ ] Capture never publishes.
- [ ] Publish remains a separate LayerStack operation chosen by the caller/runtime.

## Phase 6: Target Folder Structure

Goal: move code into the final responsibility-based layout without changing behavior.

Target structure:

```text
crates/daemon/workspace/src/
  lib.rs
  model.rs
  error.rs
  service.rs
  overlay/
    mod.rs
    dirs.rs
    capture.rs
    tree.rs
  lifecycle/
    mod.rs
    create.rs
    destroy.rs
    recovery.rs
    leases.rs
    remount/
      mod.rs
      state.rs
      plan.rs
      apply.rs
  namespace/
    mod.rs
    plan.rs
    holder.rs
    setns_runner.rs
    fds.rs
    cgroup.rs
  network_mode/
    mod.rs
    host.rs
    isolated_network.rs
  isolated_network_setup/
    mod.rs
    types.rs
    caps.rs
    manager.rs
    setup.rs
    teardown.rs
    dns.rs
    rtnl.rs
    netfilter/mod.rs
    netfilter/exprs.rs
    netfilter/wire.rs
```

Tasks:

- [ ] Move `capture.rs`, `dirs.rs`, and `tree.rs` into `overlay/`.
- [ ] Move shared create/destroy/recovery/lease logic into `lifecycle/`.
- [ ] Move remount state/plan/apply logic into `lifecycle/remount/`.
- [ ] Move namespace planning, holder entry, setns runner prep, FD mapping, and cgroup handling into `namespace/`.
- [ ] Move mode adapter logic into `network_mode/host.rs` and `network_mode/isolated_network.rs`.
- [ ] Move only veth, DNS, rtnetlink, netfilter, and dedicated-network setup/cleanup into `isolated_network_setup/`.
- [ ] Keep module re-exports narrow and caller-facing names stable.
- [ ] Split any new file above roughly 500 LOC.

Exit criteria:

- [ ] `isolated_network_setup/` does not contain shared lifecycle, namespace, recovery, cgroup, or remount code.
- [ ] `network_mode/` is a thin adapter layer.
- [ ] Workspace crate public exports are intentional and minimal.
- [ ] File moves are behavior-preserving before Phase 7 semantic changes.

## Phase 7: Holder/Setns-Only Workspace Execution

Goal: remove workspace dependence on fresh namespace initialization.

Tasks:

- [ ] Make `create(NetworkMode::Host)` launch `ns-holder` with `NamespaceNetwork::Host`.
- [ ] Make `create(NetworkMode::Isolated)` launch `ns-holder` with `NamespaceNetwork::Isolated`.
- [ ] Make workspace `run_command` always call the setns runner path.
- [ ] Make missing holder namespace FDs a workspace execution error.
- [ ] Keep `FreshNs` only behind a separate legacy/non-workspace compatibility path.
- [ ] Add tests proving workspace command requests cannot select `FreshNs`.
- [ ] Add tests proving missing holder FDs fail instead of falling back to `FreshNs`.

Exit criteria:

- [ ] `ns-holder` is the single namespace creator for workspace commands.
- [ ] `ns-runner` only enters prepared workspace namespaces with `setns`.
- [ ] `FreshNs` cannot be selected by any new workspace command path.

## Phase 8: Retire Legacy Names

Goal: remove old workspace vocabulary after compatibility shims are no longer needed.

Tasks:

- [ ] Stop exporting `EphemeralWorkspace` from the workspace crate root.
- [ ] Stop exporting `IsolatedManager` from the workspace crate root unless an internal crate still requires it.
- [ ] Stop exporting `IsolatedWorkspaceBinding` from the workspace crate root unless wire compatibility requires it.
- [ ] Replace legacy names in daemon runtime and operation adapters.
- [ ] Keep compatibility aliases only where wire contract requires them.
- [ ] Update docs and generated readme pages if workspace docs change.

Exit criteria:

- [ ] Public workspace vocabulary uses unified names.
- [ ] Compatibility names are scoped, documented, and scheduled for deletion.
- [ ] Existing wire behavior remains compatible during migration.

## Phase 9: Final Verification

Goal: prove the implementation is correct locally and identify any Linux/Docker-only proof gaps.

Focused local gates:

- [ ] `cargo test -p workspace`
- [ ] `cargo test -p operation file`
- [ ] `cargo test -p daemon workspace_runtime`
- [ ] `cargo test -p daemon --test workspace_read_paths --test workspace_write_paths --test workspace_command_paths`
- [ ] `cargo run -p xtask -- check-contract`
- [ ] `cargo clippy -p daemon --all-targets --locked -- -D warnings`
- [ ] `cargo machete --with-metadata`

Live E2E preparation:

- [ ] `cargo run -p xtask -- package`
- [ ] `cargo test -p e2e-test --features e2e --no-run`

Focused live E2E suites:

- [ ] `core`
- [ ] `workspace-runtime-command`
- [ ] `workspace-runtime-isolated`
- [ ] host/legacy ephemeral workspace suite
- [ ] pressure cross-mode suite

Exit criteria:

- [ ] All focused local gates pass or have documented pre-existing failures.
- [ ] Packaged daemon was rebuilt before live E2E.
- [ ] Live E2E report root is recorded when Linux/Docker E2E is run.
- [ ] Any skipped Linux/Docker proof is stated explicitly in the final implementation report.

## Acceptance Criteria

Architecture and ownership:

- [ ] `LayerStack` remains the only owner of publish/OCC.
- [ ] `CommandOps` remains the owner of command process lifecycle.
- [ ] `workspace` owns overlay dirs, upperdir capture primitives, holder-backed lifecycle/remount primitives, and isolated-network setup mechanics.
- [ ] `overlay` remains a low-level overlayfs mount/unmount mechanism crate.
- [ ] `linux-namespace-subprocess` owns holder namespace creation and setns command execution, not caller-facing workspace semantics.

API and DTOs:

- [ ] Caller-facing APIs use `workspace_root`, not `layer_stack_root`.
- [ ] `BaseRevision` and `LeasedBaseRevision` replace public/internal snapshot naming.
- [ ] Public DTOs do not expose storage paths, namespace FDs, holder PID, cgroup path, veth names, or netfilter details.
- [ ] `WorkspaceHandle` remains the public token; richer state remains internal.

Lifecycle and routing:

- [ ] Caller lifecycle is explicit: `create`, `run_command`, `capture_changes`, `destroy`.
- [ ] Host and Isolated use the same holder-backed workspace lifecycle.
- [ ] Host skips only dedicated network namespace and isolated-network setup.
- [ ] Isolated adds dedicated network namespace, veth, DNS, and netfilter setup.
- [ ] Command and file route decisions are centralized in `WorkspaceRuntime`.
- [ ] Adapters parse wire args and record trace events but do not choose host vs isolated behavior directly.

Namespace and runner:

- [ ] `ns-holder` creates and pins workspace namespace stacks for both modes.
- [ ] `ns-runner` only enters prepared workspace namespaces with `setns`.
- [ ] Workspace command requests do not carry a public runner mode enum.
- [ ] `FreshNs` exists only for legacy/non-workspace compatibility paths.
- [ ] Missing namespace FDs on a workspace command path are errors, not fallback triggers.

Publish and capture:

- [ ] `run_command` does not publish.
- [ ] `capture_changes` does not publish.
- [ ] `destroy` does not publish.
- [ ] Publish remains a separate LayerStack operation.
- [ ] Capture rejects or quiesces active commands before walking `upperdir`.

Directory structure:

- [ ] Overlay internals live under `overlay/`.
- [ ] Shared holder lifecycle, recovery, leases, and remount live under `lifecycle/`.
- [ ] Namespace planning, holder/setns entry, FD mapping, and cgroup code live under `namespace/`.
- [ ] Host and isolated mode adapters live under `network_mode/`.
- [ ] Dedicated-network setup/cleanup internals live under `isolated_network_setup/`.
- [ ] No single new file above roughly 500 LOC is merged.

## Evidence Log

Append one row per meaningful gate or phase closeout.

| Date | Phase | Command or Review | Result | Notes |
|---|---|---|---|---|
| 2026-06-17 | Roadmap | Created implementation roadmap | Done | No code or tests run |
| 2026-06-17 | 0 | Git baseline commands | Done | Branch `main`; HEAD `d4c2bb9d88174190dce864d7a67fe3ceb19277d8`; dirty tree before Phase 0 edits: `M docs/daemon/unified-workspace-refactor_SPEC.md`, `?? docs/daemon/unified-workspace-refactor_ROADMAP.md`; `git diff --stat`: `docs/daemon/unified-workspace-refactor_SPEC.md | 78 +++++++++++++++++---------`, `1 file changed, 52 insertions(+), 26 deletions(-)`. |
| 2026-06-17 | 0 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase0-target cargo metadata --format-version 1 --no-deps` | Pass | Exit 0; metadata emitted for workspace; no failure output. |
| 2026-06-17 | 0 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase0-target cargo machete --with-metadata` | Pass | Exit 0; `cargo-machete didn't find any unused dependencies in this directory.` |
| 2026-06-17 | 0 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase0-target cargo test -p workspace` | Pass | Exit 0; workspace unit tests passed: 18 passed, 0 failed; doc tests 0 passed, 0 failed. |
| 2026-06-17 | 0 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase0-target cargo test -p operation file` | Pass | Exit 0; filtered operation tests passed: 14 passed, 0 failed, 67 filtered out; checkpoint and contract test targets had 0 matching tests. |
| 2026-06-17 | 0 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase0-target cargo test -p daemon workspace_runtime` | Pass | Exit 0; command compiled daemon test targets but the `workspace_runtime` filter matched 0 tests across the listed targets. |
| 2026-06-17 | 0 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase0-target cargo run -p xtask -- check-contract` | Fail | Exit 1; key error: `crates/daemon/operation/ops.json is stale: regenerate with cargo run -p eosd -- dump-ops > crates/daemon/operation/ops.json`. Classified as pre-existing baseline failure because Phase 0 made no Rust/source changes before this gate. |
| 2026-06-17 | 0 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase0-target cargo clippy -p daemon --all-targets --locked -- -D warnings` | Fail | Exit 101; key error: `crates/daemon/layerstack/src/lease_aware.rs:97:5` has `clippy::double_must_use` because `reclaiming_intervals` has `#[must_use]` while returning an iterator already marked `#[must_use]`. Classified as pre-existing baseline failure because Phase 0 made no Rust/source changes before this gate. |
| 2026-06-17 | 0 | Phase 0 skipped gates review | Skipped | Live Docker/Linux E2E was not run in Phase 0 by scope rule. `cargo run -p xtask -- package` was not run because Phase 0 explicitly forbids packaging unless asked later. |
| 2026-06-17 | 0 | Phase 0 closeout | Done | Required baseline evidence recorded. Phase 1+ implementation was not started. |
| 2026-06-17 | 1 | `cargo fmt` | Pass | Formatted the Phase 1 scaffold files. |
| 2026-06-17 | 1 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase1-target cargo test -p workspace` | Fail | Exit 101; initial scaffold compile failed because `thiserror` treated `SnapshotAcquire { source: String }` as an error source. Fixed by keeping the spec field shape and implementing `Display`/`Error` manually. |
| 2026-06-17 | 1 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase1-target cargo test -p workspace` | Fail | Exit 101; an over-broad DTO leak assertion matched the spec-approved `evicted_upperdir_bytes` metric. Fixed by checking internal field-name debug labels such as `upperdir:` instead of raw substrings. |
| 2026-06-17 | 1 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase1-target cargo test -p workspace` | Pass | Exit 0; final workspace tests passed: 23 passed, 0 failed; doc tests 0 passed, 0 failed. |
| 2026-06-17 | 1 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase1-target cargo test -p operation file` | Pass | Exit 0; filtered operation tests passed: 14 passed, 0 failed, 67 filtered out; checkpoint and contract test targets had 0 matching tests. |
| 2026-06-17 | 1 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase1-target cargo test -p daemon workspace_runtime` | Pass | Exit 0; daemon test targets compiled; the `workspace_runtime` filter matched 0 tests across listed targets. |
| 2026-06-17 | 1 | Phase 1 skipped gates review | Skipped | Live Docker/Linux E2E, `cargo run -p xtask -- package`, and `cargo run -p xtask -- check-contract` were not run by Phase 1 scope. `cargo clippy -p daemon --all-targets --locked -- -D warnings` was not run because the Phase 0 baseline records the unrelated `clippy::double_must_use` failure in `crates/daemon/layerstack/src/lease_aware.rs:97`. |
| 2026-06-17 | 1 | Phase 1 closeout | Done | Added public scaffold modules and tests. The root `workspace::WorkspaceHandle` remains the legacy isolated handle for compatibility; the unified scaffold handle is public as `workspace::model::WorkspaceHandle` and `workspace::UnifiedWorkspaceHandle`. |
| 2026-06-17 | 2 | Phase 2 implementation review | Done | Added `ResolvedWorkspaceRoot`, `WorkspaceRuntime::resolve_workspace_root`, and `resolve_legacy_layer_stack_root`. New isolated enter/test-remount adapter paths parse `workspace_root`; legacy `layer_stack_root` parses through `WorkspaceRootInput::LegacyLayerStackRoot` and calls explicitly named compatibility methods. Command, file, checkpoint, and other legacy `layer_stack_root` surfaces remain later-phase work. No host lifecycle, routing centralization, capture, holder/setns, destroy, publish, or wire-regeneration work was implemented. |
| 2026-06-17 | 2 | `cargo fmt` | Pass | Formatted Phase 2 runtime, adapter, contract, and tests. |
| 2026-06-17 | 2 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase2-target cargo test -p workspace` | Pass | Exit 0; workspace unit tests passed: 24 passed, 0 failed; doc tests 0 passed, 0 failed. |
| 2026-06-17 | 2 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase2-target cargo test -p operation file` | Pass | Exit 0; filtered operation tests passed: 14 passed, 0 failed, 70 filtered out; checkpoint and contract test targets had 0 matching tests. |
| 2026-06-17 | 2 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase2-target cargo test -p daemon workspace_runtime` | Pass | Exit 0; 8 focused runtime tests passed, covering workspace-root resolution, legacy compatibility resolution, ambiguous binding rejection, binding-backed enter, lease/cancel compatibility, and snapshot normalization. |
| 2026-06-17 | 2 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase2-target cargo test -p operation isolation` | Pass | Exit 0; 5 focused isolation contract tests passed, covering `workspace_root`, legacy `layer_stack_root`, ambiguous root rejection, and test-remount force-reason parsing. |
| 2026-06-17 | 2 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase2-target cargo test -p daemon enter_trace_events_include_holder_and_dns_configuration` | Pass | Exit 0; adapter trace test passed and verifies the new enter-start trace emits `workspace_root` instead of `layer_stack_root`. |
| 2026-06-17 | 2 | `git diff --check` | Pass | Exit 0; no whitespace errors. |
| 2026-06-17 | 2 | Phase 2 skipped gates review | Skipped | Live Docker/Linux E2E, `cargo run -p xtask -- package`, and ops.json regeneration were not run by Phase 2 scope. `cargo run -p xtask -- check-contract` was not run because the Phase 0 baseline records stale `crates/daemon/operation/ops.json`; `cargo clippy -p daemon --all-targets --locked -- -D warnings` was not run because the Phase 0 baseline records the unrelated `clippy::double_must_use` failure in `crates/daemon/layerstack/src/lease_aware.rs:97`. |
| 2026-06-17 | 2 | Phase 2 closeout | Done | Workspace-root resolution compiles beside legacy compatibility. Residual work remains intentionally deferred to Phase 3+: host create/destroy lifecycle, route centralization, capture, holder/setns-only execution changes, target folder moves, and legacy export retirement. |
| 2026-06-17 | 2 | Adversarial review follow-up | Done | Tightened binding discovery to reject copied bindings whose file path does not match the declared `layer_stack_root`; made cached runtime state track `workspace_root` as part of binding identity; kept ambiguity checks active when a state binding already exists; hardened malformed dual-root parsing; preserved `invalid_argument` shaping for test-remount root request errors; changed the dispatch lifecycle test to enter through `workspace_root`. Focused gates passed with `CARGO_TARGET_DIR=/tmp/eos-adversarial-phase2-target`: `cargo test -p operation isolation`, `cargo test -p daemon workspace_runtime`, `cargo test -p workspace`, `cargo test -p operation file`, and `cargo test -p daemon --test phase2_read_paths isolated_workspace_lifecycle_ops_open_status_list_and_exit_when_enabled`. `git diff --check` passed. |
| 2026-06-17 | 3 | Phase 3 implementation review | Done | Added `LeasedBaseRevision` and explicit host `HostWorkspaceLifecycle` creation plus release custody in `WorkspaceRuntime`; moved host bounded snapshot acquisition and `EphemeralWorkspace::create` out of `CommandOps::start_ephemeral`; added an exact-once LayerStack `LeaseReleaseHandle` used by command prepare failure, finalization, and force-discard cleanup. `CommandOps` still owns process spawn, wait/progress/stdin/cancel, registry, transcripts, capture, and finalization. No Phase 4+ routing centralization, capture API, holder/setns change, target folder move, publish behavior change, legacy export retirement, or `ops.json` regeneration was implemented. |
| 2026-06-17 | 3 | `cargo fmt` | Pass | Formatted Phase 3 Rust sources. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase3-target cargo test -p daemon workspace_runtime` | Pass | Exit 0; 15 focused runtime tests passed, including host create acquiring a leased base revision, create failure releasing the lease, explicit release exactly once, and existing Phase 2 root-resolution coverage. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase3-target cargo test -p workspace` | Pass | Exit 0; 24 workspace unit tests passed; doc tests 0 passed, 0 failed. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase3-target cargo test -p operation command` | Pass | Exit 0; operation command filter passed, including 62 command unit tests plus filtered checkpoint/contract tests; new coverage proves the host lease release handle is exact-once. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase3-target cargo test -p operation file` | Pass | Exit 0; 14 operation file compatibility tests passed, with checkpoint and contract targets compiling and filtering cleanly. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase3-target cargo test -p operation finalize` | Pass | Exit 0; optional finalization filter passed: 34 operation tests plus the filtered contract fixture. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase3-target cargo test -p daemon command` | Pass | Exit 0; optional daemon command filter passed: 15 command/dispatch tests. |
| 2026-06-17 | 3 | `git diff --check` | Pass | Exit 0; no whitespace errors. |
| 2026-06-17 | 3 | Phase 3 skipped gates review | Skipped | Live Docker/Linux E2E, `cargo run -p xtask -- package`, ops.json regeneration, `cargo run -p xtask -- check-contract`, and `cargo clippy -p daemon --all-targets --locked -- -D warnings` were not run. Packaging, live E2E, and ops regeneration are explicitly outside this slice. `xtask check-contract` and daemon clippy remain covered by the Phase 0 baseline blockers: stale `crates/daemon/operation/ops.json` and pre-existing `clippy::double_must_use` in `crates/daemon/layerstack/src/lease_aware.rs:97`. |
| 2026-06-17 | 3 | Phase 3 closeout | Done | Host lifecycle ownership is explicit in `WorkspaceRuntime`, command process spawning remains in `CommandOps`, and focused tests prove lease release across create failure, destroy, and command cleanup/finalization custody. Residual work remains intentionally deferred to Phase 4+: route centralization, capture changes, holder/setns-only workspace execution, target folder structure, and legacy export retirement. |
| 2026-06-17 | 3 | Phase 3 adversarial review follow-up | Done | Fixed Host lease leakage when `CommandOps` rejects before consuming an `ExecTarget::Ephemeral` by making the LayerStack `LeaseReleaseHandle` release on last-owner drop. Added regression coverage for dropped unconsumed host workspaces, empty-command validation rejection, active-command admission rejection, and command finalization release. |
| 2026-06-17 | 3 | Phase 3 cleanup follow-up | Done | Moved the exact-once Host lease release guard out of command service and into `layerstack::service::LeaseReleaseHandle`, removed the command facade export for lease lifecycle types, and removed the dead `WorkspaceRuntime::destroy_host_workspace` shim. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-host-lease-fix-target cargo test -p operation command` | Pass | Exit 0; 66 command tests plus filtered checkpoint/contract targets passed, including Host lease last-owner drop, pre-start validation/admission rejection, and finalization release coverage. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-host-lease-fix-target cargo test -p daemon workspace_runtime` | Pass | Exit 0; 15 focused runtime tests passed after the Host lease RAII fix. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-host-lease-cleanup-target cargo test -p operation command` | Pass | Exit 0; 66 command tests plus filtered checkpoint/contract targets passed after moving Host lease custody into LayerStack service. |
| 2026-06-17 | 3 | `CARGO_TARGET_DIR=/tmp/eos-host-lease-cleanup-target cargo test -p daemon workspace_runtime` | Pass | Exit 0; 15 focused runtime tests passed after removing the dead runtime destroy shim. |
| 2026-06-17 | 4 | Phase 4 implementation review | Done | Added runtime-owned command/file route context APIs and stable route facts. Command adapters now call `WorkspaceRuntime::route_command_context` and keep the returned mode-gate context alive while invoking `CommandOps`. File adapters now call `WorkspaceRuntime::route_file_context` or the runtime direct legacy helper, then invoke the selected operation backend. No capture, holder/setns, file-move, publish, legacy-retirement, ops.json, or E2E work was implemented. |
| 2026-06-17 | 4 | `cargo fmt` | Pass | Formatted Phase 4 runtime, adapter, test, and docs edits. |
| 2026-06-17 | 4 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase4-target cargo test -p daemon workspace_runtime` | Pass | Exit 0; 21 focused runtime tests passed, including command Host/isolated routing, file direct/isolated routing, missing root compatibility, route facts, isolated file liveness touch, Phase 2 root-resolution coverage, and Phase 3 Host lease exact-once coverage. |
| 2026-06-17 | 4 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase4-target cargo test -p operation command` | Pass | Exit 0; 66 command unit tests plus focused checkpoint/contract command targets passed. Command process lifecycle and Host lease release behavior remain in `CommandOps`/LayerStack after route centralization. |
| 2026-06-17 | 4 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase4-target cargo test -p operation file` | Pass | Exit 0; 14 operation file tests passed with checkpoint/contract targets compiling and filtering cleanly. Direct and isolated file backends still own concrete read/write/edit behavior. |
| 2026-06-17 | 4 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase4-target cargo test -p workspace` | Pass | Exit 0; 24 workspace unit tests passed; doc tests 0 passed, 0 failed. |
| 2026-06-17 | 4 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase4-target cargo test -p daemon --test phase2_read_paths` | Fail then Pass | Initial full run failed because disabled isolation enter returned `invalid_argument` before `feature_disabled` when given a missing legacy root. Fixed disabled-mode precedence before root resolution; rerun passed 15 tests, including file fast-path route trace metadata. |
| 2026-06-17 | 4 | `CARGO_TARGET_DIR=/tmp/eos-unified-workspace-phase4-target cargo test -p daemon --test phase3_write_paths` | Pass | Exit 0; 6 write-path tests passed, covering direct file write/edit behavior and OCC/drop compatibility. |
| 2026-06-17 | 4 | `git diff --check` | Pass | Exit 0; no whitespace errors. |
| 2026-06-17 | 4 | Phase 4 skipped gates review | Skipped | Live Docker/Linux E2E, `cargo run -p xtask -- package`, ops.json regeneration, optional `workspace_read_paths`/`workspace_write_paths`/`workspace_command_paths`, `cargo run -p xtask -- check-contract`, and daemon clippy were not run. Packaging, live E2E, ops regeneration, and Phase 5+ target tests are outside this slice. `xtask check-contract` and daemon clippy remain covered by the Phase 0 baseline blockers: stale `crates/daemon/operation/ops.json` and pre-existing `clippy::double_must_use` in `crates/daemon/layerstack/src/lease_aware.rs:97`. |
| 2026-06-17 | 4 | Phase 4 closeout | Done | Command/file route choice lives in `WorkspaceRuntime`; adapters only parse wire args, record returned traces, invoke selected operation backends, map errors, and shape responses. Route metadata remains compatible, caller-facing storage details remain unexposed, and Phase 5+ work is intentionally deferred. |
