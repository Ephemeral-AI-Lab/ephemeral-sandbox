---
title: "Engine + Query Loop + LLM Seam"
tags: ["engine", "query-loop", "llm-seam", "supports-streaming-messages", "api-client", "run-ephemeral-agent", "live-e2e", "load-bearing", "see-also"]
created: 2026-05-10T11:31:40.882Z
updated: 2026-05-10T11:58:12.532Z
sources: []
links: ["live-e2e-testing-framework-design.md", "tools-hooks-guardrails-agents-notifications-messages.md", "task-center-pipeline.md"]
category: architecture
confidence: medium
schemaVersion: 1
---

# Engine + Query Loop + LLM Seam

_Source: explore agent draft, 2026-05-10. See `.omc/wiki-draft/engine-query.md`. **Load-bearing page** — names the LLM API seam._

## The LLM API seam (the central fact)

- **Protocol**: `providers.types.SupportsStreamingMessages` at `backend/src/providers/types.py:112`
- **Single method**: `async def stream_message(self, request: ApiMessageRequest) -> AsyncIterator[ApiStreamEvent]` (`providers/types.py:115`)
- **ApiStreamEvent union** — 5 cases, all at `providers/types.py`:
  - `ApiThinkingDeltaEvent` (line 51) — incremental thinking/reasoning chunk
  - `ApiTextDeltaEvent` (line 57) — incremental assistant text chunk; CANCEL_PATTERN extracted here
  - `ApiToolUseDeltaEvent` (line 73) — one tool_use block mid-stream with id/name/input
  - `ApiCancelEvent` (line 86) — LLM-issued abort signal targeting a running tool
  - `ApiMessageCompleteEvent` (line 64) — terminal event with full assistant ConversationMessage and UsageSnapshot
- **Where it is consumed**: `engine/query/loop.py:_consume_provider_stream` (line 171) — the single `async for event in context.api_client.stream_message(run_request.request):` at line 178
- **Where it is set**: `QueryContext.api_client` field (`engine/query/context.py:40`). Constructed by `make_api_client` (`providers/provider.py:57`), which returns `external` as-is if supplied; otherwise builds `AnthropicClient`.
- **This is the seam. Mocking only this Protocol keeps the entire query loop real.**

## QueryContext

`backend/src/engine/query/context.py:39` (`@dataclass class QueryContext`).

Key fields: `api_client` (line 40, **the seam**), `tool_registry`, `cwd`, `model`, `system_prompt`, `max_tokens`, `agent_name`, `run_id`, `task_center_task_id`, `tool_call_limit`, `tool_calls_used`, `tool_metadata: ExecutionMetadata`, `enable_background_tasks`, `terminal_tools` (set populated from registry at loop init), `exit_reason`, `terminal_result`, `prompt_report_recorder`, `notification_rules`, `notification_fired`, `notification_state`.

`QueryExitReason` (`context.py:16`, `StrEnum`):
- `TEXT_RESPONSE` — model produced no tool_uses
- `TOOL_STOP` — terminal tool succeeded
- `RESOURCE_LIMIT` — tool_call_limit exceeded or max_tokens hit

## ApiMessageRequest

`providers/types.py:37` — `model`, `messages`, `system_prompt`, `max_tokens`, `tools`, `tool_choice`, `raw_messages`.

Built per turn by `build_query_run_request` (`engine/query/request.py:44`):
- `prepare_provider_messages(messages)` (`engine/query/provider_history.py:30`) — sanitized provider-safe view
- `context.tool_registry.to_api_schema()` then `decorate_schemas_for_background(...)` when `enable_background_tasks`
- Records `event="llm_request"` to `PromptReportRecorder` (`request.py:59-67`)
- Returns `QueryRunRequest` wrapping `ApiMessageRequest` plus `prompt_report` and `prompt_report_seq`

## run_query loop algorithm

**Entry**: `run_query` at `engine/query/loop.py:371` — wraps `_run_query_loop` with agent_name/run_id stamper. Returns `(messages, AsyncIterator)`.

**Core loop**: `_run_query_loop` at `engine/query/loop.py:303`:

1. **`_initialize_loop_state`** (line 109) — coerce metadata, `ensure_system_notification_service`, `BackgroundTaskManager` if enabled, populate `terminal_tools` from registry.

2. **Per-turn body** (line 309+):
   - **`_build_stream_executor`** (line 142) — fresh `StreamingToolExecutor`, `prepare_tool_execution_context`
   - **Notification rules dispatch + drain** (lines 315-326) — `await dispatch_rules(...)`, `pop_pending_notifications()` appends pending blocks as user message
   - **`build_query_run_request`** (line 329) — records `llm_request`
   - **`_consume_provider_stream`** (line 171) — the seam consumption:
     - `ApiThinkingDeltaEvent` → yield `ThinkingDelta`
     - `ApiTextDeltaEvent` → CANCEL_PATTERN extraction (`re.compile(r'\[CANCEL:(\S+)(?:\s+reason="([^"]*)")?\]')`, line 58); yield `AssistantTextDelta`
     - `ApiToolUseDeltaEvent` → `_consume_tool_budget_or_reject(...)` (`tools/execution/tool_call.py:59`); rejected → `ToolResultBlock` rejection appended, yield `ToolExecutionCompleted(is_error=True)`; otherwise → `executor.add_tool(event)`, drain progress/events
     - `ApiCancelEvent` → `executor.cancel(event.tool_id, event.reason)`
     - `ApiMessageCompleteEvent` → set `state.final_message` + `state.usage`
     - Stream ending without `final_message` raises RuntimeError (line 224-228)
   - **`_drain_executor_after_stream`** (line 231) — apply CANCEL_PATTERN-captured cancels, drain
   - **`record_assistant_message` + `AssistantMessageComplete`** (lines 341-342)
   - **Text-only exit** (lines 344-348) — `final_message.tool_uses` empty → flush, `TEXT_RESPONSE`, break
   - **`_handle_tool_dispatch_branch`** (line 244) — `dispatch_assistant_tools`, `record_tool_results`, `flush_system_notifications`, terminal-result check, `tool_call_limit` check, append tool results as user message
   - Break on `TOOL_STOP` / `RESOURCE_LIMIT` (lines 361-365)

3. **Post-loop** (lines 367-368) — `await background_manager.cancel_all()` if pending.

## Streaming tool executor

- `StreamingToolExecutor` (`engine/tool_call/streaming.py:66`)
- `defer_background_dispatch` (`streaming.py:47`) — defers `background="always"` or `background="optional"+background=true` tools
- `_make_stream_dispatch_deferrer` (`engine/query/loop.py:61`) — once a terminal tool seen, defers all subsequent so `validate_tool_batch` (`engine/tool_call/batch.py:23`) enforces terminal-exclusivity on the full tool_uses list after stream completes

**During replay**: the fake api_client emits `ApiToolUseDeltaEvent` for tools to execute. The deferrer correctly batches them. Terminal tools deferred mid-stream are dispatched via `_dispatch_deferred_tool_calls` → `validate_tool_batch` enforces alone-ness.

## Tool budget rejection mid-stream

`_consume_tool_budget_or_reject` (`tools/execution/tool_call.py:59`) — called inside `_consume_provider_stream` for every `ApiToolUseDeltaEvent` before `executor.add_tool`. Rejection: `ToolResultBlock(is_error=True)` appended to `state.streamed_rejections`, `ToolExecutionCompleted(is_error=True, tool_id=event.id)` yielded — the tool never dispatches.

## Notification rules dispatch within loop

- `notification.dispatch_rules` (`notification/_rule_engine.py:56`) invoked **per loop iteration** at `loop.py:316-321`, before `build_query_run_request`
- `flush_system_notifications` called in three places per turn:
  1. After `dispatch_assistant_tools` (line 271)
  2. When `tool_call_limit` exceeded (line 294)
  3. On text-only exit (line 345)

## run_ephemeral_agent lifecycle

`engine/agent/lifecycle.py:73`. Signature:
```python
async def run_ephemeral_agent(
    config: RuntimeConfig,
    prompt: str,
    *,
    agent_def: AgentDefinition | None = None,
    sandbox_id: str | None = None,
    initial_messages: list[ConversationMessage] | None = None,
    persist_agent_run: bool = True,
    task_id: str | None = None,
    on_event: AgentStreamEmitter | None = None,
    on_agent_spawned: Callable[[Any], None] | None = None,
    extra_tool_metadata: ExecutionMetadata | dict[str, Any] | None = None,
) -> EphemeralRunResult
```

Steps (`lifecycle.py:96-201`):
1. `spawn_agent(config, messages, agent_def=, sandbox_id=)` (`factory.py:293`) — builds `QueryContext` with `api_client=make_api_client(config.external_api_client, db_kwargs=db_kwargs)` at `factory.py:173`
2. `on_agent_spawned(agent)` hook
3. `AgentRunTracker.create(task_id=, agent_name=)` — persists agent_run row if DB ready
4. Merges `extra_tool_metadata` into `agent.query_context.tool_metadata`
5. `async for event in agent.run(prompt)` (`factory.py:70`) → calls `run_query(self.query_context, self._messages)` and iterates
6. Captures `terminal_result` from `ToolExecutionCompleted(does_terminate=True)` events
7. `tracker.finish(messages, terminal_tool_result, token_count, error)`
8. Returns `EphemeralRunResult(status, error, terminal_result, agent_name, event_count)`

`EphemeralRunResult` (`lifecycle.py:38`): `status: "completed"|"failed"`, `error`, `terminal_result`, `agent_name`, `event_count`.

**Wiring the seam**: `_resolve_agent_identity` reads `config.external_api_client` at `factory.py:174`; `spawn_agent` wires the resolved client into `query_context.api_client` at `factory.py:357`. To inject the fake replay client:
1. **Preferred (non-subagent runs)**: pass `FakeReplayApiClient` as `config.external_api_client` — `make_api_client` short-circuits at `provider.py:68` and returns it directly
2. **Direct override (works for all agent types including subagents)**: use `on_agent_spawned` callback to set `agent.query_context.api_client = fake_client` before `agent.run(prompt)` is awaited

**Subagent caveat**: `factory.py:172` sets `needs_fresh_client = bool(agent_def and agent_def.agent_type == "subagent")` — when an agent has `agent_type="subagent"` (advisor, resolver, explorer dispatched via `run_subagent`), `make_api_client` is called with `external=None` and the `external_api_client` is ignored. Subagents always get a fresh `AnthropicClient` from db_kwargs. Use pattern #2 (or fake the HTTP layer via `db_kwargs.base_url`) for replay scenarios that exercise subagents.

## What the live-e2e framework needs

### Primary seam
Implement `FakeReplayApiClient` against `SupportsStreamingMessages` (`providers/types.py:112`). The single method `stream_message(request)` is an async generator that yields scripted `ApiStreamEvent` instances from a pre-recorded or hand-authored script.

### Wiring point
`QueryContext.api_client` (`engine/query/context.py:40`). Two patterns:
1. **Preferred (non-subagent only)**: pass via `config.external_api_client` — `make_api_client` short-circuits at `provider.py:68`. `_resolve_agent_identity` reads it at `factory.py:174`; `spawn_agent` wires the result into `QueryContext` at `factory.py:357`.
2. **Direct override (all agent types)**: `on_agent_spawned` callback sets `agent.query_context.api_client = fake_client` before `agent.run(prompt)`.

Note: subagent agents (`agent_type="subagent"`) trigger `needs_fresh_client=True` at `factory.py:172` and bypass pattern #1. Use pattern #2 for any replay that exercises advisor/resolver/explorer helpers.

### Real-vs-fake split

| Component | Real during replay |
|---|---|
| `engine.query.loop._run_query_loop` | YES — full loop runs |
| `StreamingToolExecutor` | YES |
| `BackgroundTaskManager` | YES |
| `dispatch_assistant_tools` | YES |
| `validate_tool_batch` | YES — terminal-exclusivity enforcement |
| `_consume_tool_budget_or_reject` | YES — budget enforced |
| `flush_system_notifications` | YES |
| `PromptReportRecorder` | YES — canonical capture |
| `api_client.stream_message` | FAKE — only this method |

### Public surface
- `run_ephemeral_agent` (`engine/agent/lifecycle.py:73`)
- `EphemeralRunResult` (`lifecycle.py:38`)
- `QueryContext` (`engine/query/context.py:39`)
- `SupportsStreamingMessages` (`providers/types.py:112`)
- `ApiStreamEvent` union (`providers/types.py:98-104`)

### What tests can assert
- **Tool guardrail / terminal-exclusivity**: script terminal + sibling tools → assert rejection by `validate_tool_batch` (`tool_call/batch.py:23`)
- **Max-step / budget**: script enough tool turns → loop exits `RESOURCE_LIMIT`
- **Terminal tool submission**: script terminal alone → `exit_reason=TOOL_STOP`, `terminal_result` populated
- **System notification trigger**: script multi-turn sequence → assert notifications appear in messages at right turn
- **Tool call execution**: tools run real against sandbox/task_center
- **PromptReportRecorder as capture**: `record("llm_request")`, `record("assistant")`, `record("tool_results")` per turn — read after run for canonical replay artifact

---

## Update (2026-05-10T11:58:12.532Z)

## See also

- [[live-e2e-testing-framework-design]] — the framework that consumes this seam
- [[tools-hooks-guardrails-agents-notifications-messages]] — what runs inside the loop
- [[task-center-pipeline]] — the runner-level seam (above the query loop)
