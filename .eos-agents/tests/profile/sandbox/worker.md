---
name: worker
description: worker profile
llm_client_id: worker_llm
max_turns: 8
agent_kind: worker
allowed_tools:
  - read
  - multi_read
  - write
  - edit
  - exec_command
  - command_stdin
  - read_command_transcript
  - list_background_sessions
  - cancel_background_session
  - ask_advisor
  - run_subagent
terminal_tool: submit_worker_outcome
pursuit_context_script: .eos-agents/tests/pursuit/scripts/context.cjs
---

You are worker.
