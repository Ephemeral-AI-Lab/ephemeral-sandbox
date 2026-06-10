# Claude Code Background Task Tracking

Status: Observed
Date: 2026-06-10
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript migration reference

## Data Model

`Task.ts` defines the typed core:

- `TaskType`: `'local_bash' | 'local_agent' | 'remote_agent' |
  'in_process_teammate' | 'local_workflow' | 'monitor_mcp' | 'dream'`
  (`Task.ts:6`).
- `TaskStatus`: `'pending' | 'running' | 'completed' | 'failed' | 'killed'`,
  with `isTerminalTaskStatus()` guarding eviction, message injection into
  dead teammates, and orphan cleanup (`Task.ts:27`).
- `TaskStateBase` (`Task.ts:45`): `id`, `type`, `status`, `description`,
  `toolUseId`, `startTime`/`endTime`, `outputFile`, `outputOffset`,
  `notified`.
- Task IDs are `[type-prefix][8 base36 chars]` (`Task.ts:98`) — prefix per
  kind (`b` bash, `a` agent, `t` teammate, ...), random enough to resist
  symlink-name guessing in the shared temp dir.
- Each kind extends the base with its own state union member
  (`tasks/types.ts:12`): shell tasks carry the live `ShellCommand` handle,
  agent tasks carry `abortController`, `messages`, `retain`, `evictAfter`,
  teammates carry identity, idle flags, and `pendingUserMessages`.

Runtime-only handles (`AbortController`, cleanup callbacks, process refs)
live alongside plain fields in the same state object but are nulled at
terminal transitions.

## Lifecycle and Ownership

```
spawn (tool call: background Bash / Agent / teammate)
  ├─ create TaskState (status: 'running')
  ├─ registerTask(task, setAppState)            framework.ts:77
  │    ├─ stored in AppState.tasks[taskId]      (single live map)
  │    ├─ re-register merges retained UI state  (resume case)
  │    └─ emits 'task_started' SDK event once
  ├─ task runs; output appended to disk file
  ├─ completion owned by the task type itself
  │    ├─ status → completed/failed/killed
  │    ├─ runtime handles nulled
  │    └─ enqueue*Notification() → command queue
  └─ eviction after terminal + notified (+ grace)
```

The live registry is `AppState.tasks: Record<string, TaskState>`
(`state/AppStateStore.ts:160`). A small polymorphic registry
(`tasks.ts:22-39`) maps `TaskType` to its module, each implementing
`kill(taskId, setAppState)`.

A deliberate design note (`framework.ts:199`): the generic poller does NOT
notify completions — each task type fires its own completion notification
exactly once, to avoid dual delivery. The `notified` flag is set atomically
before enqueueing.

## Output Tracking

- Output goes to a per-task disk file
  `<projectTmp>/<sessionId>/tasks/<taskId>.output`
  (`utils/task/diskOutput.ts`), opened with `O_NOFOLLOW`/`O_EXCL` to block
  symlink attacks; writes go through an async append queue capped at 5 GB.
- Agent tasks symlink their output file to the subagent transcript instead
  of duplicating it (`initTaskOutputAsSymlink`).
- The model reads increments via `outputOffset`:
  `getTaskOutputDelta(taskId, fromOffset)` returns `{ content, newOffset }`
  (8 MB read cap); tail reads prefix an omitted-bytes count.
- Before each API request, `generateTaskAttachments(state)`
  (`framework.ts:158`) reads deltas for running tasks and returns patches
  (`updatedTaskOffsets`, `evictedTaskIds`) — not a full state snapshot — so
  the caller merges against fresh state after the async reads, avoiding
  clobbering concurrent terminal transitions.

## How the Loop Learns About Completion

Completion notifications are injected into the conversation through the same
queued-command machinery as user input:

1. Task completes → `enqueueShellNotification()` /
   `enqueueAgentNotification()` formats an XML `<task_notification>` block
   (task id, output file, status, summary, usage) and calls
   `enqueuePendingNotification({ mode: 'task-notification',
   priority: 'later' })` (`framework.ts:274`,
   `messageQueueManager.ts:127`).
2. Priority `'later'` means notifications never preempt queued user prompts.
3. The loop drains them at the between-turn boundary (`query.ts:1570`) as
   attachments, or — when idle — the queue processor starts a new turn,
   which is what re-invokes the agent when background work finishes.

## Eviction

`evictTerminalTask()` (`framework.ts:125`) removes a task from
`AppState.tasks` only when ALL hold:

1. status is terminal;
2. `notified` is true (the model has been told);
3. no UI retention: `retain: true` blocks eviction, and agent tasks get a
   30 s `evictAfter` panel grace period after terminal transition.

Output eviction is separate: `evictTaskOutput()` flushes and drops the
in-memory writer but keeps the disk file readable;
`cleanupTaskOutput()` also unlinks the file.

## Task Kinds Compared

| Kind | Runs where | Output | Kill path |
| --- | --- | --- | --- |
| `local_bash` | Child process (tree) | Disk file via append queue | SIGTERM → SIGKILL (tree-kill) |
| `local_agent` | Same process, separate query loop | Symlink to agent transcript | `abortController.abort()` |
| `in_process_teammate` | Same process, AsyncLocalStorage identity | Capped `messages` mirror (50 entries) | graceful `shutdownRequested` or hard abort |
| `remote_agent` | Remote daemon, polled | Polled events; persisted to sidecar for restore | archive remote session |

## EOS Migration Takeaways

- One typed task union + one live map keyed by typed ID; runtime handles in
  the same record but explicitly non-serializable.
- Separate three flags with distinct semantics: terminal status, `notified`
  (delivered to model), and retention (UI/user hold). Evict only when all
  align.
- Disk-backed output with offset deltas is the scalable pattern; never hold
  full task output in conversation state.
- Route completion notifications through the same queue as user input with a
  lower priority, and make each task type own its single notification.
- Apply async-computed updates as patches merged against fresh state, not as
  snapshot writes.

## Source Anchors

- Task types/statuses: `/Users/yifanxu/machine_learning/LoVC/c c/src/Task.ts:6`
- Registration: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/task/framework.ts:77`
- Eviction rules: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/task/framework.ts:125`
- Delta/patch generation: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/task/framework.ts:158`
- Notification enqueue: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/task/framework.ts:274`
- Disk output: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/task/diskOutput.ts:97`
- Command queue: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/messageQueueManager.ts:53`
- Shell task lifecycle: `/Users/yifanxu/machine_learning/LoVC/c c/src/tasks/LocalShellTask/LocalShellTask.tsx:180`
- Teammate state: `/Users/yifanxu/machine_learning/LoVC/c c/src/tasks/InProcessTeammateTask/types.ts:22`
- Task registry: `/Users/yifanxu/machine_learning/LoVC/c c/src/tasks.ts:22`
