# OCC Merge Publish Spec

Status: draft

This document specifies a narrow publish-time auto-merge policy for
`sandbox-runtime-layerstack`.

The goal is to keep layerstack's existing full-file layer model while reducing
false `SourceConflict` rejections when two sessions make non-overlapping text
edits to the same non-gitignored file.

## Problem

Layerstack currently validates non-gitignored source paths with optimistic
concurrency control at file-path granularity:

```text
expected = fingerprint(path, command_base_manifest)
actual   = fingerprint(path, active_manifest)

if expected != actual:
  reject SourceConflict
```

That policy is safe, but it rejects these cases:

```text
base README.md:
  line 1
  line 2

active README.md:
  line 1 changed by another session
  line 2

command README.md:
  line 1
  line 2 changed by this session
```

The edits do not overlap, but the file fingerprint changed, so publish rejects
the entire changeset.

## Decision

Add a text-only auto-merge fallback during validated publish.

When source-path OCC detects a fingerprint mismatch, layerstack may attempt a
three-way merge for eligible regular-file writes:

```text
base    = materialized path bytes from request.base.manifest
active  = materialized path bytes from current active manifest
command = captured file bytes from the outgoing LayerChange

merged = three_way_merge(base, active, command)
```

If the merge is clean, publish a normal full-file `LayerChange::Write` with the
merged bytes on top of the active manifest. If the merge is not clean, keep the
current `SourceConflict` rejection behavior.

The persisted layer format does not change.

## Non-Goals

- Do not add patch or hunk layer types.
- Do not persist edit scripts.
- Do not attempt binary merges.
- Do not auto-merge symlinks, deletes, directories, opaque directories, or file
  type changes.
- Do not shell out to `git`.
- Do not make ignored-path writes participate in source OCC.
- Do not publish partial changesets.

## Current Behavior

The current validated publish path is:

```text
LayerStack::publish_validated_changes
  plan_publish(view, request)
    validate_base_revision(request)
    classify each change as Source or Ignored using request.base.manifest
    record SourceValidation { path, expected_fingerprint } for source paths

  lock writer
  active = read_active_manifest_unlocked()
  validate_source_paths(view, active, plan)
    for each SourceValidation:
      actual = content_fingerprint(view, active, path)
      if actual != expected:
        reject SourceConflict

  publish_layer_unlocked(active, plan.accepted_changes())
```

Relevant code:

- `crates/sandbox-runtime/layerstack/src/stack/ops/publish.rs`
- `crates/sandbox-runtime/layerstack/src/stack/publish/plan.rs`
- `crates/sandbox-runtime/layerstack/src/stack/publish/validate.rs`
- `crates/sandbox-runtime/layerstack/src/stack/publish/fingerprint.rs`
- `crates/sandbox-runtime/layerstack/src/stack/publish/model.rs`

`LayerChange::Write` and `LayerChange::WriteFile` are both logical regular-file
writes:

- `Write { content }` carries bytes in memory.
- `WriteFile { source_path, size }` points at a spooled file that is copied into
  the committed layer after size validation.

Workspace overlay capture currently materializes regular files into `Write`
with `MAX_CAPTURE_FILE_BYTES = 8 * 1024 * 1024`. `WriteFile` is supported by
layerstack but is not the normal workspace capture output today.

## Proposed Behavior

Replace `validate_source_paths` with a resolver that validates and may rewrite
the outgoing changes:

```text
resolve_publish_changes(view, request, active, plan) -> Vec<LayerChange>
```

The resolver must process the entire planned changeset before any layer is
committed. It returns either:

- all resolved changes, including any auto-merged writes, or
- one `PublishRejected` error.

No partial result is committed.

### High-Level Algorithm

```text
publish_validated_changes(request):
  plan = plan_publish(view, request)

  lock writer
  active = read_active_manifest_unlocked()

  changes = resolve_publish_changes(view, request.base.manifest, active, plan)
    if any source validation fails and cannot auto-merge:
      reject whole publish

  if changes is empty:
    return no_op with active manifest

  publish_layer_unlocked(active, changes)
```

`publish_layer_unlocked` remains responsible for staging the full layer,
renaming it into place, checking that the active manifest has not changed, and
writing the next active manifest.

### Per-Change Algorithm

```text
for each planned change:
  route = Source or Ignored from the plan

  if route == Ignored:
    keep change as-is
    continue

  expected = fingerprint(base_manifest, path)
  actual   = fingerprint(active_manifest, path)

  if actual == expected:
    keep change as-is
    continue

  merged = try_auto_merge_source_write(path, base_manifest, active_manifest, change)

  if merged is Clean:
    replace change with LayerChange::Write { path, content: merged_bytes }
  else:
    reject SourceConflict { path, expected, actual }
```

### Merge Eligibility

Auto-merge is eligible only when all of these are true:

- the route is `Source`
- the outgoing change is `LayerChange::Write` or `LayerChange::WriteFile`
- `base` entry is a regular file
- `active` entry is a regular file
- `command` entry is regular-file bytes from the outgoing write
- all three byte sequences are text according to the selected text policy
- all three byte sequences are below the per-file merge size cap
- the total merge input bytes for this publish remain below the per-publish cap

Otherwise, the resolver returns the current `SourceConflict` behavior.

### Text Policy

Initial policy:

- require valid UTF-8
- reject inputs containing NUL bytes
- preserve existing line endings by operating on line slices, not normalized
  strings

This deliberately excludes binary files and ambiguous encodings. The policy can
be broadened later only after tests demonstrate exact byte preservation.

### Three-Way Merge Semantics

The merge base is the materialized file content from `request.base.manifest`.

The two sides are:

- `active`: the file content currently visible in the active manifest
- `command`: the file content captured from the command workspace

Clean merge:

- active and command changed disjoint base ranges
- both sides made the same replacement for an overlapping range
- one side did not change a range changed by the other side

Conflict:

- active and command changed overlapping base ranges differently
- base, active, or command is not a regular file
- either side deleted or type-changed the path
- merge input exceeds a cap
- the merge implementation cannot prove a clean merge

The merge output becomes a normal full-file write:

```rust
LayerChange::Write {
    path,
    content: merged_bytes,
}
```

## Atomicity

Auto-merge must preserve current changeset atomicity.

Current publish atomicity is manifest-level:

```text
plan all changes
lock writer
validate all source paths
write one staging layer
rename staging layer into layers/
write layer digest
verify active manifest is still the locked active manifest
write new active manifest
```

The new flow keeps this shape:

```text
plan all changes
lock writer
resolve all source paths, including auto-merges
if any merge fails:
  reject without publishing anything
write one layer containing all resolved changes
write new active manifest
```

If file A auto-merges but file B conflicts, neither file is published.

If layer staging or digest writing fails, no active manifest update happens.
If a later manifest check fails, the prepared layer and digest are removed as
today.

This is not a general filesystem transaction. Crash recovery may still need to
ignore or clean orphan staging/layer artifacts. Visibility is controlled by the
active manifest.

## Leases And Multi-Layer Manifests

Auto-merge operates on materialized manifest views, not individual layers.

For a source path, the three views are:

```text
base    = MergedView::read_entry(path, request.base.manifest)
active  = MergedView::read_entry(path, active_manifest)
command = outgoing LayerChange bytes
```

This remains correct when either manifest contains many layers. The merged view
resolves the effective path content by layer precedence.

Leases are retention and remount safety, not merge participants:

- acquiring a workspace snapshot records a leased manifest and refcounts its
  layers
- release removes only layers that are no longer active and no longer leased
- compaction may replace layer identities while preserving visible content

The merge code must use the `base_manifest` carried by the publish request. It
must not re-read a lease by id during publish. A workspace session can be
remounted or retargeted after creation; the publish request is the authoritative
base snapshot for the captured changes.

Other active leases that are not the publishing command's base are irrelevant to
merge semantics.

## Ignored Paths

Ignored-path routing remains based on `.gitignore` rules from
`request.base.manifest`.

Ignored changes are not source-validated and are not auto-merged. If a publish
contains both source and ignored changes:

- source merge success means source and ignored changes publish together in one
  layer
- source merge failure rejects the whole publish, including ignored changes

This preserves current mixed-route atomicity.

## Performance

The common path should not pay merge cost.

Auto-merge is attempted only after existing source OCC detects:

```text
fingerprint(base, path) != fingerprint(active, path)
```

For non-conflicting source paths, publish stays on the current validation path.

For stale source writes, the additional work is:

- read base file bytes
- read active file bytes
- read command bytes if the change is `WriteFile`
- run text merge
- allocate merged output

Initial guardrails:

- per-file merge input cap: 8 MiB maximum, matching current workspace capture
  cap unless implementation chooses a smaller constant
- per-publish merge input cap: implementation-defined, required before release
- text-only eligibility
- conflict fallback instead of unbounded work

Metrics/tracing should count:

- auto-merge attempted
- auto-merge clean
- auto-merge conflict
- auto-merge ineligible by reason
- bytes processed

## Error Semantics

Keep `PublishRejectReason::SourceConflict` for failed auto-merge.

The existing `SourceConflict` payload should still report:

- path
- expected base fingerprint
- actual active fingerprint

Optional future extension:

```rust
pub enum SourceConflictDetail {
    FingerprintMismatch,
    AutoMergeConflict,
    AutoMergeIneligible { reason: AutoMergeIneligibleReason },
}
```

Do not add this unless callers need machine-readable distinction between
"changed and not mergeable" and "changed and merge attempted but conflicted".

## Implementation Plan

1. Extend publish planning output so each accepted change carries its route
   (`Source` or `Ignored`) in stable order.

2. Replace `validate_source_paths` with a resolver:

   ```text
   resolve_publish_changes(view, base_manifest, active_manifest, plan)
     -> Vec<LayerChange>
   ```

3. Add helper APIs:

   ```text
   read_regular_file_bytes(view, manifest, path) -> Option<Vec<u8>>
   read_change_bytes(change) -> Option<Vec<u8>>
   try_auto_merge_source_write(...)
   ```

4. Add text merge implementation behind a small internal boundary:

   ```text
   publish::merge::three_way_merge(base, active, command)
     -> MergeOutcome
   ```

   The implementation may use a focused dependency or a local algorithm, but
   callers must see only `Clean(Vec<u8>)` or `Conflict`.

5. Keep `publish_layer_unlocked` unchanged except for receiving resolved
   changes.

6. Add structured tracing at the operation boundary when tracing work lands:

   ```text
   layerstack.occ.resolve_source_paths
   layerstack.occ.auto_merge_attempted
   layerstack.occ.auto_merge_finished
   ```

## Test Plan

Layerstack unit tests:

- source publish succeeds when active matches base
- stale source write with non-overlapping text edits auto-merges
- stale source write with overlapping text edits rejects
- stale source write where both sides make the same edit succeeds
- active file changed to symlink rejects
- base path absent and active path present rejects
- command delete vs active edit rejects
- command symlink vs active edit rejects
- binary file mismatch rejects
- invalid UTF-8 mismatch rejects
- oversized merge input rejects
- multiple source writes all merge clean and publish in one layer
- one source write merges and another conflicts: whole publish rejects
- mixed source and ignored changes publish together when source merge succeeds
- mixed source and ignored changes reject together when source merge fails
- `WriteFile` source write merge reads and validates the spooled file
- `WriteFile` source write rejects if spool size changes before publish
- digest dedupe still reports no-op when resolved changes match head digest

Workspace/operation tests:

- captured workspace changes still carry `base_manifest`
- publish rejection remains structured as `SourceConflict`
- session refresh after successful publish updates the base revision and
  manifest
- remount after publish sees the merged active manifest

Lease/compaction tests:

- auto-merge succeeds when command base has multiple layers
- auto-merge succeeds after unrelated active layers are added
- auto-merge uses the request base manifest, not the active manifest suffix
- auto-merge remains correct after lease parent compaction retargets the
  session manifest
- releasing unrelated leases does not affect merge results

## Rollout

1. Land tests that characterize current conservative conflicts.
2. Add internal merge module and unit tests independent of layerstack publish.
3. Wire resolver into `publish_validated_changes`.
4. Add mixed changeset and lease/compaction coverage.
5. Add counters/tracing fields when the runtime tracing implementation exists.
6. Keep the feature enabled by default only after conflict, atomicity, and
   performance caps are tested.

## Open Questions

- Which diff/merge implementation should be used?
- Should the initial text cap equal the existing 8 MiB capture cap, or should
  publish use a lower cap for tighter latency bounds?
- Do callers need a machine-readable auto-merge rejection detail, or is
  `SourceConflict` sufficient?
- Should clean auto-merges be reported in command finalization metadata?
