# Architecture Comparison: EphemeralOS vs OpenHarness

> Comparison of agentic capabilities between EphemeralOS and [OpenHarness](https://github.com/HKUDS/OpenHarness) (open-source Claude Code reimplementation).
>
> Generated: 2026-04-07

| Capability | EphemeralOS | OpenHarness | Edge |
|---|---|---|---|
| **Agent Lifecycle** | Ephemeral per-turn; agents spawn, execute, die. Session state persists in DB separately. | Persistent engine loop; QueryEngine lives for session duration, accumulates context. | **EphemeralOS** — cleaner isolation for multi-agent |
| **Multi-Agent Orchestration** | Coordination planner/worker toolkits, subagent dispatch, pipeline module with checkpoints, BackgroundTaskManager | Coordinator mode (env-var activated), swarm layer with subprocess spawning, filesystem mailbox IPC, git worktree isolation | **Tie** — EphemeralOS has richer primitives; OpenHarness has mature swarm IPC |
| **Tool Execution Model** | Mid-stream execution via StreamingToolExecutor (tools start before LLM finishes), `[CANCEL:tool_id]` abort, streaming progress events | Standard post-response execution, `asyncio.gather()` for multi-tool parallelism, pre/post hook pipeline | **EphemeralOS** — mid-stream + cancel is a significant latency/control advantage |
| **Tool Architecture** | `@tool` decorator auto-generates Pydantic models from signatures + docstrings; `ToolkitFactory` gives role-specific tool sets per agent | 43+ built-in tools mirroring Claude Code; flat `ToolRegistry` namespace; same Pydantic pattern | **EphemeralOS** — toolkit factory enables role-scoped tooling |
| **Sandbox / Code Execution** | Daytona SDK — full lifecycle (create/start/stop/delete), LSP integration (type hints, go-to-def, find-refs), daytona_shell tool, credential management | `srt` CLI wrapper — bubblewrap (Linux) / sandbox-exec (macOS), filesystem ACLs, network domain filtering, graceful degradation | **EphemeralOS** — IDE-grade code intelligence vs security-boundary-only |
| **Code Intelligence** | LSP-powered: type hints, definitions, references, diagnostics | Basic file read/write/edit/grep | **EphemeralOS** |
| **Pipeline / Workflow** | First-class `pipeline` module with `PipelineRun`, `StepRecord`, checkpoints, resume capability | Not present | **EphemeralOS** |
| **LLM Provider Breadth** | 2 backends: AnthropicClient (native + thinking), OpenAICompatibleClient | 15+ providers: Anthropic, OpenAI, Copilot, Groq, DashScope, Ollama, vLLM, Bedrock, Vertex AI, OpenRouter | **OpenHarness** |
| **Provider Switching** | Model registration DB with priority resolution (agent → DB → settings) | Runtime `oh provider use <profile>`, three-tier auto-detection | **OpenHarness** — more flexible runtime switching |
| **Cross-Session Memory** | DB-backed session/agent-run/token persistence (session-scoped) | Filesystem-based persistent memory with scan + search (cross-session) | **OpenHarness** — agents learn across sessions |
| **Token Management** | `TokenUsageRecord` ORM, per-turn tracking, auto-compaction via `SessionState` | `CostTracker` accumulates across turns, auto-compaction before API calls | **Tie** |
| **Deployment Surface** | FastAPI server with SSE/WebSocket streaming, REST API, serves SPA frontend | CLI + React TUI + 10 chat platform adapters (Slack, Discord, Telegram, WhatsApp, DingTalk, Feishu, Matrix, QQ, Email) | **OpenHarness** — massively broader |
| **Permission / Safety** | Pre/post tool hooks (can block execution) | Three modes (FULL_AUTO, PLAN, DEFAULT), path-glob + command-pattern rules | **OpenHarness** — more granular permission model |
| **Extensibility: Plugins** | Via toolkit factory pattern | Full plugin system with `plugin.json` manifest, contributes skills/agents/hooks/MCP/commands | **OpenHarness** |
| **Extensibility: MCP** | Not present | Stdio MCP client, dynamic tool/resource discovery | **OpenHarness** |
| **Extensibility: Hooks** | Pre/post tool event hooks | 4 types (command, HTTP, prompt, agent) + hot-reload via watchfiles | **OpenHarness** |
| **Skills System** | `SkillDefinition` with DB store, registry, loader | Markdown with YAML frontmatter, bundled + user + plugin sources | **Tie** |
| **Prompt Engineering** | `build_runtime_system_prompt` with environment info, agent capabilities, skill descriptions, session context | `system_prompt.py` with CLAUDE.md convention, context injection, environment info | **Tie** |
| **Streaming Architecture** | Fine-grained event types: ThinkingDelta, TextDelta, ToolExecutionStarted/Completed/Progress/Cancelled, BackgroundTask events | StreamEvent generators, similar granularity | **EphemeralOS** — richer event taxonomy (progress, cancel, background) |
| **Database Layer** | SQLAlchemy ORM, connection pooling, graceful degradation (works without DB), auto-migration | Filesystem-based session storage service | **EphemeralOS** — structured, queryable persistence |
| **Agent Definitions** | YAML/Markdown with frontmatter: model, tool_call_limit, effort, skills[], toolkits[], hooks, background flag | Agent definitions in coordinator, less structured | **EphemeralOS** — more declarative agent specs |
| **Background Execution** | First-class `BackgroundTaskManager`, progress streaming, periodic system reminders for pending tasks | `tasks/` module with local agent + shell task types | **EphemeralOS** — tighter integration with agent loop |
