# Runtime Services and Tool Metadata Split Spec

Status: Implemented
Date: 2026-06-06

## Problem

`agent-core/crates/eos-runtime/src/app_state.rs` currently acts as a mixed
composition bag. It holds static configuration, persisted stores, engine
dependencies, sandbox provisioning, audit state, request workspace facts, and
tool-service dependencies in one type.

`agent-core/crates/eos-tools/src/core/metadata.rs` also mixes two concepts:
per-tool-call facts and service dependencies. That makes `ExecutionMetadata`
work like a service locator instead of a small immutable record of the current
agent/tool invocation.

The target design is:

- Runtime composition lives in explicitly named runtime service files.
- Request/workspace data stays request-scoped and is not stored in long-lived
  runtime services.
- `ExecutionMetadata` is flat and contains facts only.
- Tool services are wired directly into the tool implementations that need
  them.
- Isolated workspace state is represented as a per-call boolean fact, not a
  service port carried by metadata.

## Goals

- Replace `AppState` with a clearer composition type named `RuntimeServices`.
- Split runtime dependencies into:
  - `DbStoreService`
  - `AgentCoreRegistryService`
  - `EngineService`
  - `SandboxService`
  - `AuditService`
- Remove `cwd`, `repo_root`, and workflow/attempt config from long-lived
  runtime services.
- Use one request workspace fact: `workspace_root`.
- Remove `isolated_workspace` from runtime state and tool metadata.
- Add `is_isolated_workspace_mode: bool` to tool metadata.
- Keep `sandbox_invocation_id` as a per-daemon-call correlation fact.
- Wire service dependencies into local tool executors or registration functions.
- Move sandbox provisioning ownership out of `eos-runtime` and into the sandbox
  host boundary.

## Non-Goals

- No daemon protocol redesign.
- No persisted database schema redesign.
- No synthetic root workflow or global agent orchestrator.
- No peer-to-peer agent communication.
- No compatibility alias that preserves `AppState` long term.
- No `cwd` / `repo_root` pair under another name.
- No global `ToolServices` bag that recreates the current metadata problem.

## Current Shape

### Runtime State

Current file:

```text
agent-core/crates/eos-runtime/src/app_state.rs
```

Current `AppState` field groups:

| Group | Current fields | Problem |
| --- | --- | --- |
| Request/workspace facts | `cwd`, `repo_root` | Long-lived service state should not store request workspace data. |
| Workflow config | `workflow` | Config belongs in request/run input or config loading, not runtime service identity. |
| Stores | `task_store`, `request_store`, `workflow_store`, `iteration_store`, `attempt_store`, `agent_run_store`, `model_store` | These are legitimate runtime dependencies, but should be grouped under DB/store ownership. |
| Engine/provider | `llm_client`, `event_source_factory` | Legitimate engine dependencies, but not DB or sandbox dependencies. |
| Audit | `audit`, `audit_shutdown` | Legitimate audit dependency group. |
| Registries/catalogs | `tool_config`, `agent_registry`, `skill_registry` | Runtime registries should be grouped explicitly. |
| Sandbox | `transport`, `isolated_workspace`, `provisioner` | `transport` and provisioning are sandbox-host dependencies; `isolated_workspace` should be removed. |

### Tool Metadata

Current file:

```text
agent-core/crates/eos-tools/src/core/metadata.rs
```

Current `ExecutionMetadata` field groups:

| Group | Current fields | Target |
| --- | --- | --- |
| Agent/tool facts | `agent_run_id`, `agent_name`, `request_id`, `task_id`, `attempt_id`, `workflow_id`, `tool_use_id`, `sandbox_invocation_id`, `conversation` | Keep as flat metadata facts. |
| Workspace facts | `cwd`, `repo_root`, `exec_cwd`, `sandbox_id` | Replace workspace path fields with `workspace_root`; keep `sandbox_id`. |
| Sandbox services | `transport`, `isolated_workspace` | Remove from metadata. Wire into sandbox/file/command/isolated tool executors. |
| Stores | `task_store`, `request_store` | Remove from metadata. Wire through narrow submission services. |
| Registries | `skill_registry` | Remove from metadata. Wire into skill tool executor. |
| Workflow services | `workflow_control`, `plan_submission` | Remove from metadata. Wire into workflow and terminal submission tools. |
| Background/session services | `background_supervisor`, `command_session_supervisor` | Remove from metadata. Wire only into tools that need them. |
| Notifications | `notifications` | Remove from metadata. Wire into engine/tool execution where the tool actually emits notifications. |

## Target File and Folder Structure

```text
agent-core/crates/eos-runtime/src/
  lib.rs
  entry.rs
  root_agent.rs
  request_input.rs
  runtime_services/
    mod.rs
    builder.rs
    db_store.rs
    agent_core_registry.rs
    engine.rs
    sandbox.rs
    audit.rs
    tool_registry.rs

agent-core/crates/eos-tools/src/
  core/
    metadata.rs
  model_tools/
    root_submission.rs
    attempt_submission.rs
    workflow.rs
    isolated.rs
  terminal/
    command.rs
  skills/
    tool.rs

agent-core/crates/eos-engine/src/
  runtime/
    agent_loop.rs
  tool_call/
    dispatch.rs

agent-core/crates/eos-sandbox-host/src/
  provisioning.rs
  workspace_mode.rs
```

### Ownership Notes

| File | Owns |
| --- | --- |
| `runtime_services/mod.rs` | Public runtime composition surface and re-exports. |
| `runtime_services/builder.rs` | Config loading and construction of the service graph. |
| `runtime_services/db_store.rs` | Store dependency group. |
| `runtime_services/agent_core_registry.rs` | Agent, skill, and tool registry/config group. |
| `runtime_services/engine.rs` | LLM/provider/event-source engine dependencies. |
| `runtime_services/sandbox.rs` | Sandbox transport and request provisioning dependency group. |
| `runtime_services/audit.rs` | Audit sink and shutdown lifecycle. |
| `runtime_services/tool_registry.rs` | Per-agent/per-request tool registry construction. |
| `request_input.rs` | Request-scoped values such as request id, prompt, sandbox id, workspace root, and workflow config. |
| `eos-tools/core/metadata.rs` | Flat per-tool-call facts only. |
| `eos-sandbox-host/provisioning.rs` | `RequestProvisioner` trait and host-side provisioning implementation. |
| `eos-sandbox-host/workspace_mode.rs` | Host helper for reading isolated workspace mode by active sandbox caller. |

## Target Runtime Types

### RuntimeServices

```rust
pub struct RuntimeServices {
    pub(crate) db: DbStoreService,
    pub(crate) agent_core: AgentCoreRegistryService,
    pub(crate) engine: EngineService,
    pub(crate) sandbox: SandboxService,
    pub(crate) audit: AuditService,
}
```

Rules:

- This replaces `AppState`.
- Fields stay `pub(crate)` unless another crate genuinely needs them.
- Construction goes through `RuntimeServicesBuilder`.
- Request-scoped values are not stored here.

### DbStoreService

```rust
pub struct DbStoreService {
    pub(crate) task_store: Arc<dyn TaskStore>,
    pub(crate) request_store: Arc<dyn RequestStore>,
    pub(crate) workflow_store: Arc<dyn WorkflowStore>,
    pub(crate) iteration_store: Arc<dyn IterationStore>,
    pub(crate) attempt_store: Arc<dyn AttemptStore>,
    pub(crate) agent_run_store: Arc<dyn AgentRunStore>,
    pub(crate) model_store: Arc<dyn ModelStore>,
}
```

Rules:

- Owns persisted state access only.
- Does not expose stores through tool metadata.
- Narrow services are created from this group at the local registration site
  when a tool needs state mutation.

### AgentCoreRegistryService

```rust
pub struct AgentCoreRegistryService {
    pub(crate) agent_registry: Arc<AgentRegistry>,
    pub(crate) skill_registry: Arc<SkillRegistry>,
    pub(crate) tool_config: Arc<ToolConfigSet>,
}
```

Rules:

- Owns runtime registries and model-facing tool config.
- `skill_registry` is injected into skill tool executors, not metadata.
- Tool registry construction uses this group plus local service inputs.

### EngineService

```rust
pub struct EngineService {
    pub(crate) llm_client: Arc<dyn LlmClient>,
    pub(crate) event_source_factory: Option<EventSourceFactory>,
}
```

Rules:

- Owns provider/stream dependencies used by the engine loop.
- Does not own stores or workspace paths.
- Engine run handles are assembled from `DbStoreService`,
  `AgentCoreRegistryService`, `EngineService`, and `AuditService`.

### SandboxService

```rust
pub struct SandboxService {
    pub(crate) transport: Arc<dyn SandboxTransport>,
    pub(crate) provisioner: Arc<dyn RequestProvisioner>,
}
```

Rules:

- Owns host-side sandbox access.
- `RequestProvisioner` is defined in `eos-sandbox-host`, not `eos-runtime`.
- There is no `isolated_workspace` field.
- Isolated workspace lifecycle tools use `transport` directly through their
  local executor service.

### AuditService

```rust
pub struct AuditService {
    pub(crate) sink: Arc<dyn AuditSink>,
    pub(crate) shutdown: Arc<StdMutex<Option<BufferedAuditShutdown>>>,
}
```

Rules:

- Owns audit write and shutdown lifecycle.
- Audit access is passed to engine/tool execution explicitly where needed.

## Target Request Input

```rust
pub struct RequestRunInput {
    pub request_id: RequestId,
    pub prompt: String,
    pub sandbox_id: SandboxId,
    pub workspace_root: String,
    pub workflow_config: WorkflowConfig,
}
```

Rules:

- This is request-scoped input, not part of `RuntimeServices`.
- `workspace_root` is the only workspace path carried through the runtime
  request path.
- Do not reintroduce `cwd`, `repo_root`, or `exec_cwd` as separate fields unless
  a future feature proves a real distinct semantic.

## Target Tool Metadata

```rust
pub struct ExecutionMetadata {
    pub agent_name: String,
    pub agent_run_id: Option<AgentRunId>,
    pub request_id: Option<RequestId>,
    pub task_id: Option<TaskId>,
    pub attempt_id: Option<AttemptId>,
    pub workflow_id: Option<WorkflowId>,
    pub tool_use_id: Option<ToolUseId>,
    pub sandbox_invocation_id: Option<InvocationId>,
    pub sandbox_id: Option<SandboxId>,
    pub is_isolated_workspace_mode: bool,
    pub workspace_root: String,
    pub conversation: Arc<[Message]>,
}
```

Rules:

- Metadata stays flat.
- Metadata contains facts only.
- No `Arc<dyn ...>` service dependencies are allowed in metadata.
- `sandbox_invocation_id` remains distinct from `tool_use_id`.
- `is_isolated_workspace_mode` is read from sandbox host/daemon state for the
  active sandbox caller before each tool dispatch.
- `conversation` remains metadata because it is an immutable per-call input fact.

## Local Tool Service Wiring

Tool services are registered at the tool family boundary that needs them.

| Tool family | Local service/dependency | Notes |
| --- | --- | --- |
| `submit_root_outcome` | `RootSubmissionService` | Replaces raw `task_store` and `request_store` metadata access. |
| planner/generator/reducer terminal tools | `AttemptSubmissionService` | Renamed from conceptual `plan_submission`; owns attempt outcome submission behavior. |
| workflow tools | `WorkflowControlPort` | Used by `delegate_workflow`, `check_workflow_status`, and `cancel_workflow`. |
| subagent/background tools | `BackgroundSupervisorPort` | Only registered for tools that launch or inspect background agent work. |
| command session tools | `SandboxTransport`, `CommandSessionSupervisorPort` | Command sessions remain separate from generic background supervision. |
| file/shell/search/plugin sandbox tools | `SandboxTransport` | Use `workspace_root`, `sandbox_id`, and `sandbox_invocation_id` from metadata. |
| skill tools | `SkillRegistry` | Registry is captured by the skill tool executor. |
| isolated workspace tools | `SandboxTransport` | No `IsolatedWorkspacePort`; lifecycle calls route through daemon/host transport. |
| notification-emitting tools | `NotificationSink` | Registered only where a tool emits notifications. |

### Service Types

```rust
pub struct RootSubmissionService {
    task_store: Arc<dyn TaskStore>,
    request_store: Arc<dyn RequestStore>,
}

pub struct AttemptSubmissionService {
    workflow_store: Arc<dyn WorkflowStore>,
    iteration_store: Arc<dyn IterationStore>,
    attempt_store: Arc<dyn AttemptStore>,
    task_store: Arc<dyn TaskStore>,
}

pub struct SandboxToolService {
    transport: Arc<dyn SandboxTransport>,
}

pub struct CommandToolService {
    transport: Arc<dyn SandboxTransport>,
    command_session_supervisor: Arc<dyn CommandSessionSupervisorPort>,
}

pub struct SkillToolService {
    skill_registry: Arc<SkillRegistry>,
}
```

Rules:

- These services are owned by the tool implementation or registration module
  that uses them.
- They are not bundled into a global service bag.
- Prefer concrete structs for closed local dependency sets.
- Use trait objects only at real runtime provider boundaries, such as stores,
  transports, supervisors, and provider clients.

## Tool Dispatch Flow

```text
RequestRunInput
  |
  v
RuntimeServices
  |
  |-- build EngineRunHandles from db + engine + agent_core + audit
  |
  |-- build per-run ToolRegistry
        |
        |-- register root terminal tools with RootSubmissionService
        |-- register attempt terminal tools with AttemptSubmissionService
        |-- register workflow tools with WorkflowControlPort
        |-- register command tools with CommandToolService
        |-- register sandbox tools with SandboxToolService
        |-- register skill tools with SkillToolService
        |-- register isolated tools with SandboxToolService
  |
  v
Engine loop
  |
  |-- refresh is_isolated_workspace_mode for active sandbox caller
  |-- create flat ExecutionMetadata
  |-- dispatch tool executor with metadata facts + executor-owned services
  |
  v
Tool result
```

## Implementation Plan

### Phase 1: Add Runtime Service Modules

- Add `runtime_services/` files.
- Rename `AppState` to `RuntimeServices`.
- Move field groups into the target service structs.
- Keep construction behavior equivalent.
- Keep fields `pub(crate)` and expose methods only for real downstream use.

### Phase 2: Separate Request Input

- Add `RequestRunInput`.
- Move request prompt, request id, sandbox id, `workspace_root`, and
  `workflow_config` into request-scoped input.
- Remove `cwd`, `repo_root`, and workflow config from `RuntimeServices`.
- Update root-agent entry paths to pass request input explicitly.

### Phase 3: Move Sandbox Provisioning Ownership

- Move `RequestProvisioner` definition and implementation ownership to
  `eos-sandbox-host`.
- Keep `SandboxService` as the runtime handle to host-side sandbox dependencies.
- Remove `isolated_workspace` from sandbox runtime service state.

### Phase 4: Build Local Tool Services

- Add `RootSubmissionService`.
- Rename and wire `AttemptSubmissionService`.
- Add local command, sandbox, skill, and isolated tool service structs where
  the existing executors need shared dependencies.
- Register dependencies at tool family registration sites.

### Phase 5: Slim ExecutionMetadata

- Remove all service fields from `ExecutionMetadata`.
- Replace `cwd`, `repo_root`, and `exec_cwd` with `workspace_root`.
- Add `is_isolated_workspace_mode`.
- Keep `sandbox_invocation_id`.
- Update tool implementations to use executor-owned services plus metadata
  facts.

### Phase 6: Refresh Isolated Workspace Mode Per Dispatch

- Add host/daemon query path for the active sandbox caller if one does not
  already exist.
- Refresh `is_isolated_workspace_mode` immediately before tool execution.
- Use the boolean for model-tool policy checks and audit context.
- Keep lifecycle mutation in `enter_isolated_workspace` /
  `exit_isolated_workspace` tools.

### Phase 7: Verification and Documentation

- Run scoped Cargo checks and targeted tests.
- Update affected architecture docs under `docs/architecture/tools` and
  `docs/architecture/sandbox` when implementation lands.
- Remove stale references to `AppState`, `isolated_workspace` metadata, `cwd`,
  `repo_root`, and `exec_cwd`.

## Progress Tracker

| Phase | Status | Evidence |
| --- | --- | --- |
| Spec drafted | Done | This file. |
| Runtime service modules added | Done | `agent-core/crates/eos-runtime/src/runtime_services/{mod,builder,db_store,agent_core_registry,engine,sandbox,audit}.rs`. |
| `AppState` removed or renamed to `RuntimeServices` | Done | `agent-core/crates/eos-runtime/src/app_state.rs` deleted; `RuntimeServices` exported from `runtime_services/mod.rs`. |
| Request-scoped `RequestRunInput` added | Done | `agent-core/crates/eos-runtime/src/request_input.rs`; `run_request` now receives `RequestRunInput`. |
| `workspace_root` replaces `cwd` / `repo_root` / `exec_cwd` | Done | `ExecutionMetadata.workspace_root`; runtime services no longer store request workspace paths. |
| `RequestProvisioner` moved to sandbox host | Done | `agent-core/crates/eos-sandbox-host/src/provisioning.rs` owns `RequestProvisioner`; runtime `SandboxService` stores `Arc<dyn RequestProvisioner>`. |
| `isolated_workspace` service field removed | Done | `agent-core/crates/eos-runtime/src/isolated_workspace.rs` deleted; lifecycle tools call daemon via `SandboxToolService`. |
| `is_isolated_workspace_mode` metadata fact added | Done | `ExecutionMetadata.is_isolated_workspace_mode`; `eos-engine/src/tool_call/dispatch.rs` refreshes it via `eos_sandbox_api::isolated_active`. |
| Tool services locally wired | Done | `agent-core/crates/eos-tools/src/tools/services.rs` plus family registration in `tools/mod.rs`. |
| `ExecutionMetadata` contains facts only | Done | `agent-core/crates/eos-tools/src/core/metadata.rs`. |
| Scoped Cargo verification complete | Done | `cargo check -p eos-tools --all-targets`, `cargo check -p eos-engine --all-targets`, `cargo check -p eos-runtime --all-targets`, `cargo check -p eos-sandbox-host --all-targets`, `cargo check --workspace --all-targets`, `cargo test -p eos-tools --all-targets`, `cargo test -p eos-engine --all-targets`, `cargo test -p eos-runtime --all-targets`, `cargo test -p eos-sandbox-host --all-targets`, and `cargo clippy --no-deps -p eos-runtime -p eos-tools -p eos-engine -p eos-sandbox-host --all-targets -- -D warnings` pass. |
| Architecture docs refreshed | Done | `docs/architecture/tools/index.html`, `docs/architecture/tools/isolated-workspace.html`. |

## Acceptance Criteria

- `agent-core/crates/eos-runtime/src/app_state.rs` no longer exists, or it is
  reduced to a temporary deleted-in-follow-up compatibility shim with no new
  logic.
- No production type named `AppState` remains in `eos-runtime`.
- `RuntimeServices` exists and contains only:
  - `DbStoreService`
  - `AgentCoreRegistryService`
  - `EngineService`
  - `SandboxService`
  - `AuditService`
- `RuntimeServices` does not contain `cwd`, `repo_root`, `workflow_config`, or
  `attempt_config`.
- Request/root-agent execution receives `workspace_root` through request-scoped
  input.
- `ExecutionMetadata` has no service dependencies such as stores, transports,
  registries, workflow ports, background supervisors, command supervisors,
  notification sinks, or isolated workspace ports.
- `ExecutionMetadata` has `workspace_root` and does not have `cwd`, `repo_root`,
  or `exec_cwd`.
- `ExecutionMetadata` has `is_isolated_workspace_mode: bool`.
- `ExecutionMetadata` keeps `sandbox_invocation_id` as a daemon-call
  correlation fact.
- `isolated_workspace` is not carried by runtime services or tool metadata.
- `RequestProvisioner` is owned by `eos-sandbox-host`.
- Tool executors receive dependencies through local constructor/registration
  wiring.
- No global `ToolServices` bag is introduced.
- Command session supervision remains separate from generic background
  supervision.
- Root submission no longer reads `task_store` / `request_store` from metadata.
- Attempt terminal submission no longer uses a `plan_submission` name.
- Skill tools receive `SkillRegistry` directly from their executor/service.
- Sandbox/file/command/plugin tools receive `SandboxTransport` directly from
  their executor/service.
- Isolated workspace lifecycle tools call sandbox host/daemon through transport
  and use `is_isolated_workspace_mode` as a metadata fact.

## Verification Ladder

Run from `agent-core/` unless a command states otherwise:

```bash
cargo check -p eos-runtime --all-targets
cargo check -p eos-tools --all-targets
cargo check -p eos-engine --all-targets
cargo test -p eos-runtime --all-targets
cargo test -p eos-tools --all-targets
cargo test -p eos-engine --all-targets
```

If the implementation changes sandbox host/protocol ownership:

```bash
cargo check -p eos-sandbox-host --all-targets
```

If the implementation crosses the agent-core workspace dependency graph:

```bash
cargo check --workspace --all-targets
```

Report any pre-existing unrelated failures separately from failures caused by
this refactor.
