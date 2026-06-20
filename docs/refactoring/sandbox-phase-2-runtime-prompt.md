# Phase 2 Prompt: Extract `sandbox-runtime`

Use this prompt after phase 1 has completed.

```text
You are working in:

/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os

Task:

Implement phase 2 only: move the daemon operation catalog and operation
dispatch facade into the `sandbox-runtime` package.

Before editing, read:

- docs/refactoring/sandbox-implementation-guide.md
- docs/refactoring/sandbox-runtime.md
- docs/refactoring/sandbox-protocol.md
- docs/refactoring/sandbox-manager-daemon-split.md

Required starting state:

- `crates/sandbox-protocol` exists.
- `crates/daemon/rpc_protocol` no longer exists.
- Root `Cargo.toml` has workspace dependency:
  `sandbox-protocol = { path = "crates/sandbox-protocol" }`.
- `daemon_operation` still exists at `crates/daemon/operation`.
- `daemon` and `eosd` still exist under `crates/daemon`.

If this starting state is not true, stop and report that phase 1 is not
complete. Do not implement phase 2 against the old pre-phase-1 layout.

Phase goal:

- Move `crates/daemon/operation` to `crates/sandbox-runtime/operation`.
- Rename package `daemon_operation` to `sandbox-runtime`.
- Rename Rust imports from `daemon_operation` to `sandbox_runtime`.
- Keep the operation module shape and command operation behavior unchanged.
- Export the daemon operation catalog from `sandbox_runtime`.

Package move:

```text
daemon_operation -> sandbox-runtime
```

Expected resulting path and package:

```text
Path:    crates/sandbox-runtime/operation
Package: sandbox-runtime
Import:  sandbox_runtime
```

Keep in `sandbox-runtime`:

- `OperationRequest` alias to `sandbox_protocol::Request`.
- `OperationResponse` alias to `sandbox_protocol::Response`.
- `OperationDispatch`.
- `OperationEntry`.
- Command operation specs and dispatch functions.
- Workspace session and workspace remount orchestration.
- `DaemonOperations`, renamed to `SandboxDaemonOperations` after the package
  rename compiles.

Keep out of `sandbox-runtime`:

- Manager operations.
- Sandbox lifecycle and daemon endpoint registry.
- CLI behavior.
- Socket listener lifecycle.
- Low-level command process, workspace lifecycle, layerstack, overlay, and
  config implementations beyond the existing runtime dependencies.

Implementation steps:

1. Check current status:

   ```sh
   git status --short
   ```

2. Verify the phase 1 starting state:

   ```sh
   test -d crates/sandbox-protocol
   test ! -d crates/daemon/rpc_protocol
   test -d crates/daemon/operation
   rg -n "sandbox-protocol = \\{ path = \"crates/sandbox-protocol\" \\}" Cargo.toml
   ```

3. Run and record baseline results before file moves:

   ```sh
   cargo fmt --check -p sandbox-protocol -p daemon_operation -p daemon -p eosd
   cargo check -p sandbox-protocol -p daemon_operation -p daemon -p eosd
   cargo test -p sandbox-protocol -p daemon_operation -p daemon
   ```

   If any command fails, record that it was pre-existing and continue only if
   the failure is unrelated to the phase 2 move.

4. Move the operation crate:

   ```text
   crates/daemon/operation -> crates/sandbox-runtime/operation
   ```

5. Update root `Cargo.toml`:

   - Replace workspace member `crates/daemon/operation` with
     `crates/sandbox-runtime/operation`.
   - Replace workspace dependency `daemon_operation` with
     `sandbox-runtime = { path = "crates/sandbox-runtime/operation" }`.

6. Update `crates/sandbox-runtime/operation/Cargo.toml`:

   ```toml
   [package]
   name = "sandbox-runtime"
   ```

   Keep existing runtime support dependencies as-is for this phase:

   ```toml
   command.workspace = true
   workspace.workspace = true
   sandbox-protocol.workspace = true
   serde_json.workspace = true
   thiserror.workspace = true
   ```

7. Update consuming manifests:

   - In `crates/daemon/server/Cargo.toml`, replace
     `daemon_operation.workspace = true` with
     `sandbox-runtime.workspace = true`.
   - In `crates/daemon/eosd/Cargo.toml`, replace
     `daemon_operation.workspace = true` with
     `sandbox-runtime.workspace = true`.

8. Update Rust imports:

   - Replace `daemon_operation::` with `sandbox_runtime::`.
   - Replace `use daemon_operation::...` with `use sandbox_runtime::...`.
   - Update tests under the moved crate from `daemon_operation` to
     `sandbox_runtime`.

9. Keep current module shape:

   ```text
   src/
     lib.rs
     operation.rs
     public/
       mod.rs
       command/
         mod.rs
         service.rs
         service/
           impls/
             mod.rs
             exec_command.rs
             write_command_stdin.rs
             poll_command.rs
             read_command_lines.rs
             cancel_command.rs
     internal/
       mod.rs
       services.rs
       workspace_session/
       workspace_remount/
   ```

10. After the package rename compiles, rename the aggregate type:

    ```text
    DaemonOperations -> SandboxDaemonOperations
    ```

    If a compatibility alias is needed to avoid a huge diff, keep it local and
    temporary:

    ```rust
    pub type DaemonOperations = SandboxDaemonOperations;
    ```

    Prefer removing the alias before completing phase 2 if the diff stays
    manageable.

11. Export the daemon operation catalog:

    ```rust
    pub fn operation_specs() -> &'static [&'static OperationSpec];
    pub fn operation_catalog() -> OperationCatalog;
    ```

    Use the `OperationCatalog` and `OperationAuthority` shape produced in phase
    1. Do not redesign `sandbox-protocol` in this phase unless a small compile
    fix is required.

12. Ensure operation names remain unchanged:

    ```text
    exec_command
    write_command_stdin
    poll_command
    read_command_lines
    cancel_command
    ```

Non-goals:

- Do not rename `daemon` to `sandbox-daemon` in this phase.
- Do not merge `eosd` into the daemon package in this phase.
- Do not create `sandbox-manager`.
- Do not create `sandbox-gateway-cli`.
- Do not rename runtime support packages:
  - `command`
  - `workspace`
  - `namespace-process`
  - `layerstack`
  - `overlay`
  - `config`
- Do not move command execution semantics into manager code.
- Do not change command operation behavior.
- Do not rename `exec_command`, `poll_command`, or `cancel_command`.
- Do not remove `command-request.json`.
- Do not add Docker, Firecracker, or sandbox lifecycle implementation.

Acceptance checks:

```sh
test -d crates/sandbox-runtime/operation
test ! -d crates/daemon/operation
rg -n "daemon_operation" Cargo.toml crates --glob '!target/**'
rg -n "crates/daemon/operation" Cargo.toml crates --glob '!target/**'
rg -n "sandbox-runtime-operation|sandbox_runtime_operation" Cargo.toml crates --glob '!target/**'
rg -n "operation_catalog|SandboxDaemonOperations" crates/sandbox-runtime/operation
cargo fmt --check -p sandbox-runtime -p daemon -p eosd
cargo check -p sandbox-runtime -p daemon -p eosd
cargo test -p sandbox-runtime -p daemon
```

The first three `rg` acceptance scans should return no matches. The
`operation_catalog|SandboxDaemonOperations` scan should show the new runtime
facade exports.

Final response requirements:

- Summarize the package move and import rename.
- State whether phase 1 starting-state checks passed.
- State whether baseline checks had pre-existing failures.
- State final verification commands and results.
- Call out any temporary compatibility alias left in place.
- Do not claim phase 3 work was done.
```
