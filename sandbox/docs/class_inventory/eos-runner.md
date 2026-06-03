# Crate `eos-runner` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-runner/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**14 items (8 structs, 3 enums, 3 traits, 0 type aliases) across 4 files.**

`eos-runner` is the single-threaded, no-tokio namespace runner: a dedicated child the `eosd` daemon execs to perform the kernel syscalls (`unshare`, `setns`, `mount`) that require a single-threaded caller, porting the Python `sandbox/overlay` and `sandbox/isolated_workspace` namespace helpers. Its item groups are the owned request/result wire types (`RunMode`, `RunRequest`, `RunResult`, `ToolCall`, `NsFds`, `Fd`, `WorkspaceRoot`), the overlay-mount inversion port (`KernelMountPort`, `MountInputs`, `MountedOverlay`), the `thiserror` failure enum (`RunnerError`), and small fresh-ns execution helpers (`TimeoutKill`, `SyscallResult`).

## Contents

- **`eos-runner/src/error.rs`** — `RunnerError`
- **`eos-runner/src/request.rs`** — `RunMode`, `Fd`, `WorkspaceRoot`, `NsFds`, `ToolCall`, `RunRequest`, `RunResult`
- **`eos-runner/src/mount.rs`** — `MountInputs`, `MountedOverlay`, `KernelMountPort`
- **`eos-runner/src/fresh_ns.rs`** — `ParentIds`, `TimeoutKill`, `SyscallResult`

---

## `eos-runner/src/error.rs`

#### `RunnerError`  ·  _enum_  ·  derives: `Debug, Error`  ·  `#[non_exhaustive]`  ·  [L15]

Failures returned by the namespace runner.

**Variants**: `Syscall(std::io::Error)`, `InvalidRequest(String)`, `Overlay(eos_overlay::OverlayError)`, `Child(std::io::Error)`, `TimedOut`, `Unsupported`

<details><summary>Methods (1)</summary>

`from`

</details>

---

## `eos-runner/src/request.rs`

#### `RunMode`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  `#[serde(rename_all = "snake_case")]`  ·  [L24]

Which namespace strategy the runner uses for this call.

**Variants**: `FreshNs`, `SetNs`

---

#### `Fd`  ·  _struct_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  `#[repr(transparent)]`  ·  [L41]

A raw file descriptor handle; `#[repr(transparent)]` lets it cross the FFI boundary into the `setns(2)` syscall unchanged.

**Fields**

| name | type | vis |
|------|------|-----|
| `0` | `RawFd` | `pub` |

---

#### `WorkspaceRoot`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L46]

The validated workspace root the overlay is mounted at (e.g. `/testbed`).

**Fields**

| name | type | vis |
|------|------|-----|
| `0` | `PathBuf` | `pub` |

---

#### `NsFds`  ·  _struct_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  [L55]

The ns-holder's pre-opened namespace FDs, applied in the load-bearing order `user`, `mnt`, `pid`, `net`.

**Fields**

| name | type | vis |
|------|------|-----|
| `user` | `Option<Fd>` | `pub` |
| `mnt` | `Option<Fd>` | `pub` |
| `pid` | `Option<Fd>` | `pub` |
| `net` | `Option<Fd>` | `pub` |

---

#### `ToolCall`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L73]

One tool invocation, the runner's view of `ToolCallRequest`; `args` is the opaque verb payload forwarded to the in-namespace primitive.

**Fields**

| name | type | vis |
|------|------|-----|
| `invocation_id` | `String` | `pub` |
| `agent_id` | `String` | `pub` |
| `verb` | `String` | `pub` |
| `intent` | `Intent` | `pub` |
| `args` | `Value` | `pub` |
| `background` | `bool` | `pub` |

---

#### `RunRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize`  ·  [L89]

A fully-resolved request to the runner: which mode, the tool call, the overlay layout (fresh-ns), and the held namespace FDs (setns).

**Fields**

| name | type | vis |
|------|------|-----|
| `mode` | `RunMode` | `pub` |
| `tool_call` | `ToolCall` | `pub` |
| `workspace_root` | `WorkspaceRoot` | `pub` |
| `layer_paths` | `Vec<PathBuf>` | `pub` |
| `upperdir` | `Option<PathBuf>` | `pub` |
| `workdir` | `Option<PathBuf>` | `pub` |
| `ns_fds` | `Option<NsFds>` | `pub` |
| `cgroup_path` | `Option<PathBuf>` | `pub` |
| `timeout_seconds` | `Option<f64>` | `pub` |

---

#### `RunResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L127]

The runner's result: the in-namespace tool result JSON plus the child's exit code.

**Fields**

| name | type | vis |
|------|------|-----|
| `tool_result` | `Value` | `pub` |
| `exit_code` | `i32` | `pub` |

---

## `eos-runner/src/mount.rs`

#### `MountInputs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L26]

Validated overlay-mount inputs: newest-first lower layers plus upper/work dirs, mirroring the Python `MountInputs`.

**Fields**

| name | type | vis |
|------|------|-----|
| `workspace_root` | `PathBuf` | `pub` |
| `layer_paths` | `Vec<PathBuf>` | `pub` |
| `upperdir` | `PathBuf` | `pub` |
| `workdir` | `PathBuf` | `pub` |

---

#### `MountedOverlay`  ·  _trait_  ·  supertraits: `Debug`  ·  [L46]

Marker trait for a mounted-overlay guard; blanket-implemented for any `T: Debug` so a mount port can return an opaque RAII guard.

---

#### `KernelMountPort`  ·  _trait_  ·  [L50]

The overlay-mount port the runner calls once inside the target namespace; the daemon wires a thin adapter over `eos-overlay::kernel_mount`.

<details><summary>Methods (1)</summary>

`mount_overlay`

</details>

---

## `eos-runner/src/fresh_ns.rs`

#### `ParentIds`  ·  _struct_  ·  [L113]

Function-local struct in `enter_fresh_namespace` bundling the parent process's uid/gid captured before `unshare`, used to write the `uid_map`/`gid_map`.

**Fields**

| name | type | vis |
|------|------|-----|
| `user` | `u32` |  |
| `group` | `u32` |  |

---

#### `TimeoutKill`  ·  _enum_  ·  derives: `Clone, Copy`  ·  [L296]

Selects how `wait_for_child` reaps a timed-out child; currently kills the whole process group.

**Variants**: `ProcessGroup`

---

#### `SyscallResult`  ·  _trait_  ·  generics: `<T>`  ·  [L585]

Extension trait mapping a `rustix::io::Result<T>` into a `RunnerError::Syscall` so the fresh-ns syscall sites stay terse.

<details><summary>Methods (1)</summary>

`map_syscall`

</details>
