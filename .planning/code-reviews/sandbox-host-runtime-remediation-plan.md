# Sandbox Host/Runtime Remediation Plan

Source review: `.planning/code-reviews/sandbox-host-runtime-HARSH-REVIEW.md`

## Scope

Remediate the structural issues in `backend/src/sandbox/host/` and
`backend/src/sandbox/runtime/` without changing the public sandbox API behavior.
Each phase is intended to be independently testable.

## Phase 1: Host Boundary And Naming

- Move the shared sync/async bridge from `sandbox.runtime.async_bridge` to
  `sandbox.async_bridge`.
- Update production and test imports so host code no longer imports
  `sandbox.runtime.*` for local bridge utilities.
- Rename host lifecycle ownership from `host/setup.py` to `host/bootstrap.py`.
- Fold one-function host lifecycle modules (`git.py`, `recovery.py`) into the
  bootstrap owner.
- Rename `host/context.py` to `host/context_preparer.py` and make the context
  protocol describe the mapping-shaped runtime context instead of `Any`.

## Phase 2: Bundle Paths And Launch Scripts

- Move bundle/daemon remote path constants out of `runtime_bundle.py` into a
  shared wire-contract module.
- Split `bundle_hash(bundle=None)` into cached `bundle_hash()` and pure
  `compute_bundle_hash(bundle)`.
- Add `clear_bundle_caches()` as the explicit test seam.
- Replace embedded thin-client Python and daemon-launch shell strings with real
  files under `sandbox/runtime/scripts/`.
- Remove the empty forwarded-daemon-env pseudo-extension point.

## Phase 3: Handler Helper And Service Contracts

- Rename exported handler helpers to public names:
  `layer_stack_root`, `required_single_path`, `services`, and
  `project_changeset`.
- Remove private helper reach-throughs from health probes.
- Replace runtime readiness `hasattr` structural checks with typed `OccBackend`
  validation.
- Bound the OCC backend cache and expose named cache-management functions.
- Remove pure trampoline handler files where registration can target the real
  callable directly.

## Phase 4: Daemon Operation Registration

- Introduce an `OpRegistry` abstraction around operation registration and
  dispatch.
- Keep a default registry only as the daemon process default and compatibility
  surface for existing tests/plugin runtime.
- Add `api.version` and `api.capabilities` ops.
- Make server dispatch accept an explicit registry so tests can construct
  isolated daemon dispatchers.

## Phase 5: Workspace Service Simplification

- Delete or narrow the pure-forwarding `LayerStackClient`.
- Replace single-method service classes with focused functions where callers do
  not need stateful polymorphism.
- Rename service names that say `Client`/`Server` when no network client/server
  boundary exists.

## Verification

- Run the narrow daemon/runtime tests after each phase:
  `uv run pytest backend/tests/unit_test/test_sandbox/test_daemon backend/tests/unit_test/test_sandbox/test_runtime_bootstrap.py -q`
- Run targeted sandbox API/plugin/OCC slices when touched:
  `uv run pytest backend/tests/unit_test/test_sandbox/test_api backend/tests/unit_test/test_sandbox/test_plugin_handler.py backend/tests/unit_test/test_sandbox/test_occ/test_mutation_gate.py -q`
- Run bundle/import-fence guards before closeout:
  `uv run pytest backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py backend/tests/unit_test/test_sandbox/test_import_fence.py -q`
