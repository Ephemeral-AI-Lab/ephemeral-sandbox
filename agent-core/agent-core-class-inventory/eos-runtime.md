# Crate `eos-runtime` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-runtime/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**14 types across 7 files.**

The `eos-runtime` crate owns the composition root of agent-core: the typed dependency graph [`AppState`] (built via [`AppStateBuilder`]) that constructs every concrete store from `eos-db` and every concrete seam implementation (LLM client, provider registry, audit sink, clock, tool/agent/skill/plugin registries), then injects them into the trait seams the engine and workflow crates depend on (DIP). It mints the root `Task(role=root, workflow_id=None)` for a top-level request and runs the root agent directly through `eos-engine` (no root workflow), provisioning one sandbox binding per request and wiring the per-request delegated-workflow runtime. Central types are [`AppState`] and its builder, the [`RequestEntryHandle`] returned by `start_request`, the shared `run_ephemeral_agent` driver (over `EphemeralRunInput`/`EphemeralRun`), the [`RuntimeAgentRunner`] `AgentRunner` adapter for delegated workflows, the [`RequestProvisioner`] sandbox seam (with `EventSourceFactory`/`EventCallback` closure aliases), `MetadataParams` for tool-context assembly, and `LogFormat` for tracing setup. It is the only crate that constructs the async runtime and depends on essentially every upstream crate (`eos-db`, `eos-engine`, `eos-workflow`, `eos-tools`, `eos-agent-def`, `eos-sandbox-host`, `eos-llm-client`, `eos-audit`, `eos-skills`, `eos-plugin-catalog`, `eos-config`, `eos-state`, `eos-types`); its `main.rs` binary and external embedders are its only consumers.

## Contents

- **`eos-runtime/src/agent_loop.rs`** — `EphemeralRunInput`, `EphemeralRun`
- **`eos-runtime/src/agent_runner.rs`** — `RuntimeAgentRunner`
- **`eos-runtime/src/app_state.rs`** — `EventSourceFactory`, `EventCallback`, `RequestProvisioner`, `HostProvisioner`, `UnconfiguredLlmClient`, `AppState`, `AppStateBuilder`
- **`eos-runtime/src/entry.rs`** — `RequestEntryHandle`
- **`eos-runtime/src/observability.rs`** — `LogFormat`
- **`eos-runtime/src/root_agent.rs`** — `RootAgentParams`
- **`eos-runtime/src/tool_context.rs`** — `MetadataParams`

---

## `eos-runtime/src/agent_loop.rs`

#### `EphemeralRunInput`  ·  _struct_  ·  pub(crate)  ·  [L25]

Inputs for `run_ephemeral_agent`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `agent` | `AgentDefinition` | `pub` |
| `initial_messages` | `Vec<Message>` | `pub` |
| `task_id` | `Option<TaskId>` | `pub` |
| `agent_run_id` | `AgentRunId` | `pub` |
| `tool_metadata` | `ExecutionMetadata` | `pub` |
| `persist_agent_run` | `bool` | `pub` |

#### `EphemeralRun`  ·  _struct_  ·  pub(crate)  ·  [L41]

The result of one ephemeral agent run, read from the loop's `QueryContext`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `terminal_result` | `Option<ToolResult>` | `pub` |
| `error` | `Option<String>` | `pub` |

---

## `eos-runtime/src/agent_runner.rs`

#### `RuntimeAgentRunner`  ·  _struct_  ·  pub(crate)  ·  [L28]

Runtime adapter over the shared engine loop, supplied to `AttemptDeps.runner`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `state` | `AppState` |  |
| `subagent_supervisor` | `Arc<dyn SubagentSupervisorPort>` |  |

**Trait impls**: `Debug, AgentRunner`

<details><summary>Methods (1)</summary>

`new`

</details>

---

## `eos-runtime/src/app_state.rs`

#### `EventSourceFactory`  ·  _type alias_  ·  = `Arc<dyn Fn(&AgentDefinition) -> Arc<dyn EventSource> + Send + Sync>`  ·  [L42]

Per-agent event-source factory seam; `None` on `AppState` uses the live provider stream, the mock harness sets it so each agent runs against a scripted source.

#### `EventCallback`  ·  _type alias_  ·  = `Arc<dyn Fn(&StreamEvent) + Send + Sync>`  ·  [L45]

Per-run stream-event callback (replaces the Python `AgentStreamEmitter`).

#### `RequestProvisioner`  ·  _trait_  ·  bases: `Send + Sync + std::fmt::Debug`  ·  async  ·  [L56]

Request-scoped sandbox provisioning seam; production wraps the host provisioner, tests inject a fake.

**Trait items**:
- `async fn prepare_for_run(&self, request_id: &RequestId, sandbox_id: Option<&str>) -> Result<RequestSandboxBinding>;`

#### `HostProvisioner`  ·  _struct_  ·  derives: `Debug`  ·  private  ·  [L69]

Production provisioner: wraps the `eos-sandbox-host` provisioner over the real container lifecycle.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `inner` | `Arc<RequestSandboxProvisioner>` |  |

**Trait impls**: `RequestProvisioner`

#### `UnconfiguredLlmClient`  ·  _struct_  ·  derives: `Debug, Default`  ·  private  ·  [L91]

Placeholder LLM client used when no provider credentials are configured and no `event_source_factory` is set; streaming always errors.

_Unit struct — no fields._

**Trait impls**: `LlmClient`

#### `AppState`  ·  _struct_  ·  derives: `Clone`  ·  #[non_exhaustive]  ·  [L106]

The composition-root dependency graph; cloning is cheap (every field is an `Arc` or `Clone`-internal handle).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `config` | `Arc<CentralConfig>` | `pub(crate)` |
| `clock` | `Arc<dyn Clock>` | `pub(crate)` |
| `cwd` | `String` | `pub(crate)` |
| `repo_root` | `String` | `pub(crate)` |
| `task_store` | `Arc<dyn TaskStore>` | `pub(crate)` |
| `request_store` | `Arc<dyn RequestStore>` | `pub(crate)` |
| `workflow_store` | `Arc<dyn WorkflowStore>` | `pub(crate)` |
| `iteration_store` | `Arc<dyn IterationStore>` | `pub(crate)` |
| `attempt_store` | `Arc<dyn AttemptStore>` | `pub(crate)` |
| `agent_run_store` | `Arc<dyn AgentRunStore>` | `pub(crate)` |
| `model_store` | `Arc<dyn ModelStore>` | `pub(crate)` |
| `llm_client` | `Arc<dyn LlmClient>` | `pub(crate)` |
| `event_source_factory` | `Option<EventSourceFactory>` | `pub(crate)` |
| `audit` | `Arc<dyn AuditSink>` | `pub(crate)` |
| `audit_shutdown` | `Arc<StdMutex<Option<BufferedAuditShutdown>>>` | `pub(crate)` |
| `tool_registry` | `Arc<ToolRegistry>` | `pub(crate)` |
| `agent_registry` | `Arc<AgentRegistry>` | `pub(crate)` |
| `skill_registry` | `Arc<SkillRegistry>` | `pub(crate)` |
| `plugin_catalog` | `Arc<PluginCatalog>` | `pub(crate)` |
| `provider_registry` | `Arc<ProviderRegistry>` | `pub(crate)` |
| `transport` | `Arc<dyn SandboxTransport>` | `pub(crate)` |
| `provisioner` | `Arc<dyn RequestProvisioner>` | `pub(crate)` |
| `advisor` | `Arc<dyn AdvisorPort>` | `pub(crate)` |
| `notifications` | `Arc<dyn NotificationSink>` | `pub(crate)` |
| `shutdown` | `CancellationToken` | `pub(crate)` |

**Trait impls**: `Debug`

<details><summary>Methods (7)</summary>

`builder`, `shutdown_token`, `config`, `clock`, `plugin_catalog`, `provider_registry`, `flush_audit`

</details>

#### `AppStateBuilder`  ·  _struct_  ·  derives: `Default`  ·  #[must_use]  ·  [L197]

`#[must_use]` builder for `AppState`; every field is an optional override where `None` selects the production default.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `config` | `Option<CentralConfig>` |  |
| `database_url` | `Option<String>` |  |
| `clock` | `Option<Arc<dyn Clock>>` |  |
| `cwd` | `Option<String>` |  |
| `llm_client` | `Option<Arc<dyn LlmClient>>` |  |
| `event_source_factory` | `Option<EventSourceFactory>` |  |
| `audit` | `Option<Arc<dyn AuditSink>>` |  |
| `audit_path` | `Option<PathBuf>` |  |
| `agent_registry` | `Option<Arc<AgentRegistry>>` |  |
| `agents_dir` | `Option<PathBuf>` |  |
| `skill_registry` | `Option<Arc<SkillRegistry>>` |  |
| `skill_root` | `Option<PathBuf>` |  |
| `plugin_catalog` | `Option<Arc<PluginCatalog>>` |  |
| `plugin_root` | `Option<PathBuf>` |  |
| `model_registry_path` | `Option<PathBuf>` |  |
| `provisioner` | `Option<Arc<dyn RequestProvisioner>>` |  |
| `transport` | `Option<Arc<dyn SandboxTransport>>` |  |
| `compatibility_mode` | `bool` |  |

**Trait impls**: `Debug`

<details><summary>Methods (19)</summary>

`config`, `database_url`, `clock`, `cwd`, `llm_client`, `event_source_factory`, `audit`, `audit_path`, `agent_registry`, `agents_dir`, `skill_registry`, `skill_root`, `plugin_catalog`, `plugin_root`, `model_registry_path`, `provisioner`, `transport`, `compatibility_mode`, `build`

</details>

---

## `eos-runtime/src/entry.rs`

#### `RequestEntryHandle`  ·  _struct_  ·  #[non_exhaustive]  ·  [L31]

Handle to a started request: the minted ids, the per-request workflow dependency bundle, and the spawned root-agent task.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `request_id` | `RequestId` | `pub` |
| `root_task_id` | `TaskId` | `pub` |
| `attempt_deps` | `AttemptDeps` | `pub` |
| `root_agent_task` | `JoinHandle<()>` | `pub(crate)` |
| `supervisor` | `Arc<SharedSubagentSupervisor>` | `pub(crate)` |
| `state` | `AppState` | `pub(crate)` |

**Trait impls**: `Debug`

<details><summary>Methods (2)</summary>

`join`, `shutdown`

</details>

---

## `eos-runtime/src/observability.rs`

#### `LogFormat`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Default`  ·  [L12]

Output format for the text/JSON subscriber.

**Variants**: `Text` (`#[default]`), `Json`

---

## `eos-runtime/src/root_agent.rs`

#### `RootAgentParams`  ·  _struct_  ·  pub(crate)  ·  [L20]

Everything one root-agent run needs beyond the shared `AppState`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `request_id` | `RequestId` | `pub` |
| `root_task_id` | `TaskId` | `pub` |
| `prompt` | `String` | `pub` |
| `sandbox_id` | `SandboxId` | `pub` |
| `workflow_control` | `Arc<dyn WorkflowControlPort>` | `pub` |
| `subagent_supervisor` | `Arc<dyn SubagentSupervisorPort>` | `pub` |
| `on_event` | `Option<EventCallback>` | `pub` |

---

## `eos-runtime/src/tool_context.rs`

#### `MetadataParams`  ·  _struct_  ·  pub(crate)  ·  [L13]

The per-run identifiers and ports that distinguish one agent's tool context.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `agent_name` | `String` | `pub` |
| `sandbox_id` | `Option<SandboxId>` | `pub` |
| `agent_run_id` | `AgentRunId` | `pub` |
| `request_id` | `Option<RequestId>` | `pub` |
| `task_id` | `Option<TaskId>` | `pub` |
| `attempt_id` | `Option<AttemptId>` | `pub` |
| `workflow_id` | `Option<WorkflowId>` | `pub` |
| `workflow_control` | `Option<Arc<dyn WorkflowControlPort>>` | `pub` |
| `subagent_supervisor` | `Option<Arc<dyn SubagentSupervisorPort>>` | `pub` |
