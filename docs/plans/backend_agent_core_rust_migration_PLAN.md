# Backend to Agent-Core Rust Migration Plan

Date: 2026-06-02

Target: migrate the Python control plane under `backend/src` into a Rust
`agent-core` workspace.

## Verdict

Rust is a good fit for this backend because the current system is mostly a
typed control plane: task/workflow state machines, tool dispatch, provider
stream normalization, sandbox RPC boundaries, and persistence. The low reliance
on Python-only third party libraries helps. The migration risk is not library
availability; it is preserving the current semantics around terminal tools,
workflow attempts, context packets, background sessions, and sandbox host
protocols.

The migration should be staged. Do not rewrite the sandbox daemon internals,
test runner, or coding-plan clients as part of this agent-core move.

## Scope

In scope:

- `backend/src/task`
- `backend/src/workflow`
- `backend/src/runtime`
- `backend/src/engine`
- `backend/src/agents`
- `backend/src/tools`
- `backend/src/message`
- `backend/src/providers`, excluding `providers/clients/coding_plan`
- `backend/src/audit`
- `backend/src/config`
- `backend/src/db`
- host-facing `backend/src/sandbox` APIs, provider adapters, lifecycle, and
  daemon transport
- `backend/src/plugins` manifest/catalog boundaries, not deep plugin runtimes
- `backend/src/skills`
- `backend/src/notification`
- `backend/src/prompt`

Out of scope:

- `backend/src/test_runner`
- `backend/src/providers/clients/coding_plan`
- sandbox daemon implementation internals:
  `backend/src/sandbox/daemon`, `ephemeral_workspace`, `overlay`,
  `layer_stack`, `occ`, and internal isolated workspace implementation
- existing Rust sandbox daemon crates such as `eos-daemon`, `eos-layerstack`,
  `eos-overlay`, `eos-occ`, `eos-isolated`, `eos-plugin`, and
  `eos-runner`

Non-goals:

- No peer-to-peer agent messaging.
- No global agent orchestrator.
- No synthetic root workflow.
- No provider `class_path` dynamic import in the final Rust runtime.
- No tool visibility enum. A tool is visible only if its `ToolSpec` is present
  in the request's `Vec<ToolSpec>`.
- No deferred or lazy model-facing tool loading. Build concrete tool specs at
  agent spawn.

## Target Workspace

The `agent-core/` workspace is placed at the repository root —
`/Users/yifanxu/machine_learning/LoVC/EphemeralOS/agent-core/` — as a sibling of
`backend/` (the Python control plane) and the existing `sandbox/` Rust daemon
workspace. It is a separate Cargo workspace, not a member of `sandbox/`.

Recommended layout:

```text
agent-core/
  Cargo.toml
  crates/
    eos-types/
    eos-config/
    eos-state/
    eos-db/
    eos-audit/
    eos-llm-client/
    eos-agent-def/
    eos-tools/
    eos-engine/
    eos-workflow/
    eos-runtime/
    eos-sandbox-api/
    eos-sandbox-host/
    eos-skills/
    eos-plugin-catalog/
```

Dependency direction:

```text
eos-types
  <- eos-config
  <- eos-state
  <- eos-audit
  <- eos-llm-client
  <- eos-sandbox-api
  <- eos-agent-def

eos-db -> eos-state + eos-config
eos-skills -> eos-types + eos-config
eos-sandbox-host -> eos-sandbox-api + eos-config
eos-plugin-catalog -> eos-sandbox-api + eos-audit + eos-config
eos-tools -> eos-state + eos-sandbox-api + eos-skills + eos-audit + eos-llm-client
eos-engine -> eos-llm-client + eos-tools + eos-audit + eos-agent-def
eos-workflow -> eos-state + eos-tools + eos-agent-def + eos-audit
eos-runtime -> eos-db + eos-engine + eos-workflow + eos-sandbox-host
  + eos-plugin-catalog + eos-skills + eos-config + eos-agent-def
  + eos-sandbox-api + eos-state + eos-types + eos-llm-client + eos-tools
  + eos-audit
```

`ToolSpec` is owned by `eos-llm-client`; `eos-tools` depends on it to author the
model-facing tool specs. `eos-runtime` is the composition root and may depend
directly on every crate it constructs.

Use `tokio` with the `tracing` feature, `serde`, `serde_json`, `schemars`,
`sqlx` with the `sqlite` feature only, `reqwest`, `eventsource-stream` or a
small SSE parser, `futures`, `thiserror`, `tracing`, `tracing-subscriber`,
optional `console-subscriber` for `tokio-console`, dev `loom`, `uuid`, and
`time`.

## Design Rules

- Keep Task as the persisted agent interface. A request creates one root
  `Task(role=root, workflow_id=None)`. Delegation creates
  `Workflow -> Iteration -> Attempt`.
- Keep Attempt as the lifecycle unit for planner-authored generator/reducer DAGs.
  The reducer remains the exit gate.
- Keep ContextEngine as a workflow-context builder only. Lifecycle policy stays
  in workflow handlers and managers.
- Keep terminal tool enforcement in the engine/tool dispatch path. Terminal
  tools must be called alone.
- Convert Pydantic schemas to `serde` structs plus `schemars::JsonSchema`.
- Convert SQLAlchemy stores to SQLite-only `sqlx` repositories with typed rows
  and versioned migrations.
- Do not add PostgreSQL support to agent-core. The target database contract is
  local SQLite, with WAL, foreign keys, busy timeout, and explicit migrations.
- Convert Anthropic/OpenAI SDK use to direct HTTP/SSE clients under
  `eos-llm-client`.
- Keep provider-neutral message and stream events between LLM clients and the
  query loop.
- Keep sandbox provider terminology separate from LLM provider terminology:
  `llm_provider` vs `sandbox_provider`.
- Replace Python docstring fallback with explicit Rust `ToolSpec` descriptions.

## Module Plans

### 1. `eos-types`

Overview:

Common IDs, timestamps, JSON helpers, error wrappers, and small value types used
across agent-core. This should stay small. Do not make it a dumping ground for
domain logic.

Current Python files:

- `backend/src/task/task.py`
- `backend/src/workflow/_core/state.py`
- `backend/src/audit/base.py`
- `backend/src/sandbox/shared/models.py`

Core classes and fields:

- Task IDs, workflow IDs, iteration IDs, attempt IDs, request IDs, agent run IDs,
  sandbox IDs, tool use IDs.
- Timestamp fields currently appear as `created_at`, `updated_at`, `started_at`,
  `closed_at`, and event `ts`.

Core schemas:

- Newtype IDs such as `TaskId(String)`, `WorkflowId(String)`,
  `IterationId(String)`, `AttemptId(String)`, `RequestId(String)`.
- `UtcDateTime` wrapper around `time::OffsetDateTime`.
- `JsonObject = serde_json::Map<String, serde_json::Value>` for transitional
  metadata.

Rust target:

- `ids.rs`: typed ID newtypes with `Display`, `FromStr`, `Serialize`,
  `Deserialize`, `JsonSchema`.
- `time.rs`: clock trait and timestamp helpers.
- `error.rs`: shared `CoreError` only for cross-crate glue.

Gap closeout:

- Stop serializing `TaskRecord.id` as both `id` and `task_id`. Pick typed fields
  at boundaries.

### 2. `eos-state`

Overview:

Pure domain state for Task, Workflow, Iteration, Attempt, outcomes, submissions,
and store traits. No SQL, no provider HTTP, no sandbox provider code.

Current Python files:

- `backend/src/task/task.py`
- `backend/src/workflow/_core/state.py`
- `backend/src/workflow/_core/outcomes.py`
- `backend/src/workflow/_core/persistence.py`
- `backend/src/workflow/submissions.py`

Core classes and fields:

- `Task`: `id`, `request_id`, `role`, `instruction`, `status`,
  `workflow_id`, `iteration_id`, `attempt_id`, `agent_name`, `needs`,
  `outcomes`, `terminal_tool_result`.
- `TaskStatus`: `pending`, `running`, `done`, `failed`, `blocked`.
- `Workflow`: `id`, `request_id`, `workflow_goal`, `status`,
  `iteration_ids`, `parent_task_id`, `outcomes`, timestamps.
- `WorkflowStatus`: `open`, `succeeded`, `failed`, `cancelled`.
- `Iteration`: `id`, `workflow_id`, `sequence_no`, `creation_reason`,
  `iteration_goal`, `attempt_budget`, `status`, `attempt_ids`,
  `deferred_goal_for_next_iteration`, timestamps, `outcomes`.
- `Attempt`: `id`, `iteration_id`, `workflow_id`, `attempt_sequence_no`,
  `stage`, `status`, `planner_task_id`, `generator_task_ids`,
  `reducer_task_ids`, `deferred_goal_for_next_iteration`, `fail_reason`,
  timestamps, `outcomes`.
- `AttemptStage`: `plan`, `run`, `closed`.
- `AttemptStatus`: `running`, `passed`, `failed`.

Core schemas:

- `ExecutionTaskOutcome { status, role, task_id, outcome }`.
- `PlannerSubmission { attempt_id, planner_task_id, kind, generator_task_ids,
  reducer_task_ids, deferred_goal_for_next_iteration }`.
- `PlannerFailureSubmission { attempt_id, planner_task_id, fail_reason }`.
- `GeneratorSubmission { attempt_id, task_id, status, outcome,
  terminal_tool_result }`.
- `ReducerSubmission { attempt_id, task_id, status, outcome,
  terminal_tool_result }`.
- Store traits for workflow, iteration, attempt, task, request, and agent run.

Rust target:

- `task.rs`, `workflow.rs`, `iteration.rs`, `attempt.rs`.
- `outcomes.rs` with pure projection functions.
- `submissions.rs` with typed terminal submission DTOs.
- `store.rs` with async traits returning typed DTOs.

Gap closeout:

- Normalize naming: `workflow_goal`, `iteration_goal`, and
  `deferred_goal_for_next_iteration` in domain structs. DB columns can remain
  shorter, but mapping is explicit in `eos-db`.
- Normalize generator/executor naming: state role is `generator`; `executor` can
  be a profile alias only.

### 3. `eos-db`

Overview:

SQLite persistence and migrations. This crate implements `eos-state` store
traits using `sqlx` with the `sqlite` feature only. It should own the SQLite
pool, WAL/foreign-key/busy-timeout setup, migrations, row structs, and
repository implementations. PostgreSQL is not a target for agent-core.

Current Python files:

- `backend/src/db/base.py`
- `backend/src/db/engine.py`
- `backend/src/db/models/request.py`
- `backend/src/db/models/task.py`
- `backend/src/db/models/workflow.py`
- `backend/src/db/models/iteration.py`
- `backend/src/db/models/attempt.py`
- `backend/src/db/models/agent_run.py`
- `backend/src/db/models/model_registration.py`
- `backend/src/db/stores/task_store.py`
- `backend/src/db/stores/workflow_store.py`
- `backend/src/db/stores/iteration_store.py`
- `backend/src/db/stores/attempt_store.py`
- `backend/src/db/stores/agent_run_store.py`
- `backend/src/db/stores/model_store.py`

Core classes and fields:

- `RequestRecord`: `id`, `cwd`, `sandbox_id`, `request_prompt`,
  `root_task_id`, `status`, timestamps.
- `TaskRecord`: task fields listed in `eos-state`.
- `WorkflowRecord`: `id`, `request_id`, `parent_task_id`, `goal`, `status`,
  `iteration_ids`, `outcomes`, timestamps.
- `IterationRecord`: `id`, `workflow_id`, `sequence_no`, `creation_reason`,
  `goal`, `attempt_budget`, `status`, `attempt_ids`, `deferred_goal`,
  timestamps, `outcomes`.
- `AttemptRecord`: `id`, `iteration_id`, `workflow_id`, `attempt_sequence_no`,
  `stage`, `status`, `planner_task_id`, `generator_task_ids`,
  `reducer_task_ids`, `outcomes`, `deferred_goal`, `fail_reason`, timestamps.
- `AgentRunRecord`: `id`, `task_id`, `initial_messages`, `agent_name`,
  `message_history`, `terminal_tool_result`, `token_count`, `error`,
  timestamps.
- `ModelRegistrationRecord`: `id`, `key`, `label`, `class_path`,
  `kwargs_json`, `is_active`, timestamps.

Core schemas:

- `requests`, `tasks`, `workflows`, `iterations`, `attempts`, `agent_runs`,
  `model_registrations`.
- Unique constraints: `agent_runs.task_id`, `iterations(workflow_id,
  sequence_no)`, `attempts(iteration_id, attempt_sequence_no)`.
- SQLite migration schema under one local database file. JSON-like fields remain
  stored as SQLite `TEXT` containing validated JSON unless a field needs a
  dedicated relational table.

Rust target:

- `migrations/` with versioned SQL files. Replace live DDL patching in
  `db/engine.py`.
- `pool.rs` with `SqlitePool` setup, `PRAGMA foreign_keys=ON`, WAL mode, and
  busy timeout.
- `rows.rs` for SQL row structs.
- `repositories/` for each store.
- `model_registry.rs` for active model lookup and environment placeholder
  resolution during compatibility migration.

Gap closeout:

- Keep `class_path` only as migration data. Final provider dispatch is typed by
  `llm_provider` and `model_key`.
- Replace dict row serialization with typed DTO mapping.
- Provide one composition root that constructs every store required by runtime
  entry, workflow, and engine.
- Remove PostgreSQL connection-string handling from the Rust target. If legacy
  config points at PostgreSQL, fail fast with a migration error instead of
  silently starting a different backend.

### 4. `eos-config`

Overview:

Typed configuration, environment overrides, paths, and validation.

Current Python files:

- `backend/src/config/base.py`
- `backend/src/config/central.py`
- `backend/src/config/loader.py`
- `backend/src/config/settings.py`
- `backend/src/config/paths.py`
- `backend/src/config/model_config.py`
- `backend/src/config/sections/database.py`
- `backend/src/config/sections/sandbox.py`
- `backend/src/config/sections/providers.py`
- `backend/src/config/sections/runner.py`
- `backend/src/config/sections/engine.py`

Core classes and fields:

- `CentralConfig { database, sandbox, providers, runner, engine }`.
- `DatabaseConfig { url, pool_pre_ping, pool_size, max_overflow, echo }`.
- `SandboxConfig { default_provider, timeout_s, runtime_client_timeout_s,
  docker, daytona }`.
- `DockerConfig { daemon_tcp, privileged, no_privilege, default_snapshot }`.
- `DaytonaConfig { api_key, api_url, target, tcp_host, tcp_port,
  default_image, default_snapshot }`.
- `ProvidersConfig { retry, minimax }`.
- `RetryConfig { max_retries, base_delay_s, max_delay_s, status_codes }`.

Core schemas:

- Nested env prefix `EOS__`.
- Legacy env compatibility for `EPHEMERALOS_DATABASE_URL`,
  `EPHEMERALOS_SANDBOX_DEFAULT_IMAGE`,
  `EPHEMERALOS_SANDBOX_DEFAULT_SNAPSHOT`,
  `EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS`,
  `EPHEMERALOS_RUNTIME_CLIENT_TIMEOUT`, `EOS_SANDBOX_PROVIDER`,
  `MINIMAX_BASE_URL`, and `MINIMAX_MODEL`.
- Path envs: `EPHEMERALOS_CONFIG_DIR`, `EPHEMERALOS_DATA_DIR`,
  `EPHEMERALOS_LOGS_DIR`.
- Target database URL shape is SQLite only: `sqlite:<path>`,
  `sqlite://<path>`, or an app-resolved local data path. Network database URLs
  are rejected in Rust.

Rust target:

- `config.rs` with `serde` structs.
- `env.rs` with nested env parsing and legacy adapters.
- `paths.rs` with config/data/log path resolution.
- `database.rs` with Rust `DatabaseConfig { url, pool_size,
  busy_timeout_ms, wal, foreign_keys, echo }`.
- `validation.rs` for contradictions such as Docker `privileged` plus
  `no_privilege`.

Gap closeout:

- Treat `Settings` as compatibility only. `CentralConfig` is the target loader.
- Keep current Python database fields as migration evidence, but simplify the
  Rust config to SQLite settings only. Drop `pool_pre_ping` and `max_overflow`
  from the Rust target because they are connection-server concepts, not useful
  SQLite controls.
- Move provider retry defaults into config and make `eos-llm-client` consume
  them.
- Runner config is test-runner flavored and should not be imported unchanged
  into agent-core.

### 5. `eos-agent-def`

Overview:

Agent profile definitions, loading, registry, validation, and context recipe
metadata.

Current Python files:

- `backend/src/agents/definition/model.py`
- `backend/src/agents/definition/loader.py`
- `backend/src/agents/definition/registry.py`
- `backend/src/agents/definition/resolved_validation.py`
- `backend/src/agents/skills/loader.py`

Core classes and fields:

- `AgentType`: `agent`, `subagent`.
- `AgentRole`: `root`, `planner`, `generator`, `reducer`, `helper`,
  `subagent`.
- `AgentDefinition`: `name`, `description`, `system_prompt`, `model`,
  `tool_call_limit`, `role`, `agent_type`, `allowed_tools`, `terminals`,
  `notification_triggers`, `skill`, `context_recipe`.

Core schemas:

- Agent profile YAML/Markdown files become typed Rust profile files.
- Effective visible tool set is `allowed_tools union terminals`; this is materialized
  as concrete `ToolSpec`s at agent spawn.
- Main role terminal contract text remains an agent-definition validation input.

Rust target:

- `model.rs`: enums and `AgentDefinition`.
- `loader.rs`: parse profile files.
- `registry.rs`: app-state registry, not global mutable process state.
- `validation.rs`: context recipe and terminal contract validation.

Gap closeout:

- Keep the root/planner/generator/reducer roles explicit.
- Do not introduce an agent orchestrator above workflow attempts.
- Do not add a separate tool visibility abstraction. The registry is for lookup;
  request construction decides the final `Vec<ToolSpec>`.

### 6. `eos-llm-client`

Overview:

Provider-neutral LLM request/event types plus direct HTTP/SSE clients for
Anthropic and OpenAI. This crate replaces the Python Anthropic SDK dependency
and avoids a Rust dependency on an unofficial Anthropic SDK.

Current Python files:

- `backend/src/message/message.py`
- `backend/src/message/events.py`
- `backend/src/providers/types.py`
- `backend/src/providers/errors.py`
- `backend/src/providers/provider.py`
- `backend/src/providers/auth_strategy.py`
- `backend/src/providers/clients/anthropic_native.py`

Core classes and fields:

- `TextBlock { type, text }`.
- `ToolUseBlock { type, tool_use_id, name, input }`.
- `ThinkingBlock { type, text }`.
- `ToolResultBlock { type, tool_use_id, content, is_error, metadata,
  is_terminal }`.
- `SystemNotificationBlock { type, text }`.
- `Message { role, content }` where role is `user` or `assistant`.
- `MessageRequest { model, messages, system_prompt, max_tokens, tools,
  tool_choice }`.
- `UsageSnapshot { input_tokens, output_tokens, total_tokens }`.
- `StreamEvent`: assistant text delta, reasoning delta, tool-use delta,
  assistant complete, tool execution events, background events, system
  notifications.

Core schemas:

- Provider-neutral `ToolSpec { name, description, input_schema,
  output_schema }`.
- `LlmRequest` with provider-neutral messages and tools.
- `LlmStreamEvent` with normalized event variants:
  `AssistantTextDelta`, `ReasoningDelta`, `ToolUseDelta`,
  `AssistantMessageComplete`.
- `ProviderError { kind, status_code, request_id, message }`.

Rust target:

- `types.rs`: request, message, content block, tool spec, usage.
- `events.rs`: stream event enum.
- `client.rs`: `trait LlmClient`.
- `anthropic.rs`: `reqwest` POST `/v1/messages` with `stream: true`; parse
  `message_start`, `content_block_start`, `content_block_delta`,
  `content_block_stop`, `message_delta`, `message_stop`.
- `openai.rs`: `reqwest` POST `/v1/responses` with `stream: true`; parse
  `response.output_text.delta`, function/tool call argument deltas, done events,
  and completion.
- `sse.rs`: shared SSE frame parser.
- `retry.rs`: retry only before any visible stream output is emitted.
- `auth.rs`: explicit API key or bearer auth configuration.

Gap closeout:

- Rename provider-neutral `Thinking*` to `Reasoning*` in Rust, with a
  compatibility map while old JSONL transcripts exist.
- Move provider projection out of message domain. Anthropic encoding can drop
  `output_schema`; OpenAI encoding can map it when supported.
- Fix transcript mismatch where prompt-report JSONL can record a `system` role
  while `Message` only supports `user|assistant`.
- Replace base-url auth heuristics with explicit auth kind.
- Do not port coding-plan provider clients.

### 7. `eos-tools`

Overview:

Tool specs, registry, execution, hooks, terminal stamping, dispatch policy, and
model-facing tools. This crate owns the Rust replacement for Python `@tool`,
`BaseTool`, and concrete tool modules.

Current Python files:

- `backend/src/tools/_framework/core/base.py`
- `backend/src/tools/_framework/core/decorator.py`
- `backend/src/tools/_framework/core/registry.py`
- `backend/src/tools/_framework/core/results.py`
- `backend/src/tools/_framework/core/runtime.py`
- `backend/src/tools/_framework/core/validation.py`
- `backend/src/tools/_framework/execution/tool_call.py`
- `backend/src/tools/_framework/execution/hook_pipeline.py`
- `backend/src/tools/_framework/factory.py`
- `backend/src/tools/_names.py`
- `backend/src/tools/_terminals/registry.py`
- `backend/src/tools/sandbox`
- `backend/src/tools/workflow`
- `backend/src/tools/submission`
- `backend/src/tools/subagent`
- `backend/src/tools/ask_helper`
- `backend/src/tools/skills`
- `backend/src/tools/isolated_workspace`

Core classes and fields:

- `BaseTool`: `name`, `description`, `short_description`, `input_model`,
  `output_model`, `intent`, `task_type`, `is_terminal_tool`, `pre_hooks`,
  `post_hooks`, `context_requirements`.
- `ToolResult`: `output`, `is_error`, `metadata`, `is_terminal`.
- `ExecutionMetadata`: runtime IDs, sandbox ID, task/workflow IDs, background
  manager, tool use ID, notification service, extras.
- `ToolRegistry`: name to tool, with register/get/list/restrict/remove/schema
  operations.

Core schemas:

- `ToolSpec { name, description, input_schema, output_schema }`.
- `ToolIntent`: `read_only`, `write_allowed`, `lifecycle`.
- `ToolContextRequirement`: at least `sandbox`.
- Submission inputs:
  - root/generator/reducer terminal input `{ status, outcome }`.
  - planner input with generator tasks, reducer tasks, `needs`, and optional
    `deferred_goal_for_next_iteration`.
- Workflow tool inputs:
  - `delegate_workflow { goal }`.
  - `check_workflow_status { workflow_id, workflow_task_id? }`.
  - `cancel_workflow { workflow_task_id, reason }`.
- Sandbox tool inputs/outputs for read/write/edit/multi-edit/exec-command/stdin,
  grep, glob, enter/exit isolated workspace.
- Skill tool input `load_skill_reference { skill_name, reference_name }`.

Rust target:

- `spec.rs`: colocated-spec helpers; `ToolName`, `ToolIntent`, `ToolError`.
  `ToolSpec` itself is imported from `eos-llm-client`.
- `executor.rs`: `trait ToolExecutor`.
- `registry.rs`: concrete name to executor factory and spec.
- `execution.rs`: parse input, run hooks, execute, validate output, stamp
  terminal success.
- `dispatch.rs`: terminal batch rejection and lifecycle batch policy.
- `model_tools/`: sandbox, workflow, submission, helper, subagent, skills,
  isolated workspace.
- `descriptions/`: explicit static model-facing descriptions. Use
  `include_str!` for long prompt text when markdown is clearer.

Gap closeout:

- Tool docstrings do not translate to Rust doc comments. They become explicit
  `ToolSpec.description` strings plus generated JSON schemas.
- Every model-facing tool has exactly one colocated spec source. Do not mix
  prompt files, inline descriptions, and decorator fallback.
- Add compile-time or test-time coverage that every terminal tool has a terminal
  descriptor.
- Add a typed constant for every public tool name, including `write_stdin`,
  isolated workspace tools, and `load_skill_reference`.
- Wrapper tools and synthesized controls must carry `intent`; do not let them
  evade contract tests.
- Update stale subagent prompt text that still references retired generic
  wait/check controls.
- Make `exec_command` and command-session naming consistent. Keep daemon op
  names stable where protocol compatibility requires it.

### 8. `eos-engine`

Overview:

Agent query loop, request building, provider stream consumption, tool dispatch,
background task/session supervision, terminal enforcement, notifications, and
prompt reporting.

Current Python files:

- `backend/src/engine/query/context.py`
- `backend/src/engine/query/request.py`
- `backend/src/engine/query/loop.py`
- `backend/src/engine/tool_call/dispatch.py`
- `backend/src/engine/tool_call/streaming.py`
- `backend/src/engine/background/task_supervisor.py`
- `backend/src/engine/background/dispatch.py`
- `backend/src/engine/background/policy.py`
- `backend/src/engine/background/history.py`
- `backend/src/engine/agent/factory.py`
- `backend/src/engine/audit/stream.py`
- `backend/src/notification`
- `backend/src/prompt`

Core classes and fields:

- `QueryContext`: `api_client`, `tool_registry`, `cwd`, `model`,
  `system_prompt`, `max_tokens`, `tool_call_limit`, `agent_name`,
  `agent_run_id`, `task_id`, `tool_calls_used`,
  `text_only_no_terminal_turns`, `tool_metadata`,
  `enable_background_tasks`, `terminal_tools`, `exit_reason`,
  `terminal_result`, `event_source`, `prompt_report_recorder`, notification
  fields.
- `QueryExitReason`: `tool_stop`, `terminal_not_submitted`.
- `BackgroundTaskStatus`: current states for background tools.
- `BackgroundTaskRecord`, `CommandSessionRecord`, `WorkflowBackgroundRecord`.
- `SystemNotification { text, agent_name, agent_run_id }`.
- `NotificationRule { name, body, trigger, fire_once }`.
- `PromptReportRecorder` records `llm_request`, `assistant`, and
  `tool_results`.

Core schemas:

- `QueryRunRequest { request, prompt_report, prompt_report_seq }`.
- `AssistantToolDispatchOutcome { tool_results, terminal_result, events }`.
- Stream-to-audit event projection for tool started/completed.
- Prompt report JSONL event schema:
  - `llm_request { seq, system_prompt, messages, tools }`.
  - `assistant { seq, message, usage }`.
  - `tool_results { seq, tool_results }`.

Rust target:

- `query/context.rs`, `query/request.rs`, `query/loop.rs`.
- `tool_call/dispatch.rs`, `tool_call/streaming.rs`.
- `background/supervisor.rs`, `background/dispatch.rs`.
- `notifications.rs` with declarative rules.
- `prompt_report.rs` with JSONL writer.
- `agent/factory.rs` to build concrete registries and prompts from
  `AgentDefinition`.

Gap closeout:

- Keep terminal execution deferral inside the query loop/streaming executor so
  terminal exclusivity is checked after the full assistant message.
- The hard ceiling for terminal non-submission remains derived from
  `tool_call_limit`.
- Background execution is an engine dispatch mode, not provider state.
- Prompt report should reuse provider-neutral `Message` and `UsageSnapshot`
  schemas from `eos-llm-client`.

### 9. `eos-workflow`

Overview:

Delegated workflow lifecycle: starter, attempt orchestration, planner DAG
validation, run-stage scheduling, context engine, iteration coordination, and
workflow terminal submissions.

Current Python files:

- `backend/src/workflow/starter.py`
- `backend/src/workflow/attempt/orchestrator.py`
- `backend/src/workflow/attempt/plan_dag.py`
- `backend/src/workflow/attempt/run_stage.py`
- `backend/src/workflow/attempt/launch.py`
- `backend/src/workflow/context_engine/context.py`
- `backend/src/workflow/context_engine/engine.py`
- `backend/src/workflow/iteration_coordinator.py`
- `backend/src/workflow/lifecycle.py`
- `backend/src/workflow/composer.py`
- `backend/src/workflow/submissions.py`

Core classes and fields:

- `StartedWorkflow`: `parent_task_id`, `parent_attempt_id`, `workflow_id`,
  `iteration_id`, `attempt_id`.
- `WorkflowStarter`: validates parent task, creates workflow, first iteration,
  and first attempt.
- `AttemptOrchestrator`: attempt state machine.
- `AgentLaunch`: `task_id`, `request_id`, `attempt_id`, `role`, `agent_name`,
  `context`, `task_guidance`, `needs`, `agent_def`, `workflow_id`, `skill`.
- `AttemptDeps`: workflow/iteration/attempt/task stores, launcher,
  orchestrator registry, iteration coordinators, lifecycle config, composer,
  audit sink.
- `ContextSection`: `tag`, `attrs`, `text`, `children`.
- `AgentContext`: `role`, `sections`, `directive`, `context_limits`.

Core schemas:

- Planner DAG plan task input:
  `id`, `agent_name`, `task`, `needs`.
- Reducer input:
  reducer task plus dependencies on generator task IDs.
- `DagStatus`: `all_quiescent`, `all_done`, `any_failed_or_blocked`.
- Context recipes: planner, generator, reducer.

Rust target:

- `starter.rs`.
- `attempt/orchestrator.rs`.
- `attempt/plan_dag.rs`.
- `attempt/run_stage.rs`.
- `attempt/launch.rs`.
- `context/section.rs`, `context/engine.rs`.
- `iteration/coordinator.rs`.
- `lifecycle.rs`.

Gap closeout:

- Preserve first-class persisted state. Workflow lifecycle changes happen
  through store updates and terminal submissions.
- Do not mutate parent task at workflow close. Parent task remains responsible
  for its own terminal submission.
- Keep per-attempt orchestration. Do not add a global orchestrator.
- Preserve planner DAG invariants: no duplicate IDs, known dependencies, at
  least one reducer, lane shape, and acyclic graph.

### 10. `eos-runtime`

Overview:

Composition root for starting requests, constructing stores, provisioning
sandbox bindings, launching the root agent, and wiring workflow runtime.

Current Python files:

- `backend/src/runtime/entry.py`
- `backend/src/runtime/app_factory.py`
- `backend/src/runtime/sandbox_provisioning.py`

Core classes and fields:

- `RequestEntryHandle`: `request_id`, `root_task_id`, `workflow_runtime`,
  `launcher`, `root_agent_task`.
- `RequestEntry`: request prompt, cwd, sandbox binding, stores, runtime
  dependencies.
- `RequestSandboxBinding`: `sandbox_id`, `request_id`.
- `RequestSandboxProvisioner`: creates or starts sandbox and labels it.
- `RuntimeConfig`: store/model/sandbox/runtime configuration.

Core schemas:

- Request creation input: `cwd`, `request_prompt`, optional sandbox ID.
- Request row status and root task ID.
- Sandbox labels: `origin=workflow`, `request_id`.

Rust target:

- `entry.rs`: `start_request`.
- `app_state.rs`: typed runtime dependency graph.
- `sandbox_provisioning.rs`: request sandbox preparation.
- `root_agent.rs`: direct root agent lifecycle.

Gap closeout:

- Root task runs directly through the engine. There is no root workflow.
- Ensure every store required by workflow/runtime is initialized in one place.
- Treat Python model registry JSON paths as compatibility; do not depend on
  missing or optional registry files for runtime startup.

### 11. `eos-sandbox-api`

Overview:

Host-facing sandbox protocol schemas, daemon op constants, transport trait, and
typed request/result wrappers. This is the boundary agent-core uses to call the
existing sandbox runtime.

Current Python files:

- `backend/src/sandbox/api/transport.py`
- `backend/src/sandbox/api/tool/read.py`
- `backend/src/sandbox/api/tool/write.py`
- `backend/src/sandbox/api/tool/edit.py`
- `backend/src/sandbox/api/tool/command.py`
- `backend/src/sandbox/api/tool/glob.py`
- `backend/src/sandbox/api/tool/grep.py`
- `backend/src/sandbox/api/tool/shell.py`
- `backend/src/sandbox/shared/models.py`

Core classes and fields:

- `SandboxCaller`: `agent_id`, `run_id`, `agent_run_id`, `task_id`,
  `request_id`, `attempt_id`, `workflow_id`, `tool_name`, `tool_id`.
- `SandboxRequestBase`: `caller`, `description`, `invocation_id`.
- `SandboxResultBase`: `success`, `workspace`, `timings`, `conflict`,
  `conflict_reason`, `changed_paths`, `error`.
- `ToolCallRequest`: `invocation_id`, `agent_id`, `verb`, `intent`, `args`,
  `background`.
- File, edit, command, search, and isolated workspace request/result structs.

Core schemas:

- Daemon ops such as `api.v1.read_file`, `api.v1.write_file`,
  `api.v1.edit_file`, `api.v1.glob`, `api.v1.grep`, `api.v1.shell`,
  command-session writes/cancel, and audit operations.
- Guard/conflict result shape.
- Timing and workspace metadata.

Rust target:

- `models.rs`: shared request/result structs.
- `ops.rs`: daemon op constants.
- `transport.rs`: async transport trait and envelope parser.
- `tool_api/`: typed helper functions for file/edit/command/search calls.

Gap closeout:

- Populate `SandboxCaller.tool_name` or remove it from the Rust schema. Do not
  keep a field that is usually empty.
- Preserve daemon op names where needed for compatibility, but expose
  user-facing command/session terminology in tool specs.
- Keep isolated workspace host-facing enter/exit schemas, but exclude internal
  namespace implementation.

### 12. `eos-sandbox-host`

Overview:

Sandbox provider selection, provider adapters, lifecycle setup, daemon client
recovery, runtime artifact upload, context preparation, and request sandbox
provisioning support.

Current Python files:

- `backend/src/sandbox/provider/protocol.py`
- `backend/src/sandbox/provider/bootstrap.py`
- `backend/src/sandbox/provider/registry.py`
- `backend/src/sandbox/provider/docker/adapter.py`
- `backend/src/sandbox/provider/daytona/adapter.py`
- `backend/src/sandbox/host/lifecycle.py`
- `backend/src/sandbox/host/bootstrap.py`
- `backend/src/sandbox/host/daemon_client.py`
- `backend/src/sandbox/host/runtime_bundle.py`
- `backend/src/runtime/sandbox_provisioning.py`

Core classes and fields:

- `ProviderAdapter`: health, snapshots, create/get/list/start/stop/delete,
  labels, preview/log URLs, build logs, raw exec, archive upload, context
  preparation.
- Provider registry: default adapter plus per-sandbox adapter binding.
- Daemon client config: protocol version, daemon auth token, runtime selection,
  TCP endpoint, layer stack root.
- Sandbox lifecycle functions: create, start, stop, delete, set labels, ensure
  running.

Core schemas:

- Sandbox provider kind: Docker only in the Rust target (the `ProviderAdapter`
  seam + registry are kept so Daytona and other providers can be re-added later).
- Daemon envelope fields: protocol version, auth token, op, args.
- Runtime selection: Python or Rust during compatibility. Target should default
  to Rust daemon after migration.

Rust target:

- `provider.rs`: adapter trait.
- `registry.rs`: explicit app-state registry, not hidden process global state.
- `docker.rs`: the concrete adapter (Docker is the only Rust provider; `daytona.rs`
  is not ported now — the trait + registry are kept so it can be re-added).
- `daemon_client.rs`: transport, recovery, error decoding.
- `lifecycle.rs`: create/start/stop/delete/setup.
- `runtime_artifact.rs`: Rust daemon upload/readiness checks.

Gap closeout:

- `runtime_bundle.py` currently bundles Python daemon code. Rust host should
  shrink this to artifact upload plus compatibility bridge.
- Provider bootstrap is first-call-wins today. Model it as explicit app state in
  Rust.
- Do not port the Daytona adapter now: Docker is the only Rust sandbox provider.
  Keep the `ProviderAdapter` trait + registry + `ProviderKind`/`SandboxProvider`
  (Docker-only, `#[non_exhaustive]`) so Daytona/other providers can be re-added by
  implementing the trait, and fail fast on a legacy `EOS_SANDBOX_PROVIDER=daytona`
  rather than silently accepting it.
- Keep the deep sandbox migration separate. Agent-core calls the daemon; it does
  not reimplement LayerStack, OCC, overlay, or plugin execution internals.

### 13. `eos-audit`

Overview:

Audit event envelope, sinks, JSONL writing, synchronous/in-memory bus, and
engine/tool stream translation.

Current Python files:

- `backend/src/audit/base.py`
- `backend/src/audit/bus.py`
- `backend/src/audit/jsonl.py`
- `backend/src/engine/audit/stream.py`
- plugin audit shim in `backend/src/plugins/core/loader.py`

Core classes and fields:

- `AuditNode`: request/workflow/iteration/attempt/task/agent/sandbox/tool IDs.
- `AuditEvent`: `source`, `type`, `node`, `payload`, `correlation_id`, `ts`.
- `AuditSink`: protocol for event sinks.
- `AuditEventBus`: synchronous dispatch.

Core schemas:

- JSONL audit event.
- Tool execution started/completed audit rows with redacted input/output shape,
  digest, bytes, and terminal metadata.
- Plugin audit events:
  `plugin.tool_invoked`, `plugin.error`, and completion events with
  `plugin_id`, `plugin_kind`, `plugin_tool_name`, duration, status.

Rust target:

- `event.rs`, `node.rs`, `sink.rs`, `bus.rs`, `jsonl.rs`.
- `redaction.rs` for tool input/output summaries.
- `engine_stream.rs` to translate stream events.
- Add `schema_version`.

Gap closeout:

- Keep audit IDs typed.
- Make redaction deterministic and testable.
- Avoid plugin audit keys that encode plugin kind in the event name. Kind stays
  a payload value.

### 14. `eos-skills`

Overview:

Skill registry and bundled skill loading. This is distinct from developer-side
Codex skills; it is the runtime skill content exposed to agents through agent
profiles and tools.

Current Python files:

- `backend/src/skills/core/types.py`
- `backend/src/skills/core/registry.py`
- `backend/src/skills/core/loader.py`
- `backend/src/skills/bundled/__init__.py`
- `backend/src/tools/skills/load_skill_reference.py`

Core classes and fields:

- `SkillDefinition`: `name`, `description`, `content`, `source`, `path`,
  `references`.
- `SkillRegistry`: register, get, list.

Core schemas:

- Directory skill format: `<skill-name>/SKILL.md`.
- Optional YAML frontmatter: `name`, `description`.
- Optional `references/*.md` mapped by file stem.
- Tool schema: `load_skill_reference { skill_name, reference_name }` (owned by
  `eos-tools`; this crate only supplies the registry/loader it reads).

Rust target:

- `definition.rs`, `registry.rs`, `loader.rs`.
- `bundled.rs` for config-backed skill directory loading.

Gap closeout:

- Current Python loader ignores `cwd`; decide whether Rust skill loading is
  config-root only or repo-root aware, and encode that explicitly.
- Keep reference loading deterministic. Avoid implicit filesystem traversal
  outside the configured skill directory.

### 15. `eos-plugin-catalog`

Overview:

Plugin manifest parsing, discovery, catalog registration, model-facing plugin
tool specs, and plugin audit wrapping. Deep plugin runtime and LSP session
internals remain outside agent-core or are delegated to sandbox daemon/plugin
runtime crates.

Current Python files:

- `backend/src/plugins/core/manifest.py`
- `backend/src/plugins/core/discovery.py`
- `backend/src/plugins/core/loader.py`
- `backend/src/plugins/catalog/lsp/plugin.md`
- `backend/src/plugins/catalog/lsp/tools/*.py`
- `backend/src/plugins/catalog/lsp/runtime/*`

Core classes and fields:

- `ToolEntry`: `name`, `module`.
- `PluginManifest`: `name`, `description`, `tools`, `setup`, `runtime`,
  `source_dir`, `body`, `kind`.
- `ALLOWED_PLUGIN_KINDS`: `language_server`, `formatter`, `indexer`,
  `build_daemon`, `mcp_bridge`, `custom`.

Core schemas:

- `plugin.md` frontmatter with `name`, `description`, `tools`, optional
  `setup`, optional `runtime`, optional `kind`.
- Tool names must start with `<plugin_name>.`.
- Paths must resolve under the plugin directory.
- LSP tool schemas such as `hover`, `find_definitions`, `find_references`,
  `diagnostics`, `query_symbols`, `rename`, `format_document`,
  `code_actions`, `apply_code_action`, and `apply_workspace_edit`.

Rust target:

- `manifest.rs`: parse and validate `plugin.md`.
- `discovery.rs`: catalog discovery and duplicate detection.
- `discovery.rs`: also owns the immutable `PluginCatalog` app-state value.
- `tool_specs.rs`: expose model-facing specs for plugin tools.
- `audit.rs`: plugin audit wrapper.

Gap closeout:

- Do not import Python plugin tool modules in Rust. Either generate Rust specs
  from manifests or define Rust-native plugin tool specs that call sandbox
  plugin RPCs.
- Keep LSP runtime internals separate. Agent-core should expose/call the LSP
  tool boundary, not own Pyright session internals.
- Keep plugin setup/runtime paths validated but treat actual execution as a
  sandbox/plugin-runtime concern.

## Tool Description and Schema Conversion

Python today mixes:

- decorator descriptions,
- docstring fallback,
- separate `prompt.py` files,
- terminal descriptor catalog entries,
- inline descriptions in wrappers and controls.

Rust target:

- Every model-facing tool has one `ToolSpec` source.
- Long model-facing text is a static string near the tool module:
  `include_str!("description.md")` or a `const DESCRIPTION: &str`.
- Input and output schemas are generated from Rust structs with
  `schemars::JsonSchema`.
- Terminal descriptors are total over all terminal tools.
- Tool name constants are typed and exhaustive.

Example:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SubmitGeneratorOutcomeInput {
    pub status: GeneratorOutcomeStatus,
    pub outcome: String,
}

pub fn spec() -> ToolSpec {
    ToolSpec::new(
        SUBMIT_GENERATOR_OUTCOME,
        DESCRIPTION,
        schemars::schema_for!(SubmitGeneratorOutcomeInput),
        schemars::schema_for!(SubmitGeneratorOutcomeOutput),
    )
    .terminal(true)
    .intent(ToolIntent::Lifecycle)
}
```

Rust doc comments remain for developer documentation. They are not the source of
model-facing tool descriptions.

## API Client Layer

`eos-llm-client` should be simple and provider-neutral:

```text
LlmClient
  stream_message(LlmRequest) -> Stream<Result<LlmStreamEvent, ProviderError>>

AnthropicClient
  reqwest + SSE parser + Anthropic encoder/decoder

OpenAiClient
  reqwest + SSE parser + Responses API encoder/decoder
```

Reliability rules:

- Retry only when no visible event has been emitted.
- After assistant text, reasoning, or tool-use deltas are emitted, fail fast to
  avoid duplicate deltas/tool calls.
- Retry 429 and transient 5xx according to `ProvidersConfig.retry`.
- Refresh auth only before visible output, or once on initial 401 when the auth
  strategy supports refresh.
- Preserve provider request IDs and status codes in `ProviderError`.
- Log stream parse errors with enough context, but do not dump tool arguments or
  secrets.

Provider projection:

- Anthropic: map local tools to Anthropic tool schema and drop unsupported
  `output_schema`.
- OpenAI: map local tools to Responses API function/tool specs and normalize
  function-call argument deltas into `ToolUseDelta`.
- Message domain stays provider-neutral. Encoders/decoders live in provider
  modules.

## SRP, Naming, and Prompt Gaps to Close

State and workflow:

- `goal` vs `workflow_goal` vs `iteration_goal` should be normalized at domain
  boundaries.
- `deferred_goal` should become `deferred_goal_for_next_iteration` in Rust
  domain state.
- `generator` is the state role. `executor` is at most a profile alias.
- Keep Attempt orchestration per-attempt.

Providers:

- Use `llm_provider` for model clients and `sandbox_provider` for the Docker
  sandbox backend (the only Rust provider; seam kept for future providers).
- Replace dynamic `class_path` client loading with typed config.
- Unify retry config. Do not keep local retry constants in a provider client.
- Keep coding-plan clients out of agent-core.

Tools:

- Tool exposure is just the request's `Vec<ToolSpec>`.
- Visible tool set for an agent is derived from profile
  `allowed_tools union terminals`, plus synthesized typed controls where required.
- No separate visibility enum.
- No lazy model-facing tool loader.
- Every wrapper/control tool must carry `intent`.
- Every public tool name has a typed constant.
- Every terminal tool has a descriptor.
- Fix stale prompt text before porting it to Rust.

Sandbox:

- Agent-core owns host lifecycle and daemon transport only.
- Deep workspace execution remains daemon-side.
- Populate or remove `SandboxCaller.tool_name`.
- Keep public command/session naming consistent even if daemon ops retain legacy
  `shell` labels.
- Grep prompt currently says Python `re`; Rust must either preserve that
  contract or deliberately update prompt/schema together.

Plugins and skills:

- Plugin manifest discovery belongs in agent-core.
- Plugin runtime execution stays sandbox/plugin-runtime side.
- Skill loading should have one explicit root and deterministic reference
  loading.

Prompt reporting and notifications:

- Prompt reports should use provider-neutral message schemas.
- Notification rules stay declarative and engine-owned.
- System notifications remain stream-visible and transcript-visible according
  to engine policy.

## Migration Phases

### Phase 0: Scaffolding and Parity Harness

- Create `agent-core` workspace and crate skeleton.
- Add Rust formatting, clippy, and CI hooks.
- Add tracing/subscriber setup plus optional Tokio console support before
  implementing async runtime behavior.
- Add schema snapshot tests against current Python JSON schema output.
- Add fixture transcripts for provider stream normalization and prompt reports.
- Add SQLite DB schema snapshots for current tables.

Verification:

- `cargo fmt --check`.
- `cargo clippy --workspace --all-targets -- -D warnings`.
- Rust schema snapshots match current Pydantic schemas for selected message
  contracts. Sandbox request/result DTOs are dataclasses and `ToolSpec` goldens
  require per-agent tool binding, so those are deferred to `eos-sandbox-api` and
  `eos-tools` acceptance criteria.

### Phase 1: Foundation

- Implement `eos-types`: ID newtypes, `UtcDateTime`, `Clock`, `CoreError`,
  `JsonObject`.

Verification:

- ID round-trip tests (`Display`/`FromStr`/serde/schemars).
- `UtcDateTime` RFC3339 and UTC-normalization tests.

### Phase 2: Leaf Domain and Boundary Crates

- Implement `eos-config`, `eos-state`, `eos-audit`, `eos-sandbox-api`, and
  `eos-agent-def`.
- Convert workflow/task state, terminal submissions, config, audit, sandbox
  request/result envelopes, and agent definitions before persistence/execution.

Verification:

- Config env override tests.
- State outcome-projection and submission DTO tests.
- Audit JSONL golden and redaction tests.
- Sandbox API envelope shape and dataclass parity fixtures.
- Agent definition load/validate tests.

### Phase 3: Persistence, Providers, Sandbox Host, Plugins, Skills

- Implement `eos-llm-client`, `eos-skills`, `eos-db`, `eos-sandbox-host`, and
  `eos-plugin-catalog`.
- Add direct HTTP/SSE Anthropic/OpenAI clients, SQLite repositories/migrations,
  provider lifecycle/provisioning, plugin manifest discovery, and skill registry.

Verification:

- Anthropic/OpenAI SSE fixture replay, retry, and error-mapping tests.
- Store roundtrip and SQLite migration tests.
- Docker provider selection and provisioning tests (seam ready for future providers).
- Plugin manifest validation tests.
- Skill reference-loading determinism tests.

### Phase 4: Tool Framework

- Implement `eos-tools` specs, registry, execution, hooks, terminal stamping,
  dispatch policy, and all model-facing tools.
- Port submission, workflow, sandbox, subagent, helper, skill, and isolated
  workspace tools.
- Add compile/test coverage for terminal descriptors and tool names.

Verification:

- Terminal tool called with siblings is rejected.
- Terminal success stamps `is_terminal=true`.
- Lifecycle batch policy matches current behavior.
- Representative tool schema snapshots match Python where contracts are
  intentionally preserved.
- Prompt/description coverage tests pass.

### Phase 5: Execution Core

- Implement `eos-engine` and `eos-workflow`: query context/loop, provider stream
  consumption, dispatch, background supervisor, notifications, prompt reports,
  workflow starter, attempt orchestrator, run-stage scheduler, context engine,
  and iteration coordinator.

Verification:

- Query-loop stop and terminal non-submission ceiling tests.
- Background command/session cancellation tests.
- Prompt report JSONL golden and notification rule tests.
- Planner DAG validation tests.
- Generator/reducer scheduling tests.
- Reducer exit-gate, attempt close, and outcome projection tests.

### Phase 6: Runtime Composition

- Implement `eos-runtime`: `AppState` graph, root request entry, root-agent
  lifecycle, sandbox provisioning, provider/registry wiring, and observability
  initialization.

Verification:

- Root request creates root task and no root workflow.
- `delegate_workflow` creates `Workflow -> Iteration -> Attempt` and leaves the
  parent task running.
- Request sandbox provisioning tests.
- End-to-end mocked root request and delegated workflow tests.

### Phase 7: Cutover

- Add an explicit cutover adapter (subprocess CLI, JSON-RPC, or extension module
  chosen in the cutover spec) to run old and new control planes side by side.
- Define request/result schemas, exit codes, stdout/stderr/log contract, feature
  flag, DB ownership, and shadow-mode comparison tests.
- Run Rust control plane against existing daemon and DB fixtures.
- Choose the DB deployment path: greenfield SQLite-only, one-shot SQLite import,
  or explicit PostgreSQL deprecation/error handling.
- Define packaging for `eos-runtime` as a sidecar binary, standalone package, or
  bundled wheel resource with release CI and package-location tests.
- Retire Python modules by package boundary after parity is proven.
- Rebuild test-runner integration separately; do not migrate
  `backend/src/test_runner` in this plan.

Verification:

- End-to-end root agent request with mocked LLM.
- Delegated workflow end-to-end with planner/generator/reducer fixtures.
- Sandbox command/read/write/edit/search tool integration against Rust daemon.
- Provider mock tests for Anthropic and OpenAI.

## Tests to Port First

High-value current Python tests to port or recreate:

- Tool execution and terminal stamping:
  `backend/tests/unit_test/test_tools/test_tool_execution.py`.
- Terminal batch rejection:
  `backend/tests/unit_test/test_engine/test_tool_batch.py`.
- Lifecycle dispatch:
  `backend/tests/unit_test/test_engine/test_tool_call_dispatch_lifecycle.py`.
- Tool schema summaries:
  `backend/tests/unit_test/test_tools/test_schema_summary.py`.
- Main role terminal submissions:
  `backend/tests/unit_test/test_tools/test_submission_main_role_terminals.py`.
- Sandbox command and stdin:
  `backend/tests/unit_test/test_tools/test_sandbox_toolkit/test_exec_command.py`
  and `test_write_stdin.py`.
- Workflow DAG/orchestrator/context tests under `backend/tests`.
- DB store roundtrips for request/task/workflow/iteration/attempt/agent_run.

Do not port `backend/src/test_runner` as part of agent-core. Build a new Rust
or mixed integration harness later after the control plane boundary is stable.

## Implementation Order by Risk

1. Types/state/config/db/audit.
2. LLM client with fixture-based SSE tests.
3. Tool specs and dispatch policy.
4. Engine query loop.
5. Workflow lifecycle.
6. Runtime entry and sandbox provisioning.
7. Sandbox host and plugin/skill catalogs.
8. Compatibility cutover.

This order keeps the hardest semantic surfaces testable before the runtime is
fully switched.

## Fit Assessment

Good fit:

- Pydantic maps cleanly to `serde` plus `schemars`.
- SQLAlchemy maps cleanly to SQLite-only `sqlx` repositories and migrations.
- SQLite is enough for the current agent-core persistence model: local request
  state, task/workflow rows, transcripts, model registrations, and audit-adjacent
  metadata.
- Anthropic/OpenAI do not need SDKs; direct HTTP/SSE gives better control over
  retry and stream duplication semantics.
- Existing code already has narrow protocols and DTO-like state modules.
- Rust improves compile-time coverage for tool names, terminal descriptors,
  provider kinds, workflow statuses, and request schemas.

Main risks:

- Query-loop and terminal-tool behavior is subtle.
- Background task/session behavior must preserve cancellation and parent-exit
  semantics.
- Workflow attempt scheduling must preserve reducer exit-gate semantics.
- Sandbox host protocol must remain compatible with ongoing Rust daemon work.
- Prompt/tool description drift can silently change model behavior.

The migration is worth doing if the first phases are held to schema and behavior
parity tests. A direct rewrite without parity fixtures would be risky.
