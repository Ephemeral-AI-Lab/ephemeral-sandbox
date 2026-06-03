# Crate `eos-terminal-pair` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-terminal-pair/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**1 item (1 struct, 0 enums, 0 traits, 0 type aliases) across 1 file.**

`eos-terminal-pair` provides safe allocation of a PTY (pseudo-terminal) controller/attached file pair for command sessions in the eosd shell/command-execution path, wrapping the Linux `posix_openpt`/`grantpt`/`unlockpt`/`ptsname_r` FFI sequence behind a single owned struct. Its only item group is the `TerminalPair` handle returned by the crate's `open_terminal_pair` allocation function.

## Contents

- **`eos-terminal-pair/src/lib.rs`** — `TerminalPair`

---

## `eos-terminal-pair/src/lib.rs`

#### `TerminalPair`  ·  _struct_  ·  derives: `Debug`  ·  [L22]

Owned controller/attached `File` pair for an allocated pseudo-terminal session.

**Fields**

| name | type | vis |
|------|------|-----|
| `controller` | `File` | `pub` |
| `attached` | `File` | `pub` |
