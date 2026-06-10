# Claude Code Abort and Interrupt Handling

Status: Observed
Date: 2026-06-10
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript migration reference

## Controller Hierarchy

Cancellation is a native `AbortController` tree with two helpers
(`utils/abortController.ts`):

- `createAbortController(maxListeners = 50)` (`:16`) raises the listener cap
  to avoid `MaxListenersExceededWarning` on busy signals.
- `createChildAbortController(parent)` (`:68`) links one-directional
  parent→child abort propagation through `WeakRef`s on both sides, so an
  abandoned child can be GC'd without the parent retaining it; if the parent
  is already aborted the child aborts immediately with the same reason.

```
QueryEngine.abortController            (per session; interrupt() aborts it)
  └─ turn / toolUseContext.abortController   (passed into the loop + LLM fetch)
       └─ StreamingToolExecutor.siblingAbortController  (child)
            └─ per-tool child controller
                 (permission-dialog rejections bubble UP by explicitly
                  aborting the parent; sibling_error does not)
ShellCommand listens on the tool signal; teammates get an INDEPENDENT root.
```

The `abort(reason)` string is load-bearing — it distinguishes interrupt
semantics:

| `signal.reason` | Source | Semantics |
| --- | --- | --- |
| `undefined` / `'user-cancel'` | ESC / Ctrl+C (`REPL.tsx:2147`) | True abort: kill everything in the turn. |
| `'interrupt'` | New message submitted mid-turn (`handlePromptSubmit.ts:331`, `print.ts:1861`, `REPL.tsx:4102`) | Steering: stop the LLM call, but let tools background/finish; no interruption message. |
| `'sibling_error'` | Bash failure in a parallel batch (`StreamingToolExecutor.ts:362`) | Cancel sibling tools only; parent query unaffected. |
| `'background'` | Session backgrounding (`REPL.tsx:2528`) | Detach without user-facing interruption. |

## Propagation on Abort

What happens to each moving part when the turn signal aborts:

| Target | Behavior |
| --- | --- |
| In-flight LLM stream | Signal is passed to the SDK fetch (`query.ts:664`); the HTTP stream terminates. |
| Running tool calls | `StreamingToolExecutor.getAbortReason()` (`:210`) classifies each tracked tool; aborted tools get synthetic error `tool_result` blocks. With reason `'interrupt'`, only tools whose `interruptBehavior()` is `'cancel'` are cancelled — `'block'` tools run to completion. |
| Shell processes | `ShellCommand.#abortHandler` (`ShellCommand.ts:186`): kills the whole process tree (tree-kill) — EXCEPT when reason is `'interrupt'`, where it returns without killing so the command can be backgrounded and partial output preserved. |
| Background tasks | Not children of the turn controller; they keep running across turn aborts. They die only via explicit `kill` tools or process-exit cleanup. |
| Subagents / teammates | Spawned with an independent root controller (`spawnInProcess.ts:119` — explicit comment: teammates must NOT abort when the leader's query is interrupted). Killed only via task kill or `registerCleanup` at process shutdown. |
| Todo / task lists | Plain state in `AppState.todos`; untouched by abort. Pending items simply remain for the next turn. |

## History Consistency

The conversation must stay API-valid (every `tool_use` needs a
`tool_result`), so abort paths synthesize results:

- `yieldMissingToolResultBlocks()` (`query.ts:123`) emits
  `is_error: true` tool_results (`'Interrupted by user'`) for every dangling
  tool_use.
- The streaming executor's `createSyntheticErrorMessage()`
  (`StreamingToolExecutor.ts:153`) distinguishes `user_interrupted`
  (REJECT_MESSAGE), `sibling_error` (`Cancelled: parallel tool call ...
  errored`), and `streaming_fallback` (discarded).
- Interrupt text constants live in `utils/messages.ts:207`:
  `INTERRUPT_MESSAGE = '[Request interrupted by user]'` and
  `INTERRUPT_MESSAGE_FOR_TOOL_USE`. `createUserInterruptionMessage()`
  (`messages.ts:545`) appends the user-visible marker — but it is skipped
  when `signal.reason === 'interrupt'` (`query.ts:1046`), because the
  queued steering message itself explains the interruption.
- Partial assistant messages already yielded are retracted via
  `TombstoneMessage` when a streaming fallback or abort invalidates them.

The loop then returns a typed terminal: `{ reason: 'aborted_streaming' }`
(`query.ts:1051`) or `'aborted_tools'`; queued-command lifecycle completion
is deliberately skipped on abort (`query.ts:230-238`).

## Process Exit

Long-lived resources register shutdown hooks via `registerCleanup()`
(used at `spawnInProcess.ts:183`, `LocalMainSessionTask.ts:116`); on CLI
exit these abort teammate/background controllers so nothing outlives the
process unintentionally.

## EOS Migration Takeaways

- Use one cancellation tree per turn, but give detachable work (background
  tasks, subagents) independent roots plus explicit kill APIs and a
  process-exit cleanup registry.
- Encode interrupt intent in the abort reason (steer vs hard abort vs
  sibling cascade); branch shell-kill and history-marker behavior on it.
- Give every tool an `interruptBehavior` contract (`cancel` vs `block`) so
  non-reentrant operations are never half-killed.
- Always synthesize error tool_results for dangling tool_use blocks before
  ending the turn — provider-API validity is an invariant, not best effort.
- Parent→child abort links should be weak; child→parent escalation must be
  explicit and reason-filtered.

## Source Anchors

- Controller helpers: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/abortController.ts:16`
- Engine interrupt: `/Users/yifanxu/machine_learning/LoVC/c c/src/QueryEngine.ts:1158`
- Abort exit path: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:1015`
- Synthetic results: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:123`
- Abort classification: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/tools/StreamingToolExecutor.ts:210`
- Sibling cascade: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/tools/StreamingToolExecutor.ts:359`
- Shell abort handler: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/ShellCommand.ts:186`
- Interrupt constants: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/messages.ts:207`
- Submit-interrupt trigger: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/handlePromptSubmit.ts:321`
- Teammate isolation: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/swarm/spawnInProcess.ts:119`
