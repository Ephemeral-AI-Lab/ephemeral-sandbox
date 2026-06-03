# Crate `eosd` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eosd/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**3 items (3 structs, 0 enums, 0 traits, 0 type aliases) across 1 file.**

`eosd` is the binary entry point of the eosd runtime: a single static binary that
parses argv and dispatches to one of three library entry points (`daemon` async
RPC server, `ns-runner` namespace tool runner, `ns-holder` isolated-namespace
holder), preserving each library's typed exit codes. Its only types are two argv
parser config structs (`DaemonCliConfig`, `RunnerCliConfig`) plus
`OverlayMountPort`, the local adapter implementing `eos_runner::KernelMountPort`
for the ns-runner overlay-mount path.

## Contents

- **`eosd/src/main.rs`** — `DaemonCliConfig`, `RunnerCliConfig`, `OverlayMountPort`

---

## `eosd/src/main.rs`

#### `DaemonCliConfig`  ·  _struct_  ·  [L218]

Parsed argv configuration for the `eosd daemon` subcommand (socket/pid paths, optional TCP listener, spawn flag, and thin-client target).

**Fields**

| name | type | vis |
|------|------|-----|
| `socket_path` | `PathBuf` |  |
| `pid_path` | `PathBuf` |  |
| `log_path` | `Option<PathBuf>` |  |
| `tcp_host` | `Option<String>` |  |
| `tcp_port` | `Option<u16>` |  |
| `auth_token` | `Option<String>` |  |
| `spawn` | `bool` |  |
| `client` | `Option<(PathBuf, String)>` |  |

<details><summary>Methods (2)</summary>

`parse`, `foreground_args`

</details>

#### `RunnerCliConfig`  ·  _struct_  ·  [L417]

Parsed argv configuration for the `eosd ns-runner` subcommand (request/output paths and the mutually exclusive overlay-mount / remount / configure-dns mode flags).

**Fields**

| name | type | vis |
|------|------|-----|
| `request_path` | `Option<PathBuf>` |  |
| `output_path` | `Option<PathBuf>` |  |
| `mount_overlay` | `bool` |  |
| `remount_overlay` | `bool` |  |
| `configure_dns` | `bool` |  |

<details><summary>Methods (1)</summary>

`parse`

</details>

#### `OverlayMountPort`  ·  _struct_  ·  derives: `Debug`  ·  [L551]

Unit-struct adapter wiring `eos_overlay::mount_overlay` into the ns-runner overlay mount path by implementing `eos_runner::KernelMountPort`.

<details><summary>Methods (1)</summary>

`mount_overlay`

</details>
