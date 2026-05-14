---
title: "Tools, Hooks, Guardrails, Agents, Notifications, Messages"
tags: ["tools", "hooks", "guardrails", "agents", "notifications", "messages", "stream-events", "prompt-report-recorder", "live-e2e", "see-also"]
created: 2026-05-10T11:51:25.488Z
updated: 2026-05-10T11:58:13.426Z
sources: []
links: ["live-e2e-testing-framework-design.md", "engine-query-loop-llm-seam.md", "task-center-pipeline.md"]
category: architecture
confidence: medium
schemaVersion: 1
---

# Tools, Hooks, Guardrails, Agents, Notifications, Messages

_Source: explore agent draft, 2026-05-10. See `.omc/wiki-draft/tools-and-agents.md`._

## Tools

### tools/core

- **`BaseTool`** (`tools/core/base.py:27`) — abstract base with `name`, `description`, `input_model`, `output_model`, `background` policy (`"forbidden"/"optional"/"always"`), `is_terminal_tool`, `pre_hooks`, `post_hooks`, `context_requirements`; subclasses implement `execute(arguments, context) -> ToolResult`.
- **`@tool` decorator** (`tools/core/decorator.py:45`) — converts `async def` into `BaseTool`; supplies `input_model`, `output_model`, `is_terminal_tool`, `pre_hooks`, `post_hooks` at decoration time; validates hook targets.
- **`ToolRegistry`** (`tools/core/registry.py:12`) — name→`BaseTool` dict with `register`, `register_many`, `get`, `list_tools`, `remove_tools`, `restrict_to_tools`, `to_api_schema`.
- **`ExecutionMetadata`** (`tools/core/runtime.py:21`) — typed dataclass (mapping interface) carrying `sandbox_id`, `agent_run_id`, `agent_name`, `task_center_*`, `composer`, `conversation_messages`, `tool_registry`, `system_notification_service`, `background_task_manager`, plus `extras`.
- **`ToolExecutionContextService`** (`tools/core/context.py:15`) — wrapper exposing `cwd: Path`; `context.notify_system(text)` to emit notifications.
- **`ToolResult`** (`tools/core/results.py:12`) — frozen dataclass `{output, is_error, metadata, does_terminate}`; `does_terminate` stamped by `execute_tool_once` on terminal-tool success.
- **Hook protocol** (`tools/core/hooks.py:66,79`) — `ToolPreHook.run(tool_input, context) -> HookResult[Any]`; `ToolPostHook.run(tool_input, result, context) -> HookResult[ToolResult]`; `HookResult` carries `status` (`"pass"/"fail"`), optional replacement `value`, `reason`/`message`/`metadata`.
- **`validate_hook_targets`** (`tools/core/hooks.py:102`) — enforces hook-tool binding at registration.

### tools/execution

**`execute_tool_once`** (`tools/execution/tool_call.py:179`) — single function for one tool end-to-end. Order: `parse_tool_input` → `run_pre_hooks` → emit `ToolExecutionStarted` → `execute_tool_body` → `validate_tool_output` → `run_post_hooks` → `finalize_result` → stamp `does_terminate` if terminal.

**`execute_tool_call_streaming`** (`tools/execution/tool_call.py:107`) — top-level entry consuming budget via `_consume_tool_budget_or_reject`, resolves tool, builds metadata, calls `execute_tool_once`, wraps in `ToolResultBlock`.

**`ToolHookExecutionHelper`** (`tools/execution/hook_runner.py:29`) — sequential pre/post hook loop; on hook failure emits structured JSON `hookSpecificOutput`; accumulates `hook_trace` in result metadata; manages `SystemNotificationService` lifetime.

### tools/sandbox_toolkit

REAL operations inside the live sandbox. All `@tool`-decorated.

- `shell` (`shell.py:121`) — runs shell commands in the foreground only.
- `read_file` (`read_file.py:24`)
- `write_file` (`write_file.py:23`)
- `edit_file` (`edit_file.py:62`) — structured patch edits (search/replace blocks).

### tools/builtins

**Background** (`builtins/background/`):
- `check_background_task_result` (`check_background_task_result.py:71`)
- `wait_background_tasks` (`wait_background_tasks.py:46`)
- `cancel_background_task` (`cancel_background_task.py:21`)

**Skills** (`builtins/skills/`):
- `load_skill` (`load_skill.py:27`) — injects SKILL.md system prompt
- `load_skill_reference` (`load_skill_reference.py`)

### tools/subagent

**`run_subagent`** (`subagent/run_subagent.py:1`) — dispatches focused worker subagent as background task (`background="always"`); subagent must exit via terminal tool; recursion rejected at validation.

### tools/submission

Terminal-tool family driving task_center lifecycle. All `is_terminal_tool=True`; success stamps `does_terminate=True`, loop exits `TOOL_STOP`.

**main_agent/planner**
- `submit_full_plan` (`submit_full_plan.py:26`)
- `submit_partial_plan` (`submit_partial_plan.py:34`)

**main_agent/generator**
- `submit_execution_handoff` (`submit_execution_handoff.py:42`) — generator pre-edit terminal; the `request_mission_after_edit` notification reminder nudges the generator to finish through its own success/failure once edits have begun
- `submit_execution_success` / `submit_execution_failure` (`generator/executor/`)
- `submit_verification_success` / `submit_verification_failure` (`generator/verifier/`)

**main_agent/evaluator**
- `submit_evaluation_success` / `submit_evaluation_failure`

**helper_agent/advisor**
- `ask_advisor` (`ask_advisor.py:41`) — non-terminal
- `submit_advisor_feedback` (`submit_advisor_feedback.py:22`) — terminal

**helper_agent/resolver**
- `ask_resolver` — non-terminal
- `submit_resolver_result` — terminal

**subagent**
- `submit_exploration_result` (`subagent/explorer.py:20`) — terminal

## Hooks

### Hook protocol
`tools/core/hooks.py`. `ToolPreHook.run(tool_input, context) -> HookResult[Any]` may replace input via `HookResult.pass_(new_input)` or block with `HookResult.fail(reason)`. `ToolPostHook.run(tool_input, result, context) -> HookResult[ToolResult]` may replace result.

Pre-hooks run sequentially before execution; `fail` short-circuits and returns hook-failure `ToolResult` (JSON with `hookSpecificOutput`). Post-hooks run after; `fail` replaces result. Hook trace accumulates in `result.metadata["hook_trace"]`.

### Submission gate hooks

None. Submission tools no longer wire any pre-hooks. Caller-role, attempt-open, and profile-vs-terminal checks live elsewhere: structural role / open-attempt checks fail inside `resolve_attempt_submission_context` as `AttemptSubmissionContextError`; profile-vs-terminal separation is enforced by each `AgentDefinition.terminals` whitelist. Behavioral nudges (resolver-loop saturation, mission-after-edit) are delivered via notification triggers instead, so the agent retains the choice.

## Notification triggers

`NotificationRule` factories fired from inside tool execution (triggers run at top of each model turn via `dispatch_rules`).

- **`make_resolver_limit_reminder`** (`resolver_limit.py:11`) — fires when `unresolved_resolver_call_count(messages) >= 4`; rule name `"resolver_limit"`, `fire_once=True`
- **`make_mission_request_after_edit_reminder`** (`request_mission_after_edit.py`) — fires when the generator's transcript already contains a `write_file`/`edit_file`/`shell` tool use; rule name `"request_mission_after_edit"`, `fire_once=True`

Both assembled into `AgentDefinition.notification_rules` at agent launch time.

## Notification rules subsystem

**`dispatch_rules`** (`notification/rules/dispatch.py`) — evaluates all `NotificationRule` instances once per model turn; calls `rule.trigger(messages, context)`, then `rule.body(messages, context)`, then `service.notify_system(text)`; deduplicates via `context.notification_fired: set[str]`.

**`SystemNotificationService`** (`notification/runtime.py`) — run-scoped sink; `notify_system(text)` appends `SystemNotificationBlock` and emits `SystemNotification` event; `flush_events()` drains pending events; `pop_pending_notifications()` drains transcript-bound blocks.

**Call sites** (`engine/query/loop.py`):
- Line 316: `await dispatch_rules(rules, messages, context, service)` — top of every turn
- Lines 271, 294, 345: `flush_system_notification_events(notification_service)` — turn boundaries

**Built-in rule factories** (`notification/rules/factories.py`):
- `make_opening_reminder(rules_text)` — first turn only; `fire_once=True`
- `make_budget_warning(thresholds=(0.50, 0.75, 0.90))` — fires at each budget fraction crossed; managed via `context.notification_state["budget_warning"]`

## Agent definitions

### `AgentDefinition` (`agents/definition/model.py:62`)

| Field | Type | Purpose |
|---|---|---|
| `name` | `str` | Registry key |
| `description` | `str` | UI label |
| `system_prompt` | `str \| None` | `.md` body |
| `model` | `str \| None` | LLM override |
| `tool_call_limit` | `int \| None` | Per-run cap |
| `skills` | `list[str]` | Skill ids |
| `background` | `bool` | Background mode |
| `role` | `str \| None` | Read by gate hooks via `context.get("role")` |
| `permissions` | `list[str]` | Permission list |
| `agent_type` | `"agent"\|"subagent"` | Regular vs worker |
| `allowed_tools` | `list[str]` | Tool names |
| `terminals` | `list[str]` | Terminal tool subset |
| `notification_triggers` | `list[str]` | Trigger ids → `NotificationRule` |
| `notification_rules` | `list[AgentNotificationRule]` | Evaluated each turn |
| `context_recipe` | `str \| None` | ContextComposer recipe id |
| `variants` | `list[AgentVariant]` | Capability variants (first-match-wins) |

### Loading
- `load_agents_dir(directory)` (`loader.py:37`) — non-recursive `.md` load; YAML frontmatter + Markdown body
- `load_agents_tree(directory)` (`loader.py:44`) — recursive (`rglob("*.md")`)
- `register_definition(defn)` (`registry.py:14`)
- `get_definition(name)` (`registry.py:24`)
- `list_dispatchable_subagent_names()` (`registry.py:34`)

### Validation
- `validate_agent_definitions_resolved()` (`resolved_validation.py:9`) — verifies `context_recipe` registered, each `variant.when` predicate registered, `variant.use` exists with no own variants, variant recipes registered. Raises `AgentDefinitionValidationError`.
- `AgentDefinitionValidator` (`tool_validation.py:27`) — validates `allowed_tools`/`terminals` against live `ToolRegistry`.

### Profiles
Profiles live under `backend/src/agents/profile/{main,helper,subagent}/*.md`. The canonical category is `agent_kind: AgentKind` on `AgentDefinition` (one of `planner / executor / verifier / evaluator / advisor / explorer / resolver`), surfaced to gate hooks via `context.get("role")` — the key stays `"role"` for audit-consumer compat but the value is now `agent_def.agent_kind.value`. A separate `dispatchable_by_planner: bool` flag gates which profiles a planner submission may name as `agent_name`.

## Messages and stream events

### `ConversationMessage` + content blocks (`message/messages.py`)

`ConversationMessage` (line 123): `role: Literal["user", "assistant"]`, `content: list[ContentBlock]`.

| Block | Line | Purpose |
|---|---|---|
| `TextBlock` | 11 | Plain text |
| `ThinkingBlock` | 27 | Chain-of-thought (excluded from `to_api_param`) |
| `ToolUseBlock` | 18 | LLM tool call request (`id`, `name`, `input`) |
| `ToolResultBlock` | 34 | Tool result (`tool_use_id`, `content`, `is_error`, `does_terminate`) |
| `SystemNotificationBlock` | 48 | Engine-generated `<system-reminder>` |

`serialize_content_block` (line 191) converts to provider wire format; `ThinkingBlock` excluded from `to_api_param`.

### Stream events (`message/stream_events.py`)

Emitted OUT by run loop and tool executor — distinct from provider's `ApiStreamEvent` (`providers/types.py`).

| Event | Line | Emitted when |
|---|---|---|
| `AssistantTextDelta` | 32 | Streamed text chunk |
| `ThinkingDelta` | 23 | Streamed thinking chunk |
| `AssistantMessageComplete` | 41 | Full assistant message + `UsageSnapshot` |
| `ToolExecutionStarted` | 51 | About to execute tool body |
| `ToolExecutionCompleted` | 62 | Tool finished |
| `ToolExecutionProgress` | 76 | Long-running tool partial output |
| `ToolExecutionCancelled` | 92 | Cancelled by LLM signal |
| `BackgroundTaskStarted` | 103 | Tool dispatched as background |
| `SystemNotification` | (`notification/runtime.py`) | Notification emitted |

`StreamEvent` union (line 125) = all of the above.

## PromptReportRecorder

**`PromptReportRecorder`** (`prompt/prompt_report_recorder.py:15`) — appends JSONL events with monotonic `seq` via `append_prompt_report_event`. Constructed lazily by `prompt.prompt_report_recorder.recorder_for_context` from `metadata["prompt_report_messages_path"]`.

Three event types per turn:
1. `"llm_request"` — `{event, seq, system_prompt, messages, tools}`
2. `"assistant"` — `{event, seq, message, usage}`
3. `"tool_results"` — `{event, seq, tool_results}`

**This is the canonical capture mechanism for replay**: the `{llm_request, assistant, tool_results}` triple per turn gives everything needed to replay with a stubbed `stream_message` — feed the captured `assistant` message back instead of calling the API.

## Tool budgets, max-step, terminal-tool exclusivity

**Tool budget** (`tools/execution/tool_call.py:59`): `_consume_tool_budget_or_reject(context, tool_name, tool_use_id)` — checks `context.tool_calls_used >= context.tool_call_limit`; terminal tools exempt; last slot reserved exclusively for terminals when `terminal_tools` non-empty.

**Max-step** (`engine/query/loop.py:280`): `context.tool_call_limit` set from `AgentDefinition.tool_call_limit` at factory (`engine/agent/factory.py:335`); checked at line 280 each turn.

**Terminal-tool exclusivity**: `BaseTool.is_terminal_tool` + `StreamingToolExecutor` defer; `context.terminal_tools` from agent definition's `terminals` list. See engine-query wiki for full defer logic.

## What the live-e2e framework needs

### Public surface

| Symbol | File |
|---|---|
| `load_agents_dir` / `load_agents_tree` | `agents/definition/loader.py` |
| `register_definition` / `get_definition` | `agents/definition/registry.py` |
| `validate_agent_definitions_resolved` | `agents/definition/resolved_validation.py` |
| `AgentDefinitionValidator` | `agents/definition/tool_validation.py` |
| `ToolRegistry` | `tools/core/registry.py` |
| `execute_tool_once` / `execute_tool_call_streaming` | `tools/execution/tool_call.py` |
| `ConversationMessage`, `ToolUseBlock`, `ToolResultBlock` | `message/messages.py` |
| `dispatch_rules` | `notification/rules/dispatch.py` |
| `SystemNotificationService` | `notification/runtime.py` |
| `flush_system_notification_events` | `notification/runtime.py` |
| `PromptReportRecorder` | `prompt/prompt_report_recorder.py` |

### Real vs fake
- **REAL**: all tools (`execute_tool_once`), gate hooks, notification triggers and `dispatch_rules`, agent definitions loaded from `.md`, `ToolRegistry`, `SystemNotificationService`.
- **FAKE**: only `SupportsStreamingMessages.stream_message` — replaced by replaying captured `"assistant"` event from `PromptReportRecorder` JSONL.

### What to test
| Concern | Where it lives |
|---|---|
| Submission guardrails | `tools/submission/context.py` (`resolve_attempt_submission_context` → `AttemptSubmissionContextError`); each `AgentDefinition.terminals` whitelist; notification triggers under `tools/submission/notification_triggers/` |
| Pre/post hook lifecycle | `tools/execution/hook_runner.py:ToolHookExecutionHelper` |
| Max-step enforcement | `engine/query/loop.py:280` + `tool_call.py:59` |
| Terminal tool submission (`does_terminate`) | `tool_call.py:211`, loop `TOOL_STOP` at `loop.py:276` |
| System notification dispatch | `notification/rules/dispatch.py:dispatch_rules` + `loop.py:316` |
| Planner validation | `tools/submission/main_agent/planner/_schemas.py` + planner submission input schemas |

### Replay artifact format
`PromptReportRecorder` writes one JSONL event per `record()` call. Each turn produces three events with same `seq`:
```
{"event":"llm_request","seq":N,"system_prompt":"...","messages":[...],"tools":[...]}
{"event":"assistant","seq":N,"message":{...},"usage":{...}}
{"event":"tool_results","seq":N,"tool_results":[...]}
```
The replay harness reads `"assistant"` events (keyed by `seq`) and replays them instead of calling the LLM API. The `"llm_request"` events serve as ground-truth for verifying the replay sees the same prompt the original run saw.

---

## Update (2026-05-10T11:58:13.426Z)

## See also

- [[role-planner]], [[role-generator]], [[role-evaluator]] — submission contracts per role
- [[engine-query-loop-llm-seam]] — where these run
- [[task-center-pipeline]] — what terminal tools drive
