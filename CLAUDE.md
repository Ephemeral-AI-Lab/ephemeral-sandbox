# CLAUDE.md

Guidance for working in this repository. Read
`docs/maintainer-architecture.md` for the component map and boundary law; this
file covers how to write and change code here.

## Project

Rust workspace (edition 2021, rust-version 1.85) for the EphemeralOS sandbox.
Crates live under `crates/`; see the component table in
`docs/maintainer-architecture.md` for each crate's job and what it must never
do. Respect those boundaries — a change that crosses them is wrong even if it
compiles.

## Engineering practice (required)

- **SOLID, SRP first.** Every type, module, and function owns one
  responsibility. If you cannot name a unit's single job in one sentence, split
  it. Keep crate boundaries from `docs/maintainer-architecture.md` intact;
  depend on the narrowest abstraction, not a concrete implementation crate.
- **Prefer less.** Fewer fields, fewer types, fewer methods, fewer round trips.
  Before adding a field/struct/method, check whether an existing one already
  carries the responsibility. Collapse redundant indirection and avoid extra
  hops across the manager/daemon/runtime boundary when one suffices.
- **No inline comments in production code.** Names and types carry the meaning;
  if code needs an inline comment to be understood, restructure it instead.
  Doc comments (`///`/`//!`) on public items are fine. Inline comments are
  acceptable only in tests, where they explain intent of a scenario.
- **No test code in `src/`.** Keep `src/` production-only. Inline test modules
  (`#[cfg(test)] mod tests`), test support/helpers, and fakes/mocks/stubs belong
  in the crate's `tests/` directory, never under `src/`. Move any such code to
  `tests/`; share fixtures through a `tests/` support module, not a `src` item
  gated behind `cfg(test)`.
- **Parallel workers.** Other agents may be editing this repo concurrently. Only
  touch what your task requires, never revert or overwrite changes you did not
  make, and prefer additive, localized edits.

## Build & test

```sh
export PATH="$PWD/bin:$PATH"   # repo-local sandbox tools

cargo build
cargo test                     # whole workspace
cargo test -p sandbox-runtime  # focused crate
cargo test -p sandbox-daemon

cargo clippy --all-targets     # must pass; lints are configured in Cargo.toml
cargo fmt

bin/setup-musl-cross           # one-time musl cross bootstrap (zig + cargo-zigbuild)
cargo run -p xtask -- package                  # in-container daemon binary
cargo run -p xtask -- package --profile release
```

`xtask package` cross-compiles the daemon to Linux musl and picks its builder
automatically: `zigbuild` (zig cc compiles and links C/asm deps such as
`zstd-sys` and `sha2-asm`) when zig + cargo-zigbuild are installed, else the
Docker-based `cross`. Force one with `--builder
{auto|zigbuild|cross|rust-lld|cargo}` or `SANDBOX_XTASK_BUILDER`; never
hand-export per-target `CC`/sysroot flags.

## Sandbox tools

- Rebuild the Docker sandbox gateway binary with
  `bin/start-sandbox-docker-gateway --rebuild-binary`.
- Use `sandbox-manager-cli` for operator/fleet operations (create, destroy,
  list, inspect, squash, and export), `sandbox-runtime-cli --sandbox-id ID` for
  public command/file operations, and `sandbox-observability-cli` for read-only
  aggregate or sandbox-scoped views.

Workspace lints (`Cargo.toml`) deny `correctness`/`suspicious` and
`undocumented_unsafe_blocks`, and warn on `unwrap_used`/`dbg_macro`. Don't
introduce new violations; justify any `unsafe` with a `// SAFETY:` block.

## Conventions

- External crates are declared once in `[workspace.dependencies]` and consumed
  via `dep.workspace = true`. Don't pin versions inside member crates.
- The YAML parser is fenced behind `crates/sandbox-config/src/yaml.rs`; callers
  use `ConfigDocument` and typed section schemas, never the parser directly.
- Adapter-neutral operation and application-envelope vocabulary belongs to
  `sandbox-operation-contract`; public declarations and routes belong to the
  feature-gated domain modules in `sandbox-operation-catalog`, while canonical
  internal identifiers and routes belong to its always-compiled `internal`
  module; CLI-only paths, flags, usage, and help belong to
  `sandbox-cli::projection`.
- `sandbox-protocol` owns only wire codec, framing, authentication fields,
  limits, and readiness. Applications (`sandbox-manager`, `sandbox-runtime`,
  `sandbox-observability-query`) never depend on protocol, the shared
  client, product adapters, composition roots, or each other's implementations.
- `crates/sandbox-operations/`, `crates/sandbox-observability/`, and
  `crates/sandbox-runtime/` are the only namespace directories. They are
  grouping only and never gain a root `Cargo.toml`, facade, or re-export layer.
