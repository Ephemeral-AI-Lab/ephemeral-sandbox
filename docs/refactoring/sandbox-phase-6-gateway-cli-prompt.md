# Phase 6 Prompt: Add `sandbox-gateway-cli`

Use this prompt after phase 5 has completed.

```text
You are working in:

/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os

Task:

Implement phase 6 only: add the human-facing `sandbox-gateway-cli` package and
the installed `sandbox` binary. The gateway parses CLI arguments, builds
`sandbox_protocol::SandboxRequest` values, sends them to `sandbox-manager`, and
renders responses.

Before editing, read:

- docs/refactoring/sandbox-implementation-guide.md
- docs/refactoring/sandbox-gateway-cli.md
- docs/refactoring/sandbox-protocol.md
- docs/refactoring/sandbox-manager.md
- docs/refactoring/sandbox-manager-daemon-split.md

Required starting state:

- `crates/sandbox-protocol` exists.
- `sandbox_protocol::SandboxRequest` exists.
- `sandbox_protocol::OperationScope` exists.
- `sandbox_protocol::SandboxResponse` exists.
- `crates/sandbox-manager` exists.
- `crates/sandbox-manager/src/server` exists.
- `crates/sandbox-daemon` exists.
- `crates/sandbox-runtime/operation` exists.
- `crates/sandbox-gateway-cli` does not exist yet.
- `crates/sandbox-manager/src/operation/impls/invoke_sandbox_daemon.rs` does
  not exist.
- The manager operation catalog does not contain `invoke_sandbox_daemon`.
- Runtime support crates still exist under `crates/daemon`:
  `command`, `workspace`, `namespace-process`, `layerstack`, `overlay`, and
  `config`.

If this starting state is not true, stop and report that phase 5 is not
complete. Do not implement phase 6 against a manager that still uses
`invoke_sandbox_daemon` or a separate routed-request wrapper.

Phase goal:

- Create package `sandbox-gateway-cli`.
- Add binary `sandbox`.
- Add manager socket/config discovery.
- Add manager client connection over the phase 5 manager protocol.
- Build unified `SandboxRequest` values from CLI argv and `OperationSpec`.
- Map `--sandbox SANDBOX_ID` to `OperationScope::Sandbox`.
- Use `OperationScope::System` for manager operations.
- Render response data to stdout and errors to stderr.
- Keep the gateway as a protocol client, not a hidden manager.

New package:

```text
Path:    crates/sandbox-gateway-cli
Package: sandbox-gateway-cli
Import:  sandbox_gateway_cli
Binary:  sandbox
```

Keep in `sandbox-gateway-cli`:

- CLI argument parsing.
- CLI config discovery and precedence.
- Manager client socket connection.
- Request construction from operation specs and CLI argv.
- Manual/help rendering from `OperationSpec`.
- Output rendering and exit-code behavior.
- Tests for scope selection, request construction, output behavior, and config
  precedence.

Keep out of `sandbox-gateway-cli`:

- Sandbox lifecycle state.
- Daemon endpoint registry.
- Daemon operation dispatch.
- Command/workspace/layerstack/overlay semantics.
- Direct daemon endpoint knowledge for normal use.
- Direct dependencies on `sandbox-manager`, `sandbox-daemon`,
  `sandbox-runtime`, or runtime support crates.
- Public `invoke_sandbox_daemon` behavior.
- `RoutedRequest`, `ManagerRequest`, or `OperationTarget`.

Implementation steps:

1. Check current status:

   ```sh
   git status --short
   ```

2. Verify the phase 5 starting state:

   ```sh
   test -d crates/sandbox-protocol
   test -d crates/sandbox-manager
   test -d crates/sandbox-manager/src/server
   test -d crates/sandbox-daemon
   test -d crates/sandbox-runtime/operation
   test ! -d crates/sandbox-gateway-cli
   test ! -f crates/sandbox-manager/src/operation/impls/invoke_sandbox_daemon.rs
   rg -n "SandboxRequest|OperationScope|SandboxResponse" crates/sandbox-protocol/src
   rg -n "invoke_sandbox_daemon" crates/sandbox-manager/src/operation
   rg -n "RoutedRequest|ManagerRequest|OperationTarget" crates/sandbox-manager/src crates/sandbox-protocol/src
   ```

   The final two `rg` commands should return no matches.

3. Run and record baseline results:

   ```sh
   cargo fmt --check -p sandbox-protocol -p sandbox-manager
   cargo check -p sandbox-protocol -p sandbox-manager --tests
   cargo test -p sandbox-protocol -p sandbox-manager
   ```

   If any command fails, record that it was pre-existing and continue only if
   the failure is unrelated to adding `sandbox-gateway-cli`.

4. Add the package:

   ```text
   crates/sandbox-gateway-cli/
     Cargo.toml
     src/
       main.rs
       lib.rs
       config.rs
       client.rs
       manual.rs
       request_builder.rs
       output.rs
     tests/
   ```

5. Update root `Cargo.toml`:

   - Add workspace member `crates/sandbox-gateway-cli`.
   - Add workspace dependency:

     ```toml
     sandbox-gateway-cli = { path = "crates/sandbox-gateway-cli" }
     ```

   - Add CLI parsing dependency to workspace dependencies if not already
     present:

     ```toml
     clap = { version = "4", features = ["derive"] }
     ```

6. Create `crates/sandbox-gateway-cli/Cargo.toml`:

   ```toml
   [package]
   name = "sandbox-gateway-cli"
   version.workspace = true
   edition.workspace = true
   rust-version.workspace = true
   license.workspace = true

   [[bin]]
   name = "sandbox"
   path = "src/main.rs"

   [dependencies]
   sandbox-protocol.workspace = true
   anyhow.workspace = true
   clap.workspace = true
   serde_json.workspace = true
   tokio.workspace = true

   [lints]
   workspace = true
   ```

   Do not add `sandbox-manager`, `sandbox-daemon`, `sandbox-runtime`,
   `command`, `workspace`, `layerstack`, `overlay`, or `namespace-process`.

7. Add `src/config.rs`:

   - Define gateway config, at minimum:

     ```rust
     pub struct GatewayConfig {
         pub manager_socket_path: PathBuf,
         pub default_sandbox_id: Option<String>,
     }
     ```

   - Use config precedence:

     ```text
     CLI args > env vars > config file > defaults
     ```

   - Suggested env vars:

     ```text
     SANDBOX_MANAGER_SOCKET
     SANDBOX_DEFAULT_ID
     ```

   - A config file may be deferred if no repo config convention exists yet, but
     keep the type shaped so file config can be added without changing request
     construction.

8. Add `src/client.rs`:

   - Connect to the manager Unix socket from `GatewayConfig`.
   - Send exactly one newline-delimited JSON `SandboxRequest`.
   - Read exactly one newline-delimited JSON response.
   - Enforce a response size cap if practical.
   - Keep transport errors distinct from protocol errors.
   - Do not know daemon endpoint paths.

9. Add `src/request_builder.rs`:

   - Build `sandbox_protocol::SandboxRequest`.
   - Generate `request_id` values locally.
   - Use `OperationSpec.args` and `ArgCliSpec` to map CLI flags and
     positionals into `args`.
   - Gateway-owned scope flag:

     ```text
     --sandbox SANDBOX_ID
     ```

   - Scope rules:

     ```text
     manager operation without --sandbox -> OperationScope::System
     manager operation with --sandbox    -> reject
     daemon operation with --sandbox     -> OperationScope::Sandbox
     daemon operation without --sandbox  -> use default sandbox or reject
     ```

   - Manager operations that take a sandbox id as data must keep using their
     operation arg, such as `--sandbox-id`. Do not confuse `--sandbox-id` with
     the gateway-owned `--sandbox` scope selector.

10. Add `src/manual.rs`:

    - Render help/manual text from `OperationSpec`.
    - Do not duplicate operation argument descriptions by hand.
    - Preserve separate sections:

      ```text
      Sandbox Manager Operations
      Sandbox Daemon Operations
      ```

    - For phase 6, manager operations can be described through the manager
      catalog operation and daemon operations can be described through
      `describe_daemon_operations --sandbox-id ID` or a default sandbox.
    - Do not require a direct dependency on `sandbox-manager` or
      `sandbox-runtime` to render manuals.

11. Add `src/output.rs`:

    - Machine-readable response data goes to stdout.
    - Errors go to stderr.
    - Return non-zero exit codes for CLI parse errors, config errors,
      connection errors, protocol errors, and operation errors.
    - Keep JSON output stable for scripts.

12. Add `src/main.rs`:

    - Use `clap` or an equivalent parser.
    - Binary name must be `sandbox`.
    - Support at least:

      ```text
      sandbox create_sandbox --sandbox-id sbox-1
      sandbox list_sandboxes
      sandbox inspect_sandbox --sandbox-id sbox-1
      sandbox start_sandbox_daemon --sandbox-id sbox-1
      sandbox stop_sandbox_daemon --sandbox-id sbox-1
      sandbox describe_manager_operations
      sandbox describe_daemon_operations --sandbox-id sbox-1
      sandbox exec_command --sandbox sbox-1 --workspace-session-id ws-1 "pwd"
      sandbox poll_command --sandbox sbox-1 --last-n-lines 50 cmd-1
      sandbox read_command_lines --sandbox sbox-1 --start-offset 0 --limit 100 cmd-1
      sandbox write_command_stdin --sandbox sbox-1 cmd-1 "hello"
      sandbox cancel_command --sandbox sbox-1 cmd-1
      ```

    - Prefer canonical operation names as the command names:
      `exec_command`, `poll_command`, `cancel_command`, etc.
    - Ergonomic aliases can be added later, but they must not replace the
      canonical operation names.

13. Add tests.

    Include focused tests such as:

    - `manager_operation_uses_system_scope`.
    - `daemon_operation_requires_sandbox_without_default`.
    - `daemon_operation_uses_default_sandbox_when_configured`.
    - `sandbox_flag_populates_sandbox_scope`.
    - `manager_operation_rejects_sandbox_scope`.
    - `manager_sandbox_id_arg_remains_regular_arg`.
    - `exec_command_maps_workspace_session_id_and_command`.
    - `poll_command_maps_command_session_id_and_last_n_lines`.
    - `output_writes_success_to_stdout`.
    - `output_writes_errors_to_stderr`.
    - `config_precedence_cli_env_default`.

    Prefer unit tests for request construction and output behavior. Use an
    in-memory or temporary Unix socket fake only for manager client tests.

14. Update docs only where needed:

    - Keep `docs/refactoring/sandbox-gateway-cli.md` accurate if the prompt
      reveals a mismatch.
    - Do not rewrite older phase prompts unless they actively contradict the
      current target shape.

Non-goals:

- Do not implement phase 7 catalog stabilization.
- Do not implement manager server features.
- Do not add direct daemon transport.
- Do not add direct dependency on `sandbox-manager`, `sandbox-daemon`, or
  `sandbox-runtime`.
- Do not add `invoke_sandbox_daemon`.
- Do not introduce `RoutedRequest`, `ManagerRequest`, or `OperationTarget`.
- Do not rename runtime support crates.
- Do not change daemon command operation behavior.
- Do not remove `command-request.json`.

Acceptance checks:

```sh
test -d crates/sandbox-gateway-cli
test -f crates/sandbox-gateway-cli/Cargo.toml
test -f crates/sandbox-gateway-cli/src/main.rs
test -f crates/sandbox-gateway-cli/src/config.rs
test -f crates/sandbox-gateway-cli/src/client.rs
test -f crates/sandbox-gateway-cli/src/manual.rs
test -f crates/sandbox-gateway-cli/src/request_builder.rs
test -f crates/sandbox-gateway-cli/src/output.rs
rg -n "crates/sandbox-gateway-cli" Cargo.toml
rg -n "name = \"sandbox-gateway-cli\"|name = \"sandbox\"" crates/sandbox-gateway-cli/Cargo.toml
rg -n "sandbox-manager|sandbox-daemon|sandbox-runtime|command\\.workspace|workspace\\.workspace|layerstack\\.workspace|overlay\\.workspace|namespace-process\\.workspace" crates/sandbox-gateway-cli/Cargo.toml
rg -n "sandbox_manager::|sandbox_daemon::|sandbox_runtime::|command::|workspace::|layerstack::|overlay::|namespace_process::" crates/sandbox-gateway-cli/src
rg -n "invoke_sandbox_daemon|RoutedRequest|ManagerRequest|OperationTarget" crates/sandbox-gateway-cli crates/sandbox-manager/src crates/sandbox-protocol/src
cargo fmt --check -p sandbox-gateway-cli -p sandbox-protocol
cargo check -p sandbox-gateway-cli --tests
cargo test -p sandbox-gateway-cli
cargo clippy -p sandbox-gateway-cli --all-targets --no-deps -- -D warnings
```

The dependency/import/stale-name `rg` scans should return no matches.

Final response requirements:

- Summarize the new package and binary.
- State whether phase 5 starting-state checks passed.
- State whether baseline checks had pre-existing failures.
- State final verification commands and results.
- Call out how `--sandbox` maps to `OperationScope::Sandbox`.
- Call out that gateway talks only to `sandbox-manager`.
- Do not claim phase 7 work was done.
```
