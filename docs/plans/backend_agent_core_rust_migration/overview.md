# Agent-Core Rust Migration — Implementation Overview & Index

> Detailed, spec-driven companion to the high-level plan
> `../backend_agent_core_rust_migration_PLAN.md`. This document is the **index**,
> the **phase-by-phase development plan**, and (at the end) the **append-only
> progress tracker**. Per-module technical specs live in the `impl-*.md` files
> listed below.
>
> **Read order:** `spec-conventions.md` (the shared anchor — naming, contract
> ownership, SOLID seam map, doc template, tracker protocol) → this overview →
> the `impl-*.md` for the crate you are building.

Date: 2026-06-02 · Target: migrate the Python control plane under `backend/src`
into a Rust `agent-core` Cargo workspace placed at the repository root —
`/Users/yifanxu/machine_learning/LoVC/EphemeralOS/agent-core/` — a sibling of
`backend/` and the existing `sandbox/` Rust workspace (separate workspace, not a
`sandbox/` member). All crate paths in these docs are relative to that root.

---

## 1. What this document set is

| File | Role |
|---|---|
| `spec-conventions.md` | **Anchor.** Single source of truth: tension resolution, hard non-goals, naming/glossary, **Contract Ownership Map**, **SOLID Seam Map**, concurrency/error/type conventions, the `impl-*.md` template, and the progress-tracker protocol. |
| `overview.md` (this) | Index, scope-coverage map, dependency topology, phase plan, cross-cutting workstreams, risks, progress tracker. |
| `impl-workspace.md` | Phase-0 workspace scaffolding + parity harness. |
| `impl-eos-*.md` (×15) | One spec per crate: files, contracts, types/fields/schemas, concurrency/state, SOLID rationale, gap-closeouts, acceptance criteria. |
| `impl-cutover.md` | Phase-7 compatibility boundary, switch/rollback protocol, parity comparator, retirement gates. |

Every `impl-*.md` follows the anchor §12 template and ends with an instruction to
update the Progress Tracker (§Progress Tracker, below) for its own row only.

## 2. Governing design principle (flexibility vs the plan's non-goals)

The user wants **flexible, extensible, SOLID** code; the plan **forbids
speculative abstraction**. These reconcile under one rule (anchor §1):

> **Flexibility lives in the trait seams and registries the plan already defines
> — not in new layers. Add no abstraction outside the SOLID Seam Map.**

- **DIP** → the trait seams: `Clock`, per-entity `Store`s, `LlmClient`,
  `EventSource`, `ToolExecutor`, `AuditSink`, `SandboxTransport`,
  `ProviderAdapter`. The composition root (`eos-runtime`) wires concretes in.
- **OCP** → the registries (tool, agent, provider, skill, plugin catalog):
  extend by registering, never by editing dispatch.
- **ISP** → per-entity store traits, small tool/transport traits.
- **LSP** → provider-neutral `LlmStreamEvent`/`Message`; exhaustive state enums.
- **SRP** → the crate boundaries themselves.

Forbidden (anchor §2): global orchestrator, synthetic root workflow, tool
visibility enum, lazy/deferred model-facing tool loading, `class_path` dynamic
dispatch, PostgreSQL, p2p agent messaging.

## 3. Scope-coverage map (every in-scope Python area → crate)

Confirms nothing in-scope was dropped. Out-of-scope: `test_runner`,
`providers/clients/coding_plan`, sandbox daemon internals
(`daemon`/`ephemeral_workspace`/`overlay`/`layer_stack`/`occ`/isolated impl) and
the existing `eos-daemon`/`eos-*` daemon crates.

| Python area (`backend/src/…`) | Target crate |
|---|---|
| `task/` | `eos-state` (+ ids in `eos-types`) |
| `workflow/_core/` (state, outcomes, persistence, submissions) | `eos-state` |
| `workflow/` (starter, attempt/*, context_engine/*, iteration_coordinator, lifecycle, composer) | `eos-workflow` |
| `runtime/` (entry, app_factory, sandbox_provisioning) | `eos-runtime` |
| `engine/` (query, tool_call, background, agent, audit/stream) | `eos-engine` |
| `notification/`, `prompt/` | `eos-engine` |
| `agents/` (definition, skills loader) | `eos-agent-def` |
| `tools/` (`_framework`, `_names`, `_terminals`, sandbox, workflow, submission, subagent, ask_helper, skills, isolated_workspace) | `eos-tools` |
| `message/` | `eos-llm-client` |
| `providers/` (excl. `clients/coding_plan`) | `eos-llm-client` |
| `audit/` | `eos-audit` |
| `config/` | `eos-config` |
| `db/` | `eos-db` (traits in `eos-state`) |
| `sandbox/api/`, `sandbox/shared/models.py` | `eos-sandbox-api` |
| `sandbox/provider/`, `sandbox/host/` (host-facing) | `eos-sandbox-host` |
| `plugins/core/` (manifest, discovery, loader), `plugins/catalog/*/plugin.md` | `eos-plugin-catalog` |
| `skills/` (core, bundled) | `eos-skills` |
| shared IDs/timestamps/value types (cross-cutting) | `eos-types` |

## 4. Dependency topology

Strict dependency direction (acyclic). **Refinement of the plan's DAG (anchor
§5a):** `eos-tools → eos-llm-client` is added so `ToolSpec` has one owner
(`eos-llm-client`); this is acyclic and DIP-correct.

**Seam-map addition (anchor §6a):** the `AgentRunner` trait is owned by
`eos-workflow` (runtime adapter over `run_ephemeral_agent`). This keeps
`eos-workflow` free of an `eos-engine` edge while giving the agent-run call a
named, testable seam (the Python `AttemptAgentRunner` DI param promoted to a
trait).

**Seam-map addition (anchor §6b):** six downstream-state ports
(`WorkflowControlPort`, `PlanSubmissionPort`, `SubagentSupervisorPort`,
`AdvisorPort`, `IsolatedWorkspacePort`, `NotificationSink`) are owned by
`eos-tools` and implemented downstream, keeping `eos-tools` free of
`eos-engine`/`eos-workflow` edges. The implementors ride existing edges —
`eos-engine → eos-tools` (supervisor, advisor, notifications) and
`eos-workflow → eos-tools` (workflow control, plan submission). The
`IsolatedWorkspacePort` implementor is the **`eos-runtime`** adapter over the
`eos-sandbox-host` lifecycle (sandbox-host is upstream of `eos-tools`, so the port
is wired at the composition root, not by a forbidden `eos-sandbox-host → eos-tools`
edge).

**Contract-ownership resolution:** `RawExecResult` is owned by **`eos-sandbox-host`**
(the `ProviderAdapter::exec` return). `impl-eos-sandbox-api.md` explicitly drops
it as "a host concern", and anchor §5's `eos-sandbox-api` row does not enumerate
it, so the host owns it with no anchor change required.

```text
eos-types
  ├─ eos-state         (types)
  ├─ eos-audit         (types)
  ├─ eos-sandbox-api   (types)
  └─ eos-agent-def     (types)

eos-config         (no internal upstream edge)
eos-llm-client     -> types, config
eos-skills         -> types, config
eos-db             -> state, config
eos-sandbox-host   -> sandbox-api, config
eos-plugin-catalog -> sandbox-api, audit, config

eos-tools          -> state, sandbox-api, skills, audit, llm-client   (§5a edge)

eos-engine         -> llm-client, tools, audit, agent-def
eos-workflow       -> state, tools, agent-def, audit

eos-runtime        -> db, engine, workflow, sandbox-host, config, agent-def,
                      skills, plugin-catalog        (composition root)
```

## 5. Phase-by-phase development plan

Phases are a **topological layering** of the dependency graph: every crate in a
phase depends only on crates from earlier phases, so **all crates within one
phase can be built in parallel**. Phases are strictly sequential. Each phase ends
with the verification gate from the plan's Migration Phases section.

### Phase 0 — Scaffolding & Parity Harness  *(blocks everything)*
- **`impl-workspace.md`** — workspace `Cargo.toml`, 15 crate skeletons + dep
  wiring (incl. the §5a edge), workspace lints/profiles, `fmt`/`clippy` CI, and
  the parity harness (Pydantic schema snapshots, provider SSE fixtures,
  prompt-report goldens, SQLite schema snapshots).
- **Gate:** `cargo fmt --check`; `cargo clippy --workspace --all-targets -D
  warnings`; schema snapshots wired.
- **Phase-0 implementation notes (2026-06-02):**
  - **`panic = "unwind"` (GC-workspace-04):** set in `[profile.release]` and
    inherited by `[profile.bench]` (Cargo ignores an explicit `panic` for bench).
    The engine query loop and background supervisor recover from per-task /
    per-attempt panics; an `abort` strategy would escalate a single
    tool/SSE-parse panic to a whole-process kill, destroying in-flight sibling
    background tasks and persisted-state coherence. This intentionally diverges
    from the sibling `sandbox/` daemon workspace (`abort`).
    `parity/tests/profiles.rs` guards against regression to `abort`.
  - **Frozen edge set & reconciliations:** the internal `eos-* -> eos-*` edge set
    asserted by `parity/tests/dependency_dag.rs` follows the §4 dependency
    topology. Two corrections to `impl-workspace.md` §5's edge table align it with
    the topology it says to mirror and with each crate's own `impl-*.md` §2:
    (1) `eos-plugin-catalog -> sandbox-api, audit, config` (§5's `audit`-only row
    was an omission; the topology and `impl-eos-plugin-catalog.md` §2 list all
    three); (2) the Phase-0 skeleton's temporary `eos-config -> eos-types` edge
    is explicitly **not** part of the final topology — Phase 2 prunes that edge
    and updates the dependency test when `eos-config` is implemented.
  - **Parity corpus deferrals:** sandbox request/result DTOs are
    `@dataclass`es (no `model_json_schema()`) and `ToolSpec` goldens need
    per-agent tool binding; neither is frozen in Phase 0. Both are deferred to
    their owning crates' phases
    (`eos-sandbox-api`, `eos-llm-client`/`eos-tools`). `Message` + content blocks
    + `TextToolOutput` satisfy AC-workspace-05. See `parity/README.md`.

### Phase 1 — Foundation  *(sequential; blocks all domain crates)*
- **`impl-eos-types.md`** — IDs, `UtcDateTime`, `Clock`, `CoreError`,
  `JsonObject`.
- **Gate:** ID round-trip (`Display`/`FromStr`/serde/schemars) tests.

### Phase 2 — Leaf domain & boundary crates  *(parallel; deps ⊆ {types})*
- **`impl-eos-config.md`**, **`impl-eos-state.md`**, **`impl-eos-audit.md`**,
  **`impl-eos-sandbox-api.md`**, **`impl-eos-agent-def.md`**.
- **Gate:** config env-override tests; state outcome-projection tests; audit
  JSONL golden + redaction; sandbox-api envelope shape; agent-def load/validate.

### Phase 3 — Persistence, providers, sandbox host, plugins, skills  *(parallel; deps ⊆ Phases 0–2)*
- **`impl-eos-llm-client.md`** (types, config), **`impl-eos-skills.md`**
  (types, config), **`impl-eos-db.md`** (state, config), **`impl-eos-sandbox-host.md`**
  (sandbox-api, config), **`impl-eos-plugin-catalog.md`** (sandbox-api, audit, config).
- **Gate:** Anthropic/OpenAI SSE fixture replay + retry/error-mapping tests; DB
  store round-trips + migration tests; provider-selection + provisioning tests;
  plugin-manifest validation; skill reference-loading determinism.

### Phase 4 — Tool framework  *(deps include skills + llm-client from Phase 3)*
- **`impl-eos-tools.md`** — specs, registry, execution, hooks, terminal stamping,
  dispatch policy, all model-facing tools.
- **Gate:** terminal-batch rejection; terminal-success stamping; lifecycle batch
  policy; tool-schema snapshots; terminal-descriptor totality; prompt/description
  coverage (ports `test_tool_execution.py`, `test_schema_summary.py`,
  `test_submission_main_role_terminals.py`, `test_exec_command.py`,
  `test_write_stdin.py`).

### Phase 5 — Execution core  *(parallel; deps include tools)*
- **`impl-eos-engine.md`** (llm-client, tools, audit, agent-def),
  **`impl-eos-workflow.md`** (state, tools, agent-def, audit).
- **Gate:** query-loop stop + terminal non-submission ceiling; background
  command/session cancellation; prompt-report golden; notification rules; planner
  DAG validation; generator/reducer scheduling; reducer exit-gate; attempt close
  + outcome projection (ports `test_tool_batch.py`,
  `test_tool_call_dispatch_lifecycle.py`, workflow DAG/orchestrator/context tests).

### Phase 6 — Composition root  *(sequential; the integration point)*
- **`impl-eos-runtime.md`** — `AppState` DI graph, `start_request`, root-agent
  lifecycle (no root workflow), sandbox provisioning.
- **Gate:** root request creates root Task + no root workflow; `delegate_workflow`
  creates `Workflow→Iteration→Attempt` and leaves parent task running.

### Phase 7 — Cutover  *(compatibility boundary; no new Rust crate)*
- **`impl-cutover.md`** — the executable old/new boundary: subprocess JSON-RPC
  adapter, config/env switch, parity comparator, rollback trigger, DB compatibility
  invariant, and Python-package retirement gates.
- Run Rust control plane against the existing daemon + DB fixtures; retire Python
  modules by package boundary only after the matching gate passes; rebuild
  test-runner integration separately.
- **Gate:** E2E root request with mocked LLM; delegated workflow E2E with
  planner/generator/reducer fixtures; sandbox tool integration against the Rust
  daemon; Anthropic/OpenAI provider mock tests; old/new comparator over root,
  delegated workflow, sandbox-tool, and provider-stream fixtures; rollback command
  documented and exercised.

### Load / performance gates

These gates complement the per-crate correctness ACs and prevent the Rust rewrite
from regressing hot-path behavior while it is still staged:

- `eos-llm-client`: Criterion benchmark for SSE frame splitting + provider decode
  on captured Anthropic/OpenAI fixtures; no full-body buffering regression.
- `eos-engine`: query-loop/tool-dispatch benchmark over a fixed scripted
  transcript, including foreground fan-in and terminal-batch rejection.
- `eos-db`: concurrent SQLite store roundtrips under WAL/busy-timeout with
  workflow/iteration/attempt/task inserts and close updates.
- `eos-workflow`: fan-out/fan-in scheduler load test with reducer gate.
- Phase 7: old/new adapter E2E latency comparison for root + delegated workflows,
  reported as an artifact before package retirement.

## 6. Module index

| Phase | Crate | Spec | Depends on | Key owned contracts |
|---|---|---|---|---|
| 0 | (workspace) | `impl-workspace.md` | — | Cargo workspace, lints, profiles, parity harness |
| 1 | `eos-types` | `impl-eos-types.md` | — | newtype IDs, `UtcDateTime`, `Clock`, `CoreError` |
| 2 | `eos-config` | `impl-eos-config.md` | types | `CentralConfig`, section configs, env/path loaders |
| 2 | `eos-state` | `impl-eos-state.md` | types | domain state, status enums, outcome projections, **`Store` traits**, submission DTOs |
| 2 | `eos-audit` | `impl-eos-audit.md` | types | `AuditEvent`/`AuditNode`, **`AuditSink`**, bus, JSONL, redaction |
| 2 | `eos-sandbox-api` | `impl-eos-sandbox-api.md` | types | `SandboxCaller`/req/result, op constants, **`SandboxTransport`**, `tool_api` |
| 2 | `eos-agent-def` | `impl-eos-agent-def.md` | types | `AgentDefinition`, `AgentRole`/`AgentType`, `AgentRegistry`, recipes |
| 3 | `eos-llm-client` | `impl-eos-llm-client.md` | types, config | neutral `Message`/events, **`ToolSpec`**, `LlmRequest`, **`LlmClient`**, `ProviderError` |
| 3 | `eos-skills` | `impl-eos-skills.md` | types, config | `SkillDefinition`, `SkillRegistry`, loader |
| 3 | `eos-db` | `impl-eos-db.md` | state, config | `SqlitePool`, migrations, row structs, repository impls of `Store` traits |
| 3 | `eos-sandbox-host` | `impl-eos-sandbox-host.md` | sandbox-api, config | **`ProviderAdapter`**, provider registry, daemon client, lifecycle |
| 3 | `eos-plugin-catalog` | `impl-eos-plugin-catalog.md` | sandbox-api, audit, config | `PluginManifest`, `PluginCatalog`, plugin tool specs, plugin audit |
| 4 | `eos-tools` | `impl-eos-tools.md` | state, sandbox-api, skills, audit, llm-client | `ToolName`, `ToolIntent`, **`ToolExecutor`**, `ToolRegistry`, terminal descriptors |
| 5 | `eos-engine` | `impl-eos-engine.md` | llm-client, tools, audit, agent-def | `QueryContext`, query loop, background supervisor, **`EventSource`**, notifications, prompt report |
| 5 | `eos-workflow` | `impl-eos-workflow.md` | state, tools, agent-def, audit | `WorkflowStarter`, `AttemptOrchestrator`, plan-DAG, `ContextEngine` |
| 6 | `eos-runtime` | `impl-eos-runtime.md` | db, engine, workflow, sandbox-host, … | `AppState` (composition root), `RequestEntry`, provisioning |

## 7. Cross-cutting workstreams

- **Tool description & schema conversion (plan §Tool Description…):** one
  colocated `ToolSpec` source per model-facing tool; `const`/`include_str!` for
  long text; schemas from `schemars`; terminal descriptors total; typed
  `ToolName` constants exhaustive. Owned by `eos-tools`; conventions in anchor §10.
- **Provider neutrality (plan §API Client Layer):** `LlmClient` streams neutral
  events; Anthropic/OpenAI encoders live in provider modules; retry only before
  visible output. Owned by `eos-llm-client`.
- **Parity harness (Phase 0):** Pydantic→serde schema snapshots, SSE fixture
  replay, prompt-report goldens, SQLite schema snapshots — the safety net that
  makes the staged rewrite low-risk. Owned by `impl-workspace.md`.
- **Naming normalization (anchor §4):** applied in every crate; reviewers gate it.

## 8. Top risks (from plan §Main risks) and where they are handled

| Risk | Handled in |
|---|---|
| Query-loop + terminal-tool subtlety | `eos-engine` §Behavior/Concurrency; ports `test_tool_batch`, dispatch lifecycle |
| Background cancellation / parent-exit semantics | `eos-engine` background supervisor (`JoinSet`+`CancellationToken`) |
| Reducer exit-gate / attempt scheduling | `eos-workflow` run-stage + orchestrator invariants |
| Sandbox host protocol compat with ongoing Rust daemon | `eos-sandbox-api` op constants + `eos-sandbox-host` daemon client |
| Prompt/tool description drift changing model behavior | Phase-0 parity goldens + `eos-tools` single-source specs + reviewer gate |

---

## Progress Tracker

> **Append-only.** Status: `NOT STARTED` → `IN PROGRESS` → `IN REVIEW` → `DONE`
> (`BLOCKED` if stuck). On completing a crate's implementation, update **only
> that crate's row** (status, date, short note, commit/PR ref). Do not rewrite
> other rows. Spec authoring status is tracked separately from implementation
> status.

### Spec authoring

> Each spec was authored grounded in the real Python source + the anchor, then
> adversarially reviewed against source accuracy + the anchor's contract-ownership
> map, then fixed. `eos-engine` received its review+fix on the resume run.

| Doc | Spec authored | Reviewed+fixed | Notes |
|---|---|---|---|
| `spec-conventions.md` | DONE (2026-06-02) | hand-authored | Anchor; extended in-run with the §6a `AgentRunner` seam + §6b downstream-state ports (verified coherent) |
| `overview.md` | DONE (2026-06-02) | hand-authored | This file |
| `impl-workspace.md` | DONE (2026-06-02) | yes | Workspace + parity harness; `panic=unwind`; §5a edge encoded |
| `impl-eos-types.md` | DONE (2026-06-02) | yes | IDs/`UtcDateTime`/`Clock`/`CoreError`; `SandboxId` sourced from `audit/base.py` (source-accuracy fix) |
| `impl-eos-config.md` | DONE (2026-06-02) | yes | `CentralConfig`; SQLite-only; 15 legacy env adapters; network-URL reject |
| `impl-eos-state.md` | DONE (2026-06-02) | yes | DTOs + 7 per-entity `Store` traits (ISP); outcome projections; submission DTOs |
| `impl-eos-audit.md` | DONE (2026-06-02) | yes | `AuditEvent`/`Node`/`Sink`/bus/JSONL; deterministic redaction; `schema_version`; kind-in-payload |
| `impl-eos-sandbox-api.md` | DONE (2026-06-02) | yes | Transport seam + DTOs + pure `tool_api`; `tool_name` removed (GC-01) |
| `impl-eos-agent-def.md` | DONE (2026-06-02) | yes | `AgentDefinition`/roles; `AgentRegistry` app-state; tools = `allowed ∪ terminals` at spawn |
| `impl-eos-llm-client.md` | DONE (2026-06-02) | yes | Neutral types + `ToolSpec` (§5a); Anthropic/OpenAI SSE; `Reasoning` rename; retry gating |
| `impl-eos-skills.md` | DONE (2026-06-02) | yes | `SkillDefinition`/`Registry`/loader; config-root loading decided; deterministic refs |
| `impl-eos-db.md` | DONE (2026-06-02) | yes | sqlx SQLite repos of `Store` traits; migrations; column↔domain mapping; `class_path`=migration-only |
| `impl-eos-sandbox-host.md` | DONE (2026-06-02) | yes | `ProviderAdapter` + registry app-state; Docker only (seam kept for future providers); daemon client; artifact upload |
| `impl-eos-plugin-catalog.md` | DONE (2026-06-02) | yes | Manifest/catalog; kinds enum; Rust-native plugin tool specs; LSP boundary only |
| `impl-eos-tools.md` | DONE (2026-06-02) | yes | `ToolName`/`Intent`/`Executor`/`Registry`; terminal descriptors total; **owns** §6b ports |
| `impl-eos-engine.md` | DONE (2026-06-02) | yes (resume) | Query loop + deferred terminal enforcement; `EventSource` seam; `JoinSet`+`CancellationToken` supervisor; prompt-report system-role fix; implements §6b ports |
| `impl-eos-workflow.md` | DONE (2026-06-02) | yes | Starter/orchestrator/run-stage/`ContextEngine`; **owns** `AgentRunner` (§6a); implements §6b ports |
| `impl-eos-runtime.md` | DONE (2026-06-02) | yes | `AppState` composition root; no root workflow; supplies `AgentRunner` adapter + wires all ports |
| `impl-cutover.md` | DONE (2026-06-02) | yes | Subprocess JSON-RPC compatibility boundary, switch/rollback, DB invariant, retirement gates |

### Implementation

| Phase | Crate | Status | Date | Note / commit |
|---|---|---|---|---|
| 0 | (workspace) | DONE | 2026-06-02 | agent-core workspace + 15 crate skeletons + eos-parity harness; frozen DAG/profiles/schema/SSE/prompt-report guard tests green (`cargo fmt --check`, `clippy -D warnings`, `test -p eos-parity`); CI workflow `.github/workflows/agent-core.yml` added. Phase-0 gap pass (2026-06-02): (a) frozen DAG corrected to drop the spurious `eos-engine -> eos-state` edge so the §5/§4 edge set matches across all 16 rows (AC-workspace-02); (b) added the AC-workspace-10 CI "observability smoke" step (`cargo check --workspace --features eos-runtime/tokio-console`) — the default `build` never enabled `console-subscriber`, so AC-10's named proof was missing from CI; (c) the `eos-parity` `unwrap_used`/`print_stdout = allow` override (impl checklist step 5) is intentionally deferred — `[lints] workspace = true` cannot also carry per-lint overrides, and the guard tests use `.expect()` + `pretty_assertions` (no `unwrap`/`println`), so `clippy -D warnings` is green without it. Uncommitted. |
| 1 | eos-types | NOT STARTED | — | — |
| 2 | eos-config | NOT STARTED | — | — |
| 2 | eos-state | NOT STARTED | — | — |
| 2 | eos-audit | NOT STARTED | — | — |
| 2 | eos-sandbox-api | NOT STARTED | — | — |
| 2 | eos-agent-def | NOT STARTED | — | — |
| 3 | eos-llm-client | NOT STARTED | — | — |
| 3 | eos-skills | NOT STARTED | — | — |
| 3 | eos-db | NOT STARTED | — | — |
| 3 | eos-sandbox-host | NOT STARTED | — | — |
| 3 | eos-plugin-catalog | NOT STARTED | — | — |
| 4 | eos-tools | NOT STARTED | — | — |
| 5 | eos-engine | NOT STARTED | — | — |
| 5 | eos-workflow | NOT STARTED | — | — |
| 6 | eos-runtime | NOT STARTED | — | — |
| 7 | cutover | NOT STARTED | — | — |
