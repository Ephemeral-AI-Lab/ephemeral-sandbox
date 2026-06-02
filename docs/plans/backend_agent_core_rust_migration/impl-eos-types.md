# impl-eos-types — shared newtype IDs, time, JSON, and error primitives

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §1.

## 1. Purpose & Responsibility (SRP)

`eos-types` is the **leaf** crate of the agent-core dependency DAG. Its single
responsibility is to define the small, dependency-light value primitives that
every other crate shares: the typed string **ID newtypes**, the `UtcDateTime`
timestamp wrapper over `time::OffsetDateTime`, the `Clock` trait seam, the
`JsonObject` transitional-metadata alias, and a minimal `CoreError` for the few
cross-crate parse/conversion failures these primitives can produce.

This crate **must NOT**: hold any domain state (`Task`, `Workflow`, `Iteration`,
`Attempt` live in `eos-state`); define status/stage/reason enums; touch SQL,
HTTP, serde-of-domain-DTOs, sandbox, or config; perform I/O beyond reading the
system clock through the `Clock` impl; or grow into a "common utilities" dumping
ground (the plan explicitly warns "This should stay small"). If a type is owned
by another crate per the Contract Ownership Map (anchor §5), it does not appear
here.

## 2. Dependencies

- **Upstream crates (depends on):** none. This is the DAG root; it must stay
  buildable in isolation so every downstream crate can import it without cycles.
- **Downstream consumers (used by):** *all* agent-core crates (`eos-state`,
  `eos-db`, `eos-config`, `eos-audit`, `eos-llm-client`, `eos-tools`,
  `eos-agent-def`, `eos-sandbox-api`, `eos-sandbox-host`, `eos-skills`,
  `eos-plugin-catalog`, `eos-engine`, `eos-workflow`, `eos-runtime`).

- **External crates** — pinned via `[workspace.dependencies]` and inherited with
  `pkg = { workspace = true }` (`proj-workspace-deps`); no per-crate version
  drift.

| Crate | Why | rust-skills rule |
|---|---|---|
| `serde` (derive) | `Serialize`/`Deserialize` on every ID + `UtcDateTime` for wire/DTO roundtrips | anchor §9 wire types |
| `schemars` | `JsonSchema` derive on every ID + `UtcDateTime` for the Phase-0 schema-parity harness | anchor §11 |
| `serde_json` | backing map type for `JsonObject` (`serde_json::Map<String, Value>`) | plan §1 |
| `time` (features `["serde", "formatting", "parsing", "macros"]`) | `OffsetDateTime` backing `UtcDateTime`; RFC 3339 format/parse | plan §1 (`UtcDateTime` over `time::OffsetDateTime`) |
| `uuid` (features `["v4"]`) | generate UUIDv4 string IDs (Python uses `str(uuid.uuid4())`) | parity with `runtime/entry.py` |
| `thiserror` | the single `CoreError` enum | `err-thiserror-lib`, `err-custom-type` |

No `async-trait` here: `Clock` is synchronous (see §5). No `anyhow` (library
crate, anchor §8).

## 3. Scope & Source Mapping

The four Python source files are split across owning crates. `eos-types` extracts
**only the cross-cutting primitives**; the domain DTOs/enums in the same files
move to their owners (`eos-state`, `eos-audit`, `eos-sandbox-api`).

| Python source | Rust target (this crate) | What moves here / what is dropped |
|---|---|---|
| `task/task.py` — `Task.id`, `request_id`, `workflow_id`, `iteration_id`, `attempt_id` (all `str`) | `ids.rs` (`TaskId`, `RequestId`, `WorkflowId`, `IterationId`, `AttemptId`) | Only the **ID concept** → typed newtypes. `Task` struct + `TaskStatus` → `eos-state` (not here). |
| `workflow/_core/state.py` — `id`/`workflow_id`/`iteration_id`/`attempt_id` (`str`); `created_at`/`updated_at`/`closed_at` (`datetime`) | `ids.rs` (reuse), `time.rs` (`UtcDateTime`) | Timestamp-field **type** → `UtcDateTime`. `Workflow`/`Iteration`/`Attempt` DTOs + all status/stage/reason enums → `eos-state`. |
| `audit/base.py` — `AuditNode` ids (`agent_run_id`, `sandbox_id`, `tool_use_id`, plus task/workflow/iteration/attempt/request); `ts: datetime`; `JsonValue = Any` | `ids.rs` (`AgentRunId`, `SandboxId`, `ToolUseId`), `time.rs` (`ts → UtcDateTime`), `json.rs` (`JsonValue`/`JsonObject`) | The shared **id/time/json primitives**. `AuditEvent`/`AuditNode`/`AuditSink` → `eos-audit`. |
| `sandbox/shared/models.py` — `invocation_id` (`SandboxRequestBase`/`ToolCallRequest`), `agent_run_id` (`SandboxCaller`) | `ids.rs` (`InvocationId`; `AgentRunId` reused) | Only the **id concept**. All request/result DTOs + `Intent` → `eos-sandbox-api`. `SandboxCaller` also carries `task_id`/`request_id`/`attempt_id`/`workflow_id`, which reuse the same `eos-types` IDs; `SandboxId` is **not** here (sourced from `audit/base.py` row above). |

**In-scope:** 9 ID newtypes; `UtcDateTime`; `Clock` trait (+ `SystemClock`,
`TestClock` impls); `JsonValue`/`JsonObject` aliases; `CoreError`.

**Out-of-scope (owned elsewhere):** every domain struct/enum from these files;
`SandboxCaller.audit_fields()` logic (→ `eos-sandbox-api`); the `Intent` enum
(→ `eos-sandbox-api`); any `agent_id`/`run_id`/`tool_name` *string* fields the
plan leaves untyped (kept as `String` in their owning crates — see §10 GC-02).

## 4. File & Module Layout

```
eos-types/
  src/
    lib.rs        // crate docs (//!), #![lint attrs], pub use re-export facade
    ids.rs        // 9 string-newtype IDs via define_id! macro; Display/FromStr/serde/JsonSchema
    time.rs       // UtcDateTime newtype over OffsetDateTime; Clock trait; SystemClock; TestClock
    json.rs       // JsonValue + JsonObject type aliases
    error.rs      // CoreError (thiserror) — id-parse + timestamp-parse glue only
```

- `lib.rs` re-exports the public surface flatly (`pub use ids::*;` etc.) so
  consumers write `use eos_types::{TaskId, UtcDateTime, Clock, JsonObject};`
  (`proj-pub-use-reexport`).
- The `define_id!` macro is `macro_rules!` and `pub(crate)` — an internal codegen
  helper, not a public extension point (`proj-pub-crate-internal`,
  `anti-over-abstraction`). It is *not* re-exported.
- No `prelude` module — the surface is small enough that a flat re-export is
  clearer (KISS).

## 5. Contracts Owned Here

Per anchor §5, this crate owns: the 9 ID newtypes, `UtcDateTime`, the `Clock`
trait, `CoreError`, and `JsonObject`. All are fully specified here; every other
crate references them.

### 5.1 `Clock` trait (anchor §6 seam — DIP, testability)

```rust
/// Source of the current wall-clock instant. Inject instead of calling the
/// global clock so tests are deterministic (`test-mock-traits`).
pub trait Clock: Send + Sync {
    /// Current instant, normalized to UTC.
    fn now(&self) -> UtcDateTime;
}
```

- **Synchronous** (`fn`, not `async fn`): reading a clock never awaits, so this
  stays `dyn`-safe without `#[async_trait]` (anchor §6).
- **Object-safe:** used as `Arc<dyn Clock>` at the composition root; `Send +
  Sync` supertraits required because it is shared across the Tokio multi-thread
  runtime (`own-arc-shared`).
- **Not sealed:** `Clock` is a deliberate extension seam (system vs test vs
  future fixed-offset clocks). `SystemClock` (production) and `TestClock`
  (a settable `RwLock<UtcDateTime>`) ship here.

### 5.2 ID newtypes

Twelve `#[repr(transparent)]` wrappers over `String` (Python IDs are UUIDv4
strings and prefixed strings such as `root-<hex16>`, so the inner type is
`String`, **not** an integer; therefore **not** `Copy` per `own-copy-small`).

Each ID provides: `Display`, `FromStr`, `Serialize`/`Deserialize`
(`#[serde(transparent)]`), `JsonSchema`, `TryFrom<String>` + `TryFrom<&str>`
(`api-parse-dont-validate`), `as_str(&self) -> &str` (`name-as-free`),
`into_inner(self) -> String` (`name-into-ownership`), and a `new_v4()`
constructor that mints a fresh dashed UUIDv4. `new_v4()` is emitted by
`define_id!` for every ID **except `ToolUseId`**, which is model-assigned (see
§6.1) — minting one locally would be a bug. `new_v4()` is a generic helper, not a
mint-shape match: the runtime's prefixed/dashless mints (`root-<hex16>` task ids,
`uuid4().hex[:16]` agent-run ids) are produced by the owning crate, not by this
helper; only `RequestId`'s `str(uuid.uuid4())` mint is reproduced shape-for-shape.
Derives:
`Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord` (`api-common-traits`;
`Hash`/`Ord` so IDs key maps and sort deterministically). No `#[non_exhaustive]`
(tuple newtypes cannot grow fields).

`FromStr` accepts any non-empty string (these are opaque identifiers, not
format-validated UUIDs — a `root-…` task id is valid); empty input is the only
rejection, surfaced as `CoreError::EmptyId`.

### 5.3 `UtcDateTime` — see §6.2. `JsonObject`/`JsonValue` — see §6.3.
### 5.4 `CoreError` — see §8 / §6.4.

Contracts **referenced, not redefined** (owned elsewhere): `Task`/`TaskStatus`,
`Workflow`/`Iteration`/`Attempt` + their enums, per-entity `Store` traits
(`eos-state`); `AuditEvent`/`AuditNode`/`AuditSink` (`eos-audit`); `SandboxCaller`
and the `Intent` enum (`eos-sandbox-api`); `ToolSpec` (`eos-llm-client`).

## 6. Types, Fields & Schemas

### 6.1 ID newtypes (`ids.rs`)

| Newtype | Inner | Minted by (parity) | Used by (referencing crates) |
|---|---|---|---|
| `RequestId` | `String` | `runtime/entry.py` `str(uuid.uuid4())` | state, db, audit, engine, workflow |
| `TaskId` | `String` | `entry.py` `f"root-{uuid4().hex[:16]}"` + planner mint | state, db, audit, tools, engine, workflow |
| `WorkflowId` | `String` | workflow starter | state, db, audit, workflow |
| `IterationId` | `String` | iteration coordinator | state, db, audit, workflow |
| `AttemptId` | `String` | attempt orchestrator | state, db, audit, workflow |
| `AgentRunId` | `String` | engine agent factory | audit, db, engine, sandbox-api |
| `SandboxId` | `String` | sandbox host | audit, sandbox-api, sandbox-host |
| `ToolUseId` | `String` | provider stream (model-assigned) | audit, llm-client, tools, engine |
| `InvocationId` | `String` | tool dispatch | audit, sandbox-api, tools |
| `WorkflowTaskId` | `String` | engine background workflow handle | tools, engine, workflow |
| `CommandSessionId` | `String` | sandbox command-session tool | tools, engine |
| `SubagentSessionId` | `String` | engine subagent supervisor | tools, engine |

All share the layout below (one representative shown; the rest are macro-emitted):

```rust
/// Identifier for a persisted Task row. Opaque string; never an integer.
#[repr(transparent)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    /// Mint a fresh dashed UUIDv4-backed id. The runtime's prefixed task mint
    /// (`root-<hex16>`) is produced by the owning crate, not by this helper.
    #[must_use]
    pub fn new_v4() -> Self { Self(uuid::Uuid::new_v4().to_string()) }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
    #[must_use]
    pub fn into_inner(self) -> String { self.0 }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0) // bare value, no "task:" prefix — must roundtrip with FromStr
    }
}

impl std::str::FromStr for TaskId {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() { return Err(CoreError::EmptyId { kind: "TaskId" }); }
        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for TaskId {
    type Error = CoreError;
    fn try_from(s: String) -> Result<Self, Self::Error> { s.parse() }
}
impl TryFrom<&str> for TaskId {
    type Error = CoreError;
    fn try_from(s: &str) -> Result<Self, Self::Error> { s.parse() }
}
```

`Display` writes the bare inner string (not `kind:value`) so that
`id.to_string().parse::<TaskId>()` is a lossless roundtrip and DB/JSON columns
stay byte-identical to the Python strings (parity requirement — see AC-types-02).

### 6.2 `UtcDateTime` (`time.rs`)

| Field | Rust type | serde / schemars | Source of truth |
|---|---|---|---|
| (inner) | `time::OffsetDateTime` (always UTC offset) | `#[serde(transparent)]`, serialized as RFC 3339 string; `JsonSchema` as `format: date-time` | `created_at`/`updated_at`/`closed_at`/`started_at`/`ts` (`datetime`) |

```rust
/// UTC instant. Wraps `OffsetDateTime`, guaranteeing the offset is always UTC.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct UtcDateTime(#[serde(with = "time::serde::rfc3339")] OffsetDateTime);

impl UtcDateTime {
    #[must_use] pub fn now() -> Self { Self(OffsetDateTime::now_utc()) }
    /// Normalize any offset to UTC on construction (LSP: never a non-UTC value).
    #[must_use] pub fn from_offset(dt: OffsetDateTime) -> Self { Self(dt.to_offset(UtcOffset::UTC)) }
    #[must_use] pub fn to_rfc3339(self) -> String { /* time::format_description::well_known::Rfc3339 */ }
    pub fn parse_rfc3339(s: &str) -> Result<Self, CoreError> { /* map err -> CoreError::Timestamp */ }
    #[must_use] pub fn into_inner(self) -> OffsetDateTime { self.0 }
}
```

`UtcDateTime` is **`Copy`** (`OffsetDateTime` is 16 bytes, no heap —
`own-copy-small`). The constructor normalizes to UTC so the wrapper's invariant
(offset == UTC) holds for all values, making Anthropic/OpenAI/DB timestamps
substitutable (LSP). RFC 3339 is the single wire format (matches Python's
`datetime.now(UTC)` isoformat persistence).

### 6.3 JSON aliases (`json.rs`)

```rust
pub type JsonValue  = serde_json::Value;             // Python `JsonValue = Any`
pub type JsonObject = serde_json::Map<String, serde_json::Value>; // plan §1 transitional metadata
```

Aliases, not newtypes: they are deliberately *untyped transitional* containers
(`terminal_tool_result`, audit `payload`, tool args) that downstream crates parse
into typed shapes at their boundaries (`api-parse-dont-validate`). YAGNI — no
wrapper methods until a caller needs one.

`JsonObject` is the owned transitional-metadata contract enumerated in anchor §5;
`JsonValue` is a bare convenience re-export of `serde_json::Value` (mirroring
audit/base.py's `JsonValue = Any`), not a separate owned contract — it carries no
crate-specific semantics and is not on the anchor ownership map.

### 6.4 `CoreError` (`error.rs`)

| Variant | Carries | When |
|---|---|---|
| `EmptyId { kind: &'static str }` | id kind name | `FromStr` on an empty id string |
| `Timestamp(#[from] time::error::Parse)` | source parse error | `UtcDateTime::parse_rfc3339` failure |

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    #[error("empty {kind} identifier")]        // lowercase, no trailing punctuation
    EmptyId { kind: &'static str },
    #[error("invalid utc timestamp")]
    Timestamp(#[from] time::error::Parse),
}
```

`#[non_exhaustive]` so downstream crates' `#[from] CoreError` conversions stay
forward-compatible (anchor §8/§9). This is the *only* error enum here and is kept
deliberately tiny — it covers exactly the two failures these primitives can
raise; richer errors belong to the crate that owns the failing operation.

## 7. Concurrency & State Ownership

Per anchor §7:

- **Runtime-agnostic.** This crate spawns no runtime and contains no `async fn`.
  It is safe to call from any thread of the single Tokio multi-thread runtime
  created in `eos-runtime`.
- **IDs / `UtcDateTime` / `JsonObject`:** plain owned values. IDs are `Clone`
  (heap `String`); `UtcDateTime` is `Copy`. They are passed by value or by
  borrow (`&str` via `as_str`, never `&String` — `own-slice-over-vec`,
  `anti-string-for-str`).
- **`Clock` implementors:**
  - `SystemClock` is a zero-sized unit struct — trivially `Send + Sync`, shared
    as `Arc<dyn Clock>` (`own-arc-shared`).
  - `TestClock` holds its settable instant in `std::sync::RwLock<UtcDateTime>`
    (reads dominate — `own-rwlock-readers`). `now()` takes a read guard, copies
    the `Copy` `UtcDateTime` out, and drops the guard before returning — there is
    no `.await` in this crate, so the lock is never held across one
    (`async-no-lock-await`, `anti-lock-across-await` vacuously satisfied;
    `async-clone-before-await` pattern followed by construction).
- **No interior mutability, channels, cancellation, or background tasks** — none
  are needed (YAGNI). No app-level locks beyond `TestClock`'s.

## 8. Behavior & Invariants

- **ID opacity & roundtrip:** an ID is an opaque non-empty string. For every ID,
  `id.to_string().parse() == Ok(id)` and `serde_json::to_value` yields a bare
  JSON string equal to the inner value (no object wrapping — guaranteed by
  `#[serde(transparent)]` + `#[repr(transparent)]`). This preserves on-the-wire
  and on-disk parity with the current Python `str` IDs.
- **Single canonical ID field at every boundary (the core gap, §10 GC-01):** an
  entity's primary id serializes under exactly one key. There is no `id`+`task_id`
  duplication; relationships use the typed foreign-key field name
  (`task_id: TaskId`, `parent_task_id: TaskId`).
- **`UtcDateTime` is always UTC:** every constructor normalizes the offset, so no
  consumer ever observes a non-UTC value (LSP substitutability across providers
  and the DB layer).
- **Error message style:** lowercase, no trailing punctuation
  (`err-lowercase-msg`).
- **Non-goals respected (anchor §2):** no domain logic, no provider/sandbox
  terminology, no `class_path`, no stringly-typed structured data leaking out
  (the two `JsonObject`/`JsonValue` aliases are explicitly transitional and
  parsed downstream, not a stringly-typed API in the `anti-stringly-typed`
  sense).

## 9. SOLID & Principles Applied

- **DIP:** `Clock` is the dependency-inversion seam (anchor §6). Higher crates
  depend on `Clock`; `eos-runtime` injects `Arc<dyn Clock>` (`SystemClock` in
  prod, `TestClock` in tests). No crate reads the global wall clock directly.
- **LSP:** `UtcDateTime`'s UTC-normalization invariant makes every timestamp
  source substitutable; `SystemClock`/`TestClock` are interchangeable behind
  `Clock`.
- **ISP:** `Clock` has exactly one method (`now`). The crate exposes no
  god-type.
- **OCP/SRP:** new ID kinds are added by one `define_id!` line, never by editing a
  consumer's logic; the crate's responsibility is value primitives only.
- **KISS/YAGNI/DRY:** string IDs (not numeric, matching reality); aliases not
  wrappers for JSON; one tiny `CoreError`; one macro for DRY id codegen. No
  builder, no extension points beyond the `Clock` seam the plan already names.

## 10. Gap Closeouts (tracked requirements)

- **GC-types-01 — Stop double-serializing the task id as both `id` and `task_id`.**
  *(Plan §1 gap closeout.)* Resolution: an entity's own primary key serializes
  under exactly **one** key. `eos-state`'s `Task` uses a single `id: TaskId`
  (its self-key); any *other* row that references a task uses the typed foreign
  key `task_id: TaskId`. `eos-types` makes the rule mechanically enforceable by
  giving `TaskId` `#[serde(transparent)]` (no nesting) and providing exactly one
  inner accessor (`as_str`/`into_inner`); there is no serde alias, no `#[serde(rename
  = "task_id")]` on a self-id field, and no duplicate getter. A proving roundtrip
  test (AC-types-02) asserts the serialized form contains the id under one key
  only. The boundary contracts that pick which typed field to expose live in the
  owning crates (`eos-state`, `eos-db`); `eos-types` supplies the type that makes
  the single-field choice unambiguous.

- **GC-types-02 — Type the cross-cutting IDs; leave genuinely-stringy fields as
  `String`.** The plan enumerates the IDs that must become newtypes (anchor §5).
  Resolution: own exactly those 9 newtypes here. Fields that are *not* identifiers
  in the typed-ID sense — `agent_name`, `agent_id`, `run_id`, `tool_name`,
  `model_key` — stay `String`/owned-typed in their crates and are **not** minted
  here (avoids a stringly-typed sprawl masquerading as IDs; honors KISS/YAGNI and
  GC scope).

## 11. Acceptance Criteria

TDD: write each test first, watch it fail for the right reason, then implement.
Unit tests live in `#[cfg(test)] mod tests` with `use super::*`
(`test-cfg-test-module`, `test-use-super`); the schema-snapshot AC feeds the
Phase-0 parity harness (anchor §11).

- **AC-types-01 — ID `Display`/`FromStr` roundtrip.** For each of the 12 IDs,
  `s.parse::<T>().unwrap().to_string() == s` for a UUIDv4 string and for a
  `root-abc123` prefixed string; empty string parses to `Err(CoreError::EmptyId)`.
  *Test:* `ids::tests::id_display_fromstr_roundtrip` (proptest over non-empty
  strings — `test-proptest-properties`).

- **AC-types-02 — Transparent serde, no key duplication (GC-types-01).**
  `serde_json::to_value(&"t1".parse::<TaskId>().unwrap())` equals `json!("t1")` (a bare
  string, not `{"0":"t1"}`); deserializing `"t1"` yields the same id. A struct
  `{ id: TaskId }` serializes to `{"id":"t1"}` with the id appearing under
  exactly one key. *Test:* `ids::tests::id_serde_transparent_single_key`.

- **AC-types-03 — `UtcDateTime` RFC 3339 parity + UTC normalization.** A known
  instant roundtrips through `to_rfc3339`/`parse_rfc3339`; a non-UTC
  `OffsetDateTime` passed to `from_offset` reports a UTC offset; serde emits the
  same RFC 3339 string Python's `datetime.now(UTC).isoformat()`-style persistence
  produces. *Test:* `time::tests::utc_datetime_rfc3339_roundtrip` (+ a snapshot
  asserted against the Pydantic-derived `date-time` schema in the Phase-0
  harness).

- **AC-types-04 — `Clock` injection is deterministic.** `TestClock::set(t)`
  followed by `clock.now()` returns `t`; the same `Arc<dyn Clock>` shared across
  two threads yields identical reads. *Test:* `time::tests::test_clock_is_settable`
  (`test-mock-traits`).

- **AC-types-05 — `CoreError` ergonomics.** `CoreError` is `std::error::Error`;
  `time::error::Parse` converts via `?` (`#[from]`); `Display` messages are
  lowercase with no trailing punctuation. *Test:* `error::tests::core_error_from_and_display`.

- **AC-types-06 — `JsonSchema` derives present.** `schemars::schema_for!` succeeds
  for every ID and `UtcDateTime`; IDs schema as `string`, `UtcDateTime` as
  `string`/`format: date-time`. *Test:* `tests::json_schema_for_primitives`
  (feeds the schema-parity snapshot, anchor §11).

## 12. Implementation Checklist

Ordered, small, verifiable steps (`small-incremental-changes`):

1. Scaffold crate; add workspace-inherited deps (§2); apply workspace lints from
   `lib.rs` (`#![warn(missing_docs)]`, correctness deny). → `cargo build` clean.
2. `error.rs`: `CoreError` with `EmptyId` + `Timestamp(#[from])`. Write
   AC-types-05 test first. → fails, then passes.
3. `ids.rs`: write AC-types-01/02 tests; add `define_id!` macro; instantiate the
   12 IDs; implement `Display`/`FromStr`/`TryFrom`/serde/`JsonSchema`/`new_v4`. → ACs pass.
4. `time.rs`: write AC-types-03/04 tests; implement `UtcDateTime` (Copy, UTC-norm,
   RFC 3339 serde) and `Clock` + `SystemClock` + `TestClock`. → ACs pass.
5. `json.rs`: add the two aliases.
6. `lib.rs`: flat `pub use` re-export facade; write AC-types-06 schema test. → passes.
7. `cargo fmt --check` + `cargo clippy -D warnings` (`lint-rustfmt-check`). → clean.

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-types` per spec-conventions.md §13. Do not edit other crates' rows.
