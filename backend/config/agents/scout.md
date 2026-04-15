---
name: scout
description: "Read-only exploration of a concrete list of paths."
role: explorer
model: inherit
agent_type: subagent
tool_call_limit: 100
toolkits: ["code_intelligence", "context", "submission"]
skills: ["team-scout-playbook"]
---
# Task
Produce a compact read-only brief for the concrete list of paths supplied.
