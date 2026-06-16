# Command Ignored-State Publish Policy

Status: Draft
Date: 2026-06-16
Scope: `sandbox.command.exec` in ephemeral workspace mode, LayerStack/OCC
finalization, and ordinary `.gitignore`-matched workspace paths.

Related:
- `docs/command-git-occ-policy_SPEC.md` owns `.git` metadata rules.
- `docs/sandbox-event-tracing-response-plan.md` owns trace shape direction.

## 1. Intent

Define the v1 publish policy for command-produced ignored paths such as caches,
package installs, build outputs, and local generated artifacts.

The design assumes LayerStack is head-only from the caller's point of view:
layers exist to construct snapshots, but later callers lease the active head,
not old manifest branches. Therefore the policy does not introduce a separate
cache namespace or retained stale cache candidates in v1.

The policy is intentionally generic. The daemon does not infer dependency
semantics, package manager semantics, or command intent from path names. It
classifies observed filesystem changes by the LayerStack-aware ignore oracle from
the command's leased base snapshot and publishes them according to the route.
Concurrent `.gitignore` changes affect later command snapshots, not already
running commands.

## 2. Goals

1. Preserve source truth under multi-agent concurrency.
2. Avoid discarding successful expensive installs or cache writes when source
   truth was not conflicted by the same command.
3. Keep the command surface shell-like: no command intent metadata is required.
4. Keep LayerStack head-only for callers.
5. Make every ignored-state publish/drop decision visible in trace and response
   metadata.
6. Bound ignored-state storage and memory costs.

## 3. Non-Goals

1. No dependency-specific policy in the daemon.
2. No separate cache namespace in v1.
3. No guarantee that ignored state is semantically fresh for the active source
   head.
4. No attempt to merge ignored trees semantically.
5. No daemon-side command admission or command-text parsing.

## 4. Definitions

| Term | Meaning |
| --- | --- |
| Source path | Ordinary workspace path that is not `.git` metadata, not daemon metadata, and not matched by the active `.gitignore` view. |
| Ignored path | Ordinary workspace path matched by the active LayerStack-aware `.gitignore` view. |
| Protected path | `.git/**`, LayerStack/daemon control paths, sockets, pids, and other paths that must never become ordinary command output. |
| Source lane | Captured source paths published through gated OCC. |
| Ignored lane | Captured ignored paths published without per-path content OCC, using last-writer-wins semantics after source-lane eligibility is known. |
| Direct LWW | Publish without base-content validation; if later accepted layers write the same ignored path, the newest accepted head-visible layer wins. |
| Route snapshot | The command's leased base snapshot used for ordinary ignore classification. |
| Spool-backed capture | A capture representation that records metadata first and stores accepted file payloads in bounded scratch files, not long-lived in-memory `Vec<u8>` buffers. |
| Finalization | The command-settle phase that captures the private upperdir, routes paths, validates source changes, filters ignored changes, and publishes or drops the result. |

## 5. Core Invariants

1. Every command runs with a private upperdir. Sibling commands never observe
   half-written upperdir content.
2. Failed, timed-out, or cancelled commands publish no workspace or ignored
   changes.
3. Protected paths are denied or dropped before ordinary ignore routing.
4. Source paths use gated OCC and must not be last-writer-wins.
5. Ignored paths may use direct LWW only for commands whose source lane is
   non-conflicting.
6. A command that loses source OCC must not publish its ignored lane.
7. Direct LWW is a derived-state policy, not a source-truth policy.
8. The daemon must report whether ignored changes were published, dropped, or
   skipped, and why.
9. Ordinary ignored/source routing is snapshot-scoped in v1: it uses the
   command's route snapshot and is not recomputed against the publish-time head.
10. Ignored-lane byte and file limits must be enforced before ignored file
    payloads are materialized into memory.

## 6. Route Order

Path routing is evaluated in this order:

1. Protected path rules.
2. Git metadata rules from `docs/command-git-occ-policy_SPEC.md`.
3. Ordinary ignore routing using the command's route snapshot merged
   `.gitignore` view.
4. Source path fallback.

| Route | Examples | Publish rule |
| --- | --- | --- |
| Protected | daemon metadata, sockets, pids, unsupported special paths | Deny/drop with reason. |
| Git metadata | `.git/**` | Governed by the Git OCC spec; never ordinary ignored direct. |
| Ignored | `.cache/**`, `node_modules/**`, `target/**` when ignored | Conditional direct LWW. |
| Source | lockfiles, source files, manifests, configs when not ignored | Gated OCC. |

If a project ignores a meaningful lockfile, the sandbox treats that lockfile as
ignored derived state. This is an expected limitation of a generic `.gitignore`
policy. Projects that need that file protected must not ignore it, or a future
profile override must route it to the source lane.

Route classification is deliberately not recomputed inside the serialized publish
worker in v1. This avoids paying an additional merged-view ignore walk at publish
time and gives every command a stable route decision for the lifetime of its
private upperdir. A command that starts before a `.gitignore` change may therefore
publish ignored state according to its own route snapshot. This is acceptable
because source paths still use gated OCC and ignored paths are derived state.

## 7. Finalization State Machine

Command finalization consumes:

- command exit status,
- captured upperdir changes,
- the command's base snapshot facts,
- current LayerStack head facts for source OCC validation,
- route classification for each changed path.

### 7.1 Command Did Not Succeed

If the command exits non-zero, times out, or is cancelled:

1. Drop source lane.
2. Drop ignored lane.
3. Keep command output/transcript/final artifacts.
4. Report `source_publish_status = dropped_command_failed`.
5. Report `ignored_publish_status = dropped_command_failed`.

Stderr alone is not a failure signal.

### 7.2 Command Succeeded With No Captured Changes

If capture succeeds and both lanes are empty:

1. Publish nothing.
2. Report `source_publish_status = empty`.
3. Report `ignored_publish_status = empty`.

### 7.3 Command Succeeded With Source Changes

1. Validate source lane through gated OCC.
2. If any source path conflicts or fails validation:
   - publish nothing from the command,
   - drop ignored lane,
   - report `source_publish_status = conflict` or `failed`,
   - report `ignored_publish_status = dropped_due_to_source_conflict`.
3. If source lane commits, accepts, or becomes an idempotent no-op:
   - source lane is eligible,
   - ignored lane may publish by direct LWW if it passes ignored-lane filters.

### 7.4 Command Succeeded With Only Ignored Changes

If source lane is empty and ignored lane is non-empty:

1. The command is eligible for ignored direct LWW.
2. Ignored lane publishes if it passes ignored-lane filters.
3. Report `source_publish_status = empty`.
4. Report `ignored_publish_status = published_lww` or the relevant drop reason.

This accepts the operational tradeoff that a slow ignored-only install may
become head-visible even if source changed after the command started. Source
truth remains protected because no non-ignored paths are published without OCC.

## 8. Ignored-Lane Filters

Ignored direct LWW is allowed only after these checks:

1. The command succeeded.
2. Source lane is `committed`, `accepted_noop`, or `empty`.
3. No protected path is present in the ignored lane.
4. Ignored-lane capture stayed within configured limits.
5. The final accepted change set can be written atomically to LayerStack.

Limit enforcement is a capture-time requirement, not a post-capture cleanup step.
The capture walk must collect path/kind/size metadata, classify routes, and
enforce ignored limits before reading ignored regular-file bytes into memory.
When the ignored lane exceeds a file, byte, count, or duration limit, the daemon
drops the entire ignored lane with `dropped_due_to_limits`, skips reading the
ignored payloads, and may continue source capture and source OCC.

Recommended initial limits:

| Limit | Default Direction |
| --- | --- |
| Max ignored files per command | Configurable; trace actual count. |
| Max ignored bytes per command | Configurable; trace actual bytes. |
| Max single ignored file bytes | Configurable; reject or drop ignored lane. |
| Max ignored capture duration | Configurable soft budget; trace over-budget. |
| Max LayerStack ignored depth growth | Use existing squash/compaction policy. |

If ignored-lane limits are exceeded after source lane validation succeeds, the
implementation may publish the source lane without ignored changes, provided the
source publish remains atomic and the ignored drop reason is reported.

Large ignored outputs that remain within limits must publish through a
spool-backed path. The daemon must not build a large ignored `Vec<u8>` collection
just to hand it to LayerStack. Accepted regular-file payloads should be copied,
reflinked, or hardlinked into command-owned scratch storage and then installed
into the accepted layer during the atomic publish. Scratch payloads are deleted
after publish or drop.

## 9. Publish Atomicity

When both source and ignored lanes are accepted, the preferred v1 behavior is one
LayerStack publish containing all accepted changes. That keeps a successful
command's accepted source and accepted ignored outputs visible at the same head.

If ignored lane is dropped by policy before publish, source lane may still
publish alone.

If the LayerStack write fails while writing an accepted combined change set, the
publish must fail atomically and leave the previous head visible.

The atomic write path must support both in-memory small payloads and spool-backed
large payloads. A spool file is not visible workspace state until the LayerStack
publish transaction installs it into a new layer and advances the manifest. If
manifest advancement fails, the previous head remains visible and scratch spool
files are garbage-collected.

## 10. Last-Writer-Wins Semantics

Direct LWW is still serialized by LayerStack publish mechanics. It does not mean
two writers mutate the same visible tree concurrently.

It means:

1. Ignored paths skip base-content validation.
2. Accepted ignored writes enter the next layer.
3. Later accepted layers shadow earlier ignored writes for the same path.

Direct LWW is acceptable only because ignored paths are treated as derived
runtime state. It may be stale or suboptimal, but it must not change source
truth.

## 11. Response And Trace Requirements

Every command settle trace must include:

| Field | Meaning |
| --- | --- |
| `source_path_count` | Number of routed source paths. |
| `ignored_path_count` | Number of routed ignored paths. |
| `ignored_bytes` | Captured ignored byte count, bounded. |
| `ignored_spooled_bytes` | Ignored bytes written through spool-backed payloads. |
| `source_publish_status` | `empty`, `committed`, `accepted_noop`, `conflict`, `failed`, `dropped_command_failed`. |
| `ignored_publish_status` | `empty`, `published_lww`, `dropped_command_failed`, `dropped_due_to_source_conflict`, `dropped_due_to_limits`, `dropped_protected_path`, `failed`. |
| `ignored_publish_mode` | `direct_lww` when published. |
| `ignored_drop_reason` | Stable reason code when dropped. |
| `ignore_route_source` | `command_snapshot` in v1. |
| `route_manifest_version` | Manifest version used for ordinary ignore routing. |

The result should avoid silent ignored-state drops. If ignored changes are
dropped while source changes publish, the response must make that degraded state
visible.

## 12. Biggest Improvements

### 12.1 Make Conditional Direct Publish Explicit

Current routing already has an ordinary ignored-path lane:
`.gitignore`-matched paths route to `Direct`. The important v1 improvement is
grouping and filtering at command finalization time.

Direct ignored changes must not publish if the same command lost source OCC.
This prevents stale installs, caches, or generated state from surviving a source
conflict produced by the same command.

### 12.2 Add Trace Fields

Every command settle should report the source and ignored outcomes separately:

```text
source_paths: committed | conflicted | empty | failed
ignored_paths: published_lww | dropped_due_to_source_conflict |
               dropped_due_to_failure | dropped_due_to_limits | empty
ignored_bytes: <bounded byte count>
ignored_file_count: <bounded file count>
```

Without these fields, multi-agent cache and package-install behavior is hard to
debug. A user must be able to see whether source truth published, whether
ignored state published, and why any ignored state was dropped.

### 12.3 Add Quotas For Ignored LWW

Ignored paths can contain very large trees. The daemon should bound direct LWW
capture and publish work with configurable limits:

- max ignored bytes per command,
- max ignored file count per command,
- max single ignored file bytes,
- max ignored capture time.

If ignored limits are exceeded, the daemon should drop ignored changes but keep
the source OCC result when source OCC succeeds. The response and trace must
report `ignored_publish_status = dropped_due_to_limits`.

### 12.4 Use Bounded Spool-Backed Capture

Current `LayerChange::Write` stores file content in memory as `Vec<u8>`. V1 must
not depend on that representation for large ignored trees such as package
installs.

The v1 implementation should use a two-phase capture:

1. Walk the upperdir and record path, file kind, size, and overlay marker facts.
2. Route each path with the command's route snapshot.
3. Enforce ignored-lane limits from metadata before reading ignored payloads.
4. If ignored limits are exceeded, drop the ignored lane and skip ignored file
   reads.
5. Read source payloads normally for source OCC.
6. For accepted ignored writes within limits, store payloads as spool-backed
   files and publish them without keeping the full ignored tree in memory.

This is an in-place v1 requirement. Deferred streaming work may optimize the
spool path later, but v1 must already avoid unbounded ignored payload memory.

### 12.5 Compact Ignored-Heavy Layers

If every large install creates a new layer, old ignored-state layers can pile up.
Because LayerStack is head-only from the caller's point of view, ignored-heavy
layers should be aggressively squashed or compacted so old cache/package
versions do not bloat storage.

Compaction must preserve the head-visible result and must not weaken source OCC
semantics.

## 13. Implementation Plan

1. Add a command finalization split between source and ignored lanes after
   capture and before OCC validation.
2. Make ordinary ignore routing explicitly snapshot-scoped by routing against the
   command's leased base manifest and reporting `ignore_route_source =
   command_snapshot`.
3. Keep existing `.gitignore` route semantics for ordinary paths, but make the
   direct lane conditional on source-lane outcome.
4. Replace all-at-once `Vec<u8>` capture for ignored trees with metadata-first,
   spool-backed capture.
5. Add ignored-lane status fields to command metadata and trace events.
6. Add ignored-lane limits to daemon config.
7. Ensure protected paths cannot be reintroduced through ignore routing.
8. Add compaction/squash coverage for ignored-heavy layers.

## 14. Acceptance Tests

File-backed capture is a blocking v1 acceptance area. The implementation is not
accepted if tests exercise only small in-memory ignored payloads. The test suite
must force the spool-backed path and prove both its publish and drop behavior.

1. Two commands edit the same source file and ignored paths. The losing source
   command publishes neither source nor ignored paths.
2. A command edits only ignored paths and succeeds. The ignored paths publish by
   direct LWW.
3. A command edits source and ignored paths. If source OCC succeeds, both lanes
   publish in one visible head.
4. A command exits non-zero after writing source and ignored paths. Neither lane
   publishes.
5. A command writes `.git/**` and ignored paths. `.git/**` follows the Git OCC
   spec and never becomes ordinary direct ignored output.
6. A command writes protected daemon metadata. The protected path is denied or
   dropped with a stable reason.
7. Ignored-lane file or byte limits are exceeded. Source lane can still publish
   if source OCC succeeds, and ignored lane reports `dropped_due_to_limits`.
8. Two ignored-only commands write the same ignored file. The later accepted
   publish is visible at head.
9. Trace output reports source and ignored path counts, byte counts, publish
   modes, and drop reasons.
10. A command writes an ignored file larger than the ignored single-file limit and
    a valid source file. The source file can publish, the ignored file is absent,
    and finalization does not read the oversized ignored payload into memory.
11. A command writes ignored output large enough to require spool-backed publish
    but still within configured limits. The output becomes visible at head and
    trace reports non-zero `ignored_spooled_bytes`.
12. A command starts before a `.gitignore` change and finalizes after it. Routing
    follows the command snapshot and trace reports the route manifest version.

### 14.1 Required File-Backed Capture Cases

These tests are mandatory and should fail if the implementation silently falls
back to all-in-memory `Vec<u8>` capture for ignored output:

1. Spool publish path: configure a low in-memory threshold and a higher ignored
   lane byte limit, write an ignored file above the in-memory threshold but below
   the lane limit, and assert that the file publishes, `ignored_publish_status =
   published_lww`, and `ignored_spooled_bytes > 0`.
2. Spool directory path: write multiple ignored files whose aggregate size
   requires spool-backed capture, then assert all accepted files are visible at
   head and the trace reports the aggregate spooled byte count.
3. Oversized ignored drop path: write a valid source file plus an ignored file
   above `max_single_ignored_file_bytes`; assert source can publish, ignored
   output is absent, `ignored_publish_status = dropped_due_to_limits`, and the
   oversized ignored payload was not read into memory.
4. Cleanup path: after successful publish, source-only publish with ignored drop,
   and publish failure, assert command scratch spool files are removed or marked
   for deterministic garbage collection.
5. Atomicity path: inject a LayerStack publish failure after spool files are
   prepared; assert no ignored payload becomes head-visible and the previous
   manifest remains active.

## 15. Deferred Enhancements

These are intentionally out of v1:

1. Separate cache namespaces.
2. Retained stale cache candidates for old source manifests.
3. Dependency-aware path classification.
4. Singleflight/build leases for expensive ignored namespaces.
5. Path-token indexes for cheaper source OCC validation.
6. Optimized streaming for spool-backed ignored payloads.
