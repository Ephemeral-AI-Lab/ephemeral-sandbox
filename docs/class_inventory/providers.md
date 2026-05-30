# Module `providers` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/providers/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**16 classes across 6 files.**

The `providers` module is EphemeralOS's LLM provider client layer: it owns how the agent loop authenticates against, requests, and streams completions from upstream model backends. Core types (`MessageRequest`, `UsageSnapshot`, and the `SupportsStreamingMessages` streaming protocol) define the request/response contract; `auth_strategy.py` supplies pluggable `AuthStrategy` implementations that distinguish `api_mode` (raw API key) from `coding_plan_mode` (Claude Code macOS-Keychain OAuth), and `errors.py` provides the status-code-carrying `EphemeralOSApiError` hierarchy (auth/rate-limit/request failures) used for cross-provider error categorization. The `provider.py` factory dispatches a `class_path` discriminator (with an `EOS_DISABLE_CODING_PLAN_MODE` guard) to concrete streaming clients under `clients/`: the native Anthropic SDK client, its OAuth coding-plan subclass, and the Codex Responses-API client that streams against ChatGPT's backend using JWT-extracted account credentials and a flat tool envelope.

## Contents

- **`providers/auth_strategy.py`** — `AuthStrategy`, `_ApiKeyStrategy`, `ClaudeCodeOAuthCredentialError`, `_ClaudeOAuthStrategy`
- **`providers/clients/anthropic_native.py`** — `AnthropicClient`
- **`providers/clients/coding_plan/anthropic.py`** — `AnthropicPlanClient`
- **`providers/clients/coding_plan/codex.py`** — `CodexCredentialIncompleteError`, `CodexResponsesClient`, `_CodexHttpError`
- **`providers/errors.py`** — `EphemeralOSApiError`, `AuthenticationFailure`, `RateLimitFailure`, `RequestFailure`
- **`providers/types.py`** — `UsageSnapshot`, `MessageRequest`, `SupportsStreamingMessages`

---

## `providers/auth_strategy.py`

#### `AuthStrategy`  ·  _protocol_  ·  bases: `Protocol`  ·  [L29]

Protocol defining a tagged Anthropic auth strategy that yields SDK auth kwargs and a refresh hook.

**Fields**

| name | type | default |
|------|------|---------|
| `llm_client_mode` | `LlmClientMode` |  |

<details><summary>Methods (2)</summary>

`get_auth_kwargs`, `refresh`

</details>

#### `_ApiKeyStrategy`  ·  _class_  ·  [L36]

Auth strategy authenticating via a static API key or auth token, with a no-op refresh.

**Fields**

| name | type | default |
|------|------|---------|
| `llm_client_mode` | `LlmClientMode` | `LLM_CLIENT_MODE_API` |

**Instance attributes**: `_api_key`, `_use_auth_token`

<details><summary>Methods (3)</summary>

`__init__`, `get_auth_kwargs`, `refresh`

</details>

#### `ClaudeCodeOAuthCredentialError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L67]

Raised when macOS Keychain entry `Claude Code-credentials` is unreadable.

#### `_ClaudeOAuthStrategy`  ·  _class_  ·  [L71]

Reads `Claude Code-credentials` keychain entry, returns Bearer token.

**Fields**

| name | type | default |
|------|------|---------|
| `llm_client_mode` | `LlmClientMode` | `LLM_CLIENT_MODE_CODING_PLAN` |

**Class variables**: `KEYCHAIN_SERVICE = 'Claude Code-credentials'`

**Instance attributes**: `_access_token`

<details><summary>Methods (4)</summary>

`__init__`, `_read_keychain`, `get_auth_kwargs`, `refresh`

</details>

---

## `providers/clients/anthropic_native.py`

#### `AnthropicClient`  ·  _class_  ·  [L72]

Anthropic-native streaming client.

**Instance attributes**: `_base_url`, `_system_prefix`, `_client`, `_auth_strategy`

<details><summary>Methods (8)</summary>

`__init__`, `_build_sdk_client`, `aclose`, `stream_message`, `_stream_once`, `_is_retryable`, `_translate_error`, `_emit_coding_plan_mode_error`

</details>

---

## `providers/clients/coding_plan/anthropic.py`

#### `AnthropicPlanClient`  ·  _class_  ·  bases: `AnthropicClient`  ·  [L14]

`AnthropicClient` configured with the Claude Code OAuth strategy.

<details><summary>Methods (1)</summary>

`__init__`

</details>

---

## `providers/clients/coding_plan/codex.py`

#### `CodexCredentialIncompleteError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L65]

Raised when ``~/.codex/auth.json`` is missing or shape-malformed (plan §A15).

#### `CodexResponsesClient`  ·  _class_  ·  [L153]

Codex Responses-API streaming client (plan §A4/§A15).

**Fields**

| name | type | default |
|------|------|---------|
| `llm_client_mode` | `str` | `LLM_CLIENT_MODE_CODING_PLAN` |

**Instance attributes**: `_auth_path`, `_config_path`, `_chatgpt_account_id`, `_model`, `_access_token`

<details><summary>Methods (8)</summary>

`__init__`, `_load_codex_auth`, `_refresh_credentials`, `build_headers`, `build_body`, `stream_message`, `_translate_http_error`, `_emit_coding_plan_mode_error`

</details>

#### `_CodexHttpError`  ·  _exception_  ·  bases: `Exception`  ·  [L471]

Internal carrier for HTTP-status failures from the Codex stream.

**Instance attributes**: `status_code`, `message`, `request_id`

<details><summary>Methods (1)</summary>

`__init__`

</details>

---

## `providers/errors.py`

#### `EphemeralOSApiError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L6]

Base class for upstream API failures.

**Instance attributes**: `status_code`, `request_id`

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `AuthenticationFailure`  ·  _exception_  ·  bases: `EphemeralOSApiError`  ·  [L26]

Raised when the upstream service rejects the provided credentials.

#### `RateLimitFailure`  ·  _exception_  ·  bases: `EphemeralOSApiError`  ·  [L30]

Raised when the upstream service rejects the request due to rate limits.

#### `RequestFailure`  ·  _exception_  ·  bases: `EphemeralOSApiError`  ·  [L34]

Raised for generic request or transport failures.

---

## `providers/types.py`

#### `UsageSnapshot`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L21]

Token usage returned by the model provider.

**Fields**

| name | type | default |
|------|------|---------|
| `input_tokens` | `int` | `0` |
| `output_tokens` | `int` | `0` |

<details><summary>Methods (1)</summary>

`total_tokens`

</details>

#### `MessageRequest`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L39]

Input parameters for a model invocation.

**Fields**

| name | type | default |
|------|------|---------|
| `model` | `str` |  |
| `messages` | `list[Message]` | `field(default_factory=list)` |
| `system_prompt` | `str \| None` | `None` |
| `max_tokens` | `int` | `32768` |
| `tools` | `list[dict[str, Any]]` | `field(default_factory=list)` |
| `tool_choice` | `dict[str, Any] \| None` | `None` |

#### `SupportsStreamingMessages`  ·  _protocol_  ·  bases: `Protocol`  ·  [L55]

Protocol used by the query engine in tests and production.

<details><summary>Methods (1)</summary>

`stream_message`

</details>

