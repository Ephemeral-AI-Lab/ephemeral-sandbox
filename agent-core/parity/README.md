# eos-parity — Phase-0 parity corpus

Frozen Python truth, captured **before** any port, so Phases 1–4 have a fixed
comparison target. Regenerate the Python-derived artifacts with:

```sh
uv run python agent-core/parity/_capture/capture.py
```

The committed files under `schemas/`, `sqlite/`, and `prompt_report/` are the
real artifacts; `_capture/capture.py` is kept only for provenance. The `sse/`
fixtures are raw provider wire format authored by hand (not Python-derived).

## What is captured

| Dir | Artifact | Source of truth |
|---|---|---|
| `schemas/` | `model_json_schema()` for `Message` + the five content blocks + `TextToolOutput` | `backend/src/message/message.py`, `backend/src/tools/_framework/core/results.py` |
| `sqlite/schema.sql` | canonical `sqlite_master` for the seven target tables (clean `create_all`) | `backend/src/db/models/` |
| `sse/openai/*.sse` | OpenAI Responses SSE (text + tool-call) | wire format mirrored from `test_codex_event_translation.py` |
| `sse/anthropic/*.sse` | Anthropic Messages SSE (text + tool-call) | Anthropic streaming wire format |
| `prompt_report/session_golden.jsonl` | the three `PromptReportRecorder` event kinds | `backend/src/prompt/prompt_report_recorder.py` |
| `prompt_report/initial_messages_anomaly.json` | the `system`-role anomaly, frozen | `engine.agent.lifecycle._initial_message_records` |

## Annotated anomaly — the `system`-role transcript bug (anchor §4)

`prompt_report/session_golden.jsonl` is the **faithful** recorder output: the
`llm_request` event keeps `system_prompt` as a *separate request field* and the
`messages` array holds only `role: user | assistant` entries (the `Message`
model's `role` is `Literal["user","assistant"]` — a `system` role is
structurally impossible there).

The actual bug lives one transcript over, in
`engine.agent.lifecycle._initial_message_records`, which materializes the system
prompt as a raw `{"role": "system", "content": [...]}` record persisted into
`agent_runs.initial_messages`. `prompt_report/initial_messages_anomaly.json` is
that function's real output, frozen here. The Rust port **fixes** this (anchor
§4): the system prompt stays a request field and is never emitted as a
`Message`. Keeping the bug in the fixture makes that fix a visible, reviewed diff
in a later phase rather than an accidental silent change.

The live recorder also prepends a wall-clock `ts` float at write time; it is
omitted from the golden for determinism.

## Deferred (not capturable as Pydantic schema in Phase 0)

- **Sandbox request/result DTOs** (`SandboxCaller`, `SandboxRequestBase`,
  `SandboxResultBase`, `ToolCallRequest`, …) are frozen `@dataclass`es, **not**
  Pydantic models, so they have no `model_json_schema()`. Their schema contract
  is owned by `eos-sandbox-api` and asserted in that crate's phase, not here.
- **`ToolSpec` goldens.** `create_default_tool_registry()` returns an empty
  registry — model-facing tools bind per agent with sandbox context — so there is
  no dependency-light, representative `ToolSpec` to freeze in Phase 0. The
  `{name, description, input_schema, output_schema}` contract is owned by
  `eos-llm-client` / `eos-tools` and pinned in their phases. `Message` + the
  content blocks already satisfy AC-workspace-05.
