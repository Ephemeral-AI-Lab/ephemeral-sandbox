# Step 7 — Slice 6 — Public `sandbox.api.{shell, write, edit, read}`

**Goal.** Expose the four public verbs and cut tools over to them. This slice
is the public-surface flip: tools stop depending on a context-bound
`SandboxApi` object, guarded verbs route through peer clients, and the
§1.6 result hierarchy becomes the only tool-facing result surface.

**Depends on.** Step 5 / Slice 4 and Step 6 / Slice 5b.

## Live-tree research conclusion

The current checkout already has the durable peer/runtime packages:

- `backend/src/sandbox/occ/` — OCC engine, handlers, client, setup, wire, state,
  content, operations, commit, changeset, patching.
- `backend/src/sandbox/overlay/` — overlay engine, handlers, client, setup,
  runtime capture/command/mount code, wire/types.
- `backend/src/sandbox/runtime/` — bundle upload, setup orchestration, generic
  `OP_TABLE` server, and pipelines.
- `backend/src/sandbox/providers/` — provider registry and Daytona adapter seam.

The live tree also still carries migration surfaces that should not survive as
durable API:

- `sandbox.api.audited_sandbox_api`, `sandbox.api.sandbox_api`,
  `sandbox.api.audit`, and `sandbox.api.attribution`.
- `sandbox.api.transport.SandboxTransport` and
  `sandbox.daytona.transport.DaytonaTransport`.
- `sandbox.runtime.legacy_command_client`.
- old service facade/backend/registry compatibility paths if any remain outside
  `sandbox/runtime`.
- tool-side result mappers tied to old shapes:
  `tools/sandbox_toolkit/_mutation_result.py` and
  `tools/core/op_result_to_tool_result.py`.

Step 7 should therefore not add another adapter layer. The target is flatter:

```
agent tool -> sandbox.api.<verb> -> peer client/raw_exec -> ProviderAdapter.exec
```

## Expected target structure

After this slice and the immediately following legacy-delete slice, the sandbox
public/runtime structure should read as:

```
backend/src/sandbox/
    api/
        __init__.py
        models.py
        raw_exec.py
        read.py
        shell.py
        write.py
        edit.py

    providers/
        protocol.py
        registry.py
        daytona/
            __init__.py
            adapter.py

    runtime/
        __init__.py
        bundle.py
        setup_orchestrator.py
        server.py
        pipelines.py

    occ/
        __init__.py
        setup.sh
        bootstrap.py
        client.py
        engine.py
        types.py
        wire.py
        handlers/
            __init__.py
            write.py
            edit.py
            apply_changeset.py
        operations/
        content/
        commit/
        changeset/
        patching/
        state/

    overlay/
        __init__.py
        setup.sh
        bootstrap.py
        client.py
        engine.py
        types.py
        wire.py
        config.py
        handlers/
            __init__.py
            run.py
            shell.py
        runtime/
            __init__.py
            cli.py
            mounts.py
            capture.py
            command.py
            ndjson.py
            types.py
```

Tool-side structure:

```
backend/src/tools/sandbox_toolkit/
    __init__.py
    registry.py
    read_file.py
    write_file.py
    edit_file.py
    shell.py
    _file_tool_helpers.py
    _shell_prehooks.py
```

No `sandbox.code_intelligence/` directory, no `sandbox.api.transport`, no
context-bound `AuditedSandboxApi`, and no tool-side `OperationResult` mapper
should remain in the final structure.

## Files

### Add

- `backend/src/sandbox/api/read.py` — public read verb. Thin wrapper over
  `raw_exec`; reads stay direct and do not go through `runtime/server.py`.
  It returns `ReadFileResult` and can use a small inline Python/JSON command to
  distinguish missing files from empty files.
- `backend/src/sandbox/api/shell.py` — public shell verb. Delegates to
  `sandbox.overlay.client.OverlayClient.shell`; maps the peer result into
  `sandbox.api.models.ShellResult`.
- `backend/src/sandbox/api/write.py` — public write verb. Delegates to
  `sandbox.occ.client.OCCClient.write`; maps the OCC result into
  `WriteFileResult`.
- `backend/src/sandbox/api/edit.py` — public edit verb. Delegates to
  `sandbox.occ.client.OCCClient.edit`; maps the OCC result into
  `EditFileResult`.

### Modify

- `backend/src/sandbox/api/models.py`: complete the §1.6 hierarchy:
  `SandboxResultBase`, `GuardedResultBase`, `ConflictInfo`,
  `ReadFileResult`, `RawExecResult`, `WriteFileResult`, `EditFileResult`,
  `ShellResult`. All frozen + kw_only.
  - `ReadFileResult` and `RawExecResult` inherit only
    `SandboxResultBase`; they must not expose `conflict`.
  - `WriteFileResult`, `EditFileResult`, and `ShellResult` inherit
    `GuardedResultBase`.
  - Public guarded results expose `changed_paths` and `conflict` /
    `conflict_reason`. They do not expose gitinclude/gitignore routing
    partitions.
- `backend/src/sandbox/api/__init__.py`: export only the public verb functions,
  public request/result models, and raw exec primitive. Do not re-export
  `SandboxApi`, `SandboxTransport`, or `AuditedSandboxApi`.
- `backend/src/tools/core/sandbox_session.py`: keep path and sandbox-id helpers,
  but build `RequestActor` directly. Remove the dependency on
  `sandbox.api.attribution`. Once tools call verb modules directly,
  `sandbox_api_or_error` is obsolete.
- Agent tools become thin pass-throughs:
  - `backend/src/tools/sandbox_toolkit/shell.py`
  - `backend/src/tools/sandbox_toolkit/write_file.py`
  - `backend/src/tools/sandbox_toolkit/edit_file.py`
  - `backend/src/tools/sandbox_toolkit/read_file.py`
  They may keep tool schemas, path normalization, pre-hooks, and final
  `ToolResult` formatting, but no sandbox business logic and no peer-client
  imports.
- `backend/src/sandbox/providers/daytona/adapter.py`: own direct Daytona
  `exec` behavior instead of wrapping `sandbox.daytona.transport.DaytonaTransport`.
  The provider seam is one method: `ProviderAdapter.exec`.
- `backend/src/sandbox/lifecycle/workspace.py`: stop constructing or attaching
  the context-bound `sandbox_api` and deprecated `sandbox_transport` for tool
  execution. Context preparation should register the provider adapter, resolve
  repo/exec cwd, and leave guarded operations to the public verb modules.
- `test_importer_allowlist` / import-fence tests:
  - Agent tools may import only `sandbox.api.{shell, read, write, edit}` and
    `sandbox.api.models`.
  - Agent tools must not import `raw_exec`, `_registry`, `providers`, `occ`,
    `overlay`, `runtime`, `daytona`, or `code_intelligence`.
  - `raw_exec` remains allowlisted only for runtime setup, lifecycle, and
    explicit debug routes.

### Delete / make zero-hit

Delete these in this slice if the import migration makes them zero-hit. If one
still has production hits, the importer is the bug; fix the importer before the
legacy-delete slice.

- `backend/src/sandbox/api/audited_sandbox_api.py`
- `backend/src/sandbox/api/sandbox_api.py`
- `backend/src/sandbox/api/audit.py`
- `backend/src/sandbox/api/attribution.py`
- `backend/src/sandbox/api/transport.py`
- `backend/src/sandbox/daytona/transport.py`
- `backend/src/sandbox/runtime/legacy_command_client.py`
- any remaining legacy service facade files if Step 6 did not already delete
  them
- `backend/src/tools/sandbox_toolkit/_mutation_result.py`
- `backend/src/tools/core/op_result_to_tool_result.py`

Also delete or relocate any `sandbox.api.errors` usage that exists only to
support the deprecated transport. Public verbs should return typed result
objects with `ConflictInfo`; peer-client exceptions stay peer-local.

## Implementation tasks

1. Land the §1.6 result hierarchy first and migrate constructor calls. The
   dataclasses are frozen + kw_only so missing/extra fields fail loudly.
2. Implement `sandbox.api.read`.
   - Use `raw_exec`, not `runtime/server.py`.
   - Return `ReadFileResult(success=True, exists=False, content="")` for a
     missing file, not a guarded conflict.
3. Implement `sandbox.api.write` and `sandbox.api.edit`.
   - Convert public request models into OCC specs.
   - Call `OCCClient.write` / `OCCClient.edit`.
   - Map `OperationResult.success/status/conflict_*` into
     `WriteFileResult` / `EditFileResult`.
   - Populate `ConflictInfo` only on guarded failure.
4. Implement `sandbox.api.shell`.
   - Call `OverlayClient.shell`.
   - Map overlay/runtime `ShellResult` into public `ShellResult`.
   - Preserve `changed_paths` and conflict details exactly as returned by the
     pipeline/OCC verdict.
5. Cut agent tools over to public verbs.
   - Tools import verb modules, pass args through, and format `ToolResult`.
   - Tools do not fetch `context.sandbox_api`.
   - Tools do not import peer clients.
6. Move actor construction out of `sandbox.api.attribution`.
   `tools.core.sandbox_session.actor_from_context` can construct
   `RequestActor` directly.
7. Collapse tool result mappers.
   `tools/sandbox_toolkit/_mutation_result.py` and
   `tools/core/op_result_to_tool_result.py` are old-shape helpers; inline the
   small formatting needed by `write_file.py` / `edit_file.py` or use a helper
   named around public guarded results, not `OperationResult`.
8. Remove the deprecated transport stack from production imports.
   `providers/daytona/adapter.py` should not instantiate
   `DaytonaTransport`; it should be the Daytona exec adapter.
9. Run the zero-hit audits:
   - `rg "sandbox\\.code_intelligence" backend/src`
   - `rg "SandboxTransport|DaytonaTransport" backend/src`
   - `rg "audited_sandbox_api|sandbox_api|sandbox\\.api\\.audit|sandbox\\.api\\.attribution" backend/src`
   - `rg "op_result_to_tool_result|_mutation_result" backend/src`
10. Delete zero-hit legacy files in dependency order. Do not keep compatibility
    wrappers under the new public API.

## Tests

- New `backend/tests/test_sandbox/test_api/test_read.py`
  - Uses `raw_exec`.
  - Does not call `runtime/server.py`.
  - Missing file maps to `ReadFileResult(exists=False)`.
  - `ReadFileResult` has no `conflict` attribute.
- New `backend/tests/test_sandbox/test_api/test_write.py`
  - Exactly one adapter exec through `OCCClient`.
  - Result maps to `WriteFileResult`.
  - Guard rejection maps to `ConflictInfo`.
- New `backend/tests/test_sandbox/test_api/test_edit.py`
  - Exactly one adapter exec through `OCCClient`.
  - Applied edit count and conflict mapping are correct.
- New `backend/tests/test_sandbox/test_api/test_shell.py`
  - Exactly one adapter exec through `OverlayClient.shell`.
  - `changed_paths` and conflict details round-trip from the runtime result.
  - Overlay/OCC rejection maps to `ConflictInfo`.
- Updated import-fence tests:
  - Agent tool imports are restricted to public verb modules.
  - `raw_exec` is unreachable from agent paths.
  - Public verb modules may import peer clients; tools may not.
- Updated tool tests:
  - Tools no longer require `context.sandbox_api`.
  - Tools still require `sandbox_id` and repo/path context.
  - Output payload shape remains stable for callers.
- Delete-regression tests, either in this slice or Step 8 / Slice 7:
  - `import sandbox.code_intelligence` raises `ModuleNotFoundError`.
  - `from sandbox.api.transport import SandboxTransport` raises
    `ImportError`.

## Exit criteria

- Build / ruff / tests green.
- `sandbox.api.{shell, read, write, edit}` exist and are the only agent-tool
  sandbox operation imports.
- Guarded API modules route through peer clients:
  - `shell` -> `OverlayClient`
  - `write` / `edit` -> `OCCClient`
- Agent tools do not import `raw_exec`, `providers`, `occ`, `overlay`,
  `runtime`, `daytona`, or `code_intelligence`.
- `ReadFileResult` and `RawExecResult` cannot carry `ConflictInfo`.
- `WriteFileResult`, `EditFileResult`, and `ShellResult` use the same guarded
  shape and expose `changed_paths` plus conflict.
- No production import depends on `AuditedSandboxApi`, `SandboxApi`,
  `SandboxTransport`, `DaytonaTransport`, `legacy_command_client`,
  `sandbox.code_intelligence`, old `OperationResult` tool mappers, or
  `_mutation_result`.
- If any legacy file is not physically deleted in this slice, the doc must name
  the remaining production importer and Step 8 / Slice 7 must delete it.

## Risks

- The result-type migration touches many call sites at once. Mitigation:
  frozen + kw_only dataclasses, targeted API tests, then focused tool tests.
- A public verb accidentally becomes a second server-envelope builder.
  Mitigation: tests assert the verb delegates to the peer client, and peer
  clients remain the only server-envelope constructors.
- Tool migration preserves the old context-bound `sandbox_api` dependency.
  Mitigation: tool tests run with `sandbox_id` but without `sandbox_api`.
- Deleting `code_intelligence` too early could break transitional whitebox
  tests. Mitigation: migrate or retire those tests with the production import
  path they validate; zero production hits gate deletion.
- Daytona exec behavior regresses when `DaytonaTransport` is removed.
  Mitigation: move the existing command wrapping/exit-code extraction behavior
  into `providers/daytona/adapter.py` before deleting the old transport file.
