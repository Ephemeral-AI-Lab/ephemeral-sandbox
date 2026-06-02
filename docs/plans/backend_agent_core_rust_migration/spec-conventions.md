# Agent-Core Rust Migration — Shared Spec Conventions (Anchor)

> **Read this first.** This is the single source of truth for naming, contract
> ownership, the SOLID-to-seam map, the document template, and the progress
> tracker protocol. Every `impl-*.md` module spec and every reviewer **must**
> conform to this document. When an `impl-*.md` would restate a shared contract,
> it instead **references the owning crate's doc** (see the Contract Ownership
> Map). This prevents 16 independently authored specs from quietly contradicting
> each other.

Parent plan: `../backend_agent_core_rust_migration_PLAN.md`
Index / phases / progress tracker: `./overview.md`

---

## 1. The Tension We Are Resolving

The user asks for **flexible, extensible code following SOLID**. The parent plan
explicitly **forbids speculative abstraction**. These are not in conflict once
you accept the governing rule:

> **Flexibility lives in the trait seams and registries the plan already
> defines — not in new layers.** Add no abstraction that is not on the SOLID
> Seam Map (§5). Everything else is concrete.

Concretely:

- **DIP** is satisfied by the existing trait seams (`Store`s, `LlmClient`,
  `ToolExecutor`, `ProviderAdapter`, `SandboxTransport`, `AuditSink`, `Clock`,
  `EventSource`). High-level crates depend on these traits; low-level crates
  implement them; the **composition root (`eos-runtime`)** wires concretes in.
- **OCP** is satisfied by the **registries** (tool, agent, provider, skill,
  plugin catalog). Extend behavior by *registering an implementation*, never by
  editing a `match` in a dispatch path.
- **ISP** is satisfied by **per-entity store traits** (no god-store) and small
  focused tool/transport traits.
- **LSP** is satisfied by **provider-neutral types** (`LlmStreamEvent`,
  `Message`) so Anthropic/OpenAI/mocks are substitutable, and by exhaustive
  enums for mutually-exclusive states.
- **SRP** is satisfied by the crate boundaries themselves; do not let a crate
  reach across its boundary (e.g. `ContextEngine` builds packets, it does **not**
  own lifecycle policy).

KISS / YAGNI / DRY apply on top: prefer the smallest concrete shape; do not add
config, generics, or extension points that no current caller needs; every shared
contract has exactly one definition (enforced by the Ownership Map).

---

## 2. Hard Non-Goals (verbatim from the plan — never violate)

- No peer-to-peer agent messaging.
- No global agent orchestrator (orchestration is **per-Attempt** only).
- No synthetic root workflow (the root request is a root `Task`, not a Workflow).
- No provider `class_path` dynamic import in the final Rust runtime. `class_path`
  survives **only as migration data**; final dispatch is typed by `llm_provider`
  + `model_key`.
- **No tool visibility enum.** A tool is visible iff its `ToolSpec` is present in
  the request's `Vec<ToolSpec>`.
- **No deferred / lazy model-facing tool loading.** Build concrete `ToolSpec`s at
  agent spawn.
- No PostgreSQL in agent-core. Target DB contract is local **SQLite** (WAL,
  foreign keys, busy timeout, explicit migrations). A network DB URL is rejected
  (fail fast).
- Do not port `coding_plan` provider clients, `test_runner`, or deep sandbox
  daemon internals (LayerStack/OCC/overlay/plugin runtime).

---

## 3. Core Design Rules (verbatim subset)

- Task is the persisted agent interface. A request creates one root
  `Task(role=root, workflow_id=None)`. Delegation creates
  `Workflow -> Iteration -> Attempt`.
- Attempt is the lifecycle unit for planner-authored generator/reducer DAGs. The
  **reducer is the exit gate**.
- `ContextEngine` is a workflow-context builder only. Lifecycle policy stays in
  workflow handlers/managers.
- Terminal-tool enforcement stays in the engine/tool-dispatch path. **Terminal
  tools must be called alone**; their results are persisted task/workflow state
  inputs, not just user-facing messages.
- Background execution is an **engine dispatch mode**, not a provider-level
  persistent shell session.
- Convert Pydantic → `serde` structs + `schemars::JsonSchema`.
- Convert SQLAlchemy → SQLite-only `sqlx` repositories with typed rows + versioned
  migrations.
- Do not mutate the parent Task at workflow close; the parent Task owns its own
  terminal submission.

---

## 4. Glossary & Naming Normalization (canonical Rust names)

| Concept | Canonical Rust (domain) | Notes / legacy |
|---|---|---|
| Workflow goal | `workflow_goal` | DB column may be `goal`; map explicitly in `eos-db`. |
| Iteration goal | `iteration_goal` | DB column may be `goal`. |
| Deferred goal | `deferred_goal_for_next_iteration` | DB column may be `deferred_goal`. |
| Execution role (state) | `generator` | `executor` is **at most a profile alias**; never enters state. |
| Model reasoning content | `Reasoning*` (`ReasoningDelta`, `ReasoningBlock`) | Old JSONL `thinking` → compatibility decode map. |
| Model client provider | `llm_provider` | Never bare `provider`. |
| Sandbox backend | `sandbox_provider` (Docker; seam kept for future providers) | Never bare `provider`. |
| Command execution tool | `exec_command` + command session | Daemon ops are `api.v1.exec_command` and `api.v1.exec_stdin`; the legacy `shell` op is removed. |
| Transcript message role | `user` \| `assistant` | Fix the prompt-report `system`-role mismatch (system prompt is a request field, not a `Message`). |

**Acronyms are words:** `Uuid`, `Json`, `Http`, `Sse`, `Lsp`, `Occ` →
`UuidThing`, `JsonObject`, `HttpClient`, `SseFrame` (rust-skills `name-acronym-word`).

---

## 5. Contract Ownership Map (single source of truth per contract)

A contract is **defined once**, in its owning crate's `impl-*.md`. Other docs
**reference** it (`see impl-eos-state.md §Store Traits`) and must not re-specify
fields. The dependency DAG is the tie-breaker: the **upstream** crate owns the
shared contract.

| Contract / type | Owning crate | Referenced by |
|---|---|---|
| Newtype IDs (`TaskId`, `WorkflowId`, `IterationId`, `AttemptId`, `RequestId`, `AgentRunId`, `SandboxId`, `ToolUseId`, `InvocationId`, `WorkflowSessionId`, `CommandSessionId`, `SubagentSessionId`), `UtcDateTime`, `Clock` trait, `CoreError`, `JsonObject` | `eos-types` | all |
| Domain state (`Task`, `Workflow`, `Iteration`, `Attempt`), status/stage/reason enums, outcome projections, terminal submission DTOs, **per-entity `Store` traits** | `eos-state` | `eos-db`, `eos-tools`, `eos-engine`, `eos-workflow`, `eos-runtime` |
| SQLite row structs, repository impls, migrations, `SqlitePool` builder, model registry | `eos-db` | `eos-runtime` |
| `CentralConfig` + section configs, env loading, path resolution, validation | `eos-config` | `eos-db`, `eos-llm-client`, `eos-sandbox-host`, `eos-skills`, `eos-plugin-catalog`, `eos-runtime` |
| `AuditEvent`, `AuditNode`, **`AuditSink` trait**, `AuditEventBus`, JSONL writer, redaction | `eos-audit` | `eos-tools`, `eos-engine`, `eos-workflow`, `eos-plugin-catalog`, `eos-runtime` |
| Provider-neutral `Message`/content blocks, `UsageSnapshot`, `LlmRequest`, `LlmStreamEvent`, `ProviderError`, **`ToolSpec`** (`{name, description, input_schema, output_schema}`), **`LlmClient` trait** | `eos-llm-client` | `eos-tools`, `eos-engine` |
| `ToolName` (typed constants), `ToolIntent`, `ToolError`, **`ToolExecutor` trait**, `ToolRegistry`, terminal descriptors, execution/dispatch policy | `eos-tools` | `eos-engine`, `eos-workflow` |
| `AgentDefinition`, `AgentRole`, `AgentType`, `AgentRegistry`, context-recipe metadata | `eos-agent-def` | `eos-engine`, `eos-workflow`, `eos-runtime` |
| `SandboxCaller`, `SandboxRequestBase/ResultBase`, `ToolCallRequest`, daemon op constants, **`SandboxTransport` trait**, typed `tool_api` | `eos-sandbox-api` | `eos-tools`, `eos-sandbox-host` |
| **`ProviderAdapter` trait**, provider registry, the Docker adapter (seam kept for future providers), daemon client, lifecycle, runtime-artifact upload | `eos-sandbox-host` | `eos-runtime` |
| `SkillDefinition`, `SkillRegistry`, loader | `eos-skills` | `eos-tools`, `eos-runtime` |
| `PluginManifest`, `ToolEntry`, `PluginCatalog`, plugin tool specs, plugin audit wrapper | `eos-plugin-catalog` | `eos-runtime` |
| `QueryContext`, query loop, dispatch/streaming, background supervisor, notifications, prompt report, agent factory, **`EventSource` trait** | `eos-engine` | `eos-runtime` |
| `WorkflowStarter`, `AttemptOrchestrator`, plan-DAG validation, run-stage scheduler, `ContextEngine`, iteration coordinator, lifecycle | `eos-workflow` | `eos-runtime` |
| `AppState` (composition root / DI graph), `RequestEntry`, sandbox provisioning, root-agent lifecycle | `eos-runtime` | (top) |

### 5a. Deliberate DAG refinement (must be stated in overview)

The plan lists `ToolSpec` under **both** `eos-llm-client` (§6) and `eos-tools`
(§7). To keep a single definition: **`ToolSpec` is owned by `eos-llm-client`**
(it is the neutral declaration sent to the model), and **`eos-tools` gains a
dependency edge on `eos-llm-client`** to author specs. This is acyclic
(`eos-tools -> eos-llm-client -> eos-types`) and DIP-correct: tools depend on the
neutral spec abstraction, not on any provider. `eos-tools` still owns `ToolName`,
`ToolIntent`, `ToolExecutor`, and the registry. Record this edge explicitly in
`overview.md`'s dependency graph.

---

## 6. SOLID Seam Map (the ONLY allowed abstractions)

These trait seams + registries are the extensibility surface. Each ties to
rust-skills rules. **Do not introduce abstractions outside this list.**

| Seam (trait / registry) | Owner | Implementors | Principle | rust-skills |
|---|---|---|---|---|
| `Clock` | eos-types | system clock, test clock | DIP, testability | `test-mock-traits` |
| per-entity `Store` traits | eos-state | `eos-db` sqlx repos, in-memory test stores | DIP + ISP | `test-mock-traits`, `api-sealed-trait` |
| `LlmClient` | eos-llm-client | `AnthropicClient`, `OpenAiClient`, mock | DIP + LSP | `async-tokio-runtime`, `anti-type-erasure` |
| `EventSource` | eos-engine | real provider stream, mock/replay | DIP, deterministic tests | `test-mock-traits` |
| `ToolExecutor` + `ToolRegistry` | eos-tools | per-tool executors | OCP + SRP | `api-sealed-trait` |
| `AuditSink` + `AuditEventBus` | eos-audit | JSONL sink, in-memory sink | DIP + OCP | — |
| `SandboxTransport` | eos-sandbox-api | daemon client | DIP | `async-tokio-runtime` |
| `ProviderAdapter` + provider registry | eos-sandbox-host | Docker (+ `#[cfg(test)]` mock; seam kept for future providers) | OCP + LSP | — |
| `AgentRegistry` | eos-agent-def | profile-backed registry | OCP | — |
| `SkillRegistry` | eos-skills | bundled/config-dir loader | OCP | — |
| `PluginCatalog` | eos-plugin-catalog | manifest-discovered catalog | OCP | — |
| `AgentRunner` (§6a) | eos-workflow | runtime adapter over `run_ephemeral_agent`, mock | DIP, testability | `test-mock-traits`, `anti-type-erasure` |
| `WorkflowControlPort` (§6b) | eos-tools | eos-workflow + eos-engine workflow-handle adapter | DIP + ISP | `api-sealed-trait` |
| `PlanSubmissionPort` (§6b) | eos-tools | eos-workflow `AttemptOrchestrator` | DIP + ISP | `api-sealed-trait` |
| `SubagentSupervisorPort` (§6b) | eos-tools | eos-engine background supervisor | DIP + ISP | `api-sealed-trait` |
| `AdvisorPort` (§6b) | eos-tools | eos-engine helper-agent runner | DIP + ISP | `api-sealed-trait` |
| `IsolatedWorkspacePort` (§6b) | eos-tools | eos-runtime adapter over eos-sandbox-host lifecycle + eos-engine background state | DIP + ISP | `api-sealed-trait` |
| `NotificationSink` (§6b) | eos-tools | eos-engine notification service | DIP + ISP | `api-sealed-trait` |

**Trait object-safety:** traits used behind `dyn` in the composition root that
have `async fn`s use `#[async_trait]` (native async-fn-in-trait is not yet
`dyn`-safe). Document this choice per crate. Prefer `impl Trait`/generics where
no `dyn` is needed (`anti-type-erasure`), but accept `Arc<dyn Trait>` at the
composition root where heterogeneous storage requires it.

### 6a. Deliberate seam-map addition (must be stated in overview)

The Python `EphemeralAttemptAgentLauncher` carries a runner DI param
(`AttemptAgentRunner`, default `engine.api.run_ephemeral_agent`). The Rust
redesign keeps `eos-workflow` free of any `eos-engine` edge (the agent-run call
is wired by `eos-runtime`), so this DI param is promoted to a named seam:
**`AgentRunner` is owned by `eos-workflow`** (it defines the trait;
`eos-runtime` supplies the concrete adapter over `run_ephemeral_agent`). Owner
must be `eos-workflow`, not `eos-engine`, or it would force the
`eos-workflow → eos-engine` edge the topology forbids — mirroring how
`EventSource` is owned by `eos-engine`. Use an `#[async_trait]` trait, not a
type-erased `Arc<dyn Fn -> BoxFuture>` (`anti-type-erasure`). Record this seam in
`overview.md`'s dependency-topology refinement note.

### 6b. Deliberate seam-map addition — eos-tools downstream-state ports (must be stated in overview)

Six tools (`delegate`/`check`/`cancel_workflow`, planner/reducer/generator
submission, `run_subagent`/control, `ask_advisor`, enter/exit isolated workspace,
system notifications) need engine/workflow/host state that lives **downstream** of
`eos-tools`. To keep `eos-tools` free of any `eos-engine`/`eos-workflow` edge, the
DI params are promoted to **six named ports owned by `eos-tools`**
(`WorkflowControlPort`, `PlanSubmissionPort`, `SubagentSupervisorPort`,
`AdvisorPort`, `IsolatedWorkspacePort`, `NotificationSink`); the concrete
implementors live downstream and are injected at the composition root — mirroring
how `EventSource` is owned by `eos-engine`. `IsolatedWorkspacePort`'s implementor
is the **`eos-runtime`** adapter over the `eos-sandbox-host` lifecycle, not
`eos-sandbox-host` directly, because sandbox-host is upstream of `eos-tools` and a
direct edge would invert the phase layering. All six use `#[async_trait]`, sealed
(`api-sealed-trait`). See `impl-eos-tools.md` §5.6 for methods. Record these edges
in `overview.md`'s dependency-topology refinement note.

---

## 7. Concurrency & State-Ownership Conventions

Every `impl-*.md` includes a **"Concurrency & State Ownership"** section using
these conventions. Name the actual primitives; do not hand-wave.

- **Runtime:** single Tokio multi-thread runtime, created in `eos-runtime`. Lower
  crates are runtime-agnostic (they take `&self`/`async fn`, never spawn their
  own runtime).
- **Shared immutable state** (config, registries, agent defs, skills): `Arc<T>`,
  cloned cheaply (`own-arc-shared`).
- **Shared mutable state:** prefer message passing. Where shared mutation is
  unavoidable, **choose the lock by access pattern**:
  - *Short, synchronous critical sections that never `.await` while the guard is
    held* (counters, small registry/cache maps): use **`parking_lot::Mutex` /
    `RwLock`** (or `std::sync::Mutex`). Their guard is `!Send`, so "hold across
    `.await`" is a **compile error** in spawned tasks and clippy
    `await_holding_lock` flags it; `parking_lot` also does not poison (matters
    under `panic=unwind`) and is smaller/faster (`own-mutex-interior`). Use
    `RwLock` when reads dominate (`own-rwlock-readers`).
  - *Only when the guard genuinely must span an `.await`* (e.g. single-flight
    dedup of an async resolve): use **`tokio::sync::Mutex` / `RwLock`** — the one
    job the async lock exists for; otherwise it adds scheduler overhead for no
    benefit.
  - **Never hold a lock across `.await`** as the default (`async-no-lock-await`,
    `anti-lock-across-await`); clone/extract then drop the guard before awaiting
    (`async-clone-before-await`).
- **Background supervisor (eos-engine):** `tokio::task::JoinSet` for the dynamic
  task group (`async-joinset-structured`), `CancellationToken` for graceful +
  parent-exit cancellation (`async-cancellation-token`), `mpsc` for the work
  queue (`async-mpsc-queue`), `oneshot` for completion handoff
  (`async-oneshot-response`), `watch` for latest-status snapshots
  (`async-watch-latest`). Bounded channels for backpressure
  (`async-bounded-channel`).
- **Observability from day one:** `tracing` spans are required on request,
  agent-run, LLM stream, tool execution, workflow transition, sandbox RPC, and
  SQLite transaction boundaries. `tracing-subscriber` is initialized only at the
  binary/sync-wrapper boundary. `tokio-console` support is an optional dev
  feature (`console-subscriber`) for stuck task/resource debugging. Use `loom`
  dev tests for small lock/channel/state-machine components where interleavings
  matter.
- **Streaming (eos-llm-client / engine):** model the provider stream as
  `impl Stream<Item = Result<LlmStreamEvent, ProviderError>>` (`futures`). SSE
  parsing is incremental and zero-copy where practical (`mem-zero-copy`).
- **DB (eos-db):** `SqlitePool` owns connection concurrency; SQLite single-writer
  handled via `busy_timeout` + WAL. No app-level DB mutex.
- **CPU-bound work** (if any, e.g. large redaction/hashing): `spawn_blocking`
  (`async-spawn-blocking`).
- **Workflow attempt scheduling (eos-workflow):** run-stage schedules the
  generator∪reducer task set to quiescence; model with `JoinSet` over launched
  task futures, store-state as the source of truth (no in-memory DAG mutation
  that can diverge from persistence).

---

## 8. Error Handling Conventions

- Each crate defines **one** `thiserror` error enum (`err-thiserror-lib`,
  `err-custom-type`). No `Box<dyn Error>` in public signatures.
- `#[from]` for upstream-crate error conversion (`err-from-impl`), `#[source]` to
  chain (`err-source-chain`). Messages lowercase, no trailing punctuation
  (`err-lowercase-msg`).
- Return `Result` for expected failures (`err-result-over-panic`); `?` for
  propagation (`err-question-mark`). No `.unwrap()` in non-test code
  (`err-no-unwrap-prod`); `.expect()` only for true invariants
  (`err-expect-bugs-only`).
- `eos-runtime` (the binary/app layer) **may** use `anyhow` for top-level wiring
  (`err-anyhow-app`); library crates must not.
- Validate at boundaries and **parse into validated types** rather than passing
  raw strings inward (`api-parse-dont-validate`, `type-no-stringly`). Fail fast on
  contradictory config / malformed manifests / network DB URLs.

---

## 9. Type-Safety & API Conventions

- IDs are newtypes (`type-newtype-ids`); validated values (paths, URLs, tool
  names) are newtypes or enums (`type-newtype-validated`, `type-no-stringly`).
- Mutually-exclusive states are enums (`type-enum-states`); nullable is
  `Option<T>`; fallible is `Result<T,E>`.
- Public structs/enums that may grow: `#[non_exhaustive]` (`api-non-exhaustive`).
- Derive `Debug, Clone, PartialEq` eagerly (`api-common-traits`); `Default` where
  sensible (`api-default-impl`). `Serialize/Deserialize` + `JsonSchema` on all
  wire/DTO types.
- Builder pattern + `#[must_use]` for complex construction
  (`api-builder-pattern`, `api-builder-must-use`). `#[must_use]` on
  `Result`-returning fns where ignoring is a bug (`api-must-use`).
- Accept `&str`/`&[T]` not `&String`/`&Vec<T>` (`own-slice-over-vec`,
  `anti-string-for-str`). Borrow over clone (`own-borrow-over-clone`).
- Seal traits not meant for external impl (`api-sealed-trait`).

---

## 10. Tool Description & Schema Conventions (applies to tool-bearing crates)

- Every model-facing tool has **exactly one** `ToolSpec` source colocated with
  the tool module. No docstring fallback, no separate prompt file + inline
  description mix.
- Long model-facing text: `const DESCRIPTION: &str` or
  `include_str!("description.md")`.
- Input/output schemas generated from Rust structs via `schemars::JsonSchema`.
- Terminal descriptors are **total** over all terminal tools (compile/test
  coverage).
- Every public tool name has a typed constant in `ToolName` (`write_stdin`,
  isolated-workspace tools, `load_skill_reference`, etc.).
- Every wrapper/synthesized control carries an `intent`.
- Agent profile tool names may enter `eos-agent-def` as raw strings to keep the
  dependency DAG clean, but final startup validation happens after registries are
  built: unknown `allowed_tools` / `terminals` fail fast unless an explicit
  compatibility mode is enabled.
- Rust doc comments are for developers, **not** the source of model-facing text.

---

## 11. Testing & Acceptance-Criteria Conventions

TDD mode is in effect: **write the failing test first**, confirm it fails for the
right reason, then implement.

- Acceptance Criteria (AC) are **testable assertions with IDs** (`AC-<crate>-NN`),
  each naming the test that proves it.
- Map ACs to the plan's **"Tests to Port First"** where applicable. Key mappings:

  | Crate | Ports / recreates |
  |---|---|
  | eos-db | store roundtrips for request/task/workflow/iteration/attempt/agent_run |
  | eos-llm-client | Anthropic + OpenAI SSE fixtures; retry-after-visible-output; error mapping (request id + status) |
  | eos-tools | `test_tool_execution.py`, `test_schema_summary.py`, `test_submission_main_role_terminals.py`, `test_sandbox_toolkit/test_exec_command.py`, `test_write_stdin.py` |
  | eos-engine | `test_tool_batch.py`, `test_tool_call_dispatch_lifecycle.py`, prompt-report golden, notification rules |
  | eos-workflow | workflow DAG / orchestrator / context tests under `backend/tests`; planner-DAG invariants; reducer exit gate |
  | eos-config | env-override tests |
  | eos-audit | JSONL golden + deterministic redaction |
  | eos-sandbox-api/host | daemon envelope tests; Docker selection (seam ready for future providers); provisioning |
  | eos-skills | reference-loading determinism |
  | eos-plugin-catalog | manifest validation |

- Unit tests: `#[cfg(test)] mod tests` + `use super::*` (`test-cfg-test-module`,
  `test-use-super`). Async tests: `#[tokio::test]`. Cross-crate behavior:
  `tests/` integration dir. Property tests (`proptest`) for parsers/projections
  where valuable. Mock via traits (`test-mock-traits`).
- Phase 0 parity harness: schema snapshots vs current Pydantic JSON schema; SSE
  fixture replay; SQLite schema snapshots; prompt-report golden.

---

## 12. `impl-*.md` Document Template (every module spec uses these sections, in order)

```markdown
# impl-<crate> — <one-line purpose>

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §<n>.

## 1. Purpose & Responsibility (SRP)
One paragraph: the single responsibility. What this crate must NOT do.

## 2. Dependencies
- Upstream crates (depends on): ...
- Downstream consumers (used by): ...
- External crates (with one-line justification + rust-skills rule): table.
  Pin to workspace dependency inheritance (proj-workspace-deps).

## 3. Scope & Source Mapping
Table: Python source file -> Rust target file -> what moves / what is dropped.
In-scope / out-of-scope bullets.

## 4. File & Module Layout
Tree of `src/*.rs` with a one-line responsibility per file. `lib.rs` re-exports
(proj-pub-use-reexport); `pub(crate)` internals (proj-pub-crate-internal).

## 5. Contracts Owned Here
Traits/types this crate OWNS (per Ownership Map). For each: signature sketch +
object-safety/async note. Contracts merely USED are listed as references only.

## 6. Types, Fields & Schemas
Per struct/enum: a field table (name | Rust type | serde/schemars notes |
source-of-truth). Enums list all variants. Note `#[non_exhaustive]`, derives,
newtypes. Show 1-2 representative Rust snippets (not the whole crate).

## 7. Concurrency & State Ownership
Per §7 of conventions: runtime assumptions, shared-state primitives, channels,
cancellation, lock discipline. State which data is `Arc`, which is owned, which
is behind a lock and why.

## 8. Behavior & Invariants
The semantics that must be preserved (cite the plan). State machines, ordering,
terminal/exit-gate rules, etc. Call out the subtle risks named in the plan.

## 9. SOLID & Principles Applied
Map THIS crate's seams to DIP/OCP/ISP/LSP/SRP (reference §6 seam map). Note
KISS/YAGNI/DRY decisions and which non-goals this crate must respect.

## 10. Gap Closeouts (tracked requirements)
Turn each plan "Gap closeout" bullet for this module into a numbered requirement
GC-<crate>-NN with a one-line resolution. These are mandatory.

## 11. Acceptance Criteria
AC-<crate>-NN testable assertions, each naming the proving test and mapping to
"Tests to Port First" where relevant.

## 12. Implementation Checklist
Ordered, small, verifiable steps (small-incremental-changes).

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`<crate>` per spec-conventions.md §13. Do not edit other crates' rows.
```

---

## 13. Progress Tracker Protocol

- The Progress Tracker is the **last section of `overview.md`** so rows can be
  appended without churn.
- Status vocabulary: `NOT STARTED` → `IN PROGRESS` → `IN REVIEW` → `DONE`
  (optionally `BLOCKED`).
- When an implementing agent finishes a crate, it **updates only its own row**
  (status + date + short note + commit/PR ref). It never rewrites other rows.
- Each `impl-*.md` ends with the completion instruction pointing here.

---

## 14. Cargo / Workspace Conventions (summary; full detail in impl-workspace.md)

- Cargo **workspace** with shared `[workspace.dependencies]`
  (`proj-workspace-large`, `proj-workspace-deps`); crate names have **no `-rs`
  suffix** (`name-crate-no-rs`).
- Workspace lints (`lint-workspace-lints`): `#![deny(clippy::correctness)]`,
  `#![warn(clippy::suspicious, clippy::style, clippy::complexity, clippy::perf)]`,
  selective `pedantic`, `missing_docs` on public crates.
- Release profile: `lto="fat"`, `codegen-units=1`, `strip=true`
  (`opt-lto-release`, `opt-codegen-units`). `panic` strategy decided in
  impl-workspace.md (note: query-loop/background error recovery argues for
  `unwind`, not `abort` — resolve there).
- `cargo fmt --check` + `clippy -D warnings` in CI (`lint-rustfmt-check`).
