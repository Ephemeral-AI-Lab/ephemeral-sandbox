# Crate `eos-llm-client` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-llm-client/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**22 types across 9 files.**

The `eos-llm-client` crate owns the provider-neutral LLM vocabulary and the direct HTTP/SSE clients that turn an `LlmRequest` into a stream of normalized `LlmStreamEvent`s. It is the single boundary where a wire protocol (Anthropic Messages, OpenAI Responses) is encoded from neutral types and decoded back into them: the neutral conversation shape (`Message`, `ContentBlock`, `MessageRole`), the request-side value types (`UsageSnapshot`, `ToolSpec`, `ToolChoice`, `LlmRequest` + its `LlmRequestBuilder`), the streamed-output vocabulary (`LlmStreamEvent`, `StopReason`), the single `ProviderError`/`ProviderErrorKind` failure type, the `Auth` credential carrier, and the `LlmClient` seam (`LlmStream`) implemented by `AnthropicClient` and `OpenAiClient`. The provider-specific projection state (`BlockAccum`/`AnthropicState`, `ToolItem`/`OpenAiState`) and the allocation-light `SseFrameSplitter` stay private. It depends on no provider SDK — direct `reqwest` plus a hand-rolled SSE splitter, `eos-types` for `JsonObject`/`ToolUseId`, and `eos-config` for `RetryConfig` — and owns no engine-domain events, tool registry, or lifecycle policy. The composition root (`eos-runtime`) stores implementors behind `Arc<dyn LlmClient>` and `eos-tools` authors `ToolSpec`s against it.

## Contents

- **`eos-llm-client/src/lib.rs`** — _(no inventoried types; crate root + re-exports)_
- **`eos-llm-client/src/anthropic.rs`** — `AnthropicClient`, `BlockAccum`, `AnthropicState`
- **`eos-llm-client/src/auth.rs`** — `Auth`
- **`eos-llm-client/src/client.rs`** — `LlmStream`, `LlmClient`
- **`eos-llm-client/src/error.rs`** — `ProviderErrorKind`, `ProviderError`
- **`eos-llm-client/src/events.rs`** — `LlmStreamEvent`, `StopReason`
- **`eos-llm-client/src/message.rs`** — `MessageRole`, `ContentBlock`, `Message`
- **`eos-llm-client/src/openai.rs`** — `OpenAiClient`, `ToolItem`, `OpenAiState`
- **`eos-llm-client/src/sse.rs`** — `SseFrameSplitter`
- **`eos-llm-client/src/types.rs`** — `UsageSnapshot`, `ToolSpec`, `ToolChoice`, `LlmRequest`, `LlmRequestBuilder`

---

## `eos-llm-client/src/anthropic.rs`

#### `AnthropicClient`  ·  _struct_  ·  derives: `Debug`  ·  [L41]

The Anthropic-native streaming client: encodes an `LlmRequest` to `/v1/messages` (`stream: true`) and decodes the SSE response into normalized events.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `http` | `reqwest::Client` |  |
| `endpoint` | `reqwest::Url` |  |
| `auth` | `Arc<Auth>` |  |
| `retry` | `Arc<RetryConfig>` |  |

**Trait impls**: `LlmClient`

<details><summary>Methods (2)</summary>

`new`, `build_headers`

</details>

#### `BlockAccum`  ·  _struct_  ·  derives: `Debug`  ·  private  ·  [L105]

In-flight reassembly state for one Anthropic content block (text / thinking / tool-use accumulation).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `block_type` | `String` |  |
| `id` | `String` |  |
| `name` | `String` |  |
| `text` | `String` |  |
| `input_json` | `String` |  |

#### `AnthropicState`  ·  _struct_  ·  derives: `Debug, Default`  ·  private  ·  [L115]

Decoder state across the whole Anthropic message stream (open blocks, finalized content, usage, stop reason).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `blocks` | `HashMap<usize, BlockAccum>` |  |
| `content` | `Vec<ContentBlock>` |  |
| `input_tokens` | `u32` |  |
| `output_tokens` | `u32` |  |
| `stop_reason` | `Option<String>` |  |

---

## `eos-llm-client/src/auth.rs`

#### `Auth`  ·  _enum_  ·  derives: `Debug`  ·  #[non_exhaustive]  ·  [L27]

How to authenticate a provider request; credentials are held in `secrecy::SecretString` so they are redacted in `Debug`/logs.

**Variants**: `ApiKey(SecretString)`, `Bearer(SecretString)`

<details><summary>Methods (3)</summary>

`api_key`, `bearer`, `apply`

</details>

---

## `eos-llm-client/src/client.rs`

#### `LlmStream`  ·  _type alias_  ·  = `Pin<Box<dyn Stream<Item = Result<LlmStreamEvent, ProviderError>> + Send>>`  ·  [L24]

A streamed model invocation: a single linear stream of normalized events or errors, with the retry gate running lazily inside it.

#### `LlmClient`  ·  _trait_  ·  bases: `Send + Sync`  ·  async  ·  [L31]

The provider-neutral streaming client seam (DIP + LSP); implemented by `AnthropicClient`, `OpenAiClient`, and test mocks.

**Trait items**:
- `async fn stream_message(&self, request: LlmRequest) -> Result<LlmStream, ProviderError>;`

---

## `eos-llm-client/src/error.rs`

#### `ProviderErrorKind`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  #[non_exhaustive]  ·  [L19]

The category of a provider failure, derived from HTTP status (plus SDK-free `Transport`/`Decode` additions) so the retry gate can branch on a typed kind.

**Variants**: `Authentication`, `RateLimit`, `Server`, `Request`, `Transport`, `Decode`

#### `ProviderError`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, thiserror::Error`  ·  #[non_exhaustive]  ·  [L43]

A normalized upstream provider failure carrying a typed kind, optional HTTP status, the provider request-id header, and a lowercase message.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `kind` | `ProviderErrorKind` | `pub` |
| `status_code` | `Option<u16>` | `pub` |
| `request_id` | `Option<String>` | `pub` |
| `message` | `String` | `pub` |

**Trait impls**: `Error, Display`

<details><summary>Methods (4)</summary>

`from_status`, `transport`, `decode`, `request`

</details>

---

## `eos-llm-client/src/events.rs`

#### `LlmStreamEvent`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  #[non_exhaustive]  ·  [L21]

A single normalized event from a streaming model invocation; the three `*Delta` variants are "visible output" for the retry gate and `AssistantMessageComplete` is the success terminus.

**Variants**:
- `AssistantTextDelta { text: String }`
- `ReasoningDelta { text: String }`
- `ToolUseDelta { tool_use_id: ToolUseId, name: String, input: JsonObject }`
- `AssistantMessageComplete { message: Message, usage: UsageSnapshot, stop_reason: Option<StopReason> }`

#### `StopReason`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L56]

A parsed provider stop reason (`api-parse-dont-validate`).

**Variants**: `EndTurn`, `MaxTokens`, `ToolUse`, `StopSequence`, `Other(String)`

<details><summary>Methods (1)</summary>

`parse`

</details>

---

## `eos-llm-client/src/message.rs`

#### `MessageRole`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  [L21]

The role of a conversation message; deliberately has no `System` variant (the system prompt is a request field), so `"system"` fails to deserialize.

**Variants**: `User`, `Assistant`

<details><summary>Methods (1)</summary>

`as_wire`

</details>

#### `ContentBlock`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  #[non_exhaustive]  ·  [L47]

A single content block within a `Message` (the discriminated union); the Python `ThinkingBlock` is renamed to `Reasoning` with a `thinking` serde alias for legacy transcripts.

**Variants**:
- `Text { text: String }`
- `ToolUse { tool_use_id: ToolUseId, name: String, input: JsonObject }`
- `Reasoning { text: String }` (`#[serde(alias = "thinking")]`)
- `ToolResult { tool_use_id: ToolUseId, content: String, is_error: bool #[serde(default)], metadata: JsonObject #[serde(default)], is_terminal: bool #[serde(default)] }`
- `SystemNotification { text: String }`

#### `Message`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L98]

A single assistant or user message: a role plus an ordered list of content blocks.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `role` | `MessageRole` | `pub` |
| `content` | `Vec<ContentBlock>` | `pub` · `#[serde(default)]` |

<details><summary>Methods (4)</summary>

`from_user_text`, `assistant_text`, `reasoning_text`, `tool_uses`

</details>

---

## `eos-llm-client/src/openai.rs`

#### `OpenAiClient`  ·  _struct_  ·  derives: `Debug`  ·  [L37]

The OpenAI Responses streaming client: encodes an `LlmRequest` to `/v1/responses` and decodes the SSE response into the same normalized events as the Anthropic path.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `http` | `reqwest::Client` |  |
| `endpoint` | `reqwest::Url` |  |
| `auth` | `Arc<Auth>` |  |
| `retry` | `Arc<RetryConfig>` |  |

**Trait impls**: `LlmClient`

<details><summary>Methods (2)</summary>

`new`, `build_headers`

</details>

#### `ToolItem`  ·  _struct_  ·  derives: `Debug`  ·  private  ·  [L90]

In-flight reassembly state for one OpenAI function-call item (call id, name, accumulating argument JSON).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `call_id` | `String` |  |
| `name` | `String` |  |
| `arguments` | `String` |  |

#### `OpenAiState`  ·  _struct_  ·  derives: `Debug, Default`  ·  private  ·  [L100]

Decoder state across the whole OpenAI response stream (accumulated text, in-progress function-call items keyed by `item_id`, finalized tool-use blocks).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `text` | `String` |  |
| `items` | `HashMap<String, ToolItem>` |  |
| `tools` | `Vec<ContentBlock>` |  |

---

## `eos-llm-client/src/sse.rs`

#### `SseFrameSplitter`  ·  _struct_  ·  derives: `Debug, Default`  ·  pub(crate)  ·  [L28]

A pushable, allocation-light SSE frame splitter: bytes are pushed in arbitrary chunks and complete frames (lines between blank-line boundaries) emit as they close, tolerating `\n` and `\r\n`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `buf` | `Vec<u8>` |  |
| `current` | `Vec<String>` |  |

<details><summary>Methods (3)</summary>

`push`, `finish`, `consume_line`

</details>

---

## `eos-llm-client/src/types.rs`

#### `UsageSnapshot`  ·  _struct_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema`  ·  [L19]

Token usage reported by a model provider (prompt + completion token counts).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `input_tokens` | `u32` | `pub` |
| `output_tokens` | `u32` | `pub` |

<details><summary>Methods (1)</summary>

`total_tokens`

</details>

#### `ToolSpec`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  #[non_exhaustive]  ·  [L41]

A neutral tool declaration sent to the model; the provider encoders project it (Anthropic drops `output_schema`, OpenAI maps it).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `String` | `pub` |
| `description` | `String` | `pub` |
| `input_schema` | `JsonObject` | `pub` |
| `output_schema` | `Option<JsonObject>` | `pub` |

<details><summary>Methods (1)</summary>

`new`

</details>

#### `ToolChoice`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L78]

How the model should choose among the offered tools; the per-provider wire shape is produced by the encoders.

**Variants**: `Auto`, `Any`, `Tool { name: String }`

#### `LlmRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L96]

A neutral model invocation request, built via `LlmRequest::builder`; `system_prompt` is a request field, never a `Message`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `model` | `String` | `pub` |
| `messages` | `Vec<Message>` | `pub` |
| `system_prompt` | `Option<String>` | `pub` |
| `max_tokens` | `u32` | `pub` |
| `tools` | `Vec<ToolSpec>` | `pub` |
| `tool_choice` | `Option<ToolChoice>` | `pub` |

<details><summary>Methods (1)</summary>

`builder`

</details>

#### `LlmRequestBuilder`  ·  _struct_  ·  derives: `Debug, Clone`  ·  #[must_use = "..."]  ·  [L131]

Builder for `LlmRequest` (`api-builder-pattern`, `api-builder-must-use`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `model` | `String` |  |
| `messages` | `Vec<Message>` |  |
| `system_prompt` | `Option<String>` |  |
| `max_tokens` | `u32` |  |
| `tools` | `Vec<ToolSpec>` |  |
| `tool_choice` | `Option<ToolChoice>` |  |

<details><summary>Methods (7)</summary>

`messages`, `message`, `system_prompt`, `max_tokens`, `tools`, `tool_choice`, `build`

</details>
