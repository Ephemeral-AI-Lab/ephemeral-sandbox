# sandbox-gateway-cli Crate Spec

## Identity

```text
Path:    crates/sandbox-gateway-cli
Package: sandbox-gateway-cli
Import:  sandbox_gateway_cli
Binary:  sandbox
```

`sandbox-gateway-cli` is the human-facing command line. It builds protocol
requests, sends them to `sandbox-manager`, and renders responses.

## Owns

- CLI argument parsing.
- CLI config discovery and precedence.
- Manager client connection setup.
- Request construction from `OperationSpec` and CLI argv.
- Manual/help rendering for manager and daemon catalogs.
- Output formatting and exit-code behavior.

## Must Not Own

- Sandbox lifecycle state.
- Daemon endpoint registry.
- Daemon operation dispatch.
- Command/workspace/layerstack/overlay semantics.
- Direct daemon endpoint knowledge for normal use.

## Target Modules

```text
src/
  main.rs
  config.rs
  client.rs
  manual.rs
  request_builder.rs
  output.rs
```

## CLI Rules

- Installed binary name is `sandbox`.
- Errors go to stderr.
- Machine-readable responses go to stdout.
- Default route is gateway -> manager.
- Daemon operations require `--sandbox SANDBOX_ID` unless config provides a
  default sandbox.
- Help/manual text is generated from `OperationSpec`, not duplicated by hand.

## Example Commands

```text
sandbox create_sandbox
sandbox list_sandboxes
sandbox exec_command --sandbox sbox-1 --cmd "pwd"
sandbox poll_command --sandbox sbox-1 cmd-1
```

## Dependency Rules

Allowed:

- `sandbox-protocol`
- CLI parsing/output crates

Forbidden:

- `sandbox-daemon`
- `sandbox-runtime-*`
- direct sandbox runtime libraries

The CLI talks to `sandbox-manager`; it does not become a hidden manager.

## Verification

```sh
cargo fmt --check -p sandbox-gateway-cli
cargo check -p sandbox-gateway-cli --tests
cargo test -p sandbox-gateway-cli
```
