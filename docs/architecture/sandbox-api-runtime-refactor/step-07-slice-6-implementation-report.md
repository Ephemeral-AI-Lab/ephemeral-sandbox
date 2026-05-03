# Step 7 / Slice 6 - Implementation Report

Companion to
[`step-07-slice-6-public-api.md`](./step-07-slice-6-public-api.md).
This report records the public sandbox API flip, the agent-tool cutover, the
legacy-import cleanup verified for this slice, and the current verification
evidence.

---

## 1. Verdict

**Step 7 is implemented and green in the current checkout.**

The slice exposes the public `sandbox.api.{read,write,edit,shell}` verbs,
completes the public request/result hierarchy, moves agent sandbox tools off the
old context-bound `sandbox_api` surface, and restricts tools to public API
imports only.

The guarded public verbs now route through the peer clients introduced by the
earlier runtime slices:

- `sandbox.api.write.write_file` -> `OCCClient.write`
- `sandbox.api.edit.edit_file` -> `OCCClient.edit`
- `sandbox.api.shell.shell` -> `OverlayClient.shell`

`sandbox.api.read.read_file` intentionally stays unguarded and uses `raw_exec`
directly. Missing files return `ReadFileResult(exists=False, content="")`
rather than a guarded conflict.

---

## 2. File Inventory

### Added Public Verb Modules

| File | Purpose |
| --- | --- |
| `backend/src/sandbox/api/read.py` | Public read verb over `raw_exec`; distinguishes missing files from empty files |
| `backend/src/sandbox/api/write.py` | Public write verb over `OCCClient.write`; maps OCC results to `WriteFileResult` |
| `backend/src/sandbox/api/edit.py` | Public edit verb over `OCCClient.edit`; maps OCC results to `EditFileResult` |
| `backend/src/sandbox/api/shell.py` | Public shell verb over `OverlayClient.shell`; maps overlay/OCC verdicts to `ShellResult` |

### Updated Public API Contract

| File | Change |
| --- | --- |
| `backend/src/sandbox/api/models.py` | Defines the frozen, kw-only public request/result hierarchy |
| `backend/src/sandbox/api/__init__.py` | Exports only public request/result models, public verb functions, and `raw_exec` |

### Updated Tool Boundary

| File | Change |
| --- | --- |
| `backend/src/tools/core/sandbox_session.py` | Builds `RequestActor` directly and keeps only sandbox-id/path helpers |
| `backend/src/tools/sandbox_toolkit/read_file.py` | Calls `sandbox.api.read.read_file`; no context-bound API lookup |
| `backend/src/tools/sandbox_toolkit/write_file.py` | Calls `sandbox.api.write.write_file`; keeps only tool schema and formatting |
| `backend/src/tools/sandbox_toolkit/edit_file.py` | Calls `sandbox.api.edit.edit_file`; keeps edit input normalization and formatting |
| `backend/src/tools/sandbox_toolkit/shell.py` | Calls `sandbox.api.shell.shell`; keeps pre-hooks and final `ToolResult` formatting |

### Updated Provider / Lifecycle Boundary

| File | Change |
| --- | --- |
| `backend/src/sandbox/providers/daytona/adapter.py` | Owns direct Daytona `exec` behavior instead of wrapping a legacy transport |
| `backend/src/sandbox/lifecycle/workspace.py` | Registers the provider adapter and no longer attaches context-bound `sandbox_api` / `sandbox_transport` |

### Added / Updated Tests

| Test | Coverage |
| --- | --- |
| `backend/tests/test_sandbox/test_api/test_read.py` | `raw_exec` delegation, missing-file result, no conflict field |
| `backend/tests/test_sandbox/test_api/test_write.py` | OCC delegation, write result mapping, conflict mapping |
| `backend/tests/test_sandbox/test_api/test_edit.py` | OCC delegation, applied-edit count, conflict mapping |
| `backend/tests/test_sandbox/test_api/test_shell.py` | Overlay delegation, changed-path/conflict round trip |
| `backend/tests/test_sandbox/test_api_contract.py` | Public model hierarchy, API import boundary, legacy module deletion |
| `backend/tests/test_sandbox/test_import_fence.py` | Agent tools import only public sandbox API verb/model modules |
| `backend/tests/test_sandbox/test_api/test_raw_exec_import_allowlist.py` | `raw_exec` remains restricted to explicit allowlisted paths |
| `backend/tests/test_tools/test_sandbox_toolkit/` | Tool behavior without a context-bound `sandbox_api` dependency |

---

## 3. Behavior Delivered

### Public Result Hierarchy

`sandbox.api.models` is the stable tool-facing contract. All result/request
dataclasses are frozen and kw-only.

Unguarded results inherit only `SandboxResultBase`:

- `RawExecResult`
- `ReadFileResult`

Guarded results inherit `GuardedResultBase` and share the same conflict shape:

- `WriteFileResult`
- `EditFileResult`
- `ShellResult`

The guarded result surface exposes:

- `changed_paths`
- `status`
- `conflict`
- `conflict_reason`

It does not expose legacy gitinclude/gitignore routing partitions.

### Read API

`read_file` is a thin unguarded wrapper over `raw_exec`. It uses a small Python
JSON command inside the sandbox and maps:

- existing files -> `ReadFileResult(success=True, exists=True, content=...)`
- missing files -> `ReadFileResult(success=True, exists=False, content="")`
- malformed/failed command output -> `ReadFileResult(success=False, exists=False, content="")`

This keeps read failures separate from OCC/overlay conflicts.

### Write / Edit API

`write_file` and `edit_file` convert public request models into OCC specs and
delegate exactly once through `OCCClient`.

OCC `OperationResult` values are projected onto the public result hierarchy:

- successful operation files become `changed_paths`
- failed operation status becomes `ConflictInfo.reason`
- conflict file/message are preserved on `ConflictInfo`
- `EditFileResult.applied_edits` is populated only on success

### Shell API

`shell` delegates to `OverlayClient.shell`, preserving the Overlay/OCC pipeline
as the only guarded command execution path. The public shell result preserves:

- `exit_code`
- `stdout`
- `changed_paths`
- conflict details from the pipeline
- warning strings

The agent-facing shell tool still decides final `ToolResult.is_error` from
command exit code plus guarded API success.

### Agent Tool Cutover

The sandbox tools are now pass-through adapters:

```text
tool schema/path handling -> sandbox.api.<verb> -> public result -> ToolResult formatting
```

They no longer fetch `context.sandbox_api`, import peer clients, or depend on
provider/runtime internals.

### Lifecycle Boundary

Context preparation registers the provider adapter and resolves repo/exec cwd.
Guarded operations are left to the public verb modules instead of a
context-bound API object.

---

## 4. Legacy Cleanup Verified

The following production import audits are zero-hit:

```bash
rg "sandbox\\.code_intelligence" backend/src
rg "SandboxTransport|DaytonaTransport" backend/src
rg "audited_sandbox_api|sandbox_api|sandbox\\.api\\.audit|sandbox\\.api\\.attribution" backend/src
rg "op_result_to_tool_result" backend/src
```

All four commands returned no matches.

The source tree also has no production file for the deleted public-API legacy
surfaces:

- `backend/src/sandbox/api/audited_sandbox_api.py`
- `backend/src/sandbox/api/sandbox_api.py`
- `backend/src/sandbox/api/audit.py`
- `backend/src/sandbox/api/attribution.py`
- `backend/src/sandbox/api/transport.py`
- `backend/src/sandbox/daytona/transport.py`
- `backend/src/sandbox/runtime/legacy_command_client.py`
- `backend/src/tools/core/op_result_to_tool_result.py`

`sandbox.code_intelligence` is also absent from the source tree.

---

## 5. Boundaries Preserved

- Agent tools import only `sandbox.api.{read,write,edit,shell}` and
  `sandbox.api.models`.
- Agent tools do not import `raw_exec`, `providers`, `occ`, `overlay`,
  `runtime`, `daytona`, or `code_intelligence`.
- Public verb modules may import peer clients where the slice explicitly allows
  it.
- `raw_exec` remains reachable only from `sandbox.api.read`, runtime setup /
  bundle paths, lifecycle paths, the public API lazy export, and the explicit
  debug route.
- `sandbox.api.models` imports no provider, runtime, OCC, overlay, tool, or
  Daytona SDK internals.
- `DaytonaProviderAdapter` owns provider `exec`; no `DaytonaTransport` wrapper
  remains in production imports.

---

## 6. Verification

Targeted Slice 6 API, import-fence, and tool tests:

```bash
uv run pytest backend/tests/test_sandbox/test_api/test_read.py backend/tests/test_sandbox/test_api/test_write.py backend/tests/test_sandbox/test_api/test_edit.py backend/tests/test_sandbox/test_api/test_shell.py backend/tests/test_sandbox/test_api_contract.py backend/tests/test_sandbox/test_import_fence.py backend/tests/test_sandbox/test_api/test_raw_exec_import_allowlist.py backend/tests/test_tools/test_sandbox_toolkit/test_write_file.py backend/tests/test_tools/test_sandbox_toolkit/test_edit_file.py backend/tests/test_tools/test_sandbox_toolkit/test_shell.py -q
```

Result:

- `37 passed`

Focused ruff check:

```bash
uv run ruff check backend/src/sandbox backend/src/tools/sandbox_toolkit backend/src/tools/core/sandbox_session.py backend/tests/test_sandbox/test_api backend/tests/test_sandbox/test_api_contract.py backend/tests/test_sandbox/test_import_fence.py backend/tests/test_tools/test_sandbox_toolkit
```

Result:

- `All checks passed!`

Full sandbox test suite:

```bash
uv run pytest backend/tests/test_sandbox -q
```

Result:

- `356 passed`

Default backend test suite:

```bash
uv run pytest -q
```

Result:

- `1023 passed`
- `280 deselected`

---

## 7. Notes

The verification above was performed against the current live checkout. The
working tree also contains later sandbox runtime/OCC cleanup edits, but those do
not change the Step 7 verdict: the public API flip is present, import fences are
green, legacy production imports are zero-hit, and the full default backend test
suite passes.
