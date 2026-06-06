//! Live Codex access-token smoke test.
//!
//! This intentionally reads only the typed `providers` config section loaded by
//! `eos_config::load()`. Local credentials belong in gitignored
//! `agent-core/config/local.yml`; this crate does not know about or parse any
//! credential cache file.

use std::sync::Arc;
use std::time::Duration;

use eos_config::{ProviderKind, ProvidersConfig};
use eos_llm_client::{
    Auth, CodexCodingPlanClient, LlmClient, LlmRequest, LlmStreamEvent, Message, ProviderError,
    ReasoningEffort, ToolChoice, ToolSpec,
};
use eos_types::JsonObject;
use futures::StreamExt;
use serde_json::{json, Value};

const DEFAULT_MODEL: &str = "gpt-5.5";
const SMOKE_TOOL_NAME: &str = "codex_smoke_terminal";
const PROVIDER_EVENT_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::test]
async fn codex_access_token_gets_llm_client_response() -> Result<(), ProviderError> {
    let Some(providers) = load_codex_provider_config()? else {
        return Ok(());
    };
    providers
        .validate()
        .map_err(|e| ProviderError::request(format!("providers config is invalid: {e}")))?;
    let token = providers
        .codex_coding_plan
        .access_token
        .as_ref()
        .ok_or_else(|| {
            ProviderError::request("providers.codex_coding_plan.access_token is required")
        })?;

    let client = CodexCodingPlanClient::new(
        &providers.codex_coding_plan.base_url,
        Auth::codex_access_token_from_jwt(token.expose_secret())?,
        Arc::new(providers.retry),
    )?;
    let request = LlmRequest::builder(DEFAULT_MODEL)
        .system_prompt(
            "You are checking Codex access. Call the codex_smoke_terminal tool exactly once.",
        )
        .message(Message::from_user_text(
            "Call codex_smoke_terminal with an empty JSON object.",
        ))
        .tools(vec![ToolSpec::new(
            SMOKE_TOOL_NAME,
            "No-op terminal-style smoke test tool.",
            empty_object_schema(),
            None,
        )])
        .tool_choice(ToolChoice::Tool {
            name: SMOKE_TOOL_NAME.to_owned(),
        })
        .reasoning_effort(ReasoningEffort::Medium)
        .max_tokens(256)
        .build();

    let mut stream = client.stream_message(request).await?;
    let next = tokio::time::timeout(PROVIDER_EVENT_TIMEOUT, stream.next())
        .await
        .map_err(|_| ProviderError::request("timed out waiting for codex provider event"))?;
    let event =
        next.ok_or_else(|| ProviderError::request("codex provider stream ended without events"))??;
    match event {
        LlmStreamEvent::ToolUseDelta { name, .. } => {
            if name == SMOKE_TOOL_NAME {
                return Ok(());
            }
            Err(ProviderError::decode(
                None,
                format!("codex smoke returned unexpected tool {name}"),
            ))
        }
        LlmStreamEvent::AssistantTextDelta { .. }
        | LlmStreamEvent::ReasoningDelta { .. }
        | LlmStreamEvent::AssistantMessageComplete { .. } => Ok(()),
        _ => Ok(()),
    }
}

fn load_codex_provider_config() -> Result<Option<ProvidersConfig>, ProviderError> {
    let doc = eos_config::load()
        .map_err(|e| ProviderError::request(format!("loading config failed: {e}")))?;
    let providers = doc
        .section::<ProvidersConfig>("providers")
        .map_err(|e| ProviderError::request(format!("loading providers config failed: {e}")))?;
    if providers.active != ProviderKind::CodexCodingPlan {
        return Ok(None);
    }
    Ok(Some(providers))
}

fn empty_object_schema() -> JsonObject {
    match json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    }) {
        Value::Object(schema) => schema,
        _ => JsonObject::new(),
    }
}
