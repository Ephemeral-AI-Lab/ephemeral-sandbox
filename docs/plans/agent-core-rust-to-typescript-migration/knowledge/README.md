# Claude Code Source Study — Knowledge Index

Source studied: `/Users/yifanxu/machine_learning/LoVC/c c/src` (extracted
Claude Code TypeScript source). All docs are observations with file:line
anchors plus EOS migration takeaways.

| Doc | Covers |
| --- | --- |
| `claude-code-tech-stack.md` | Observed dependency/stack signals, lifecycle lessons, recommended EOS state ownership |
| `agent-loop-and-components.md` | `query()`/`queryLoop()` async-generator turn lifecycle, recovery continue-sites, main components, tool concurrency |
| `compaction.md` | Auto/manual/reactive compaction triggers and thresholds, microcompact variants, post-compact rebuild and cleanup |
| `background-task-tracking.md` | Task data model, registration, disk-backed output offsets, completion notification, eviction rules |
| `abort-and-interrupt-handling.md` | AbortController tree, abort-reason semantics, what survives an interrupt (background tasks, subagents, todos), synthetic tool_results |
| `message-steering.md` | Prioritized command queue, mid-turn vs idle drains, steer-vs-abort asymmetry, subagent inboxes |
| `event-stream-and-sse.md` | Inbound provider SSE consumption, internal event union, outbound SDKMessage surface, side event queue |
