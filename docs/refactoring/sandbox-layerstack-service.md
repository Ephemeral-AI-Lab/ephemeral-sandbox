# Sandbox LayerStack Service Spec

This spec defines the operation-internal layerstack service used by runtime
command finalization and explicit layerstack maintenance.

## Goal

Add a small `LayerStackService` under `sandbox-runtime` operation internals.
The service owns publish and squash orchestration for one sandbox layer stack.
It wraps `sandbox-runtime-layerstack` primitives without reintroducing the old
commit queue, publish-time autosquash, or workspace-owned publication behavior.
The publish safety floor lives in `sandbox-runtime-layerstack`; this operation
service only adapts command finalization to that layerstack API.

Related:

- `docs/refactoring/sandbox-layerstack-publish-algorithm.md` defines the
  authoritative publish route/OCC/ignored-lane algorithm.

## Location

```text
crates/sandbox-runtime/operation/src/internal/layerstack/
  mod.rs
  error.rs
  service.rs
  service/
    core.rs
    model.rs
    impls/
      mod.rs
      publish_changes.rs
      squash.rs
```

Wire it from:

```text
crates/sandbox-runtime/operation/src/internal/mod.rs
crates/sandbox-runtime/operation/src/internal/services.rs
```

Add a direct runtime facade dependency:

```toml
sandbox-runtime-layerstack.workspace = true
```

## Ownership

`LayerStackService` is per sandbox. It owns the layer stack location as service
state, not as an argument on each operation.

```rust
pub struct LayerStackService {
    layer_stack_root: PathBuf,
    binding: WorkspaceBinding,
}
```

`layer_stack_root` is the storage root containing:

```text
manifest.json
workspace.json
layers/
staging/
.layer-metadata/
```

It is not `workspace_root`. `workspace_root` is the mounted workspace path used
by command execution and overlay capture. `layer_stack_root` is where committed
layers and the active manifest live.

The workspace binding does not remove the need for `layer_stack_root` service
state because reading the binding requires knowing where `workspace.json` lives.
The binding is still useful for validation and for checking that the service was
initialized against the expected sandbox workspace.

## Boundary

Workspace captures changes. Layerstack publishes changes.

```text
command exits successfully
  -> WorkspaceSessionService::capture_session_changes(...)
  -> LayerStackService::publish_changes(...)
  -> WorkspaceSessionService::refresh_after_publish(...)
  -> optional WorkspaceRemountService::remount_workspace_session(...)
```

`LayerStackService` must not call workspace capture APIs. It receives
`LayerChange` values and base revision data that were produced elsewhere.

`WorkspaceSessionService` must not directly call `LayerStack::publish_layer`.
It should call the operation-internal `LayerStackService` when publication is
part of command finalization.

## API

### Service

```rust
impl LayerStackService {
    pub fn new(layer_stack_root: PathBuf) -> Result<Self, LayerStackServiceError>;

    pub fn publish_changes(
        &self,
        request: PublishChangesRequest,
    ) -> Result<PublishChangesResult, LayerStackServiceError>;

    pub fn squash(&self) -> Result<SquashLayerStackResult, LayerStackServiceError>;
}
```

### Models

```rust
pub struct LayerStackRevision {
    pub manifest_version: i64,
    pub root_hash: String,
    pub layer_count: usize,
}

pub struct PublishChangesRequest {
    pub expected_base: LayerStackRevision,
    pub base_manifest: sandbox_runtime_layerstack::Manifest,
    pub protected_drops: Vec<sandbox_runtime_layerstack::LayerProtectedDrop>,
    pub changes: Vec<sandbox_runtime_layerstack::LayerChange>,
}

pub struct PublishChangesResult {
    pub revision: LayerStackRevision,
    pub layer_paths: Vec<PathBuf>,
}

pub struct SquashLayerStackResult {
    pub squashed: bool,
    pub revision: Option<LayerStackRevision>,
    pub layer_paths: Vec<PathBuf>,
    pub lease_release_error: Option<String>,
}
```

`expected_base` and `base_manifest` are required for command publication. They
come from the command's leased base snapshot. `base_manifest` is needed so
layerstack can evaluate base-snapshot `.gitignore` rules and compute source path
fingerprints even when the active manifest has advanced.

## Publish Changes

`publish_changes` commits captured workspace changes into the sandbox layer
stack through the policy-enforced layerstack publish API.

Algorithm:

1. Open `LayerStack::open(self.layer_stack_root.clone())`.
2. Convert `PublishChangesRequest` into
   `sandbox_runtime_layerstack::PublishValidatedChangesRequest`.
3. Call `LayerStack::publish_validated_changes(...)`.
4. Convert the returned manifest into `LayerStackRevision` and absolute layer
   paths.
5. Return `PublishChangesResult`.

The operation wrapper must not call raw `LayerStack::publish_layer` for command
publication.

The operation must not call `squash()` after publishing. Squash is explicit
maintenance.

### Base Revision Behavior

The operation layer passes the command base revision and base manifest into
layerstack. Layerstack validates that they agree:

```text
expected_base.manifest_version == base_manifest.version
expected_base.root_hash == manifest_root_hash(base_manifest)
expected_base.layer_count == base_manifest.layers.len()
```

The active manifest is allowed to advance after the command starts. Conflict
detection is path-level and owned by `LayerStack::publish_validated_changes`.
The error should include enough route/conflict detail so command finalization can
report a precise publish failure.

### Empty Changes

Empty changes should be a successful no-op. This allows commands that only read
state to complete without creating empty layers.

## Squash

`squash()` is explicit layer compaction.

Current layerstack behavior:

- No `max_depth`.
- No `can_squash`.
- Compress every safe multi-layer run.
- Preserve active lease-head layers as boundaries.
- Return no-op when no safe multi-layer run exists.

Algorithm:

1. Open `LayerStack::open(self.layer_stack_root.clone())`.
2. Call `LayerStack::squash()`.
3. If `SquashOutcome.manifest` is `None`, return `squashed: false`.
4. If a manifest was produced, return `squashed: true`, the new revision, and
   absolute layer paths.
5. Preserve `lease_release_error` as a warning string when present.

Squash is not allowed to invalidate active command snapshots. Lease-head layers
remain manifest boundaries, so live snapshots can still read their historical
view.

## Errors

```rust
pub enum LayerStackServiceError {
    Init {
        layer_stack_root: PathBuf,
        error: String,
    },
    InvalidBaseRevision {
        expected: LayerStackRevision,
        base: LayerStackRevision,
    },
    PublishRejected {
        reason: String,
    },
    LayerStack {
        operation: &'static str,
        error: sandbox_runtime_layerstack::LayerStackError,
    },
}
```

Use `thiserror::Error`. Keep errors internal to the operation crate unless a
public runtime operation exposes layerstack maintenance directly later.

## Integration

`SandboxRuntimeOperations` should eventually hold:

```rust
pub struct SandboxRuntimeOperations {
    pub command: Arc<CommandOperationService>,
    pub layerstack: Arc<LayerStackService>,
}
```

If each sandbox runtime already has a single layer stack, construct
`LayerStackService` during sandbox runtime setup after the layer stack base and
workspace binding are initialized.

Command finalization should use this sequence:

```text
take process exit
if exit was killed or failed:
  do not publish
if exit succeeded:
  capture workspace changes
  publish captured changes with captured base revision
  refresh workspace session snapshot from publish result
complete command record
```

`WorkspaceSession::refresh_after_capture` should be replaced or complemented by
`refresh_after_publish`, because capture alone does not create a new active
manifest or layer path list. The publish result is the source of truth for the
post-command snapshot.

## Non-Goals

Do not add:

- `publish_changes_to_layerstack`
- a commit queue
- old commit/worker/OCC route queue modules
- `can_squash(max_depth)`
- `squash(max_depth)`
- publish-time autosquash
- a workspace dependency inside `LayerStackService`
- request-level `root: PathBuf` for publish or squash

## Tests

Layerstack service tests should cover:

- `new` loads and validates the binding.
- `publish_changes` rejects invalid base revision metadata.
- `publish_changes` with empty changes returns the current revision.
- `publish_changes` writes a new layer and returns updated layer paths.
- `publish_changes` reports layerstack source conflicts.
- `publish_changes` reports layerstack `.git` mutation rejection.
- `publish_changes` publishes ignored-only changes through the layerstack
  validated publish API.
- `squash` returns no-op for a single-layer stack.
- `squash` compacts multiple unleased layers.
- `squash` respects active lease-head boundaries.
- command finalization publishes only after successful command exit.
- cancelled or killed commands do not publish captured changes.

Focused verification:

```sh
cargo fmt --check -p sandbox-runtime -p sandbox-runtime-layerstack
cargo test -p sandbox-runtime-layerstack
cargo test -p sandbox-runtime
```
