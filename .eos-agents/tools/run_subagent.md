---
intent: write_allowed
terminal: false
hooks: []
---
Spawn a registered subagent as a supervised async session. You hand it `prompt` as
its only input. It must finish by calling its terminal tool; whatever that
terminal tool emits becomes your result.

Use this when:
- You need to delegate a focused, context-isolated investigation (e.g.,
  "where is X used across the repo?") so your context isn't polluted by
  intermediate tool output.
- You can launch multiple independent investigations and want to run them
  in parallel — fire them all in a single message with multiple
  `run_subagent` calls.

Do NOT use for:
- Work you'd handle in 1–3 of your own tool calls — direct execution
  beats subagent overhead.
- Spawning further subagents from inside a subagent — that path is
  rejected at validation time. Handle the work directly, or submit your
  findings via your own terminal tool.
- Tasks that need shared context with you — the subagent does NOT
  inherit your conversation. The `prompt` is the only channel.

Writing the prompt:
Brief the subagent like a smart colleague who just walked into the room.
It hasn't seen your conversation, doesn't know what you've tried, doesn't
understand why the task matters.
- Explain what you're trying to accomplish and why.
- Include the exact paths, symbol names, or commands you'd run yourself.
- Specify what's in scope and what's out of scope.
- Tell it what shape of answer you want ("report in under 200 words",
  "list the file paths").
- Terse command-style prompts produce shallow, generic work.

Don't peek. The launch returns an `agent_run_id`; the subagent runs
asynchronously. Don't poll or inspect its transcript — that defeats the
point of forking off its tool noise. You'll be notified when it completes.

Don't race. After launching, you know nothing about what the subagent
will find. Never predict its result in any format. If the user asks a
follow-up before completion, give status, not a guess.

Capabilities and constraints:
- The launch returns immediately with an `agent_run_id`.
- Cancel with `cancel_subagent(agent_run_id=...)`.
- A subagent that exits without calling a terminal tool is marked
  failed.

Launch output:
- On success: `[SUBAGENT LAUNCHED]` with `agent_run_id`, `status=running`,
  and `agent_name`.
- Metadata includes `agent_run_id`, `status`, and `agent_name`.
- Completion is delivered later as a typed `[BACKGROUND COMPLETED]`
  notification; terminal metadata such as `subagent_terminal_called`
  belongs to that completion result, not the launch.

Example:
  run_subagent(
    agent_name="subagent",
    prompt=(
      "Find every call site of "
      "`AttemptRun.record_worker_outcome` in agent-core/crates/eos-workflow "
      "and report (file, line, calling function). The signature is "
      "changing in PR #842; I need the punch list of files to update. "
      "Report under 200 words."
    ),
  )
