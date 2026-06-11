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
| `tool-definition-and-registry.md` | `Tool` contract, runtime metadata predicates, `buildTool` fail-closed defaults, registry/pool assembly, `ToolUseContext` |
| `tool-execution-pipeline.md` | Per-tool pipeline stages, batch partitioning + concurrency cap, `StreamingToolExecutor`, sync vs async tool shapes |
| `tool-hooks.md` | Hook events/definition shapes, runner mechanics (stdin JSON, exit codes, parallelism), PreToolUse permission precedence, PostToolUse output rewrite, async hooks |
| `background-task-spawn-and-cancellation.md` | Foreground→background promotion, per-type kill paths, signal/exit-code semantics, interrupt-survival matrix, TaskStop |
| `agent-run-concurrency.md` | Run inventory (main/sync/async/teammate/SDK), QueryGuard single-main-run invariant, queue-vs-steer-vs-abort on new input, spawn lanes + promotion restart, task-notification re-entry, concurrency limits |
| `subagent-state-management.md` | `createSubagentContext` shared-vs-isolated field table, per-agent AppState views and keyed slices, sidechain transcripts/resume/fork, abort topology, inboxes, cleanup ledger, agentType-keyed memory |
| `task-list-tools-and-notifications.md` | TaskCreate/Get/List/Update tool inventory and storage model, TaskCreated/TaskCompleted hook veto points, silent-create + push-on-assign mailbox policy, turn-counter stale-task reminder (10/10), idle-loop auto-claim and tasks-mode watcher |
| `notification-injection-and-message-merging.md` | Attachment→`<system-reminder>` normalization, adjacent-user-message merging (hoist/seam rules), the #21049 smoosh of reminder text into `tool_result`, steering exemption, EOS 04.9 separate-messages comparison and revisit trigger |
| `ask-user-question-tool.md` | AskUserQuestion schema, answer-injection via permission `updatedInput` (no answer tool), preview formats, main-thread-only availability matrix, plan-mode coupling |
| `inter-agent-messaging-protocol.md` | SendMessage signature + routing decision tree, mailbox wire shape and pollers, protocol messages (shutdown/plan-approval/permissions, anti-forgery), per-role tool matrix, teammate colors, cross-session UDS/bridge addressing |
