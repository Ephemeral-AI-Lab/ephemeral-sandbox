---
name: planner
description: planner profile
llm_client_id: planner_llm
max_turns: 8
agent_kind: planner
allowed_tools:
  - ask_advisor
terminal_tool: submit_planner_outcome
pursuit_context_script: .eos-agents/tests/pursuit/scripts/broken-planner.cjs
---

You are planner.
