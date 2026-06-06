//! Neutral request-side value types: token usage, the model-facing tool
//! declaration, the tool-choice control, and the request itself.
//!
//! Provider-specific wire projection lives under `clients/`, never here.

use eos_types::JsonObject;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::message::Message;

/// Token usage reported by a model provider.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct UsageSnapshot {
    /// Prompt tokens consumed.
    pub input_tokens: u32,
    /// Completion tokens produced.
    pub output_tokens: u32,
}

impl UsageSnapshot {
    /// Total accounted tokens (`input + output`).
    #[must_use]
    pub fn total_tokens(&self) -> u32 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// A neutral tool declaration sent to the model.
///
/// Owned here; `eos-tools` depends on this crate to author it from
/// `schemars`-generated input/output schemas. The provider encoders project it
/// into each upstream wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ToolSpec {
    /// The tool name the model calls.
    pub name: String,
    /// The model-facing description.
    pub description: String,
    /// The JSON Schema of the tool's input (a JSON object).
    pub input_schema: JsonObject,
    /// The JSON Schema of the tool's output, when authored. Anthropic encode
    /// drops this; `OpenAI` encode maps it.
    pub output_schema: Option<JsonObject>,
}

impl ToolSpec {
    /// Construct a tool declaration. (`ToolSpec` is `#[non_exhaustive]`, so
    /// downstream `eos-tools` constructs it through this constructor.)
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: JsonObject,
        output_schema: Option<JsonObject>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            output_schema,
        }
    }
}

/// How the model should choose among the offered tools.
///
/// The per-provider wire shape is produced by the provider encoders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// The model decides whether to call a tool.
    Auto,
    /// The model must call some tool.
    Any,
    /// The model must call the named tool.
    Tool {
        /// The forced tool name.
        name: String,
    },
}

/// Provider reasoning-effort hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasoningEffort {
    /// Minimal reasoning effort.
    Minimal,
    /// Low reasoning effort.
    Low,
    /// Medium reasoning effort.
    Medium,
    /// High reasoning effort.
    High,
}

impl ReasoningEffort {
    /// Wire token used by provider request bodies.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A neutral model invocation request.
///
/// Source: `types.py::MessageRequest`. Built via [`LlmRequest::builder`]
/// (`api-builder-pattern`). `system_prompt` is a request field, never a
/// [`Message`] (GC-llm-client-03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmRequest {
    /// Opaque provider model key (`model_key` upstream).
    pub model: String,
    /// The conversation so far.
    pub messages: Vec<Message>,
    /// The system prompt, when set. Sent as a request field, not a message.
    pub system_prompt: Option<String>,
    /// Maximum completion tokens (default 32768).
    pub max_tokens: u32,
    /// The tools offered to the model (built at agent spawn upstream).
    pub tools: Vec<ToolSpec>,
    /// The tool-choice control, when forced.
    pub tool_choice: Option<ToolChoice>,
    /// Optional reasoning-effort hint for providers that support it.
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// The `max_tokens` default mirrors `MessageRequest.max_tokens = 32768`.
pub const DEFAULT_MAX_TOKENS: u32 = 32768;

impl LlmRequest {
    /// Start building a request for `model`.
    pub fn builder(model: impl Into<String>) -> LlmRequestBuilder {
        LlmRequestBuilder {
            model: model.into(),
            messages: Vec::new(),
            system_prompt: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            tools: Vec::new(),
            tool_choice: None,
            reasoning_effort: None,
        }
    }
}

/// Builder for [`LlmRequest`] (`api-builder-pattern`, `api-builder-must-use`).
#[derive(Debug, Clone)]
#[must_use = "an LlmRequestBuilder does nothing until `.build()` is called"]
pub struct LlmRequestBuilder {
    model: String,
    messages: Vec<Message>,
    system_prompt: Option<String>,
    max_tokens: u32,
    tools: Vec<ToolSpec>,
    tool_choice: Option<ToolChoice>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl LlmRequestBuilder {
    /// Set the full conversation.
    pub fn messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    /// Append one message.
    pub fn message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    /// Set the system prompt.
    pub fn system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    /// Override the completion-token cap.
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set the offered tools.
    pub fn tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    /// Force a tool-choice control.
    pub fn tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = Some(tool_choice);
        self
    }

    /// Set a provider reasoning-effort hint.
    pub fn reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Finish building the request.
    #[must_use]
    pub fn build(self) -> LlmRequest {
        LlmRequest {
            model: self.model,
            messages: self.messages,
            system_prompt: self.system_prompt,
            max_tokens: self.max_tokens,
            tools: self.tools,
            tool_choice: self.tool_choice,
            reasoning_effort: self.reasoning_effort,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_tokens_sums() {
        let u = UsageSnapshot {
            input_tokens: 10,
            output_tokens: 5,
        };
        assert_eq!(u.total_tokens(), 15);
        assert_eq!(UsageSnapshot::default().total_tokens(), 0);
    }

    #[test]
    fn builder_defaults_and_overrides() {
        let req = LlmRequest::builder("claude-opus-4-8").build();
        assert_eq!(req.model, "claude-opus-4-8");
        assert_eq!(req.max_tokens, DEFAULT_MAX_TOKENS);
        assert!(req.messages.is_empty());
        assert!(req.system_prompt.is_none());

        let req = LlmRequest::builder("m")
            .system_prompt("be terse")
            .max_tokens(64)
            .tool_choice(ToolChoice::Any)
            .reasoning_effort(ReasoningEffort::Medium)
            .build();
        assert_eq!(req.system_prompt.as_deref(), Some("be terse"));
        assert_eq!(req.max_tokens, 64);
        assert_eq!(req.tool_choice, Some(ToolChoice::Any));
        assert_eq!(req.reasoning_effort, Some(ReasoningEffort::Medium));
    }
}
