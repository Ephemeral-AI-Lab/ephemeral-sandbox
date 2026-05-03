# Step 6 — Slice 5b — Overlay peer relocation

**Goal.** Move overlay under `sandbox/overlay/`; add Overlay's `client.py`
route point and `setup.sh`; register its setup and handlers with the runtime at
import time. Introduce `shell_pipeline` as the only place overlay and OCC
compose, remove the old `sandbox/code_intelligence/` source package, and keep
the shell result surface aligned with write/edit: changed files plus conflict.
With 5a having reshaped the seam (overlay = pure upperdir capture; OCC =
merge-policy decider), 5b is mostly structural — relocate, rewire dispatch
through `runtime/server.py`, enforce peer-isolation.

**Depends on.** Step 1 / Slice 5a and Step 5 / Slice 4 (both must be merged green first).

## Files

### Move
- `backend/src/sandbox/code_intelligence/overlay/` → `backend/src/sandbox/overlay/`.

### Target package layout

The relocated overlay peer should be organized by boundary, not by the old
temporary shim filenames. Root-level files are package entrypoints, host route
points, contracts, wire helpers, and configuration only. Runtime-server adapters
live under `handlers/`. Sandbox-side execution internals live under `runtime/`.

```
backend/src/sandbox/
  overlay/
    __init__.py                 # package marker only
    bootstrap.py                # registers setup + ops
    setup.sh                    # idempotent overlay setup
    client.py                   # host-side route; one adapter.exec
    engine/                     # OverlayEngine / LocalOverlayEngine split by role
      __init__.py               # public re-exports
      constants.py              # shared knobs/constants
      fingerprint.py            # workspace lowerdir guard helpers
      helpers.py                # command encoding / samples
      local.py                  # LocalOverlayEngine orchestration
      protocol.py               # OverlayEngine Protocol
      readback.py               # stdout/diff/envelope cleanup + timing
      runner.py                 # runtime upload and command execution
      runtime_bundle.py         # local overlay runtime tarball builder
    types.py                    # OverlayRunOutcome, UpperChange, ConflictInfo
    wire.py                     # JSON/base64 encode/decode
    config.py                   # env knobs only

    handlers/
      __init__.py               # register_handlers()
      run.py                    # OP: overlay.run
      shell.py                  # OP: shell -> runtime.pipelines.shell_pipeline

    runtime/
      __init__.py
      cli.py                    # script entrypoint, replaces old run.py facade
      mounts.py                 # namespace + overlay mounts
      capture.py                # walk upperdir, build UpperChange
      command.py                # run user command, stdout handling
      ndjson.py                 # diff/result file IO
      types.py                  # sandbox-local runtime-only types
```

### Modify
- `sandbox/runtime/pipelines.py::shell_pipeline`:
  - Call `overlay.run` first.
  - On overlay reject: short-circuit. No OCC call. Return `ShellResult` with `conflict` populated.
  - On overlay success: call `occ.apply_changeset` with `upper_changes`.
    Project the OCC verdict onto `ShellResult` as only
    `changed_paths` plus `conflict`. Do not expose routing partitions from this
    boundary.
- `sandbox/runtime/server.py`: import `sandbox.overlay.bootstrap` / handlers so overlay ops register in `OP_TABLE`; the `shell` op now dispatches to `shell_pipeline`. Server dispatch remains table-driven; no per-overlay branch is added.
- `sandbox/runtime/shell_command_executor.py`: host-side shell compatibility
  adapter for service callers. It must not live under `sandbox/code_intelligence`.

### Delete
- `backend/src/sandbox/code_intelligence/overlay/` (after move).
- `backend/src/sandbox/code_intelligence/` after the remaining facade/backend
  files have moved to `sandbox/runtime/`.
- `backend/src/sandbox/overlay/process_exec.py` if it was carried over by the
  directory move. Its host-side request routing belongs in `overlay/client.py`;
  bundle upload/setup belongs in `runtime/bundle.py`, `runtime/setup_orchestrator.py`,
  and `overlay/setup.sh`.
- `backend/src/sandbox/overlay/daemon_local.py` if it was carried over by the
  directory move. Its in-sandbox execution/read-diff/cleanup responsibilities
  belong in `overlay/engine/`, `overlay/handlers/run.py`, and
  `overlay/runtime/{command,ndjson}.py` behind `runtime/server.py`.
- `backend/src/sandbox/overlay/capture_runner.py` if it was carried over by the
  directory move. The durable split is `overlay/engine/` for orchestration,
  `overlay/runtime/capture.py` for upperdir capture, and `overlay/client.py`
  for host routing.
- `backend/src/sandbox/overlay/run.py` if it was carried over by the directory
  move. Its stable replacement is `overlay/runtime/cli.py`.

## Implementation tasks

1. `git mv` overlay → `sandbox/overlay/`, then immediately reshape it into the
   target layout above. Update imports to the new package paths.
2. Extract `OverlayEngine` Protocol and `LocalOverlayEngine` under
   `overlay/engine/`. The engine owns one overlay run's orchestration:
   lease creation/cleanup, timing, setup invocation, and call into the
   sandbox-side runtime. Split IO/readback/upload helpers into separate files;
   do not keep a single god file. The engine package does not import OCC.
3. Implement `OverlayClient`. It owns all host-side overlay/shell request
   routing and is the only place outside `runtime/` that constructs overlay
   server envelopes. It does not import OCC.
4. Add `overlay/setup.sh` and make `overlay/bootstrap.py` register it with
   `runtime/setup_orchestrator.py`. Keep setup idempotent; mount/upperdir setup
   belongs here, while user command execution stays under `overlay/runtime/`.
5. Implement `shell_pipeline` per §1.5. The pipeline does not classify — it
   forwards `upper_changes` to `occ.apply_changeset` and projects the verdict
   onto `ShellResult.changed_paths` and `ShellResult.conflict`. Any
   classification or merge-policy import in `runtime/pipelines.py` is a
   structural review red flag. Returning routing-specific fields is also a
   review failure.
6. Register overlay handlers in `server.OP_TABLE` at module import time:
   - `overlay.run` → `overlay/handlers/run.py`.
   - `shell` → `overlay/handlers/shell.py`, which delegates to
     `runtime.pipelines.shell_pipeline`.
7. Add lint allowlist tests:
   - `from sandbox.occ` is forbidden inside `sandbox/overlay/`.
   - `from sandbox.overlay` is forbidden inside `sandbox/occ/`.
8. Split the Step 1 temporary execution shims:
   - `process_exec.py` host-side request/envelope logic → `overlay/client.py`;
     setup/upload logic → `runtime/bundle.py`, `runtime/setup_orchestrator.py`,
     and `overlay/setup.sh`.
   - `daemon_local.py` in-sandbox run/read-diff/cleanup logic →
     `overlay/engine/`, `overlay/handlers/run.py`, and
     `overlay/runtime/{command,ndjson}.py`.
   - `capture_runner.py` orchestration → `overlay/engine/`; upperdir walk →
     `overlay/runtime/capture.py`.
   - `run.py` script facade → `overlay/runtime/cli.py`.
   - Delete the shim files after those responsibilities are covered. Do not
     preserve old names as compatibility wrappers inside the new package.
9. Confirm 5a's reshaped overlay package transplants cleanly to
   `sandbox/overlay/` with no remaining classification surfaces.

## Tests

- All overlay tests pass at the new path.
- New `test_sandbox/test_overlay/test_client.py`:
  - `OverlayClient` performs exactly one adapter exec per request.
  - `OverlayClient` serializes requests to `runtime/server.py` rather than
    reaching into handlers directly.
  - `OverlayClient` does not import `sandbox.occ`.
- New `test_sandbox/test_overlay/test_bootstrap.py`:
  - `overlay/bootstrap.py` registers `overlay/setup.sh` with the setup orchestrator.
  - repeated setup registration/execution is idempotent.
- New `test_sandbox/test_overlay/test_package_structure.py`:
  - root `sandbox/overlay/` contains only the files and directories in the
    target layout above.
  - `process_exec.py`, `daemon_local.py`, `capture_runner.py`, and `run.py` do
    not exist in `sandbox/overlay/`.
  - no import in `sandbox/overlay/` reaches into `sandbox.occ.*`.
- New `test_sandbox/test_overlay/test_wire.py`:
  - `UpperChange` bytes round-trip through JSON/base64 encoding unchanged.
  - overlay reject and success outcomes decode to typed result objects.
- New `test_sandbox/test_overlay/test_runtime_capture.py`:
  - upperdir walk emits raw `UpperChange` records with no git classification.
- New `test_sandbox/test_overlay/test_runtime_command.py`:
  - user command stdout/stderr capture and exit-code preservation remain
    equivalent to the old runner path.
- New `test_sandbox/test_runtime/test_shell_pipeline.py`:
  - **One wire trip per shell op** — assert exactly one `adapter.exec` invocation per pipeline call.
  - Overlay reject → no `occ.apply_changeset` invocation; `ShellResult.conflict` populated.
  - Overlay success → OCC applies the successful file set; `ShellResult.changed_paths`
    contains the files actually changed, without exposing routing partitions.
- Lint allowlist test: peer-isolation invariants enforced.

## Exit criteria

- Build / ruff / tests green.
- `backend/src/sandbox/code_intelligence/` no longer exists.
- `sandbox/overlay/client.py` is the only host-side route for overlay/shell
  server requests.
- `sandbox/overlay/setup.sh` is registered through `overlay/bootstrap.py`.
- `sandbox/overlay/` follows the target package layout in this document.
- `sandbox/overlay/engine.py` no longer exists; `sandbox/overlay/engine/` owns
  the split implementation.
- `process_exec.py` and `daemon_local.py` do not exist under
  `sandbox/overlay/`; their responsibilities are represented by
  `overlay/client.py`, `overlay/engine/`, `overlay/handlers/run.py`,
  `overlay/runtime/{command,ndjson}.py`, and the runtime setup files.
- `capture_runner.py` and `run.py` do not exist under `sandbox/overlay/`;
  their responsibilities are represented by `overlay/engine/`,
  `overlay/runtime/capture.py`, and `overlay/runtime/cli.py`.
- Peer-isolation lint test passes (overlay ↔ OCC mutual non-import).
- One-wire-trip assertion holds for every `shell_pipeline` test.

## Risks

- A refactor mistake reintroduces a second wire trip. Mitigation: explicit one-wire-trip-per-op assertion in pipeline tests.
- `shell_pipeline` accidentally re-introduces classification at the seam.
  Mitigation: peer-isolation lint forbids classification imports inside
  `sandbox/overlay/` and inside `runtime/pipelines.py`; reviewers reject any
  classification helper added in this slice.
- 5a's lifted helpers (`direct_merge_factory`, `narrow_prune_opaque_factory`) live under `mutations/` post-5a; 5b's OCC relocation (Slice 4, already merged) means they're now at `sandbox/occ/`. Mitigation: confirm via grep at the start of 5b that the helpers landed in OCC's tree and are not still imported from the old `code_intelligence/mutations/` path before relocating overlay.
- `OverlayClient` becomes a second public API. Mitigation: importer allowlist
  permits it only from `sandbox.api.shell`, runtime tests, and temporary
  migration shims; agent tools still import only `sandbox.api.*`.
