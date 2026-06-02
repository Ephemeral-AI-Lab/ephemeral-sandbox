# impl-workspace — agent-core Cargo workspace scaffolding + Phase-0 parity harness

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §Target Workspace,
> §Migration Phases (Phase 0), §Tool Description and Schema Conversion.

This doc owns no Rust *crate* — it owns the **workspace root**
(`agent-core/Cargo.toml`), the shared `[workspace.dependencies]` /
`[workspace.lints]` / profiles, the 15 crate skeletons + their `Cargo.toml`
dependency edges, the toolchain pin, the fmt/clippy CI, and the Phase-0 parity
harness (`agent-core/parity/`). No domain logic lives here; every crate body is
specified in its own `impl-<crate>.md`.

**Workspace location:** the `agent-core/` workspace is placed at the repository
root — `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/agent-core/` — as a
sibling of `backend/` (the Python control plane being migrated) and the existing
`sandbox/` Rust daemon workspace. Every path in this doc (`agent-core/Cargo.toml`,
`crates/…`, `parity/…`) is relative to that root. `agent-core` is its own
workspace, **not** a member of the `sandbox/` workspace; the two share version
pins by convention (§2), not a common `Cargo.toml`.

---

## 1. Purpose & Responsibility (SRP)

The single responsibility of this spec is **making the agent-core workspace
exist, compile empty, and lint clean**, plus standing up the **parity harness**
that pins current Python behavior as committed fixtures before any port begins.
It defines: the workspace `Cargo.toml`, shared dependency versions, workspace
lints, dev/release/bench profiles (with the `panic` decision resolved below),
the 15 member crates and the **exact `[dependencies]` edges** between them
(encoding the dependency DAG as code, including the anchor §5a
`eos-tools -> eos-llm-client` edge), rustfmt/clippy config, the CI lint job, and
the snapshot/fixture/golden test corpus under `agent-core/parity/`.

This spec must **NOT**: implement any type, trait, store, client, tool, or
state machine (those belong to the per-crate docs); add no dependency edge
beyond the plan's DAG closed under (a) composition-root concrete wiring in
`eos-runtime` and (b) anchor §5 / §5a contract-ownership edges; introduce a
binary with logic (`eos-runtime` owns
the only `main.rs`, specified in impl-eos-runtime.md — here it is an empty
skeleton); or re-specify any contract owned by another crate.

---

## 2. Dependencies

This is the workspace root. It has no upstream crate dependencies and no
downstream consumers — it *contains* all 15 crates. The "dependencies" it owns
are the **shared external-crate versions** declared once in
`[workspace.dependencies]` and inherited by members via `dep.workspace = true`
(`proj-workspace-deps`). Crate names carry **no `-rs` suffix**
(`name-crate-no-rs`); this matches the sibling `sandbox/` workspace's `eos-*`
naming.

| External crate | Version (pin in workspace) | Justification | rust-skills |
|---|---|---|---|
| `tokio` | `1`, features `["rt-multi-thread","macros","sync","time","fs","io-util","tracing"]` | single multi-thread runtime created in `eos-runtime`; lower crates only `async fn`; runtime resource spans available for optional console debugging | `async-tokio-runtime` |
| `tokio-util` | `0.7`, feature `["rt"]` | `CancellationToken` for engine background supervisor + parent-exit cancel | `async-cancellation-token` |
| `futures` | `0.3` | provider stream modeled as `impl Stream<Item=Result<LlmStreamEvent,_>>` | (§7 streaming) |
| `serde` | `1`, feature `["derive"]` | Pydantic → serde structs on every wire/DTO type | plan §Design Rules |
| `serde_json` | `1` | `JsonObject = serde_json::Map<String,Value>`; SQLite TEXT-JSON columns | anchor §4 |
| `schemars` | `0.8` | tool input/output JSON schema from Rust structs (no docstring fallback) | anchor §10 |
| `sqlx` | `0.8`, `default-features=false`, features `["runtime-tokio","sqlite","macros","migrate"]` — **sqlite only** | SQLAlchemy → SQLite-only repos + migrations; no Postgres | anchor §2 non-goal |
| `reqwest` | `0.12`, `default-features=false`, features `["json","stream","rustls-tls"]` | direct Anthropic/OpenAI HTTP/SSE, no SDK | plan §6 |
| `thiserror` | `2` | one error enum per library crate | `err-thiserror-lib` |
| `anyhow` | `1` | **only** `eos-runtime` (top-level wiring) | `err-anyhow-app` |
| `tracing` | `0.1` | structured spans without dumping tool args/secrets | plan §API Client Layer |
| `tracing-subscriber` | `0.3`, features `["env-filter","fmt","json"]` | app-level subscriber setup in `eos-runtime`; env-filtered text/JSON output | anchor §7 observability |
| `time` | `0.3`, features `["serde","formatting","parsing","macros"]` | `UtcDateTime` wraps `time::OffsetDateTime` | anchor §4 / plan §1 |
| `async-trait` | `0.1` | `dyn`-safe async traits at the composition root (native AFIT not `dyn`-safe) | anchor §6 |
| `bytes` | `1` | zero-copy SSE buffer slices | `mem-zero-copy` |
| `parking_lot` | `0.12` | `Mutex`/`RwLock` for short **synchronous** (non-await) critical sections in `eos-workflow`/`eos-sandbox-host` — `!Send` guard (hold-across-await = compile error), no poison under `panic=unwind`, smaller/faster; `tokio::sync::*` is reserved for guards that must span an `.await` (anchor §7) | `own-mutex-interior` |
| `uuid` | `1`, features `["v4","serde"]` | typed ID helpers plus DB/runtime ID generation | impl-eos-types, impl-eos-db, impl-eos-runtime |

Dev-only (also workspace-inherited, `[dev-dependencies]`):

| Dev crate | Version | Purpose | rust-skills |
|---|---|---|---|
| `insta` | `1`, feature `["json"]` | snapshot assertions for Pydantic-schema / SQLite-schema / prompt-report goldens | anchor §11 Phase-0 |
| `proptest` | `1` | property tests for parsers/projections | `test-proptest-properties` |
| `tokio` (test) | inherits, `["macros","rt-multi-thread"]` | `#[tokio::test]` | `test-tokio-async` |
| `wiremock` | `0.6` | provider SSE replay servers (Phase 2 leans on it; introduced here so the version is pinned) | plan §Phase 2 |
| `pretty_assertions` | `1` | readable golden diffs | — |
| `loom` | `0.7` | model-check small lock/channel/state-machine components where interleavings matter | anchor §7 observability |

Optional dev feature:

| Optional crate | Version | Purpose | Feature |
|---|---|---|---|
| `console-subscriber` | `0.4` | `tokio-console` resource/task debugging for stuck async work | `tokio-console` |

Versions follow the sibling `sandbox/` workspace where the crates overlap
(`thiserror = 2`, `tokio = 1`, `serde`/`serde_json`, `tracing`, `proptest`,
`anyhow`) so the version pins do not drift (`proj-workspace-deps`); feature sets
and the `futures` vs `futures-util` choice diverge (sibling uses `tokio = ["full"]`
and `futures-util`) and would still need reconciliation in a future merge. No crate is
declared `optional` at the workspace level (Cargo forbids it; gate per-member if
ever needed).

---

## 3. Scope & Source Mapping

There is no 1:1 Python→Rust file map here — Phase 0 produces **no domain code**.
The mapping is *Python observable behavior → committed fixture*:

| Source (Python) | Parity artifact (committed) | What is captured / dropped |
|---|---|---|
| Pydantic `model_json_schema()` for `ToolSpec` inputs, `Message`/content blocks, sandbox request/result DTOs | `parity/schemas/<name>.schema.json` | exact JSON Schema; drops nothing — these are the contract goldens |
| `backend/src/db/engine.py` live DDL + SQLAlchemy models (`requests`,`tasks`,`workflows`,`iterations`,`attempts`,`agent_runs`,`model_registrations`) | `parity/sqlite/schema.sql` (canonicalized `sqlite_master` dump) | table/column/constraint shape incl. unique constraints; drops the live DDL patching the target replaces with clean versioned migrations |
| Anthropic native client SSE (`message_start`…`message_stop`) | `parity/sse/anthropic/*.sse` | raw byte stream replay input |
| OpenAI Responses SSE (`response.output_text.delta`, tool-arg deltas, done) | `parity/sse/openai/*.sse` | raw byte stream replay input |
| `PromptReportRecorder` JSONL (`llm_request`/`assistant`/`tool_results`) | `parity/prompt_report/*.jsonl` | golden transcript with the `system`-role bug **left intact in the fixture** and a note that the Rust port fixes it (anchor §4) |

In scope: workspace + crate skeletons + dependency wiring + lints + profiles +
toolchain + CI + parity corpus + a tiny snapshot-test crate that asserts the
fixtures stay stable. Out of scope: anything inside a crate's `src/` beyond a
`lib.rs` that compiles (a `//! placeholder` doc-comment + re-export stubs), and
any of the deep-sandbox `eos-daemon`/`eos-overlay`/etc. crates which live in the
separate `sandbox/` workspace (anchor §2 non-goal).

---

## 4. File & Module Layout

```text
agent-core/
  Cargo.toml                 # [workspace] root: members, deps, lints, profiles
  rust-toolchain.toml        # channel = "stable", components rustfmt+clippy
  rustfmt.toml               # edition 2021, max_width 100
  clippy.toml                # msrv = "1.85"
  .gitignore                 # /target
  crates/
    eos-types/   { Cargo.toml, src/lib.rs }   # no internal deps (leaf)
    eos-config/  { Cargo.toml, src/lib.rs }   # -> eos-types
    eos-state/   { Cargo.toml, src/lib.rs }   # -> eos-types
    eos-db/      { Cargo.toml, src/lib.rs, migrations/ } # -> eos-state, eos-config
    eos-audit/   { Cargo.toml, src/lib.rs }   # -> eos-types
    eos-llm-client/   { Cargo.toml, src/lib.rs } # -> eos-types, eos-config
    eos-agent-def/    { Cargo.toml, src/lib.rs } # -> eos-types
    eos-sandbox-api/  { Cargo.toml, src/lib.rs } # -> eos-types
    eos-skills/       { Cargo.toml, src/lib.rs } # -> eos-types, eos-config
    eos-tools/        { Cargo.toml, src/lib.rs } # -> eos-state, eos-sandbox-api, eos-skills, eos-audit, eos-llm-client
    eos-engine/       { Cargo.toml, src/lib.rs } # -> eos-llm-client, eos-tools, eos-audit, eos-agent-def
    eos-workflow/     { Cargo.toml, src/lib.rs } # -> eos-state, eos-tools, eos-agent-def, eos-audit
    eos-sandbox-host/ { Cargo.toml, src/lib.rs } # -> eos-sandbox-api, eos-config
    eos-plugin-catalog/ { Cargo.toml, src/lib.rs } # -> eos-audit
    eos-runtime/      { Cargo.toml, src/lib.rs, src/main.rs } # composition root -> all crates (see §5 table)
  parity/
    Cargo.toml                 # member crate `eos-parity` (dev/test only)
    src/lib.rs                 # //! empty; harness lives in tests/
    tests/dependency_dag.rs    # cargo metadata eos-* edge set vs §5 table (AC-workspace-02)
    tests/profiles.rs          # ../Cargo.toml profile keys incl. panic=unwind (AC-workspace-04)
    tests/schema_snapshots.rs  # insta vs parity/schemas/*.schema.json
    tests/sqlite_schema.rs     # canonical sqlite_master vs parity/sqlite/schema.sql
    tests/sse_fixtures.rs      # presence/shape guard over parity/sse/**
    tests/prompt_report.rs     # golden over parity/prompt_report/*.jsonl
    schemas/   *.schema.json
    sqlite/    schema.sql
    sse/       anthropic/*.sse  openai/*.sse
    prompt_report/  *.jsonl
```

Each skeleton `lib.rs` is the minimal compilable shape:

```rust
//! eos-types — shared IDs, timestamps, JSON helpers, and CoreError.
//! Phase-0 skeleton: contracts are specified in impl-eos-types.md.
#![forbid(unsafe_code)]
```

`eos-runtime` is the only crate with a `main.rs`; per `proj-lib-main-split` it
stays a thin `fn main() -> anyhow::Result<()>` that defers to `eos_runtime::run`
(the body is impl-eos-runtime.md's concern). All public surface is re-exported
from each `lib.rs` (`proj-pub-use-reexport`); internals are `pub(crate)`
(`proj-pub-crate-internal`). The `parity` crate (`eos-parity`) carries no public
API — it exists only to host `tests/`.

---

## 5. Contracts Owned Here

This spec owns **no trait or domain type** — those are owned per the Contract
Ownership Map (anchor §5). What it owns is the **build contract**, expressed as
Cargo manifests. The authoritative artifact is the workspace `Cargo.toml`:

```toml
# agent-core/Cargo.toml
[workspace]
resolver = "2"
members = [
  "crates/eos-types", "crates/eos-config", "crates/eos-state",
  "crates/eos-db", "crates/eos-audit", "crates/eos-llm-client",
  "crates/eos-agent-def", "crates/eos-sandbox-api", "crates/eos-skills",
  "crates/eos-tools", "crates/eos-engine", "crates/eos-workflow",
  "crates/eos-sandbox-host", "crates/eos-plugin-catalog", "crates/eos-runtime",
  "parity",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.85"
license = "MIT"

[workspace.dependencies]
# external (versions pinned once)
tokio       = { version = "1", features = ["rt-multi-thread","macros","sync","time","fs","io-util","tracing"] }
tokio-util  = { version = "0.7", features = ["rt"] }
futures     = "0.3"
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
schemars    = "0.8"
sqlx        = { version = "0.8", default-features = false, features = ["runtime-tokio","sqlite","macros","migrate"] }
reqwest     = { version = "0.12", default-features = false, features = ["json","stream","rustls-tls"] }
thiserror   = "2"
anyhow      = "1"
tracing     = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter","fmt","json"] }
time        = { version = "0.3", features = ["serde","formatting","parsing","macros"] }
async-trait = "0.1"
bytes       = "1"
parking_lot = "0.12"
uuid        = { version = "1", features = ["v4","serde"] }
console-subscriber = "0.4"
# dev
insta       = { version = "1", features = ["json"] }
proptest    = "1"
wiremock    = "0.6"
pretty_assertions = "1"
loom        = "0.7"
# internal crates — these path edges ARE the dependency DAG
eos-types          = { path = "crates/eos-types" }
eos-config         = { path = "crates/eos-config" }
eos-state          = { path = "crates/eos-state" }
eos-db             = { path = "crates/eos-db" }
eos-audit          = { path = "crates/eos-audit" }
eos-llm-client     = { path = "crates/eos-llm-client" }
eos-agent-def      = { path = "crates/eos-agent-def" }
eos-sandbox-api    = { path = "crates/eos-sandbox-api" }
eos-skills         = { path = "crates/eos-skills" }
eos-tools          = { path = "crates/eos-tools" }
eos-engine         = { path = "crates/eos-engine" }
eos-workflow       = { path = "crates/eos-workflow" }
eos-sandbox-host   = { path = "crates/eos-sandbox-host" }
eos-plugin-catalog = { path = "crates/eos-plugin-catalog" }
eos-runtime        = { path = "crates/eos-runtime" }
```

The per-member `Cargo.toml` files encode the edges. Example for the two
DAG-critical crates (note the anchor §5a `eos-tools -> eos-llm-client` edge):

```toml
# crates/eos-tools/Cargo.toml
[package]
name = "eos-tools"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
eos-state.workspace      = true
eos-sandbox-api.workspace = true
eos-skills.workspace     = true
eos-audit.workspace      = true
eos-llm-client.workspace = true   # §5a: authors ToolSpec (neutral, acyclic)
serde.workspace      = true
serde_json.workspace = true
schemars.workspace   = true
thiserror.workspace  = true
async-trait.workspace = true

[lints]
workspace = true
```

The **complete edge set** (single source of truth; mirror into `overview.md`):

| Crate | `[dependencies]` internal edges |
|---|---|
| eos-types | — |
| eos-config | eos-types |
| eos-state | eos-types |
| eos-db | eos-state, eos-config |
| eos-audit | eos-types |
| eos-llm-client | eos-types, eos-config |
| eos-agent-def | eos-types |
| eos-sandbox-api | eos-types |
| eos-skills | eos-types, eos-config |
| eos-tools | eos-state, eos-sandbox-api, eos-skills, eos-audit, **eos-llm-client** |
| eos-engine | eos-llm-client, eos-tools, eos-audit, eos-agent-def |
| eos-workflow | eos-state, eos-tools, eos-agent-def, eos-audit |
| eos-sandbox-host | eos-sandbox-api, eos-config |
| eos-plugin-catalog | eos-audit |
| eos-runtime | eos-db, eos-engine, eos-workflow, eos-sandbox-host, eos-plugin-catalog, eos-skills, eos-config, eos-agent-def, eos-sandbox-api, eos-state, eos-types, eos-llm-client, eos-tools, eos-audit, anyhow |

The plan's terse DAG (lines 90-106) omits a few transitive edges that the
composition root needs directly; `eos-runtime` therefore lists every crate it
constructs concretely (DIP: it is the only place `Arc<dyn Trait>` concretes are
wired). `eos-llm-client` depends on `eos-config` to read provider retry defaults
(plan §4 gap closeout "move provider retry defaults into config"). `eos-engine`
holds no `eos-state` edge: it deals in `ToolResult`/`EphemeralRunResult` and
typed IDs (`eos-types`); persisting terminal results against domain state is the
`eos-workflow`/`eos-runtime` concern (matches the plan DAG, overview §4, and
impl-eos-engine.md §2). All edges are acyclic — verified by `cargo metadata` in
CI (AC-workspace-02).

---

## 6. Types, Fields & Schemas

No domain types. The only declarative artifacts owned here are the **profiles**,
**lints**, and the **parity fixture schemas** (which are *captured*, not
authored). Profiles (`opt-lto-release`, `opt-codegen-units`):

```toml
# agent-core/Cargo.toml (continued)
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
panic = "unwind"        # see GC-workspace-04 — deliberately NOT abort

[profile.dev]
opt-level = 0
debug = true

[profile.dev.package."*"]
opt-level = 3            # optimize deps even in dev (faster test runs)

[profile.bench]
inherits = "release"
debug = true
strip = false

[workspace.lints.clippy]
correctness = { level = "deny",  priority = -1 }
suspicious  = { level = "deny",  priority = -1 }
style       = { level = "warn",  priority = -1 }
complexity  = { level = "warn",  priority = -1 }
perf        = { level = "warn",  priority = -1 }
# selective pedantic (lint-pedantic-selective)
doc_markdown = "warn"
needless_pass_by_value = "warn"
semicolon_if_nothing_returned = "warn"
redundant_closure_for_method_calls = "warn"
# restriction (selective) — production crates
unwrap_used = "warn"
dbg_macro   = "warn"
print_stdout = "warn"
undocumented_unsafe_blocks = "deny"
await_holding_lock = "deny"   # std/parking_lot guard held across .await is a bug (anchor §7)

[workspace.lints.rust]
unsafe_code = "forbid"     # no agent-core crate needs unsafe; sandbox owns FFI
unused_must_use = "warn"
missing_debug_implementations = "warn"

[workspace.lints.rustdoc]
broken_intra_doc_links = "deny"
```

Lint-group entries carry `priority = -1` so the individual per-lint levels win
(`lint-workspace-lints`; this is the exact pattern the sibling `sandbox/`
workspace already uses — clippy's `lint_groups_priority` gate). `correctness`
is **deny** (`lint-deny-correctness`). `missing_docs` is **not** enabled
workspace-wide (it would block empty Phase-0 skeletons); it is added per-crate
in each `impl-<crate>.md` once that crate has public items.
`missing_debug_implementations` stays `warn` (hence deny-under-`-D warnings`)
rather than per-crate-deferred because anchor §9 already mandates deriving
`Debug` eagerly on all public types (`api-common-traits`), so a new public type
satisfies it by construction — unlike `missing_docs`, it imposes no doc-writing
burden on Phase-0 skeletons (which have no public types yet).

Per-crate lint override needed by the binary (`eos-runtime` top-level wiring may
`unwrap` during startup is still discouraged — keep `unwrap_used = "warn"`; do
**not** allow it). The `eos-parity` test crate overrides
`unwrap_used = "allow"` and `print_stdout = "allow"` (test code).

Parity schema fixtures are byte-stable JSON captured from Python; their "fields"
are whatever the current Pydantic models emit. They are treated as **opaque
goldens** — this spec does not enumerate `ToolSpec`/`Message` fields (those are
owned by impl-eos-llm-client.md / impl-eos-tools.md). The harness only asserts
*the Rust port reproduces them*, which is a later-phase obligation; Phase 0
merely commits them and guards them against accidental edits.

---

## 7. Concurrency & State Ownership

Phase-0 scaffolding introduces **no runtime concurrency** — the skeletons are
empty and the parity tests are synchronous file/JSON comparisons (or
`#[tokio::test]` only where a future SSE-replay test needs an async server).
What this spec *fixes in place* for every downstream crate:

- **Runtime ownership:** the single Tokio multi-thread runtime is created **only**
  in `eos-runtime` (`async-tokio-runtime`); library crates take `&self`/`async fn`
  and never call `Runtime::new`, `#[tokio::main]`, or `tokio::spawn` a runtime.
  This is a **review/lint convention, not feature-enforced**: Cargo feature
  unification links one `tokio` with the union of every member's features
  (including `eos-runtime`'s `rt-multi-thread`), so a "sync-only" crate would still
  compile a `Runtime::new`/`spawn` call. The invariant is upheld by code review
  (and, if teeth are wanted, a CI grep/clippy gate banning those symbols outside
  `crates/eos-runtime`), not by `default-features = false` on a lower crate.
- **Shared immutable state** (config, registries) is `Arc<T>` (`own-arc-shared`);
  this spec only guarantees `tokio-util` (for `CancellationToken`) and `futures`
  (for `Stream`) are available so `eos-engine`'s background supervisor
  (`JoinSet` + `CancellationToken` + bounded `mpsc`/`oneshot`/`watch`, anchor §7)
  has its primitives pinned at consistent versions.
- **No app-level DB mutex:** `sqlx` `SqlitePool` is the only DB concurrency
  primitive; the workspace pins it with `sqlite` + `runtime-tokio` so every crate
  agrees on one async SQLite stack (anchor §7 DB).
- **Lock discipline** (`async-no-lock-await`/`anti-lock-across-await`) is enforced
  by clippy `await_holding_lock = "deny"` (§6) — which fires on `std`/`parking_lot`
  guards held across `.await` — plus review; this spec adds no lock itself. Per
  anchor §7, synchronous critical sections use `parking_lot` (its `!Send` guard
  also makes hold-across-await a compile error), reserving `tokio::sync` locks for
  the rare guard that must span an `.await`.

The `eos-parity` crate owns its fixture files immutably; tests read them, never
mutate them. `insta` snapshot review is the only "state change" and it is
developer-driven (`cargo insta review`), never at test runtime.

---

## 8. Behavior & Invariants

- **Empty workspace must build and lint clean from day one.**
  `cargo build --workspace`, `cargo fmt --check`, and
  `cargo clippy --workspace --all-targets -- -D warnings` all pass with only
  skeleton `lib.rs` files (plan §Phase 0 Verification).
- **Dependency DAG is acyclic and matches the plan.** The edge set in §5 is the
  contract; `cargo` itself rejects cycles, and a CI `cargo metadata` assertion
  pins the edge set so a stray dependency (e.g. someone adding
  `eos-state -> eos-db`, which would invert the layering) fails CI
  (AC-workspace-02). The `eos-tools -> eos-llm-client` edge (anchor §5a) is
  **required**, not optional: `ToolSpec` is owned by `eos-llm-client`.
- **Parity fixtures are frozen Python truth.** They are captured *before* porting
  and are the comparison target for Phases 1-4. The prompt-report fixture
  deliberately preserves the `system`-role transcript bug (anchor §4) so the Rust
  fix is a visible, reviewed diff later — not an accidental silent change.
- **SQLite schema snapshot reflects the *clean* target.** `engine.py` applies live
  DDL patches the target replaces with versioned migrations; the snapshot
  canonicalizes the *resulting* `sqlite_master` so eos-db's versioned migrations
  have a fixed target (plan §3 gap closeout "replace live DDL patching").
- **No network DB.** The harness only ever opens `sqlite::memory:` or a temp file
  (anchor §2: a network DB URL is rejected — that rejection is eos-config/eos-db
  behavior, asserted there, but the harness must never depend on one).
- **`panic = "unwind"` is load-bearing**, see GC-workspace-04: the engine query
  loop and background supervisor recover from per-task/per-attempt failures; an
  `abort` strategy would turn a single tool/stream panic into a process kill,
  losing all sibling background tasks and persisted-state coherence. This
  *diverges* from the sibling `sandbox/` workspace (which uses `panic = "abort"`
  for a small static daemon binary) — the divergence is intentional and recorded.

---

## 9. SOLID & Principles Applied

- **SRP** — the workspace boundaries *are* the SRP partition (anchor §5). This
  spec's only job is to draw those boundaries in Cargo and forbid edges that
  cross them the wrong way. It deliberately owns no logic.
- **DIP** — the dependency edges flow from concrete down to abstract leaves
  (`eos-types` at the bottom; `eos-runtime` at the top as the composition root).
  `async-trait` is pinned here precisely so the `dyn`-safe trait seams (anchor §6
  `LlmClient`, `Store`, `ToolExecutor`, `SandboxTransport`, `AuditSink`,
  `ProviderAdapter`, `EventSource`) can be wired behind `Arc<dyn _>` in
  `eos-runtime`.
- **OCP** — registries (tool/agent/provider/skill/plugin) are the extension
  points; the workspace adds none of its own and adds no `match`-on-string
  dispatch.
- **ISP / LSP** — not exercised at scaffolding time; the workspace simply makes
  the per-entity-store and provider-neutral-type crates separable so those
  principles can be honored inside them.
- **KISS / YAGNI** — exactly 15 crates + 1 test crate, no `xtask`, no codegen
  build scripts, no feature flags beyond what a dependency strictly requires, no
  `[patch]` table, no nested workspaces. Parity is plain committed files + `insta`
  rather than a bespoke harness binary. The SSE frame parser is hand-rolled in
  `eos-llm-client` (impl-eos-llm-client.md); `eventsource-stream` (the plan's
  alternative suggestion) is intentionally not added — no extra dependency for a
  small `event:`/`data:` parser (anchor §5 `sse.rs`).
- **DRY** — every external version is declared once (`proj-workspace-deps`); lints
  and profiles are declared once and inherited via `[lints] workspace = true`.

Non-goals respected: no Postgres feature on `sqlx` (sqlite-only); no
SDK dependency (Anthropic/OpenAI go through `reqwest`); no global orchestrator
crate; no synthetic-root-workflow crate; no separate tool-visibility crate.

---

## 10. Gap Closeouts (tracked requirements)

The plan's Phase-0 obligations and the cross-cutting workspace items, turned into
tracked requirements:

- **GC-workspace-01** — *Create the `agent-core` workspace + 15 crate skeletons
  with the exact DAG.* Resolution: workspace `Cargo.toml` from §5 with the member
  list and `[workspace.dependencies]` path edges; each member's `[dependencies]`
  encodes only its row in the §5 edge table. Proven by AC-workspace-01/02.
- **GC-workspace-02** — *Encode the anchor §5a `eos-tools -> eos-llm-client` edge.*
  Resolution: `eos-tools/Cargo.toml` lists `eos-llm-client.workspace = true`;
  `ToolSpec` is NOT redefined in eos-tools. Acyclicity proven by AC-workspace-02.
- **GC-workspace-03** — *Add rustfmt, clippy, and CI hooks.* Resolution:
  `rust-toolchain.toml` (stable + rustfmt + clippy), `rustfmt.toml`,
  `clippy.toml` (`msrv=1.85`), and a CI job running `cargo fmt --check` +
  `cargo clippy --workspace --all-targets -- -D warnings`
  (`lint-rustfmt-check`). Proven by AC-workspace-03.
- **GC-workspace-04** — *Resolve `panic = unwind` vs `abort` and justify.*
  Resolution: **`panic = "unwind"`** in `[profile.release]`; `[profile.bench]`
  inherits release and must not redeclare `panic`, because Cargo ignores that key
  in the bench profile and emits a warning.
  Justification: the engine query loop and background supervisor (anchor §7,
  plan §8) recover from per-call and per-task failures; `abort` would escalate a
  single tool/SSE-parse/attempt panic to a whole-process kill, destroying
  in-flight sibling background tasks and the persisted-state ordering guarantees
  the plan calls "subtle." This intentionally differs from the sibling
  `sandbox/` daemon workspace (`abort`, a tiny static binary) and the rust-skills
  `opt-lto-release` example default (`abort`) — the recovery requirement
  overrides the marginal binary-size/perf gain. Recorded in `overview.md`.
- **GC-workspace-05** — *Pydantic JSON-schema snapshots.* Resolution:
  `parity/schemas/*.schema.json` captured via a one-shot
  `uv run python -m ...model_json_schema` dump (documented in
  `parity/README` *only if needed*; the committed JSON is the artifact), guarded
  by `tests/schema_snapshots.rs` with `insta`. Proven by AC-workspace-05.
- **GC-workspace-06** — *Provider SSE fixtures.* Resolution:
  `parity/sse/anthropic/*.sse` and `parity/sse/openai/*.sse` raw byte captures;
  `tests/sse_fixtures.rs` guards presence + non-empty + UTF-8 framing. Replay
  assertions are Phase-2 (impl-eos-llm-client.md). Proven by AC-workspace-06.
- **GC-workspace-07** — *Prompt-report goldens.* Resolution:
  `parity/prompt_report/*.jsonl` capturing `llm_request`/`assistant`/`tool_results`
  events, with the `system`-role bug preserved and annotated. Guarded by
  `tests/prompt_report.rs`. Proven by AC-workspace-07.
- **GC-workspace-08** — *SQLite schema snapshots.* Resolution:
  `parity/sqlite/schema.sql` = canonicalized `sqlite_master` for the seven target
  tables incl. unique constraints (`agent_runs.task_id`,
  `iterations(workflow_id,sequence_no)`, `attempts(iteration_id,attempt_sequence_no)`);
  `tests/sqlite_schema.rs` guards it. Proven by AC-workspace-08.
- **GC-workspace-09** — *No `-rs` suffix; align with sibling workspace.*
  Resolution: all crate names are `eos-<area>` (`name-crate-no-rs`); version pins
  match `sandbox/` where the crates overlap (feature sets and the
  `futures`/`futures-util` choice would still need reconciliation in a merge).
- **GC-workspace-10** — *Observability/debugging from day one.* Resolution:
  workspace deps include `tracing`, `tracing-subscriber`, Tokio's `tracing`
  feature, optional `console-subscriber` for `tokio-console`, and dev `loom`.
  `eos-runtime` owns subscriber initialization.

---

## 11. Acceptance Criteria

TDD: each criterion names a test/command written (or fixture committed) **before**
the thing it proves. Phase-0 "tests" are mostly the build/lint commands plus the
parity-guard tests; they map to plan §Phase 0 Verification and seed the
anchor §11 "Tests to Port First" corpus.

- **AC-workspace-01** — `cargo build --workspace` succeeds with only skeleton
  `lib.rs`/`main.rs` files. *Proof:* CI `build` step (and local smoke). Maps to
  plan §Phase 0.
- **AC-workspace-02** — the internal dependency edge set equals §5 exactly and is
  acyclic; adding a layering-violating edge fails. *Proof:*
  `parity/tests/dependency_dag.rs` parses `cargo metadata --format-version=1`,
  extracts `eos-*` → `eos-*` edges, and `assert_eq!`s against a hardcoded
  expected set (including `eos-tools -> eos-llm-client`).
- **AC-workspace-03** — `cargo fmt --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` both pass.
  *Proof:* CI `lint` job (`lint-rustfmt-check`). Maps to plan §Phase 0
  Verification bullets 1-2.
- **AC-workspace-04** — release profile sets `panic = "unwind"`, `lto="fat"`,
  `codegen-units=1`, `strip=true`; bench profile inherits release and does not
  declare ignored `panic`. *Proof:*
  `parity/tests/profiles.rs` reads `../Cargo.toml` and asserts the profile keys
  (guards GC-workspace-04 against regression to `abort`).
- **AC-workspace-05** — committed Pydantic schema JSON matches an `insta`
  snapshot; an edit to a fixture fails the test until reviewed. *Proof:*
  `parity/tests/schema_snapshots.rs` (`insta::assert_json_snapshot!`). Maps to
  plan §Phase 0 "Rust schema snapshots match current Pydantic schemas."
- **AC-workspace-06** — SSE fixture corpus exists, is non-empty, and parses as
  `event:`/`data:` framed UTF-8. *Proof:* `parity/tests/sse_fixtures.rs`. Seeds
  eos-llm-client AC (anchor §11: "Anthropic + OpenAI SSE fixtures").
- **AC-workspace-07** — prompt-report golden round-trips as valid JSONL with the
  three event kinds present and the `system`-role anomaly annotated. *Proof:*
  `parity/tests/prompt_report.rs`. Seeds eos-engine "prompt-report golden"
  (anchor §11).
- **AC-workspace-08** — SQLite schema snapshot lists the seven tables and the
  three unique constraints; deviation fails. *Proof:*
  `parity/tests/sqlite_schema.rs`. Seeds eos-db "store roundtrips" target
  (anchor §11).
- **AC-workspace-09** — `cargo test -p eos-parity` runs all guard tests green in
  CI. *Proof:* CI `test` step over the parity crate only (the 15 domain crates
  have no tests yet).
- **AC-workspace-10** — `agent-core/Cargo.toml` pins observability/debug deps:
  Tokio `tracing` feature, `tracing-subscriber`, optional `console-subscriber`,
  and dev `loom`; `cargo check --workspace --features eos-runtime/tokio-console`
  succeeds. *Proof:* CI `observability` smoke.

---

## 12. Implementation Checklist

Ordered, small, individually verifiable (`small-incremental-changes`):

1. Create `agent-core/Cargo.toml` `[workspace]` with `members` + empty
   `[workspace.dependencies]`; add `crates/eos-types/{Cargo.toml,src/lib.rs}`
   only. → verify: `cargo build -p eos-types`.
2. Add the remaining 14 crate skeletons (each `lib.rs` a `//! placeholder`,
   `#![forbid(unsafe_code)]`), each with `[lints] workspace = true` and *only*
   its §5 internal edges. → verify: `cargo build --workspace`.
3. Fill `[workspace.dependencies]` with the §2/§5 external + internal pins; switch
   members to `dep.workspace = true`. → verify: `cargo build --workspace`.
4. Add `[workspace.lints]`, `[profile.*]` (with `panic="unwind"`),
   `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`. → verify:
   `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
5. Add the `eos-parity` member crate (empty `lib.rs`, `tests/` dir,
   `unwrap_used=allow` override). → verify: `cargo test -p eos-parity` (no tests
   yet, passes).
6. Capture Pydantic schema JSON, SSE byte streams, prompt-report JSONL, and the
   canonical SQLite `schema.sql` from the running Python backend; commit under
   `parity/{schemas,sse,prompt_report,sqlite}/`.
7. Write the guard tests (`dependency_dag.rs`, `profiles.rs`,
   `schema_snapshots.rs`, `sse_fixtures.rs`, `prompt_report.rs`,
   `sqlite_schema.rs`); accept the initial `insta` snapshots. → verify:
   `cargo test -p eos-parity` green (AC-workspace-02/04/05/06/07/08/09).
8. Add the CI job (`.github/workflows/`): toolchain → `fmt --check` →
   `clippy -D warnings` → `build --workspace` → `test -p eos-parity`. → verify:
   CI green (AC-workspace-01/03).
9. Mirror the §5 edge table + GC-workspace-04 `panic=unwind` rationale into
   `overview.md`'s dependency graph + Phase-0 notes (anchor §5a requirement).

---

**On completion:** update the Progress Tracker in `./overview.md` for row
`workspace` per spec-conventions.md §13. Do not edit other crates' rows.
