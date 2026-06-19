# Layerstack Capture Responsibility Migration Spec

Date: 2026-06-20
Status: Draft
Scope: `crates/daemon/layerstack`, `crates/daemon/workspace`,
`crates/daemon/operation_service`

## Summary

This migration removes upperdir capture responsibility from `layerstack`.

`layerstack` should own durable layer storage and the state transitions around
that storage:

- snapshot read views
- snapshot leases
- lease release and cleanup
- publish into the layer stack
- layer compaction for remount/autosquash paths

`layerstack` should not know about command capture, workspace upperdirs,
capture limits, bounded capture policy, or command-specific protected path
drop reporting. Those are workspace/operation responsibilities because they
depend on the writable overlay, command lifecycle, caller policy, and
workspace cleanup semantics.

## Core Decision

Move upperdir capture out of `layerstack`.

After this migration, the boundary is:

```text
workspace / operation_service:
  inspect workspace upperdir
  apply capture policy
  decide ignored/protected paths
  materialize captured changes
  call layerstack publish with already-captured changes

layerstack:
  read current stack manifest
  acquire/release leases
  accept publish payloads
  validate optimistic concurrency
  write committed layers
  compact existing layers
```

The publish API name should describe the layerstack operation directly:

```text
publish_changes_to_layerstack(...)
```

It replaces command-oriented names such as:

```text
publish_command_capture_lane_aware(...)
```

## Final Layerstack Structure

Target structure:

```text
crates/daemon/layerstack/src/
|-- lib.rs
|-- error.rs
|-- manifest.rs
|-- route.rs
|-- service.rs
|-- service/
|   |-- cache.rs
|   |-- model.rs
|   |-- support.rs
|   `-- impls/
|       |-- mod.rs
|       |-- get_snapshot.rs
|       |-- acquire_snapshot_with_lease.rs
|       |-- release_lease.rs
|       |-- publish_changes_to_layerstack.rs
|       `-- compact_snapshot_layers.rs
`-- stack/
    |-- layer_read.rs
    |-- layer_write.rs
    |-- lease_cleanup.rs
    |-- leases.rs
    |-- mod.rs
    |-- view.rs
    `-- workspace_commit.rs
```

`service.rs` should become a thin facade that exports public service APIs and
delegates implementation to `service/impls`.

Expected service responsibilities:

```text
get_snapshot(root) -> Snapshot
acquire_snapshot_with_lease(root, request_id) -> LeasedSnapshot
release_lease(root, lease_id)
publish_changes_to_layerstack(...)
compact_snapshot_layers(...)
```

Expected model split:

```text
service/model.rs:
  Snapshot
  LeasedSnapshot
  PublishChangesRequest
  PublishChangesResult
  CompactSnapshotLayersRequest
  CompactSnapshotLayersResult

service/cache.rs:
  StackHealthCache
  cached stack health helpers

service/support.rs:
  shared stack loading
  shared manifest conversion
  shared OCC/version helpers
```

The `stack/layer_read.rs` file exists only if internal layer materialization
still needs a local helper after `capture.rs` is removed. It must not expose
workspace upperdir capture APIs.

## Capture Destination

Target workspace structure:

```text
crates/daemon/workspace/src/overlay/
|-- capture.rs
`-- capture/
    |-- model.rs
    |-- upperdir.rs
    `-- materialize.rs
```

The workspace capture module owns:

```text
CaptureChangesRequest policy
upperdir traversal
ignored/protected path policy
bounded capture decisions, if any remain
conversion from upperdir state to layerstack publish payload
workspace-facing capture tests
```

`operation_service` owns command lifecycle policy and passes capture decisions
to `workspace`. `operation_service` should not call `layerstack` capture APIs
directly because no such APIs should remain.

## Removal Items

Remove from `crates/daemon/layerstack/src/service.rs`:

```text
IgnoredCaptureLimits
BoundedCaptureOptions
BoundedCapturedUpperdir
SnapshotNormalization
CommandSnapshot
acquire_bounded_snapshot_for_command
capture_upperdir_for_snapshot_with_options
command_route_probe_change
command_git_metadata_probe_needs_payload
capture_route_stats_from_metadata
ignored_limit_drop_reason
materialize_bounded_capture_changes
materialize_dropped_command_entry
materialize_direct_entry
publish_command_capture_lane_aware
```

Remove or relocate from `crates/daemon/layerstack/src`:

```text
capture.rs
capture_upperdir.rs
capture_support.rs
acquire_snapshot_with_depth_guard.rs
```

`capture_upperdir.rs`, `capture_support.rs`, and
`acquire_snapshot_with_depth_guard.rs` should not be created as replacement
layerstack service files. Their responsibilities either move to workspace or
are deleted.

Remove from layerstack public exports unless still needed by stack internals:

```text
CaptureError
CaptureStats
ProtectedPathDrop
ProtectedPathDropReason
BoundedCommandSnapshot
```

Remove from workspace/operation plumbing:

```text
CaptureChangesRequest.bounds
BoundedCaptureOptions imports from layerstack
one_shot_capture bounded options plumbing
test-only BoundedCaptureOptions fixtures
```

Remove or rewrite tests:

```text
crates/daemon/layerstack/tests/unit/capture.rs
bounded capture sections in crates/daemon/layerstack/tests/unit/route.rs
bounded snapshot test in crates/daemon/layerstack/tests/unit/service.rs
workspace and operation_service tests that construct BoundedCaptureOptions
```

If stack-level bounded snapshot has no production callers, also remove:

```text
BoundedCommandSnapshot
LayerStack::acquire_bounded_snapshot_for_command
BoundedCommandSnapshot re-export from layerstack::lib
stack tests that only exercise bounded command snapshots
```

## Expected LOC Reduction

Current measured anchors before this cleanup:

```text
crates/daemon/layerstack/src/service.rs                  ~605 LOC
crates/daemon/layerstack/src/capture.rs                  ~663 LOC
crates/daemon/layerstack/tests/unit/capture.rs           ~292 LOC
crates/daemon/layerstack/tests/unit/route.rs           ~2,074 LOC
crates/daemon/layerstack/tests/unit/service.rs           ~289 LOC
```

Expected reduction by area:

| Area | Expected reduction |
|---|---:|
| `layerstack/src/service.rs` capture and bounded snapshot code | ~320 LOC |
| `layerstack/src/capture.rs` removed or mostly moved out | ~480-660 LOC from layerstack |
| Layerstack capture and bounded tests | ~350-750 LOC |
| Workspace and operation bounded-options plumbing | ~35-60 LOC |
| Optional stack-level bounded snapshot removal | ~40-70 LOC |

Expected final impact:

```text
Layerstack LOC reduction: ~1,100-1,500 LOC
Whole repo net reduction: ~450-800 LOC
```

The whole-repo reduction is lower than the layerstack reduction because
workspace should receive the remaining real upperdir capture code. The deleted
surfaces are the command-capture coupling, bounded capture options plumbing,
depth-guard naming, and layerstack-owned capture tests.

## Migration Plan

### Phase 1: Split service facade

Create the `service/` folder and move service-owned code into:

```text
service/cache.rs
service/model.rs
service/support.rs
service/impls/mod.rs
service/impls/get_snapshot.rs
service/impls/acquire_snapshot_with_lease.rs
service/impls/release_lease.rs
service/impls/publish_changes_to_layerstack.rs
service/impls/compact_snapshot_layers.rs
```

Keep `service.rs` as the public facade during the migration to avoid a large
import churn.

### Phase 2: Remove bounded snapshot service API

Delete service-level bounded snapshot models and functions:

```text
SnapshotNormalization
CommandSnapshot
acquire_bounded_snapshot_for_command
```

Keep two public snapshot APIs:

```text
get_snapshot(...)
acquire_snapshot_with_lease(...)
```

`get_snapshot` returns a read-only snapshot view without creating a lease.
`acquire_snapshot_with_lease` returns a leased snapshot for writable workspace
lifecycles.

### Phase 3: Move upperdir capture to workspace

Move upperdir traversal and materialization into `workspace/src/overlay`.

The workspace API should expose capture in workspace terms:

```text
capture_changes(handle, request) -> CapturedWorkspaceChanges
```

The workspace implementation can then call:

```text
layerstack::service::publish_changes_to_layerstack(...)
```

The layerstack publish API should receive already-materialized changes. It
should not traverse workspace upperdirs.

### Phase 4: Delete bounded capture options

Drop `BoundedCaptureOptions` entirely unless a current production caller proves
that the policy is required.

Remove:

```text
CaptureChangesRequest.bounds
operation_service one-shot bounded capture plumbing
test fixtures that only exist to fill bounded options
```

If bounded behavior is still needed later, reintroduce it under
`workspace/src/overlay/capture/model.rs` as workspace policy, not as a
layerstack model.

### Phase 5: Rename publish and compaction APIs

Rename command/remount-oriented service methods:

```text
publish_command_capture_lane_aware -> publish_changes_to_layerstack
compact_snapshot_for_remount -> compact_snapshot_layers
```

The new names describe layerstack responsibilities and avoid encoding command
or remount call sites into the storage service API.

### Phase 6: Remove layerstack capture tests

Delete tests that validate workspace upperdir capture through layerstack.

Move only still-relevant capture behavior tests to workspace overlay tests.
Keep layerstack tests focused on:

```text
snapshot read view
lease acquisition and release
publish OCC/version behavior
layer storage materialization
compaction behavior
```

## API Boundary After Migration

Layerstack service surface:

```rust
pub fn get_snapshot(root: impl AsRef<Path>) -> Result<Snapshot, LayerStackError>;

pub fn acquire_snapshot_with_lease(
    root: impl AsRef<Path>,
    request_id: impl Into<String>,
) -> Result<LeasedSnapshot, LayerStackError>;

pub fn release_lease(
    root: impl AsRef<Path>,
    lease_id: impl AsRef<str>,
) -> Result<(), LayerStackError>;

pub fn publish_changes_to_layerstack(
    request: PublishChangesRequest,
) -> Result<PublishChangesResult, LayerStackError>;

pub fn compact_snapshot_layers(
    request: CompactSnapshotLayersRequest,
) -> Result<CompactSnapshotLayersResult, LayerStackError>;
```

Layerstack service must not expose:

```text
capture_upperdir(...)
capture_upperdir_for_snapshot(...)
capture_upperdir_for_snapshot_with_options(...)
publish_command_capture(...)
bounded command snapshot APIs
workspace upperdir traversal helpers
command protected-path policy
```

## Verification Plan

Run focused checks after each phase:

```sh
cargo test -p layerstack --tests
cargo test -p workspace --tests
cargo check -p operation_service --all-targets
cargo fmt --check
git diff --check
```

For the final cleanup pass, also run a repo-wide symbol check:

```sh
rg "BoundedCaptureOptions|BoundedCapturedUpperdir|IgnoredCaptureLimits"
rg "capture_upperdir_for_snapshot_with_options|acquire_bounded_snapshot_for_command"
rg "publish_command_capture_lane_aware|acquire_snapshot_with_depth_guard"
```

All searches should return no production references. Test references are
allowed only while a phase is actively migrating them.

## Risks

- Moving capture out of layerstack may temporarily duplicate capture helpers
  until workspace overlay tests are in place.
- Route and publish tests currently mix capture behavior with storage behavior;
  deleting them before workspace replacements exist could reduce coverage.
- `operation_service` tests may need fixture simplification after
  `BoundedCaptureOptions` is removed.
- If stack-level bounded snapshot has hidden external callers, deleting the
  public re-export could be a breaking API change. Confirm with `rg` before
  removal.

## Completion Criteria

The migration is complete when:

```text
layerstack has no public capture API
layerstack service has no command-specific capture models
workspace owns upperdir capture
operation_service reaches capture only through workspace
get_snapshot does not acquire a lease
acquire_snapshot_with_lease is the only leased snapshot facade
publish_changes_to_layerstack is the storage publish entrypoint
BoundedCaptureOptions and related bounded capture structs are gone
targeted tests and checks pass
```
