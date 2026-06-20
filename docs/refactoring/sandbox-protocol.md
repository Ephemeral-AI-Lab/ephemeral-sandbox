# sandbox-protocol Crate Spec

## Identity

```text
Path:    crates/sandbox-protocol
Package: sandbox-protocol
Import:  sandbox_protocol
```

`sandbox-protocol` is the shared process contract used by
`sandbox-gateway-cli`, `sandbox-manager`, and `sandbox-daemon`.

## Owns

- Generic request and response structs.
- JSON-line framing helpers.
- Auth field constants.
- Request size and timeout limits.
- Protocol error kind vocabulary.
- Operation metadata types:
  - `OperationSpec`
  - `ArgSpec`
  - `ArgKind`
  - `ArgCliSpec`
  - `CliSpec`
  - `OperationCatalog`
  - `OperationAuthority`
- Manual/help rendering helpers that operate only on `OperationSpec`.

## Must Not Own

- Socket listeners or clients.
- Manager operation dispatch.
- Daemon/runtime operation dispatch.
- Command, workspace, layerstack, overlay, namespace, or container runtime
  semantics.
- Any concrete operation list.

## Target Modules

```text
src/
  lib.rs
  request.rs
  response.rs
  framing.rs
  auth.rs
  limits.rs
  error_kind.rs
  operation_spec.rs
  catalog.rs
  manual.rs
```

## Dependency Rules

Allowed:

- `serde_json`
- small serialization/error crates if needed

Forbidden:

- `sandbox-manager`
- `sandbox-daemon`
- `sandbox-runtime-*`
- `tokio` unless framing becomes explicitly async, which should be avoided.

## Migration Source

Move from:

```text
crates/daemon/rpc_protocol
crates/daemon/operation/src/operation.rs protocol-neutral spec types
```

Keep implementation-specific `OperationEntry` in the owning operation crates.

## Verification

```sh
cargo fmt --check -p sandbox-protocol
cargo check -p sandbox-protocol --tests
cargo test -p sandbox-protocol
```
