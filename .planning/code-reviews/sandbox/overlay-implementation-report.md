# Overlay Architecture Implementation Report

Source plan: `.planning/code-reviews/sandbox/overlay-remediation-plan.md`

## Phase 1 - Flat Package Shape

Status: complete

Changed:

- Replaced nested source modules under `overlay/capture/`, `overlay/namespace/`, and `overlay/runner/` with flat modules under `backend/src/sandbox/overlay/`.
- Added the public `sandbox.overlay` facade for request/result/change/runner/invoker/workspace helpers.
- Updated production callers, native probes, bundle assertions, and docs to stop documenting the old nested import paths.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_overlay -q` -> 19 passed.

## Phase 2 - Entry Point and Invoker Contract

Status: complete

Changed:

- Moved core worker orchestration into `overlay/worker.py`; kept `overlay/cli.py` as a thin compatibility shim.
- Moved request serialization onto `OverlayShellRequest.to_dict()` / `OverlayShellRequest.from_dict()`.
- Moved result serialization and output-ref helpers into `overlay/result.py`.
- Added public `OverlayInvoker` plus `OverlayRuntimeInvoker`; runner default construction now goes through `overlay/factory.py`.
- Made `OverlayCapture.timings` immutable after construction and added a focused regression assertion.

Verification:

- Covered by the same overlay unit slice: `uv run pytest backend/tests/unit_test/test_sandbox/test_overlay -q` -> 19 passed.

## Phase 3 - Polish, Docs, and Guardrails

Status: complete

Changed:

- Added capture-code sections for copy-backed population, upperdir walking, and overlay marker decoding.
- Removed review-code comment labels from overlay source and nearby focused tests.
- Documented the copy-backed workspace behavior in `overlay/mounts.py` and `docs/wiki/sandbox-subsystem.md`.
- Updated runtime-bundle and dependency-boundary tests to require the flat package and reject the old nested modules.
- Updated OCC and command-exec consumers to use the public `sandbox.overlay` facade for overlay change values.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_overlay -q` -> 19 passed.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_changeset.py backend/tests/unit_test/test_sandbox/test_daemon/test_overlay_capture.py backend/tests/unit_test/test_sandbox/test_occ/test_occ_dependency_boundaries.py -q` -> 20 passed.
- `uv run ruff check backend/src/sandbox/overlay backend/src/sandbox/runtime/daemon/handler/overlay.py backend/src/sandbox/command_exec/workspace/capture.py backend/src/sandbox/occ/capture/overlay.py backend/tests/unit_test/test_sandbox/test_overlay backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py backend/tests/unit_test/test_sandbox/test_occ/test_occ_dependency_boundaries.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_changeset.py backend/tests/unit_test/test_sandbox/test_daemon/test_overlay_capture.py` -> passed.

## Verification

Status: complete
