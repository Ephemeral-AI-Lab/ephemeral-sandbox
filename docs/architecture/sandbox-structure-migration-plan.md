# Sandbox Structure Migration Plan

## Goal

Reorganize `backend/src/sandbox` so the package layout reflects ownership,
runtime location, and dependency direction without changing observable
behavior. The first pass is structural only: move files with `git mv`, update
imports mechanically, and keep logic changes for later commits.

## Implementation Status

Implemented in the current tree.

- Move-plan items 1-12 are complete: contracts moved to `sandbox/contracts.py`,
  status flattened to `sandbox/api/status.py`, host folders flattened,
  root foundation helpers moved, the resident daemon moved to
  `sandbox/runtime/daemon`, `command_exec`, `layer_stack`, and `occ` were
  regrouped, `providers` was renamed to `provider`, overlay CLI ownership moved
  to `overlay/cli.py`, and generated sandbox state was removed.
- OCC-owned projection and capture conversion now live under OCC:
  `sandbox/occ/result_projection.py` and `sandbox/occ/capture/overlay.py`.
  The old top-level `sandbox/occ/overlay_capture.py` facade was removed because
  production code no longer imported it.
- Host-side runtime setup now uses provider adapter exec and
  `sandbox.host.daemon_client`; it no longer imports the public `sandbox.api`
  tool internals.
- `sandbox/testing/` was deleted after confirming there were no in-repo callers;
  old eval-sandbox fixtures should stay test-local if needed again.

## Naming Convention

- Package and module names use `snake_case`.
- Folders use responsibility nouns, not buckets: `provider`, `handler`,
  `service`, `contract`, `workspace`, `manifest`, `merge`, `capture`.
- Avoid vague buckets: no `utils`, `helpers`, `common`, or `misc`.
- Prefer singular folders when the folder names a layer or role:
  `provider`, `handler`, `service`.
- Keep established domain names when they carry architecture meaning:
  `layer_stack`, `command_exec`, `occ`, `overlay`.
- Keep acronyms lowercase in paths: `api`, `rpc`, `occ`.
- Private implementation modules may keep leading underscores only when they
  are intentionally hidden behind a package facade.

## Target Structure

```text
sandbox/
  __init__.py
  contracts.py
  async_bridge.py
  bash.py

  api/
    __init__.py
    facade.py
    status.py
    tool/
      __init__.py
      _daemon_client.py
      read.py
      write.py
      edit.py
      shell.py
      raw_exec.py

  host/
    __init__.py
    runtime_bundle.py
    daemon_client.py
    context.py
    git.py
    recovery.py
    setup.py
    workspace.py

  runtime/
    __init__.py
    daemon/
      __init__.py
      __main__.py
      rpc/
        __init__.py
        dispatcher.py
        server.py
      handler/
        __init__.py
        request_context.py
        read.py
        write.py
        edit.py
        shell.py
        health.py
        metrics.py
        workspace.py
      service/
        __init__.py
        layer_stack_client.py
        occ_backend.py
        shell_runner.py
        workspace_binding.py
        workspace_server.py

  command_exec/
    __init__.py
    contract/
      __init__.py
      request.py
      result.py
      ports.py
    workspace/
      __init__.py
      environment.py
      mount.py
      namespace_entrypoint.py
      capture.py

  layer_stack/
    __init__.py
    manager.py
    manifest/
      __init__.py
      model.py
      store.py
    layer/
      __init__.py
      change.py
      index.py
      publisher.py
    view/
      __init__.py
      merged.py
    lease/
      __init__.py
      registry.py
    commit/
      __init__.py
      staging.py
    maintenance/
      __init__.py
      squash.py
    workspace/
      __init__.py
      binding.py
      base.py

  occ/
    __init__.py
    client.py
    service.py
    ports.py
    commit_transaction.py
    result_projection.py
    changeset/
      __init__.py
      builders.py
      prepared.py
      types.py
    routing/
      __init__.py
      orchestrator.py
      single_path.py
      runtime_ops.py
    merge/
      __init__.py
      direct.py
      gated.py
      serial.py
    content/
      __init__.py
      gitignore_oracle.py
      hashing.py
      layer_backed.py
    capture/
      __init__.py
      overlay.py

  overlay/
    __init__.py
    cli.py
    capture/
      __init__.py
      changes.py
      types.py
      upperdir.py
    namespace/
      __init__.py
      command.py
      mounts.py
    runner/
      __init__.py
      runtime_invoker.py
      snapshot_overlay_runner.py

  provider/
    __init__.py
    protocol.py
    registry.py
    daytona/
      __init__.py
      adapter.py
      bootstrap.py
      context.py
      errors.py
      client/
        __init__.py
        async_client.py
        sync_client.py
        shutdown.py
        credentials.py
```

## Directory Responsibilities

- `contracts.py`: shared sandbox DTOs used across `api`, `host`, `provider`,
  and runtime code. It must not import other sandbox layers.
- `api/`: the public in-repo entry point for tools, routes, and app code.
- `host/`: orchestrator-side setup, bundle upload, provider-backed daemon RPC,
  workspace discovery, and recovery.
- `runtime/daemon/`: resident in-sandbox daemon server, dispatch, handlers, and
  daemon-local services.
- `command_exec/`: guarded workspace-replaced command execution. It owns command
  request/result contracts, workspace mount execution, namespace entrypoint, and
  upperdir capture.
- `layer_stack/`: append-only layer storage, manifests, leases, merged reads,
  staging, squash, and workspace base/binding.
- `occ/`: optimistic concurrency preparation, routing, merge validation,
  serial commit coordination, result projection, and capture-to-OCC conversion.
- `overlay/`: policy-blind filesystem overlay capture and snapshot runner.
- `provider/`: external sandbox provider adapters and provider registry.

## Dependency Direction

The import graph should be a DAG:

```text
contracts, async_bridge, bash
  <- layer_stack
  <- overlay
  <- occ
  <- command_exec
  <- runtime.daemon
  <- provider
  <- host
  <- api
  <- external callers
```

Rules:

- `layer_stack` must not import `occ`, `overlay`, `command_exec`, `runtime`,
  `host`, `api`, or `provider`.
- `overlay` may import `layer_stack` and foundation modules, but not `occ`,
  `api`, `host`, or provider modules.
- `occ` may import `layer_stack`, `overlay.capture`, and foundation modules,
  but not `runtime.daemon`, `host`, `api`, or provider modules.
- `command_exec` may import its own contracts, `overlay.capture`, and OCC
  ports/types. It must not import concrete `OccService`, commit internals, or
  daemon services.
- `runtime.daemon` may compose `command_exec`, `occ`, `overlay`, and
  `layer_stack`. It must not import Daytona or any provider implementation.
- `host` may import `provider`, `contracts`, and foundation modules. It must
  not import `api` to avoid host-to-public-surface cycles.
- `provider` may import `contracts`, `bash`, and foundation modules. It must
  not import `api` or `host`.
- `api` is the top-level facade and may call `host` and provider registry
  surfaces.

## Focused Grouping

### `command_exec`

Current issues:

- `clients.py` names protocol ports as clients.
- `env.py`, `workspace_mount.py`, and `namespace_helper.py` are all part of the
  same workspace replacement workflow but are split flatly.
- `capture/changeset.py` converts captured filesystem changes into OCC changes,
  so it belongs under `occ/capture`, not `command_exec`.
- `namespace_helper.py` uses helper naming even though it is an entrypoint.

Target grouping:

```text
command_exec/
  contract/request.py
  contract/result.py
  contract/ports.py
  workspace/environment.py
  workspace/mount.py
  workspace/namespace_entrypoint.py
  workspace/capture.py
```

Workflow:

```text
CommandExecRequest
  -> workspace.environment resolves cwd/env
  -> workspace.mount runs command against replaced workspace
  -> workspace.namespace_entrypoint performs private mount namespace work
  -> workspace.capture captures upperdir changes
  -> occ.capture.overlay converts changes to OCC mutations
```

### `layer_stack`

Current issues:

- Storage concepts are mostly flat, so readers must infer which files belong to
  manifest state, immutable layer content, views, leases, staging, maintenance,
  or workspace import.
- `workspace.py` and `workspace_base.py` change together but are separated only
  by filename.
- `stack_manager.py` should be the package facade, but its name is noisier than
  `manager.py` inside the `layer_stack` package.

Target grouping:

```text
layer_stack/
  manager.py
  manifest/{model.py,store.py}
  layer/{change.py,index.py,publisher.py}
  view/merged.py
  lease/registry.py
  commit/staging.py
  maintenance/squash.py
  workspace/{binding.py,base.py}
```

Responsibilities:

```text
manifest/     active stack state and manifest persistence
layer/        immutable layer changes, indexes, and publishing
view/         newest-first read/materialized filesystem view
lease/        pinned snapshot lease lifecycle
commit/       commit staging values
maintenance/ stack depth control and checkpoint/squash work
workspace/   real workspace binding and base import
```

## Move Plan

Each item should be a separate commit.

1. Move shared contracts out of API internals.
   - `sandbox/api/utils/models.py -> sandbox/contracts.py`
   - Update `sandbox.api.__init__` to re-export the same public DTO names.
   - Remove `sandbox/api/utils/`.

2. Flatten public API status.
   - `sandbox/api/status/__init__.py -> sandbox/api/status.py`
   - Keep `from sandbox.api import status` working.

3. Flatten host package and remove generic `ops`, `deploy`, and `rpc` folders.
   - `sandbox/host/deploy/bundle.py -> sandbox/host/runtime_bundle.py`
   - `sandbox/host/rpc/client.py -> sandbox/host/daemon_client.py`
   - `sandbox/host/ops/*.py -> sandbox/host/*.py`

4. Move foundation helpers to named root modules.
   - `sandbox/utils/async_bridge.py -> sandbox/async_bridge.py`
   - Remove `sandbox/utils/`.

5. Move resident daemon under runtime.
   - `sandbox/daemon/rpc/* -> sandbox/runtime/daemon/rpc/*`
   - `sandbox/daemon/handlers/* -> sandbox/runtime/daemon/handler/*`
   - `sandbox/daemon/services/* -> sandbox/runtime/daemon/service/*`
   - `sandbox/daemon/__main__.py -> sandbox/runtime/daemon/__main__.py`

6. Move and regroup `command_exec`.
   - `request.py -> contract/request.py`
   - `result.py -> contract/result.py`
   - `clients.py -> contract/ports.py`
   - `env.py -> workspace/environment.py`
   - `workspace_mount.py -> workspace/mount.py`
   - `namespace_helper.py -> workspace/namespace_entrypoint.py`
   - `capture/upperdir.py -> workspace/capture.py`

7. Move OCC-owned capture conversion out of `command_exec`.
   - `command_exec/capture/changeset.py -> occ/capture/overlay.py`
   - Reuse this from command-exec and any overlay-capture bridge.

8. Regroup `layer_stack`.
   - `stack_manager.py -> manager.py`
   - `changes.py -> layer/change.py`
   - `layer_index.py -> layer/index.py`
   - `publisher.py -> layer/publisher.py`
   - `manifest.py -> manifest/model.py` plus `manifest/store.py`
   - `merged_view.py -> view/merged.py`
   - `lease_registry.py -> lease/registry.py`
   - `staging.py -> commit/staging.py`
   - `squash.py -> maintenance/squash.py`
   - `workspace.py -> workspace/binding.py`
   - `workspace_base.py -> workspace/base.py`

9. Regroup `occ`.
   - `orchestrator.py -> routing/orchestrator.py`
   - `single_path_prepare.py -> routing/single_path.py`
   - `runtime_ops.py -> routing/runtime_ops.py`
   - `direct/merge.py -> merge/direct.py`
   - `gated/merge.py -> merge/gated.py`
   - `serial_merger.py -> merge/serial.py`
   - `content/layer_backed_content.py -> content/layer_backed.py`
   - Keep `commit_transaction.py` top-level until it is split by behavior in a
     later non-move commit.

10. Rename provider package to singular.
    - `providers -> provider`
    - Keep `provider/daytona/client/*` with explicit module names:
      `async_client.py`, `sync_client.py`, `shutdown.py`, `credentials.py`.

11. Move overlay CLI to overlay ownership.
    - `daemon/overlay_shell/cli.py -> overlay/cli.py`
    - Keep daemon dispatch handlers under `runtime/daemon/handler`.

12. Remove generated and stale source-tree artifacts.
    - Remove `sandbox/**/__pycache__/`.
    - Remove `sandbox/.omc/state/` from the source tree if it is untracked
      generated state.
    - Remove empty leftover directories after confirming they are untracked:
      `occ/handlers`, `occ/patching`, and stale empty routing/merge leftovers.

## Public Entry Points

External production callers should import only:

```python
from sandbox.api import api
from sandbox.api import ReadFileRequest, WriteFileRequest, EditFileRequest, ShellRequest
```

Layer-internal tests may import deeper modules when they are testing that layer
directly. Production code outside `sandbox/` should not import `runtime`,
`layer_stack`, `occ`, `overlay`, `command_exec`, or `provider.daytona`
directly unless it is a startup/bootstrap integration point explicitly
allowlisted by an import-fence test.

## Deferred Logic Refactors

Do not mix these with move commits:

- Remove duplicate capture-to-OCC conversion after the moved modules are green.
- Re-audit pass-through parameters after import churn is complete.
- Split `occ/commit_transaction.py` only after behavior tests are green; it is
  large but central and currently has user-owned dirty changes.

## Verification Plan

Run the narrowest checks after each commit:

```bash
uv run pytest backend/tests/unit_test/test_sandbox -q
uv run pytest backend/tests/unit_test/test_sandbox/test_import_fence.py -q
uv run pytest backend/tests/unit_test/test_sandbox/test_api -q
uv run pytest backend/tests/unit_test/test_sandbox/test_daemon -q
uv run pytest backend/tests/unit_test/test_sandbox/test_command_exec -q
uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack -q
uv run pytest backend/tests/unit_test/test_sandbox/test_occ -q
uv run pytest backend/tests/unit_test/test_sandbox/test_overlay -q
uv run ruff check backend/src/sandbox backend/tests/unit_test/test_sandbox
```

Live/e2e suites are not part of the default move loop because they require
external sandbox services. Run them only for runtime-bundle, daemon transport,
or public sandbox API behavior changes.

## Revert Discipline

- One concern per commit.
- Structure moves first, logic cleanup second.
- If a structural commit breaks behavior tests, revert that commit and redo the
  import update rather than patching behavior.
- Do not touch existing dirty files unless they are in the confirmed move set
  and the user has approved the migration execution.
