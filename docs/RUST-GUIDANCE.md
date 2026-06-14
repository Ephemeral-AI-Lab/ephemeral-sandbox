# Rust implementation guidance for the `/sandbox` (eosd) port

This is the standard every crate in this workspace follows. It distills the project's
`rust-skills` rule set (`.agents/skills/rust-skills/`, rule ids cited inline) and the
**source-verified contract traps** found during extraction (`docs/contract/*.md`).
Those `docs/contract/` files are frozen historical migration evidence, not the
live operation contract. For current behavior, read `SPEC.md`, `API.md`,
`../crates/daemon/operation/ops.json`,
`../crates/shared/protocol/PROTOCOL.md`, and owner-local fixtures.

Priority order when rules conflict: **correctness (byte-identity) > the live contract > these idioms > style**.

---

## 0. Non-negotiables for this codebase

- **No `.unwrap()` / `.expect()` / `panic!` in library code** (`err-no-unwrap-prod`, `anti-unwrap-abuse`).
  Return `Result`. `.expect("BUG: …")` is allowed only for a genuine invariant the type system
  can't express, with a `BUG:` message. `unwrap()` is fine in `#[cfg(test)]` and in `eosd/src/main.rs`.
- **Library errors are `thiserror` enums** (`err-thiserror-lib`), `#[from]` for source conversions
  (`err-from-impl`), `?` for propagation, lowercase messages with no trailing punctuation
  (`err-lowercase-msg`). No `Box<dyn Error>` in public APIs (`err-custom-type`). `anyhow` only in
  `eosd`/`xtask` binaries, never in library crates.
- **Deferred ports are explicit, typed, and anchored.** Callable surfaces must return a typed
  deferred/unsupported error rather than panic. Do not use `unimplemented!()` or silent success
  stubs. Future parity anchors should cite a current contract fixture, frozen contract section, or
  live Rust owner module; do not add new backend-era source-path anchors to active Rust guidance.
  Use `todo!("PORT: ...")` only for an unwired future port scaffold, never in an implemented
  runtime path.
- **`#![forbid(unsafe_code)]` in every crate that has no syscalls.** Only `namespace`
  and `overlay` may contain `unsafe` (raw mount/ns syscalls). Those crates use
  `#![deny(unsafe_op_in_unsafe_fn)]` and **every** `unsafe` block carries a `// SAFETY: …` comment
  (`doc-safety-section`, `lint-unsafe-doc`); the workspace denies
  `clippy::undocumented_unsafe_blocks`, and every `pub unsafe fn` has a `# Safety` doc section.
- **Lints are inherited from the workspace** (`lint-workspace-lints`): each crate's `Cargo.toml` has
  `[lints] workspace = true`. For intentional lint exceptions, prefer
  `#[expect(..., reason = "...")]` so stale suppressions fail the build; use `allow` only when a
  target/test cfg matrix cannot be represented as a checked expectation.
- **Module docs (`//!`) state the invariant the crate owns** (`doc-module-inner`). For the subsystem
  crates, the first lines of `lib.rs` must name the architecture invariant being enforced and, where
  relevant, the *build-time* guarantee (see §4).

---

## 2. The byte-identity contract (`layerstack`) — historical traps in `docs/contract/02-cas-byte-identity.md`

These two hashes are **correctness-bearing** (plan AV-1c). A wrong byte ⇒ silent data divergence
that passes every ASCII test. The golden fixtures in
`crates/daemon/layerstack/tests/fixtures/cas/cases.json` were produced by
executing the real Python and MUST all pass through
`layerstack/tests/cas_fixtures.rs`.

### 2a. `manifest_root_hash` — THE #1 TRAP: Python `ensure_ascii=True`
`sha256( serialize({"layers":[{"layer_id":..,"path":..}, ...]}) )` where `serialize` reproduces
`json.dumps(payload, sort_keys=True, separators=(",",":"))` — and Python's **default
`ensure_ascii=True`**. Only the `layers` array is hashed (NOT `version`/`schema_version`).

**`serde_json` emits raw UTF-8 and will SILENTLY DIVERGE on any non-ASCII byte.** Do NOT use
`serde_json::to_string` for this hash. Hand-build the exact byte string with this escaping
(verified against fixtures `manifest_unicode_bmp` = `b3d7d650…` and `manifest_unicode_nonbmp`):

For each JSON **string** value, between `"`…`"`, emit per `char`:
- `"` → `\"`,  `\` → `\\`
- ``→`\b`, `	`→`\t`, `
`→`\n`, ``→`\f`, ``→`\r`
- any other control char `< 0x20` → `\u00XX` (lowercase hex, 4 digits)
- `0x20..=0x7E` (except `"` and `\`) → the literal byte
- any char `>= 0x7F`: take its `u32` scalar; if `<= 0xFFFF` → `\uXXXX` (lowercase); if `> 0xFFFF`
  → UTF-16 surrogate pair `\uHHHH\uLLLL` where `hi = 0xD800 + ((c-0x10000)>>10)`,
  `lo = 0xDC00 + ((c-0x10000)&0x3FF)` (both lowercase hex). This matches Python exactly.
The object is `{"layer_id":<esc>,"path":<esc>}` with keys in sorted order (`layer_id` < `path`),
compact (no spaces), array in **given order** (order-sensitive — do NOT sort layers).
A `// PORT manifest.py:134-138` anchor goes on this fn. Add a `debug_assert!`-style unit test that
the escaper reproduces `{"layers":[{"layer_id":"Lunicodé","path":"layers/café"}]}`.

### 2b. `layer_digest` — raw UTF-8, the OPPOSITE of 2a
`sha256` over `aggregate_layer_changes(changes)`, feeding each change:
`kind_bytes + b"\0" + path_utf8_bytes + b"\0" + payload + b"\0"` where
`payload = write_content` (raw bytes, **write** kind only) | `source_path` UTF-8 (symlink kind) |
empty (delete/opaque_dir). The trailing `\0` is **always** present. **Paths here are raw UTF-8 —
do NOT escape them.** `source_path` is hashed for symlink only, **never for write** (write hashes
only `write_content`). `aggregate` = last-write-wins per path (a later same-path change of *any*
kind replaces), then emit in `sorted(path)` order (Rust `str` `Ord` == Python code-point order ==
UTF-8 byte order, so a plain sort matches). `kind` strings are `write`/`delete`/`symlink`/`opaque_dir`.
Use `sha2::Sha256`, feed via `Digest::update` over `&[u8]` slices (`mem-zero-copy` — no intermediate
`Vec` concatenation). `// PORT changes.py:145-165, publisher.py:144-158`.

### 2c. Path normalization is a SEPARATE locked surface
`normalize_layer_path` (`changes.py:27-40`): `\`→`/`, strip, drop `./` and empty parts, reject
absolute / `..` / NUL. Reproduce it as a `parse`-style constructor (`api-parse-dont-validate`):
`LayerPath::parse(&str) -> Result<LayerPath, _>` so an invalid path is unrepresentable downstream.

---

## 3. Wire protocol (`shared/protocol` / `daemon`) — live `crates/shared/protocol/PROTOCOL.md`, historical traps in `docs/contract/01-wire-protocol.md`

- **Framing**: one newline-delimited compact JSON object per message: `json.dumps(obj,
  separators=(",",":")) + "\n"`. For wire messages (not the CAS hash) `serde_json` with compact
  formatting matches (these payloads are ASCII op names + structured values). Provide
  `encode(&WireMessage) -> Vec<u8>` and `decode(&[u8]) -> Result<WireMessage, _>`.
- Request = `{"op": String, "invocation_id": String, "args": Object}`. The top-level
  `invocation_id` is canonical; host forwarding duplicates it into `args` only for daemon
  compatibility. The protocol-version field `_eos_daemon_protocol_version` lives **inside `args`**,
  is `1`, and the daemon **never reads it** (inert hook) — reproduce its presence, don't gate on it.
- Auth field `_eos_daemon_auth_token` is **top-level, TCP-only, conditional**; the server pops it
  before dispatch; AF_UNIX never carries it.
- Error response = `{"success":false,"warnings":[],"timings":{},"error":{"kind","message","details"}}`.
  Model `kind` as a `#[non_exhaustive]` enum (`api-non-exhaustive`) over the verified kinds
  (`invalid_request`,`bad_json`,`request_too_large`,`unauthorized`,`unknown_op`,`internal_error`,
  `forbidden`,`forbidden_in_isolated_workspace`,`lifecycle_in_progress`).
- **There is NO `ping` op.** Liveness is `sandbox.call.heartbeat` and
  readiness is `sandbox.runtime.ready`.
  Do NOT invent a `ping` op.
- Exit codes are constants: `CONNECT_FAILED = 97`, `IO_FAILED = 98`. `MAX_REQUEST_BYTES = 16 MiB`,
  `REQUEST_READ_TIMEOUT_S = 30.0`, `_CONNECT_RETRY_DELAYS_S = [0.25,0.5,1.0,2.0]`.
- **Canonical comparison (AV-1)**: operation responses use the `OperationEnvelope`
  shape and are compared with keys sorted. *Requests* and the CAS hashes are
  byte/structurally exact.

---

## 4. Crate structure & the build-time guarantees — live workspace graph first, historical traps in `docs/contract/06-crate-map-and-invariants.md`

- Workspace dep inheritance (`proj-workspace-deps`): declare versions once in
  `[workspace.dependencies]`; crates use `dep.workspace = true`. Internal crates are path deps.
- `proj-lib-main-split`: `eosd/src/main.rs` is subcommand dispatch only; all logic in libraries.
- Cargo package names such as `host`, `trace`, `protocol`, and `workspace`
  are private workspace implementation names, not a stable external API. Keep
  them unless a dedicated package-rename migration updates workspace members,
  dependency keys, imports, generated docs, and stale-name checks in one change.
- **The dependency edges ARE the architecture.** The single sharpest invariant —
  *isolated keeps writes private and NEVER publishes* — is encoded inside
  **`workspace::isolated_workspace`**, which must not own publish paths.
  static plugin support is intentionally narrow now: first-party provider
  request/response contracts live in `operation::plugin::contract`, while
  provider runtime code lives in the daemon-side `plugin` crate. There is no
  dynamic plugin crate or PPC/package pipeline. Verified edges (get these
  EXACTLY right):
  - `crates/daemon/operation/ops.json` → reviewed static op catalog.
  - `crates/shared/protocol/PROTOCOL.md` and
    `crates/shared/protocol/fixtures/` → protocol fixtures/prose only; no
    host/gateway/daemon implementation code.
  - `layerstack` → storage, leases, CAS hashes, route/commit policy.
  - `overlay` → overlayfs mechanics and captured path changes.
  - `namespace` → single-threaded namespace holder/runner support.
  - `workspace` → reusable per-operation overlay workspace helpers plus
    isolated session lifecycle, network setup, TTL/GC; isolated code must not
    publish workspace changes.
  - `command` / `operation::command` → command-process
    mechanics and command runtime policy.
  - `operation::core` → static operation contracts, catalog rendering, and
    shared operation outcome contracts.
  - `operation::file` → file operation semantics over direct and isolated
    backends.
  - `operation::plugin` → static first-party provider operation contracts.
  - `plugin` → daemon-side static provider runtime implementation, currently
    `pyright_lsp`.
  - `operation::checkpoint` → checkpoint commit pipeline.
  - `daemon` → transport, dispatch, wire-message codec, op adapters, service
    composition, daemon-owned plugin/checkpoint process glue.
  - `eosd` → binary subcommand dispatch over daemon/namespace/overlay support.
  - `xtask` is a workspace package for packaging and is not part of the runtime architecture graph.
- **Port traits invert the upward edges** (so the graph stays leaf→root). Lower crates define only
  the narrow ports they actually consume (for example `RouteProvider` and
  `CommitTransactionPort` in `layerstack`); `daemon` owns the concrete
  service composition and injections.

---

## 5. Async and syscall boundaries — `async-*`

- `tokio` is justified in `daemon` and `eosd`; `workspace` has Linux-target
  `tokio` only for rtnetlink/netlink helpers. `namespace` (both the holder and runner children)
  remains **single-threaded, syscall-only, NO tokio** (kernel requires a single-threaded caller
  for `unshare(CLONE_NEWUSER)` / `setns` into a userns — this is a correctness requirement, not a
  style choice).
- **Never hold a lock across `.await`** (`async-no-lock-await`, `anti-lock-across-await`): clone the
  data out, drop the guard, then await. The live OCC single-writer path is the
  dispatcher-owned per-root `OccService` cache; do not reintroduce a second
  daemon-side publish queue.
- Cancellation/teardown uses `tokio_util::sync::CancellationToken` (`async-cancellation-token`); the
  cancel path must kill the full process group (Python `start_new_session=True`).
- **The reentrant-lock trap**: the Python `storage_lock` uses a *reentrant* `threading.RLock`
  re-acquired on the same thread. A naive 1:1 port to `std::sync::Mutex` (non-reentrant) **deadlocks**.
  Restructure the re-entrant sections; reproduce BOTH lease layers (the `flock(LOCK_EX|LOCK_NB)`
  cross-process lease AND the in-process refcount + shared mutex). The current
  `layerstack::storage_lock` implementation uses a small reentrant guard;
  keep the module doc warning intact so future edits do not regress to a
  deadlocking 1:1 `Mutex` port.

---

## 6. Types, API, memory

- Newtypes for ids/handles (`type-newtype-ids`, `api-newtype-safety`): `InvocationId(String)`,
  `LayerId(String)`, `SandboxId(String)`, `Fd(RawFd)` etc. — don't pass bare `String`/`i32`.
  Validated values parse at the boundary (`api-parse-dont-validate`): `LayerPath`, `LayerId`.
- Syscall FD/handle wrappers that cross FFI use `#[repr(transparent)]` (`type-repr-transparent`) and
  own their cleanup via `Drop` (RAII) so a dropped mount/ns handle unmounts/closes.
- `#[non_exhaustive]` on public protocol enums/error kinds (`api-non-exhaustive`); derive
  `Debug, Clone, PartialEq, Eq` eagerly on data types (`api-common-traits`); `serde` derives gated or
  always-on as the crate needs (protocol types: always-on serde).
- Zero-copy on hot byte paths (`mem-zero-copy`): hash via `Digest::update(&[u8])` slices, accept
  `&[u8]`/`&str` not `&Vec`/`&String` (`own-slice-over-vec`); `Cow` for conditional ownership.

---

## 7. Testing — `test-*`

- Unit tests in-module under `#[cfg(test)] mod tests { use super::*; … }` (`test-cfg-test-module`).
- **contract fixture tests are mandatory and gate the build**: `layerstack`
  loads `crates/daemon/layerstack/tests/fixtures/cas/cases.json` and asserts
  every `expected` hash; `daemon` loads
  `crates/shared/protocol/fixtures/wire_messages/*.json` and asserts
  encode/decode round-trips plus canonical equality.
- Property tests (`test-proptest-properties`) for invariants: `decode(encode(x)) == x`;
  `aggregate` is idempotent and order-insensitive; the escaper never emits a non-ASCII byte.
- `#[tokio::test]` for daemon async tests (`test-tokio-async`); RAII fixtures for teardown
  (`test-fixture-raii`).

---

## 8. Cargo / build hygiene

- Edition 2021, `resolver = "2"`. Conservative deps (plan §1): `serde`, `serde_json`, `sha2`,
  `thiserror`; `rustix`/`nix` + `libc` only in syscall crates; `tokio`/`tokio-util`/`tracing` only
  in `daemon` (+ `eosd`); `proptest`/`criterion` dev-only. Do not add deps beyond what a crate
  needs.
- Release profile (workspace root): `opt-level=3, lto="fat", codegen-units=1, panic="abort",
  strip=true` (`perf-release-profile`) — static-musl artifact.
- Every crate must `cargo fmt`-clean and `cargo clippy`-clean (workspace lints). On **macOS** only
  the non-Linux `cfg` surface compiles; gate syscall bodies behind `#[cfg(target_os = "linux")]`
  with `#[cfg(not(target_os="linux"))]` typed unsupported or no-op parity arms so
  `cargo check --workspace` is green on the dev host. Use `todo!("PORT: ...")` only for true
  deferred ports, never for implemented cfg parity stubs. Real Linux/musl + runtime checks happen
  in CI / the dask image.
