//! The normalized model-stream events.
//!
//! Source: `message/events.py`. Only the **four model-stream variants** move
//! here. Tool execution and notification events are engine-domain `ProviderStreamSource`
//! events owned by `eos-engine`, not provider stream events — they are
//! intentionally absent. The `agent_name`/`agent_run_id` identity fields are
//! dropped (the engine stamps those on its own envelope).

use eos_types::{JsonObject, ToolUseId};
use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::types::UsageSnapshot;

/// A single normalized event from a streaming model invocation.
///
/// The three `*Delta` variants are "visible output" for the retry gate (§8);
/// [`LlmStreamEvent::AssistantMessageComplete`] is the success terminus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LlmStreamEvent {
    /// Incremental assistant text (`AssistantTextDeltaEvent`).
    AssistantTextDelta {
        /// The text fragment.
        text: String,
    },
    /// Incremental model reasoning (Rust `ThinkingDeltaEvent`).
    ReasoningDelta {
        /// The reasoning fragment.
        text: String,
    },
    /// A fully-assembled tool call, emitted mid-stream at the block's close
    /// (`ToolUseDeltaEvent`) so the engine can begin executing it early.
    ToolUseDelta {
        /// The provider tool-use id.
        tool_use_id: ToolUseId,
        /// The tool name.
        name: String,
        /// The parsed tool arguments (`{}` if the provider sent malformed JSON).
        input: JsonObject,
    },
    /// The completed assistant message (`AssistantMessageCompleteEvent`).
    AssistantMessageComplete {
        /// The reassembled assistant message.
        message: Message,
        /// Final token usage.
        usage: UsageSnapshot,
        /// The provider stop reason, when reported.
        stop_reason: Option<StopReason>,
    },
}

/// A parsed provider stop reason (`api-parse-dont-validate`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model ended its turn.
    EndTurn,
    /// The completion-token cap was hit.
    MaxTokens,
    /// The model stopped to call a tool.
    ToolUse,
    /// A stop sequence was emitted.
    StopSequence,
    /// Any other provider-specific reason, preserved verbatim.
    Other(String),
}

impl StopReason {
    /// Parse a provider `stop_reason` string. Crate-internal: the decoders
    /// construct `StopReason` from the wire here; consumers receive the parsed
    /// `Option<StopReason>` on the event, never a raw provider string.
    #[must_use]
    pub(crate) fn parse(raw: &str) -> Self {
        match raw {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "tool_use" => Self::ToolUse,
            "stop_sequence" => Self::StopSequence,
            other => Self::Other(other.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_and_unknown_stop_reasons() {
        assert_eq!(StopReason::parse("end_turn"), StopReason::EndTurn);
        assert_eq!(StopReason::parse("max_tokens"), StopReason::MaxTokens);
        assert_eq!(StopReason::parse("tool_use"), StopReason::ToolUse);
        assert_eq!(StopReason::parse("stop_sequence"), StopReason::StopSequence);
        assert_eq!(
            StopReason::parse("refusal"),
            StopReason::Other("refusal".to_owned())
        );
    }
}
