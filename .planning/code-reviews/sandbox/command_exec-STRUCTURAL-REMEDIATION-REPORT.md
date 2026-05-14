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

### Phase 2 - Executor Boundary And Typed Dependencies

Status: complete.

Changes:

- Added `command_exec/executor.py` as the owner of the snapshot lease, command run, capture, OCC apply, lease release, and transient lowerdir cleanup pipeline.
- Reduced `runtime/daemon/service/shell_runner.py` to service lookup plus API payload projection, with a thin internal delegate for existing callers.
- Added `CommandExecutor` to the command-exec contract and exported `execute_command` from the package facade.
- Routed command-exec OCC type imports through the `sandbox.occ` facade.
- Typed `WorkspaceCapture.changes` as `OverlayPathChange` values and `CommandExecResult.occ_result` as `ChangesetResult`.
- Added mount-mode coercion in result dataclasses so older tests/fakes passing string values normalize to `MountMode`.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_to_occ_client.py::test_shell_capture_goes_through_occ_client_before_lease_release -q` -> 11 passed.
- Facade import smoke: `CommandExecRequest`, `execute_command`, `WorkspaceReplacementMountSpec`, and `shell_runner.execute_shell_api` import successfully.

### Phase 3 - Strategy Boundary And Fallback Signaling

Status: complete.

Changes:

- Added `command_exec/strategies/` with `ExecutionStrategy`, `CopyBackedStrategy`, `PrivateNamespaceStrategy`, and `StrategyRegistry`.
- Reduced `workspace/mount.py` to ordered strategy dispatch plus compatibility helpers.
- Split copy-backed workspace path rewriting into `workspace/path_rewrite.py`.
- Moved the private namespace subprocess module to `entrypoints/namespace_helper.py`; kept `workspace/namespace_entrypoint.py` as a compatibility import module.
- Replaced stderr JSON fallback detection with `namespace-control.json` and reserved exit code `125` for recoverable namespace infrastructure failures.
- Removed the forever `lru_cache` namespace capability probe; strategy availability is now bootstrapped explicitly per registry construction.
- Updated runtime bundle required-path coverage for the new command-exec structure.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py -q` -> 11 passed.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_import_fence.py::test_command_exec_imports_only_client_protocol_boundaries -q` -> 1 passed.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py::test_bundle_layout_includes_required_paths -q` -> 1 passed.

### Phase 4 - Policy Injection And Helper Hardening

Status: complete.

Changes:

- Added `command_exec/policy.py` with `CommandExecPolicy` and `DEFAULT_COMMAND_EXEC_POLICY`.
- Moved restricted env keys, workspace env keys, overlay path characters, and `GIT_OPTIONAL_LOCKS=0` into the injectable policy object.
- Threaded policy through copy-backed execution, private namespace payloads, subprocess env creation, and namespace mount validation.
- Removed predictable `/tmp/namespace-entrypoint-*` fallback refs; malformed payloads now fail directly if the caller did not provide result refs.
- Documented why command-exec namespace handling remains separate from `sandbox.overlay.namespace`.
- Added focused policy tests for default filtering and test-time tightening.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py backend/tests/unit_test/test_sandbox/test_command_exec/test_env_policy.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_to_occ_client.py::test_shell_capture_goes_through_occ_client_before_lease_release -q` -> 17 passed.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_import_fence.py::test_command_exec_imports_only_client_protocol_boundaries backend/tests/unit_test/test_sandbox/test_import_fence.py::test_internal_sandbox_layers_do_not_import_public_api -q` -> 2 passed.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py::test_bundle_layout_includes_required_paths backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py::test_bundle_extracted_python_modules_import_clean -q` -> 2 passed.

### Final Verification Pass

Status: partially blocked by unrelated dirty-tree sandbox/OCC changes.

Passed:

- `uv run ruff check backend/src/sandbox/command_exec backend/src/sandbox/runtime/daemon/service/shell_runner.py backend/src/sandbox/overlay/mounts.py backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py backend/tests/unit_test/test_sandbox/test_command_exec/test_env_policy.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_to_occ_client.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py` -> all checks passed.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py backend/tests/unit_test/test_sandbox/test_command_exec/test_env_policy.py backend/tests/unit_test/test_sandbox/test_import_fence.py::test_command_exec_imports_only_client_protocol_boundaries backend/tests/unit_test/test_sandbox/test_import_fence.py::test_internal_sandbox_layers_do_not_import_public_api -q` -> command-exec and import-fence coverage passed.
- Command-exec runtime bundle membership probe -> all new command-exec files are present in `_runtime_bundle_bytes()`.
- Facade import smoke: `CommandExecPolicy`, `CommandExecRequest`, `WorkspaceReplacementMountSpec`, `execute_command`, and `namespace_helper.main` import successfully.

Blocked:

- `test_capture_to_occ_client.py::test_shell_capture_goes_through_occ_client_before_lease_release` now fails during collection because the current dirty tree has `sandbox.occ.service -> sandbox.occ.stage.transaction` importing `DirectMerge` from `sandbox.occ.stage.direct`, where that symbol is absent.
- Full bundle layout/import tests now fail on current dirty-tree layout drift unrelated to command-exec: missing old `sandbox/occ/capture/*`, `sandbox/occ/merge/*`, and `sandbox/occ/routing/*` paths, plus `sandbox.api.__init__` importing missing `sandbox.api.default`.

### Cleanup Follow-up - 2026-05-14

Status: previous OCC/API collection blockers resolved.

Changes:

- Updated command-exec comments/tests from `OccSerialMerger` to `CommitQueue`.
- Removed the stale `commit_prepared_changeset` call path; command-exec write
  and edit handlers now call `OCCClient.commit_prepared`.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_api backend/tests/unit_test/test_sandbox/test_host backend/tests/unit_test/test_sandbox/test_runtime_bootstrap.py backend/tests/unit_test/test_sandbox/test_live_setup_api.py backend/tests/unit_test/test_sandbox/test_occ backend/tests/unit_test/test_sandbox/test_command_exec/test_edit_snapshot_byte_derivation.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_to_occ_client.py -q` -> 178 passed, 1 skipped.
- `uv run ruff check backend/src/sandbox/api backend/src/sandbox/occ backend/src/sandbox/runtime/daemon/service/occ_backend.py backend/src/sandbox/runtime/daemon/handler/tools/edit.py backend/src/sandbox/runtime/daemon/handler/tools/write.py backend/tests/unit_test/test_sandbox/test_api backend/tests/unit_test/test_sandbox/test_occ backend/tests/unit_test/test_sandbox/test_command_exec/test_edit_snapshot_byte_derivation.py` -> all checks passed.
