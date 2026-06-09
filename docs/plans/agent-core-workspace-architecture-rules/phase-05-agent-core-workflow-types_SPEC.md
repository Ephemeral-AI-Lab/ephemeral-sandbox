# Phase 05 - Agent Core, Workflow, and Types Spec

Status: Draft
Date: 2026-06-09
Owner: eos-agent-core / eos-workflow / eos-types

## Scope

This phase cleans the remaining ownership vocabulary after the tool, engine, and
run boundaries are stable.

It creates `eos-agent-core` as the external Rust facade, folds the old
`eos-runtime` request-running object graph into private `runtime/` modules,
folds agent definitions, runtime config, and audit wiring into their owners,
keeps workflow domain logic in `eos-workflow`, and reduces `eos-types` to
passive contracts.

`router` is explicitly out of scope. HTTP routing belongs in `backend-server`,
not in `agent-core`.

## Local Architecture

### eos-agent-core

`eos-agent-core` is the external-project boundary. Backend-server or another
Rust project depends on this crate when it wants to run, inspect, or cancel
agent-core behavior.

It exposes a narrow facade such as:

```rust
pub struct AgentCore;

impl AgentCore {
    pub async fn run_request(&self, request: RunAgentRequest) -> Result<RunAgentOutcome>;
    pub async fn cancel_request(&self, request_id: &RequestId) -> Result<CancelReport>;
    pub async fn read_state(&self, query: StateQuery) -> Result<StateSnapshot>;
}
```

Private `runtime/` modules build stores, engine handles, sandbox handles,
agent definitions, audit sinks, plugin wiring, and request-scoped run input.
They are not named `service`, `composition`, or `deps`.

### eos-workflow

`eos-workflow` owns workflow lifecycle state transitions and attempt/iteration
domain rules. Because siblings call those behaviors, it has `services.rs`.
There is no first-target `services/` folder; lifecycle, attempt, and query
helpers stay in owner-named files until a split clearly earns its size.

### eos-types

`eos-types` owns passive typed IDs, DTOs, state enums, JSON helpers, and store
traits. It must not own services, runtime logic, provider logic, or tool logic.

## Resulting File Structure

```text
agent-core/crates/eos-agent-core/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── error.rs
│   ├── model.rs
│   ├── agent_core.rs
│   ├── request.rs
│   ├── state.rs
│   ├── cancellation.rs
│   ├── agents.rs
│   ├── runtime.rs
│   └── runtime/
│       ├── builder.rs
│       ├── database.rs
│       ├── engine.rs
│       ├── sandbox.rs
│       ├── audit.rs
│       └── plugins.rs
└── tests/
    └── facade/
```

```text
agent-core/crates/eos-workflow/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── error.rs
│   ├── model.rs
│   ├── services.rs
│   ├── attempts.rs
│   ├── run_stage.rs
│   ├── iterations.rs
│   ├── planning.rs
│   └── context.rs
└── tests/
    └── workflow/
```

The current `attempt/` tree is 2414 LOC (`orchestrator.rs` 653, `run_stage.rs`
611, `launch.rs` 540, `plan_dag.rs` 506). Do **not** collapse it into a single
`attempts.rs`; that would be a ~1900 LOC god-file (Phase 6, "Cohesion outranks
file count"). Split it by ownership boundary instead — attempt lifecycle in
`attempts.rs`, stage orchestration in `run_stage.rs`, the plan DAG in
`planning.rs` — which still lands well under the `<= 10` module budget.

```text
agent-core/crates/eos-types/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── error.rs
│   ├── ids.rs
│   ├── json.rs
│   ├── time.rs
│   ├── state.rs
│   ├── stores.rs
│   └── dto.rs
└── tests/
    └── contracts/
```

## Naming Rules

Do not create an `eos-agent-api` crate. Do not create an agent-core router crate.

| Concept | Target name |
| --- | --- |
| external Rust facade crate | `eos-agent-core` |
| public facade type | `AgentCore` |
| hidden request-running object graph | `runtime.rs`, `runtime/` |
| concrete resource groups | `DatabaseHandles`, `EngineHandles`, `SandboxHandles` |
| agent definitions | `agents.rs` |
| loaded plugin definitions | `PluginCatalog` only if runtime-loaded definitions need that name |
| audit writer | `AuditSink` |
| HTTP/path routing | backend-server router, outside `agent-core` |

Forbidden in target code:

```text
eos-agent-api
eos-agent-def
eos-config
eos-audit
router.rs inside agent-core
composition
deps
runtime_services
```

## Workflow Rules

- `WorkflowService` is allowed if consumed by sibling crates.
- `eos-workflow` uses a flat `services.rs` first; no `services/` folder in the
  first target.
- Workflow state mutation stays in `eos-workflow`.
- Tool-facing workflow registration helpers live in `eos-tool`.
- Engine hooks reach workflow behavior **only** through the injected workflow
  handle (trait defined in `eos-tool`, captured in `ToolRuntime`); they invoke
  it but must not own workflow state transitions. `eos-engine` has no crate
  dependency on `eos-workflow` — adding one would close a cycle
  (`eos-engine -> eos-workflow -> eos-agent-run -> eos-engine`).
- `eos-agent-core` is the composition root: it depends on both `eos-tool` and
  `eos-workflow`, builds the concrete workflow handle impl, and injects it into
  the engine's `AgentLoopExecutionRequest`. It does not implement workflow
  lifecycle rules.

## Types Rules

`eos-types` may contain:

- typed IDs,
- passive DTOs,
- state enums,
- serde contracts,
- store traits,
- JSON/time helpers.

`eos-types` must not contain:

- `service.rs`,
- `services/`,
- `api.rs`,
- generic config schemas that belong to behavior owners,
- agent definition loading,
- audit sink behavior,
- provider clients,
- request runtime wiring,
- DB implementations,
- tool behavior,
- workflow lifecycle logic.

## Progress Tracker

| Item | Status |
| --- | --- |
| Create `eos-agent-core` facade | Not started |
| Fold `eos-runtime` into private `runtime/` modules | Not started |
| Fold `eos-agent-def` into `eos-agent-core/src/agents.rs` | Not started |
| Fold `eos-config` into owner-local config structs | Not started |
| Fold `eos-audit` into `eos-agent-core/src/runtime/audit.rs` | Not started |
| Rename runtime-local `*Service` types to canonical `Runtime` / `Handles` / `Context` / `Client` / `Records` names | Not started |
| Move external DTOs into `eos-agent-core` or `eos-types` | Not started |
| Keep workflow services sibling-facing only | Not started |
| Remove non-passive logic from `eos-types` | Not started |
| Remove `router` vocabulary from agent-core | Not started |
| Update backend/external import path documentation | Not started |
| Update workspace guard allowlists | Not started |
| Update `index.md` Progress Tracker with Phase 05 result and exit artifact | Not started |

## Acceptance Criteria

- No crate named `eos-agent-api` remains.
- No crate named `eos-runtime` remains.
- No crate named `eos-agent-def`, `eos-config`, or `eos-audit` remains.
- `eos-agent-core` exposes a narrow external Rust facade and hides internal
  runtime wiring.
- `router` appears only in external server docs or backend-server code, not in
  `agent-core` source.
- `composition`, `deps`, and `runtime_services` are absent from target module
  and type names.
- `eos-workflow` exposes only workflow-domain services consumed by siblings and
  has no first-target `services/` folder.
- `eos-types` contains no service, runtime, provider, DB implementation, or tool
  behavior.
- `cargo check -p eos-agent-core --all-targets` passes.
- `cargo check -p eos-workflow --all-targets` passes.
- `cargo check -p eos-types --all-targets` passes.
