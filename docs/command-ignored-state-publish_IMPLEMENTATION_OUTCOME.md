# Command Ignored-State Publish: Milestone 1 Outcome And Handoff

Date: 2026-06-16

Spec: `docs/command-ignored-state-publish_SPEC.md`

## Scope Completed

This iteration completed Milestone 1: non-success command discard semantics and
lane metadata publication. It did not attempt the later protected/Git/opaque
lane routing, file-backed spool capture, lane-aware publish API replacement, or
compaction work.

The final behavior is:

- Non-success ephemeral commands do not publish source or ignored writes into
  the mutable layer.
- Non-success finalization still records bounded lane diagnostics for response
  metadata and tracing.
- Responses include a flattened `publish_lanes` metadata object.
- Finalize tracing emits `command.publish_lanes_decided`.
- Timeout and cancellation paths now use the same finalizer path as ordinary
  nonzero command exits, so discard behavior is consistent.

## Milestone Checklist

| Item | Status | Notes |
| --- | --- | --- |
| Gate source and ignored publish for non-success commands | Complete | The non-success branch returns before OCC conflict handling, LayerStack publish, and spool installation. |
| Include flattened `publish_lanes` response metadata | Complete | Metadata records source and ignored lane statuses plus routing counts/bytes. |
| Emit `command.publish_lanes_decided` trace event | Complete | Foreground response trace and durable finalize records include the same lane object. |
| Route timeout/cancel/nonzero through shared finalizer | Complete | Lifecycle discard paths now call `finalize_ephemeral_command`. |
| Preserve successful command behavior | Complete | Successful commands still use the existing capture/publish path, with lane metadata derived from the current route snapshot. |
| Add unit and contract coverage | Complete | Operation, LayerStack, trace, response flattening, and fixture coverage were updated. |
| Add live E2E coverage | Complete | Added a workspace runtime command test proving nonzero source and ignored writes are both discarded while metadata is present. |
| Run focused and full verification | Complete | Cargo unit suites, package build, and full workspace-runtime-command live E2E suite passed. |

## Implementation Notes

Primary implementation files:

- `crates/daemon/operation/src/command/contract.rs`
  - Added `PUBLISH_LANES_METADATA_KEY`.
  - Added `PublishLanesMetadata`, source/ignored lane metadata, routing metadata,
    and insertion helpers.
- `crates/daemon/operation/src/command/finalize.rs`
  - Added the non-success publish gate before OCC/publish/spool side effects.
  - Added response metadata for dropped source and ignored lanes.
  - Added lane metadata for successful command responses.
- `crates/daemon/operation/src/command/service/lifecycle.rs`
  - Routed ephemeral timeout, cancellation, and non-success lifecycle outcomes
    through `finalize_ephemeral_command`.
- `crates/daemon/operation/src/command/trace.rs`
  - Added `publish_lanes` to finalize trace facts.
  - Emitted `command.publish_lanes_decided`.
- `crates/daemon/layerstack/src/commit/mod.rs`
  - Added `CaptureRouteStats`.
  - Added snapshot-manifest route classification so lane diagnostics are stable
    against the finalize-time route manifest.
- `crates/daemon/layerstack/src/service.rs`
  - Exposed `capture_route_stats_for_snapshot`.
- `crates/daemon/layerstack/src/lib.rs`
  - Re-exported `CaptureRouteStats`.

## Post-Review Fixes

The review pass found two Phase 1 correctness gaps. Both were fixed in this
follow-up:

- `publish_capture_with_options` now prepares route decisions and gated base
  hashes from the command snapshot manifest and passes those decisions to the
  commit worker. Successful command publish routing no longer drifts to the
  active head's current `.gitignore` view after the command snapshot was leased.
- Non-success ephemeral finalization now falls back to a terminal
  `dropped_command_failed` response with flattened `publish_lanes` metadata when
  upperdir payload capture itself fails, such as an oversized failed-command
  ignored write. The fallback does not enter OCC or publish a LayerStack layer.
- Added regression coverage for snapshot-route publish drift and oversized
  non-success capture fallback.

Test and fixture files updated:

- `crates/daemon/layerstack/tests/unit/route.rs`
- `crates/daemon/operation/tests/command/service.rs`
- `crates/daemon/operation/tests/contract.rs`
- `crates/daemon/operation/fixtures/command_finalize_conflict_response.json`
- `crates/e2e-test/tests/workspace-runtime-command/command_error_and_backpressure.rs`

## Verification

Commands run:

```sh
cargo fmt
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack capture_route_stats_use_supplied_manifest_snapshot
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation command::
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation --all-targets
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo run -p e2e-test --bin e2e-runner -- --suites workspace-runtime-command --max-parallel 5 --container-weight-cap 10 --heavy-test-threads 4
git diff --check
```

Results:

- `cargo fmt` passed.
- Focused LayerStack route stats and snapshot-publish regression tests passed.
- Focused operation command tests passed.
- Full `layerstack` package tests passed.
- Full `operation --all-targets` tests passed.
- `xtask package` passed and rebuilt `dist/eosd-linux-amd64`.
- Final live workspace-runtime-command E2E run passed 60/60.
- `git diff --check` passed.

Live E2E report root:

```text
crates/e2e-test/test-reports/runs/e2e-run-1781606945305
```

Important verification context:

- An initial live E2E attempt used a stale packaged daemon and failed the new
  test. Rebuilding with `cargo run -p xtask -- package` fixed that.
- One existing `setsid_nohup_contract` case failed once during a full run,
  passed when rerun alone, and passed again in the final full 60/60 suite.

## Subagent Coordination

The central plan was split into implementation, test coverage, and adversarial
review lanes.

- One subagent completed a useful implementation pass touching the operation
  contract, finalizer, lifecycle, trace, tests, and fixture work. Its output was
  reviewed and integrated against the spec.
- Two subagent runs failed because the selected model was at capacity. Their
  intended coverage was completed locally before this handoff.
- The final gate remained local: changed code was inspected against the
  milestone checklist, focused tests were run, the daemon was repackaged, and
  the full live E2E suite passed.

## Milestone 2A Outcome And Handoff

This follow-up iteration completed a narrow, shippable stone inside Milestone 2:
Git metadata route ownership and stable drop-reason publication. It did not
attempt the full protected-path reason set, unsupported special-file capture, or
opaque directory descendant expansion.

### Scope Completed

- `.git` metadata is now detected by path segment, so both root `.git/config`
  and nested `some/path/.git/config` route to `Drop`.
- Dropped Git metadata uses the stable reason code
  `git_metadata_unsupported`.
- `CaptureRouteStats` now records `drop_path_count` and
  `drop_reason_counts`.
- OCC worker handoff trace details include `drop_reason_counts`.
- Command `publish_lanes.routing` now includes `dropped_path_count` and
  `drop_reason_counts`, so response metadata and
  `command.publish_lanes_decided` expose Git metadata drops.
- Successful commands that write `.git/**` plus ordinary ignored output drop the
  Git metadata while preserving the ordinary ignored direct-LWW publish.

### Milestone 2A Checklist

| Item | Status | Notes |
| --- | --- | --- |
| Keep ordinary ignore routing snapshot-scoped | Complete | Existing snapshot route plumbing is preserved; new drop summaries are derived from the same snapshot route stats. |
| Ensure `.git/**` never routes through ordinary ignored/source publish | Complete | `.git` detection now matches any path segment, including nested command outputs. |
| Surface stable Git metadata reason in response metadata | Complete | `publish_lanes.routing.drop_reason_counts.git_metadata_unsupported` is emitted. |
| Surface stable Git metadata reason in trace | Complete | The existing `command.publish_lanes_decided` trace event carries the same `publish_lanes` object. |
| Preserve ignored output behavior when Git metadata is dropped | Complete | Live E2E proves ignored output still publishes with `published_lww`. |
| Add focused unit, contract, and live E2E coverage | Complete | LayerStack route tests, operation command/contract tests, and workspace-runtime-command E2E were updated. |

### Files Updated

- `crates/daemon/layerstack/src/commit/mod.rs`
  - Added `GIT_METADATA_UNSUPPORTED_DROP_REASON`.
  - Added route drop reason counts to `CaptureRouteStats`.
  - Added drop reason counts to OCC worker handoff event details.
  - Changed Git metadata detection from root-only `.git/**` to any `.git` path
    segment.
- `crates/daemon/layerstack/tests/unit/route.rs`
  - Added tests for stable Git drop decisions, route stats, nested `.git`
    detection, and publish handoff reason counts.
- `crates/daemon/operation/src/command/contract.rs`
  - Added `dropped_path_count` and `drop_reason_counts` under
    `publish_lanes.routing`.
- `crates/daemon/operation/src/command/finalize.rs`
  - Propagates LayerStack route drop counts into non-success and success command
    metadata.
  - Added a unit test proving successful `.git/config` writes are dropped and do
    not advance the manifest.
- `crates/daemon/operation/tests/command/service.rs`
  - Extended finalize trace assertions for routing drop summary fields.
- `crates/daemon/operation/tests/contract.rs`
  - Extended contract assertions for routing drop summary fields.
- `crates/daemon/operation/fixtures/command_finalize_conflict_response.json`
  - Updated the response fixture for the expanded `publish_lanes.routing`
    object.
- `crates/e2e-test/tests/workspace-runtime-command/command_error_and_backpressure.rs`
  - Added live E2E for a successful command that writes nested `.git/config` and
    an ignored cache file.

### Verification

Commands run:

```sh
cargo fmt
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack route_tests
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation command::
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --no-run
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo run -p e2e-test --bin e2e-runner -- --suites workspace-runtime-command --max-parallel 5 --container-weight-cap 10 --heavy-test-threads 4
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation --all-targets
git diff --check
```

Results:

- `cargo fmt` passed.
- Focused LayerStack route tests passed.
- Focused operation command tests passed.
- `e2e-test --no-run` passed.
- `xtask package` passed and rebuilt `dist/eosd-linux-amd64`.
- The first live E2E run failed the new `.git` case because route detection was
  root-only and did not catch nested `dir/.git/config`.
- After fixing `.git` detection to match any path segment, focused tests passed
  again, the daemon was repackaged, and the full live
  `workspace-runtime-command` suite passed 61/61.
- Final full `layerstack` package tests passed.
- Final full `operation --all-targets` tests passed.
- `git diff --check` passed.

Final live E2E report root:

```text
crates/e2e-test/test-reports/runs/e2e-run-1781611599567
```

### Subagent Coordination For 2A

- One worker owned the LayerStack route/drop reason plumbing and focused route
  tests.
- One worker owned operation-side response/trace metadata and contract tests.
- One explorer performed a read-only adversarial review of remaining Milestone 2
  gaps.
- The final integration gate remained local: the shared worktree was reconciled,
  path-string reconstruction in operation finalization was removed in favor of
  LayerStack route stats, the live E2E failure was root-caused, and final tests
  passed.

## Milestone 2B Outcome And Handoff

This iteration completed the next narrow Milestone 2 blocker: unsupported
special filesystem entries are no longer silently skipped during upperdir
capture. They are captured as protected drop facts, routed as `Drop`, and
reported through the same `publish_lanes.routing` summary used by Git metadata
drops.

### Scope Completed

- LayerStack capture now records unsupported special entries as
  `ProtectedPathDrop` facts with the stable reason code
  `unsupported_special_file`.
- `ProtectedPathDropReason` and internal `RouteDropReason` keep protected drop
  reason plumbing typed until the final wire/message boundary.
- Route stats and OCC worker handoff events include protected drop facts without
  requiring a storage payload.
- Command finalization passes protected drops into route stats and publish, so
  response metadata and `command.publish_lanes_decided` trace events report
  `routing.dropped_path_count` and
  `routing.drop_reason_counts.unsupported_special_file`.
- Successful commands may still publish ordinary ignored output while dropping
  unsupported special files.

### Milestone 2B Checklist

| Item | Status | Notes |
| --- | --- | --- |
| Surface unsupported special files instead of silently skipping them | Complete | Capture records representable unsupported special paths as protected drops. |
| Use stable protected drop reason | Complete | `unsupported_special_file` is emitted in route stats, OCC handoff, response metadata, and command finalize trace. |
| Preserve ordinary ignored/source publish behavior | Complete | Protected drop facts have no layer payload and do not block ordinary ignored direct-LWW output. |
| Add focused unit coverage | Complete | LayerStack capture, route stats, publish handoff, and operation finalization tests were added. |
| Add live E2E coverage | Complete | Workspace-runtime-command now covers a command-created FIFO plus ordinary ignored output. |

### Files Updated

- `crates/daemon/layerstack/src/capture.rs`
  - Added protected drop facts to `CapturedUpperdir`.
  - Records unsupported special filesystem entries as
    `ProtectedPathDropReason::UnsupportedSpecialFile`.
- `crates/daemon/layerstack/src/commit/mod.rs`
  - Added `UNSUPPORTED_SPECIAL_FILE_DROP_REASON`.
  - Replaced ad hoc drop messages with typed `RouteDropReason` in
    `PublishDecision`.
  - Added route stats and publish decisions that accept protected drop facts
    without a layer payload.
- `crates/daemon/layerstack/src/commit/worker/queue.rs`
  - Renders typed drop reasons when CAS retry exhaustion reports dropped paths.
- `crates/daemon/layerstack/src/commit/worker/transaction.rs`
  - Renders typed drop reasons into dropped `FileResult` messages.
- `crates/daemon/layerstack/src/lib.rs`
  - Re-exported protected drop capture types.
- `crates/daemon/layerstack/src/service.rs`
  - Added protected-drop-aware route stats and publish APIs.
  - Removed unused empty-drop helper wrappers once callers used the explicit
    protected-drop-aware APIs.
- `crates/daemon/workspace/src/capture.rs`
  - Carries protected drop facts through `CapturedChanges`.
- `crates/daemon/operation/src/command/finalize.rs`
  - Passes protected drop facts into route stats and publish.
  - Added an operation unit test for successful special-file-only drop
    reporting.
- `crates/daemon/layerstack/tests/unit/capture.rs`
  - Added FIFO capture coverage proving unsupported special files do not become
    layer payloads.
- `crates/daemon/layerstack/tests/unit/route.rs`
  - Added protected drop route stat and publish handoff coverage.
- `crates/daemon/layerstack/tests/unit/commit/queue.rs`
  - Updated helper construction for typed drop reasons.
- `crates/daemon/layerstack/tests/unit/commit/transaction.rs`
  - Updated helper construction for typed drop reasons.
- `crates/e2e-test/tests/workspace-runtime-command/command_error_and_backpressure.rs`
  - Added live E2E for a successful command that creates a FIFO and an ignored
    cache file.

### Verification

Commands run:

```sh
cargo fmt
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack captures_unsupported_special_files_as_protected_drops
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack route_tests
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation successful_ephemeral_command_reports_unsupported_special_file_drop
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation command::
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --no-run
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation --all-targets
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --features e2e --test workspace-runtime-command command_lifecycle::setsid_nohup_contract -- --nocapture --test-threads 1
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --features e2e --test workspace-runtime-command command_lifecycle::finite_exec_before_yield_recycles_transient_transcript_file -- --nocapture --test-threads 1
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo run -p e2e-test --bin e2e-runner -- --suites workspace-runtime-command --max-parallel 5 --container-weight-cap 10 --heavy-test-threads 4
git diff --check
```

Results:

- `cargo fmt` passed.
- Focused LayerStack capture and route tests passed.
- Focused operation command tests passed.
- `e2e-test --no-run` passed.
- Full `layerstack` package tests passed.
- Full `operation --all-targets` tests passed.
- `xtask package` passed and rebuilt `dist/eosd-linux-amd64`.
- The first full live E2E run passed the new
  `unsupported_special_file_is_dropped_with_publish_lane_reason` test but failed
  existing `setsid_nohup_contract`; the failing case passed when rerun alone.
- The second full live E2E run passed the new special-file test but failed
  existing `finite_exec_before_yield_recycles_transient_transcript_file`; the
  failing case passed when rerun alone.
- The final full live `workspace-runtime-command` suite passed 62/62.
- `git diff --check` passed.

Final live E2E report root:

```text
crates/e2e-test/test-reports/runs/e2e-run-1781614161960
```

## Milestone 2C Outcome And Handoff

This iteration advanced the highest-risk remaining Milestone 2 item: opaque
directory markers now route from the command snapshot descendants they would
hide, rather than only from the marker path.

### Scope Completed

- LayerStack publish preparation expands `LayerChange::OpaqueDir` against the
  command route snapshot before route grouping.
- Opaque markers whose hidden descendants are all ignored route through the
  ignored direct-LWW lane.
- Opaque markers whose hidden descendants are all source route through the
  gated source lane and validate those hidden source descendants before
  publishing the marker.
- Opaque markers that would hide Git/protected descendants reject the publish
  with `opaque_dir_protected_descendant`.
- Opaque markers that would hide both source and ignored descendants reject the
  publish with `opaque_dir_mixed_routes`.
- Opaque expansion has an internal descendant cap and rejects with
  `opaque_dir_expansion_limit` when exceeded.
- Operation finalization surfaces opaque rejection reasons through
  `publish_lanes.routing.drop_reason_counts`.

### Milestone 2C Checklist

| Item | Status | Notes |
| --- | --- | --- |
| Expand opaque markers against the command route snapshot | Complete | Expansion scans the snapshot manifest's visible descendants under the marker. |
| All-ignored descendant behavior | Complete | The marker routes direct and hides ignored descendants through ordinary LayerStack publish. |
| All-source descendant behavior | Complete | The marker routes gated and validates hidden source descendant base hashes before publish. |
| Mixed source/ignored descendant behavior | Complete | Publish is rejected with `opaque_dir_mixed_routes`; the marker is not published. |
| Protected descendant behavior | Complete | Git metadata descendants reject with `opaque_dir_protected_descendant`; broader protected families remain future work. |
| Expansion bound behavior | Complete | The decision path emits `opaque_dir_expansion_limit` when the scan exceeds the internal cap. |
| Response metadata coverage | Complete | Operation finalization reports `opaque_dir_mixed_routes` in `publish_lanes.routing.drop_reason_counts`. |

### Files Updated

- `crates/daemon/layerstack/src/commit/mod.rs`
  - Added stable opaque drop reason codes.
  - Expands opaque markers against snapshot-visible descendants.
  - Adds source-descendant validation hashes for gated opaque markers.
  - Marks unsafe opaque decisions as publish-rejecting route drops.
- `crates/daemon/layerstack/src/commit/worker/transaction.rs`
  - Treats publish-rejecting drop decisions as validation failures.
  - Validates source-only opaque markers against hidden descendant base hashes.
- `crates/daemon/layerstack/src/commit/worker/queue.rs`
  - Preserves publish-rejecting drop state in CAS retry exhaustion results.
- `crates/daemon/layerstack/tests/unit/route.rs`
  - Added all-ignored, all-source validation, mixed-route, protected-descendant,
    and expansion-limit opaque route coverage.
- `crates/daemon/operation/src/command/finalize.rs`
  - Added operation coverage proving `opaque_dir_mixed_routes` reaches command
    response metadata.

### Verification

Commands run:

```sh
cargo fmt
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack route_tests --no-run
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack route_tests -- --nocapture
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation successful_ephemeral_command_reports_opaque_mixed_route_drop -- --nocapture
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation command::
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation --all-targets
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --no-run
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo run -p e2e-test --bin e2e-runner -- --suites workspace-runtime-command --max-parallel 5 --container-weight-cap 10 --heavy-test-threads 4
git diff --check
```

Results:

- `cargo fmt` passed.
- Focused LayerStack route tests passed, including the new opaque route cases.
- Focused operation finalization coverage passed for
  `opaque_dir_mixed_routes` response metadata.
- Full `layerstack` package tests passed.
- Focused `operation command::` tests passed.
- Full `operation --all-targets` tests passed.
- `xtask package` passed and rebuilt `dist/eosd-linux-amd64`.
- `e2e-test --no-run` passed.
- Final live `workspace-runtime-command` suite passed 62/62.
- `git diff --check` passed.

Final live E2E report root:

```text
crates/e2e-test/test-reports/runs/e2e-run-1781615978727
```

## Milestone 2D Closeout

This iteration closed the remaining Milestone 2 route-ownership gaps that were
left after 2C: daemon/control paths, command scratch paths, invalid captured
layer paths, and non-Git protected descendants under opaque directory
expansion.

### Scope Completed

- Route classification now drops daemon/LayerStack control paths with
  `daemon_control_path`.
- Route classification now drops command scratch, transcript, final response,
  and spool-like paths with `command_scratch_path`.
- Capture now converts invalid captured layer paths that cannot become a normal
  `LayerPath` into protected drop facts with `invalid_layer_path` when feasible,
  instead of failing command finalization.
- Opaque directory expansion now treats non-Git protected descendants as
  protected and rejects the marker with `opaque_dir_protected_descendant`.
- The new reasons flow through existing LayerStack route stats, OCC worker
  handoff `drop_reason_counts`, command `publish_lanes.routing`, and the
  `command.publish_lanes_decided` trace event because they share the existing
  protected-drop route decision pipeline.
- Live `workspace-runtime-command` coverage now includes a successful command
  that writes command scratch-like output plus ordinary ignored output; the
  scratch path is dropped with `command_scratch_path` while the ignored output
  still publishes.

### Milestone 2D Checklist

| Item | Status | Notes |
| --- | --- | --- |
| Add daemon/control protected reason | Complete | `manifest.json`, `workspace.json`, LayerStack storage dirs, and `.layer-metadata` route to `Drop` with `daemon_control_path`. |
| Add command scratch protected reason | Complete | Command artifact names and reserved scratch/spool prefixes route to `Drop` with `command_scratch_path`. |
| Convert invalid captured layer paths | Complete | Invalid representability during capture emits an `invalid_layer_path` protected drop fact. Non-layer-path payload failures, such as oversized file reads, still remain capture errors. |
| Feed non-Git protected descendants into opaque validation | Complete | Snapshot-visible `.layer-metadata` descendants under an opaque marker reject with `opaque_dir_protected_descendant`. |
| Surface every new reason in metadata and trace | Complete | Existing route stats and protected-drop-aware publish paths now count the new reasons into response and trace metadata. |
| Add focused unit and operation coverage | Complete | LayerStack capture/route/publish tests and operation finalization tests cover the new reasons. |
| Add live E2E coverage | Complete | `workspace-runtime-command` includes `command_scratch_path_is_dropped_with_publish_lane_reason`. |

### Files Updated

- `crates/daemon/layerstack/src/capture.rs`
  - Added protected drop reason variants for daemon control, command scratch,
    and invalid layer paths.
  - Converts invalid layer path capture cases into protected drop facts using a
    stable representable placeholder path.
- `crates/daemon/layerstack/src/commit/mod.rs`
  - Added stable reason constants and route classification for daemon/control
    and command scratch paths.
  - Routes protected descendants to `Drop` before ordinary ignore/source
    routing, which lets opaque expansion reject them as protected descendants.
- `crates/daemon/layerstack/tests/unit/capture.rs`
  - Added invalid layer path capture coverage.
- `crates/daemon/layerstack/tests/unit/route.rs`
  - Added route, route-stat, OCC handoff, invalid-drop, daemon-control,
    command-scratch, and non-Git opaque protected descendant coverage.
- `crates/daemon/operation/src/command/finalize.rs`
  - Added response metadata coverage for `daemon_control_path`,
    `command_scratch_path`, and `invalid_layer_path`.
- `crates/e2e-test/tests/workspace-runtime-command/command_error_and_backpressure.rs`
  - Added live E2E coverage for `command_scratch_path` drop metadata and trace
    metadata while preserving ignored output publish.

### Verification

Commands run:

```sh
cargo fmt
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack route_tests
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation command::
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation --all-targets
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --no-run
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo run -p e2e-test --bin e2e-runner -- --suites workspace-runtime-command --max-parallel 5 --container-weight-cap 10 --heavy-test-threads 4
git diff --check
```

Results:

- `cargo fmt` passed.
- `layerstack route_tests` passed 28/28.
- Full `layerstack` package tests passed.
- `operation command::` passed 39 focused command tests.
- Full `operation --all-targets` passed.
- `e2e-test --no-run` passed.
- `xtask package` passed and rebuilt `dist/eosd-linux-amd64`.
- Final live `workspace-runtime-command` suite passed 63/63 with
  `max_parallel=5`, `container_weight_cap=10`, and `heavy-test-threads=4`.
- `git diff --check` passed.

Final live E2E report root:

```text
crates/e2e-test/test-reports/runs/e2e-run-1781617260016
```

## Milestone 3 Outcome And Handoff

This iteration completed Milestone 3: bounded file-backed capture for ignored
command output. It did not attempt later command Git OCC, broader lane-aware OCC
API replacement, or compaction/squash policy work.

### Scope Completed

- LayerStack now supports file-backed write payload references via
  `LayerChange::WriteFile`.
- Layer digests stream file-backed payload bytes, preserving digest semantics
  with in-memory writes.
- Layer publication copies accepted file-backed payloads into the staged layer
  and verifies source and destination sizes.
- Upperdir capture now has a metadata-first path that records regular-file
  path, kind, and size before deciding whether to read or spool payloads.
- Ephemeral command finalization routes captured metadata against the command
  snapshot before reading ignored regular-file payloads.
- Ignored file-size, aggregate-byte, file-count, and metadata-duration limits are
  enforced before ignored payload reads.
- When ignored limits are exceeded, the ignored lane is dropped with
  `dropped_due_to_limits` and a stable limit reason while eligible source output
  can still publish.
- Accepted ignored regular files are copied into command-owned spool files when
  an individual file or the accepted ignored aggregate crosses the in-memory
  threshold, then published from those file-backed references.
- Command finalization removes spool files after successful publish, ignored-lane
  drop, and publish failure.
- `publish_lanes.ignored.spooled_bytes` now reports accepted file-backed ignored
  bytes in responses and `command.publish_lanes_decided` trace events.

### Files Updated

- `crates/daemon/layerstack/src/model.rs`
  - Added `LayerChange::WriteFile` and streaming digest support.
- `crates/daemon/layerstack/src/capture.rs`
  - Added metadata-first upperdir entries and payload materialization helpers.
- `crates/daemon/layerstack/src/service.rs`
  - Added bounded command-snapshot capture options, ignored limit reason codes,
    route stats from metadata, and spool-backed materialization.
- `crates/daemon/layerstack/src/stack/layer_write.rs`
  - Added staged layer writes from file-backed payloads.
- `crates/daemon/layerstack/src/stack/mod.rs`
  - Uses fallible streaming layer digest calculation for file-backed writes.
- `crates/daemon/layerstack/src/commit/mod.rs`
  - Carries ignored spooled-byte and limit-drop metadata in route stats.
- `crates/daemon/workspace/src/capture.rs`
  - Exposes snapshot-routed bounded capture for ephemeral command finalization.
- `crates/daemon/operation/src/command/finalize.rs`
  - Uses bounded capture, reports ignored limit and spooled-byte metadata, and
    owns spool cleanup.
- `crates/daemon/operation/src/core/workspace_outcome.rs`
  - Treats file-backed writes as ordinary write path kinds.
- `crates/daemon/layerstack/tests/unit/route.rs`
  - Added file-backed write, ignored-limit, and spool-backed capture coverage.
- `crates/e2e-test/tests/workspace-runtime-command/command_error_and_backpressure.rs`
  - Added live runtime coverage for ignored-limit source publish, large ignored
    spool-backed publish, and aggregate multi-file spool-backed publish.

### Milestone 3 Checklist

| Item | Status | Notes |
| --- | --- | --- |
| Replace ignored-tree all-in-memory capture with metadata-first capture | Complete | The command path routes metadata before ignored regular-file payload reads. |
| Enforce ignored file/count/byte/duration limits before ignored payload reads | Complete | Limit reasons are `ignored_file_byte_limit`, `ignored_lane_file_limit`, `ignored_lane_byte_limit`, and `ignored_capture_duration_limit`. |
| Publish accepted large ignored payloads through command-owned spool files | Complete | Accepted ignored writes become `LayerChange::WriteFile` when an individual file or accepted ignored aggregate crosses the threshold. |
| Clean up spool files after publish, drop, and publish failure | Complete | Command finalization owns a cleanup guard over the command run-dir spool. |
| Preserve Milestone 1 and 2 behavior | Complete | Existing non-success, Git/protected, route, and opaque coverage continues to pass. |

### Verification

Commands run:

```sh
cargo fmt
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack route_tests -- --nocapture
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation command:: -- --nocapture
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation --all-targets
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --no-run
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo run -p e2e-test --bin e2e-runner -- --suites workspace-runtime-command --max-parallel 5 --container-weight-cap 10 --heavy-test-threads 4
```

Results:

- `cargo fmt` passed.
- Focused LayerStack route tests passed 31/31.
- Full `layerstack` package tests passed.
- Focused operation command tests passed 42/42.
- Full `operation --all-targets` passed.
- `e2e-test --no-run` passed.
- `xtask package` passed and rebuilt `dist/eosd-linux-amd64`.
- Final live `workspace-runtime-command` suite passed 65/65 with the new ignored
  limit and spool-backed publish cases.

Final live E2E report root:

```text
crates/e2e-test/test-reports/runs/e2e-run-1781619166861
```

### Remaining Risks And Review Focus

- Ignored capture limits are internal defaults in this milestone. Config wiring
  and validation remain part of later config closeout work.
- Source regular-file payloads still use the existing in-memory capture limit.
  This milestone only changes ignored command output capture.
- The command finalizer still uses the existing publish path for OCC conflict
  semantics. Broader lane-aware publish API replacement remains Milestone 4.
- Spool-backed payloads are copied into LayerStack layers rather than reflinked
  or hardlinked. That is intentionally conservative and can be optimized later.

## Remaining Work

The following spec areas are intentionally left for later iterations:

- Command Git OCC remains future work; until then, command-produced `.git`
  metadata is dropped with `git_metadata_unsupported`.
- Bounded spool capture with file-backed digests.
- Lane-aware publish API that replaces the existing all-capture publish path.
- Configurable response/trace byte limits for ignored-lane metadata.
- Compaction and retention semantics for ignored-lane artifacts.
- Contract expansion for later milestones once those behaviors exist.

## Handoff Risks And Review Focus

- Non-success commands now capture bounded route metadata before returning, but
  they do not publish source or ignored writes. This is deliberate diagnostic
  metadata, not a mutable-layer side effect.
- Successful commands still use the existing all-capture publish path. The new
  `publish_lanes` object reflects route classification, but the full lane-aware
  publish API is still future work.
- The 2A Git metadata handling is route/drop reporting, not command Git OCC.
  Dropped `.git` paths are visible in metadata; they are not semantically merged
  or published.
- Invalid captured layer paths now become protected drop facts when the capture
  path itself is the invalid part; unrelated payload failures such as oversized
  file reads and non-UTF-8 symlink targets remain capture errors.
- Opaque directory markers now expand against snapshot descendants and reject
  Git plus non-Git protected descendants with
  `opaque_dir_protected_descendant`.
- Live E2E validation depends on the packaged daemon under `dist/`; run
  `cargo run -p xtask -- package` before live E2E when daemon code changes.
- Reviewers should pay particular attention to whether the non-success gate is
  early enough in `finalize_ephemeral_command` and whether timeout/cancel paths
  should preserve any additional legacy discard-side trace facts.
