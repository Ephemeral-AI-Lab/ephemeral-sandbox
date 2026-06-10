# EOS Agent Core Rust to TypeScript Migration - Phase 03 Agent Loop Engine

Status: Proposed
Date: 2026-06-10
Owner: eos-agent-core
Migration direction: Rust -> TypeScript
Project path: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`
Rust source boundary: `agent-core/crates/eos-engine/src/agent_loop` (loop spine only), `agent-core/crates/eos-engine/src/run_output/stream.rs` (event shapes, subset)
Depends on: Phase 02 (`@eos/contracts`, `@eos/llm-client`)

## 1. Intent

Phase 03 ports the agent loop spine to a new `@eos/engine` package: a thin
while loop that streams one assistant turn, executes the returned tool calls,
feeds results back, and repeats until the model replies without tool use.

The phase introduces the engine's three load-bearing structures around that
loop:

- a dual transcript (`displayed_messages` for users, `llm_messages` for the
  provider) with one writer and a named compaction seam,
- a normalized `AgentEvent` stream consumed as an `AsyncIterable`,
- a run handle owning interruption (`AbortController`) and steering (a
  pending-user-message queue drained at turn boundaries).

This phase is additive. The Rust engine remains the live implementation;
nothing under `agent-core/` changes.

## 2. Design Decisions

These are deliberate choices for this phase, recorded so later phases do not
mistake them for omissions:

1. `agent-loop.ts` stays thin: the loop function is control flow only
   (target well under 100 lines); streaming, transcript writes, and tool
   dispatch live in their own modules.
2. The transcript is split into `displayed_messages` and `llm_messages` from
   day one. Compaction is not implemented, but `llm_messages` is its future
   rewrite target and `Conversation.llmMessages()` is the only place a
   provider request reads history from.
3. The loop exits when an assistant message contains no `tool_use` blocks
   (the standard Anthropic/Codex CLI convention). This deliberately diverges
   from the Rust engine, where text-only turns continue and only a terminal
   tool ends the run (`executor.rs:286-288`, `state.rs:144-148`). The
   terminal-tool exit returns in a later phase (§11).
4. Steering is soft: a queued steer waits for the in-flight turn to finish.
   There is no in-run "hard steer" mode and there will not be one:
   redirecting an in-flight run is the composition `interrupt()` + a new
   `startAgentRun` over the salvaged `llm` history (§9).
5. Events are push-fed into a queue consumed as a single pull-based
   `AsyncIterable`. This deliberately diverges from both the Rust sink
   callback (`AgentRunStreamSink`, `stream.rs:163`) and the Claude Code
   async-generator loop (`knowledge/agent-loop-and-components.md`): a
   generator couples execution progress to consumption — right for a CLI
   with one eager UI consumer, wrong for a server engine where a recorder
   and an SSE adapter will both consume and where steering already requires
   a handle object. A future SSE/WebSocket endpoint is a transport adapter
   over this iterable, owned by a later server phase.
6. There is exactly one stop semantic. `interrupt(reason?)` aborts one
   `AbortController`; `reason` is a label recorded on the outcome, never a
   behavior branch. The public surface is three verbs total:
   `startAgentRun`, `steer`, `interrupt`. The Claude Code abort-reason
   taxonomy (`user-cancel` vs `interrupt` vs `sibling_error`) and the
   sibling-error cascade are rejected by design, not deferred (§9, §11).
7. Provider-history validity is an invariant at every point including the
   terminal state: every `tool_use` in `llm_messages` is answered by a
   `tool_result` before the run finishes, with synthetic error results on
   cancel (§7). `AgentRunOutcome.llm` is therefore always valid restart
   input.
8. Partial output (interrupt or provider failure) is salvaged to
   `displayed_messages` only. Accepted asymmetry: a restarted run does not
   remember half-streamed text.
9. The loop never throws: a top-level catch-all classifies every exit and a
   single finish path emits `run_finished` exactly once (§5).

## 3. Scope

In scope:

- `@eos/engine` package: loop, turn runner, conversation, minimal tool
  registry seam, tool batch runner, event stream, run handle,
- scripted-`LlmClient` test doubles and the test suite in §14,
- workspace metadata for the new package.

Out of scope (each named with its seam in §11):

- terminal tools and batch policies, lifecycle tools, notifications,
  turn-ceiling formula, background sessions, subagents,
- persistence (`eos-db`, record store): outcomes exist in memory only,
- the run service/registry layer (`eos-agent-run`) — future `@eos/runtime`,
- compaction, context budgeting,
- server transports (SSE/WebSocket endpoints), observability wiring,
- a real tool framework (`eos-tool` port): this phase defines only the
  narrow `ToolDefinition` interface the loop needs,
- any edit under `agent-core/`.

## 4. Rust Surface and TypeScript Target

| Rust source | TypeScript target | Carries |
| --- | --- | --- |
| `agent_loop/executor.rs:67-172` (`execute_agent_loop`) | `packages/engine/src/agent-loop.ts` | The while loop, exit dispatch |
| `agent_loop/executor.rs:231-311` (`execute_assistant_turn`) | `packages/engine/src/turn.ts` | Stream one provider turn, accumulate, emit events |
| `agent_loop/state.rs` (`conversation_messages`, `loop_messages_to_llm_messages`) | `packages/engine/src/conversation.ts` | Redesigned as the dual transcript (§6) |
| `agent_loop/executor.rs:313-410` (`dispatch_tool_batch`, minus batch policies) | `packages/engine/src/tool-runner.ts` | Parallel execution, cap 8, result assembly |
| `eos-tool` (reduced to the loop's seam) | `packages/engine/src/tools.ts` | `ToolDefinition`, `ToolRegistry` |
| `run_output/stream.rs:24-150` (subset) | `packages/engine/src/events.ts` | `AgentEvent` union + `EventStream` queue |
| `launcher.rs:18-79` (cancel pair) + new design | `packages/engine/src/run-handle.ts` | `AgentRunHandle`: abort, steer queue, outcome |
| n/a | `packages/engine/src/index.ts` | `startAgentRun` + public types |

## 5. Loop Design

```
startAgentRun(input) ──► AgentRunHandle { events, outcome, steer(), interrupt() }
                              │
            ┌─────────────────▼──────────────────────────────────┐
            │ while (true)                              loop.ts  │
            │  1. aborted?            ──► finish(cancelled)      │
            │  2. turns ≥ maxTurns?   ──► finish(max_turns)      │
            │  3. drain steered msgs  ──► conversation           │
            │  4. emit turn_started;                             │
            │     msg = runAssistantTurn(...) ────────────────────┼─► turn.ts: streams
            │       ProviderError ──► salvage partial,           │   LlmStreamEvents,
            │                         finish(provider_error)     │   re-emits AgentEvents
            │       aborted mid-stream ──► salvage partial,      │
            │                              finish(cancelled)     │
            │  5. conversation.appendAssistant(msg); turns++     │
            │  6. toolUses = tool_use blocks of msg              │
            │       none + steers queued ──► continue (step 1)   │
            │       none                 ──► finish(completed)   │
            │  7. results = runToolBatch(toolUses, signal) ──────┼─► tool-runner.ts
            │       aborted mid-batch ──► append settled +       │
            │         synthetic results, finish(cancelled)       │
            │  8. conversation.appendToolResults(results)        │
            └────────────────────────────────────────────────────┘
```

Termination:

| Trigger | Check site | Outcome |
| --- | --- | --- |
| Assistant message has no `tool_use` blocks and no steers are pending | step 6 | `completed` (carries final `stop_reason`) |
| `interrupt()` / signal aborted | steps 1, 4 (mid-stream), 7 (mid-batch) | `cancelled` |
| `ProviderError` thrown from the retry-wrapped stream | step 4 | `failed` `{ kind: 'provider_error' }` |
| `turns` reaches `maxTurns` (default 32) before completion | step 2 | `failed` `{ kind: 'max_turns' }` |
| Any other thrown error (engine bug, tool-runner fault) | top-level catch | `failed` `{ kind: 'internal' }` |

Notes:

- A turn is one provider call. The Rust ceiling formula
  (`tool_call_limit * 3/2 + 1`, `state.rs:199-201`) is replaced by the plain
  `maxTurns` cap until budgets get their own phase.
- The `maxTurns` check precedes the drain deliberately: a steer that arrives
  after the budget is spent dies in the queue (consistent with `steer()`
  returning `false` after finish) instead of entering the transcript as a
  user message no provider call ever saw.
- The "no tool_use but steers pending" branch makes a steer that lands during
  the final stream extend the run instead of silently dying in the queue.
- The loop decides on the presence of `tool_use` blocks, not on
  `stop_reason` (parity with the Rust executor, which extracts tool calls
  from the final message); `stop_reason` is recorded on events and on the
  `completed` outcome, so a `max_tokens` truncation or refusal is
  detectable by callers (escalation/retry is a §11 seam).
- Catch-all: the loop body runs inside one `try`. The catch classifies —
  `signal.aborted` → `cancelled`, `ProviderError` → `provider_error`,
  anything else → `internal` — and a `finally`-guarded finish path emits
  `run_finished` exactly once, closes the event stream, and resolves
  `outcome`. No exit can leak an unresolved promise or an unhandled
  rejection.
- Finishing is atomic: the step-6 decision, the transition to the finished
  state, and the flip of `steer()` to return `false` happen in one
  synchronous block — no `await` sits between the decision and `finish()`.
- `finish()` appends nothing to the transcript (the §7 cancel rule appends
  its settled + synthetic results *before* finish); it emits `run_finished`,
  closes the event stream, and resolves `outcome`.

## 6. Conversation: `displayed_messages` vs `llm_messages`

`Conversation` owns both lists; the loop never touches arrays directly.
Every append method writes both lists in one call (single-writer rule), so
the lists diverge only by the declared policies below, never by call-site
mistake.

```ts
interface DisplayedMessage {
  seq: number;          // monotonic within the run
  created_at: string;   // ISO-8601
  message: Message;
  partial?: 'interrupted' | 'provider_error'; // set only on salvaged partial assistant output
}

class Conversation {
  appendUser(message: Message): void;                 // initial + steered input
  appendAssistant(message: Message): void;            // completed assistant turn
  appendToolResults(blocks: ToolResultBlock[]): void; // one user message wrapping the batch
  appendPartialAssistant(partial: Message,
    reason: 'interrupted' | 'provider_error'): void;  // displayed only
  llmMessages(): readonly Message[];                  // the ONLY history source for LlmRequest
  displayedMessages(): readonly DisplayedMessage[];
}
```

Divergence policy:

| Append path | `displayed_messages` | `llm_messages` |
| --- | --- | --- |
| Initial and steered user messages | yes | yes |
| Completed assistant message | yes | yes |
| Tool-result user message | yes | yes |
| Tool-result message closing a cancelled batch (§7: settled + synthetic) | yes | yes |
| Partial assistant output (interrupt or provider failure) | yes (`partial` set) | no |
| Future compaction | unchanged (append-only) | rewritten |

Rules:

- The system prompt lives in neither list; it is the `LlmRequest`
  `system_prompt` field (parity with the Rust split, where
  `AgentLoopMessage::SystemPrompt` is filtered out of provider messages,
  `executor.rs:571-580`).
- Tool results for one batch form a single user message whose
  `tool_result` blocks follow the originating `tool_use` order.
- `llm_messages` entries are exactly `@eos/contracts` `Message` values; the
  list is valid `LlmRequest.messages` input at every point between loop
  steps *and at finish*: every `tool_use` is answered by a `tool_result`
  even on cancel (§7), so `AgentRunOutcome.llm` can always seed a new run.
- Compaction (later phase) will rewrite `llm_messages` (e.g. replace a
  prefix with a summary message) while `displayed_messages` remains the
  append-only source of truth shown to users. Phase 03 only guarantees the
  seam: history is read exclusively through `llmMessages()`.

## 7. Tool Execution

The loop needs a tool seam, not a tool framework:

```ts
interface ToolContext {
  signal: AbortSignal;
}

interface ToolOutput {
  content: string;
  is_error?: boolean; // default false
}

interface ToolDefinition {
  spec: ToolSpec; // from @eos/contracts; spec.name keys the registry
  isConcurrencySafe?(input: JsonObject): boolean; // default true; see batch rules
  execute(input: JsonObject, ctx: ToolContext): Promise<ToolOutput>;
}

type ToolRegistry = ReadonlyMap<string, ToolDefinition>;
```

Batch rules (`tool-runner.ts`):

- The batch is all `tool_use` blocks of the assistant message, executed
  concurrently with a cap of 8 (parity with
  `MAX_FOREGROUND_TOOL_CONCURRENCY`, `executor.rs:24`) via a small local
  limiter — `p-queue` stays uninstalled.
- Results are assembled in `tool_use` order regardless of completion order
  and wrapped into one user message of `tool_result` blocks.
- A thrown error from `execute` becomes `tool_result { is_error: true,
  content: <error message> }`; the loop continues. A tool failure never
  aborts its siblings — there is no sibling-error cascade (§2.6); the model
  sees the `is_error` result and decides.
- An unregistered tool name becomes `tool_result { is_error: true,
  content: "tool not found: <name>" }`; the loop continues.
- `isConcurrencySafe` is recorded on the contract but ignored by this
  phase's dispatcher (every batch runs fully concurrent; the `true` default
  preserves exactly these semantics). It exists now because per-call
  concurrency partitioning (`knowledge/agent-loop-and-components.md`) is a
  dispatch-semantics change: retrofitting the method later would alter the
  execution model under every already-written tool. Partitioning itself is
  a §11 seam.
- Tool results carry no terminal marker in this phase; the terminal-tool
  phase adds `tool_result.is_terminal` to `@eos/contracts` (additive, per
  the Phase 02 extension policy) together with the loop check that reads it.
- Each call emits `tool_execution_started` at dispatch and
  `tool_execution_completed` on settle.
- On abort mid-batch the batch settles immediately: calls that already
  completed keep their real results; every other dispatched `tool_use` gets
  a synthetic `tool_result { is_error: true, content: "interrupted" }`. The
  combined tool-result message is appended to BOTH transcript lists before
  `finish(cancelled)`, so `llm_messages` never ends with a dangling
  `tool_use` (`knowledge/abort-and-interrupt-handling.md`: provider-API
  validity is an invariant, not best effort).
- Dispatch happens only after `assistant_message_complete`; early dispatch
  on `tool_use_delta` is a named later optimization (parity: the Rust
  executor also extracts calls from the completed message,
  `executor.rs:278`).

## 8. Event Stream

`AgentEvent` is a `type`-discriminated union — the provider events forwarded
unchanged, plus tool execution and run lifecycle (subset of
`run_output/stream.rs`):

| Event | Payload |
| --- | --- |
| `turn_started` | `turn` (1-based; emitted after the drain, before each provider call) |
| `assistant_text_delta` | `text` |
| `reasoning_delta` | `text` |
| `tool_use_delta` | `tool_use_id`, `name`, `input` |
| `assistant_message_complete` | `message`, `usage`, `stop_reason?` |
| `tool_execution_started` | `tool_use_id`, `name`, `input` |
| `tool_execution_completed` | `tool_use_id`, `name`, `output`, `is_error` |
| `run_finished` | `outcome` |

`EventStream` semantics (`events.ts`):

- a push queue exposed as a single-consumer `AsyncIterable<AgentEvent>`;
  iterating twice is an error,
- pushes never block the loop; the buffer is unbounded in this phase (the
  consumer is in-process; backpressure belongs to the server phase),
- events are emitted in loop order; `run_finished` is always last, after
  which the iterable completes,
- `handle.outcome` resolves after `run_finished` is enqueued,
- a consumer that stops early (`break` / iterator `return()`) detaches: the
  run continues to completion and later events are discarded, not buffered;
  `outcome` remains the completion surface,
- if no consumer ever iterates, the run still executes to completion; every
  event is then retained for the run's lifetime — the accepted in-process
  memory cost of deferring backpressure to the server phase.

Identity stamping (`agent_name`/`agent_run_id` envelope fields,
`stream.rs:171-232`) is intentionally absent: a handle serves exactly one
run, so events need no identity until the `@eos/runtime` supervisor
multiplexes runs — that phase extends the envelope.

Field naming follows the Phase 02 wire rule: event payload fields are
snake_case because this union will cross an SSE/WebSocket boundary in a later
phase.

## 9. Run Handle: Interrupt and Steering

```ts
interface AgentRunHandle {
  events: AsyncIterable<AgentEvent>;
  outcome: Promise<AgentRunOutcome>; // never rejects
  steer(message: Message): boolean;  // false once finishing has begun
  interrupt(reason?: string): void;  // idempotent; no-op after finish
}
```

Interrupt — the only stop semantic:

- `interrupt(reason)` calls `AbortController.abort()`. One signal threads
  everywhere: the loop-top check, `streamMessage({ signal })` (kills the
  provider fetch mid-token), and every `ToolContext.signal`.
- `reason` (default `"interrupted"`) is recorded verbatim as the `cancelled`
  outcome's `reason` and is never a behavior branch. Streams and tools
  observe only `signal.aborted`; there is no reason taxonomy, no per-reason
  tool behavior, no sibling cascade (§2.6).
- When the signal is aborted, any error thrown by the stream or tools is
  classified as cancellation, not failure.

Partial output — one rule for interrupt and provider failure:

- Text/reasoning deltas accumulated when a turn dies — aborted mid-stream,
  or killed by a `ProviderError` after visible output (which the Phase 02
  retry gate deliberately will not retry) — become one
  `appendPartialAssistant` entry in `displayed_messages`, flagged
  `'interrupted'` or `'provider_error'`, if non-empty. Incomplete `tool_use`
  blocks are discarded entirely; nothing reaches `llm_messages`. Rationale:
  a half-streamed message is not valid provider history, and
  consistent-but-simple beats salvage. Consumers that rendered the deltas
  live learn the partial's fate from `run_finished`.
- A cancel mid-batch additionally appends the settled + synthetic
  tool-result message to both lists (§7) before finishing, keeping `llm`
  provider-valid.

Steering:

- `steer(message)` requires `message.role === 'user'` (throws `TypeError`
  otherwise), enqueues it, and returns `true`; once the loop has committed
  to finishing it returns `false` and discards the message.
- The commit is atomic (§5): the step-6 pending-steers check, the finished
  transition, and the `steer()` flip happen in one synchronous block, so
  there is no window where a steer is accepted but never delivered.
- The queue drains at exactly one point — loop step 3 — in arrival order,
  through `Conversation.appendUser` (both lists). A steer landing while a
  turn or tool batch is in flight therefore applies to the next provider
  call.
- A steer landing during what would be the final turn keeps the loop alive
  (step 6's pending-steers branch).
- Steers queued when `interrupt()` fires die with the run (deterministic:
  the run is cancelled, the queue is dropped).

Redirecting a run (replaces "hard steering"):

```ts
handle.interrupt('user redirected');
const { llm } = await handle.outcome;
const next = startAgentRun({ ...cfg, initialMessages: [...llm, newUserMessage] });
```

This composition needs no engine feature: cancelled outcomes guarantee
provider-valid `llm` (§7), so the restart never sends dangling `tool_use`
history. Accepted asymmetry (§2.8): the partial was displayed-only, so the
restarted run does not remember half-streamed text.

The Rust engine has cancellation (watch-channel pair, `launcher.rs:18-79`)
but no message injection; steering is new design introduced by this phase,
not a port.

## 10. Public API

```ts
function startAgentRun(input: StartAgentRunInput): AgentRunHandle;

interface StartAgentRunInput {
  llmClient: LlmClient;        // from @eos/llm-client, already configured
  tools: ToolRegistry;
  model: string;
  systemPrompt?: string;
  initialMessages: Message[];  // must be non-empty; throws TypeError otherwise
  maxTokens?: number;          // default DEFAULT_MAX_TOKENS
  reasoningEffort?: ReasoningEffort;
  maxTurns?: number;           // default 32
  signal?: AbortSignal;        // optional parent; external abort ≡ interrupt()
}

interface AgentRunFailure {
  kind: 'provider_error' | 'max_turns' | 'internal';
  message: string;
}

type AgentRunOutcome = {
  displayed: DisplayedMessage[];
  llm: Message[];              // provider-valid history (§6/§7); restart input
  usage: UsageSnapshot;        // summed across turns
  turns: number;
} & (
  | { status: 'completed'; final_message: Message; stop_reason?: string }
  | { status: 'cancelled'; reason: string }
  | { status: 'failed'; failure: AgentRunFailure }
);
```

- Dependency injection sits at the real provider and tool boundaries only
  (`llmClient`, `tools`); everything else is concrete inside the package.
- `startAgentRun` validates `initialMessages` is non-empty and starts the
  loop as a detached promise; the loop never throws — every exit path
  resolves `outcome` (§5 catch-all).
- `signal` is linked as the parent of the run's `AbortController`
  (`knowledge/abort-and-interrupt-handling.md` controller-tree pattern): an
  external abort is indistinguishable from `interrupt()`, so a server
  handler can tie a run to a request scope without glue code.
- `failure.kind` is typed so callers distinguish "budget exhausted"
  (`max_turns`, restartable with a fresh budget) from `provider_error` and
  `internal` without parsing prose; `message` carries the human-readable
  detail.
- `StartAgentRunInput` is in-process-only API and uses camelCase;
  `AgentRunOutcome`, `DisplayedMessage`, and `AgentEvent` may later be
  persisted or serialized and use snake_case wire naming (Phase 02 §4.1
  rule).

## 11. Deferred Rust Behavior (named seams)

| Deferred behavior | Source | Seam left by this phase |
| --- | --- | --- |
| Terminal-tool exit | `executor.rs:163-169`, `batch.rs:54-94` | Terminal-tool phase adds `tool_result.is_terminal` to contracts (additive) plus a check between loop steps 7 and 8 |
| Lifecycle/terminal batch policies | `batch.rs:54-172` | A future policy hook in `tool-runner.ts` before dispatch |
| Per-call concurrency partitioning | `knowledge/agent-loop-and-components.md` | `ToolDefinition.isConcurrencySafe` exists (default `true`); a future dispatcher partitions batches on it |
| Per-tool interrupt behavior (`cancel` vs `block`) | `knowledge/abort-and-interrupt-handling.md` | Genuinely additive optional method, once non-reentrant tools exist |
| Notifications at turn boundaries | `state.rs:160-187`, `notifications.rs` | Loop step 3 is the drain point; the steer queue gains a priority field then so notifications never preempt user steers (`knowledge/message-steering.md`) |
| Turn/token budget ceiling | `state.rs:144-148, 199-201` | `maxTurns` option; `usage` already accumulated per turn |
| Max-output-tokens recovery (escalate/retry) | `knowledge/agent-loop-and-components.md` | `stop_reason` on the `completed` outcome is the detection signal |
| Stream idle watchdog | `knowledge/event-stream-and-sse.md` | A `turn.ts` timeout option; "stream went quiet" classifies as `provider_error` |
| Persistence / record store | `executor.rs:187-228`, `eos-db` | `AgentRunOutcome.displayed` (audit/display) + `llm` (resume) + events carry what a recorder needs |
| Run service, registry, identity stamping | `eos-agent-run`, `stream.rs:171-232` | Future `@eos/runtime` wraps `AgentRunHandle`; event envelope extends |
| Compaction | none (new) | `Conversation.llmMessages()` is the only history read |
| Transcript-projection events for SSE | `eos-agent-core-server` | `turn_started` exists; message-level projection events (steered input, salvaged partials) arrive with the transport phase |
| Early tool dispatch on `tool_use_delta` | `events.rs:33` intent comment | `tool-runner.ts` currently dispatches post-completion only |
| Server SSE/WebSocket transport | `eos-agent-core-server` | Adapter over `handle.events` |

Rejected, not deferred (decisions; no seam is kept):

- Hard steering (abort-and-continue inside one run): replaced by
  `interrupt()` + restart over `outcome.llm` (§9).
- Abort-reason behavior taxonomy: one stop semantic; `reason` is a label
  (§2.6).
- Sibling-error cascade: a failed tool yields `is_error`; it never cancels
  siblings (§7).
- Tombstone/retraction events: the dual transcript plus `partial` flags
  covers fallback/abort without retraction semantics
  (`knowledge/event-stream-and-sse.md` recommends tombstones for a
  single-mutable-history design; this engine does not have one).

## 12. Workspace Changes

- `packages/engine/`: new package `@eos/engine` with `package.json`
  (`dependencies`: `@eos/contracts`, `@eos/llm-client` via `workspace:*`),
  `src/` per §4, `tests/` including the scripted `LlmClient` double.
- Test doubles stay under `packages/engine/tests/`; the existing
  `@eos/testkit` placeholder package stays empty until a second consumer
  needs the scripted client.
- No new third-party dependencies.

## 13. Migration Steps

1. Add `events.ts` (`AgentEvent`, `EventStream`) -> verify: queue ordering,
   single-consumer guard, early-break detach, completion-after-`run_finished`
   tests pass.
2. Add `conversation.ts` -> verify: divergence-policy table tests pass
   (§14 case 17).
3. Add `tools.ts` + `tool-runner.ts` -> verify: ordering, concurrency cap,
   error and unknown-tool mapping tests pass.
4. Add `turn.ts` over a scripted `LlmClient` -> verify: delta forwarding,
   message accumulation, abort classification tests pass.
5. Add `agent-loop.ts` + `run-handle.ts` + `index.ts` -> verify: the loop
   suite in §14 passes.
6. Wire workspace metadata -> verify: `pnpm run check` green from
   `eos-agent-core/`.
7. Update the migration `index.md` row for this phase.

## 14. Verification

Scripted-loop suite (all against an in-process `MockLlmClient`; no network):

| # | Case | Asserts |
| --- | --- | --- |
| 1 | Text-only reply | `completed` after one turn; both lists hold user + assistant; `stop_reason` surfaced on the outcome |
| 2 | Tool round-trip | Turn 2 request contains the tool-result user message; `completed` |
| 3 | Parallel batch | N `tool_use` blocks all execute; concurrency never exceeds 8; one result message in request order |
| 4 | Tool throws | `is_error: true` block; siblings unaffected; loop continues |
| 5 | Unknown tool | `is_error: true` "tool not found"; loop continues |
| 6 | Interrupt mid-stream | `cancelled` with the passed reason; partial in `displayed` flagged `partial: 'interrupted'`; absent from `llm_messages` |
| 7 | Interrupt mid-batch | `cancelled`; settled results + synthetic `"interrupted"` results appended to BOTH lists; every `tool_use` in `outcome.llm` answered |
| 8 | Steer between turns | Steered message in the next provider request, both lists |
| 9 | Steer during final turn | Loop continues instead of finishing |
| 10 | Steer at/after finish | Returns `false` in the same tick the loop commits to finishing; transcripts unchanged |
| 11 | `maxTurns` exceeded | `failed { kind: 'max_turns' }`; a steer queued after budget exhaustion never enters the transcript |
| 12 | Provider error mid-stream | `failed { kind: 'provider_error' }`; pre-error deltas salvaged to `displayed` as `partial: 'provider_error'` |
| 13 | Internal invariant throw | `failed { kind: 'internal' }`; `run_finished` emitted; iterable completes; `outcome` resolves |
| 14 | `max_tokens` truncation | `completed` with `stop_reason: 'max_tokens'` on the outcome |
| 15 | External `signal` aborts | `cancelled`; identical paths to `interrupt()` |
| 16 | Early consumer break | Consumer breaks after the first event; run executes to completion; `outcome` resolves |
| 17 | Conversation invariants | Divergence table of §6 holds across cases 1-16; `outcome.llm` provider-valid at every finish |
| 18 | Event golden sequence | Full ordered `AgentEvent` list for a scripted two-turn run incl. `turn_started`; `run_finished` last; iterable completes |

Commands:

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core
pnpm install
pnpm run check
```

- Rust boundary hygiene: `git diff --stat -- agent-core` stays empty.
- Docs hygiene: `git diff --check -- docs/plans/agent-core-rust-to-typescript-migration eos-agent-core`.

## 15. Coexistence and Rollback

- Coexistence: the Rust engine remains the live loop. `@eos/engine` has no
  runtime consumer (no server, no CLI) in this phase; it is exercised only
  by its tests.
- Rollback: delete `packages/engine/` and root config edits, drop the index
  row. Phase 02 packages are unaffected.

## 16. Acceptance Criteria

Phase 03 is accepted when:

- `@eos/engine` exposes exactly the §10 API, with the loop implemented as
  the §5 control flow and nothing from the §11 deferred list,
- `agent-loop.ts` contains control flow only and stays well under 100 lines,
- every exit resolves `outcome` exactly once with `run_finished` as the last
  event — including internal errors (the loop never throws or leaks an
  unresolved promise),
- `outcome.llm` is provider-valid restart input on every status, including
  a cancel mid-batch (synthetic tool results per §7),
- the dual-transcript divergence policy of §6 is enforced by `Conversation`
  and covered by tests,
- interrupt and steer behave per §9: one stop semantic with a label-only
  reason, atomic finish (no steer race), and partial-output salvage on both
  interrupt and provider failure,
- the run exits on a non-tool-use assistant message with the final
  `stop_reason` surfaced on the outcome, and the divergence from the Rust
  terminal-tool regime is documented in this spec,
- the §14 suite passes under `pnpm run check` with no network I/O,
- the Rust `agent-core/` tree is byte-for-byte unchanged,
- and this migration directory's `index.md` lists Phase 03 with its status
  and verification.

## 17. Progress Tracker

| Step | Status | Required proof |
| --- | --- | --- |
| Event stream | Pending | Ordering + single-consumer tests green |
| Conversation dual transcript | Pending | Divergence-policy tests green |
| Tool seam + batch runner | Pending | Ordering, cap, error-mapping tests green |
| Turn runner | Pending | Delta/accumulation/abort tests green |
| Loop + run handle + API | Pending | §14 suite green |
| Workspace wiring | Pending | `pnpm run check` green from `eos-agent-core/` |
| Index updated | Pending | Phase 03 row present in `index.md` |
