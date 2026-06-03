# Crate `xtask` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/xtask/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**1 item (1 struct, 0 enums, 0 traits, 0 type aliases) across 1 file.**

`xtask` is the dev-only build and release tooling binary for the eosd runtime workspace; it packages the `eosd` daemon for the musl Linux release targets, copies and chmods the built artifact, and emits checksums, a protocol-version stamp, a JSON manifest, and optional minisign signatures. It is never linked into the runtime, and its single production item is the parsed `package`-command argument set.

## Contents

- **`xtask/src/main.rs`** — `PackageArgs`

---

## `xtask/src/main.rs`

#### `PackageArgs`  ·  _struct_  ·  derives: `Debug`  ·  [L39]

Parsed arguments for the `package` subcommand: release target, output directory, build/builder selection, and optional minisign signing inputs.

**Fields**

| name | type | vis |
|------|------|-----|
| `target` | `String` |  |
| `out_dir` | `PathBuf` |  |
| `no_build` | `bool` |  |
| `builder` | `String` |  |
| `sign` | `bool` |  |
| `minisign_key` | `Option<PathBuf>` |  |

<details><summary>Methods (1)</summary>

`parse`

</details>
