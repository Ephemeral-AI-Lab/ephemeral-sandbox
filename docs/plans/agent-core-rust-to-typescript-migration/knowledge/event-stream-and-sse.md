# Claude Code Event Streams and SSE

Status: Observed
Date: 2026-06-10
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript migration reference

## Two Directions

```
INBOUND                                   OUTBOUND
Anthropic API (SSE)                       SDK consumers / remote clients
  │ Stream<BetaRawMessageStreamEvent>       ▲ SDKMessage union (Zod schemas)
  ▼                                         │
services/api/claude.ts                    QueryEngine.ts normalization
  raw event loop, accumulation              │ + drainSdkEvents() side queue
  ▼                                         │
query.ts queryLoop                        remote/sdkMessageAdapter.ts
  yields StreamEvent + Message              → WebSocket frames (CCR)
```

## Inbound: Consuming the Provider SSE Stream

The API call (`services/api/claude.ts:1822`) uses
`anthropic.beta.messages.create({ ...params, stream: true })` and iterates
the RAW event stream rather than the SDK's accumulated stream — an explicit
choice to avoid O(n²) JSON re-parsing on every `input_json_delta` chunk.

Per-event handling in the `for await` loop (`claude.ts:1940+`):

| SSE event | Action |
| --- | --- |
| `message_start` | init partial message, capture initial usage + TTFB |
| `content_block_start` | init indexed accumulator per block type (text / tool_use / thinking) |
| `content_block_delta` | append `partial_json` / `text` / `thinking` strings |
| `content_block_stop` | materialize a complete `AssistantMessage` and yield it immediately with `stop_reason: null` |
| `message_delta` | mutate the last yielded message in place with final usage and `stop_reason`; surface refusal / max_tokens / context-window errors |

Yielding per content block (not per message) is what lets tool execution
start while later blocks are still streaming (the loop feeds each completed
tool_use into `StreamingToolExecutor`).

Robustness around the stream:

- Idle watchdog: abort if no chunk arrives within `STREAM_IDLE_TIMEOUT_MS`
  (default 90 s) with a warning at 50% (`claude.ts:1868-1928`).
- Stall detection: gaps >30 s between events are logged with event type and
  count (`claude.ts:1944-1966`).
- Fallbacks: a non-streaming retry path exists; if a fallback fires after
  partial output was already yielded, the loop emits `TombstoneMessage`s to
  retract the orphaned partials (`query.ts:709-741`).

## Internal Event Union

The loop's yield type is the internal event source
(`query.ts:219-227`): `StreamEvent` (raw SSE passthrough),
`RequestStartEvent` (`stream_request_start`), `Message`
(assistant/user/system/attachment/progress), `TombstoneMessage`,
`ToolUseSummaryMessage`, and the `Terminal` return. UI and SDK layers are
both consumers of this single generator — there is no separate event bus for
conversation content.

## Outbound: SDK Message Surface

`QueryEngine` consumes the loop and re-emits a versioned, Zod-validated
`SDKMessage` union (`entrypoints/sdk/coreSchemas.ts:1854-1881`), ~24
variants including `SDKAssistantMessage`, `SDKPartialAssistantMessage`
(`type: 'stream_event'`, wrapping the raw SSE event — only when
`includePartialMessages` is on), `SDKResultMessage`,
`SDKToolProgressMessage`, `SDKTaskStarted/Progress/Notification`,
`SDKSessionStateChangedMessage` (`idle | running | requires_action`),
`SDKCompactBoundaryMessage`, and `SDKRateLimitEvent`. Every message carries
`uuid` and `session_id`.

Events that originate outside the loop (task lifecycle, session state)
travel through a small bounded side queue
(`utils/sdkEventQueue.ts:74-101`): `enqueueSdkEvent()` appends (cap 1000,
drop-oldest), `drainSdkEvents()` splices and stamps uuid/session_id before
the engine interleaves them into the output stream. Remote (CCR) clients
get the same SDKMessages adapted into WebSocket frames
(`remote/sdkMessageAdapter.ts:45`, `remote/SessionsWebSocket.ts`).

## Cross-Component Signaling

No global event bus. Three narrow mechanisms:

- `createSignal()` — tiny typed pub/sub used for session switches
  (`bootstrap/state.ts:478`) and command-queue change notification.
- The bounded SDK event queue above for loop-external lifecycle events.
- Store subscription (`state/store.ts`) for UI reactivity.

## EOS Migration Takeaways

- Consume provider SSE raw and accumulate yourself; yield completed content
  blocks eagerly so tool execution overlaps streaming.
- Put a per-chunk idle watchdog + stall telemetry on every provider stream;
  treat "stream went quiet" as a first-class failure mode.
- Maintain one internal event union yielded by the loop generator; derive
  the external (SSE/WebSocket) surface as a versioned, schema-validated
  mapping of it — never let externals consume internal types directly.
- Support message retraction (tombstones) in the event contract from the
  start; fallback/abort make it unavoidable.
- Use a bounded, drop-oldest side queue for lifecycle events produced
  outside the loop, stamped with ids at drain time and merged into the same
  output stream.
- For an SSE server in EOS, the SDKMessage union is the model: typed
  variants, uuid + session id on every event, partial-output variants
  opt-in.

## Source Anchors

- Stream creation: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/api/claude.ts:1822`
- Raw event loop: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/api/claude.ts:1940`
- Idle watchdog: `/Users/yifanxu/machine_learning/LoVC/c c/src/services/api/claude.ts:1868`
- Loop yield union: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:219`
- SDK schema union: `/Users/yifanxu/machine_learning/LoVC/c c/src/entrypoints/sdk/coreSchemas.ts:1854`
- Partial message schema: `/Users/yifanxu/machine_learning/LoVC/c c/src/entrypoints/sdk/coreSchemas.ts:1496`
- SDK event queue: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/sdkEventQueue.ts:74`
- Remote adapter: `/Users/yifanxu/machine_learning/LoVC/c c/src/remote/sdkMessageAdapter.ts:45`
- Session signal: `/Users/yifanxu/machine_learning/LoVC/c c/src/bootstrap/state.ts:478`
