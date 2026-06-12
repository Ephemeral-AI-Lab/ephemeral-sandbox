---
name: worker
description: worker profile
llm_client_id: worker_llm
max_turns: 8
agent_kind: worker
allowed_tools:
  - ask_advisor
terminal_tool: submit_worker_outcome
pursuit_context_script: .eos-agents/tests/pursuit/scripts/worker.cjs
---

You are worker.
