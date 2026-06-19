# Phase 2 Milestone 6.6 Workspace Profile Symmetry Spec

Date: 2026-06-19
Parent plan: `phase_2_command_service_IMPLEMENTATION_PLAN.md`

## Summary

Milestone 6.6 is a boundary-cleanup gate between the Milestone 6.5 command
exec boundary migration and the Milestone 7 daemon dispatch migration. It makes
host-compatible and isolated workspaces use the same workspace lifecycle shape,
with exactly one intended profile-specific difference:

```text
HostCompatibleProfile
  host network access
  no isolated veth, DNS rewrite, or isolated net-ready setup

IsolatedProfile
  private network namespace
  veth, DNS rewrite, and isolated net-ready setup
```

Every other concern is profile-neutral:

- holder process lifecycle;
- namespace FD ownership and projection;
- scratch directory lifecycle;
- cgroup creation, holder join, command join, teardown, and recovery cleanup;
- caller-owned workspace lifetime;
- capture/publish policy;
- command lifecycle;
- remountability;
- file-operation routing policy.

The milestone is complete only when the spec, implementation, and tests make
those common concerns provably common for both profiles.

## Current Evidence To Reconcile

The current code already has some shared profile lifecycle pieces, but it does
not yet prove symmetry:

- `profile::common` owns holder startup, namespace FD opening, overlay mounting,
  FD close, holder kill, and scratch removal.
- `HostCompatibleProfile` currently only supplies `NamespacePlan::host_workspace()`.
- `IsolatedProfile` currently performs isolated-network setup and cgroup
  create/remove.
- `HostWorkspace` remains a host-specific public abstraction with its own
  lifecycle behavior.
- `WorkspaceModeManager::enter` creates only isolated handles.
- command launch still accepts missing namespace FDs by falling back to fresh
  namespace launch.
- daemon file operations still route isolated sessions versus direct LayerStack
  backends instead of using a profile-neutral workspace-session route.

Milestone 6.6 must close these asymmetries or explicitly reject them as
temporary compatibility that is outside the target architecture.

## Goals

- Define the profile target so host-compatible and isolated differ only by
  network setup.
- Keep `WorkspaceProfile` or profile hooks focused on profile-specific
  environment setup.
- Move cgroup behavior out of isolated profile code into common workspace
  lifecycle/resource-control code.
- Make host-compatible workspaces use the same managed handle/context shape as
  isolated workspaces.
- Keep command lifecycle, one-shot versus persistent lifetime, capture/publish,
  remount, and file routing outside profile implementations.
- Make compatibility shims explicit, temporary, and policy-free.
- Add acceptance criteria and static checks that catch reintroduced profile
  policy leakage.

## Non-Goals

- No daemon dispatch migration. That remains Milestone 7.
- No deletion of every old compatibility wrapper unless it is required to prove
  this milestone's symmetry.
- No new wire protocol shape.
- No new publish mode or command lifecycle mode.
- No per-command remount opt-in.
- No fake `IsolatedWorkspace` type added only to mirror `HostWorkspace`.
- No permanent public `HostWorkspace` target abstraction.
- No use of the compatibility `network_mode` module path for new profile code.

## Target Ownership

| Concern | Owner | Profile role | Acceptance proof |
| --- | --- | --- | --- |
| Holder process lifecycle | common workspace lifecycle | provide namespace plan only | one create/teardown sequence drives both profiles |
| Namespace FD ownership and projection | common workspace lifecycle and launch projection | host omits only net FD; isolated includes net FD | command/file launch consumes one projected context shape |
| Scratch directory lifecycle | common workspace lifecycle | none | create, rollback, teardown, and recovery use common code |
| Cgroup lifecycle | common resource-control lifecycle | none | host-compatible and isolated both create, join, remove, and recover cgroups the same way |
| Caller-owned lifetime | `WorkspaceManagerService` / session manager | none | one-shot/session lifetime is selected by command/workspace policy, not profile kind |
| Capture/publish policy | `CommandOperationService` and layerstack publish policy | none | profiles never choose publish, discard, or snapshot refresh |
| Command lifecycle | `CommandOperationService` and command crate substrate | none | start/finalize/cancel paths do not branch on profile except launch context data |
| Remountability | `WorkspaceRemountService` plus workspace remount primitives | none | host-compatible and isolated sessions are either both remountable or both rejected for the same non-profile reason |
| File-operation routing | daemon/operation-service file routing owner | none | route policy does not live in profile kind or profile implementation |
| Network setup | `HostCompatibleProfile` / `IsolatedProfile` | sole profile-owned difference | host keeps host network; isolated creates veth/DNS/net-ready |

## Target Profile Interface

The profile interface should expose only profile-owned setup:

```rust
trait WorkspaceProfile {
    fn kind(&self) -> NetworkMode;
    fn namespace_plan(&self) -> NamespacePlan;
    fn setup_network(
        &mut self,
        runtime: &NamespaceRuntime,
        context: &mut WorkspaceProfileNetworkContext<'_>,
    ) -> Result<(), WorkspaceProfileError>;
    fn teardown_network(
        &mut self,
        runtime: &NamespaceRuntime,
        context: &WorkspaceProfileNetworkContext<'_>,
    );
}
```

The exact names may differ, but the capability boundary must hold:

- profile setup may mutate only profile-owned network fields such as veth and
  DNS configuration;
- profile setup must not allocate scratch dirs, spawn or kill holder processes,
  persist handles, create or remove cgroups, select command lifecycle, decide
  publish/discard behavior, decide remount eligibility, or route file
  operations;
- default no-op hooks are acceptable only for explicitly optional network setup,
  not for required common lifecycle or resource-control steps.

## Common Lifecycle Sequence

Both profiles must use one lifecycle sequence:

1. Validate caller/workspace request and acquire the caller-owned workspace
   lifetime owner.
2. Acquire snapshot/lease and allocate scratch dirs.
3. Build a handle with the requested `NetworkMode`.
4. Spawn the holder with the profile namespace plan.
5. Open and persist namespace FDs.
6. Run profile network setup.
7. Mount the overlay.
8. Run common cgroup setup and attach the holder.
9. Publish one handle/context shape to workspace manager and command launch.
10. On command launch, attach the command process to the handle cgroup when a
    cgroup is available.
11. On teardown, stop commands through command lifecycle owners, remove network
    resources through the profile, kill the holder, close FDs, remove cgroup
    state, and remove scratch dirs through common lifecycle code.
12. On recovery, reap persisted holder, namespace, cgroup, network, and scratch
    resources using the same persisted handle schema for both profiles.

If implementation order requires a different low-level ordering, the milestone
must document why the ordering is equivalent and must keep ownership common.

## Host-Compatible Profile Target

`HostCompatibleProfile` must:

- request the holder namespace stack that preserves host network access;
- produce the same managed workspace handle/context shape as isolated;
- skip isolated veth creation;
- skip DNS rewrite;
- skip isolated net-ready signaling;
- rely on common lifecycle for holder, namespace FDs, scratch, cgroup, remount,
  command launch, capture, publish, and file routing.

It must not be treated as inherently one-shot, inherently non-remountable, or
ineligible for caller-owned session lifetime.

## Isolated Profile Target

`IsolatedProfile` must:

- request the holder namespace stack that includes a private network namespace;
- perform isolated-network setup only: bridge/veth allocation, DNS rewrite, and
  isolated net-ready signaling;
- tear down only isolated-network resources in profile teardown;
- rely on common lifecycle for holder, namespace FDs, scratch, cgroup, remount,
  command launch, capture, publish, and file routing.

It must not be treated as inherently persistent, inherently publish-discard
different, or the only profile eligible for cgroups and remount.

## Compatibility Rules

- `HostWorkspace` may remain only as a temporary compatibility shim while call
  sites migrate. It must be private or explicitly marked temporary, and it must
  not be the target public abstraction for host-compatible workspaces.
- Do not add an `IsolatedWorkspace` shim for naming symmetry.
- Do not add new code that imports the compatibility `network_mode` module path.
- Do not encode one-shot/session lifetime in `NetworkMode` or profile
  implementation.
- Do not encode file-operation routing in `WorkspaceProfile`.
- Do not encode remount eligibility in host-compatible versus isolated target
  matching.

## Expected Files To Change

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
- affected tests under `workspace`, `operation_service`, and `daemon/core`

This list is not permission for broad cleanup. Changes should stay limited to
making the profile boundary symmetric and proving it.

## Migration Steps

1. Add a profile-neutral workspace create path.
   - Accept a profile kind or profile implementation at the workspace resource
     boundary.
   - Build host-compatible and isolated handles through the same manager path.

2. Narrow profile hooks.
   - Replace whole-handle mutable access with a context/effects object that
     exposes only network-owned fields and methods.
   - Keep common lifecycle fields private behind typed methods where practical.

3. Move cgroup behavior to common lifecycle.
   - Create cgroups for every holder-backed workspace when resource control is
     available.
   - Attach holders and command processes through profile-neutral code.
   - Remove cgroups during common teardown.
   - Reap cgroups during common recovery cleanup.

4. Retire host-only lifecycle ownership.
   - Stop exposing `HostWorkspace` as a public target abstraction.
   - Route one-shot host-compatible workspaces through the same managed handle
     shape as persistent host-compatible workspaces.

5. Make command launch profile-neutral.
   - Workspace-session command launch must require holder namespace FDs.
   - Missing namespace FDs are an error for holder-backed workspace commands, not
     a silent `FreshNs` fallback.
   - One-shot versus persistent finalization remains command policy.

6. Make remount and file routing policy profile-neutral.
   - Remount decisions must use workspace/session state, not profile kind.
   - File routing must use the resolved workspace-session context and must not be
     encoded in profile implementations.

7. Add focused tests and static checks.
   - Use both host-compatible and isolated fixtures for the same create, command,
     remount, file-route, teardown, and recovery contract where platform support
     permits.

## Tests And Verification Commands

Required static checks:

```text
rg -n "HostWorkspace|HostNamespaceWorkspaceRequest|WorkspaceModeContext|WorkspaceModeManager|ExecTarget::Host|ExecTarget::IsolatedNetwork|IsolatedNetworkError|network_mode" crates/daemon/workspace/src crates/daemon/operation/src crates/daemon/operation_service/src crates/daemon/core/src
rg -n "one.shot|one_shot|publish|published|remountable|cgroup|ResourcePolicy" crates/daemon/workspace/src/profile crates/daemon/operation/src/command crates/daemon/operation_service/src/command
rg -n "FreshNs|namespace_fds: None|NetworkMode::Host" crates/daemon/command/src crates/daemon/operation_service/src crates/daemon/core/src
git diff --check -- docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_6_6_workspace_profile_symmetry_SPEC.md docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_IMPLEMENTATION_PLAN.md
```

Required compile/test gates after implementation:

```text
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

The static `rg` commands are evidence scans, not automatic pass/fail checks.
Each remaining match must be classified as target code, temporary compatibility,
test fixture, or bug before the milestone is accepted.

## Acceptance Criteria

- [ ] The parent implementation plan lists Milestone 6.6 before Milestone 7.
- [ ] The implementation record has a Milestone 6.6 entry with verification and
  unresolved issues.
- [ ] Host-compatible and isolated workspaces share one create/setup/teardown
  sequence.
- [ ] Both profiles produce one handle/context shape.
- [ ] Cgroup create, holder join, command join, teardown, and recovery cleanup
  are common and profile-neutral.
- [ ] `WorkspaceProfile` or profile hooks cannot mutate common lifecycle policy
  directly.
- [ ] `HostWorkspace` is not a permanent public target abstraction.
- [ ] No fake isolated-only adapter is added for naming symmetry.
- [ ] One-shot versus persistent lifetime is owned outside profiles.
- [ ] Capture/publish policy is owned outside profiles.
- [ ] Command lifecycle is owned outside profiles.
- [ ] Remount eligibility is owned outside profiles.
- [ ] File-operation routing policy is owned outside profiles.
- [ ] The only accepted profile-specific difference is host network access versus
  isolated network namespace, veth, DNS rewrite, and isolated net-ready setup.

## Open Questions

- Should `NetworkMode` remain a closed two-variant enum for Phase 2, or should
  the profile interface be shaped for third-party profile extension later?
- Should file-operation routing move during Milestone 6.6, or should this
  milestone define the invariant and make Milestone 7/M8 perform the daemon
  adapter move? The answer must still be explicit before implementation starts;
  file routing cannot be left as an unowned profile asymmetry.
- Which live E2E environments can reliably prove host-compatible cgroup behavior
  and isolated network setup without requiring privileged host access?
