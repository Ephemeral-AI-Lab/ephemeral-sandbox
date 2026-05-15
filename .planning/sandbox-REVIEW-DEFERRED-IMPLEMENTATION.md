# Sandbox Deferred Review Implementation Report

Source review: `.planning/sandbox-REVIEW-DEFERRED.md`

## Current Baseline

- Starting dirty worktree contained pre-existing changes under `backend/src/task_center*`.
- Sandbox work will avoid those paths unless a later phase explicitly requires them.
- `.planning/sandbox-REVIEW-DEFERRED.md` is untracked in this checkout and treated as the source artifact for this pass.

## Phase 1 - Prep Guard

Status: complete

Scope:
- Inspect `git status --short`.
- Read `.planning/sandbox-REVIEW-DEFERRED.md`.
- Consult `.planning/sandbox-REVIEW.md` and `/tmp/sandbox_review/execution.md` only for the C2 blocker and implementation shape.
- Establish this report.

Selected implementation order:
1. C2 two-pipeline collapse.
2. S4 provider Daytona client collapse.
3. S5 OCC flattening.
4. S6 plugin runtime flattening with compatibility shim.
5. Deferred daemon depth decision.
6. Local cleanups S7-S10 and smaller wins.
7. Cross-cutting naming renames only after flattening phases are green.

Blocker review:
- The historical C2 blocker is `backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py`, which asserts the overlay runner/pipeline/worker/mount files exist.
- The current task resolves the direction: collapse into `orchestrator.execute_command(..., occ_apply=False, mount_mode=MountMode.COPY_BACKED)` and rewrite/delete tests according to the new boundary.
- Public surface choice for C2: use `occ_apply: bool = True`, matching the deferred review's preferred flag.

Changed files:
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None

Tests and guards run:
- `git status --short`
- `git diff --stat`
- `git diff --check`

Failures and fixes:
- None

Next phase recommendation:
- Proceed to C2. Start by migrating daemon overlay calls to the orchestrator with `occ_apply=False`, then remove obsolete overlay pipeline modules after tests are rewritten.

## Phase 2 - C2 Two-Pipeline Collapse

Status: complete

Scope:
- Merge snapshot-overlay execution into `sandbox.execution.orchestrator.execute_command`.
- Add `occ_apply: bool = True` and `mount_mode: MountMode | None = None` to the orchestrator path.
- Route daemon `overlay.run` through the orchestrator with `occ_apply=False` and `mount_mode=MountMode.COPY_BACKED`.
- Delete obsolete overlay runner/pipeline/worker/mount modules after callers and tests moved.
- Rewrite the listed unit tests around the new orchestrator boundary.

Implementation notes:
- `CommandExecResult` now carries stdout/stderr refs so no-OCC overlay callers can return readable artifacts.
- `WorkspaceCapture` now carries the snapshot manifest so daemon `overlay.run` can preserve the old `OverlayCapture` payload shape.
- No-OCC orchestrator runs keep capture artifacts while removing bulk runtime intermediates.
- `CommandExecPolicy` now supports optional host-env allowlists; daemon `overlay.run` uses the old minimal environment behavior while command-exec default behavior remains unchanged.
- A concurrent commit appeared during the guard: `4a5ad60b Reframe TaskCenter naming and collapse overlay execution`. It contains the C2 work plus pre-existing TaskCenter naming changes. The current working tree now has a separate dirty `backend/src/task_center/entry/coordinator.py` change that is not part of this sandbox phase.

Changed files:
- `backend/src/sandbox/daemon/handler/overlay.py`
- `backend/src/sandbox/execution/contract.py`
- `backend/src/sandbox/execution/orchestrator.py`
- `backend/src/sandbox/execution/policy.py`
- `backend/src/sandbox/execution/workspace_mount.py`
- `backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py`
- `backend/tests/unit_test/test_sandbox/test_overlay/test_namespace_command_env.py`
- `backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py`
- `backend/tests/unit_test/test_sandbox/test_overlay/test_runtime_invoker_cleanup.py`
- `backend/tests/unit_test/test_sandbox/test_overlay/test_snapshot_overlay_runner.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- `backend/src/sandbox/execution/overlay_mounts.py`
- `backend/src/sandbox/execution/overlay_pipeline.py`
- `backend/src/sandbox/execution/overlay_runner.py`
- `backend/src/sandbox/execution/overlay_worker.py`

Compatibility shims:
- None kept for the deleted execution internals. They were not public plugin surfaces per the deferred review.
- `sandbox.execution.overlay_request`, `overlay_result`, `overlay_capture`, and `overlay_change` remain.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_overlay/test_snapshot_overlay_runner.py backend/tests/unit_test/test_sandbox/test_overlay/test_runtime_invoker_cleanup.py backend/tests/unit_test/test_sandbox/test_overlay/test_namespace_command_env.py backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py -q` - 25 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 545 passed, 1 skipped
- `rg -n "from sandbox\\.execution\\.overlay_(runner|pipeline|worker|mounts)|sandbox\\.execution\\.overlay_(runner|pipeline|worker|mounts)" backend/src backend/tests` - no hits
- `git diff --stat` - current uncommitted diff only shows unrelated `backend/src/task_center/entry/coordinator.py`
- `git show --stat --oneline HEAD` - phase changes are in `4a5ad60b`
- `git diff --check` - clean

Failures and fixes:
- Initial broad import scan found stale live-e2e native probe text importing `sandbox.overlay.OverlayRuntimeInvoker`. Those probes predate this execution package layout and do not import the deleted `sandbox.execution.overlay_*` modules. No C2 code/test blocker remains.

Next phase recommendation:
- Proceed to S4 provider Daytona client collapse. Keep watching for dirty TaskCenter changes and avoid touching them.

## Phase 3 - S4 Provider Daytona Client Collapse

Status: complete

Scope:
- Collapse `sandbox/provider/daytona/client/` into `sandbox/provider/daytona/client.py`.
- Rewrite Daytona provider internals and tests from `sandbox.provider.daytona.client.*` deep imports to the flat `sandbox.provider.daytona.client` surface.
- Replace adapter imports of private sync-client helper names with explicit public helper names on the flat client module.

Implementation notes:
- The flat client module now owns credential loading, sync client caching, async loop-local client caching, timeout wrapping, pagination, and async-client shutdown helpers.
- Public helper names were introduced for adapter use: `SANDBOX_TIMEOUT_SECONDS`, `HEALTH_TIMEOUT_SECONDS`, `normalize_dict`, `normalize_optional_text`, `creation_param_classes`, `paginate_all`, and `call_with_optional_timeout`.
- Tests now patch/import the flat client module directly.
- Current working tree also contains unrelated concurrent edits under `backend/src/db/stores/`, `backend/src/task_center/`, and `backend/src/task_center_runner/`; they were not edited for S4.

Changed files:
- `backend/src/sandbox/provider/daytona/client.py`
- `backend/src/sandbox/provider/daytona/adapter.py`
- `backend/src/sandbox/provider/daytona/context.py`
- `backend/tests/unit_test/test_sandbox/test_service.py`
- `backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py`
- `backend/tests/unit_test/test_sandbox/test_lifecycle.py`
- `backend/tests/unit_test/test_sandbox/test_credentials.py`
- `backend/tests/unit_test/test_sandbox/test_async/test_client.py`
- `backend/tests/unit_test/test_sandbox/test_context.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- `backend/src/sandbox/provider/daytona/client/__init__.py`
- `backend/src/sandbox/provider/daytona/client/sync_client.py`
- `backend/src/sandbox/provider/daytona/client/async_client.py`
- `backend/src/sandbox/provider/daytona/client/credentials.py`
- `backend/src/sandbox/provider/daytona/client/shutdown.py`

Compatibility shims:
- None kept. The deferred S4 item explicitly changes the internal provider client surface to the flat `sandbox.provider.daytona.client` module.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_service.py backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py backend/tests/unit_test/test_sandbox/test_lifecycle.py backend/tests/unit_test/test_sandbox/test_credentials.py backend/tests/unit_test/test_sandbox/test_async/test_client.py backend/tests/unit_test/test_sandbox/test_context.py backend/tests/unit_test/test_sandbox/test_provider_registry.py backend/tests/unit_test/test_sandbox/test_workspace.py backend/tests/unit_test/test_sandbox/test_providers/test_daytona_adapter.py backend/tests/unit_test/test_sandbox/test_providers/test_daytona_bash_exit_code.py -q` - 73 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 545 passed, 1 skipped
- `rg -n "from sandbox\\.provider\\.daytona\\.client\\.|import sandbox\\.provider\\.daytona\\.client\\." backend` - no hits
- `.venv/bin/ruff check backend/src/sandbox/provider/daytona/client.py backend/src/sandbox/provider/daytona/adapter.py backend/src/sandbox/provider/daytona/context.py backend/tests/unit_test/test_sandbox/test_service.py backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py backend/tests/unit_test/test_sandbox/test_lifecycle.py backend/tests/unit_test/test_sandbox/test_credentials.py backend/tests/unit_test/test_sandbox/test_async/test_client.py backend/tests/unit_test/test_sandbox/test_context.py` - passed
- `git diff --stat` - shows S4 plus unrelated concurrent non-sandbox edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Proceed to S5 OCC flattening. Keep the S5 import rewrite mechanical and avoid mixing in the deferred OCC behavior cleanups.

## Phase 4 - S5 OCC Flattening

Status: complete

Scope:
- Flatten `sandbox.occ.stage`, `sandbox.occ.content`, and `sandbox.occ.changeset` subpackages into depth-3 modules.
- Delete pure re-export shims and the `occ.timing_keys` re-export.
- Keep `sandbox.occ.__init__` as a stable facade.
- Rewrite sandbox, daemon, task-center-runner, live-e2e, and test imports mechanically.

Implementation notes:
- Promoted `occ/stage/transaction.py` to `occ/commit_transaction.py`.
- Promoted `occ/stage/merge.py` to `occ/stage.py` and inlined `stage/_edit.py`.
- Promoted `occ/stage/policy.py` to `occ/stage_policy.py`.
- Merged `occ/changeset/{types,prepared}.py` into `occ/changeset.py`.
- Promoted `occ/content/hashing.py` to `occ/hashing.py`.
- Promoted `occ/content/gitignore_oracle.py` to `occ/gitignore.py`.
- Replaced `sandbox.occ.timing_keys.TimingKey` imports with `sandbox.timing_keys.TimingKey`.
- Updated runtime bundle assertions to require the flat OCC modules and reject the old paths.

Changed files:
- `backend/src/sandbox/occ/__init__.py`
- `backend/src/sandbox/occ/changeset.py`
- `backend/src/sandbox/occ/client.py`
- `backend/src/sandbox/occ/commit_queue.py`
- `backend/src/sandbox/occ/commit_transaction.py`
- `backend/src/sandbox/occ/gitignore.py`
- `backend/src/sandbox/occ/hashing.py`
- `backend/src/sandbox/occ/maintenance.py`
- `backend/src/sandbox/occ/overlay.py`
- `backend/src/sandbox/occ/router.py`
- `backend/src/sandbox/occ/service.py`
- `backend/src/sandbox/occ/stage.py`
- `backend/src/sandbox/occ/stage_policy.py`
- `backend/src/sandbox/daemon/*` files that imported deep OCC paths
- `backend/tests/live_e2e_test/sandbox/occ/*`
- `backend/tests/unit_test/test_sandbox/test_occ/*`
- OCC-related sandbox API, command-exec, daemon, and toolkit tests
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- `backend/src/sandbox/occ/changeset/__init__.py`
- `backend/src/sandbox/occ/changeset/prepared.py`
- `backend/src/sandbox/occ/changeset/types.py`
- `backend/src/sandbox/occ/content/__init__.py`
- `backend/src/sandbox/occ/content/gitignore_oracle.py`
- `backend/src/sandbox/occ/content/hashing.py`
- `backend/src/sandbox/occ/stage/__init__.py`
- `backend/src/sandbox/occ/stage/_edit.py`
- `backend/src/sandbox/occ/stage/direct.py`
- `backend/src/sandbox/occ/stage/gated.py`
- `backend/src/sandbox/occ/stage/merge.py`
- `backend/src/sandbox/occ/stage/policy.py`
- `backend/src/sandbox/occ/stage/transaction.py`
- `backend/src/sandbox/occ/timing_keys.py`

Compatibility shims:
- No deep-path shims kept for `occ.stage.*`, `occ.content.*`, `occ.changeset.*`, or `occ.timing_keys`; S5 explicitly removes these depth-4 surfaces.
- `sandbox.occ` facade remains and now exports `CommitTransaction`, `DirectStager`, `GatedStager`, `FileResult`, and `FileStatus`.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_occ backend/tests/unit_test/test_sandbox/test_api/test_gitignore_oracle_cache.py backend/tests/unit_test/test_sandbox/test_api/test_shell_staleness_telemetry.py backend/tests/unit_test/test_sandbox/test_api/test_shell_atomic_by_path_count.py backend/tests/unit_test/test_sandbox/test_api/test_guarded_result_status.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_to_occ_client.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_changeset.py backend/tests/unit_test/test_sandbox/test_command_exec/test_edit_snapshot_byte_derivation.py backend/tests/unit_test/test_sandbox/test_daemon/test_overlay_capture.py -q` - 98 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 545 passed, 1 skipped
- `rg -n "from sandbox\\.occ\\.(stage|changeset|content)\\.|import sandbox\\.occ\\.(stage|changeset|content)\\." backend/src backend/tests` - no hits
- `rg -n "sandbox\\.occ\\.timing_keys|from sandbox\\.occ\\.timing_keys" backend/src backend/tests` - no hits
- `.venv/bin/ruff check backend/src/sandbox/occ backend/tests/unit_test/test_sandbox/test_occ backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py` - passed after removing one unused import
- `git diff --stat` - shows S5 plus earlier S4 and unrelated concurrent rename edits
- `git diff --check` - clean

Failures and fixes:
- Ruff reported an unused `RouteDecision` import in `backend/src/sandbox/occ/stage.py`; removed it and reran the guard successfully.

Next phase recommendation:
- Proceed to S6 plugin/runtime flattening. Keep `plugin/runtime/__init__.py` as a deprecation shim as required by the deferred review.

## Phase 5 - S6 Plugin Runtime Flattening

Status: complete

Scope:
- Move `sandbox/plugin/runtime/context.py` to `sandbox/plugin/op_context.py`.
- Move `sandbox/plugin/runtime/registry.py` to `sandbox/plugin/op_registry.py`.
- Keep `sandbox/plugin/runtime/__init__.py` as a deprecation re-export shim.
- Update sandbox-internal imports and runtime bundle shipping.
- Preserve in-tree LSP plugin compatibility through `from sandbox.plugin.runtime import register_plugin_op`.

Implementation notes:
- `sandbox.plugin.handler` now imports `PluginOpContext` and registry helpers from `op_context` / `op_registry`.
- Runtime bundle now ships `sandbox/plugin/op_context.py`, `sandbox/plugin/op_registry.py`, and the `sandbox/plugin/runtime/__init__.py` shim.
- Plugin tests use `sandbox.plugin.op_registry` for registry internals while retaining `sandbox.plugin.runtime` imports where compatibility is intentional.
- The shim emits `DeprecationWarning` on import and re-exports the public runtime API.

Changed files:
- `backend/src/sandbox/plugin/op_context.py`
- `backend/src/sandbox/plugin/op_registry.py`
- `backend/src/sandbox/plugin/runtime/__init__.py`
- `backend/src/sandbox/plugin/handler.py`
- `backend/src/sandbox/host/runtime_bundle.py`
- `backend/tests/unit_test/test_sandbox/test_plugin_runtime_registry.py`
- `backend/tests/unit_test/test_sandbox/test_plugin_handler.py`
- `backend/tests/unit_test/test_sandbox/test_plugin_lifecycle_wedge.py`
- `backend/tests/unit_test/test_sandbox/test_runtime_bundle_includes_plugin.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- `backend/src/sandbox/plugin/runtime/context.py`
- `backend/src/sandbox/plugin/runtime/registry.py`

Compatibility shims:
- Kept `backend/src/sandbox/plugin/runtime/__init__.py` as a deprecation shim for plugin authors and the in-tree LSP plugin.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_plugin_runtime_registry.py backend/tests/unit_test/test_sandbox/test_plugin_handler.py backend/tests/unit_test/test_sandbox/test_plugin_lifecycle_wedge.py backend/tests/unit_test/test_sandbox/test_runtime_bundle_includes_plugin.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py -q` - 29 passed, 1 expected deprecation warning
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 545 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "from sandbox\\.plugin\\.runtime\\.(context|registry)|import sandbox\\.plugin\\.runtime\\.(context|registry)|sandbox\\.plugin\\.runtime\\.(context|registry)" backend/src backend/tests` - no hits
- `.venv/bin/ruff check backend/src/sandbox/plugin backend/tests/unit_test/test_sandbox/test_plugin_runtime_registry.py backend/tests/unit_test/test_sandbox/test_plugin_handler.py backend/tests/unit_test/test_sandbox/test_plugin_lifecycle_wedge.py backend/tests/unit_test/test_sandbox/test_runtime_bundle_includes_plugin.py` - passed after moving the warning below imports
- `git diff --stat` - shows S6 plus prior phases and unrelated concurrent rename edits
- `git diff --check` - clean

Failures and fixes:
- Ruff reported `E402` in the deprecation shim because the warning ran before re-export imports. Moved the warning below the imports and reran the guard successfully.

Next phase recommendation:
- Inspect current daemon handler/service imports and decide whether Option B is still the smallest boundary-preserving daemon-depth fix.

## Phase 6 - Deferred Daemon Depth Decision

Status: complete

Decision:
- Implemented Option B.
- Rationale: current imports still showed broad handler/service depth-4 coupling, but only four shared modules needed promotion. Option A would flatten ~24 daemon files into one directory, while Option C would leave the strict import-depth issue unresolved.

Scope:
- Promote shared daemon internals up one level.
- Rewrite daemon and test imports away from `sandbox.daemon.handler.request_context`, `sandbox.daemon.service.occ_backend`, `sandbox.daemon.service.result_projection`, and `sandbox.daemon.service.workspace_server`.
- Keep `service/` for remaining non-promoted services: `layer_stack_client.py`, `workspace_binding.py`, and `shell_runner.py`.

Changed files:
- `backend/src/sandbox/daemon/_toolbox.py`
- `backend/src/sandbox/daemon/_wire.py`
- `backend/src/sandbox/daemon/occ_backend.py`
- `backend/src/sandbox/daemon/workspace_server.py`
- Daemon handlers and services importing those modules
- Daemon, command-exec, OCC, and API tests importing those modules
- `backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- `backend/src/sandbox/daemon/handler/request_context.py`
- `backend/src/sandbox/daemon/service/occ_backend.py`
- `backend/src/sandbox/daemon/service/result_projection.py`
- `backend/src/sandbox/daemon/service/workspace_server.py`

Compatibility shims:
- None kept. These are daemon-internal modules and the deferred review frames this as a boundary cleanup, not a public API.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_daemon backend/tests/unit_test/test_sandbox/test_command_exec backend/tests/unit_test/test_sandbox/test_occ/test_mutation_gate.py backend/tests/unit_test/test_sandbox/test_api/test_shell_staleness_telemetry.py backend/tests/unit_test/test_sandbox/test_occ/test_shell_capture_atomicity.py -q` - 139 passed after fixing leftover test imports
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 545 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "from sandbox\\.daemon\\.(handler\\.request_context|service\\.(occ_backend|result_projection|workspace_server))|sandbox\\.daemon\\.(handler\\.request_context|service\\.(occ_backend|result_projection|workspace_server))" backend/src backend/tests` - no hits
- `.venv/bin/ruff check backend/src/sandbox/daemon backend/tests/unit_test/test_sandbox/test_daemon backend/tests/unit_test/test_sandbox/test_occ/test_mutation_gate.py` - passed
- `git diff --stat` - shows daemon Option B plus prior phases and unrelated concurrent rename edits
- `git diff --check` - clean

Failures and fixes:
- First daemon-focused test run failed on stale `request_context` test imports and one indentation issue caused by mechanical rewrite. Updated tests to import `sandbox.daemon._toolbox` and reran successfully.

Next phase recommendation:
- Proceed to local cleanups S7-S10 one at a time. Start with S7 because it is a narrow one-caller deletion.

## Phase 7.1 - S7 Delete Host Context Preparer

Status: complete

Scope:
- Delete `backend/src/sandbox/host/context_preparer.py`.
- Keep the public `sandbox.api.context_preparer_for` compatibility surface.
- Remove the stale host package note for the deleted module.
- Add focused tests for the public factory behavior.

Implementation notes:
- `context_preparer_for` now lives in `sandbox.api._control`, alongside the other provider-facing public control helpers.
- `sandbox.api.__init__` only re-exports the factory from `_control`, preserving the API package import fence.
- The empty `SandboxRuntimeContext` and `SandboxContextPreparer` Protocol stubs were removed with the deleted host module.
- Removed one stale unused import from `sandbox.api._impl._results` after the phase ruff guard reported it.

Changed files:
- `backend/src/sandbox/api/__init__.py`
- `backend/src/sandbox/api/_control.py`
- `backend/src/sandbox/api/_impl/_results.py`
- `backend/src/sandbox/host/__init__.py`
- `backend/tests/unit_test/test_sandbox/test_context.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- `backend/src/sandbox/host/context_preparer.py`

Compatibility shims:
- Kept `sandbox.api.context_preparer_for`.
- No shim kept for `sandbox.host.context_preparer`; S7 explicitly deletes that host-internal module.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_context.py -q` - 18 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_context.py backend/tests/unit_test/test_sandbox/test_api/test_contract.py -q` - 40 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "sandbox\\.host\\.context_preparer|from sandbox\\.host import context_preparer|context_preparer.py" backend/src backend/tests` - no hits
- `.venv/bin/ruff check backend/src/sandbox/api backend/src/sandbox/host backend/tests/unit_test/test_sandbox/test_context.py` - passed after removing one stale unused import
- `git diff --stat` - shows S7 plus prior sandbox phases and unrelated concurrent rename edits
- `git diff --check` - clean

Failures and fixes:
- The first full sandbox guard failed because `sandbox.api.__init__` imported `sandbox.provider.registry`, violating the API import-boundary contract. Moved the factory implementation into `sandbox.api._control`, which already owns provider-facing control helpers, and reran the guard successfully.
- The first targeted ruff check found an unused `WriteFileResult` import in `sandbox.api._impl._results`; removed it and reran the guard successfully.

Next phase recommendation:
- Proceed to S8. Keep it scoped to `host/daemon_client.py`, preserve retry/readiness semantics, and run daemon-client focused tests before the full sandbox suite.

## Phase 7.2 - S8 Inline Daemon Client Dispatch Stack

Status: complete

Scope:
- Simplify `backend/src/sandbox/host/daemon_client.py`.
- Collapse the private `_exec_daemon_call`, `_should_retry_after_connect_failure`, `_check_daemon_readiness_after_spawn`, and `_readiness_request_for_original` chain into one `_dispatch_once_with_retry` helper.
- Keep `_call_daemon` as the stable internal entry point used by callers and tests.

Implementation notes:
- `_dispatch_once_with_retry` now owns the thin-client call, one reconnect retry after `_THIN_CLIENT_CONNECT_FAILED`, daemon spawn, runtime readiness probe, bootstrap readiness exception, and final retry of the original envelope.
- Readiness payload construction now uses the already-normalized `op` and `args` from `_call_daemon`, so the old JSON reparse helper is no longer needed.
- Spawn failure behavior remains fail-closed by returning the failed spawn result to `_call_daemon`, which raises the existing `RuntimeExecFailed` error.

Changed files:
- `backend/src/sandbox/host/daemon_client.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed. Removed helpers were private; `_call_daemon`, `call_daemon_api`, and `ensure_daemon_current` remain.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_daemon/test_daemon_transport.py backend/tests/unit_test/test_sandbox/test_api/test_daemon_client.py -q` - 13 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_daemon backend/tests/unit_test/test_sandbox/test_api/test_daemon_client.py backend/tests/unit_test/test_sandbox/test_runtime_bootstrap.py -q` - 75 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "_exec_daemon_call|_should_retry_after_connect_failure|_check_daemon_readiness_after_spawn|_readiness_request_for_original" backend/src/sandbox/host/daemon_client.py backend/tests/unit_test/test_sandbox` - no hits
- `.venv/bin/ruff check backend/src/sandbox/host/daemon_client.py backend/tests/unit_test/test_sandbox/test_daemon/test_daemon_transport.py backend/tests/unit_test/test_sandbox/test_api/test_daemon_client.py` - passed
- `git diff --stat` - shows S8 plus prior sandbox phases and unrelated concurrent rename edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Proceed to S9. Keep it to the tuple-driven runtime bundle inclusion loop and bundle-content tests.

## Phase 7.3 - S9 Data-Driven Runtime Bundle Includes

Status: complete

Scope:
- Simplify repeated `_add_if_exists` blocks in `backend/src/sandbox/host/runtime_bundle.py`.
- Preserve the exact bundle file set and archive names.

Implementation notes:
- Replaced the root sandbox module `_add_if_exists` calls with a tuple-driven loop.
- Replaced the plugin module `_add_if_exists` calls with a tuple-driven loop.
- Did not change tree bundling, exclusions, pathspec vendoring, or upload behavior.

Changed files:
- `backend/src/sandbox/host/runtime_bundle.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_runtime_bundle_includes_plugin.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py -q` - 13 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `.venv/bin/ruff check backend/src/sandbox/host/runtime_bundle.py backend/tests/unit_test/test_sandbox/test_runtime_bundle_includes_plugin.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py` - passed
- `git diff --stat` - shows S9 plus prior sandbox phases and unrelated concurrent rename edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Proceed to S10. Work in `backend/src/sandbox/occ/commit_transaction.py` because S5 promoted the old transaction module there.

## Phase 7.4 - S10 Extract Commit Transaction Helpers

Status: complete

Scope:
- Refactor `backend/src/sandbox/occ/commit_transaction.py`.
- Extract route timing accumulation from `CommitTransaction.revalidate_and_publish`.
- Extract the atomic/overlay drop decision from `CommitTransaction.revalidate_and_publish`.
- Preserve OCC validation, staging, publish, and rollback behavior.

Implementation notes:
- Added `_accumulate_route_timings`, which records gated/direct timing totals and returns whether any OCC-gated path failed.
- Added `_atomic_or_overlay_dropped`, which returns the existing drop message for atomic validation failure or overlay-capture OCC-gated failure.
- `revalidate_and_publish` now keeps the transaction orchestration flow while delegating the two policy calculations to helpers.

Changed files:
- `backend/src/sandbox/occ/commit_transaction.py`
- `backend/tests/unit_test/test_sandbox/test_audit/test_operation.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_occ/test_commit_transaction.py backend/tests/unit_test/test_sandbox/test_occ/test_concurrent_commits.py backend/tests/unit_test/test_sandbox/test_occ/test_direct_merge.py backend/tests/unit_test/test_sandbox/test_occ/test_tracked_merge.py backend/tests/unit_test/test_sandbox/test_occ/test_shell_capture_atomicity.py backend/tests/unit_test/test_sandbox/test_occ/test_shell_atomic_conflicts.py backend/tests/unit_test/test_sandbox/test_occ/test_gitignore_policy_edge_cases.py -q` - 22 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_occ -q` - 57 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_audit/test_operation.py -q` - 3 passed after fixing the guard typo
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `.venv/bin/ruff check backend/src/sandbox/occ/commit_transaction.py backend/tests/unit_test/test_sandbox/test_occ backend/tests/unit_test/test_sandbox/test_audit/test_operation.py` - passed
- `git diff --stat` - shows S10 plus prior sandbox phases and unrelated concurrent rename edits
- `git diff --check` - clean

Failures and fixes:
- The first full sandbox guard failed in `backend/tests/unit_test/test_sandbox/test_audit/test_operation.py` on a concurrent dirty typo, `nodegoal_id`. The live audit node still exposes `mission_id`, so the test was repaired to `node.mission_id` and the guard reran successfully.

Next phase recommendation:
- Inspect `.planning/sandbox-REVIEW-DEFERRED.md` section 5 and `/tmp/sandbox_review/*` LOW items for remaining independent smaller wins. Do not start cross-cutting naming renames until the local cleanup queue is exhausted.

## Phase 7.5 - Inline API Payload CWD Helper

Status: complete

Scope:
- Inline `normalize_overlay_cwd` from `backend/src/sandbox/api/_impl/_payload.py`.
- Keep shell cwd normalization behavior unchanged.
- Remove the helper-only test.

Implementation notes:
- `sandbox.api._impl.shell.shell` now computes `cwd = (request.cwd or "").strip() or "."` directly.
- Removed `normalize_overlay_cwd` from `_payload.py` and its `__all__`.
- Current visible dirty state has narrowed to this phase plus two unrelated TaskCenter test edits, likely because earlier sandbox phases were committed by concurrent work.

Changed files:
- `backend/src/sandbox/api/_impl/_payload.py`
- `backend/src/sandbox/api/_impl/shell.py`
- `backend/tests/unit_test/test_sandbox/test_api/test_payload_helpers.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None kept. The helper was sandbox-internal and the review explicitly called for inlining it.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_api/test_payload_helpers.py backend/tests/unit_test/test_sandbox/test_api/test_shell.py backend/tests/unit_test/test_sandbox/test_api/test_contract.py -q` - 29 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 546 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "normalize_overlay_cwd" backend/src/sandbox backend/tests/unit_test/test_sandbox` - no hits
- `.venv/bin/ruff check backend/src/sandbox/api/_impl/_payload.py backend/src/sandbox/api/_impl/shell.py backend/tests/unit_test/test_sandbox/test_api/test_payload_helpers.py backend/tests/unit_test/test_sandbox/test_api/test_shell.py` - passed
- `git diff --stat` - shows this API payload phase plus unrelated TaskCenter test edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Proceed to the adjacent `_payload.py` docstring cleanup for `int_from_payload`, then continue through the remaining independent smaller wins.

## Phase 7.6 - Document Strict Integer Payload Decoding

Status: complete

Scope:
- Add the missing contract docstring to `int_from_payload`.
- Explain the bool-rejection behavior called out in the deferred review.

Implementation notes:
- Added a one-line docstring: `Return an integer boundary value without accepting bool-as-int.`
- No behavior changed.

Changed files:
- `backend/src/sandbox/api/_impl/_payload.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_api/test_payload_helpers.py -q` - 4 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 546 passed, 1 skipped, 1 expected deprecation warning
- `.venv/bin/ruff check backend/src/sandbox/api/_impl/_payload.py backend/tests/unit_test/test_sandbox/test_api/test_payload_helpers.py` - passed
- `git diff --stat` - shows this payload docstring plus previous API payload cleanup and unrelated concurrent test edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Proceed to the audit translation helper inline (`_terminal_type` / `_subsystem_event`) if the live code still has the one-call helpers.

## Phase 7.7 - Inline Audit Translation Helpers

Status: complete

Scope:
- Inline the one-call `_terminal_type` helper in `sandbox.audit.translation`.
- Inline the one-call `_subsystem_event` helper in `sandbox.audit.translation`.
- Preserve emitted audit event types and payload shapes.

Implementation notes:
- `events_from_result` now computes the terminal event type locally.
- `_subsystem_events` now constructs `AuditEvent` objects directly in its list comprehension.
- Repaired another concurrent dirty test typo in `backend/tests/unit_test/test_sandbox/test_audit/test_operation.py`: `SandboxCaller` still exposes `task_center_mission_id`, not `task_center_goal_id`.

Changed files:
- `backend/src/sandbox/audit/translation.py`
- `backend/tests/unit_test/test_sandbox/test_audit/test_operation.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed. Removed helpers were private.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_audit -q` - 3 passed after fixing the concurrent field-name typo
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 546 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "_terminal_type|_subsystem_event\\(" backend/src/sandbox/audit backend/tests/unit_test/test_sandbox/test_audit` - no hits
- `.venv/bin/ruff check backend/src/sandbox/audit/translation.py backend/tests/unit_test/test_sandbox/test_audit` - passed
- `git diff --stat` - shows this audit phase plus API payload cleanup and unrelated concurrent TaskCenter/test edits
- `git diff --check` - clean

Failures and fixes:
- First audit test run failed because a concurrent dirty edit changed `task_center_mission_id` to `task_center_goal_id` in a `SandboxCaller` constructor. The live model contract still uses `task_center_mission_id`; repaired the test and reran successfully.

Next phase recommendation:
- Check the remaining consolidated smaller wins: `timing.py` normalization machinery, `occ/service.py:_wrap_commit_result`, `daemon/handler/health.py` dead probe, and any stale `committed_paths` logic now that daemon result projection moved to `_wire.py`.

## Phase 7.8 - Remove Dead Stringified TimingKey Audit Fallback

Status: complete

Scope:
- Simplify `backend/src/sandbox/timing.py`.
- Keep `normalize_timing_map` support for actual `Enum`/`TimingKey` keys.
- Remove the dead `TimingKey.NAME` string-prefix fallback from `timing_audit_signals`.

Implementation notes:
- Production only calls `timing_audit_signals` from `sandbox.audit.translation` after `operation_payload` normalizes result timings.
- Removed `_matches_timing_prefix`, `_STRINGIFIED_TIMING_KEY_PREFIXES`, and `_TIMING_KEY_NAME_TO_VALUE`.
- Updated timing tests to exercise normalized string timing keys only for audit signal classification.

Changed files:
- `backend/src/sandbox/timing.py`
- `backend/tests/unit_test/test_sandbox/test_timing.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None. This removes internal fallback behavior that no production caller used.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_timing.py backend/tests/unit_test/test_sandbox/test_audit -q` - 8 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 543 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "timing_audit_signals\\(" backend/src backend/tests/unit_test/test_sandbox` - production caller remains `sandbox.audit.translation` after normalization; tests call it directly with normalized string keys
- `.venv/bin/ruff check backend/src/sandbox/timing.py backend/tests/unit_test/test_sandbox/test_timing.py backend/src/sandbox/audit/translation.py backend/tests/unit_test/test_sandbox/test_audit` - passed
- `git diff --stat` - shows this timing phase plus prior API/audit cleanup and unrelated concurrent TaskCenter/test edits
- `git diff --check` - clean after removing a trailing blank line

Failures and fixes:
- First `git diff --check` found a trailing blank line at EOF in `backend/tests/unit_test/test_sandbox/test_timing.py`; removed it and reran the guard successfully.

Next phase recommendation:
- Proceed to `occ/service.py:_wrap_commit_result` if the simplification remains local and testable.

## Phase 7.9 - Remove Dead Daemon Health Shell Probe

Status: complete

Scope:
- Remove the discarded `shell_runner.services(...)` call from `daemon/handler/health.py`.
- Remove the now-unused `shell_runner` import.
- Adjust runtime-ready tests that only monkeypatched the discarded call.

Implementation notes:
- `_probe_data_plane` still validates the handler data-plane backend via `request_context.services(layer_stack_root)`.
- The mutation-gate probe still validates `occ_backend.build_occ_backend`.
- Removed test monkeypatches for `health.shell_runner.services`; those were only supporting the deleted dead probe.

Changed files:
- `backend/src/sandbox/daemon/handler/health.py`
- `backend/tests/unit_test/test_sandbox/test_daemon/test_runtime_ready.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed. This removes private probe work only.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_daemon/test_runtime_ready.py -q` - 7 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 543 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "health\\.shell_runner|shell_runner\\.services\\(\\{\\\"layer_stack_root\\\"" backend/src/sandbox/daemon backend/tests/unit_test/test_sandbox/test_daemon` - no hits
- `.venv/bin/ruff check backend/src/sandbox/daemon/handler/health.py backend/tests/unit_test/test_sandbox/test_daemon/test_runtime_ready.py` - passed
- `git diff --stat` - shows this daemon health phase plus prior API/audit/timing cleanup and unrelated concurrent TaskCenter/test edits
- `git diff --check` - clean

Failures and fixes:
- None

Deferred note:
- `occ/service.py:_wrap_commit_result` direct-index simplification is not currently safe as written in the review: `TimingKey.COMMIT_TOTAL` is absent from CAS-exhaustion results, so replacing all defensive `.get(..., 0.0)` calls with direct indexing would introduce a `KeyError`. Keep this item deferred unless the commit queue starts stamping a worker duration on CAS-exhaustion results first.

Next phase recommendation:
- Inspect the old result-projection `committed_paths` fallback after the daemon Option B move to `daemon/_wire.py`.

## Phase 7.10 - Simplify Daemon Wire Committed Paths Fallback

Status: complete

Scope:
- Simplify `committed_paths` in `backend/src/sandbox/daemon/_wire.py`.
- Preserve current result shape: all published paths when present, otherwise the first available file path, otherwise the caller fallback path.

Implementation notes:
- Replaced the explicit committed/aborted/fallback branch ladder with a compact tuple fallback expression.
- Existing tests already covered committed, accepted-as-published, aborted fallback, no files, and empty paths.

Changed files:
- `backend/src/sandbox/daemon/_wire.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_api/test_guarded_result_status.py backend/tests/unit_test/test_sandbox/test_daemon/test_overlay_capture.py backend/tests/unit_test/test_sandbox/test_command_exec/test_edit_snapshot_byte_derivation.py -q` - 19 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 543 passed, 1 skipped, 1 expected deprecation warning
- `.venv/bin/ruff check backend/src/sandbox/daemon/_wire.py backend/tests/unit_test/test_sandbox/test_api/test_guarded_result_status.py` - passed
- `git diff --stat` - shows this daemon wire phase plus prior API/audit/timing/health cleanup and unrelated concurrent TaskCenter/test edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Review remaining low-priority deep-dive items. Implement only items that are unambiguous and local; otherwise document the deferral rationale before moving to cross-cutting naming renames.

## Phase 7.11 - Clarify Sandbox Audit Package Docstring

Status: complete

Scope:
- Fix the misleading `sandbox.audit.__init__` docstring.
- Preserve the package export surface.

Implementation notes:
- The package still re-exports nothing.
- The docstring now says callers should import concrete helpers from submodules.

Changed files:
- `backend/src/sandbox/audit/__init__.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_audit -q` - 3 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 543 passed, 1 skipped, 1 expected deprecation warning
- `.venv/bin/ruff check backend/src/sandbox/audit/__init__.py backend/tests/unit_test/test_sandbox/test_audit` - passed
- `git diff --stat` - shows this audit docstring phase plus prior local cleanups and unrelated concurrent TaskCenter/test edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Continue with only clear local hardening cleanups from the deep dives; document larger or public-surface suggestions as deferred.

## Phase 7.12 - Harden Daytona Project Root Fallback

Status: complete

Scope:
- Fix provider H4 in `backend/src/sandbox/provider/daytona/client.py`.
- Avoid `IndexError` for shallow paths when no repository marker is found.

Implementation notes:
- `_find_project_root` still returns the first parent containing `pyproject.toml` or `.git`.
- If no marker is found, it now returns the provided `start` path instead of indexing `start.parents[6]`.
- Added a shallow-path regression test.

Changed files:
- `backend/src/sandbox/provider/daytona/client.py`
- `backend/tests/unit_test/test_sandbox/test_credentials.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_credentials.py backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py -q` - 7 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 544 passed, 1 skipped, 1 expected deprecation warning
- `.venv/bin/ruff check backend/src/sandbox/provider/daytona/client.py backend/tests/unit_test/test_sandbox/test_credentials.py backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py` - passed
- `git diff --stat` - shows this provider hardening phase plus prior local cleanups and unrelated concurrent TaskCenter/test edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Implement provider H5 by replacing `assert factory_name in (...)` with explicit `ValueError` checks.

## Phase 7.13 - Replace Daytona Factory Asserts

Status: complete

Scope:
- Fix provider H5 in `backend/src/sandbox/provider/daytona/client.py`.
- Replace factory-name `assert` statements with explicit runtime validation.

Implementation notes:
- Added `_validate_factory_name`.
- `client_cache_key` and `build_sdk_client` now raise `ValueError` for unsupported factory names even under `python -O`.
- Added tests for both call paths.

Changed files:
- `backend/src/sandbox/provider/daytona/client.py`
- `backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py backend/tests/unit_test/test_sandbox/test_credentials.py -q` - 9 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 546 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "assert factory_name" backend/src/sandbox/provider/daytona/client.py backend/tests/unit_test/test_sandbox` - no hits
- `.venv/bin/ruff check backend/src/sandbox/provider/daytona/client.py backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py backend/tests/unit_test/test_sandbox/test_credentials.py` - passed
- `git diff --stat` - shows this provider hardening phase plus prior local cleanups and unrelated concurrent TaskCenter/test edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Check provider H6 (`close_client` bounded join behavior). Implement only if it can be done without changing lifecycle semantics unexpectedly.

## Phase 7.14 - Bound Daytona Fallback Client Shutdown Join

Status: complete

Scope:
- Fix provider H6 in `backend/src/sandbox/provider/daytona/client.py`.
- Prevent fallback-loop async client shutdown from waiting up to `N * 5s`.
- Preserve active-loop async close behavior.

Implementation notes:
- Split async close thread startup into `_start_async_close_thread`.
- Added `_join_close_threads`, which applies one shared timeout budget across a list of close threads.
- `close_client` preserves the old single-client behavior by starting one thread and joining it with a 5s budget.
- `shutdown_cached_client_async` now starts fallback-loop close threads first, then joins them as one batch with a single 5s budget.
- Added a regression test that verifies fallback-loop closers are joined in one batch.

Changed files:
- `backend/src/sandbox/provider/daytona/client.py`
- `backend/tests/unit_test/test_sandbox/test_lifecycle.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_lifecycle.py backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py -q` - 11 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `.venv/bin/ruff check backend/src/sandbox/provider/daytona/client.py backend/tests/unit_test/test_sandbox/test_lifecycle.py backend/tests/unit_test/test_sandbox/test_daytona_client_cache.py` - passed
- `git diff --stat` - shows this provider shutdown phase plus prior local cleanups and unrelated concurrent TaskCenter/test edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Summarize remaining local cleanup decisions, then move to cross-cutting naming renames only if the current dirty TaskCenter rename work is not an unsafe overlap.

## Phase 7.15 - Use lstat for Stale Staging Fence Metadata

Status: complete

Scope:
- Fix daemon L6 in `backend/src/sandbox/daemon/workspace_server.py`.
- Avoid following symlinks when reading stale-staging directory metadata.

Implementation notes:
- Replaced `child.stat().st_mtime` with `child.lstat().st_mtime`.
- The existing symlink skip remains in place.

Changed files:
- `backend/src/sandbox/daemon/workspace_server.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_daemon/test_stale_staging_fence.py -q` - 3 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `.venv/bin/ruff check backend/src/sandbox/daemon/workspace_server.py backend/tests/unit_test/test_sandbox/test_daemon/test_stale_staging_fence.py` - passed
- `git diff --stat` - shows this daemon fence hardening phase plus prior local cleanups and unrelated concurrent TaskCenter/test edits
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Finish with a remaining-item decision table and stop before cross-cutting naming renames if dirty TaskCenter changes remain.

## Phase 7.16 - Remaining Local Cleanup Decisions and Phase 8 Stop

Status: blocked for Phase 8; local cleanup pass complete for currently safe items

Scope:
- Classify remaining smaller-win/deep-dive items after S7-S10 and local hardening.
- Stop before cross-cutting naming renames if the dirty worktree makes that unsafe.

Implemented local cleanup items in this pass:
- `api/_impl/_payload.py:normalize_overlay_cwd` inline.
- `api/_impl/_payload.py:int_from_payload` docstring.
- `audit/translation.py` helper inline.
- `timing.py` stringified `TimingKey.*` audit fallback removal.
- `daemon/handler/health.py` dead shell-services probe removal.
- `daemon/_wire.py:committed_paths` fallback simplification.
- `audit/__init__.py` docstring correction.
- Provider H4, H5, H6.
- Daemon L6 `lstat` stale-staging hardening.

Remaining items intentionally deferred:
- `api/_control.py` passthrough fold: blocked by the public API import-boundary contract. Moving provider/host imports into `sandbox.api.__init__` already tripped `test_api_import_boundaries`; this requires an explicit API boundary decision.
- `api/transport.py` `DAEMON_OP_*` enum/constants: public-ish constants used by verb modules and tests; no meaningful LOC win, better handled in a naming/API pass.
- `host.versioned_payload`: verified not unused; `sandbox.api.transport` imports it for daemon protocol stamping.
- Host naming/runtime items (`_DaemonDispatchError` naming, lifecycle setup names, thin-client heredoc, Python candidate discovery, `_runtime_probe` malformed input): deferred because they are naming or runtime behavior changes, not safe local cleanup.
- `occ/service.py:_wrap_commit_result`: deferred because the review's direct-index suggestion is unsafe in current code. `TimingKey.COMMIT_TOTAL` is absent on CAS-exhaustion results, so direct indexing can introduce `KeyError`.
- Daemon server drain timeout, boot timestamp ownership, executor worker env config, PID lock owner message, overlay manager-cache change, and handler `__all__`: deferred as behavior/runtime hardening or cosmetic work needing separate focused tests.
- Execution contract split, round-tripper deletion, `execution/__init__` export trimming, and output-ref relocation: deferred because they touch serialization/public internal surfaces after the C2 collapse.
- OCC low-priority type/order/timing-policy/stager naming/benchmark items: deferred because they are behavior, naming, or benchmark decisions outside the local cleanup batch.
- Provider M3-M9 and L-items beyond H4-H6: deferred because they alter async/sync API behavior, import side effects, logging policy, timeout defaults, or provider initialization semantics.
- Layer-stack/plugin H3-H5/M14/M15 and minor L-items: deferred because they are larger lifecycle/plugin behavior changes, not local cleanup.

Phase 8 stop condition:
- Cross-cutting naming renames are not safe to start in the current dirty tree.
- Current dirty files include broad TaskCenter, `task_center_runner`, tool, and test edits unrelated to sandbox local cleanup.
- Phase 8 explicitly crosses sandbox callers/tests, including `task_center_runner/`, live/e2e, and shared tests. Starting mechanical renames now risks overwriting or entangling concurrent user work.

Decision needed before Phase 8:
- Either finish/commit/stash the current concurrent TaskCenter rename work, or explicitly authorize a sandbox-only naming subset that avoids every dirty non-sandbox caller path.

Tests and guards run for the stop decision:
- `git status --short`
- `git diff --name-only`

Next phase recommendation:
- Stop here until the dirty-overlap decision is resolved.

## Phase 8.1 - Aggressive Execution Overlay Contract Collapse

Status: complete

Scope:
- Collapse `OverlayShellRequest` and `OverlayCapture` into `sandbox.execution.contract`.
- Delete the redundant `sandbox.execution.overlay_request` and `sandbox.execution.overlay_result` modules.
- Remove dead overlay result-file round-trippers and the dead `OverlayPathChange.from_dict` parser.
- Keep daemon `overlay.run` behavior and the bundle/runtime boundary intact.

Implementation notes:
- `OverlayShellRequest.from_dict` remains because daemon `overlay.run` still decodes request payloads through that shape.
- `OverlayCapture.to_dict` remains because daemon `overlay.run` still returns that wire payload.
- `read_output_ref` was narrowed to a private `_read_output_ref` helper in `orchestrator.py`; the deleted public-ish execution module no longer carries a one-line helper.
- Runtime bundle boundary tests now require `execution/contract.py` as the request/result owner and reject the deleted `overlay_request.py` / `overlay_result.py` modules.

Changed files:
- `backend/src/sandbox/execution/contract.py`
- `backend/src/sandbox/execution/overlay_change.py`
- `backend/src/sandbox/execution/orchestrator.py`
- `backend/src/sandbox/daemon/handler/overlay.py`
- `backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py`
- `backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py`
- `backend/tests/unit_test/test_sandbox/test_overlay/test_snapshot_overlay_runner.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- `backend/src/sandbox/execution/overlay_request.py`
- `backend/src/sandbox/execution/overlay_result.py`

Compatibility shims:
- None kept for the deleted execution-internal modules.
- Public daemon `overlay.run` payload behavior is preserved through `OverlayShellRequest.from_dict` and `OverlayCapture.to_dict` in `sandbox.execution.contract`.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_overlay/test_snapshot_overlay_runner.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py backend/tests/unit_test/test_sandbox/test_daemon/test_overlay_capture.py -q` - 17 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py backend/tests/unit_test/test_sandbox/test_overlay/test_snapshot_overlay_runner.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py -q` - 19 passed after boundary-test update
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `.venv/bin/ruff check backend/src/sandbox/execution/contract.py backend/src/sandbox/execution/orchestrator.py backend/src/sandbox/execution/overlay_change.py backend/src/sandbox/daemon/handler/overlay.py backend/tests/unit_test/test_sandbox/test_overlay/test_snapshot_overlay_runner.py backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py` - passed
- `rg -n "from sandbox\\.execution\\.overlay_(request|result)|import sandbox\\.execution\\.overlay_(request|result)|sandbox\\.execution\\.overlay_(request|result)" backend/src backend/tests` - no hits
- `rg -n "OverlayPathChange\\.from_dict|OverlayCapture\\.from_dict|write_overlay_capture" backend/src/sandbox backend/tests/unit_test/test_sandbox` - no hits
- `git diff --stat` - phase-specific execution diff shows 98 insertions, 189 deletions
- `git diff --check` - clean

Failures and fixes:
- First full sandbox guard failed because `test_overlay_dependency_boundaries.py` still required `sandbox/execution/overlay_result.py` in the runtime bundle. Updated the architectural boundary assertion to require `sandbox/execution/contract.py` and reject both deleted modules, then reran the guard successfully.

Next phase recommendation:
- Continue the aggressive execution cleanup with the one-caller `workspace_capture.py` deletion, then consider whether `workspace_mount.py` can be folded without weakening strategy fallback diagnostics.

## Phase 8.2 - Delete One-Function Workspace Capture Wrapper

Status: complete

Scope:
- Delete `backend/src/sandbox/execution/workspace_capture.py`.
- Inline the copy-backed/private-namespace capture branch into `sandbox.execution.orchestrator`.
- Update direct command-exec tests to call `capture_changes` directly for copy-backed captures.
- Update runtime bundle boundary tests to reject the deleted wrapper.

Implementation notes:
- `execute_command` now calls `capture_changes` directly.
- Copy-backed runs pass `lowerdir` and `mounted_workspace_root` to preserve the old merged-view diff reconstruction.
- Private-namespace runs continue to capture `spec.upperdir` directly.
- Tests that exercise the strategy runner no longer import the wrapper; they call `capture_changes` with the same copy-backed arguments the orchestrator now uses.

Changed files:
- `backend/src/sandbox/execution/orchestrator.py`
- `backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py`
- `backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py`
- `backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- `backend/src/sandbox/execution/workspace_capture.py`

Compatibility shims:
- None kept. The wrapper was an execution-internal one-call module.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py backend/tests/unit_test/test_sandbox/test_overlay/test_snapshot_overlay_runner.py backend/tests/unit_test/test_sandbox/test_overlay/test_runtime_invoker_cleanup.py backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py -q` - 32 passed
- `.venv/bin/ruff check backend/src/sandbox/execution/orchestrator.py backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py` - passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "from sandbox\\.execution\\.workspace_capture|import sandbox\\.execution\\.workspace_capture|capture_workspace_upperdir|sandbox\\.execution\\.workspace_capture" backend/src backend/tests` - no hits
- `git diff --stat` - combined aggressive execution/report diff shows 167 insertions, 240 deletions
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Continue with execution serialization/name cleanup only if it can stay sandbox-local. The next highest-LOC aggressive target is `workspace_mount.py` / strategy dispatch folding, but it should first preserve or improve fallback diagnostics so failures do not lose namespace-child stderr context.

## Phase 8.3 - Fold Workspace Strategy Dispatcher Into Orchestrator

Status: complete

Scope:
- Delete `backend/src/sandbox/execution/workspace_mount.py`.
- Move `run_workspace_replaced_command` and mount-mode strategy selection into `sandbox.execution.orchestrator`.
- Preserve the public `sandbox.execution.run_workspace_replaced_command` facade.
- Preserve existing strategy fallback semantics and timing keys.

Implementation notes:
- `run_workspace_replaced_command` now lives next to `execute_command`, which is its only production orchestration owner.
- Default strategy order remains private namespace first, then copy-backed fallback.
- Explicit `mount_mode=MountMode.COPY_BACKED` and private namespace selection still construct the same single-strategy tuples.
- Existing tests still monkeypatch `shell_runner.run_workspace_replaced_command`; the import is preserved through the `sandbox.execution` facade.

Changed files:
- `backend/src/sandbox/execution/orchestrator.py`
- `backend/src/sandbox/execution/__init__.py`
- `backend/src/sandbox/daemon/handler/overlay.py`
- `backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py`
- `backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py`
- `backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- `backend/src/sandbox/execution/workspace_mount.py`

Compatibility shims:
- Kept `sandbox.execution.run_workspace_replaced_command` as the stable facade import.
- No shim kept for `sandbox.execution.workspace_mount`; the module was execution-internal and the bundle boundary now rejects it.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py backend/tests/unit_test/test_sandbox/test_command_exec/test_capture_to_occ_client.py backend/tests/unit_test/test_sandbox/test_overlay/test_snapshot_overlay_runner.py backend/tests/unit_test/test_sandbox/test_overlay/test_runtime_invoker_cleanup.py backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py backend/tests/unit_test/test_sandbox/test_daemon/test_overlay_capture.py backend/tests/unit_test/test_sandbox/test_occ/test_shell_capture_atomicity.py -q` - 37 passed
- `.venv/bin/ruff check backend/src/sandbox/execution/orchestrator.py backend/src/sandbox/execution/__init__.py backend/src/sandbox/daemon/handler/overlay.py backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py` - passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "from sandbox\\.execution\\.workspace_mount|import sandbox\\.execution\\.workspace_mount|sandbox\\.execution\\.workspace_mount" backend/src backend/tests` - no hits
- `git diff --stat` - combined aggressive execution/report diff shows 295 insertions, 344 deletions
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Stop before broader execution renames. The remaining large `execution/` cuts require either renaming public-ish modules (`entrypoints.py`, `workspace_environment.py`, `overlay_capture.py`) or changing copy-backed capture ownership; both should be separate decisioned phases because they can affect runtime bundle and live overlay probes.

## Phase 8.4 - Remove Redundant Namespace Input Restat

Status: complete

Scope:
- Remove the redundant `_assert_same_dir` helper from `backend/src/sandbox/execution/entrypoints.py`.
- Keep mount input validation through path text checks, symlink checks, directory checks, `O_NOFOLLOW | O_DIRECTORY`, and `/proc/self/fd/*` mount references.
- Preserve namespace helper payload and command execution behavior.

Implementation notes:
- The deleted helper performed a second `path.stat()` after opening the directory fd. The mount operation uses the fd-backed `/proc/self/fd/*` paths, so the post-open path restat did not strengthen the fd guarantee and left a misleading TOCTOU-shaped check in the code.
- `_validate_mount_inputs` now opens validated directory fds and returns those fd-backed paths directly.

Changed files:
- `backend/src/sandbox/execution/entrypoints.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed. Removed helper was private to the namespace child module.

Tests and guards run:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py backend/tests/unit_test/test_sandbox/test_overlay/test_runtime_invoker_cleanup.py backend/tests/unit_test/test_sandbox/test_overlay/test_snapshot_overlay_runner.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py -q` - 29 passed
- `.venv/bin/ruff check backend/src/sandbox/execution/entrypoints.py` - passed
- `rg -n "_assert_same_dir|mount input changed during validation" backend/src/sandbox/execution backend/tests/unit_test/test_sandbox` - no hits
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `git diff --stat` - combined aggressive execution/report diff shows 341 insertions, 355 deletions
- `git diff --check` - clean

Failures and fixes:
- None

Next phase recommendation:
- Stop before the next aggressive execution reductions. Remaining high-LOC cuts are not simple removals: `entrypoints.py` and `workspace_environment.py` are rename/runtime-shape changes, `overlay_capture.py` needs a copy-backed capture ownership decision, and `execution/__init__.py` export trimming is a public-ish facade decision.

## Phase 9.1 - Shared Chunked Base64 Upload Helper

Status: complete

Scope:
- Extract the duplicated chunked base64 `printf | base64 -d >> remote` upload loop used by runtime bundle upload and plugin install.
- Keep the existing transport behavior, chunk size, command shape, per-chunk timeout, and caller-specific error handling.
- Do not change runtime bundle finalization, plugin setup, marker writes, or staging cleanup semantics.

Implementation notes:
- Added `sandbox.host.chunked_upload.write_base64_chunks`.
- `host/runtime_bundle.py` still creates the runtime staging tarball first, writes base64 chunks into that tarball, extracts it, removes staging, and writes `.bundle-hash`.
- `plugin/install.py` still acquires the lock, creates its staging dir/tarball, writes base64 chunks into the tarball, extracts, publishes, optionally runs trusted `setup.sh`, writes the marker, and cleans up.
- The helper centralizes only the append-chunk loop and delegates result validation back to each caller.

Changed files:
- `backend/src/sandbox/host/chunked_upload.py`
- `backend/src/sandbox/host/runtime_bundle.py`
- `backend/src/sandbox/plugin/install.py`
- `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`

Deleted files:
- None

Compatibility shims:
- None needed. This is a host-side internal helper.

Tests and guards run:
- `.venv/bin/ruff check backend/src/sandbox/host/chunked_upload.py backend/src/sandbox/host/runtime_bundle.py backend/src/sandbox/plugin/install.py` - passed after removing stale imports/type names
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_daemon/test_bundle.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py backend/tests/unit_test/test_sandbox/test_plugin_install.py -q` - 22 passed
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` - 547 passed, 1 skipped, 1 expected deprecation warning
- `rg -n "base64\\.b64encode|_CHUNK_SIZE =|for .*range\\(0, len\\(encoded\\)" backend/src/sandbox/host backend/src/sandbox/plugin` - only `sandbox.host.chunked_upload` owns the chunking loop now
- `git diff --stat` - current tracked diff shows this phase plus prior execution/report changes; new helper is untracked until staged
- `git diff --check` - clean

Failures and fixes:
- First ruff pass caught stale `RawExecResult`, `Protocol`, and `_RawExecCallable` references after the extraction. Removed/replaced them and reran successfully.

Next phase recommendation:
- Continue behavior-preserving reductions with provider Daytona client cache deduplication or plugin handler state consolidation. Avoid host upload-overlap removal and copy-backed capture ownership changes unless explicitly switching to behavior-bearing cleanup.
