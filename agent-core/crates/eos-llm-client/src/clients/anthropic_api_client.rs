//! The Anthropic Messages client: encode an [`LlmRequest`] to `/v1/messages`
//! (`stream: true`), decode the SSE response into normalized
//! [`LlmStreamEvent`]s, and wrap attempts in the retry gate.
//!
//! Source: `providers/clients/anthropic_native.py` — the official SDK is
//! replaced by direct `reqwest` + the `sse.rs` splitter. All provider projection
//! lives here (GC-llm-client-02): Anthropic encode drops `Reasoning` blocks and
//! `ToolSpec.output_schema`, and omits `metadata`/`is_terminal` from
//! `tool_result` wire bodies. Tool-use blocks are emitted mid-stream at
//! `content_block_stop` (the Python advantage). Decode is a pure
//! frame-stream → event-stream function, independent of `reqwest`, so fixtures
//! replay through it with no HTTP.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use eos_config::RetryConfig;
use eos_types::ToolUseId;
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde_json::{json, Value};

use crate::auth::Auth;
use crate::client::{build_endpoint, open_stream, LlmClient, LlmStream};
use crate::error::ProviderError;
use crate::events::{LlmStreamEvent, StopReason};
use crate::message::{ContentBlock, Message, MessageRole};
use crate::retry::retry_stream;
use crate::sse::{json_str, json_u32, json_usize, parse_sse_value, parse_tool_args};
use crate::types::{LlmRequest, ToolChoice, ToolSpec, UsageSnapshot};

/// The mandatory Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The Messages streaming endpoint path.
const MESSAGES_PATH: &str = "/v1/messages";
/// The Claude Code system identity prepended for OAuth-backed transport.
const CLAUDE_CODE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// The Anthropic-native streaming client.
#[derive(Debug)]
pub struct AnthropicApiClient {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    auth: Arc<Auth>,
    retry: Arc<RetryConfig>,
    extra_headers: HeaderMap,
    prepend_claude_code_system_prompt: bool,
}

impl AnthropicApiClient {
    /// Construct a client for `base_url` (e.g. `https://api.anthropic.com`).
    ///
    /// Returns the outer `Err` only on a malformed base url
    /// (`api-parse-dont-validate`).
    pub fn new(base_url: &str, auth: Auth, retry: Arc<RetryConfig>) -> Result<Self, ProviderError> {
        Self::new_with_options(base_url, auth, retry, HeaderMap::new(), false)
    }

    /// Construct a client for Claude coding-plan OAuth access tokens.
    pub(crate) fn new_claude_coding_plan(
        base_url: &str,
        auth: Auth,
        retry: Arc<RetryConfig>,
        beta_header: HeaderValue,
    ) -> Result<Self, ProviderError> {
        let mut extra_headers = HeaderMap::new();
        extra_headers.insert(HeaderName::from_static("anthropic-beta"), beta_header);
        extra_headers.insert(
            HeaderName::from_static("anthropic-dangerous-direct-browser-access"),
            HeaderValue::from_static("true"),
        );
        extra_headers.insert(USER_AGENT, HeaderValue::from_static("claude-cli/2.1.75"));
        extra_headers.insert(
            HeaderName::from_static("x-app"),
            HeaderValue::from_static("cli"),
        );
        Self::new_with_options(base_url, auth, retry, extra_headers, true)
    }

    fn new_with_options(
        base_url: &str,
        auth: Auth,
        retry: Arc<RetryConfig>,
        extra_headers: HeaderMap,
        prepend_claude_code_system_prompt: bool,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            http: reqwest::Client::new(),
            endpoint: build_endpoint(base_url, MESSAGES_PATH)?,
            auth: Arc::new(auth),
            retry,
            extra_headers,
            prepend_claude_code_system_prompt,
        })
    }

    fn build_headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        for (name, value) in &self.extra_headers {
            headers.insert(name, value.clone());
        }
        self.auth.apply(&mut headers)?;
        Ok(headers)
    }
}

#[async_trait::async_trait]
impl LlmClient for AnthropicApiClient {
    async fn stream_message(&self, request: LlmRequest) -> Result<LlmStream, ProviderError> {
        // Synchronous build — the only outer-`Err` path (§5).
        let body_value = if self.prepend_claude_code_system_prompt {
            encode_anthropic_body_with_options(&request, true)
        } else {
            encode_anthropic_body(&request)
        };
        let body = serde_json::to_vec(&body_value).map_err(|e| {
            ProviderError::request(format!("request body serialization failed: {e}"))
        })?;
        let body = Bytes::from(body);
        let headers = self.build_headers()?;
        let http = self.http.clone();
        let url = self.endpoint.clone();

        // Each attempt replays the owned bytes; connect/status/decode errors
        // surface as stream items, not the outer `Err`. The shared transport
        // plumbing lives in `client::open_stream`; only the decode differs.
        let factory = move || {
            let http = http.clone();
            let url = url.clone();
            let headers = headers.clone();
            let body = body.clone();
            Box::pin(open_stream(http, url, headers, body, |frames, rid| {
                decode_anthropic(frames, rid)
            })) as BoxFuture<'static, Result<LlmStream, ProviderError>>
        };
        Ok(retry_stream((*self.retry).clone(), factory))
    }
}

/// In-flight reassembly state for one content block.
#[derive(Debug)]
struct BlockAccum {
    block_type: String,
    id: String,
    name: String,
    text: String,
    input_json: String,
}

/// Decoder state across the whole message stream.
#[derive(Debug, Default)]
struct AnthropicState {
    blocks: HashMap<usize, BlockAccum>,
    content: Vec<ContentBlock>,
    input_tokens: u32,
    output_tokens: u32,
    stop_reason: Option<String>,
}

/// Decode an Anthropic Messages SSE frame stream into normalized events.
///
/// Pure: independent of `reqwest`, so fixtures replay through it. `input_tokens`
/// comes from `message_start`, `output_tokens` from `message_delta` (the SDK's
/// `get_final_message` merge). Malformed frame JSON logs (content-free) and ends
/// the stream with a `Decode` error stamped with `request_id` (§8.7, §8.8).
fn decode_anthropic<S>(
    frames: S,
    request_id: Option<String>,
) -> impl Stream<Item = Result<LlmStreamEvent, ProviderError>> + Send
where
    S: Stream<Item = Result<String, ProviderError>> + Send,
{
    async_stream::stream! {
        let mut state = AnthropicState::default();
        let mut frame_index: usize = 0;
        futures::pin_mut!(frames);
        while let Some(frame) = frames.next().await {
            frame_index += 1;
            let frame = match frame {
                Ok(frame) => frame,
                Err(err) => {
                    yield Err(err);
                    return;
                }
            };
            let value = match parse_sse_value(&frame, &request_id, "anthropic", frame_index) {
                Ok(Some(value)) => value,
                Ok(None) => continue,
                Err(err) => {
                    yield Err(err);
                    return;
                }
            };

            match value.get("type").and_then(Value::as_str) {
                Some("message_start") => {
                    state.input_tokens = json_u32(&value, &["message", "usage", "input_tokens"]);
                }
                Some("content_block_start") => {
                    let index = json_usize(&value, &["index"]);
                    let block = &value["content_block"];
                    state.blocks.insert(
                        index,
                        BlockAccum {
                            block_type: json_str(block, &["type"]),
                            id: json_str(block, &["id"]),
                            name: json_str(block, &["name"]),
                            text: String::new(),
                            input_json: String::new(),
                        },
                    );
                }
                Some("content_block_delta") => {
                    let index = json_usize(&value, &["index"]);
                    let delta = &value["delta"];
                    match delta.get("type").and_then(Value::as_str) {
                        Some("text_delta") => {
                            let text = json_str(delta, &["text"]);
                            if let Some(block) = state.blocks.get_mut(&index) {
                                block.text.push_str(&text);
                            }
                            yield Ok(LlmStreamEvent::AssistantTextDelta { text });
                        }
                        Some("thinking_delta") => {
                            let text = json_str(delta, &["thinking"]);
                            if let Some(block) = state.blocks.get_mut(&index) {
                                block.text.push_str(&text);
                            }
                            yield Ok(LlmStreamEvent::ReasoningDelta { text });
                        }
                        Some("input_json_delta") => {
                            let partial = json_str(delta, &["partial_json"]);
                            if let Some(block) = state.blocks.get_mut(&index) {
                                block.input_json.push_str(&partial);
                            }
                        }
                        _ => {}
                    }
                }
                Some("content_block_stop") => {
                    let index = json_usize(&value, &["index"]);
                    if let Some(block) = state.blocks.remove(&index) {
                        match block.block_type.as_str() {
                            "tool_use" => {
                                let input = parse_tool_args(&block.input_json);
                                // An empty/missing tool-use id is a malformed
                                // stream here, not a tolerated default. Python
                                // passed the empty id through / synthesized a
                                // `toolu_<uuid>`, but the `ToolUseId` newtype
                                // rejects empty and the spec states default-id
                                // minting "lives in eos-types/engine, not here"
                                // (§6) — so this fails fast rather than minting
                                // or propagating an empty id. Anthropic always
                                // sends a `toolu_` id, so this never triggers.
                                let tool_use_id = match ToolUseId::try_from(block.id.as_str()) {
                                    Ok(id) => id,
                                    Err(_) => {
                                        yield Err(ProviderError::decode(
                                            request_id.clone(),
                                            "tool_use block missing id",
                                        ));
                                        return;
                                    }
                                };
                                state.content.push(ContentBlock::ToolUse {
                                    tool_use_id: tool_use_id.clone(),
                                    name: block.name.clone(),
                                    input: input.clone(),
                                });
                                yield Ok(LlmStreamEvent::ToolUseDelta {
                                    tool_use_id,
                                    name: block.name,
                                    input,
                                });
                            }
                            "text" => state.content.push(ContentBlock::Text { text: block.text }),
                            "thinking" => {
                                state.content.push(ContentBlock::Reasoning { text: block.text });
                            }
                            _ => {}
                        }
                    }
                }
                Some("message_delta") => {
                    if let Some(reason) = value["delta"].get("stop_reason").and_then(Value::as_str) {
                        state.stop_reason = Some(reason.to_owned());
                    }
                    state.output_tokens = json_u32(&value, &["usage", "output_tokens"]);
                }
                Some("message_stop") => {
                    yield Ok(LlmStreamEvent::AssistantMessageComplete {
                        message: Message {
                            role: MessageRole::Assistant,
                            content: std::mem::take(&mut state.content),
                        },
                        usage: UsageSnapshot {
                            input_tokens: state.input_tokens,
                            output_tokens: state.output_tokens,
                        },
                        stop_reason: state.stop_reason.as_deref().map(StopReason::parse),
                    });
                    return;
                }
                _ => {}
            }
        }
    }
}

/// Encode an [`LlmRequest`] into an Anthropic `/v1/messages` request body.
pub(crate) fn encode_anthropic_body(request: &LlmRequest) -> Value {
    encode_anthropic_body_with_options(request, false)
}

fn encode_anthropic_body_with_options(
    request: &LlmRequest,
    prepend_claude_code_system_prompt: bool,
) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|message| {
            let content: Vec<Value> = message
                .content
                .iter()
                .filter(|block| !matches!(block, ContentBlock::Reasoning { .. }))
                .map(serialize_anthropic_block)
                .collect();
            json!({ "role": message.role.as_wire(), "content": content })
        })
        .collect();

    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "max_tokens": request.max_tokens,
        "stream": true,
    });
    if prepend_claude_code_system_prompt {
        let mut system = vec![json!({
            "type": "text",
            "text": CLAUDE_CODE_SYSTEM_PROMPT,
        })];
        if let Some(prompt) = &request.system_prompt {
            system.push(json!({
                "type": "text",
                "text": prompt,
            }));
        }
        body["system"] = Value::Array(system);
    } else if let Some(system) = &request.system_prompt {
        body["system"] = json!(system);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(serialize_anthropic_tool).collect());
    }
    if let Some(choice) = &request.tool_choice {
        body["tool_choice"] = encode_anthropic_tool_choice(choice);
    }
    body
}

/// Project one neutral block to the Anthropic wire shape. `Reasoning` is
/// filtered before this is called; `tool_result` omits `metadata`/`is_terminal`
/// (§8.6).
fn serialize_anthropic_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
        ContentBlock::ToolUse {
            tool_use_id,
            name,
            input,
        } => json!({ "type": "tool_use", "id": tool_use_id, "name": name, "input": input }),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        }),
        ContentBlock::SystemNotification { text } => json!({
            "type": "text",
            "text": format!("<system-reminder>\n{text}\n</system-reminder>"),
        }),
        // Filtered out before this call; encoded defensively for totality.
        ContentBlock::Reasoning { text } => json!({ "type": "thinking", "text": text }),
    }
}

/// Project a tool spec to an Anthropic tool entry, dropping `output_schema`.
fn serialize_anthropic_tool(spec: &ToolSpec) -> Value {
    json!({
        "name": spec.name,
        "description": spec.description,
        "input_schema": spec.input_schema,
    })
}

fn encode_anthropic_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({ "type": "auto" }),
        ToolChoice::Any => json!({ "type": "any" }),
        ToolChoice::Tool { name } => json!({ "type": "tool", "name": name }),
    }
}

/// Decode an Anthropic SSE frame stream to events for cross-provider
/// substitutability tests (AC-llm-client-10, exercised from
/// `openai_api_client.rs`).
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) async fn decode_anthropic_for_test(
    frames: impl Stream<Item = Result<String, ProviderError>> + Send,
) -> Vec<LlmStreamEvent> {
    decode_anthropic(frames, None)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use eos_types::JsonObject;
    use tracing_test::traced_test;

    use crate::sse::frame_stream;

    async fn decode_fixture(raw: &str) -> Vec<Result<LlmStreamEvent, ProviderError>> {
        let bytes = Bytes::from(raw.to_owned());
        let byte_stream = futures::stream::iter(vec![Ok::<Bytes, ProviderError>(bytes)]);
        decode_anthropic(frame_stream(byte_stream), Some("req-test".to_owned()))
            .collect()
            .await
    }

    // AC-llm-client-01: full fixture decodes to reasoning/text deltas, then a
    // mid-stream tool_use delta with parsed args, then a complete with correct
    // usage (input from message_start, output from message_delta) + stop reason.
    #[tokio::test]
    async fn decodes_anthropic_sse_fixture() {
        let events: Vec<LlmStreamEvent> =
            decode_fixture(include_str!("../../tests/fixtures/anthropic/full.sse"))
                .await
                .into_iter()
                .map(Result::unwrap)
                .collect();

        assert_eq!(events.len(), 5);
        assert_eq!(
            events[0],
            LlmStreamEvent::ReasoningDelta {
                text: "Let me think".into()
            }
        );
        assert_eq!(
            events[1],
            LlmStreamEvent::AssistantTextDelta {
                text: "Hello".into()
            }
        );
        assert_eq!(
            events[2],
            LlmStreamEvent::AssistantTextDelta {
                text: " world".into()
            }
        );

        match &events[3] {
            LlmStreamEvent::ToolUseDelta {
                tool_use_id,
                name,
                input,
            } => {
                assert_eq!(tool_use_id.as_str(), "toolu_01");
                assert_eq!(name, "read_file");
                assert_eq!(input.get("path").and_then(Value::as_str), Some("foo.txt"));
            }
            other => panic!("expected tool_use delta, got {other:?}"),
        }

        match &events[4] {
            LlmStreamEvent::AssistantMessageComplete {
                message,
                usage,
                stop_reason,
            } => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 15);
                assert_eq!(*stop_reason, Some(StopReason::ToolUse));
                assert_eq!(message.content.len(), 3);
                assert_eq!(message.assistant_text(), "Hello world");
                assert_eq!(message.reasoning_text(), "Let me think");
                assert_eq!(message.tool_uses().count(), 1);
            }
            other => panic!("expected complete, got {other:?}"),
        }
    }

    // AC-llm-client-03 (event half): an Anthropic `thinking_delta` event decodes
    // to `LlmStreamEvent::ReasoningDelta` (the legacy "thinking" *block* half is
    // proven in message.rs::reasoning_compat_decode_maps_thinking).
    #[tokio::test]
    async fn reasoning_compat_decode_maps_thinking_delta() {
        let sse = concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"reasoning step\"}}\n",
        );
        let events: Vec<LlmStreamEvent> = decode_fixture(sse)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            events,
            vec![LlmStreamEvent::ReasoningDelta {
                text: "reasoning step".into()
            }]
        );
    }

    // AC-llm-client-06 (anthropic side): encode drops output_schema and drops
    // Reasoning content blocks.
    #[test]
    fn encode_projects_tools_per_provider() {
        let mut input_schema = JsonObject::new();
        input_schema.insert("type".into(), json!("object"));
        let mut output_schema = JsonObject::new();
        output_schema.insert("type".into(), json!("string"));
        let spec = ToolSpec::new(
            "read_file",
            "Read a file",
            input_schema,
            Some(output_schema),
        );

        let message = Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: "private".into(),
                },
                ContentBlock::Text { text: "hi".into() },
            ],
        };
        let request = LlmRequest::builder("claude")
            .message(message)
            .tools(vec![spec])
            .system_prompt("sys")
            .build();
        let body = encode_anthropic_body(&request);

        let tool = &body["tools"][0];
        assert_eq!(tool["name"], json!("read_file"));
        assert!(tool.get("input_schema").is_some());
        assert!(
            tool.get("output_schema").is_none(),
            "anthropic drops output_schema"
        );

        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(
            content.len(),
            1,
            "reasoning block dropped from wire messages"
        );
        assert_eq!(content[0]["type"], json!("text"));
        assert_eq!(content[0]["text"], json!("hi"));

        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["system"], json!("sys"));
    }

    #[test]
    fn tool_result_wire_omits_metadata_and_is_terminal() {
        let tuid: ToolUseId = "toolu_9".parse().unwrap();
        let mut metadata = JsonObject::new();
        metadata.insert("secret".into(), json!("nope"));
        let message = Message {
            role: MessageRole::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tuid,
                content: "ok".into(),
                is_error: false,
                metadata,
                is_terminal: true,
            }],
        };
        let body = encode_anthropic_body(&LlmRequest::builder("m").message(message).build());
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], json!("tool_result"));
        assert!(
            block.get("metadata").is_none(),
            "metadata omitted from wire"
        );
        assert!(
            block.get("is_terminal").is_none(),
            "is_terminal omitted from wire"
        );
        assert_eq!(block["content"], json!("ok"));
    }

    #[test]
    fn system_notification_wraps_in_reminder_tag() {
        let message = Message {
            role: MessageRole::User,
            content: vec![ContentBlock::SystemNotification {
                text: "stay on task".into(),
            }],
        };
        let body = encode_anthropic_body(&LlmRequest::builder("m").message(message).build());
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], json!("text"));
        assert_eq!(
            block["text"],
            json!("<system-reminder>\nstay on task\n</system-reminder>")
        );
    }

    #[test]
    fn claude_coding_plan_uses_oauth_transport_shape() {
        let client = AnthropicApiClient::new_claude_coding_plan(
            "https://api.anthropic.com",
            Auth::bearer("oauth-token"),
            Arc::new(RetryConfig::default()),
            HeaderValue::from_static("claude-code-20250219,oauth-2025-04-20"),
        )
        .unwrap();

        assert_eq!(
            client.endpoint.as_str(),
            "https://api.anthropic.com/v1/messages"
        );
        let headers = client.build_headers().unwrap();
        assert_eq!(
            headers.get("anthropic-beta").unwrap(),
            "claude-code-20250219,oauth-2025-04-20"
        );
        assert_eq!(headers.get("x-app").unwrap(), "cli");
        assert_eq!(headers.get("user-agent").unwrap(), "claude-cli/2.1.75");
        assert_eq!(
            headers
                .get("anthropic-dangerous-direct-browser-access")
                .unwrap(),
            "true"
        );
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer oauth-token"
        );
        assert!(headers.get("x-api-key").is_none());

        let body = encode_anthropic_body_with_options(
            &LlmRequest::builder("claude")
                .system_prompt("repo prompt")
                .message(Message::from_user_text("hi"))
                .build(),
            true,
        );
        assert_eq!(
            body["system"],
            json!([
                {
                    "type": "text",
                    "text": "You are Claude Code, Anthropic's official CLI for Claude."
                },
                {
                    "type": "text",
                    "text": "repo prompt"
                }
            ])
        );
    }

    // AC-llm-client-05: a forced SSE parse failure logs without echoing frame
    // content (no secrets/system_prompt/tool input in the log fields).
    #[tokio::test]
    #[traced_test]
    async fn parse_error_log_omits_secrets() {
        let results =
            decode_fixture(include_str!("../../tests/fixtures/anthropic/malformed.sse")).await;
        // The stream ends with exactly one Decode error item that preserves the
        // captured request-id (§8.8).
        let last = results.last().expect("at least one item");
        match last {
            Err(e) => {
                assert_eq!(e.kind, crate::error::ProviderErrorKind::Decode);
                assert_eq!(e.request_id.as_deref(), Some("req-test"));
            }
            Ok(event) => panic!("expected a decode error, got {event:?}"),
        }

        assert!(logs_contain("anthropic sse frame failed to parse"));
        assert!(!logs_contain("SUPERSECRET"), "log leaked frame content");
    }
}
