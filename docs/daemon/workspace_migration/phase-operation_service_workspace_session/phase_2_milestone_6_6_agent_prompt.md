# Phase 2 Milestone 6.6 Agent Prompt

You are implementing Phase 2 Milestone 6.6 only in:

`/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os`

Milestone 6.6 is: make host-compatible and isolated workspace profiles
symmetric for every concern except network setup.

The only allowed profile-specific difference is:

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

## First Rule

Inspect the live repo before editing. The worktree may already contain unrelated
changes. Do not revert, overwrite, or cleanup changes outside this milestone.

This milestone is allowed to touch code, focused tests, and the implementation
record. It is not a review-only task. Still, keep edits tightly scoped to the
Phase 6.6 profile-symmetry target.

## Read First

Before code changes, read these files:

- `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_milestone_6_6_workspace_profile_symmetry_SPEC.md`
- `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_IMPLEMENTATION_PLAN.md`
- `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_command_service_SPEC.md`
- `docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
- `crates/daemon/workspace/src/model.rs`
- `crates/daemon/workspace/src/profile/mod.rs`
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
- `crates/daemon/workspace/src/lifecycle/remount/apply.rs`
- `crates/daemon/operation_service/src/workspace_manager/service.rs`
- `crates/daemon/operation_service/src/workspace_manager/session_manager.rs`
- `crates/daemon/operation_service/src/command/exec.rs`
- `crates/daemon/operation_service/src/command/remount.rs`
- `crates/daemon/operation_service/src/command/finalize.rs`
- `crates/daemon/operation_service/src/workspace_remount/service.rs`
- `crates/daemon/command/src/launch.rs`
- `crates/daemon/core/src/op_adapter/files.rs`

Also run and record the starting state:

```text
git status --short --untracked-files=all
git diff --stat
git diff --check
```

## Scope

Implement only:

- A profile-neutral workspace lifecycle for host-compatible and isolated
  workspaces.
- Common cgroup create, holder join, command join, teardown, and recovery
  cleanup.
- A narrower profile hook or profile context that only owns network setup.
- Removal or quarantine of `HostWorkspace` as a permanent public target
  abstraction.
- Holder-backed command launch behavior that treats missing namespace FDs as an
  error instead of silently falling back to fresh namespace launch.
- Profile-neutral remount eligibility and file-operation routing ownership.
- Focused tests proving host-compatible and isolated use the same lifecycle
  contract where platform support permits.
- Implementation-record updates for Milestone 6.6.

## Do Not Implement

- No daemon dispatch migration away from `WorkspaceRuntime`; that is Milestone 7.
- No protocol catalog rename.
- No wire schema change.
- No new publish mode or command lifecycle mode.
- No per-command remount opt-in.
- No fake `IsolatedWorkspace` adapter added only to mirror `HostWorkspace`.
- No permanent public `HostWorkspace` target abstraction.
- No new code that uses the compatibility `network_mode` module path.
- No encoding of one-shot/session lifetime in `NetworkMode` or profile
  implementations.
- No encoding of capture/publish, command lifecycle, remount eligibility, or
  file-operation routing in `WorkspaceProfile`.
- No broad cleanup of old `operation::command` or daemon dispatch code beyond
  what is required to prove the profile-symmetry boundary.

## Target Ownership

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

## Implementation Sequence

### 0. Open The Record

Before code changes, update
`docs/daemon/workspace_migration/phase-operation_service_workspace_session/phase_2_implementation_record.md`
under `Milestone 6.6: Workspace Profile Symmetry`:

- set status to in progress;
- list intended files;
- carry forward the unresolved issues already listed there;
- note that daemon dispatch migration remains Milestone 7.

### 1. Map Current Asymmetries

Before editing code, classify current matches from:

```text
rg -n "HostWorkspace|HostNamespaceWorkspaceRequest|WorkspaceModeContext|WorkspaceModeManager|ExecTarget::Host|ExecTarget::IsolatedNetwork|IsolatedNetworkError|network_mode" crates/daemon/workspace/src crates/daemon/operation/src crates/daemon/operation_service/src crates/daemon/core/src
rg -n "one.shot|one_shot|publish|published|remountable|cgroup|ResourcePolicy" crates/daemon/workspace/src/profile crates/daemon/operation/src/command crates/daemon/operation_service/src/command
rg -n "FreshNs|namespace_fds: None|NetworkMode::Host" crates/daemon/command/src crates/daemon/operation_service/src crates/daemon/core/src
```

Use the classification to decide the smallest implementation path. Do not treat
every match as a bug. Some matches may be tests, compatibility code, or old
evidence outside the target path.

### 2. Make Workspace Creation Profile-Neutral

In the workspace crate:

- adapt the managed lifecycle so both host-compatible and isolated workspaces
  can be created through one handle/context path;
- keep `NetworkMode` as the profile selector, not a lifetime or publish-policy
  selector;
- ensure both profiles produce the same launch handle/context shape;
- keep snapshot/lease ownership and caller-owned lifetime outside the profile.

Rules:

- `NetworkMode::Host` must not mean one-shot.
- `NetworkMode::Isolated` must not mean persistent.
- host-compatible and isolated must share holder, namespace FD, scratch, cgroup,
  teardown, and recovery behavior.

### 3. Narrow Profile Hooks

Replace or constrain current profile hooks so profile implementations can only
affect profile-owned network setup.

Acceptable target shape:

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

The exact names can differ, but the capability boundary must hold:

- profiles may set only network-owned outputs such as veth and DNS state;
- profiles must not allocate scratch dirs, spawn or kill holders, persist
  handles, create or remove cgroups, choose command lifecycle, choose
  publish/discard behavior, decide remount eligibility, or route file
  operations;
- default no-op hooks must not hide required common setup.

### 4. Move Cgroup Behavior To Common Lifecycle

Move cgroup behavior out of `IsolatedProfile`.

Implement common lifecycle ownership for:

- cgroup creation;
- holder join;
- command process join;
- teardown/remove;
- recovery cleanup;
- timing/reporting for cgroup phases.

Preserve current ordering unless a change is required. If ordering changes,
document the reason in the implementation record.

### 5. Retire Host-Only Lifecycle Ownership

`HostWorkspace` must not remain the target public abstraction for
host-compatible workspace lifecycle.

Acceptable outcomes:

- remove the public re-export and route callers through the common handle path;
- keep a private compatibility adapter that returns the common handle/context
  shape;
- mark any remaining compatibility path temporary with removal criteria.

Do not add a fake isolated-only adapter just for symmetry.

### 6. Make Command Launch Profile-Neutral

Holder-backed workspace command launch must require the launch material needed
for `SetNs`.

Rules:

- missing namespace FDs for a holder-backed workspace command are an error;
- no silent `FreshNs` fallback for workspace-session command launch;
- one-shot versus persistent finalization remains `CommandOperationService`
  policy;
- command cgroup join uses common launch/resource-control data, not profile kind.

### 7. Keep Remount And File Routing Outside Profiles

Confirm or update:

- remount eligibility and quiesce decisions are based on workspace/session state,
  not host-compatible versus isolated profile kind;
- file-operation routing is owned by daemon/operation-service routing code, not
  by `WorkspaceProfile`;
- if full file-route migration is too large, record an explicit Milestone 7/M8
  blocker in the implementation record. Do not leave file routing as an implicit
  profile asymmetry.

### 8. Update Tests

Add focused tests proving the same contract for both profiles where feasible:

- create produces the same handle/context shape;
- holder and namespace FD projection are common;
- cgroup create/remove/recovery is common;
- command launch rejects missing required namespace FDs;
- one-shot and persistent lifetime are not inferred from profile kind;
- remount checks do not branch on profile kind;
- file routing policy is outside profile implementations.

Prefer narrow unit/integration tests over broad E2E rewrites. If a test cannot
run without privileged host support, add a lower-level deterministic unit test
and record the live-E2E gap.

### 9. Update Docs And Record

Update:

- `phase_2_milestone_6_6_workspace_profile_symmetry_SPEC.md` if implementation
  discovers a necessary spec clarification;
- `phase_2_command_service_IMPLEMENTATION_PLAN.md` only for real plan drift;
- `phase_2_implementation_record.md` with files changed, verification, design
  deviations, unresolved issues, and Milestone 7 handoff notes.

## Verification

Run and record:

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

Run static evidence scans and document remaining matches:

```text
rg -n "HostWorkspace|HostNamespaceWorkspaceRequest|WorkspaceModeContext|WorkspaceModeManager|ExecTarget::Host|ExecTarget::IsolatedNetwork|IsolatedNetworkError|network_mode" crates/daemon/workspace/src crates/daemon/operation/src crates/daemon/operation_service/src crates/daemon/core/src
rg -n "one.shot|one_shot|publish|published|remountable|cgroup|ResourcePolicy" crates/daemon/workspace/src/profile crates/daemon/operation/src/command crates/daemon/operation_service/src/command
rg -n "FreshNs|namespace_fds: None|NetworkMode::Host" crates/daemon/command/src crates/daemon/operation_service/src crates/daemon/core/src
```

Every remaining match must be classified as target code, temporary
compatibility, test fixture, or bug.

## Acceptance Checklist

- [ ] Host-compatible and isolated workspaces share one create/setup/teardown
  sequence.
- [ ] Both profiles produce one handle/context shape.
- [ ] Cgroup create, holder join, command join, teardown, and recovery cleanup
  are common and profile-neutral.
- [ ] Profile hooks cannot mutate common lifecycle policy directly.
- [ ] `HostWorkspace` is not a permanent public target abstraction.
- [ ] One-shot versus persistent lifetime is owned outside profiles.
- [ ] Capture/publish policy is owned outside profiles.
- [ ] Command lifecycle is owned outside profiles.
- [ ] Remount eligibility is owned outside profiles.
- [ ] File-operation routing policy is owned outside profiles.
- [ ] The only accepted profile-specific difference is host network access versus
  isolated network namespace, veth, DNS rewrite, and isolated net-ready setup.
- [ ] Implementation record is updated with exact verification results and any
  remaining compatibility shims.

## Final Response

Report:

- changed files;
- the profile-symmetry path implemented;
- any intentional compatibility shims left in place and their removal criteria;
- verification commands and pass/fail status;
- remaining risks or live-E2E gaps.
