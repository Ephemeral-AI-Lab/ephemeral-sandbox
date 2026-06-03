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
eos-workflow       -> types, state, tools, agent-def, audit

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
  **`impl-eos-workflow.md`** (types, state, tools, agent-def, audit).
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
- `eos-workflow`: fan-out/fan-in scheduler load test with a configured per-attempt
  `max_concurrent_task_runs` cap and reducer gate.
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
| 5 | `eos-workflow` | `impl-eos-workflow.md` | types, state, tools, agent-def, audit | `WorkflowStarter`, `AttemptOrchestrator`, plan-DAG, `ContextEngine` |
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
| Background cancellation / parent-exit semantics | `eos-engine` background supervisor status/handle tracker; spawned runner + `CancellationToken` wiring is a Phase-6 residual |
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
| `impl-eos-engine.md` | DONE (2026-06-02) | yes (resume) | Query loop + deferred terminal enforcement; `EventSource` seam; supervisor status/handle semantics with Phase-6 spawned-runner residual; prompt-report system-role fix; implements §6b ports |
| `impl-eos-workflow.md` | DONE (2026-06-02) | yes | Starter/orchestrator/run-stage/`ContextEngine`; **owns** `AgentRunner` (§6a); implements §6b ports |
| `impl-eos-runtime.md` | DONE (2026-06-02) | yes | `AppState` composition root; no root workflow; supplies `AgentRunner` adapter + wires all ports |
| `impl-cutover.md` | DONE (2026-06-02) | yes | Subprocess JSON-RPC compatibility boundary, switch/rollback, DB invariant, retirement gates |

### Implementation

| Phase | Crate | Status | Date | Note / commit |
|---|---|---|---|---|
| 0 | (workspace) | DONE | 2026-06-02 | agent-core workspace + 15 crate skeletons + eos-parity harness; frozen DAG/profiles/schema/SSE/prompt-report guard tests green (`cargo fmt --check`, `clippy -D warnings`, `test -p eos-parity`); CI workflow `.github/workflows/agent-core.yml` added. Phase-0 gap pass (2026-06-02): (a) frozen DAG corrected to drop the spurious `eos-engine -> eos-state` edge so the §5/§4 edge set matches across all 16 rows (AC-workspace-02); (b) added the AC-workspace-10 CI "observability smoke" step (`cargo check --workspace --features eos-runtime/tokio-console`) — the default `build` never enabled `console-subscriber`, so AC-10's named proof was missing from CI; (c) the `eos-parity` `unwrap_used`/`print_stdout = allow` override (impl checklist step 5) is intentionally deferred — `[lints] workspace = true` cannot also carry per-lint overrides, and the guard tests use `.expect()` + `pretty_assertions` (no `unwrap`/`println`), so `clippy -D warnings` is green without it. Uncommitted. |
| 1 | eos-types | DONE | 2026-06-02 | 12 newtype IDs via `define_id!` (`TryFrom<String>`/`TryFrom<&str>` parse-don't-validate; `new_v4` on all but `ToolUseId`); `UtcDateTime` (Copy, UTC-normalized incl. deserialize path, rfc3339 serde + `schema_with` date-time); `Clock`/`SystemClock`/`TestClock`; `CoreError`; `JsonValue`/`JsonObject`. AC-types-01..06 covered (47 tests). Gates green: `cargo fmt --check`, `clippy -p eos-types --all-targets -D warnings`, `test -p eos-types`; eos-parity unaffected. 3-lens adversarial review applied: fixed From→TryFrom (empty-id invariant), deserialize UTC-normalization blocker, and 9→12 IDs after a parallel spec edit. Per-user decision: the 12th ID is `WorkflowSessionId` (renamed from spec's `WorkflowTaskId` for consistency with `Command`/`SubagentSessionId`; wire/tool param stays `workflow_task_id`) — spec/anchor/tools/engine refs updated to match. Deferred (cutover, not Phase-1): exact `Z`-vs-`+00:00` byte parity with Python isoformat (parsing accepts both). Uncommitted. |
| 2 | eos-config | DONE | 2026-06-03 | `CentralConfig{database,sandbox,providers,attempt}` + `DatabaseUrl` (network-URL reject), Docker-only `SandboxProvider`, `RetryConfig` (`BTreeSet<u16>`), Rust-only `AttemptConfig`(=8); `ConfigLoader` hand-rolls `defaults<YAML<env<init` deep-merge over `serde_yaml::Value` (no figment — recursive map-merge is trivial); env injected (not process-env mutation) for deterministic parallel tests; provider alias applied **per-source** (not post-merge) so a source `provider` is not dropped by the always-present default; all-scalar YAML coercion; 9 legacy adapters (legacy>EOS__ on shared path); pure no-mkdir `paths` (CWD=repo-root proxy). AC-eos-config-01..11 + 2 subtle-risk tests (legacy-beats-EOS__, alias-always-pops) green (22 tests); AC-01 confirmed red under inverted precedence then reverted. **DAG edge prune:** removed scaffold `eos-config→eos-types` edge; updated `parity/tests/dependency_dag.rs` frozen set to `("eos-config",&[])` (authorized by Phase-0 note + impl-workspace §5) — `internal_edges_match_frozen_set` green. Added `serde_yaml="0.9"` to workspace deps. AC-10 = field-name cross-check vs committed Python schema fixture + normalized `insta` snapshot; **deferred (loud):** type-level Pydantic schema parity to Phase-7 cutover (no recorded golden; surviving-subset comparator under-specified). **Known cutover requirement (loud):** the existing repo `ephemeralos.yaml` carries top-level `runner:` and `sandbox.daytona:` sections that `deny_unknown_fields` (GC-06, correct per spec) rejects — so `load_central_config()` run against today's deployment YAML returns `ParseYaml`; Phase-7 needs a YAML migration (drop `runner`/`daytona`) or a tolerant compat-loader. Gates green: `fmt --check`, `clippy -p eos-config --all-targets -D warnings`, `test -p eos-config`, `test -p eos-parity`, `check --workspace` (workspace-wide `clippy -D warnings` blocked only by a parallel agent's mid-edit syntax error in `eos-sandbox-api`, unrelated to this crate). Uncommitted. |
| 2 | eos-state | DONE | 2026-06-03 | `task`/`workflow`/`iteration`/`attempt`/`request`/`agent_run`/`model`/`outcomes`/`submissions`/`store` + `#[cfg(test)] fakes`. AC-eos-state-01..09 covered (13 tests; the **AC-07** proving test `model_registration_no_class_path_dispatch` — field-presence + assembled-needle `class_path`-no-dispatch grep — was added this session, making the AC-01..09 claim genuinely complete). DTOs are plain owned structs (deliberately **not** `#[non_exhaustive]` — `eos-db` constructs them via struct literals per impl-eos-db §6; enums are exhaustive state machines for LSP); enums `#[serde(rename_all="snake_case")]` matching Python wire strings (`deferred_goal_continuation`/`run_exhausted`/`task_failed`/etc.). `Task.role` is the local 4-variant `TaskRole` (GC-05, keeps DAG acyclic). The 7 `#[async_trait]` `Store` traits return `Result<_, CoreError>` (`StoreError` alias) and extend a `#[doc(hidden)] pub trait Sealed` — true crate-private sealing is incompatible with `eos-db` (a separate downstream crate) implementing them, so the marker is doc-hidden-pub (the documented "friend" seal). Pure outcome projections ported from `outcomes.py`; the JSON string↔records codec + raw-record normalization fallbacks are ceded to `eos-db` (GC-03), so projections read pre-normalized typed `Vec<ExecutionTaskOutcome>`. AC-05 (closing-attempt-only filter; earlier reducer successes hidden) + AC-02 (no `executor` token; needle assembled to avoid self-match) + AC-03 (serde wire table + `proptest!` round-trip) + AC-04 (`insta` submission-schema drift snapshot; Pydantic parity deferred to cutover, no golden — consistent with eos-config). **Re-exports** the eos-types primitives in its public API (IDs/`UtcDateTime`/`CoreError`/`JsonObject`) so `eos-db` names them without a direct `eos-db→eos-types` edge — frozen DAG `eos-db→{state,config}` kept intact (no parity edit). **Cross-crate change (recorded here, not in eos-types' row):** added one `CoreError::Store(String)` variant to `eos-types` (it is `#[non_exhaustive]`) so `eos-db::DbError` can flatten into the `Store`-trait contract error via `impl From<DbError> for CoreError` (the leaf `CoreError` cannot name `DbError`); +1 eos-types test. Gates green: `cargo fmt --check`, `clippy -p eos-state --all-targets -D warnings`, `test -p eos-state` (13), `clippy -p eos-types`/`test -p eos-types` (49), `test -p eos-parity --test dependency_dag` (3, frozen set unchanged). **Cleanup pass (2026-06-03):** review→adversarial-verify workflow (4 dimensions; each finding verified against downstream consumers + the spec contract). Removed the speculative unused `JsonValue` re-export (zero downstream consumers — `eos-db` uses a local `serde_json::Value` alias, `eos-audit` uses `eos_types` directly; absent from spec §5.3 USED list; `JsonObject` kept). Deduped two test-only skeletons: shared `crate_src_files()` for the AC-02/AC-07 source-greps, and `apply_task_updates()` in `fakes.rs`. Added the AC-03 `proptest!` serialize→deserialize round-trips over `ExecutionTaskOutcome`/`PlannerSubmission` (closes the AC-03 proptest half + makes the `proptest` dev-dep genuinely used). **Kept (verified owned contract, not removed):** `StoreError` alias (spec §4-mandated, unused-but-contract). **Flagged, deliberately NOT changed (would break the build):** DTOs/enums lack the spec §6-mandated `#[non_exhaustive]` because `eos-db` constructs them via cross-crate struct literals (adding it = E0639) — a spec-vs-impl divergence for the spec owner, not an eos-state code change. `dup-task-builder` candidate rejected (additive, not a reduction; spec §9 discourages it). Gates re-verified green after the pass. Uncommitted. |
| 2 | eos-audit | DONE | 2026-06-02 | `AuditEvent`/`AuditNode`(+builder)/`AuditSource`; `AuditSink`+`NoopAuditSink`; sync `AuditEventBus` with `Err`-stashing isolation (GC-audit-04, panics out of contract); `JsonlSink` + production `BufferedJsonlSink`/`BufferedAuditShutdown` (bounded sync channel + writer thread, `Backpressure`); deterministic redaction (recursive sorted-key canonical bytes, `sha256:` digest, encoded size, shape/redacted-shape, lists→5; GC-audit-02); neutral `tool_started`/`tool_completed` constructors (no engine dep, GC-audit-05) + 3 fixed `plugin.*` constructors with `plugin_kind`-in-payload `"custom"` fallback (GC-audit-06); single Clock-stamped `ts` + top-level `schema_version=1` (GC-audit-01). AC-audit-01..10 covered (14 tests incl. byte-exact golden JSONL + proptest digest); crate-local `tests/no_downstream_deps.rs` (AC-audit-07). Added `sha2` to workspace deps. Gates green: `cargo fmt --check`, `clippy -p eos-audit --all-targets -D warnings`, `test -p eos-audit`; parity `dependency_dag` unaffected. Uncommitted. |
| 2 | eos-sandbox-api | DONE | 2026-06-03 | Transport seam (`#[async_trait] SandboxTransport`) + `DaemonOp` (current verbatim wire strings, including `api.v1.exec_command` / `api.v1.exec_stdin`, no `api.v1.shell`) + `Intent`/`Workspace`/`SandboxCaller` (no `tool_name`, GC-01; `identity_block()` + typed accessors) + all verb/isolated DTOs + `ToolCallRequest` + pure `tool_api` (hand-written `parse_*` with fail-closed `success`/`exists`, strict-int bool-reject, blank-path filtering, exec success-from-status, edit conflict→`Ok`, `exec_stdin` plus model-facing `write_stdin` alias, control RPCs). AC-01..07 covered (25 tests: 24 lib + 1 schema-snapshot integration). Gates green: `cargo fmt --check`, `clippy -p eos-sandbox-api --all-targets -D warnings`, `test -p eos-sandbox-api`; workspace clippy + eos-parity DAG/profile guards unaffected (only internal edge is `eos-types`). Decisions: `RawExecResult` dropped (host concern); audit wrapping deferred to `eos-tools` (no `eos-audit` edge); TimingKey keys passed through verbatim (str-Enum already serializes as value — full enum port would couple to daemon internals); `from_payload` fallible with typed `InvocationId`; `missing_docs` opted in (matches eos-types). 3-lens adversarial review (payload/parse/contract+AC, each finding refutation-tested) surfaced one confirmed divergence — guarded `mutation_source` now uses Python `str(x or "")` falsy-collapse — fixed + regression-tested. **Cleanup pass:** removed unused mock response/call-counter scaffolding from the deleted shell tests, added the `ExecStdinRequest` schema snapshot, and replaced stale legacy-wire wording with the current daemon wire contract. Known cross-crate invariant: edit conflict markers duplicated with the audit side once it lands in `eos-tools` (per `conflict_markers.py` docstring). Uncommitted. |
| 2 | eos-agent-def | DONE | 2026-06-03 | `error`/`model`/`loader`/`registry`/`validation` + `tests/loader_profiles.rs`. AC-01..10 covered (18 unit + 1 integration tests). `AgentType`/`AgentRole` closed snake_case enums; `AgentName` newtype (trim + reject-empty, transparent serde/schemars); `RawAgentDefinition` serde DTO (`deny_unknown_fields`) → `from_frontmatter` invariants (`NonZeroU32`, non-empty terminals, blank-trigger strip); inlined frontmatter split (no `eos-config` edge); `_*.md` skip + `main/` contract prepend + stem/`Agent:` defaults + skill canonicalize-must-exist; `AgentRegistryBuilder`/`AgentRegistry` (`Arc`, sorted dispatchable subagents); pure recipe role-gating precheck + `skill_lint::scan_skill_file` (injected terminal keys, regex-free). AC-09: real Pydantic golden captured to `parity/schemas/agent_definition.schema.json` (+ `capture.py` provenance + parity insta snap); field-name + enum-value parity, required-set delta exactly `{role}`. Deliberate spec refinements: `from_frontmatter(raw, path)` (path needed for `MissingRole`); added `AgentDefError::EmptyName` for the §6 empty-name invariant; unused `eos-types` edge kept (frozen DAG mandates it). Gates green for this crate: `cargo fmt --check`, `clippy --all-targets -D warnings`, `test -p eos-agent-def`; `eos-parity` DAG/schema-snapshot green. Uncommitted. |
| 3 | eos-llm-client | DONE | 2026-06-03 | `error`/`types`/`message`/`events`/`auth`/`sse`/`client`/`retry`/`anthropic`/`openai` + `tests/no_legacy_surface.rs`. AC-01..11 + GC-01..05 covered (33 unit + 2 integration tests). Provider-neutral `Message`/`ContentBlock`(+`Reasoning`,`#[serde(alias="thinking")]`)/`LlmStreamEvent`(4 variants)/`StopReason`/`UsageSnapshot`/`LlmRequest`(+builder)/`ToolSpec`(+`new`,`#[non_exhaustive]`)/`ToolChoice`; one `ProviderError`{kind,status,request_id,message} struct + `ProviderErrorKind`(6 kinds); `Auth{ApiKey,Bearer}` via `secrecy::SecretString` (redacted Debug + `set_sensitive`); `LlmClient` seam (`#[async_trait]`, `Arc<dyn>`, boxed `LlmStream`). **Key design (per advisor):** SSE decode is a pure frame-stream→event-stream fn decoupled from `reqwest` (fixtures replay with no HTTP); request body serialized once, `retry_stream` factory replays owned `Bytes`; outer `Err` only for sync build, all connect/status/decode/transport errors are stream items. Retry gate on `emitted_visible` (3 delta variants), 1+`max_retries` attempts, `min(base*2^n,max)` backoff. Anthropic decode emits `tool_use` mid-stream at `content_block_stop`; usage merge (input←message_start, output←message_delta); malformed tool-args→`{}`. OpenAI `tool_use_id`←`call_id`. Encoders own all projection: Anthropic drops `Reasoning`+`output_schema`, omits `metadata`/`is_terminal`; OpenAI maps `output_schema`. **Adversarial review (4 lenses + verify + final pass): 0 critical/major defects.** Fixes applied from review: (a) `backoff` `try_from_secs_f64` (no panic on unvalidated `+inf`/huge `max_delay_s` — eos-config only rejects negatives; +regression test); (b) `request_id` stamped onto mid-stream transport errors (§8.8); (c) content-free `frame_index` in parse-error logs (§8.7); (d) AC-03 event-half (`thinking_delta`→`ReasoningDelta`) test added; (e) AC-06 openai test renamed to spec name; (f) multibyte-boundary splitter test. **Deliberate refinements (loud):** AC-05 proving test lives in `anthropic.rs` (the real JSON-parse site; `sse.rs` only frames, never parses) not `sse.rs`; empty/missing `tool_use` id is a strict `Decode` error (Python passed empty through / minted a uuid, but `ToolUseId` rejects empty and spec §6 states default-id minting "lives in eos-types/engine, not here" — never triggers since Anthropic always sends `toolu_`). Workspace deps added: `async-stream`, `secrecy="0.8"`, dev `tracing-test`. Gates green: `fmt --check`, `clippy -p eos-llm-client --all-targets -D warnings`, `test -p eos-llm-client` (35 tests), `test -p eos-parity` (DAG/SSE/schema unaffected; frozen edge set already carried `eos-llm-client→{eos-types,eos-config}`). **Deferred (loud):** the §"Load / performance gates" Criterion SSE-split+decode benchmark is NOT built — it is a cross-phase load-gate complement, not an `impl-eos-llm-client.md` §11 AC / §12 checklist item / Phase-3 formal Gate (all 11/11 ACs, 5/5 GCs, 13/13 steps complete). Wiring `benches/*.rs` (a separate crate) would force `decode_anthropic`/`decode_openai`/`SseFrameSplitter` from `pub(crate)` to `pub`, leaking internal surface against the minimal-public-API rule; the decoders are already structured as pure fixture-replayable fns, so the bench is a cheap add when the load gates are run as their cross-phase batch (alongside eos-engine/eos-db/eos-workflow/Phase-7). **Post-impl refactor pass** (review/refactor/remove-unused; audit→verify workflow + final adversarial verify, 0 defects, behavior-preserving): removed unused `pretty_assertions` dev-dep; tightened `Auth::apply`/`StopReason::parse` to `pub(crate)` (crate-internal, not §5/§6 consumer surface; stops leaking `reqwest::HeaderMap` into the public API); DRY'd the two providers' byte-identical transport plumbing into one `client::open_stream<D,R>` (decode closure over `BoxStream`) and the per-frame SSE parse preamble into `sse::parse_sse_value` (~−80/−73 lines in `anthropic.rs`/`openai.rs`; §8.7/§8.8 invariants preserved in the shared helpers); `BlockAccum`/`ToolItem` finalize via `.remove()` and dropped now-unused `Clone`/`Default`. Rejected narrowing `encode_*_body` (spec §4 mandates provider encode/decode helpers stay `pub(crate)`); kept `ProviderError` ctors (replay-client extension point), `Eq`/`Copy` derives (idiomatic per anchor §9, test-used), `DEFAULT_MAX_TOKENS`. Gates still green: 35 tests, `clippy --all-targets -D warnings`, `fmt --check`, `eos-parity`. (Impl was swept into parallel commit `7ec4783d5`; refactor uncommitted.) |
| 3 | eos-skills | DONE | 2026-06-03 | `definition`/`registry`/`bundled`/`loader`/`error` + `test_support`. AC-skills-01..11 covered (13 tests: 11 spec-named AC proofs + 2 regression guards — empty-`name`→dir-name fallback and the no-`SKILL.md` skip). `SkillName`/`ReferenceName` validated newtypes (reject empty/`..`/separator/NUL, accept dotted stems like `api.v2`); the real traversal guarantee is map-key-only usage (never path-joined), newtype rejection is defense-in-depth (GC-skills-02). `SkillSource` snake_case `#[non_exhaustive]`; `SkillDefinition` 6-field immutable value type (Serialize-only, `#[non_exhaustive]`); `SkillRegistry` over `BTreeMap` (last-wins `register` via sort-then-register, key-sorted `list_skills`). Loader: `load_from_dir`/`load_skill_registry(skill_root)` — missing root → empty registry, exists-but-non-dir → `RootNotDir` (deliberate stricter-than-Python split), Python `cwd` param dropped (GC-skills-01/03). `_parse_skill_metadata` ported faithfully incl. the **full-content** (not post-frontmatter body) scan and the `"# "`-vs-`"#"` predicate asymmetry (AC-10); broken frontmatter swallowed, never fails the load (AC-09); AC-08 serde snapshot committed. **eos-config helper added (recorded here, not in eos-config's row):** new `eos-config/src/markdown.rs` `parse_markdown_frontmatter(&str) -> (serde_yaml::Mapping, String)` (swallows malformed/non-dict YAML per Python `config/markdown.py`) — the frozen DAG mandates the `eos-skills→eos-config` edge and the skill root is passed in by `eos-runtime`, so the frontmatter split is that edge's only code use; anchor §5 "upstream owns the shared contract" makes eos-config the correct owner (5 new eos-config tests). Added `serde_yaml` dep to eos-skills to read the returned `Mapping`; `eos-types` edge kept-but-unused (frozen DAG mandates it, parity with eos-agent-def). **Deferred (loud, surgical scope):** `eos-agent-def` retains its own inlined `split_frontmatter`; consolidating it onto the new eos-config helper is future cleanup, not done here. Gates green: `cargo fmt --check`; `clippy -p eos-skills -p eos-config --all-targets -D warnings`; `test -p eos-skills` (13), `test -p eos-config` (27), `test -p eos-parity` (DAG/schema/SSE guards incl. `internal_edges_match_frozen_set` + leaf `eos-config`). **Review/cleanup pass (2026-06-03):** 4-lens adversarial review (parity/idiom/dead-code/tests), 7 findings → 3 confirmed / 4 rejected; removed unused `pretty_assertions` dev-dep; added the 2 regression guards above; no correctness defects found. The `SkillName`/`ReferenceName` macro-dedup was reviewed and **rejected as over-engineering** for two instances (`validate_name` is already the shared logic). Uncommitted. |
| 3 | eos-db | DONE | 2026-06-03 | `error`/`pool`/`json_col`/`rows`/`model_registry`/`composition`/`repositories/{request_task,workflow,iteration,attempt,agent_run}` + `migrations/0001_initial.sql` + `tests/integration.rs`. AC-eos-db-01..08 + GC-01..06 covered (13 unit + 7 integration = 20 tests). **Runtime sqlx** (`query_as::<Sqlite,Row>`/`query`, **not** the `query!` macros — no `DATABASE_URL` at build) + `migrate!()` + `FromRow` by-name; row structs hold sqlx-native primitives (`String` for ids+enums, `OffsetDateTime`, `i64`, `bool`) and the `rows.rs` mappers parse into typed DTOs (orphan rule blocks impl'ing sqlx traits on eos-types ids). UPDATEs use `… RETURNING *` + `fetch_optional`→`NotFound`; appends use `json_insert(COALESCE(col,'[]'),'$[#]',?)` (atomic, no read-modify-write); `set_status`/`set_task_status` use `COALESCE(?,col)` for the "leave unchanged on None" params; `set_deferred_goal*` ASSIGNS (None→NULL); `finish_request` is a tx with the terminal no-op; `set_task_status_if_current` is a tx (SELECT-check-then-UPDATE) distinguishing missing(`NotFound`) from mismatch(`Ok(None)`); `close_succeeded` is a single atomic UPDATE; every UPDATE bumps `updated_at` (mirrors Python ORM `onupdate`). **Outcome normalization (the highest-risk port, §6.8): two distinct normalizers in `rows.rs`** — `normalize_task_outcomes` fills a *missing* record status via `present_status` (`done`→success) but a *present* status via `_normalize_status` (`done`→FAILED), role-fallback from task role; `normalize_attempt_outcomes` never fills (missing→failed) / never role-fallbacks (missing→generator); both proven by `task_outcomes_parity`/`attempt_outcomes_parity` + a typed round-trip. `json_col` has the two decode paths (`decode_default` `x or []` coercion vs `decode_opt` null-preserving for agent_run). `0001_initial.sql` is the sole authoritative schema (final column names; the `engine.py` conditional rename/drop is dropped, not a forward migration); FK cascades enforced via per-connection `PRAGMA foreign_keys=ON` (set in `pool.rs` with WAL + busy_timeout via `SqliteConnectOptions`). `ModelRegistry` impls `ModelStore` (register tx with deactivate-all-then-`CASE` activate; get/active redacted, `active_resolved` real) + concrete `active_resolved`/`seed_from_json`; `class_path` carried verbatim, never dispatched (GC-01). `DbError` has `PostgresRejected` (defence-in-depth at the pool boundary; primary fail-fast is eos-config) + `#[from] sqlx::Error`/`MigrateError`/`io::Error` + `From<DbError> for CoreError`. **DAG kept intact:** `eos-db→{state,config}` only (eos-types primitives reached via eos-state re-exports), so `parity/tests/dependency_dag.rs` frozen set is unchanged (no edit). **Deferred (loud):** the §"Load / performance gates" concurrent-WAL Criterion benchmark is a cross-phase load gate, not an impl §11 AC / §12 step (all 8 ACs + 6 GCs done); cheap to add in the load-gate batch. Workspace dev-dep `tempfile="3"` added (file-per-test; `:memory:`+pool gives each conn its own db). **Adversarial source-accuracy review (5-dimension workflow vs the Python source, each finding refutation-verified): 5 findings, 1 confirmed (low) — `parse_dollar_var` was ASCII-only `is_ascii_alphanumeric`; widened to Unicode `is_alphanumeric` to match Python's Unicode `\w` (+`${VÄR}` regression test); the other 4 not actionable — 3 refutation-verified as deliberate documented spec decisions + 1 low representational finding whose verifier emitted no verdict (rejected on spec-grounded judgment, not workflow-verified): trait/concrete `ModelStore` split with `active_resolved`/`seed_from_json` on the concrete; `kwargs_json`-as-string per eos-state §6.7; no derived `model_id` field — not in the DTO spec; the documented empty-`TaskId` record drop, unrepresentable in the typed DTO and never written by our serializer.** **AC-01 deviation (loud):** AC-eos-db-01's `list_tasks_for_attempt` (ordered by `created_at`) clause is intentionally NOT implemented — the eos-state `TaskStore` trait roster (§6.9) excludes it and nothing calls it (Python `project_attempt_outcomes` uses per-id `get_task`, not `list_for_attempt`); **forward risk:** if eos-workflow's context/projection layer later needs per-attempt task listing, the method must be added to the eos-state `TaskStore` trait first, then implemented here. **Cleanup pass (2026-06-03):** review→adversarial-verify workflow (4 dimensions; each finding verified safe-to-apply against the spec contract + downstream consumers). Applied: (a) hoisted the byte-identical `UPDATE tasks … RETURNING *` literal shared by `set_task_status`/`set_task_status_if_current` into one file-local `const UPDATE_TASK_STATUS_SQL` (DRY, no new abstraction; both methods keep their distinct executor/CAS logic); (b) trimmed `[dev-dependencies]` to `tokio`+`tempfile` only — `eos-state`/`serde_json`/`sqlx` were redundant re-declarations of normal `[dependencies]` (integration tests see those automatically; 7 integration tests confirm); (c) refactored `seed_from_json` to call a new `register_inner` returning the native `DbError` instead of fabricating a bogus `DbError::JsonDecode(io::Error)` to flatten a `CoreError` round-trip — the trait `register` now delegates to `register_inner` and flattens once. **Rejected as no-ops/over-reach (verified):** unifying the inline `DbError::NotFound` across repos (the `attempt.rs` helper is justified by 6 uses; 1-use sites stay inline); "decode_default never None" (it is None in tests + is the documented defensive boundary). Gates re-verified green after the pass. Gates green: `cargo fmt -p eos-db --check`, `clippy -p eos-db --all-targets -D warnings`, `test -p eos-db` (20), `test -p eos-parity --test dependency_dag` (3). Uncommitted. |
| 3 | eos-sandbox-host | DONE | 2026-06-03 | `error`/`provider`/`registry`/`daemon_client`/`runtime_artifact`/`docker`/`lifecycle`/`isolated_workspace`/`provisioning` + `#[cfg(test)] testutil` mock + vendored `install_git.sh`. AC-01..11 covered (33 unit tests + AC-10 sealed `compile_fail` doctest + AC-08 compile-time `const _: () = assert!` protocol lockstep). **Recovery state machine** (daemon_client): faithful port incl. the load-bearing distinction that `CONNECT_FAILED`(97) recovery is **not** op-gated while empty-response(98 + exact `EOS_DAEMON_IO_FAILED:empty_response`) **is** (`_can_retry_empty_response` set = `{api.edit_file, api.v1.edit_file, api.write_file, api.v1.write_file, api.v1.exec_command, api.v1.exec_stdin}` + `plugin.*`); 5-send/4-sleep connect-retry; TCP single-flight = `parking_lot::RwLock` cache + per-sandbox `tokio::sync::Mutex` held across the resolve await (the one across-await lock), negative-cache on `Ok(None)` but **no** cache on resolver `Err` (Python parity); decode policy-vs-transport gate; envelope `{op,invocation_id,args}` + `api.v1.cancel` fresh id + TCP-only auth field; implements `eos_sandbox_api::SandboxTransport` via `with_daemon_protocol_version`+`call_daemon_api`. **GC-04**: eosd commands emitted unconditionally (Python launcher/compat-bridge **not** ported; `compat_python_bundle` flag noted-not-built); `runtime_bundle_sha` in the env signature uses `EOSD_VERSION` (the dropped module-bundle `bundle_hash()` has no Rust analog). **Lifecycle**: setup A–E (JoinSet overlap launched pre-`ensure_git`, drained fail-open; sequential eosd bootstrap fail-closed; `ensure_workspace_base` binding-mismatch→`build_workspace_base reset=true`; readiness gate `ready==true` literal + control_plane `ok` + `manifest_version>=1`); delete plugin-forget **dropped** (GC-03, no plugin cache); ensure_git fail-open/fail-closed split. **runtime_artifact**: the spec §6 amd64 sha (`321efbd…`) is stale, and the amd64 `eosd` binary is **actively rebuilt** by a parallel xtask (observed churn this session: `bb066eb…`→`0bf55d43…`→`033ed149…`→`5589fff8…`→`4c306b4e…`), so the const is a best-effort snapshot synced to the working-tree Python pin + the actual `sandbox/dist/eosd-linux-amd64` digest (currently `4c306b4e…`, verified rust==python==binary) and is **intentionally not unit-test-pinned** (a value-coupled test would be permanently flaky against the rebuild cadence); the pin VALUE is a Phase-7 cutover reconciliation concern (crate not yet runtime-wired, `eos-runtime` Phase 6), while the upload/verify LOGIC (arch map, sha-mismatch, marker-skip decision) is fully unit-tested. arm64 stable. Uncompressed put_archive tar (mode 0755); `chunked_upload`/module-tarball dropped (GC-01). **docker** (bollard 0.17, hyper-1.x stack): full `host_config_kwargs` (caps/seccomp/apparmor/tmpfs + privileged/no-privilege/tmpfs env hatches from `docker/client.py`), `serialize_container` (leading-`/` strip, **spec-introduced** lowercase `state`, `managed_by_app`, project_dir label-over-WorkingDir), `daemon_tcp_endpoint` (token from container **env** not label), demuxed exec, pull-on-`ImageNotFound` retry. **isolated_workspace**: DAEMON branch only (Python LOCAL `_control_plane` not ported → non-empty `SandboxId` required); enter gate `max(local,daemon)` (not sum) with kinds `ephemeral_jobs_in_flight`/`command_session_count_unavailable` (count-check fail-closed); exit `grace_s`→local drain only, evicted folded into `phases_ms`/`timings`; audit span reduced to nothing (no `eos-audit` edge). **provisioning**: `prepare_for_run` explicit-start vs `request-<8hex>`+`origin/request_id` create; Python `create_sandbox returned no id` branch **eliminated by typing** (`SandboxInfo.id` is a non-empty `SandboxId`). **Error enum**: added `InvalidRequest(String)` growth-slot variant (docker create missing image/snapshot) — `#[non_exhaustive]` justified. **DAG**: added direct `eos-types` edge + updated `parity/tests/dependency_dag.rs` frozen set to `{eos-sandbox-api, eos-config, eos-types}` (names `SandboxId`/`JsonObject`/etc. that sandbox-api does not re-export; per impl §2, mirrors the eos-plugin-catalog precedent). Added `bollard="0.17"`/`tar` to workspace deps; crate adds `bytes`+`uuid`. **Cleanup pass** (review/refactor/remove-unused, audit-workflow + 2 review lanes): removed the unused `flate2` dep (compat-bundle never built, eosd tar is uncompressed), the spec-omitted empty `MINISIGN_PUBLIC_KEY` const + re-export, sourced the `37657` magic literal from `DAEMON_TCP_INTERNAL_PORT`, and deduped the byte-identical `plain_string` into one `pub(crate)` helper; audit verified `from_client` (config-injection DI seam), `ensure_daemon_current`/`prepare_context_async` (faithful public-contract ports), `InvalidRequest`, and the load-bearing `clean_args` clone are NOT dead (kept). **Compile-only (no Docker daemon here):** the bollard container/exec calls in `docker.rs` compile + are seam-substituted by the mock in unit tests but are exercised against a real daemon only behind the `docker` feature at Phase-7 integration; everything else (registry/WR-01, envelope/recovery via mock transport, parser decode, sha verify, readiness gate, isolated gates, provisioning) is unit-tested. **Adversarial review** (5-lens workflow; lens-0 returned, 4 lanes failed to emit structured output): lens-0 caught the stale amd64 pin (fixed → synced to source-of-truth) + 2 test-coverage gaps → added `docker::tests::put_archive_fast_path` (AC-07 named, uncompressed-tar fast-path contract) and kept skip-decision coverage via `marker_indicates_skip`; **daemon_client recovery state machine reviewed clean**. Gates green: `fmt --check`, `clippy -p eos-sandbox-host --all-targets -D warnings`, `test -p eos-sandbox-host` (33+doctest), `test -p eos-parity` (dependency_dag/profiles green with the new edge). Uncommitted. |
| 3 | eos-plugin-catalog | DONE | 2026-06-03 | `error`/`names`/`frontmatter`/`manifest`/`discovery`/`tool_specs`/`audit`/`lib`. AC-01..10 covered (15 tests). `PluginName`/`PluginToolName`/`PluginResolvedPath` validated newtypes (Serialize+JsonSchema, **no** Deserialize); `resolve_under` = lexical `..`-normalize + `starts_with` (no symlink-follow; catches every `..` escape, GC-06). Two-stage parse: tolerant `pub(crate) RawManifest` whose fields are `Option<serde_yaml::Value>` (**not** the spec's `Option<String>`) → granular `MissingField`/`KindNotString` matching Python's `isinstance`-style `_require_str`/`_parse_kind` (source-fidelity tie-breaker over the spec sketch). `PluginKind` closed enum + `UnknownKind`/`KindNotString` hard errors; setup-default-`setup.sh`-iff-exists; paths validated-not-executed (GC-05). `PluginCatalog::discover_under` over `BTreeMap` (deterministic), empty-vs-`RootNotDir`; AC-06 duplicate sub-case reached via a Unix **symlink** fixture (the `name==dir` invariant makes it otherwise unreachable). 10 LSP `PluginToolSpec`s (`u32` line/char, `JsonObject` opaque payloads, `lsp.format` per source GC-07); AC-10 = normalized field/required/default + `u32`-min + name parity (not byte-equal). **Audit:** reused `eos_audit::PluginSection`/`plugin_event`/`PLUGIN_*` (eos-audit's GC-audit-06 explicitly assigned this crate the *wrapper*) — dropped the spec's own `PluginAuditSection`/`PluginCallStatus`; `audit_plugin_call` is a generic `async fn` combinator (gained an `AuditNode` param `plugin_event` requires; `error_kind` via `type_name::<E>()`; wall-clock `duration_ms` from two `Clock::now()` reads, deliberate deviation from `monotonic_now()`); `plugin_section` carries the `Custom` fallback (GC-03). **DAG:** added a direct `eos-types` edge (named `Clock`/`JsonObject` — cannot compile without it) and updated `parity/tests/dependency_dag.rs` frozen set to `{eos-types, sandbox-api, audit, config}` per impl §2 — supersedes the Phase-0 reconciliation note's three-edge claim for this crate (mirrors the eos-sandbox-host precedent already in that test); `eos-config` kept (frozen-DAG-mandated, unused in code). Gates green: `fmt --check`, `clippy -p eos-plugin-catalog --all-targets -D warnings`, `test -p eos-plugin-catalog` (16), `test -p eos-parity --test dependency_dag` (3). Workspace-wide clippy still blocked only by parallel agents' mid-edit `eos-llm-client`/`eos-skills`, unrelated to this crate. **Review/refactor pass (2026-06-03):** 5-lens multi-agent review (19 findings, 17 adversarially confirmed) → applied 7: closed 4 live-path test gaps (`KindNotString`, `MissingFrontmatter`, `Frontmatter`/invalid-yaml, setup-default-when-present), DRY'd `resolve_setup`→`resolve_optional_path` (−8 LOC), removed the unused `pretty_assertions` dev-dep + one vacuous always-true assertion. Skipped with rationale: `RawManifest` removal (spec-mandated two-stage parse; `needs_human`), `len`/`is_empty` (idiomatic, clippy-paired, near-certain eos-runtime need), `eos-config` prune (frozen-DAG-mandated), cosmetic error-identity + idiom flags (`type_name` error_kind, `as_wire`/serde dup). Uncommitted. |
| 4 | eos-tools | DONE | 2026-06-03 | `name`/`intent`/`error`/`result`/`metadata`/`ports`/`hooks`/`executor`/`registry`/`execution`/`dispatch`/`terminal`/`spec`/`meta` + `model_tools/{sandbox,isolated,submission,advisor,workflow,subagent,skills}` + `descriptions/*.md` + `#[cfg(test)] testsupport`. AC-tools-01..12 covered (24 tests). `ToolName`(24, `#[non_exhaustive]`, serde-snake==`as_str` asserted; GC-04 adds the 4 names `_names.py` omits + 2 subagent controls), `ToolIntent`(+`From`/`Into eos_sandbox_api::Intent`, GC: owned-not-aliased), `ToolError`(framework faults only), `ToolResult`+`OutputShape`(Text|Json fn-ptr validator). **Err-vs-in-band (§8.2) honored as the Rust boundary, not Python's:** framework faults = `Err(ToolError)` (unknown tool, missing port/context, store/sandbox transport, internal); tool-domain failures (bad args, hook deny, "tool said no", submission rejected) = in-band `Ok(ToolResult{is_error})`. `execute_tool_once` pipeline = background-reject → pre-hooks(first Deny short-circuits to verbatim `hook_failure` JSON+metadata shape) → execute → validate-output → stamp-terminal-on-success (the sole `TOOL_STOP` source). Per-tool typed input self-parsed in `execute` (the framework owns the generic `background`-key reject + output-shape validation); read_file auto-window done in-executor (serde can't detect omitted `end_line`). **Hooks** = sealed 6-variant `Hook` enum (GC-06), all pre-phase (post-hook stage dropped, unexercised); `DestructiveGitShell`/`DestructiveShell` faithfully ported (regex via new workspace `regex` dep — RE2-compatible patterns, no lookaround; git subcommand parser + clean dry-run/apply read-only logic verbatim; whitespace-split substitutes Python `shlex` on the best-effort, non-authoritative prehook). `RequireNoInflight`/`AdvisorApproval`/`DisallowNestedPlannerDeferral`/`BlockInIsolatedMode` read downstream state via ports/transport with fail-open vs fail-safe asymmetry + bailout-submission preserved; verbatim messages/policy/reason/count metadata. `ToolRegistry` = insertion-ordered `Vec`+`HashMap<ToolName,usize>` (deterministic `specs()`). **6 sealed `#[async_trait]` ports** (`ports.rs`, doc-hidden `Sealed`): `WorkflowControlPort`(+`is_nested_workflow` for the nested-deferral hook), `PlanSubmissionPort`(returns `SubmissionAck{Accepted|Rejected(msg)}` for the in-band reject channel; `PlannerPlan` is a richer eos-tools DTO than eos-state `PlannerSubmission` since the task rows don't exist yet — the orchestrator creates them downstream), `SubagentSupervisorPort`(+`background_inflight_count`), `AdvisorPort`(`review`+`approval_status`), `IsolatedWorkspacePort`, `NotificationSink`. `dispatch.rs` pure predicates (`reject_terminal_batch`/`lifecycle_batch_decision`) with verbatim engine messages. `terminal.rs` 6-variant `TerminalTool` totality: 4 descriptors verbatim from `_terminals/registry.py` + 2 authored (advisor/exploration) to close GC-03's fallback. **All 24 model tools** over the `tool_api`/ports; sandbox file tools build+serialize typed output DTOs; exec_command surfaces `command_session_id`, write_stdin `\x03`+running→cancel (AC-11); descriptions ported verbatim from `prompt.py` into `descriptions/*.md` (run_subagent drops the retired `wait` mention, GC-08); run_subagent spec is per-caller enum-patched (`text_spec_with_agent_enum`). **Deliberate deviations (loud):** (a) `ExecutionMetadata` carries **both** `task_store` AND `request_store` (eos-state ISP-split the Python `TaskStore.finish_request`); (b) root submission writes outcome only to `terminal_tool_result`+status, not the typed `outcomes` column (root ∉ `ExecutionRole`; anchor §4); (c) generator `terminal_tool_result` normalized `{"generator_role":"generator"}` (anchor §4 forbids the Python `"executor"` token in persisted state); (d) command-session registration + recover/mark-reported dropped to `eos-engine` (background = engine dispatch mode, anchor §3); (e) the planner's ownership/unknown-agent/DAG-cycle/persistence checks live in `PlanSubmissionPort` downstream (eos-tools has no `eos-agent-def` edge) — the tool does only the pure structural checks (AC-12: duplicate ids / missing+extra task_specs / deferred nonblank). **DAG:** added direct `eos-types` edge + updated `parity/tests/dependency_dag.rs` frozen set to `{eos-types, eos-state, eos-sandbox-api, eos-skills, eos-audit, eos-llm-client}` (names `InvocationId`/`ToolUseId`/`WorkflowSessionId`/`CommandSessionId`/`SubagentSessionId` none of the other deps re-export; mirrors sandbox-host/plugin-catalog precedent); the §5a `eos-tools→eos-llm-client` edge kept (`tools_depends_on_llm_client` green). Added `regex="1"` to workspace deps. AC-08 = crate-owned `insta` snapshot of `registry.specs()` (24 specs, insertion order) with an explicit `run_subagent` patched-enum guard. Gates green: `cargo fmt -p eos-tools --check`, `clippy -p eos-tools --all-targets -D warnings`, `test -p eos-tools` (24), `test -p eos-parity --test dependency_dag --test profiles` (3+3). schemars pulls `///` docs into output-schema `description`s (faithful, like pydantic Field); AC-08 defaults verified present in the committed snapshot (`end_line:200`/`start_line:1`/`yield_time_ms:1000`/`head_limit:250`, etc.). **Deferred (loud):** `eos-audit` is a declared dep per the frozen DAG (and the eos-sandbox-api row's "audit wrapping deferred to eos-tools" note), but the §2 tool-call audit **wrapper** is not built — no AC/GC/§12 step mandates it in Phase 4 and `ExecutionMetadata` carries no audit sink; it lands when the engine wires tool-call audit. **Review/close-out pass (2026-06-03):** adversarial parity review (4-lens workflow + advisor) drove four contract-faithful fixes — (1) **`hook_failure` shape completed**: `hook_failure_result` now also stamps `hook_trace` (accumulated *passing*-hook entries, denier excluded) + `effective_tool_input`, and `HookOutcome::Pass(JsonObject)` carries pass-phase metadata so the daemon-unavailable bailout records `reason=daemon_unavailable_bailout` (parity `_build_hook_failure_result`; spec §6.3/§6.5) — 3 new tests (`hook_trace_records_passing_hooks`, AC-02 extended for the new keys, `bailout_pass_carries_daemon_unavailable_reason`); (2) **submission verbatim**: planner (`Plan contains duplicate task id '…'.` / `task spec for '…' must be nonblank`) **and** root (`Root task '…' was not found.` / `Task '…' is not a root task.`) messages switched from Rust `{:?}` to Python `!r` single-quote rendering — AC-10/12 assert only substrings, so neither caught the divergence; the absent dependency-cycle check is **correctly downstream** (AC-12 scopes eos-tools to duplicate/missing+extra-specs/deferred-nonblank; the cycle check + ownership/unknown-agent live in `PlanSubmissionPort` per `build_planner_submission`); root's `Missing …` branches stay `Err(ToolError::MissingContext)` per the deliberate §8.2 boundary; (3) **numeric schema constraints**: `#[schemars(range)]` added to every §6.6 numeric field (read_file start/end_line ≥1, exec_command+write_stdin yield_time_ms ≤30000, timeout+max_output_tokens ≥1, last_n_messages 1..=10) so the AC-08 snapshot is schema-faithful (min/max + defaults reverified present); a parallel Phase-5 edit added the matching runtime rejections, giving full pydantic-parity (schema+runtime); (4) **byte-exact dispatch**: AC-05/06 now assert the terminal/lifecycle batch messages verbatim (were substring-only) — all three confirmed byte-identical to the Python source, as are the background-arg + output-validation wrappers. **Rejected as deliberate deviations (no churn):** `conflict_reason: Option<String>`→`null` (spec types it `Option`, not Python's failure-path `or ""`), and the single output-validation message (the `OutputShape` seam intentionally collapses Python's non-JSON-vs-mismatch split). Gates re-green: `fmt --check`, `clippy -p eos-tools --all-targets -D warnings`, `test -p eos-tools` (33, incl. Phase-5 additions), `test -p eos-parity --test dependency_dag --test profiles` (3+3). Uncommitted. |
| 5 | eos-engine | DONE | 2026-06-03 | Implemented `eos-engine` crate modules for `EngineError`, `StreamEvent` + identity stamping, `QueryContext`/`EventSource`, provider-source adaptation, prompt-report JSONL, declarative notifications + §6b notification/advisor ports, deferred tool streaming policy, post-message tool dispatch with terminal/lifecycle batch rejection, background supervisor status precedence + parent-exit handling, query request/loop hard-ceiling behavior, termination prompt/factory assembly, and stream→audit projection. Added focused AC coverage for terminal-batch rejection, defer-all with terminal tools, hard no-terminal ceiling, prompt-report ordering/no system role, notification dedup/order, background parent-exit/cancel-complete precedence, typed background handles, factory assembly, and foreground fan-in final-result delivery. **Adversarial cleanup pass:** fixed the owned-`QueryContext` API so `exit_reason`/`terminal_result` state remains caller-observable; moved hard-ceiling failure into the crossing turn (no extra reminder/provider turn after threshold); replaced enum-order-derived background precedence with explicit source precedence (`Cancelled < Failed < Completed < Delivered`) and corrected terminal-undelivered `outstanding()`; aligned `run_subagent` with the current `spawn -> subagent_session_id` port; removed unused notification scaffolding and unused `tokio-util`/`tracing` deps. **Second adversarial pass:** aligned terminal-batch rejection with the Python source by keeping rejection blocks non-terminal (`terminal_result=None`), prevented terminal-tool in-band errors from ending the loop, and replaced sequential multi-tool foreground dispatch with bounded `mpsc`/`JoinSet` fan-in plus a barrier regression test. **Residual risk:** the current background supervisor is still a status/handle tracker; full spawned subagent/workflow runner ownership, heartbeat, and `CancellationToken` cancellation require Phase-6 runtime/agent-runner wiring. Added the required direct `eos-engine -> eos-types` edge and updated the parity DAG guard; also aligned the guard with the current worktree's `eos-workflow -> eos-types` direct edge while filtering dev-deps from the production DAG. Gates green: `cargo fmt -p eos-tools -p eos-engine -p eos-parity --check`, `cargo clippy -p eos-engine --all-targets -- -D warnings`, `cargo clippy -p eos-tools --all-targets -- -D warnings`, `cargo test -p eos-engine` (14), `cargo test -p eos-tools` (32), `cargo test -p eos-parity --test dependency_dag` (3), scoped `git diff --check`. Uncommitted. |
| 5 | eos-workflow | IN REVIEW | 2026-06-03 | Implemented delegated workflow lifecycle in `eos-workflow`: `WorkflowStarter`, per-attempt `AttemptOrchestrator`, bounded `AttemptStageAdvancer`, iteration/workflow close handling, context/composer surfaces, `AgentRunner` seam, and `WorkflowControlPort`/`PlanSubmissionPort` adapters. Direct `eos-types` edge is required for `WorkflowSessionId`/typed ids not re-exported by `eos-state`; parity DAG already reflects it. GC-04 split kept: persisted DAG reachability/quiescence lives in workflow; planner-submission structural checks reject in-band before terminal success. Gates green: `cargo check -p eos-workflow`, `cargo clippy -p eos-workflow --all-targets -- -D warnings`, `cargo test -p eos-workflow` (6). Uncommitted. |
| 6 | eos-runtime | NOT STARTED | — | — |
| 7 | cutover | NOT STARTED | — | — |
