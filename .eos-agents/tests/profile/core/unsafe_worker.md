---
name: unsafe_worker
description: unsafe_worker profile
llm_client_id: unsafe_llm
max_turns: 8
agent_kind: worker
allowed_tools: []
terminal_tool: submit_worker_outcome
pursuit_context_script: .eos-agents/tests/pursuit/scripts/context.cjs
---

You are unsafe_worker.
