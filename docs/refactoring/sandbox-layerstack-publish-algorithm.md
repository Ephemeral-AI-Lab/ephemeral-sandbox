# Sandbox LayerStack Publish Algorithm Spec

Status: Draft
Date: 2026-06-21
Scope: `sandbox-runtime-layerstack` publish safety floor and the
operation-internal `LayerStackService::publish_changes` wrapper.

## Purpose

Define the new command publish algorithm for captured workspace changes.

The algorithm replaces the old coarse "expected active root hash must still
match" publish policy with path-level OCC for source paths, direct publish for
ignored paths, and an authoritative `.git` mutation ban. The safety floor lives
inside `sandbox-runtime-layerstack` so every layerstack publish caller gets the
same protection.

## Goals

1. Publish non-gitignored source changes only when their active content still
   matches the command's base content.
2. Publish gitignored changes as direct last-writer-wins derived state only
   after the source lane is eligible.
3. Reject the whole publish when any captured change mutates `.git`.
4. Evaluate `.gitignore` rules without invoking Git.
5. Support nested `.gitignore` files from the command's base LayerStack
   snapshot.
6. Keep validation and layer creation atomic under the layerstack writer lock.
7. Keep command finalization orchestration in `sandbox-runtime/operation`, while
   keeping the publish safety floor in `sandbox-runtime-layerstack`.

## Non-Goals

- Do not reintroduce `publish_changes_to_layerstack`.
- Do not reintroduce a commit queue.
- Do not reintroduce publish-time autosquash.
- Do not call `git check-ignore`, `git status`, or any Git subprocess.
- Do not use Git index tracked/untracked state to decide ignored routing.
- Do not allow `.gitignore` to route `.git/**` into the ignored/direct lane.
- Do not move command capture into `sandbox-runtime-layerstack`.
- Do not make `sandbox-runtime-layerstack` depend on operation or workspace
  crate types.

## Ownership

`sandbox-runtime-layerstack` owns:

- publish route classification
- `.git` mutation rejection
- protected layerstack/control-path rejection
- `.gitignore` route oracle
- nested `.gitignore` inheritance
- source-path fingerprinting
- opaque-directory expansion and route validation
- atomic validate-and-publish under the writer lock

`sandbox-runtime/operation` owns:

- command success/failure gate
- calling workspace capture
- converting workspace capture DTOs into layerstack publish DTOs
- mapping publish results into command finalization metadata
- refreshing/remounting the workspace session after a successful publish

## Module Layout

Add the publish implementation under the layerstack crate:

```text
crates/sandbox-runtime/layerstack/src/stack/publish/
  mod.rs
  model.rs
  plan.rs
  route.rs
  gitignore.rs
  fingerprint.rs
  opaque_dir.rs
  validate.rs
```

Wire it from:

```text
crates/sandbox-runtime/layerstack/src/stack/mod.rs
crates/sandbox-runtime/layerstack/src/stack/ops/publish.rs
```

Keep the existing raw low-level API:

```rust
LayerStack::publish_layer(&mut self, changes: &[LayerChange])
```

Add a policy-enforced publish API:

```rust
LayerStack::publish_validated_changes(
    &mut self,
    request: PublishValidatedChangesRequest,
) -> Result<PublishValidatedChangesResult, LayerStackError>
```

The operation crate should call only the policy-enforced API for command
finalization. Tests may still use `publish_layer` to seed fixtures.

## LayerStack API

```rust
pub struct PublishValidatedChangesRequest {
    pub base: PublishBase,
    pub changes: Vec<LayerChange>,
    pub protected_drops: Vec<LayerProtectedDrop>,
}

pub struct PublishBase {
    pub manifest: Manifest,
    pub revision: PublishBaseRevision,
}

pub struct PublishBaseRevision {
    pub manifest_version: i64,
    pub root_hash: String,
    pub layer_count: usize,
}

pub struct LayerProtectedDrop {
    pub path: String,
    pub reason: LayerProtectedDropReason,
}

pub enum LayerProtectedDropReason {
    UnsupportedSpecialFile,
    InvalidLayerPath,
}

pub struct PublishValidatedChangesResult {
    pub manifest: Manifest,
    pub route_summary: PublishRouteSummary,
    pub no_op: bool,
}
```

`PublishBase.manifest` is required. `manifest_version`, `root_hash`, and
`layer_count` alone are not enough to evaluate base-snapshot `.gitignore` rules
or compute base fingerprints after the active manifest has advanced.

The implementation must verify that:

```text
base.revision.manifest_version == base.manifest.version
base.revision.root_hash == manifest_root_hash(base.manifest)
base.revision.layer_count == base.manifest.layers.len()
```

Invalid base metadata is a caller bug and should return a layerstack error
before route planning.

## Publish Routes

Every captured path resolves to one route:

```rust
enum PublishRoute {
    SourceOcc {
        expected: ContentFingerprint,
    },
    IgnoredDirect,
    Forbidden {
        reason: PublishRejectReason,
    },
}
```

Route order is fixed:

1. `.git` mutation check.
2. Protected layerstack/control path check.
3. Ordinary `.gitignore` route oracle.
4. Source fallback.

### Forbidden `.git` Mutation

Any path with a `.git` segment is forbidden:

```text
.git/config
.git/index
.git/logs/HEAD
pkg/.git/config
```

Forbidden means:

- reject the whole publish
- write no new layer
- do not publish ignored changes
- report the first offending path and reason

This is intentionally stricter than the old command Git metadata floor. The new
policy forbids shared `.git` mutation rather than attempting to validate Git
metadata.

### Protected Paths

Layerstack/control paths are forbidden before ordinary ignore routing, even if a
project `.gitignore` would match them.

Initial protected set:

```text
manifest.json
workspace.json
layers/**
staging/**
.layer-metadata/**
**/.layer-metadata/**
```

Command scratch/spool paths should also be represented in the layerstack DTO as
protected drops when capture sees them. `LayerProtectedDrop.path` is a string
because invalid captured paths cannot always be represented as `LayerPath`. The
publish algorithm treats any `LayerProtectedDrop` as publish-relevant metadata;
protected drops reject the whole publish. This preserves the simple all-or-none
rule: if any captured change cannot safely enter a publish lane, no source or
ignored changes are written.

### Ignored Direct

Ignored paths are ordinary workspace paths matched by `.gitignore` rules from
the command's base manifest.

Ignored direct publish means:

- no per-path active-content OCC validation
- last accepted layer wins for that path
- publish only after forbidden checks pass and source OCC is successful
- publish in the same new layer as accepted source changes

Ignored direct does not mean a separate second publish. A separate publish would
allow partial source-only success if the ignored publish fails.

Ignored-only captures are allowed. If there are no source paths and no forbidden
paths, ignored changes publish directly in one new layer.

### Source OCC

Source paths are ordinary workspace paths that are not forbidden, not protected,
and not matched by the base `.gitignore` view.

For every source path:

1. Compute the expected fingerprint from the base manifest.
2. Under the writer lock, compute the active fingerprint from the current active
   manifest.
3. If any active fingerprint differs from expected, reject the whole publish.
4. If all source validations pass, publish source and ignored changes together
   in one new layer.

This is path-level OCC. The active manifest may have advanced since the command
started. Advancement is allowed when the active content of every source path
touched by this command still equals the command's base content.

## Content Fingerprints

Do not use `Option<String>` alone as the long-term fingerprint model. It cannot
distinguish absent, file content, symlink target, and future metadata extensions
clearly enough.

Use a typed fingerprint:

```rust
enum ContentFingerprint {
    Absent,
    File {
        digest: String,
        executable: Option<bool>,
    },
    Symlink {
        target: String,
    },
}
```

The first implementation may leave `executable` as `None` if layer changes do
not preserve executable mode yet. The type should still leave space for it.

`MergedView` currently exposes `read_bytes(...)`. The publish implementation
should add a metadata-aware read helper so file content and symlink target do not
collapse into the same fingerprint by accident.

Deletes validate the fingerprint of the deleted path. Writes validate the
fingerprint of the written path. Symlink changes validate the fingerprint of the
changed path.

## `.gitignore` Oracle

Use the Rust `ignore` crate, not Git:

```rust
ignore::gitignore::GitignoreBuilder
```

The root workspace already has `ignore = "0.4"`; add it to
`crates/sandbox-runtime/layerstack/Cargo.toml` as a workspace dependency.

The oracle reads `.gitignore` files from the base manifest through
`MergedView`, not from the live workspace path and not from the host checkout.

### Inheritance Algorithm

For a path `frontend/dist/app.js`:

1. Read root `.gitignore` from the base manifest.
2. Match the full relative path against root rules.
3. Walk each ancestor directory.
4. For `frontend/.gitignore`, match `dist/app.js` relative to `frontend/`.
5. Apply whitelist and ignore matches in traversal order.
6. If an ancestor directory is excluded, the subtree is sealed; deeper `!`
   patterns cannot rescue descendants below that excluded directory.

Build each directory matcher with:

```rust
GitignoreBuilder::new(".")
```

The caller is responsible for passing the path relative to the directory that
owns the `.gitignore`. Do not build nested matchers with the nested directory as
the builder root; that can double-strip repeated path prefixes for anchored
patterns.

Invalid UTF-8 `.gitignore` content contributes no rules. Invalid individual
patterns should be ignored consistently with `ignore` crate behavior.

### Required Semantics

Tests must cover:

- `node_modules/` matches at any depth.
- `logs/*.log` does not match `logs/sub/x.log`.
- `**/build/` matches across path segments.
- nested `.gitignore` applies only to its subtree.
- `!keep.log` re-includes in a non-sealed directory.
- an ignored directory seals descendants against deeper re-includes.
- `.gitignore` content published in newer base layers affects later command
  snapshots.
- `.gitignore` cannot route `.git/**` to direct or source.

## Opaque Directory Changes

`LayerChange::OpaqueDir` can hide many lower-layer descendants. A single route
for the opaque path is not enough.

For an opaque directory:

1. Expand visible descendants hidden by the opaque marker from the base manifest.
2. If expansion exceeds a bounded limit, reject the whole publish with
   `OpaqueDirExpansionLimit`.
3. Route every hidden descendant using the normal route order.
4. If any hidden descendant is forbidden or protected, reject the whole publish.
5. If hidden descendants include both source and ignored routes, reject the whole
   publish with `OpaqueDirMixedRoutes`.
6. If all hidden descendants are ignored, route the opaque marker as
   `IgnoredDirect`.
7. If all hidden descendants are source, route the opaque marker as `SourceOcc`
   and validate every hidden descendant fingerprint before publishing.
8. If there are no hidden descendants, route the opaque marker itself using the
   normal route order.

The initial expansion limit should be small and explicit, for example `4096`,
matching the old bounded route behavior.

## Atomic Publish Algorithm

High-level operation:

```text
plan = plan_publish(base_manifest, changes, protected_drops)
if plan has forbidden:
  reject without writer-side validation

lock layerstack writer
active = read active manifest
validate source fingerprints against active
if any conflict:
  reject without writing a layer
if accepted changes are empty:
  return active manifest as no-op
write one layer containing source + ignored changes
advance manifest
return new manifest and route summary
```

Implementation detail: planning needs the base manifest and can happen before
the writer lock. The final active-content validation and layer write must happen
under one exclusive writer lock.

Do not call `publish_layer` after validation if that would reacquire the writer
lock and re-read active state separately. Extract an internal unlocked layer
write helper if needed:

```rust
fn publish_layer_unlocked(
    &mut self,
    active: &Manifest,
    changes: &[LayerChange],
) -> Result<Manifest, LayerStackError>
```

Then implement:

```rust
pub fn publish_validated_changes(
    &mut self,
    request: PublishValidatedChangesRequest,
) -> Result<PublishValidatedChangesResult, LayerStackError> {
    let plan = publish::plan(&self.view, &request)?;
    if let Some(reject) = plan.first_forbidden() {
        return Err(LayerStackError::PublishRejected(reject));
    }

    let _guard = self.writer_lock.exclusive()?;
    let active = self.read_active_manifest_unlocked()?;
    publish::validate_source_paths(&self.view, &active, &plan)?;
    let changes = plan.accepted_changes();
    if changes.is_empty() {
        return Ok(no_op_result(active, plan.summary()));
    }
    let manifest = self.publish_layer_unlocked(&active, &changes)?;
    Ok(published_result(manifest, plan.summary()))
}
```

The exact error names may differ, but the locking boundary must not.

## Error Model

Add publish-specific error variants to `LayerStackError` or wrap them in a
dedicated error that `LayerStackError` can carry.

Required reject reasons:

```rust
enum PublishRejectReason {
    InvalidBaseRevision,
    GitMutationForbidden,
    ProtectedPath,
    SourceConflict,
    OpaqueDirProtectedDescendant,
    OpaqueDirMixedRoutes,
    OpaqueDirExpansionLimit,
    RoutePreparationFailed,
}
```

A source conflict should report:

```rust
struct SourceConflict {
    path: LayerPath,
    expected: ContentFingerprint,
    actual: ContentFingerprint,
}
```

Command finalization should be able to return a structured reason to the caller,
but layerstack should not depend on the command response schema.

## Operation Integration

`LayerStackService::publish_changes` remains in:

```text
crates/sandbox-runtime/operation/src/internal/layerstack/service/impls/publish_changes.rs
```

It should:

1. Receive captured `LayerChange` values, protected drops, and base revision data.
2. Construct a layerstack-native `PublishValidatedChangesRequest`.
3. Call `LayerStack::publish_validated_changes`.
4. Convert the resulting manifest into operation `LayerStackRevision` and
   absolute layer paths.
5. Map publish rejection into command finalization failure metadata.

The operation wrapper must not call raw `LayerStack::publish_layer` for command
publication.

## Current Data Gap

Current workspace capture exposes `base_revision`, but not the base `Manifest`.
The publish algorithm requires the exact base manifest.

Acceptable fixes:

1. Extend the leased snapshot/service model to carry the base `Manifest` through
   `WorkspaceHandle` and `CapturedWorkspaceChanges`.
2. Add a layerstack lease lookup API that returns the manifest for the active
   workspace lease during finalization.

Prefer option 1 if it keeps the base manifest immutable and explicit in the
command session. Avoid reconstructing a base manifest from only layer path
strings in the operation crate.

## Tests

Layerstack unit tests:

- source write succeeds when active fingerprint equals base fingerprint
- source write conflicts when active fingerprint differs
- source delete conflicts when active path changed
- ignored write publishes direct when there are no source paths
- mixed source plus ignored publishes in one layer after source validation
- source conflict prevents ignored direct publish
- `.git/config` rejects the whole publish
- `pkg/.git/config` rejects the whole publish
- `.gitignore` matching `*` does not bypass `.git` rejection
- root `.gitignore` direct routes ignored output
- nested `.gitignore` is scoped to its subtree
- published upper-layer `.gitignore` affects later base snapshots
- invalid `.gitignore` does not panic and contributes no rules
- protected layerstack path rejects the whole publish
- opaque directory over all source validates hidden descendants
- opaque directory over all ignored publishes direct
- opaque directory over mixed source/ignored rejects
- opaque directory over `.git` or protected descendants rejects
- opaque directory expansion limit rejects

Operation tests:

- successful command finalization calls publish
- failed, killed, timed-out, or cancelled command does not publish
- publish conflict marks finalization failed with structured metadata
- successful publish refreshes/remounts session from the publish result
- operation wrapper never calls raw `publish_layer` for command publication

Regression tests from old history to preserve:

- nested `.gitignore` support
- no Git subprocess for ignored routing
- no Git index/status dependency for ignored routing
- `.gitignore` cannot route `.git/**` to direct/source
- source conflict cancels ignored publish
- ignored-only publish is last-writer-wins

## Verification

Focused commands:

```sh
cargo fmt --check -p sandbox-runtime-layerstack -p sandbox-runtime
cargo test -p sandbox-runtime-layerstack --all-targets
cargo test -p sandbox-runtime --all-targets
cargo clippy -p sandbox-runtime-layerstack -p sandbox-runtime --all-targets --no-deps -- -D warnings
git diff --check
```

When the base-manifest model changes, also run downstream crates that construct
or serialize workspace/session DTOs:

```sh
cargo check -p sandbox-daemon -p sandbox-gateway -p sandbox-manager -p sandbox-protocol --all-targets
```
