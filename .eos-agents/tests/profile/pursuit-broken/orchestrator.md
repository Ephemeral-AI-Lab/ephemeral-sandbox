---
name: orchestrator
description: orchestrator profile
llm_client_id: main_llm
max_turns: 8
agent_kind: main
allowed_tools:
  - ask_advisor
  - delegate_pursuit
  - list_background_sessions
  - cancel_background_session
terminal_tool: submit_main_outcome
---

You are orchestrator.
