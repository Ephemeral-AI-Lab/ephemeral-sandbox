# sandbox-protocol Crate Spec

## Identity

```text
Path:    crates/sandbox-protocol
Package: sandbox-protocol
Import:  sandbox_protocol
```

`sandbox-protocol` is the shared process contract used by
`sandbox-gateway`, `sandbox-manager`, and `sandbox-daemon`.

## Owns

- Generic request and response structs.
- Unified request scope vocabulary:
  - `OperationScope`
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
- `OperationExecutionSpace`
- Protocol-owned catalog JSON document conversion and parsing helpers for
  `OperationCatalog` and cataloged `OperationSpec` data.

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
  scope.rs
  request.rs
  response.rs
  framing.rs
  auth.rs
  limits.rs
  error_kind.rs
  operation_spec.rs
  catalog.rs
```

## Public DTO Contract

The public protocol has one request DTO and one response DTO. It does not use a
separate routing envelope and it does not expose a `Manager` or `Daemon` target
field.

```rust
pub struct Request {
    pub request_id: String,
    pub scope: OperationScope,
    pub op: String,
    pub args: serde_json::Value,
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationScope {
    System,
    Sandbox { sandbox_id: String },
}

pub struct Response {
    // Opaque operation result/error JSON wrapper.
}
```

Use explicit protocol DTO names in new code.

Example manager-scoped request:

```json
{
  "request_id": "req-1",
  "scope": { "kind": "system" },
  "op": "list_sandboxes",
  "args": {}
}
```

Example sandbox-scoped request:

```json
{
  "request_id": "req-2",
  "scope": {
    "kind": "sandbox",
    "sandbox_id": "sbox-1"
  },
  "op": "exec_command",
  "args": {
    "workspace_session_id": "ws-1",
    "cmd": "pwd"
  }
}
```

Example response:

```json
{
  "command_session_id": "cmd-1",
  "status": "running",
  "exit_code": null,
  "output": {
    "stdout": ""
  },
  "finalized": null
}
```

`scope` identifies the resource the operation applies to. It is not an
implementation target and it is not the operation-execution-space selector.
`OperationExecutionSpace` belongs in catalog metadata only, for example
`manager` vs `runtime`.

Catalog JSON exposes one execution-space selector:

```json
{
  "operation_execution_space": "manager",
  "operations": []
}
```

Do not add separate `owner`, `target`, `route`, `implementation_owner`, or
`operation_target` fields. Do not add grouping metadata to operation specs; use
the catalog execution space as the operation classifier.

## Dependency Rules

Allowed:

- `serde`
- `serde_json`
- small serialization/error crates if needed

Forbidden:

- `sandbox-manager`
- `sandbox-daemon`
- `sandbox-runtime-*`
- `tokio` unless framing becomes explicitly async, which should be avoided.

## Verification

```sh
cargo fmt --check -p sandbox-protocol
cargo check -p sandbox-protocol --tests
cargo test -p sandbox-protocol
```
