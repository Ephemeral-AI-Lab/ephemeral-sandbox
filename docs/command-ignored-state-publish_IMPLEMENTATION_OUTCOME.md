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
| Route timeout/cancel/nonzero through shared finalizer | Complete | Lifecycle discard paths now call the ephemeral command finalizer. |
| Preserve successful command behavior | Complete | Successful commands still use the existing capture/publish path, with lane metadata derived from the leased command snapshot. |
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
    through the ephemeral command finalizer.
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

- `crates/daemon/layerstack/src/stack/mod.rs`
  - Added an opt-in one-shot `.layer-metadata/fail-next-publish` publish
    failpoint so live E2E can inject a real LayerStack publish failure after
    command capture.
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

## Milestone 4 Outcome And Handoff

This iteration completed Milestone 4: lane-aware OCC publish semantics for
ephemeral command finalization. It did not start command Git OCC, config/YAML
limit wiring, compaction, or retention policy work.

### Scope Completed

- Successful command finalization now publishes captured output through a
  lane-aware LayerStack API instead of the earlier all-capture publish call.
- Source-lane changes remain gated by snapshot-manifest OCC validation.
- Ignored-lane changes publish as direct LWW only after the source lane is
  known to be eligible: committed, accepted/no-op, or empty.
- Source OCC conflict or source publish failure drops ignored output from the
  same command with
  `ignored.publish_status=dropped_due_to_source_conflict` and
  `ignored.drop_reason=source_not_published`.
- Route rejection failures with source output are reported as publish failures
  rather than being mislabeled as source-conflict ignored drops.
- Accepted source and ignored changes are submitted atomically, so mixed-lane
  success advances the manifest once and appears as one coherent publish result.
- Ignored-only commands still use direct-LWW semantics, and later ignored
  writers win as expected.
- Milestone 3 spool-backed ignored payloads are preserved, including aggregate
  spool threshold behavior and `publish_lanes.ignored.spooled_bytes`.
- Existing protected routing drop reasons and `publish_lanes.routing` metadata
  continue to flow through responses and `command.publish_lanes_decided`.

### Milestone 4 Checklist

| Item | Status | Notes |
| --- | --- | --- |
| Replace command finalizer publish call with lane-aware API | Complete | `finalize_ephemeral_command_with_capture_options` calls `publish_command_capture_lane_aware`. |
| Keep source changes behind OCC | Complete | Source decisions still carry snapshot base hashes into the gated worker path. |
| Publish ignored LWW only after source eligibility | Complete | The atomic worker drops direct accepted paths when gated validation fails. |
| Drop ignored output on source conflict or source publish failure | Complete | Operation and live E2E coverage verify conflict and injected publish-failure drops, including spooled ignored output. |
| Preserve atomic mixed source+ignored success | Complete | LayerStack, operation, and live E2E coverage verify one manifest advance for mixed-lane success. |
| Preserve ignored-only LWW overwrite semantics | Complete | Unit and live E2E coverage verify two ignored-only writers where the later accepted writer wins. |
| Preserve spool-backed ignored payload support | Complete | Operation and live E2E coverage verify spooled ignored payloads still publish and are dropped atomically on publish failure. |

### Files Updated

- `crates/daemon/layerstack/src/service.rs`
  - Added `publish_command_capture_lane_aware`, using the command snapshot
    manifest and protected-drop-aware route decisions.
- `crates/daemon/layerstack/src/commit/mod.rs`
  - Added the command lane-aware writer entry point, preserving atomic worker
    submission for mixed source/direct decisions.
- `crates/daemon/layerstack/tests/unit/route.rs`
  - Added unit coverage for source conflict dropping ignored output,
    ignored-only LWW overwrite behavior, mixed source+ignored one-manifest
    publish, and nested snapshot ignored spool capture.
- `crates/daemon/operation/src/command/finalize.rs`
  - Switched command finalization to the lane-aware publish API.
  - Fixed ignored-lane metadata precedence so route rejection failures are not
    reported as source-conflict drops.
  - Added operation coverage for source conflict metadata, ignored drop
    visibility, mixed success metadata, route rejection metadata, real
    publish-failure cleanup, and nested spooled ignored output.
- `crates/e2e-test/tests/workspace-runtime-command/command_error_and_backpressure.rs`
  - Added live coverage for source conflict plus ignored output, mixed
    source+ignored success, dedicated ignored-only two-writer LWW overwrite,
    injected source publish failure, and spool-backed ignored success.
  - Uses trace export for the background finalize record when a stdin-gated
    source conflict or slow spool command finalizes outside the original
    response trace.

### Verification

Commands run:

```sh
cargo fmt
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation command::
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --no-run
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation --all-targets
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo run -p e2e-test --bin e2e-runner -- --suites workspace-runtime-command --max-parallel 5 --container-weight-cap 10 --heavy-test-threads 4
git diff --check
```

Results:

- `cargo fmt` passed.
- Full `layerstack` package tests passed.
- `operation command::` passed 48/48.
- Full `operation --all-targets` passed.
- `e2e-test --no-run` passed.
- `xtask package` passed and rebuilt `dist/eosd-linux-amd64` with SHA
  `4b3c85ae6cc6342e90ae3a6aa603db6e9c311803c455b97dbf3fb635eb1c1067`.
- An initial full live run passed the new injected publish-failure and
  ignored-only LWW tests but exposed a slow-spool foreground timing assumption;
  the affected spool tests now poll to terminal and prove the exported finalize
  trace when needed.
- Final live `workspace-runtime-command` suite passed 70/70 with the
  feature-enabled E2E test binary, `max_parallel=5`,
  `container_weight_cap=10`, and `heavy-test-threads=4`.
- `git diff --check` passed.

Final live E2E report root:

```text
crates/e2e-test/test-reports/runs/e2e-run-1781625053708
```

## Milestone 5 Outcome And Closeout

This iteration completed Milestone 5: ignored-lane config wiring, validation,
compaction coverage, and final regression closeout.

### Scope Completed

- `daemon.commands.ignored_capture` is now part of the typed daemon config
  schema and production YAML.
- Ignored capture config validates file count, aggregate bytes, per-file bytes,
  spool threshold, and metadata capture duration before daemon startup accepts
  the config.
- Daemon startup converts typed ignored capture config into LayerStack
  `BoundedCaptureOptions` and stores those options in `CommandOps`.
- Ephemeral command lifecycle finalization now passes configured capture limits
  into `finalize_ephemeral_command_with_capture_options`.
- The workspace-runtime-command test overlay uses small ignored-lane limits so
  live E2E exercises configured limit/drop and spool behavior rather than
  relying on production defaults.
- Auto-squash regression coverage now verifies head-visible ignored-style cache
  writes survive compaction with the newest values.
- E2E README/index artifacts were regenerated for the expanded publish-lanes
  coverage.
- `discarded_response` now always includes `publish_lanes`, using route manifest
  version `0` when no manifest version is available.
- Successful-command capture and publish finalization errors now return
  finalized responses with `publish_lanes` instead of falling back to generic
  command errors.
- The layerstack publish-failure marker is gated behind the
  `EOS_LAYERSTACK_ENABLE_TEST_FAILPOINTS` opt-in and is enabled only by the E2E
  daemon spec.

### Milestone 5 Checklist

| Item | Status | Notes |
| --- | --- | --- |
| Add ignored-lane limits to daemon config and production YAML | Complete | `config/prd.yml` declares the production `ignored_capture` block. |
| Validate ignored file/count/byte/duration/spool limits | Complete | Config tests cover zero values, file greater than aggregate bytes, and spool threshold greater than or equal to aggregate bytes. |
| Wire config into command ignored capture | Complete | Runtime services map daemon config into `BoundedCaptureOptions`; `CommandOps` carries those options into finalization. |
| Preserve Milestone 4 lane semantics | Complete | Operation, LayerStack, host, and E2E compile regression checks pass; live E2E was not rerun in this post-review closeout. |
| Add compaction/squash coverage | Complete | Auto-squash keeps latest ignored-style cache values after squashing older layers. |
| Align docs/contracts/readmes | Complete | Workspace-runtime-command README JSON/HTML/index artifacts were regenerated. |
| Assert response metadata and exported trace behavior for success, drop, limit, protected-path, and conflict cases | Complete | The live suite includes published response metadata and exported finalize trace cases added across Milestones 1-5. |
| Ensure finalize paths include `publish_lanes` | Complete | Added coverage for discarded responses without a manifest version and successful-command capture finalization failures. |

### Files Updated

- `config/prd.yml`
  - Added production `daemon.commands.ignored_capture` defaults.
- `crates/e2e-test/tests/workspace-runtime-command/config/default.test.yml`
  - Added suite-specific smaller ignored capture limits to exercise configured
    drops and spool paths in live E2E.
- `crates/daemon/config/src/configs/daemon.rs`
  - Added daemon-local `CommandConfig` and `IgnoredCaptureConfig`.
  - Added ignored capture validation.
- `crates/daemon/config/tests/unit/configs/daemon.rs`
  - Added invalid-config coverage for ignored capture limits.
- `crates/daemon/core/src/runtime/services.rs`
  - Added `capture_options_from_schema`.
  - Added runtime-service construction with explicit command capture options.
  - Removed the default-only `RuntimeServices::with_commit_options` wrapper;
    the remaining non-default constructor requires explicit capture options.
- `crates/daemon/core/src/transport/server.rs`
  - Wires configured capture options into daemon runtime services.
- `crates/daemon/operation/src/command/service.rs`
  - Stores capture options in `CommandOps`.
  - Removed the default-only `CommandOps::with_commit_options` wrapper; callers
    now either use `CommandOps::new` defaults or the explicit capture-options
    constructor.
- `crates/daemon/operation/src/command/service/lifecycle.rs`
  - Calls the configured-capture finalizer for ephemeral commands.
  - Converts any remaining finalization error into a finalized response with
    `publish_lanes`.
- `crates/daemon/operation/src/command/finalize.rs`
  - Keeps the configurable finalizer entry point and adds coverage for
    configured ignored limit behavior, manifest-less discarded responses, and
    successful-command capture finalization failures.
- `crates/daemon/layerstack/src/stack/mod.rs`
  - Gates the publish-failure marker behind explicit test failpoint opt-in.
- `crates/e2e-test/src/container.rs`
  - Enables the layerstack test failpoint only for E2E daemon bring-up.
- `crates/host/src/container.rs`
  - Adds an explicit daemon test-failpoint switch so E2E-only settings do not
    apply to normal daemon specs.
- `crates/daemon/workspace/src/capture.rs`
  - Exposes snapshot capture with explicit bounded-capture options.
- `crates/daemon/layerstack/tests/unit/commit/transaction.rs`
  - Adds auto-squash coverage for ignored-style cache layers and verifies the
    publish-failure marker is inert without explicit test opt-in.
- `crates/e2e-test/tests/workspace-runtime-command/command_error_and_backpressure.rs`
  - Adjusts ignored limit/spool payload sizes to the suite-configured limits.
- `crates/e2e-test/tests/workspace-runtime-command/readme.md`
  - Documents the publish-lanes coverage group.

### Verification

Commands run:

```sh
cargo fmt --check
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p config
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation command::
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation --all-targets
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p host
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --no-run
git diff --check
```

Results:

- `cargo fmt --check` passed.
- `config` package tests passed 20/20.
- Full `layerstack` package tests passed.
- `operation command::` passed 53 focused command tests.
- Full `operation --all-targets` passed.
- Full `host` package tests passed 53/53.
- `e2e-test --no-run` passed.
- `git diff --check` passed.
- Live E2E and `xtask package` were not rerun in this post-review closeout.

### Remaining Risks And Review Focus

- Command Git OCC has since advanced to the sandbox floor described below.
  Clean commit acceptance, frame-shift classification, quiet-stack behavior,
  and broader Git maintenance remain future work.
- Source regular-file payloads still use the existing in-memory capture limit.
  The spool path remains scoped to accepted ignored command output.
- Spool-backed payloads are copied into LayerStack layers rather than reflinked
  or hardlinked. That is intentionally conservative and can be optimized later.
- Live E2E validation depends on the packaged daemon under `dist/`; run
  `cargo run -p xtask -- package` before live E2E when daemon code changes.
- Stale `eos-e2e-*` Docker containers can trip the configured container cap
  after interrupted live runs; remove only stale E2E containers before reruns.
- The live workspace-runtime-command suite validates exported finalize traces by
  decoding and ACKing trace batches; it does not assert audit-store ingestion of
  those trace records.

## Command Git OCC Floor Outcome And Handoff

This iteration implemented the first sandbox-side Command Git OCC floor after
ignored-state Milestone 5. It did not add agent hooks, daemon admission policy,
command-text parsing, frame-shift quiet-stack behavior, ref deletion support, or
Git GC/pruning support.

### Scope Completed

- Command-produced `.git/**` paths now use command-specific Git metadata
  classification instead of the legacy blanket `git_metadata_unsupported` drop.
- Generic publish/file/edit/plugin-style paths keep the previous unsupported
  `.git` drop behavior; the new classifier is used by
  `sandbox.command.exec` finalization only.
- Accepted Git metadata currently publishes only through gated OCC, never the
  ignored/direct LWW lane.
- Rejected Git metadata marks the route decision as publish-rejecting, so the
  existing atomic worker drops the whole command result: source lane, ignored
  lane, and Git metadata all remain unpublished.
- `.gitignore` still cannot route `.git/**` to ordinary ignored/direct output.
- No-op and stat-cache-only index refreshes are normalized away with
  `git_index_stat_refresh`, so they do not conflict and do not publish durable
  staged state.
- Semantic `.git/index` changes reject with `git_index_staged_state`.
- `.git/**/*.lock`, incomplete operation markers, hook writes, Git metadata
  deletions, `.git` root opaque replacement, ref writes, object rewrites, and
  reflog rewrites reject with stable closed codes.
- New Git object writes and append-only reflog writes are accepted through
  gated OCC.
- Response `publish_lanes.routing.drop_reason_counts` and command finalize
  traces expose the Git reason codes.

### Files Updated

- `docs/command-git-occ-policy_SPEC.md`
  - Added the current sandbox floor code set, including the non-rejecting
    `git_index_stat_refresh` normalization code.
- `crates/daemon/layerstack/src/commit/mod.rs`
  - Added command-specific Git metadata policy mode and closed reason codes.
  - Added explicit command Git decisions for index refresh/staged state, locks,
    incomplete operation markers, hooks, destructive deletes, ref writes, object
    rewrites, and reflog append/rewrite checks.
  - Preserved legacy generic `.git` drop behavior outside command finalization.
- `crates/daemon/layerstack/src/service.rs`
  - Uses command Git decisions for command capture and command lane-aware
    publish.
  - Reads only Git metadata payloads needed for routing during the metadata
    pass; ignored payload capture remains metadata-first and bounded.
- `crates/daemon/layerstack/tests/unit/route.rs`
  - Added focused route tests for `.git` classification, rejection codes,
    append-only reflog acceptance, lock/control/hook/delete rejection,
    no-op index refresh, staged index rejection, object rewrite rejection, and
    `.gitignore` bypass prevention.
- `crates/daemon/operation/src/command/finalize.rs`
  - Updated operation coverage for rejected Git metadata and all-or-nothing
    source/ignored lane drops.
- `crates/e2e-test/tests/workspace-runtime-command/command_error_and_backpressure.rs`
  - Updated `.gitignore`/`.git` routing live coverage to expect whole-publish
    rejection.
  - Added live real-Git command coverage for `git add` without commit, leftover
    locks, hook writes, and deleting `HEAD` plus object metadata.

### Verification

Commands run:

```sh
cargo fmt --check
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack route_tests
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p layerstack
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation command::
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p operation --all-targets
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p daemon --no-run
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo test -p e2e-test --no-run
cargo run -p xtask -- package
CARGO_TARGET_DIR=/tmp/ephemeral-os-target cargo run -p e2e-test --bin e2e-runner -- --suites workspace-runtime-command --max-parallel 5 --container-weight-cap 10 --heavy-test-threads 4
git diff --check
```

Results:

- `cargo fmt --check` passed.
- Focused LayerStack route tests passed 45/45.
- Full `layerstack` package tests passed.
- Focused `operation command::` tests passed 54/54.
- Full `operation --all-targets` passed.
- `daemon --no-run` passed.
- `e2e-test --no-run` passed.
- `xtask package` passed and rebuilt `dist/eosd-linux-amd64` with SHA
  `5b1e493a90338706e2ad433b26c5ee18d331f9ab44f9e14f7b5701542fa58e82`.
- Live `workspace-runtime-command` passed 74/74 with the new Git floor cases.
- `git diff --check` passed.

Final live E2E report root:

```text
crates/e2e-test/test-reports/runs/e2e-run-1781631871630
```

### Remaining Git OCC Work

- Clean `git add && git commit` acceptance still needs final index-to-HEAD tree
  validation and repository health checks.
- Ref fast-forward support, HEAD relabel support, frame-shift classification,
  and quiet-stack merge rules remain future work.
- Reflog union-merge, clean index regeneration, ref deletion, and Git GC/pruning
  remain intentionally deferred.
- The current index semantic parser covers v2/v3 SHA-1 indexes for stat-refresh
  normalization; broader hash algorithms and index formats should be added with
  explicit tests before accepting more workflows.

## Remaining Work

The following spec areas are intentionally left for later iterations:

- Later Command Git OCC milestones remain: clean commit acceptance, ref
  fast-forward/frame-shift classification, quiet-stack merging, repository
  health validation, and deferred Git maintenance policies.
- Audit-store ingestion assertions for publish-lane trace records.
- Configurable response/trace byte limits for ignored-lane metadata.
- Contract expansion for later milestones once those behaviors exist.

## Handoff Risks And Review Focus

- Non-success commands now capture bounded route metadata before returning, but
  they do not publish source or ignored writes. This is deliberate diagnostic
  metadata, not a mutable-layer side effect.
- Successful commands now use the lane-aware publish API. Source output is OCC
  gated, ignored output is direct LWW only after source eligibility is known,
  and mixed success is submitted atomically.
- The 2A Git metadata handling is route/drop reporting, not command Git OCC.
  This has now been superseded for `sandbox.command.exec` by the command Git OCC
  floor above; generic non-command publish paths still keep the unsupported
  `.git` drop behavior.
- Invalid captured layer paths now become protected drop facts when the capture
  path itself is the invalid part; unrelated payload failures such as oversized
  file reads and non-UTF-8 symlink targets remain capture errors.
- Opaque directory markers now expand against snapshot descendants and reject
  Git plus non-Git protected descendants with
  `opaque_dir_protected_descendant`.
- Ignored capture limits are now daemon-configured and validated, with E2E
  coverage using smaller suite limits to prove the wiring.
- Auto-squash now has regression coverage for head-visible ignored-style cache
  layers.
- Live E2E validation depends on the packaged daemon under `dist/`; run
  `cargo run -p xtask -- package` before live E2E when daemon code changes.
- Reviewers should pay particular attention to whether the non-success gate is
  early enough in `finalize_ephemeral_command_with_capture_options` and whether
  timeout/cancel paths should preserve any additional discard-side trace facts.
