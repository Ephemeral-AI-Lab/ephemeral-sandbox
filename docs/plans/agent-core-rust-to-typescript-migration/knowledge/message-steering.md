# Claude Code Message Steering (Typing While the Agent Runs)

Status: Observed
Date: 2026-06-10
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript migration reference

## The Queue

All mid-run input lands in one module-level queue, not React state:
`commandQueue: QueuedCommand[]` in `utils/messageQueueManager.ts:53`, with a
frozen snapshot regenerated on every mutation for `useSyncExternalStore`
consumers. Each `QueuedCommand` carries `mode`
(`prompt | task-notification | ...`), `value`, `uuid`, `agentId`, and a
`priority`:

| Priority | Used by | Effect |
| --- | --- | --- |
| `now` | submit-interrupt path | watcher aborts the in-flight request with reason `'interrupt'` (`print.ts:1861`, `REPL.tsx:4102`) |
| `next` | normal user prompts typed mid-run (`enqueue()`, `:113`) | injected at the next loop boundary |
| `later` | background task notifications (`enqueuePendingNotification()`, `:127`) | never starves user input |

## Two Drain Boundaries

```
user types while isLoading
  └─ enqueue({ mode:'prompt', priority:'next' })
                          │
        ┌─────────────────┴──────────────────┐
        │ mid-turn drain                     │ idle drain
        │ (loop still running)               │ (no active query)
        │ query.ts:1570 after tool exec:     │ useQueueProcessor effect:
        │   getCommandsByMaxPriority(...)    │   fires when isQueryActive
        │   filter: no slash commands;       │   becomes false & queue
        │   main thread takes user prompts,  │   non-empty; batches prompts,
        │   subagents take only their own    │   runs slash/bash commands
        │   task-notifications               │   one at a time
        │ → getQueuedCommandAttachments()    │ → executeUserInput() starts
        │   yields attachment messages       │   a fresh turn
        │ → remove(consumed) from queue      │
        └────────────────────────────────────┘
```

Mid-turn details (`query.ts:1547-1646`):

- Drain happens after tool execution, before the next API call — never
  mid-stream.
- If the model just ran the Sleep tool, the drain widens to priority
  `'later'` (`sleepRan ? 'later' : 'next'`), letting task notifications wake
  it; otherwise only `next`-priority input is taken.
- Slash commands are excluded from mid-turn drain — they stay queued for the
  post-turn processor, since they need the full REPL command machinery.
- Consumed commands are removed from the queue and their lifecycle is
  notified only on normal turn completion (`query.ts:230` wrapper).

## Steer vs Abort

Two distinct behaviors share the input box:

| | Queue-and-continue (steer) | Interrupt-and-replace |
| --- | --- | --- |
| Trigger | typing + Enter while tools run | submit when `hasInterruptibleToolInProgress` (`handlePromptSubmit.ts:321-332`) or `priority: 'now'` command |
| LLM stream | continues | aborted with reason `'interrupt'` |
| Running tools | finish normally | `interruptBehavior() === 'cancel'` tools cancelled; `'block'` tools finish; shells backgrounded instead of killed |
| History marker | none needed | no `[Request interrupted by user]` marker either — the new user message itself explains the cut (`query.ts:1046`) |
| Queued input survives | yes | yes (the queue is independent of the abort) |

This is the key asymmetry with a hard ESC abort: ESC yields the interruption
marker and kills shells; steering with reason `'interrupt'` suppresses the
marker and lets work background. See `abort-and-interrupt-handling.md`.

## Framing of Injected Messages

Queued input is injected as a typed attachment, not a raw user message:
`getQueuedCommandAttachments()` (`utils/attachments.ts:1047`) produces
`{ type: 'queued_command', prompt, commandMode, origin, isMeta,
source_uuid }`. User prompts have `isMeta: false`; coordinator/system
injections carry `origin: { kind: 'coordinator' }, isMeta: true`. At the SDK
boundary these replay as user messages with `isReplay: true`
(`QueryEngine.ts:875-893`).

## Steering Subagents

Teammates do not share the global queue. Messages to an in-process teammate
go through `injectUserMessageToTeammate()`
(`tasks/InProcessTeammateTask/InProcessTeammateTask.tsx:68`), which appends
to that task's `pendingUserMessages` array in AppState; the teammate's
runner polls and drains it FIFO between its own loop iterations
(`inProcessRunner.ts:704-738`). Same pattern, task-scoped: steering is
always "append to the target's inbox, drained at its loop boundary".

## EOS Migration Takeaways

- One prioritized input queue (`now`/`next`/`later`) shared by user input
  and system notifications; background-work notifications always at the
  lowest priority.
- Drain at exactly two boundaries: post-tool-execution inside a running
  turn, and on idle to start a new turn. Never inject mid-stream.
- Express "interrupt to steer" as `abort('interrupt')` + queued message, and
  make downstream consumers (shells, tools, history markers) branch on the
  reason; don't invent a separate steering channel.
- Inject queued input as typed attachments with origin/meta flags so the
  model and the transcript can distinguish user steering from system
  notifications.
- Per-subagent inboxes drained at the subagent's own loop boundary; the
  global queue stays main-thread-only.

## Source Anchors

- Queue + priorities: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/messageQueueManager.ts:53`
- Enqueue paths: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/messageQueueManager.ts:113`
- Mid-turn drain: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:1570`
- Consumption: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:1632`
- Attachment conversion: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/attachments.ts:1047`
- Idle processor: `/Users/yifanxu/machine_learning/LoVC/c c/src/hooks/useQueueProcessor.ts:37`
- Submit-interrupt: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/handlePromptSubmit.ts:321`
- Teammate inbox: `/Users/yifanxu/machine_learning/LoVC/c c/src/tasks/InProcessTeammateTask/InProcessTeammateTask.tsx:68`
