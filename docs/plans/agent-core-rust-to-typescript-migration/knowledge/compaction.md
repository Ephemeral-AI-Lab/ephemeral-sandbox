# Claude Code Compaction Design

Status: Observed
Date: 2026-06-10
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/compact`
Migration context: `eos-agent-core/` TypeScript migration reference

## Where Compaction Sits in the Loop

All context maintenance runs at the top of each `queryLoop()` iteration,
before the API call, in a fixed order (`query.ts:369-543`):

```
1. tool-result token budget       (truncate oversized tool results)
2. snip                           (feature-gated old-message removal)
3. microcompact                   (cache-aware tool-result dedup)
4. context collapse               (feature-gated staged summaries)
5. auto-compact                   (full summary if over threshold)
→ API call
```

A second, reactive path exists after the API call: if the request comes back
prompt-too-long (413), the loop tries collapse-drain first, then a one-shot
reactive compact, before surfacing the error (`query.ts:1062-1183`).

## Trigger Thresholds

All thresholds derive from an effective context window =
`contextWindow - 20_000` reserved output tokens for the summary itself
(`autoCompact.ts:33`, `MAX_OUTPUT_TOKENS_FOR_SUMMARY`).

| Threshold | Value | Purpose |
| --- | --- | --- |
| Auto-compact | `effectiveWindow - 13_000` (`AUTOCOMPACT_BUFFER_TOKENS`, `autoCompact.ts:62`) | Fires full compaction before the API call. |
| Warning UI | `autoThreshold - 20_000` (`WARNING_THRESHOLD_BUFFER_TOKENS`) | "Context low" indicator. |
| Error UI | `autoThreshold - 20_000` (`ERROR_THRESHOLD_BUFFER_TOKENS`) | Stronger indicator (currently same buffer). |
| Blocking limit | `actualWindow - 3_000` (`MANUAL_COMPACT_BUFFER_TOKENS`) | Hard stop for further API calls. |

Token usage comes from `tokenCountWithEstimation(messages)` minus tokens
already freed by snip (`autoCompact.ts:225-238`). Env overrides:
`CLAUDE_CODE_AUTO_COMPACT_WINDOW`, `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`,
`DISABLE_AUTO_COMPACT`. Compaction is suppressed when `querySource` is
itself `'compact'` or `'session_memory'` (prevents the forked summarizer
from recursively compacting).

A circuit breaker stops auto-compact after
`MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES = 3` (`autoCompact.ts:70`); the
failure counter is threaded through the loop's `State.autoCompactTracking`.

## Full Compaction Mechanics

`compactConversation()` (`compact.ts:387`):

1. Pre-compact hooks run and may amend custom instructions.
2. `readFileState` and nested-memory caches are snapshotted then cleared.
3. Messages are sanitized for the summarizer: images/documents replaced with
   `[image]` markers, re-injected skill attachments stripped.
4. The summary request runs as a forked agent with `querySource: 'compact'`
   reusing the main thread's prompt cache when enabled (identical
   system/tools/model), else falls back to a plain streaming call with a
   minimal toolset and thinking disabled.
5. The summary prompt (`prompt.ts:61-143`) is a 9-section template (intent,
   technical concepts, files/code, errors, problem solving, user messages,
   pending tasks, current work, next step) wrapped in explicit no-tools
   preamble/trailer.
6. Post-compact messages are rebuilt as
   `[compactBoundary, summaryUserMessage, ...messagesToKeep, ...attachments,
   ...sessionStartHookMessages]` (`buildPostCompactMessages`,
   `compact.ts:330`). The boundary is a typed
   `SystemCompactBoundaryMessage` recording trigger (`auto`/`manual`),
   pre-compact token count, and last message UUID.
7. Re-injected attachments restore working context within budgets: up to 5
   files / 50k tokens total / 5k per file, plus plan, skills, deferred-tool
   and MCP instruction deltas (`compact.ts:122-131, 532-585`).
8. `runPostCompactCleanup()` resets module-level caches (microcompact state,
   system prompt sections, classifier approvals), with main-thread-only
   resets guarded by `querySource` so subagents sharing the process do not
   clobber the main thread (`postCompactCleanup.ts:31-77`).

If the compaction request itself hits prompt-too-long, the oldest API-round
message groups are truncated and it retries up to 3 times
(`truncateHeadForPTLRetry`, `compact.ts:243`).

## Variants

| Variant | What it does | Trigger | Mutates history? |
| --- | --- | --- | --- |
| Auto-compact | Full LLM summary, rebuilds message list | Token threshold pre-API | Yes (boundary + summary replace old messages) |
| Manual `/compact` | Same engine, optional custom instructions, no follow-up suppression | User command | Yes |
| Reactive compact | One-shot full compact | 413 after API call (feature-gated) | Yes |
| Cached microcompact | Marks old tool results (Read/Bash/Grep/Edit/etc.) deleted via API-level `cache_edits`; local messages untouched, prompt cache preserved | Every iteration | No (API-layer only) |
| Time-based microcompact | Clears old tool-result content in place, keeping last 5 | >60 min idle gap (cold cache, GB-config) | Yes (content replaced with `[Old tool result content cleared]`) |
| Session-memory compact | Stores pre-compact conversation into session memory, keeps 10k–40k tokens of recent messages; short-circuits full compact | Tried before full compact | Yes |

The cached/time-based split is cache-economics driven: while the server
prompt cache is warm, deletion happens at the API layer so the prefix stays
cache-valid; once the cache is cold anyway, in-place content clearing is
cheaper.

## EOS Migration Takeaways

- Compute one effective window (reserve summary output tokens) and derive
  warning/auto/blocking thresholds from it as named buffer constants.
- Run compaction before the provider call in the loop, plus a reactive
  recovery path on provider "too long" errors; guard the reactive path with
  a once-per-turn flag.
- Add a consecutive-failure circuit breaker on auto-compact from day one.
- Make the post-compact state an explicit typed boundary message plus a
  single summary user message; rebuild rather than edit history.
- Treat post-compact cleanup of module caches as an owned function with an
  explicit main-thread guard if subagents share the process.

## Source Anchors

- Threshold constants: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/compact/autoCompact.ts:62`
- Trigger check: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/compact/autoCompact.ts:160`
- Loop invocation: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:454`
- Reactive recovery: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:1062`
- Full compact: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/compact/compact.ts:387`
- Summary prompt: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/compact/prompt.ts:61`
- Microcompact: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/compact/microCompact.ts:253`
- Time-based config: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/compact/timeBasedMCConfig.ts:18`
- Post-compact cleanup: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/compact/postCompactCleanup.ts:31`
- Manual command: `/Users/yifanxu/machine_learning/LoVC/c c/src/commands/compact/compact.ts:40`
