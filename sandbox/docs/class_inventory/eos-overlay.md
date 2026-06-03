# Crate `eos-overlay` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-overlay/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**9 items (5 structs, 2 enums, 1 trait, 1 type alias) across 4 files.**

`eos-overlay` is the eosd runtime's overlayfs kernel-mount and upper-dir capture leaf: it builds workspace overlay mounts via the raw new-mount API (`fsopen`/`fsconfig`/`fsmount`/`move_mount`), allocates the writable `upper`/`work` side of each mount, walks the `upperdir` to capture a policy-blind write set, and converts those captured changes one-way into `eos_protocol::LayerChange` for OCC publication. Its main item groups are the error type (`OverlayError`, `Result`), the mount mechanics (`OverlayHandle`, `OverlayMount`, `ValidatedMountInputs`, `MountIo`), the writable-dir allocation (`OverlayWritableDirs`), and the captured-change model (`OverlayPathChange`, `OverlayPathChangeKind`).

## Contents

- **`eos-overlay/src/error.rs`** — `OverlayError`, `Result`
- **`eos-overlay/src/kernel_mount.rs`** — `OverlayHandle`, `OverlayMount`, `ValidatedMountInputs`, `MountIo`
- **`eos-overlay/src/path_change.rs`** — `OverlayPathChangeKind`, `OverlayPathChange`
- **`eos-overlay/src/writable_dirs.rs`** — `OverlayWritableDirs`

---

## `eos-overlay/src/error.rs`

#### `OverlayError`  ·  _enum_  ·  derives: `Debug, Error`  ·  `#[non_exhaustive]`  ·  [L11]

Failures raised by the overlay kernel-mount and upper-dir capture paths.

**Variants**: `WritableRootUnavailable(String)`, `InvalidMountInput(String)`, `MountSyscall { context: &'static str, source: io::Error }`, `Capture(io::Error)`, `Path(CasError)`, `InvalidPathChange(String)`, `Unsupported`

#### `Result`  ·  _type alias_  ·  `= std::result::Result<T, OverlayError>`  ·  [L54]

Crate result alias.

---

## `eos-overlay/src/kernel_mount.rs`

#### `OverlayHandle`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L40]

The inputs for one overlay mount; `layer_paths` is the leased lower stack in newest-first order with the writable `upperdir`/`workdir` side.

**Fields**

| name | type | vis |
|------|------|-----|
| `upperdir` | `PathBuf` | `pub` |
| `workdir` | `PathBuf` | `pub` |
| `layer_paths` | `Vec<PathBuf>` | `pub` |

#### `OverlayMount`  ·  _struct_  ·  derives: `Debug`  ·  [L56]

A live overlay mount at a workspace root; RAII handle whose `Drop` unmounts the stacked mounts at the workspace root.

**Fields**

| name | type | vis |
|------|------|-----|
| `workspace_root` | `PathBuf` |  |

<details><summary>Methods (2)</summary>

`workspace_root`, `drop`

</details>

#### `ValidatedMountInputs`  ·  _struct_  ·  [L183]

Linux-only validated mount inputs: forbidden-char-checked, `O_DIRECTORY|O_NOFOLLOW`-pinned lowerdir fd paths plus the writable upper/work dirs, with the opened fds held alive for the syscall sequence.

**Fields**

| name | type | vis |
|------|------|-----|
| `workspace_root` | `PathBuf` |  |
| `layer_paths` | `Vec<PathBuf>` |  |
| `upperdir` | `PathBuf` |  |
| `workdir` | `PathBuf` |  |
| `_fds` | `Vec<File>` |  |

<details><summary>Methods (1)</summary>

`open`

</details>

#### `MountIo`  ·  _trait_  ·  generics: `<T>`  ·  [L354]

Linux-only adapter mapping a `rustix::io::Result` mount syscall outcome into a crate `Result` with `OverlayError::MountSyscall` context.

<details><summary>Methods (1)</summary>

`map_mount_syscall`

</details>

---

## `eos-overlay/src/path_change.rs`

#### `OverlayPathChangeKind`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L24]

The kind of a captured overlay path change.

**Variants**: `Write`, `Delete`, `Symlink`, `OpaqueDir`

#### `OverlayPathChange`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L41]

A single change captured from the overlay upperdir, before layer-stack policy is applied; `write`/`symlink` carry a staged `content_path` + `final_hash`, the others carry neither.

**Fields**

| name | type | vis |
|------|------|-----|
| `path` | `String` | `pub` |
| `kind` | `OverlayPathChangeKind` | `pub` |
| `content_path` | `Option<String>` | `pub` |
| `final_hash` | `Option<String>` | `pub` |

<details><summary>Methods (2)</summary>

`new`, `into_layer_change`

</details>

---

## `eos-overlay/src/writable_dirs.rs`

#### `OverlayWritableDirs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L20]

Per-overlay writable directories created beside each other under one run dir.

**Fields**

| name | type | vis |
|------|------|-----|
| `run_dir` | `PathBuf` | `pub` |
| `upperdir` | `PathBuf` | `pub` |
| `workdir` | `PathBuf` | `pub` |
