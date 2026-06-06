//! The Claude coding-plan client.
//!
//! This is the explicit client surface for Claude Code OAuth access tokens. It
//! reuses the Anthropic Messages projection with the OAuth beta transport shape:
//! bearer auth, Claude Code identity headers, and
//! `anthropic-beta: claude-code-20250219,oauth-2025-04-20`.

use std::sync::Arc;

use eos_config::RetryConfig;
use reqwest::header::HeaderValue;

use crate::auth::Auth;
use crate::client::{LlmClient, LlmStream};
use crate::error::ProviderError;
use crate::types::LlmRequest;

use super::anthropic_api_client::AnthropicApiClient;

/// OAuth beta header required by Claude Code subscription access tokens.
const CLAUDE_CODING_PLAN_BETA: &str = "claude-code-20250219,oauth-2025-04-20";

/// Claude coding-plan streaming client.
#[derive(Debug)]
pub struct ClaudeCodingPlanClient {
    inner: AnthropicApiClient,
}

impl ClaudeCodingPlanClient {
    /// Construct a Claude coding-plan client for `base_url`.
    ///
    /// The caller supplies the OAuth access token from config; this crate does
    /// not read Claude Code credential stores or refresh tokens.
    pub fn new(
        base_url: &str,
        access_token: &str,
        retry: Arc<RetryConfig>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            inner: AnthropicApiClient::new_claude_coding_plan(
                base_url,
                Auth::bearer(access_token),
                retry,
                HeaderValue::from_static(CLAUDE_CODING_PLAN_BETA),
            )?,
        })
    }
}

#[async_trait::async_trait]
impl LlmClient for ClaudeCodingPlanClient {
    async fn stream_message(&self, request: LlmRequest) -> Result<LlmStream, ProviderError> {
        self.inner.stream_message(request).await
    }
}
