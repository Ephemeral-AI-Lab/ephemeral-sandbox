# Step 6 / Slice 5b - Implementation Report

Companion to
[`step-06-slice-5b-overlay-relocation.md`](./step-06-slice-5b-overlay-relocation.md).
This report records the Overlay peer relocation deliverable, the runtime
composition now present in the checkout, and the current verification status.

---

## 1. Verdict

**Step 6 is structurally implemented: Overlay now lives as a peer under
`sandbox/overlay/`, registers through the generic runtime server, and composes
with OCC only in `sandbox/runtime/pipelines.py`.**

The slice moved the capture-only overlay runtime out of the old
`sandbox/code_intelligence/` package, added the `OverlayClient` host route
point, registered `overlay.run` and `shell` handlers through
`overlay/bootstrap.py`, and implemented `shell_pipeline` as the single
overlay-to-OCC composition boundary.

**Verification is currently partial, not green.** The core relocation,
bootstrap, wire, capture, command, and shell-pipeline tests pass. The broader
focused overlay/runtime command-executor suite still has stale `transport=`
constructor expectations and fails before exercising the intended behavior.

---

## 2. File Inventory

### Added Overlay Runtime Peer Files

| File | Purpose |
| --- | --- |
| `backend/src/sandbox/overlay/bootstrap.py` | Registers Overlay setup and runtime handlers at import time |
| `backend/src/sandbox/overlay/client.py` | Host-side typed route point for `overlay.run` and `shell` runtime ops |
| `backend/src/sandbox/overlay/setup.sh` | Idempotent peer setup script for `/tmp/eos-shell-overlay` |
| `backend/src/sandbox/overlay/types.py` | Overlay capture, shell result, upper-change, and conflict dataclasses |
| `backend/src/sandbox/overlay/wire.py` | JSON/base64 wire helpers for overlay outcomes and shell results |
| `backend/src/sandbox/overlay/config.py` | Overlay environment knobs |

### Split Overlay Engine Package

| New Path | Responsibility |
| --- | --- |
| `backend/src/sandbox/overlay/engine/protocol.py` | `OverlayEngine` Protocol |
| `backend/src/sandbox/overlay/engine/local.py` | `LocalOverlayEngine` orchestration root |
| `backend/src/sandbox/overlay/engine/runner.py` | Runtime upload and command execution helpers |
| `backend/src/sandbox/overlay/engine/readback.py` | Stdout, diff, result-envelope, cleanup, and timing readback |
| `backend/src/sandbox/overlay/engine/runtime_bundle.py` | Capture-runtime tarball builder |
| `backend/src/sandbox/overlay/engine/fingerprint.py` | Local lowerdir freshness guard helpers |
| `backend/src/sandbox/overlay/engine/helpers.py` | Command encoding and log sampling helpers |
| `backend/src/sandbox/overlay/engine/constants.py` | Shared overlay constants |

### Sandbox-Side Overlay Runtime

| New Path | Responsibility |
| --- | --- |
| `backend/src/sandbox/overlay/runtime/cli.py` | Sandbox-side script entrypoint replacing the old runner facade |
| `backend/src/sandbox/overlay/runtime/mounts.py` | Overlay namespace and mount setup |
| `backend/src/sandbox/overlay/runtime/capture.py` | Raw upperdir walk and `UpperChange` construction |
| `backend/src/sandbox/overlay/runtime/command.py` | User command execution and stdout capture |
| `backend/src/sandbox/overlay/runtime/ndjson.py` | Diff/result file writing |
| `backend/src/sandbox/overlay/runtime/types.py` | Runtime-local capture types |

### Runtime Handler Files

| File | Change |
| --- | --- |
| `backend/src/sandbox/overlay/handlers/__init__.py` | Registers `overlay.run` and `shell` in `server.OP_TABLE` idempotently |
| `backend/src/sandbox/overlay/handlers/run.py` | Thin adapter for raw overlay capture requests |
| `backend/src/sandbox/overlay/handlers/shell.py` | Thin adapter for OCC-gated shell requests through `shell_pipeline` |

### Updated Existing Files

| File | Change |
| --- | --- |
| `backend/src/sandbox/runtime/server.py` | Imports `sandbox.overlay.bootstrap` alongside OCC bootstrap; dispatch stays OP_TABLE-based |
| `backend/src/sandbox/runtime/pipelines.py` | Implements `shell_pipeline` as overlay capture followed by OCC changeset application |
| `backend/src/sandbox/runtime/bundle.py` | Bundles `sandbox/overlay/**/*.py` and peer `setup.sh` |
| `backend/src/sandbox/runtime/shell_command_executor.py` | Hosts service-caller shell compatibility outside `sandbox/code_intelligence/` |

### Deleted Legacy Overlay Surface

| Legacy Surface | Replacement |
| --- | --- |
| `backend/src/sandbox/code_intelligence/` | Removed from the source tree |
| `sandbox/code_intelligence/overlay/process_exec.py` | Replaced by `sandbox/overlay/client.py` plus runtime dispatch helpers |
| `sandbox/code_intelligence/overlay/daemon_local.py` | Split into `overlay/engine/`, handlers, and sandbox runtime modules |
| `sandbox/code_intelligence/overlay/capture_runner.py` | Split into `overlay/engine/` and `overlay/runtime/capture.py` |
| `sandbox/code_intelligence/overlay/run.py` | Replaced by `overlay/runtime/cli.py` |

---

## 3. Behavior Delivered

### Overlay Host Route

`OverlayClient` is the host-side route point for Overlay runtime requests. It
serializes a JSON envelope to:

```text
python3 -m sandbox.runtime.server '<json-envelope>'
```

and receives typed `OverlayRunOutcome` / `ShellResult` values through
`sandbox.overlay.wire`. The client imports neither OCC nor Overlay handlers.

### Runtime Registration

`sandbox/runtime/server.py` remains generic. Overlay behavior is registered
through bootstrap:

```text
import sandbox.overlay.bootstrap
  -> setup_orchestrator.register(SetupScript(...sandbox/overlay/setup.sh))
  -> sandbox.overlay.handlers.register_handlers()
  -> OP_TABLE["overlay.run"] = run.handle
  -> OP_TABLE["shell"] = shell.handle
```

No overlay-specific branch was added to the dispatcher.

### Shell Pipeline Boundary

`shell_pipeline` is now the only production composition point between Overlay
and OCC:

1. run the command through `OverlayEngine.execute`
2. if Overlay rejects, return `ShellResult` with `conflict` and skip OCC
3. if Overlay succeeds, pass `upper_changes` to `OCC.apply_changeset`
4. project OCC's verdict onto `ShellResult.changed_paths` and `conflict`

The boundary does not expose routing partitions as public shell result fields.
Classification remains OCC-owned; Overlay emits raw upperdir records only.

### Capture Runtime

The sandbox-side runtime bundle is capture-only. It contains the CLI, mount,
capture, command, NDJSON, and runtime type modules. It does not carry classifier,
gitignore, or direct-routing runtime modules.

### Service Compatibility

`AuditedCommandExecutor` now lives under `sandbox/runtime/` and routes service
commands through `shell_pipeline`. In-process service callers can provide a
cached `LocalOverlayEngine` and a `WriteCoordinator.apply_changeset` adapter.

---

## 4. Boundaries Preserved

- `sandbox/overlay/` does not import `sandbox.occ.*`.
- `sandbox/occ/` does not import `sandbox.overlay.*`.
- `runtime/pipelines.py` is the intentional peer composition layer and imports
  both peers.
- `runtime/server.py` imports peer bootstraps but dispatches only through
  `OP_TABLE`.
- `OverlayClient` is an internal route point, not a public agent/tool API.
- The public `sandbox.api.{shell,read,write,edit}` work belongs to Step 7 and
  is not assessed as a Step 6 deliverable in this report.

---

## 5. Verification

Focused Step 6 tests that passed:

```bash
uv run pytest backend/tests/test_sandbox/test_overlay/test_client.py backend/tests/test_sandbox/test_overlay/test_bootstrap.py backend/tests/test_sandbox/test_overlay/test_package_structure.py backend/tests/test_sandbox/test_overlay/test_wire.py backend/tests/test_sandbox/test_overlay/test_runtime_capture.py backend/tests/test_sandbox/test_overlay/test_runtime_command.py backend/tests/test_sandbox/test_runtime/test_shell_pipeline.py -q
```

Result:

- `19 passed`

Fuller focused overlay/runtime command-executor run:

```bash
uv run pytest backend/tests/test_sandbox/test_overlay backend/tests/test_sandbox/test_runtime/test_shell_pipeline.py backend/tests/test_sandbox/test_runtime/test_shell_command_executor_overlay_occ.py -q
```

Result:

- `45 passed`
- `7 failed`

The failures are all constructor drift around removed `transport=` parameters:

- `backend/tests/test_sandbox/test_overlay/test_engine_direct_runtime_parity.py`
- `backend/tests/test_sandbox/test_overlay/test_engine_execution.py`
- `backend/tests/test_sandbox/test_runtime/test_shell_command_executor_overlay_occ.py`

Isolated command-executor verification still fails for the same reason:

```bash
uv run pytest backend/tests/test_sandbox/test_runtime/test_shell_command_executor_overlay_occ.py -q
```

Result:

- `4 failed`
- each failure is `TypeError: AuditedCommandExecutor.__init__() got an unexpected keyword argument 'transport'`

Structural checks observed while preparing this report:

- `backend/src/sandbox/code_intelligence/` is absent.
- `backend/src/sandbox/overlay/` has the target source layout.
- `test_package_structure.py` passed its overlay/OCC mutual non-import check.
- `test_shell_pipeline.py` passed overlay reject, OCC success projection, and
  OCC conflict projection behavior.

`ruff`, `mypy`, full `backend/tests/test_sandbox`, and live Daytona E2E/perf
were not run for this report because the focused suite is already blocked by
the stale `transport=` call sites.

---

## 6. Open Cleanup Before Green

The next cleanup pass should resolve the remaining constructor drift by choosing
one policy consistently:

- remove the stale `transport=` arguments from the affected tests and any
  remaining service construction paths, if provider adapters are now the only
  daemon selection mechanism; or
- reintroduce an explicit compatibility parameter if callers still need a
  transition surface.

After that, rerun:

```bash
uv run pytest backend/tests/test_sandbox/test_overlay backend/tests/test_sandbox/test_runtime -q
uv run ruff check backend/src/sandbox backend/tests/test_sandbox
git diff --check
```

Step 6 should not be treated as fully green until those gates pass.
