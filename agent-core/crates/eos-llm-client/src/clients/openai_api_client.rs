//! The `OpenAI` Responses client: encode an [`LlmRequest`] to `/v1/responses`
//! (`stream: true`), decode the SSE response into the **same** normalized
//! [`LlmStreamEvent`] variants as the Anthropic path (LSP substitutability), and
//! wrap attempts in the retry gate.
//!
//! No Python source — this is the SDK-free Responses-API counterpart. Decode
//! normalizes `response.output_text.delta` → `AssistantTextDelta` and
//! function-call argument deltas → a single `ToolUseDelta` per call at
//! `response.function_call_arguments.done` (keyed by `call_id`, not `item_id`).
//! Encode maps `ToolSpec.output_schema` when present (GC-llm-client-02).

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use eos_config::RetryConfig;
use eos_types::ToolUseId;
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::auth::Auth;
use crate::client::{build_endpoint, open_stream, LlmClient, LlmStream};
use crate::error::ProviderError;
use crate::events::{LlmStreamEvent, StopReason};
use crate::message::{ContentBlock, Message, MessageRole};
use crate::retry::retry_stream;
use crate::sse::{json_str, json_u32, parse_sse_value, parse_tool_args};
use crate::types::{LlmRequest, ToolChoice, ToolSpec, UsageSnapshot};

/// The Responses streaming endpoint path.
const RESPONSES_PATH: &str = "/v1/responses";
/// The ChatGPT-backed Codex Responses endpoint path.
const CODEX_RESPONSES_PATH: &str = "/responses";

/// The `OpenAI` Responses streaming client.
#[derive(Debug)]
pub struct OpenAiApiClient {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    auth: Arc<Auth>,
    retry: Arc<RetryConfig>,
    request_dialect: OpenAiRequestDialect,
}

#[derive(Debug, Clone, Copy)]
enum OpenAiRequestDialect {
    PublicResponses,
    CodexResponses,
}

impl OpenAiApiClient {
    /// Construct a client for `base_url` (e.g. `https://api.openai.com`).
    pub fn new(base_url: &str, auth: Auth, retry: Arc<RetryConfig>) -> Result<Self, ProviderError> {
        Self::new_with_path(
            base_url,
            RESPONSES_PATH,
            OpenAiRequestDialect::PublicResponses,
            auth,
            retry,
        )
    }

    /// Construct a client for the ChatGPT-backed Codex endpoint.
    pub(crate) fn new_codex_backend(
        base_url: &str,
        auth: Auth,
        retry: Arc<RetryConfig>,
    ) -> Result<Self, ProviderError> {
        Self::new_with_path(
            base_url,
            CODEX_RESPONSES_PATH,
            OpenAiRequestDialect::CodexResponses,
            auth,
            retry,
        )
    }

    fn new_with_path(
        base_url: &str,
        path: &str,
        request_dialect: OpenAiRequestDialect,
        auth: Auth,
        retry: Arc<RetryConfig>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            http: reqwest::Client::new(),
            endpoint: build_endpoint(base_url, path)?,
            auth: Arc::new(auth),
            retry,
            request_dialect,
        })
    }

    fn build_headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        self.auth.apply(&mut headers)?;
        Ok(headers)
    }
}

#[async_trait::async_trait]
impl LlmClient for OpenAiApiClient {
    async fn stream_message(&self, request: LlmRequest) -> Result<LlmStream, ProviderError> {
        let body = serde_json::to_vec(&encode_openai_body_with_dialect(
            &request,
            self.request_dialect,
        ))
        .map_err(|e| ProviderError::request(format!("request body serialization failed: {e}")))?;
        let body = Bytes::from(body);
        let headers = self.build_headers()?;
        let http = self.http.clone();
        let url = self.endpoint.clone();

        let factory = move || {
            let http = http.clone();
            let url = url.clone();
            let headers = headers.clone();
            let body = body.clone();
            Box::pin(open_stream(http, url, headers, body, |frames, rid| {
                decode_openai(frames, rid)
            })) as BoxFuture<'static, Result<LlmStream, ProviderError>>
        };
        Ok(retry_stream((*self.retry).clone(), factory))
    }
}

/// In-flight reassembly state for one function-call item.
#[derive(Debug)]
struct ToolItem {
    call_id: String,
    name: String,
    arguments: String,
}

/// Decoder state across the whole response stream. Usage and stop reason are
/// read directly from the `response.completed` payload, so they are not held
/// here.
#[derive(Debug, Default)]
struct OpenAiState {
    text: String,
    /// `item_id` → in-progress function call.
    items: HashMap<String, ToolItem>,
    /// Finalized tool-use blocks in completion order.
    tools: Vec<ContentBlock>,
}

/// Decode an `OpenAI` Responses SSE frame stream into normalized events.
fn decode_openai<S>(
    frames: S,
    request_id: Option<String>,
) -> impl Stream<Item = Result<LlmStreamEvent, ProviderError>> + Send
where
    S: Stream<Item = Result<String, ProviderError>> + Send,
{
    async_stream::stream! {
        let mut state = OpenAiState::default();
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
            let value = match parse_sse_value(&frame, &request_id, "openai", frame_index) {
                Ok(Some(value)) => value,
                Ok(None) => continue,
                Err(err) => {
                    yield Err(err);
                    return;
                }
            };

            match value.get("type").and_then(Value::as_str) {
                Some("response.output_text.delta") => {
                    let text = json_str(&value, &["delta"]);
                    state.text.push_str(&text);
                    yield Ok(LlmStreamEvent::AssistantTextDelta { text });
                }
                Some("response.output_item.added") => {
                    let item = &value["item"];
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let item_id = json_str(item, &["id"]);
                        state.items.insert(
                            item_id,
                            ToolItem {
                                call_id: json_str(item, &["call_id"]),
                                name: json_str(item, &["name"]),
                                arguments: String::new(),
                            },
                        );
                    }
                }
                Some("response.function_call_arguments.delta") => {
                    let item_id = json_str(&value, &["item_id"]);
                    let delta = json_str(&value, &["delta"]);
                    if let Some(item) = state.items.get_mut(&item_id) {
                        item.arguments.push_str(&delta);
                    }
                }
                Some("response.function_call_arguments.done") => {
                    let item_id = json_str(&value, &["item_id"]);
                    if let Some(item) = state.items.remove(&item_id) {
                        let input = parse_tool_args(&item.arguments);
                        let tool_use_id = match ToolUseId::try_from(item.call_id.as_str()) {
                            Ok(id) => id,
                            Err(_) => {
                                yield Err(ProviderError::decode(
                                    request_id.clone(),
                                    "function call missing call_id",
                                ));
                                return;
                            }
                        };
                        state.tools.push(ContentBlock::ToolUse {
                            tool_use_id: tool_use_id.clone(),
                            name: item.name.clone(),
                            input: input.clone(),
                        });
                        yield Ok(LlmStreamEvent::ToolUseDelta {
                            tool_use_id,
                            name: item.name,
                            input,
                        });
                    }
                }
                Some("response.completed") => {
                    let response = &value["response"];
                    let usage = UsageSnapshot {
                        input_tokens: json_u32(response, &["usage", "input_tokens"]),
                        output_tokens: json_u32(response, &["usage", "output_tokens"]),
                    };
                    let stop_reason = response
                        .get("stop_reason")
                        .and_then(Value::as_str)
                        .map(StopReason::parse);

                    let mut content = Vec::new();
                    if !state.text.is_empty() {
                        content.push(ContentBlock::Text {
                            text: std::mem::take(&mut state.text),
                        });
                    }
                    content.append(&mut state.tools);

                    yield Ok(LlmStreamEvent::AssistantMessageComplete {
                        message: Message {
                            role: MessageRole::Assistant,
                            content,
                        },
                        usage,
                        stop_reason,
                    });
                    return;
                }
                _ => {}
            }
        }
    }
}

/// Encode an [`LlmRequest`] into an `OpenAI` `/v1/responses` request body.
#[cfg(test)]
pub(crate) fn encode_openai_body(request: &LlmRequest) -> Value {
    encode_openai_body_with_dialect(request, OpenAiRequestDialect::PublicResponses)
}

fn encode_openai_body_with_dialect(request: &LlmRequest, dialect: OpenAiRequestDialect) -> Value {
    let input: Vec<Value> = request
        .messages
        .iter()
        .flat_map(serialize_openai_message)
        .collect();

    let mut body = json!({
        "model": request.model,
        "input": input,
        "stream": true,
        "store": false,
        "parallel_tool_calls": false,
        "include": [],
    });
    if matches!(dialect, OpenAiRequestDialect::PublicResponses) {
        body["max_output_tokens"] = json!(request.max_tokens);
    }
    if let Some(system) = &request.system_prompt {
        body["instructions"] = json!(system);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(serialize_openai_tool).collect());
    }
    if let Some(choice) = &request.tool_choice {
        body["tool_choice"] = encode_openai_tool_choice(choice, dialect);
    }
    if let Some(effort) = request.reasoning_effort {
        body["reasoning"] = json!({ "effort": effort.as_wire() });
    }
    body
}

/// Project one neutral message to Responses-API input items: a `message` item
/// for its text parts (if any), plus `function_call` / `function_call_output`
/// items for tool calls and results. `Reasoning` is dropped (managed by the
/// provider).
fn serialize_openai_message(message: &Message) -> Vec<Value> {
    let text_type = match message.role {
        MessageRole::User => "input_text",
        MessageRole::Assistant => "output_text",
    };
    let mut items = Vec::new();
    let mut text_parts = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                text_parts.push(json!({ "type": text_type, "text": text }));
            }
            ContentBlock::SystemNotification { text } => {
                text_parts.push(json!({
                    "type": text_type,
                    "text": format!("<system-reminder>\n{text}\n</system-reminder>"),
                }));
            }
            ContentBlock::ToolUse {
                tool_use_id,
                name,
                input,
            } => items.push(json!({
                "type": "function_call",
                "call_id": tool_use_id,
                "name": name,
                "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".to_owned()),
            })),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => items.push(json!({
                "type": "function_call_output",
                "call_id": tool_use_id,
                "output": content,
            })),
            ContentBlock::Reasoning { .. } => {}
        }
    }
    if !text_parts.is_empty() {
        items.insert(
            0,
            json!({ "type": "message", "role": message.role.as_wire(), "content": text_parts }),
        );
    }
    items
}

/// Project a tool spec to a Responses function tool entry, mapping
/// `output_schema` when present (GC-llm-client-02).
fn serialize_openai_tool(spec: &ToolSpec) -> Value {
    let mut tool = json!({
        "type": "function",
        "name": spec.name,
        "description": spec.description,
        "parameters": spec.input_schema,
    });
    if let Some(output_schema) = &spec.output_schema {
        tool["output_schema"] = json!(output_schema);
    }
    tool
}

fn encode_openai_tool_choice(choice: &ToolChoice, dialect: OpenAiRequestDialect) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Any => json!("required"),
        ToolChoice::Tool { name } => match dialect {
            OpenAiRequestDialect::PublicResponses => json!({ "type": "function", "name": name }),
            OpenAiRequestDialect::CodexResponses => json!("required"),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use eos_types::JsonObject;

    use crate::sse::frame_stream;

    fn discriminant(event: &LlmStreamEvent) -> &'static str {
        match event {
            LlmStreamEvent::AssistantTextDelta { .. } => "text",
            LlmStreamEvent::ReasoningDelta { .. } => "reasoning",
            LlmStreamEvent::ToolUseDelta { .. } => "tool_use",
            LlmStreamEvent::AssistantMessageComplete { .. } => "complete",
        }
    }

    async fn decode_fixture(raw: &str) -> Vec<LlmStreamEvent> {
        let bytes = Bytes::from(raw.to_owned());
        let byte_stream = futures::stream::iter(vec![Ok::<Bytes, ProviderError>(bytes)]);
        decode_openai(frame_stream(byte_stream), Some("req-test".to_owned()))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect()
    }

    // AC-llm-client-10: an `OpenAI` fixture (text delta + function-call argument
    // deltas + response.completed) decodes into the same variant sequence as the
    // Anthropic path (LSP substitutability).
    #[tokio::test]
    async fn decodes_openai_responses_fixture() {
        let events = decode_fixture(include_str!("../../tests/fixtures/openai/full.sse")).await;

        assert_eq!(
            events[0],
            LlmStreamEvent::AssistantTextDelta { text: "Hi".into() }
        );
        match &events[1] {
            LlmStreamEvent::ToolUseDelta {
                tool_use_id,
                name,
                input,
            } => {
                assert_eq!(tool_use_id.as_str(), "call_9");
                assert_eq!(name, "read_file");
                assert_eq!(input.get("path").and_then(Value::as_str), Some("foo.txt"));
            }
            other => panic!("expected tool_use delta, got {other:?}"),
        }
        match &events[2] {
            LlmStreamEvent::AssistantMessageComplete {
                usage,
                stop_reason,
                message,
            } => {
                assert_eq!(usage.input_tokens, 5);
                assert_eq!(usage.output_tokens, 4);
                assert_eq!(*stop_reason, Some(StopReason::ToolUse));
                assert_eq!(message.content.len(), 2); // Text + ToolUse
            }
            other => panic!("expected complete, got {other:?}"),
        }

        // Variant sequence matches the Anthropic text+tool path.
        let openai_seq: Vec<_> = events.iter().map(discriminant).collect();
        let anthropic_events: Vec<LlmStreamEvent> = {
            let bytes = Bytes::from(
                include_str!("../../tests/fixtures/anthropic/text_tool.sse").to_owned(),
            );
            let byte_stream = futures::stream::iter(vec![Ok::<Bytes, ProviderError>(bytes)]);
            crate::clients::anthropic_api_client::decode_anthropic_for_test(frame_stream(
                byte_stream,
            ))
            .await
        };
        let anthropic_seq: Vec<_> = anthropic_events.iter().map(discriminant).collect();
        assert_eq!(
            openai_seq, anthropic_seq,
            "providers are variant-substitutable"
        );
    }

    // AC-llm-client-06 (openai side): encode retains output_schema in the
    // function tool entry (spec-named proving test).
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

        let request = LlmRequest::builder("gpt").tools(vec![spec]).build();
        let body = encode_openai_body(&request);
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], json!("function"));
        assert_eq!(tool["name"], json!("read_file"));
        assert!(tool.get("parameters").is_some());
        assert!(
            tool.get("output_schema").is_some(),
            "openai maps output_schema"
        );
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["parallel_tool_calls"], json!(false));
        assert_eq!(body["include"], json!([]));
        assert_eq!(body["max_output_tokens"], json!(request.max_tokens));
    }

    #[test]
    fn encode_codex_response_body_uses_chatgpt_backend_shape() {
        let mut input_schema = JsonObject::new();
        input_schema.insert("type".into(), json!("object"));
        let spec = ToolSpec::new("smoke", "Smoke tool", input_schema, None);

        let request = LlmRequest::builder("gpt-5.5")
            .tools(vec![spec])
            .tool_choice(ToolChoice::Tool {
                name: "smoke".to_owned(),
            })
            .reasoning_effort(crate::types::ReasoningEffort::Medium)
            .max_tokens(256)
            .build();
        let body = encode_openai_body_with_dialect(&request, OpenAiRequestDialect::CodexResponses);

        assert!(body.get("max_output_tokens").is_none());
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["tool_choice"], json!("required"));
        assert_eq!(body["parallel_tool_calls"], json!(false));
        assert_eq!(body["include"], json!([]));
        assert_eq!(body["reasoning"], json!({ "effort": "medium" }));
    }
}
