# Claude Code Agent Loop and Main Components

Status: Observed
Date: 2026-06-10
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript migration reference

## Loop Shape

The agent loop is an async generator, not a class-based state machine. The
public entry is `query()` (`query.ts:219`), a thin wrapper that delegates to
`queryLoop()` (`query.ts:241`) and notifies queued-command lifecycle only on
normal return. `queryLoop()` is a `while (true)` loop (`query.ts:307`) that
threads an explicit `State` object across iterations and `return`s a
`Terminal` object (`{ reason: 'completed' | 'aborted_streaming' | ... }`)
when the turn ends.

```
queryLoop iteration N
  ├─ context maintenance (pre-API)
  │    ├─ apply tool-result token budget          query.ts:379
  │    ├─ microcompact / snip / context-collapse  query.ts:401-447
  │    └─ auto-compact if over threshold          query.ts:454
  ├─ yield { type: 'stream_request_start' }       query.ts:337
  ├─ stream LLM call: for await deps.callModel()  query.ts:654
  │    ├─ yield each assistant message as it completes
  │    ├─ collect tool_use blocks
  │    └─ feed tools to StreamingToolExecutor while still streaming
  ├─ no tool_use?  → recovery / stop-hook / exit path
  │    ├─ abort check → synthetic tool_results, return 'aborted_streaming'
  │    ├─ prompt-too-long → collapse drain or reactive compact, continue
  │    ├─ max-output-tokens → escalate (8k→64k) or retry ≤3, continue
  │    ├─ stop hooks may block exit → inject error msg, continue
  │    └─ otherwise return { reason: 'completed' }
  └─ tool_use present → execute tools, yield results
       ├─ drain queued user commands as attachments  query.ts:1570
       ├─ inject memory / file-change / skill attachments
       └─ continue with State{ turnCount+1, messages: [...prev, ...new] }
```

Per-iteration `State` (`query.ts:204-217`) carries: `messages`,
`toolUseContext`, `autoCompactTracking`, `maxOutputTokensRecoveryCount`,
`hasAttemptedReactiveCompact`, `maxOutputTokensOverride`,
`pendingToolUseSummary`, `stopHookActive`, `turnCount`, and `transition`
(why the loop continued — used for tests/telemetry). There are ~9 distinct
`continue` sites, each clearing specific recovery counters so retries cannot
spiral.

## What the Loop Yields

`AsyncGenerator<StreamEvent | RequestStartEvent | Message | TombstoneMessage
| ToolUseSummaryMessage, Terminal>` (`query.ts:221-227`).

| Yield | Meaning |
| --- | --- |
| `stream_request_start` | An API request is about to begin (UI spinner). |
| `StreamEvent` | Raw Anthropic SSE event passthrough (partial output). |
| `Message` (assistant/user/system/attachment/progress) | Completed conversation messages, tool results, boundaries, errors. |
| `TombstoneMessage` | Tells consumers to drop an already-yielded message (streaming fallback, abort). |
| `ToolUseSummaryMessage` | Async Haiku-generated summary of the previous tool batch. |
| `Terminal` (return value) | Why the turn ended: `completed`, `aborted_streaming`, `aborted_tools`, etc. |

## Main Components

| Component | File | Responsibility |
| --- | --- | --- |
| Loop driver | `query.ts` (`query()` 219, `queryLoop()` 241) | Turn lifecycle, recovery paths, compaction hooks, queued-input drain. |
| Engine facade | `QueryEngine.ts` (class, `interrupt()` at 1158) | Session-scoped wrapper: holds `mutableMessages`, root `AbortController`, usage totals; normalizes loop output into SDK messages. |
| Tool contract | `Tool.ts` (`Tool` type 362, `ToolUseContext` 158) | Per-tool `call()`, `checkPermissions()`, `isConcurrencySafe()`, `inputSchema` (Zod), result mapping. |
| Batch orchestration | `services/tools/toolOrchestration.ts` (`runTools()` 19, `partitionToolCalls()` 91) | Non-streaming path: partitions tool_use blocks into serial vs concurrent batches. |
| Streaming executor | `services/tools/StreamingToolExecutor.ts` (class 40) | Starts tools while the model is still streaming; per-tool child abort controllers; sibling-error cascade. |
| Single tool run | `services/tools/toolExecution.ts` (`runToolUse()`) | Permission check → hooks → `tool.call()` → result formatting; yields progress + context modifiers. |
| Hooks | `services/tools/toolHooks.ts` | Pre/post tool-use hooks (permissions, telemetry, failure handling). |
| Compaction | `services/compact/*` | Auto/manual/micro/reactive compaction (see `compaction.md`). |
| Message factory | `utils/messages.ts` | `createUserMessage`, interrupt constants, boundary messages, API normalization. |
| Context assembly | `context.ts`, `utils/api.ts` | System context appended to system prompt, user context prepended to first user message (cache-stable injection). |

## Tool Concurrency

Concurrency is decided per call, input-aware, not per tool name:

- `partitionToolCalls()` (`toolOrchestration.ts:91`) parses each tool input
  with the tool's Zod schema and asks `tool.isConcurrencySafe(parsedInput)`;
  parse or callback errors are treated as not safe.
- Consecutive concurrency-safe calls merge into one batch and run via
  `runToolsConcurrently()` with a cap (`getMaxToolUseConcurrency()`, default
  10); unsafe calls run alone via `runToolsSerially()`.
- Serial path applies tool-driven `ToolUseContext` modifiers immediately;
  concurrent path queues modifiers and applies them after the batch
  (`toolOrchestration.ts:54-62`) so parallel tools see a stable context.
- The streaming executor enforces the same invariant dynamically: a tool may
  start only if nothing is executing, or everything executing is
  concurrency-safe (`StreamingToolExecutor.ts:129`). A Bash failure aborts
  sibling tools through a shared child controller with reason
  `'sibling_error'` (`StreamingToolExecutor.ts:359-362`).

## EOS Migration Takeaways

- Model the loop as an async generator returning a typed `Terminal`; encode
  every "why did we continue" reason in an explicit transition value.
- Keep all per-turn mutable state in one `State` object passed across
  iterations; never hide recovery counters in module globals.
- Make concurrency safety a per-call, input-derived property on the tool
  contract, with a conservative default on validation failure.
- Yield partial output and tombstones rather than buffering: consumers must
  be able to retract messages on fallback/abort.
- Run context maintenance (budgeting, compaction) at the top of each
  iteration, before the provider call, so a single code path owns context
  size.

## Source Anchors

- Loop entry: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:219`
- Loop body and `State`: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:204`
- Abort exit: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:1015`
- Queued-command drain: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:1570`
- Batch partition: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/tools/toolOrchestration.ts:91`
- Streaming executor: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/tools/StreamingToolExecutor.ts:40`
- Tool contract: `/Users/yifanxu/machine_learning/LoVC/c c/src/Tool.ts:158`
- Engine facade: `/Users/yifanxu/machine_learning/LoVC/c c/src/QueryEngine.ts:184`
