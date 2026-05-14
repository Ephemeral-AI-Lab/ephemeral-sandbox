# Overlay Architecture Remediation Plan

Source review: `.planning/code-reviews/sandbox/overlay-architecture-review.md`

## Goals

- Make `sandbox.overlay` read as one small runtime package, not three fake domains.
- Keep overlay policy-blind: no OCC, gitignore, or publish policy in this package.
- Preserve current behavior while shortening imports and making extension seams explicit.
- Update tests and docs so the new shape is the only documented shape.

## Phase 1 - Flat Package Shape

Addresses: H-01, H-03, H-05, M-01, M-07.

- Replace the nested `capture/`, `namespace/`, and `runner/` source modules with flat modules under `sandbox/overlay/`.
- Use boundary-revealing module names:
  - `change.py` for `OverlayPathChange` and hashing.
  - `capture.py` for upperdir/copy-backed diff capture.
  - `result.py` for `OverlayCapture` and result I/O helpers.
  - `mounts.py` for the copy-backed mounted snapshot workspace.
  - `command.py` for user-command execution in that workspace.
  - `request.py` for `OverlayShellRequest`.
  - `runner.py` for the lease/invoke/release runner.
  - `invoker.py` for the default runtime invoker.
  - `worker.py` for the worker entrypoint.
- Fill `sandbox.overlay.__init__` with the public surface so callers can use short imports.

## Phase 2 - Entry Point and Invoker Contract

Addresses: H-02, H-04, M-03, M-04, M-05, M-06, L-01, L-02, L-06, L-07.

- Move core worker orchestration from `cli.py` to `worker.py`.
- Keep any CLI compatibility as a thin shim only.
- Move request serialization onto `OverlayShellRequest.to_dict()` / `from_dict()`.
- Move result JSON/stdout/stderr helpers into `result.py`.
- Promote the invoker seam to public `OverlayInvoker`; remove private duplicate protocols.
- Move default invoker construction out of `runner.py` and into `factory.py`.
- Freeze `OverlayCapture.timings` as an immutable mapping.
- Align payload types on `Mapping[str, Any]`.
- Keep runtime request-id sanitization as documented defense in depth.

## Phase 3 - Polish, Docs, and Guardrails

Addresses: M-02, L-03, L-04, L-05.

- Use a clear naming rule: public cross-module overlay data types carry the `Overlay` prefix; private/local helpers do not.
- Add section boundaries in capture code for copy-backed population, upperdir walking, and overlay marker decoding.
- Replace review-code comments such as `WR-02` with durable reason comments.
- Document why copy-backed tree copying is not a plain one-shot `copytree`.
- Update boundary tests, runtime-bundle expectations, docs, and imports to the flat package.

## Verification

- `uv run pytest backend/tests/unit_test/test_sandbox/test_overlay -q`
- `uv run pytest backend/tests/unit_test/test_sandbox/test_daemon/test_overlay_capture.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_changeset.py backend/tests/unit_test/test_sandbox/test_occ/test_occ_dependency_boundaries.py -q`
- If `uv` hits local cache permission issues, retry with `.venv/bin/pytest` for the same slices.
