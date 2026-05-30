# Module `prompt` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/prompt/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**1 classes across 1 files.**

The `prompt` module assembles the runtime system prompt for an agent run and captures a per-run debug record of the conversation. Its prompt-assembly helpers (`build_runtime_system_prompt`, which layers the configured system prompt with a fast-mode section, and `build_termination_condition_prompt`, which emits the one-way terminal-tool warning block) live in `runtime_prompt.py`. Its single class, `PromptReportRecorder`, appends JSONL "prompt report" events (llm_request, assistant, tool_results) with a monotonically increasing per-context sequence number, enriched by a shared base event (agent run id, agent name, model); the `recorder_for_context` factory lazily builds and caches one recorder on the `QueryContext` from its tool metadata, writing only when a report path is configured.

## Contents

- **`prompt/prompt_report_recorder.py`** — `PromptReportRecorder`

---

## `prompt/prompt_report_recorder.py`

#### `PromptReportRecorder`  ·  _class_  ·  [L20]

Append prompt-report events with a monotonically increasing sequence.

**Instance attributes**: `_path`, `_base_event`, `_seq`

<details><summary>Methods (6)</summary>

`__init__`, `next_seq`, `record`, `record_llm_request`, `record_assistant`, `record_tool_results`

</details>

