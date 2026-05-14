# Command Exec Structural Remediation Report

Review source: `.planning/code-reviews/sandbox/command_exec-STRUCTURAL-REVIEW.md`

Started: 2026-05-14

## Remediation Plan

### Phase 1 - Contract Surface And Safety

Issues addressed: 1, 4, 8, 13, 16, 18.

- Add a package facade so consumers import from `sandbox.command_exec`.
- Move `WorkspaceReplacementMountSpec` into the contract layer.
- Make scratch containment strict and require distinct lower/upper/work paths.
- Replace string mount modes with a typed enum.
- Remove the unused `snapshot_manifest` capture parameter.
- Remove the ignored local `.DS_Store` file from the source tree.

### Phase 2 - Executor Boundary And Typed Dependencies

Issues addressed: 5, 6, 12.

- Introduce a command-exec service boundary in `command_exec.executor`.
- Keep `shell_runner.py` as the daemon/API projection shim.
- Type workspace captures as overlay changes and OCC results as OCC result values.
- Route command-exec OCC imports through a stable `sandbox.occ` facade instead of internal changeset modules.

### Phase 3 - Strategy Boundary And Fallback Signaling

Issues addressed: 2, 3, 7, 10, 14, 19.

- Add an `ExecutionStrategy` protocol and concrete strategy modules.
- Split copy-backed path rewriting into its own module with explicit tests.
- Move the namespace helper to `entrypoints/` while keeping a compatibility import for existing callers.
- Replace stderr JSON fallback sniffing with a sidecar control file and reserved infrastructure-failure exit code.
- Replace the forever-cached private probe with an explicit strategy registry object.

### Phase 4 - Policy Injection And Helper Hardening

Issues addressed: 9, 15, 17.

- Add a `CommandExecPolicy` value object for env filtering, workspace env keys, overlay path constraints, and default env.
- Inject policy into process runners and strategies while preserving default behavior.
- Remove predictable `/tmp/namespace-entrypoint-*` fallback paths.
- Document the relationship between command-exec namespace handling and `sandbox.overlay.namespace`.

## Phase Completion Log

### Phase 1 - Contract Surface And Safety

Status: complete.

Changes:

- Added `contract/spec.py` and moved `WorkspaceReplacementMountSpec` ownership to the contract layer.
- Tightened scratch containment so lower/upper/work must be strictly below `scratch_root`, and added pairwise distinctness checks.
- Added `MountMode` enum and updated command-exec process/capture results to use it.
- Populated the package facade in `sandbox.command_exec`.
- Removed the unused `snapshot_manifest` argument from `capture_workspace_upperdir`.
- Removed the ignored local `.DS_Store` file from `backend/src/sandbox/command_exec/`.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py -q` -> 10 passed.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_command_exec -q` -> 54 passed, 6 unrelated OCC write/edit tests failed because the OCC serial merger was not started in this local run.
