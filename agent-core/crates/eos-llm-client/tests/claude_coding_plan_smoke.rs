//! Live Claude coding-plan OAuth smoke test.
//!
//! This test accepts a Claude Code OAuth token from
//! `EOS_CLAUDE_CODING_PLAN_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, or from the
//! typed providers config when `providers.active = claude_coding_plan`. It
//! verifies the production Claude coding-plan transport by default. Set
//! `EOS_CLAUDE_CODING_PLAN_PROBE_NO_BETA=1` to also probe whether the normal
//! Anthropic bearer path works without Claude Code beta headers.

use std::sync::Arc;
use std::time::Duration;

use eos_config::{ProviderKind, ProvidersConfig, RetryConfig};
use eos_llm_client::{
    AnthropicApiClient, Auth, ClaudeCodingPlanClient, LlmClient, LlmRequest, LlmStreamEvent,
    Message, ProviderError,
};
use futures::StreamExt;

const TOKEN_ENV: &str = "EOS_CLAUDE_CODING_PLAN_OAUTH_TOKEN";
const CLAUDE_CODE_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const BASE_URL_ENV: &str = "EOS_CLAUDE_CODING_PLAN_BASE_URL";
const MODEL_ENV: &str = "EOS_CLAUDE_CODING_PLAN_MODEL";
const PROBE_NO_BETA_ENV: &str = "EOS_CLAUDE_CODING_PLAN_PROBE_NO_BETA";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const SMOKE_MARKER: &str = "eos-claude-smoke-ok";
const PROVIDER_EVENT_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::test]
async fn claude_coding_plan_real_token_gets_response() -> Result<(), ProviderError> {
    let Some(config) = load_smoke_config()? else {
        return Ok(());
    };
    let request = smoke_request(&config.model);
    let retry = Arc::new(config.retry.clone());

    if should_probe_no_beta() {
        let standard_client = AnthropicApiClient::new(
            &config.base_url,
            Auth::bearer(config.access_token.clone()),
            retry.clone(),
        )?;
        match run_smoke(
            "anthropic messages bearer no-beta probe",
            &standard_client,
            request.clone(),
        )
        .await
        {
            Ok(summary) => {
                eprintln!("claude smoke: no-beta probe succeeded: {summary}");
                return Ok(());
            }
            Err(err) => {
                eprintln!(
                    "claude smoke: no-beta probe unavailable; proceeding with Claude coding-plan transport: kind={:?} status={:?}",
                    err.kind, err.status_code
                );
            }
        }
    }

    let beta_client = ClaudeCodingPlanClient::new(&config.base_url, &config.access_token, retry)?;
    let summary = run_smoke("claude coding-plan oauth beta", &beta_client, request).await?;
    eprintln!("claude smoke: ClaudeCodingPlanClient succeeded: {summary}");
    Ok(())
}

#[derive(Debug)]
struct SmokeConfig {
    base_url: String,
    access_token: String,
    model: String,
    retry: RetryConfig,
}

fn load_smoke_config() -> Result<Option<SmokeConfig>, ProviderError> {
    let doc = eos_config::load()
        .map_err(|e| ProviderError::request(format!("loading config failed: {e}")))?;
    let providers = doc
        .section::<ProvidersConfig>("providers")
        .map_err(|e| ProviderError::request(format!("loading providers config failed: {e}")))?;

    let model = optional_env(MODEL_ENV).unwrap_or_else(|| DEFAULT_MODEL.to_owned());
    if let Some(access_token) =
        optional_env(TOKEN_ENV).or_else(|| optional_env(CLAUDE_CODE_TOKEN_ENV))
    {
        return Ok(Some(SmokeConfig {
            base_url: optional_env(BASE_URL_ENV).unwrap_or_else(|| {
                non_empty_or_default(&providers.claude_coding_plan.base_url, DEFAULT_BASE_URL)
            }),
            access_token,
            model,
            retry: providers.retry,
        }));
    }

    if providers.active != ProviderKind::ClaudeCodingPlan {
        eprintln!("claude smoke: skipped; set {TOKEN_ENV} or providers.active=claude_coding_plan");
        return Ok(None);
    }
    providers
        .validate()
        .map_err(|e| ProviderError::request(format!("providers config is invalid: {e}")))?;
    let access_token = providers
        .claude_coding_plan
        .access_token
        .as_ref()
        .ok_or_else(|| {
            ProviderError::request("providers.claude_coding_plan.access_token is required")
        })?
        .expose_secret()
        .to_owned();

    Ok(Some(SmokeConfig {
        base_url: non_empty_or_default(&providers.claude_coding_plan.base_url, DEFAULT_BASE_URL),
        access_token,
        model,
        retry: providers.retry,
    }))
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn should_probe_no_beta() -> bool {
    matches!(
        optional_env(PROBE_NO_BETA_ENV).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn non_empty_or_default(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn smoke_request(model: &str) -> LlmRequest {
    LlmRequest::builder(model)
        .system_prompt(
            "You are running a tiny connectivity smoke test. Reply with the exact marker only.",
        )
        .message(Message::from_user_text(format!(
            "Reply with exactly: {SMOKE_MARKER}"
        )))
        .max_tokens(32)
        .build()
}

async fn run_smoke<C>(
    label: &str,
    client: &C,
    request: LlmRequest,
) -> Result<SmokeSummary, ProviderError>
where
    C: LlmClient + ?Sized,
{
    tokio::time::timeout(PROVIDER_EVENT_TIMEOUT, run_smoke_inner(client, request))
        .await
        .map_err(|_| ProviderError::request(format!("{label} timed out waiting for provider")))?
}

async fn run_smoke_inner<C>(client: &C, request: LlmRequest) -> Result<SmokeSummary, ProviderError>
where
    C: LlmClient + ?Sized,
{
    let mut stream = client.stream_message(request).await?;
    let mut text = String::new();
    let mut event_count = 0usize;
    while let Some(event) = stream.next().await {
        event_count = event_count.saturating_add(1);
        match event? {
            LlmStreamEvent::AssistantTextDelta { text: delta } => {
                text.push_str(&delta);
                if text.contains(SMOKE_MARKER) {
                    return Ok(SmokeSummary { event_count, text });
                }
            }
            LlmStreamEvent::AssistantMessageComplete { message, .. } => {
                text.push_str(&message.assistant_text());
                return Ok(SmokeSummary { event_count, text });
            }
            LlmStreamEvent::ReasoningDelta { .. } | LlmStreamEvent::ToolUseDelta { .. } | _ => {}
        }
    }
    Err(ProviderError::request(
        "provider stream ended without assistant output",
    ))
}

#[derive(Debug)]
struct SmokeSummary {
    event_count: usize,
    text: String,
}

impl std::fmt::Display for SmokeSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "events={} text={:?}",
            self.event_count,
            self.text.chars().take(80).collect::<String>()
        )
    }
}
