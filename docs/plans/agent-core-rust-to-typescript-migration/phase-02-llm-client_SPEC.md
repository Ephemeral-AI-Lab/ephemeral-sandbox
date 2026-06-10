# EOS Agent Core Rust to TypeScript Migration - Phase 02 LLM Client

Status: Proposed
Date: 2026-06-10
Owner: eos-agent-core
Migration direction: Rust -> TypeScript
Project path: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`
Rust semantics reference: `agent-core/crates/eos-llm-client`, `agent-core/crates/eos-types/src/llm.rs` (what to normalize, not a line-for-line port source)
Knowledge inputs: `knowledge/event-stream-and-sse.md`, `knowledge/claude-code-tech-stack.md`, `knowledge/compaction.md`

## 1. Intent

Phase 02 builds the provider-client boundary in TypeScript:

- `@eos/contracts`: the minimum provider-neutral message DTOs,
- `@eos/llm-client`: the `LlmClient` interface, the normalized stream-event
  union, the provider error taxonomy, the visible-output retry gate, and two
  provider clients — Anthropic Messages and OpenAI Responses.

Division of labor, and the central design rule of this phase:

- **Official SDKs own transport.** HTTP, SSE parsing, headers, API
  versioning, and connection errors are owned by `@anthropic-ai/sdk` and
  `openai`. We do not hand-roll an SSE frame splitter, auth header
  application, or a fetch transport. Evidence this is the right call in
  TypeScript: Claude Code itself consumes the raw SDK event stream
  (`knowledge/event-stream-and-sse.md:24-28`) rather than parsing SSE.
- **We own the contracts.** The neutral DTOs, the 4-variant event union, the
  error kinds, the retry policy, and the per-provider normalizers are the
  surfaces the engine and persistence depend on. SDK types never leak past
  `providers/*`.

Both providers ship in this phase deliberately: a neutral contract designed
against one provider overfits; the second provider is the proof of
neutrality. The Rust crate already validated that both wire protocols
normalize into the same event union (its cross-provider substitutability
tests), so the design risk is low.

This phase is additive. The Rust crates remain the live implementation and
are not edited, moved, or retired.

## 2. Scope

In scope:

- `@eos/contracts` source: `MessageRole`, `ContentBlock`, `Message`,
  `ToolSpec`, `ToolUseId`, `JsonValue`/`JsonObject`, `DEFAULT_MAX_TOKENS`,
  with Zod schemas,
- `@eos/llm-client` source: request types, normalized events, provider
  error, retry gate, stream idle guard, config, secret wrapper, and the two
  provider clients over their official SDKs,
- copies of both providers' SSE fixtures from the Rust crate and ports of
  the Rust unit-test assertions that still apply,
- workspace metadata and the two SDK dependencies.

Out of scope:

- coding-plan access (`clients/claude_coding_plan.rs`,
  `clients/codex_coding_plan.rs`, `Auth::CodexAccess` JWT parsing, FedRAMP
  detection) — these are auth/header/base-URL **presets** over the same two
  clients (SDK `authToken` / `defaultHeaders` / `baseURL` options), not new
  clients; a later phase adds them as config presets,
- the multi-provider `ProvidersConfig`/`ProviderKind` envelope and
  `ConfiguredLlmClient` defaults wrapper (arrive with provider selection),
- extended thinking / reasoning replay (named seam, §5 caveats),
- engine/loop, tool framework, persistence, observability wiring, server
  surfaces,
- any edit under `agent-core/` (fixtures are copied, not moved).

## 3. Resulting File and Folder Structure

```
eos-agent-core/
├── package.json / pnpm-workspace.yaml / pnpm-lock.yaml
├── tsconfig.json / tsconfig.base.json / vitest.config.ts
└── packages/
    ├── contracts/                      # this phase: provider-neutral, persistence-stable
    │   ├── package.json                #   @eos/contracts (zod dependency)
    │   ├── src/
    │   │   ├── json.ts                 #   JsonValue, JsonObject
    │   │   ├── ids.ts                  #   ToolUseId (branded string; provider-assigned, no mint)
    │   │   ├── messages.ts             #   MessageRole, ContentBlock, Message, ToolSpec,
    │   │   │                           #   DEFAULT_MAX_TOKENS (Zod schemas + inferred types)
    │   │   └── index.ts
    │   └── tests/
    ├── llm-client/                     # this phase
    │   ├── package.json                #   @eos/llm-client (deps: @eos/contracts workspace:*,
    │   │                               #   @anthropic-ai/sdk, openai)
    │   ├── src/
    │   │   ├── client.ts               #   LlmClient interface, LlmStreamOptions
    │   │   ├── types.ts                #   LlmRequest, UsageSnapshot, ToolChoice, ReasoningEffort
    │   │   ├── events.ts               #   LlmStreamEvent (4 variants), StopReason
    │   │   ├── errors.ts               #   ProviderError + kind, SDK-error -> kind mapping
    │   │   ├── secret.ts               #   SecretString (redacting wrapper, ~20 lines)
    │   │   ├── config.ts               #   AnthropicApiConfig, OpenAiApiConfig, RetryConfig,
    │   │   │                           #   idle-timeout setting (Zod, with defaults)
    │   │   ├── retry.ts                #   retryStream: visible-output gate, signal-aware,
    │   │   │                           #   retry-after-aware
    │   │   ├── providers/
    │   │   │   ├── anthropic.ts        #   AnthropicApiClient: encode LlmRequest -> Messages params,
    │   │   │   │                       #   raw SDK event stream -> LlmStreamEvent
    │   │   │   └── openai.ts           #   OpenAiResponsesClient: encode LlmRequest -> Responses
    │   │   │                           #   params, SDK event stream -> LlmStreamEvent
    │   │   └── index.ts                #   public re-exports
    │   └── tests/
    │       └── fixtures/
    │           ├── anthropic/*.sse     #   copied from agent-core/crates/eos-llm-client/tests
    │           └── openai/*.sse        #   (full, text_tool, malformed, ...)
    ├── engine/                         # phase 03 (agent loop): agent-loop.ts, conversation.ts,
    │   └── ...                         #   turn.ts, tool-runner.ts, tools.ts, events.ts, run-handle.ts
    ├── db/                             # placeholder (later phase)
    ├── observability/                  # placeholder (later phase)
    ├── runtime/                        # placeholder (later phase)
    └── testkit/                        # placeholder (later phase)
```

Deliberately absent relative to the Rust crate:

| Rust module | Why it has no TypeScript counterpart |
| --- | --- |
| `sse.rs` (frame splitter, frame helpers) | The SDKs parse SSE; fixtures replay through the SDKs' real parsers via an injected `fetch` (§10) |
| `auth.rs` (header application) | SDK constructor options (`apiKey`, `authToken`, `defaultHeaders`); `secret.ts` keeps only the redaction wrapper |
| `client.rs` shared fetch transport, request-id capture | SDK-owned; request ids are read from SDK errors/responses in `errors.ts` |
| `clients/claude_coding_plan.rs`, `clients/codex_coding_plan.rs` | Config presets over the same two clients; later phase |
| `config.rs` `ProvidersConfig`/`ProviderKind` envelope | Arrives with provider selection; this phase configures each client directly |

## 4. Owned Contracts

### 4.1 Naming and wire-shape rule

`@eos/contracts` defines the canonical owned shape for everything that may
be persisted or cross a process boundary: `type`-discriminated unions,
snake_case field names, explicit defaults. There is no mapping layer between
the in-memory types and their JSON. New TS-only API surfaces that never
serialize (constructor options, method names) use camelCase. SDK types are
implementation details of `providers/*` and never appear in public
signatures.

Extension policy: the `ContentBlock` union and all DTO objects grow
**additively** (new variants, new optional fields). Later phases add what
they own — this is why this phase can ship the minimum set below without
boxing anyone in.

### 4.2 Message DTOs (`@eos/contracts`) — minimum set

- `MessageRole`: `'user' | 'assistant'`. No `system` role; the schema
  rejects `"system"`. The system prompt is a request field, never a message.
- `ContentBlock`, discriminated on `type`:
  - `text { text }`
  - `tool_use { tool_use_id, name, input }`
  - `tool_result { tool_use_id, content, is_error = false }`
  - `reasoning { text }`
- `Message`: `{ role, content: ContentBlock[] }`, `content` defaults to
  `[]`. Helpers: `fromUserText`, `assistantText`, `reasoningText`,
  `toolUses`.
- `ToolSpec`: `{ name, description, input_schema, output_schema? }`.
- `ToolUseId`: branded string, provider-assigned; no local mint.
- `DEFAULT_MAX_TOKENS = 32768`.

Cut from the Rust shape, with the phase that owns reintroducing each
(additive, per §4.1):

| Cut | Rust source | Owning future phase |
| --- | --- | --- |
| `tool_result.metadata` | `llm.rs:78-81` | Audit/persistence phase |
| `tool_result.is_terminal` | `llm.rs:82-84` | Terminal-tool phase |
| `system_notification` block variant | `llm.rs:86-90` | Notifications phase |
| `thinking` decode alias on `reasoning` | `llm.rs:64` | Only if a data-migration phase imports Rust-persisted transcripts |
| image/document blocks | n/a (Rust never had them) | Multimodal phase |
| server-side compaction blocks | n/a | Context-management phase (§5 caveats) |

### 4.3 Request types (`types.ts`)

- `LlmRequest`: `{ model, messages, system_prompt?, max_tokens =
  DEFAULT_MAX_TOKENS, tools, tool_choice?, reasoning_effort? }`. Plain
  object plus a `buildLlmRequest(partial)` defaulting helper; no builder
  class.
- `ToolChoice`: `'auto' | 'any' | { tool: string }`.
- `ReasoningEffort`: `'minimal' | 'low' | 'medium' | 'high' | 'max'`. The
  neutral set is the union of both providers' vocabularies; each encoder
  clamps to its provider's supported range (mapping in §5). Kept because
  agent profiles need per-agent effort control; `tool_choice` kept for
  forced-tool structured output.
- `UsageSnapshot`: `{ input_tokens, output_tokens,
  cache_read_input_tokens?, cache_creation_input_tokens? }` plus a
  `totalTokens` helper. The optional cache fields are adopted from the
  cache-economics findings in `knowledge/compaction.md`: future context
  management is driven by cache-aware token accounting, and retrofitting
  usage shapes after the engine starts summing totals is churn.

### 4.4 Normalized stream events (`events.ts`)

`LlmStreamEvent` is a `type`-discriminated union with exactly four variants:

| Event | Payload | Notes |
| --- | --- | --- |
| `assistant_text_delta` | `text` | Visible output for the retry gate |
| `reasoning_delta` | `text` | Visible output for the retry gate |
| `tool_use_delta` | `tool_use_id`, `name`, `input` | Emitted at block close, fully assembled; malformed argument JSON yields `{}` |
| `assistant_message_complete` | `message`, `usage`, `stop_reason?` | Success terminus; iteration ends after it |

`StopReason`: `'end_turn' | 'max_tokens' | 'tool_use' | 'stop_sequence'`
plus verbatim passthrough of any other provider string
(parse-don't-validate; already proven necessary by `refusal` and
`pause_turn` existing in the wild).

Both normalizers share the same accumulation semantics: per-block string
accumulation (linear — never re-parse accumulated JSON per chunk, the
O(n^2) trap called out in `knowledge/event-stream-and-sse.md:27`), tool
arguments parsed once at block close, message reassembled at the terminus,
exactly one `assistant_message_complete`.

### 4.5 Client interface and error contract (`client.ts`, `errors.ts`)

```ts
interface LlmStreamOptions {
  signal?: AbortSignal;
}

interface LlmClient {
  streamMessage(
    request: LlmRequest,
    options?: LlmStreamOptions,
  ): AsyncIterable<LlmStreamEvent>;
}
```

Iteration contract:

- single-pass: the returned iterable may be iterated once,
- success: zero or more deltas, then exactly one
  `assistant_message_complete`, then end,
- an assistant message with empty `content` is legal (the engine treats it
  as a no-tool-use turn),
- absent usage fields default to `0`,
- a stream that ends without the provider's terminal event is a **truncated
  stream**: the iterable throws `ProviderError` kind `decode` (retryable
  pre-visible under §4.6). This pins behavior the Rust implementation left
  ambiguous (clean generator end with no final message),
- all failures surface as a thrown `ProviderError` from iteration — except
  cancellation: when `options.signal` aborts, the SDK's abort error is
  rethrown as-is, and callers classify by `signal.aborted`, never by error
  type.

`ProviderError` is an `Error` subclass: `{ kind, status_code?, request_id?,
retry_after_s?, message }`, lowercase punctuation-free message. Mapping
table, applied to SDK errors:

| Source | Kind |
| --- | --- |
| HTTP 401, 403 | `authentication` |
| HTTP 429 (capture `retry-after` into `retry_after_s`) | `rate_limit` |
| HTTP 500, 502, 503, 529 | `server` |
| other HTTP status; invalid request construction | `request` |
| SDK connection/timeout error (no status) | `transport` |
| stream parse failure, truncated stream (no status) | `decode` |

`request_id` is read from the SDK error / response headers
(`request-id` / `x-request-id`). Named seam: the future reactive-compaction
path must distinguish context-window-exceeded from other `request` errors
(`knowledge/compaction.md` reactive path); the taxonomy grows a
classification then — kept extensible now, not implemented.

### 4.6 Retry gate (`retry.ts`)

```ts
function retryStream(
  cfg: RetryConfig,
  attempt: () => AsyncIterable<LlmStreamEvent>, // one provider attempt
  signal?: AbortSignal,
): AsyncIterable<LlmStreamEvent>;
```

Semantics (ported from `retry.rs`, with two deliberate improvements):

- one `emitted_visible` flag spans all attempts: once any delta variant has
  been forwarded, any later failure fails fast (re-running would duplicate
  text and double-dispatch `tool_use_id`s),
- retry is permitted iff not visible yet, `attempt < cfg.max_retries`, and
  the error is retryable,
- retryable: `rate_limit`/`server` only when `status_code` is in
  `cfg.status_codes`; `transport` and truncated-stream `decode` always;
  `authentication`/`request` and parse-failure `decode` never,
- backoff delay: `min(retry_after_s ?? base_delay_s * 2^attempt,
  max_delay_s)` — honoring the provider's `retry-after` is improvement one
  (the Rust gate ignores it); non-finite or non-positive delays skip the
  sleep,
- **signal-aware** is improvement two: the backoff sleep races the abort
  signal, and no new attempt starts after abort — an interrupt during
  backoff must not wait out the sleep or fire another request,
- a clean end of an attempt's iteration ends the wrapped stream.

There is exactly one retry-policy owner: both SDK clients are constructed
with `maxRetries: 0` so SDK-internal retries never stack with this gate.

### 4.7 Stream idle guard (`config.ts` setting, enforced in providers)

A per-chunk idle watchdog wraps each provider stream: if no event arrives
within `idle_timeout_s` (default `90`), the attempt is aborted and surfaces
as `ProviderError` kind `transport` (retryable pre-visible). Adopted from
`knowledge/event-stream-and-sse.md:45-48` — "stream went quiet" is a
first-class failure mode, and without this a hung connection hangs the
future agent loop forever.

### 4.8 Providers (`providers/anthropic.ts`, `providers/openai.ts`)

Both clients follow the same internal shape:

1. constructed from their config (`{ base_url?, api_key }` as
   `SecretString`) plus `RetryConfig` and the idle-timeout setting; the SDK
   client is created with explicit credentials (`maxRetries: 0`, injectable
   `fetch` for tests). Credentials are always passed explicitly — SDK
   environment-variable fallback is not relied upon, so a server runtime
   behaves deterministically,
2. `streamMessage` = `retryStream` over an attempt factory,
3. one attempt = encode `LlmRequest` to provider params -> SDK streaming
   call (raw event stream, `stream: true`, with `signal`) -> idle guard ->
   normalizer state machine -> `LlmStreamEvent`s,
4. unknown provider event types and unknown content block types are ignored
   (forward compatibility; revisited by the context-management phase when
   server-side compaction blocks must be preserved — §5 caveats).

Anthropic specifics: raw `messages.create({ stream: true })` iteration —
not the SDK's accumulating `.stream()`/`.finalMessage()` helpers, per the
linear-accumulation rule in §4.4. `message_start` carries initial usage;
`message_delta` carries `stop_reason` and output/cache usage; tool-use
blocks assemble across `content_block_start`/`input_json_delta`/
`content_block_stop`.

OpenAI specifics: Responses API streaming (`responses.create({ stream:
true })`); text deltas from `response.output_text.delta`, function calls
assembled from `response.output_item.added` +
`response.function_call_arguments.delta/.done`, terminus and usage from
`response.completed`.

The full neutral-to-wire projection for both is the compatibility table in
§5, which is normative for the encoders and decoders.

### 4.9 Config and secrets (`config.ts`, `secret.ts`)

- `AnthropicApiConfig`: `{ base_url = "https://api.anthropic.com",
  api_key }`. `OpenAiApiConfig`: `{ base_url = "https://api.openai.com/v1",
  api_key }`.
- `RetryConfig`: `{ max_retries = 3, base_delay_s = 1.0, max_delay_s =
  30.0, status_codes = [429, 500, 502, 503, 529] }`.
- Stream guard: `{ idle_timeout_s = 90 }`.
- All Zod schemas with defaults; negative delays/timeouts rejected at parse.
- `SecretString`: private field; `toString`/`toJSON`/`util.inspect.custom`
  return `[redacted]`; explicit `.expose()` called exactly once, inside the
  provider constructor.

## 5. Anthropic vs OpenAI Compatibility

Both clients implement `LlmClient` against the same neutral contracts. This
table is normative for the two encoders/decoders.

| Neutral contract | Anthropic Messages API | OpenAI Responses API |
| --- | --- | --- |
| Endpoint | `POST /v1/messages` (via `@anthropic-ai/sdk`) | `POST /v1/responses` (via `openai`) |
| Auth | `x-api-key` (SDK `apiKey`) | `Authorization: Bearer` (SDK `apiKey`) |
| `system_prompt` | `system` field | `instructions` field |
| `messages` history | `messages[]` of content blocks | `input[]` items (message / `function_call` / `function_call_output`) |
| `max_tokens` | `max_tokens` | `max_output_tokens` |
| `text` block | `{ type: "text", text }` | output/input text content |
| `tool_use` block | `{ type: "tool_use", id, name, input }` | `function_call` item (`call_id`, `name`, `arguments` JSON string) |
| `tool_result` block | `{ type: "tool_result", tool_use_id, content, is_error }` | `function_call_output` item (`call_id`, `output`) |
| `reasoning` block | dropped on encode (provider-managed; see caveats) | dropped on encode (provider-managed; see caveats) |
| `ToolSpec` | `{ name, description, input_schema }`; `output_schema` dropped | function tool (`name`, `description`, `parameters`); `output_schema` mapped where supported |
| `tool_choice: auto / any / {tool}` | `{type:"auto"} / {type:"any"} / {type:"tool", name}` | `"auto"` / `"required"` / `{type:"function", name}` |
| `reasoning_effort` | effort control: `minimal -> low`, `low/medium/high` direct, `max -> max` | `minimal/low/medium/high` direct, `max -> high` (clamp) |
| `assistant_text_delta` | `content_block_delta: text_delta` | `response.output_text.delta` |
| `reasoning_delta` | `content_block_delta: thinking_delta` | reasoning summary deltas, where the API provides them |
| `tool_use_delta` (assembled, emitted at block close) | `content_block_start` + `input_json_delta` accumulation, emitted at `content_block_stop` | `response.output_item.added` + `function_call_arguments.delta`, emitted at `function_call_arguments.done` |
| `assistant_message_complete` | at `message_stop` (stop reason + usage from `message_delta`) | at `response.completed` (status + usage from the completed response) |
| `stop_reason` | `end_turn / tool_use / max_tokens / stop_sequence / ...` passthrough | derived: function calls present -> `tool_use`; `incomplete: max_output_tokens` -> `max_tokens`; complete -> `end_turn`; others passthrough |
| `usage` | `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` | `input_tokens`, `output_tokens`, `input_tokens_details.cached_tokens -> cache_read_input_tokens` |
| `ToolUseId` | `toolu_*` (opaque, provider-assigned) | `call_*` / `fc_*` (opaque, provider-assigned) |
| Errors | HTTP status -> §4.5 table; SDK connection errors -> `transport` | same mapping, same table |

Compatibility caveats (named seams, both providers):

1. **Reasoning replay.** Dropping `reasoning` on encode is correct only
   while provider reasoning is off. Anthropic extended thinking with tool
   use requires replaying signed thinking blocks; OpenAI similarly requires
   replaying encrypted reasoning items when state is not server-stored. The
   thinking phase adds an optional signature/opaque-payload field to the
   `reasoning` block (additive) and the encoders stop dropping blocks that
   carry it.
2. **Effort vocabulary.** The neutral `ReasoningEffort` set is the union of
   both providers'; the clamping in the table is normative and must be
   covered by encode tests.
3. **Unknown blocks.** Both decoders ignore unrecognized content; enabling
   Anthropic server-side compaction later requires preserving compaction
   blocks in history (additive `ContentBlock` variant + decoder
   passthrough, owned by the context-management phase).

## 6. Validation Strategy

| Surface | Mechanism |
| --- | --- |
| `ContentBlock` / `Message` / `ToolSpec` (persisted, cross-package DTOs) | Zod schema is the source of truth; TS types inferred |
| Configs (external input) | Zod schemas with defaults |
| Provider stream events (hot path) | SDK-typed events + parse-don't-validate accumulation; no Zod per event; `parseToolArgs`: malformed JSON -> `{}` |
| `LlmRequest`, `LlmStreamEvent`, `ProviderError` (in-process) | Plain TS types/classes; no runtime validation |

## 7. Dependencies and Workspace Changes

- New runtime dependencies of `@eos/llm-client`: `@anthropic-ai/sdk` and
  `openai` (pinned). Justification per the Phase 00 rule: they own the
  provider transport boundary (HTTP/SSE/auth/versioning) that this phase
  would otherwise hand-port and maintain (~800-1000 LOC of `sse.rs`,
  `auth.rs`, and transport code), and the studied production reference
  (Claude Code) builds on the same SDK. Constraints: `maxRetries: 0`
  (§4.6), explicit credentials (§4.8), injectable `fetch` for tests (§10).
- `packages/contracts/`: gains `src/`, `tests/`, `zod` dependency,
  `exports` map.
- `packages/llm-client/`: new package per §3, depends on `@eos/contracts`
  via `workspace:*`.
- No other new dependencies; `p-queue` remains uninstalled.

## 8. Migration Steps

1. `@eos/contracts` DTOs + Zod schemas -> verify: round-trip tests;
   `system` role rejected; `tool_result.is_error` defaults false; the §4.2
   cut fields are absent.
2. `errors.ts`, `types.ts`, `events.ts`, `secret.ts`, `config.ts` ->
   verify: SDK-error and status -> kind mapping tests (including
   `retry_after_s` capture); secrets render `[redacted]`; config defaults
   and rejections.
3. `retry.ts` -> verify: the six ported `retry.rs` cases (fail-fast after
   visible output, retry-then-succeed, non-retryable auth, budget
   exhaustion at `1 + max_retries` attempts, `tool_use_delta` blocks retry,
   degenerate delay skips sleep) **plus** the two new cases: abort during
   backoff stops immediately with no further attempt; `retry_after_s`
   overrides exponential delay capped by `max_delay_s`.
4. `providers/anthropic.ts` + Anthropic fixtures -> verify: golden decode
   replay through the real SDK parser via injected `fetch` matches the Rust
   fixture assertions; encode tests cover the §5 column (reasoning drop,
   effort clamp, tool_choice mapping); truncated-stream and idle-timeout
   tests.
5. `providers/openai.ts` + OpenAI fixtures -> verify: same battery for the
   OpenAI column, including stop-reason derivation and cached-token usage
   mapping.
6. `index.ts` exports + workspace wiring -> verify: `pnpm install`,
   `pnpm run check` green from `eos-agent-core/`.
7. Update the migration `index.md` row for this phase.

## 9. Coexistence and Rollback

- Coexistence: the Rust `eos-llm-client` crate remains the live,
  authoritative client. The TypeScript package has no runtime consumer
  until Phase 03 and performs no I/O unless explicitly invoked.
- Rollback: delete `packages/llm-client/`, revert the
  `packages/contracts/` additions, remove the two SDK dependencies and root
  config edits, drop the index row. No other surface is affected.

## 10. Verification

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core
pnpm install
pnpm run check   # typecheck + vitest
```

- Fixture-golden tests replay the copied `.sse` fixtures byte-for-byte by
  injecting a `fetch` test double into each SDK client, exercising the real
  SDK parser plus our normalizer end to end, and asserting the same
  normalized event sequences as the Rust fixture tests.
- Live-provider smoke tests (real keys) are env-gated and excluded from
  `pnpm run check`; CI performs no network I/O.
- Rust boundary hygiene: `git diff --stat -- agent-core` stays empty.
- Docs hygiene: `git diff --check -- docs/plans/agent-core-rust-to-typescript-migration eos-agent-core`.

## 11. Acceptance Criteria

Phase 02 is accepted when:

- `@eos/contracts` exposes exactly the §4.2 minimum DTO set (the cut table
  fields/variants are absent) with Zod schemas and the additive extension
  policy documented,
- `@eos/llm-client` exposes the `LlmClient` interface and **two** clients —
  `AnthropicApiClient` and `OpenAiResponsesClient` — both normalizing to
  the same 4-variant event union per the §5 table,
- transport is SDK-owned: no hand-rolled SSE parsing, auth headers, or
  fetch plumbing exists in the package; SDK clients run with
  `maxRetries: 0`, explicit credentials, and injectable `fetch`,
- the retry gate is signal-aware and `retry-after`-aware; the idle guard
  and truncated-stream semantics behave per §4.5-§4.7,
- both providers' fixture-golden, encode-projection, retry, and
  error-mapping tests pass under `pnpm run check` with no network I/O,
- the §5 caveats (reasoning replay, effort clamp, unknown blocks) appear as
  named seams, not silent behavior,
- the Rust `agent-core/` tree is byte-for-byte unchanged,
- and this migration directory's `index.md` lists Phase 02 with its status
  and verification.

## 12. Progress Tracker

| Step | Status | Required proof |
| --- | --- | --- |
| Contracts minimum DTOs + schemas | Pending | Round-trip + rejection tests green; cut fields absent |
| Errors, types, events, secret, config | Pending | Mapping + redaction + default tests green |
| Retry gate (signal- and retry-after-aware) | Pending | Six ported + two new retry cases green |
| Anthropic provider + fixtures | Pending | Golden decode parity via injected fetch; §5 encode tests green |
| OpenAI provider + fixtures | Pending | Same battery for the OpenAI column green |
| Workspace wiring + exports | Pending | `pnpm run check` green from `eos-agent-core/` |
| Index updated | Pending | Phase 02 row present in `index.md` |
