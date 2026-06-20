# Phase 1 Prompt: Extract `sandbox-protocol`

Use this prompt to start phase 1 of the sandbox manager/daemon split.

```text
You are working in:

/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os

Task:

Implement phase 1 only: extract and rename the shared daemon RPC protocol crate
to `sandbox-protocol`.

Before editing, read:

- docs/refactoring/sandbox-implementation-guide.md
- docs/refactoring/sandbox-protocol.md
- docs/refactoring/sandbox-manager-daemon-split.md

Phase goal:

- Move `crates/daemon/rpc_protocol` to `crates/sandbox-protocol`.
- Rename package `daemon_rpc_protocol` to `sandbox-protocol`.
- Rename Rust imports from `daemon_rpc_protocol` to `sandbox_protocol`.
- Move protocol-neutral operation metadata types from `daemon_operation` into
  `sandbox-protocol`.
- Keep behavior unchanged for the existing daemon, eosd, and command
  operations.

Current source facts:

- Root `Cargo.toml` currently has workspace member:
  `crates/daemon/rpc_protocol`.
- Root `Cargo.toml` currently has workspace dependency:
  `daemon_rpc_protocol = { path = "crates/daemon/rpc_protocol" }`.
- `crates/daemon/rpc_protocol/Cargo.toml` currently has:
  `name = "daemon_rpc_protocol"`.
- `crates/daemon/operation/src/operation.rs` currently defines:
  `OperationFamily`, `ArgKind`, `ArgCliSpec`, `ArgSpec`, `CliSpec`,
  `OperationSpec`, `OperationDispatch`, and `OperationEntry`.

Move to `sandbox-protocol`:

- `OperationFamily` or a renamed `OperationGroup`.
- `ArgKind`.
- `ArgCliSpec`.
- `ArgSpec`.
- `CliSpec`.
- `OperationSpec`.
- New `OperationAuthority`.
- New `OperationCatalog`.

Keep in `daemon_operation`:

- `OperationRequest` alias, updated to `sandbox_protocol::Request`.
- `OperationResponse` alias, updated to `sandbox_protocol::Response`.
- `OperationDispatch`.
- `OperationEntry`.
- Concrete operation specs and dispatch functions.
- `DaemonOperations`.

Implementation steps:

1. Check current status:

   ```sh
   git status --short
   ```

2. Run and record baseline results before file moves:

   ```sh
   cargo fmt --check -p daemon_rpc_protocol -p daemon_operation -p daemon -p eosd
   cargo check -p daemon_rpc_protocol -p daemon_operation -p daemon -p eosd
   cargo test -p daemon_rpc_protocol -p daemon_operation -p daemon
   ```

   If any command fails, record that it was pre-existing and continue only if
   the failure is unrelated to the phase 1 move.

3. Move the protocol crate:

   ```text
   crates/daemon/rpc_protocol -> crates/sandbox-protocol
   ```

4. Update root `Cargo.toml`:

   - Replace workspace member `crates/daemon/rpc_protocol` with
     `crates/sandbox-protocol`.
   - Replace workspace dependency `daemon_rpc_protocol` with
     `sandbox-protocol = { path = "crates/sandbox-protocol" }`.

5. Update `crates/sandbox-protocol/Cargo.toml`:

   ```toml
   [package]
   name = "sandbox-protocol"
   ```

6. Update consuming manifests:

   - In `crates/daemon/operation/Cargo.toml`, replace
     `daemon_rpc_protocol.workspace = true` with
     `sandbox-protocol.workspace = true`.
   - In `crates/daemon/server/Cargo.toml`, replace
     `daemon_rpc_protocol.workspace = true` with
     `sandbox-protocol.workspace = true`.

7. Update Rust imports:

   - Replace `daemon_rpc_protocol::` with `sandbox_protocol::`.
   - Replace `use daemon_rpc_protocol::...` with
     `use sandbox_protocol::...`.
   - Update tests under the moved crate from `daemon_rpc_protocol` to
     `sandbox_protocol`.

8. Add protocol metadata modules under `crates/sandbox-protocol/src/`:

   ```text
   operation_spec.rs
   catalog.rs
   manual.rs
   ```

   Keep these modules protocol-only. They must not know about command,
   workspace, daemon dispatch, manager dispatch, sockets, or runtime services.

9. Export the protocol metadata types from `sandbox-protocol/src/lib.rs`.

10. Update `crates/daemon/operation/src/operation.rs` so it imports or
    re-exports protocol metadata types from `sandbox_protocol`, while keeping
    `OperationDispatch` and `OperationEntry` local.

11. Update `crates/daemon/operation/src/public/protocol.rs` to re-export from
    `sandbox_protocol`.

12. Keep `OperationEntry` out of `sandbox-protocol`. Verify with:

    ```sh
    rg -n "OperationEntry|OperationDispatch|DaemonOperations" crates/sandbox-protocol
    ```

    This should have no hits.

Non-goals:

- Do not rename `daemon_operation` in this phase.
- Do not create `sandbox-runtime` in this phase.
- Do not rename `daemon` or `eosd` in this phase.
- Do not create `sandbox-manager` or `sandbox-gateway-cli`.
- Do not rename runtime support crates.
- Do not change command operation behavior.
- Do not rename `exec_command`, `poll_command`, or `cancel_command`.
- Do not remove `command-request.json`.
- Do not add socket clients, socket listeners, manager dispatch, or daemon
  runtime dispatch to `sandbox-protocol`.

Acceptance checks:

```sh
test -d crates/sandbox-protocol
test ! -d crates/daemon/rpc_protocol
rg -n "daemon_rpc_protocol" Cargo.toml crates --glob '!target/**'
rg -n "OperationEntry|OperationDispatch|DaemonOperations" crates/sandbox-protocol
cargo fmt --check -p sandbox-protocol -p daemon_operation -p daemon
cargo check -p sandbox-protocol -p daemon_operation -p daemon
cargo test -p sandbox-protocol -p daemon_operation
```

The two `rg` commands should return no matches. If they match historical docs
only, do not count that as a phase 1 code failure; the command above is scoped
to `Cargo.toml` and `crates`.

Final response requirements:

- Summarize moved files and package rename.
- State whether baseline checks had pre-existing failures.
- State final verification commands and results.
- Call out any intentionally deferred work.
- Do not claim phase 2 work was done.
```
