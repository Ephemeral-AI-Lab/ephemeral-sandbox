---
name: scribe
description: scribe profile
llm_client_id: scribe_llm
max_turns: 8
agent_kind: worker
allowed_tools:
  - read_note
  - write_note
  - ask_advisor
terminal_tool: submit_worker_outcome
pursuit_context_script: .eos-agents/tests/pursuit/scripts/context.cjs
---

You are scribe.
