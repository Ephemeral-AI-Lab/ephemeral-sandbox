# Phase 02 - Overlay Snapshot Runtime Implementation Report

Companion to
[`phase-02-overlay-snapshot-runtime.md`](./phase-02-overlay-snapshot-runtime.md).
This report records the Phase 02 overlay runtime delivered in the current
checkout, the tests added around it, and the cutover work intentionally left for
later phases.

---

## 1. Verdict

**Phase 02 is implemented and verified as a new layer-stack-backed overlay
runtime path.**

The new runtime leases a frozen `Manifest`, materializes that snapshot into a
per-call workspace view, runs exactly one argv command, captures the runtime
upper changes, writes a `RuntimeResultEnvelope`, and releases the snapshot
lease in `finally`.

The implementation is deliberately policy-blind:

- no OCC conflict validation
- no direct layer publish
- no gitignore routing
- no git `check-ignore` calls
- no primary NDJSON capture contract

The older live-root `OverlayCaptureEngine` remains present for existing callers
until the integration/cutover phase removes or reroutes it.

---

## 2. File Inventory

### Runtime Package

| File | Purpose |
| --- | --- |
| `backend/src/sandbox/overlay/types.py` | Adds `OverlayShellRequest` plus JSON-safe request helpers while preserving the existing overlay capture types |
| `backend/src/sandbox/overlay/client.py` | Adds `shell_snapshot` and `run_snapshot` client methods backed by `SnapshotOverlayRunner`; existing runtime-server methods stay compatible |
| `backend/src/sandbox/overlay/handlers/run.py` | Adds an optional `layer_stack_root` handler path for Phase 02 snapshot overlay requests |
| `backend/src/sandbox/overlay/capture/changes.py` | Defines Phase 02 `UpperChange(path, kind, content_path, final_hash)` values and content hashing |
| `backend/src/sandbox/overlay/capture/upperdir.py` | Captures writes, deletes, symlinks, and opaque dirs from an upperdir; also supports copy-backed local diff capture |
| `backend/src/sandbox/overlay/namespace/mounts.py` | Materializes a leased manifest into a per-call lowerdir and prepares upper/work/merged directories |
| `backend/src/sandbox/overlay/namespace/command.py` | Runs one argv command inside the mounted snapshot view with stdout/stderr refs |
| `backend/src/sandbox/overlay/runner/snapshot_overlay_runner.py` | Acquires the layer-stack snapshot lease, invokes runtime execution, and releases the lease in `finally` |
| `backend/src/sandbox/overlay/runner/runtime_invoker.py` | Invokes the runtime-local overlay shell command and returns a typed envelope |
| `backend/src/sandbox/overlay/runner/runtime_bundle.py` | Builds the Phase 02 runtime bundle without NDJSON capture modules |
| `backend/src/sandbox/runtime/overlay_shell/cli.py` | Runtime entrypoint for one leased snapshot shell request |
| `backend/src/sandbox/runtime/overlay_shell/result_envelope.py` | Defines and serializes `RuntimeResultEnvelope` |

### Tests

This checkout uses `backend/tests/test_sandbox/`, so the Phase 02 tests were
added under `backend/tests/test_sandbox/test_overlay/`.

| Test file | Coverage |
| --- | --- |
| `test_snapshot_overlay_runner.py` | Leased snapshot execution, stdout/stderr refs, upper write capture, no layer publish, lease release on success and failure |
| `test_upperdir_capture.py` | Write/delete/symlink/opaque-dir capture and copy-backed local diff capture |
| `test_overlay_dependency_boundaries.py` | No OCC/gitignore/publish imports in Phase 02 modules, no forbidden overlay modules, runtime bundle contents |
| `test_package_structure.py` | Updates overlay package layout expectations for `capture`, `namespace`, and `runner` |

---

## 3. Behavior Delivered

### Request And Lease Flow

`OverlayShellRequest` is the typed request value for one shell call. The
`SnapshotOverlayRunner` accepts that request, calls
`LayerStackManager.acquire_snapshot_lease(request_id)`, invokes the runtime with
the leased manifest, and releases the lease in a `finally` block.

The release behavior is covered by a failing-runtime regression test, so a
runtime exception cannot leave layer refs pinned.

### Snapshot Workspace Runtime

`namespace.mounts.mount_snapshot` materializes the leased manifest into a
runtime-local lowerdir and prepares per-call `upper`, `work`, and `merged`
directories. The current implementation uses a copy-backed merged view so the
unit path works without requiring kernel overlay privileges. The module boundary
matches the future kernel overlay mount point: callers receive a
`MountedSnapshot` and do not know whether the merged view was copy-backed or
kernel-mounted.

`namespace.command.run_user_command` runs one argv command in the mounted
workspace, writes stdout/stderr to files, and returns refs to those files.

### Upperdir Capture

`capture.upperdir.capture_changes` emits raw `UpperChange` values only:

- `write` carries a content path and SHA-256 final hash
- `delete` carries no content
- `symlink` carries a content path to the symlink and hashes the link target
- `opaque_dir` carries no content

The capture layer does not carry base bytes, gitignore decisions, direct-merge
state, OCC status, or publish state. Those decisions belong to later OCC and
pipeline phases.

### Runtime Envelope

`RuntimeResultEnvelope` contains:

```text
exit_code
stdout_ref
stderr_ref
snapshot_version
upper_changes
```

The envelope is written to `result.json` in the runtime run directory and is
returned to the caller as a typed dataclass. It remains runtime-local and has no
OCC projection.

---

## 4. Exit Criteria Mapping

| Phase 02 exit condition | Implementation evidence |
| --- | --- |
| Shell request can run against a leased snapshot | `SnapshotOverlayRunner.shell`; `test_snapshot_runner_executes_against_leased_manifest_without_publish` |
| Frozen manifest is used as lower view | `mount_snapshot(manifest=lease.manifest, ...)` materializes the leased manifest |
| Command runs inside mounted merged view | `run_user_command(..., workspace_root=mounted.workspace_root, ...)` |
| Upperdir writes/deletes/symlinks/opaque dirs are captured | `capture_changes`; `test_upperdir_capture_emits_raw_runtime_changes` |
| Runtime result envelope is returned | `RuntimeResultEnvelope`; `runtime/overlay_shell/cli.py` writes `result.json` |
| Lease is released in finally | `SnapshotOverlayRunner.shell`; `test_snapshot_runner_releases_lease_when_runtime_fails` |
| Runtime handler path is exercised without importing private handler modules directly | `dispatch_envelope({"op": "overlay.run", ...})`; `test_overlay_run_handler_supports_layer_stack_snapshot_requests` |
| Overlay does not import OCC or git policy | `test_phase02_overlay_modules_do_not_import_occ_or_git_policy` |

---

## 5. Verification

Focused Phase 02 plus layer-stack tests:

```bash
uv run pytest backend/tests/test_sandbox/test_overlay/test_snapshot_overlay_runner.py backend/tests/test_sandbox/test_overlay/test_upperdir_capture.py backend/tests/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py backend/tests/test_sandbox/test_layer_stack -q
```

Result:

```text
22 passed in 0.24s
```

Focused Phase 02 lint:

```bash
uv run ruff check backend/src/sandbox/overlay backend/src/sandbox/runtime/overlay_shell backend/tests/test_sandbox/test_overlay/test_snapshot_overlay_runner.py backend/tests/test_sandbox/test_overlay/test_upperdir_capture.py backend/tests/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py
```

Result:

```text
All checks passed!
```

Existing overlay package tests:

```bash
uv run pytest backend/tests/test_sandbox/test_overlay -q
```

Result:

```text
50 passed in 1.74s
```

API contract, shell pipeline, and layer-stack compatibility:

```bash
uv run pytest backend/tests/test_sandbox/test_api_contract.py backend/tests/test_sandbox/test_runtime/test_shell_pipeline.py backend/tests/test_sandbox/test_layer_stack -q
```

Result:

```text
30 passed in 0.28s
```

Broad lint over touched overlay/runtime test surface:

```bash
uv run ruff check backend/src/sandbox/overlay backend/src/sandbox/runtime/overlay_shell backend/tests/test_sandbox/test_overlay
```

Result:

```text
All checks passed!
```

---

## 6. Deferred Work

| Deferred item | Reason |
| --- | --- |
| OCC changeset routing from `UpperChange` | Phase 03 owns capture-to-changeset conversion and routing |
| Final active-manifest validation and layer publish | Phase 04 owns atomic OCC commit transactions |
| Squash, lease budget, and GC | Phase 05 owns layer-stack maintenance |
| Public API/runtime cutover from old live-root overlay path | Phase 06 owns integration and removal of obsolete production paths |
| Kernel overlay mount replacement for the copy-backed local runtime | The Phase 02 module boundary supports it, but the portable implementation keeps unit verification independent of host mount privileges |
