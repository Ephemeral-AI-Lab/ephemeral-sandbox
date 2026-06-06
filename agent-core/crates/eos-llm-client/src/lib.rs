//! `eos-llm-client` — the provider-neutral LLM vocabulary and the direct
//! HTTP/SSE clients that turn an [`LlmRequest`] into a stream of normalized
//! [`LlmStreamEvent`]s.
//!
//! This crate is the single boundary where a wire protocol (Anthropic Messages,
//! `OpenAI` Responses) is encoded from neutral types and decoded back into neutral
//! types. It owns [`Message`]/[`ContentBlock`], [`UsageSnapshot`], [`LlmRequest`],
//! [`LlmStreamEvent`], [`ProviderError`], [`ToolSpec`], and the [`LlmClient`]
//! seam (anchor §5). It depends on no provider SDK — direct `reqwest` + a
//! hand-rolled SSE splitter only — and owns no engine-domain events, tool
//! registry, or lifecycle policy.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod auth;
mod client;
mod clients;
mod error;
mod events;
mod message;
mod retry;
mod sse;
mod types;

pub use auth::Auth;
pub use client::{LlmClient, LlmStream};
pub use clients::{
    AnthropicApiClient, ClaudeCodingPlanClient, CodexCodingPlanClient, OpenAiApiClient,
};
pub use error::{ProviderError, ProviderErrorKind};
pub use events::{LlmStreamEvent, StopReason};
pub use message::{ContentBlock, Message, MessageRole};
pub use types::{
    LlmRequest, LlmRequestBuilder, ReasoningEffort, ToolChoice, ToolSpec, UsageSnapshot,
    DEFAULT_MAX_TOKENS,
};
