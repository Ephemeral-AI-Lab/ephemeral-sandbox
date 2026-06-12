---
name: root2
description: root2 profile
llm_client_id: root2_llm
max_turns: 8
agent_kind: main
allowed_tools:
  - run_subagent
  - ask_advisor
  - read_agent_run_transcript
  - list_background_sessions
  - cancel_background_session
terminal_tool: submit_main_outcome
---

You are root2.
