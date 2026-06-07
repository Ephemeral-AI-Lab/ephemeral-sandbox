//! Engine telemetry events emitted for one agent run.

use eos_audit::{AuditEvent, AuditNode, AuditSource};
use eos_audit::{AGENT_RUN_COMPLETED, OS_RESOURCE_SAMPLED};
use eos_types::{JsonObject, SystemClock};
use serde_json::{json, Value};

use crate::query::{QueryContext, QueryExitReason};
use crate::runtime::EngineRunHandles;

use super::resource_sample::capture_process_resource_sample;

pub(crate) fn publish_agent_run_completed(
    handles: &EngineRunHandles,
    ctx: &QueryContext,
    duration_ms: f64,
    error: Option<&str>,
) {
    let mut section = JsonObject::new();
    section.insert("duration_ms".to_owned(), json!(duration_ms));
    section.insert(
        "status".to_owned(),
        json!(if error.is_some() { "error" } else { "ok" }),
    );
    section.insert(
        "exit_reason".to_owned(),
        json!(ctx.exit_reason.map(exit_reason_value)),
    );
    if let Some(error) = error {
        section.insert("error".to_owned(), json!(error));
    }

    let mut payload = JsonObject::new();
    payload.insert("agent_run".to_owned(), Value::Object(section));
    publish_audit_event(handles, ctx, AGENT_RUN_COMPLETED, payload);
}

pub(crate) fn publish_os_resource_sampled(handles: &EngineRunHandles, ctx: &QueryContext) {
    if !handles.audit.enabled() {
        return;
    }
    let Some(sample) = capture_process_resource_sample() else {
        return;
    };

    let mut payload = JsonObject::new();
    payload.insert(
        "os_resource".to_owned(),
        Value::Object(sample.into_payload()),
    );
    publish_audit_event(handles, ctx, OS_RESOURCE_SAMPLED, payload);
}

fn publish_audit_event(
    handles: &EngineRunHandles,
    ctx: &QueryContext,
    event_type: &str,
    payload: JsonObject,
) {
    let event = AuditEvent::new(
        AuditSource::Engine,
        event_type,
        agent_run_audit_node(ctx),
        payload,
        &SystemClock,
    );
    if let Err(err) = handles.audit.publish(&event) {
        tracing::warn!(
            error = %err,
            agent_run_id = ctx.agent_run_id.as_str(),
            event_type,
            "obs publish failed"
        );
    }
}

fn agent_run_audit_node(ctx: &QueryContext) -> AuditNode {
    let mut node = AuditNode::builder()
        .agent_name(ctx.agent_name.clone())
        .agent_run_id(ctx.agent_run_id.clone());
    if let Some(request_id) = &ctx.tool_metadata.request_id {
        node = node.request_id(request_id.clone());
    }
    if let Some(task_id) = ctx
        .task_id
        .clone()
        .or_else(|| ctx.tool_metadata.task_id.clone())
    {
        node = node.task_id(task_id);
    }
    if let Some(sandbox_id) = &ctx.tool_metadata.sandbox_id {
        node = node.sandbox_id(sandbox_id.clone());
    }
    node.build()
}

const fn exit_reason_value(reason: QueryExitReason) -> &'static str {
    match reason {
        QueryExitReason::ToolStop => "tool_stop",
        QueryExitReason::TerminalNotSubmitted => "terminal_not_submitted",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use eos_agent_def::AgentRegistry;
    use eos_audit::{AuditSink, AGENT_RUN_COMPLETED};
    use eos_llm_client::{LlmClient, LlmRequest, LlmStream, ProviderError};
    use eos_sandbox_port::SandboxTransport;
    use eos_skills::SkillRegistry;
    use eos_state::{
        AgentRun, AgentRunStore, CoreError, Sealed as StateSealed, TaskId, UtcDateTime,
    };
    use eos_testkit::{metadata, test_tools_root, FakeTransport};
    use eos_tools::{SandboxToolService, SkillToolService, ToolConfigSet, ToolRegistry};
    use eos_types::{AgentRunId, JsonObject};
    use serde_json::json;

    use super::*;
    use crate::notifications::NotificationService;

    #[derive(Debug, Default)]
    struct RecordingAuditSink {
        events: Mutex<Vec<eos_audit::AuditEvent>>,
    }

    impl RecordingAuditSink {
        fn events(&self) -> Vec<eos_audit::AuditEvent> {
            self.events.lock().expect("audit lock").clone()
        }
    }

    impl AuditSink for RecordingAuditSink {
        fn publish(&self, event: &eos_audit::AuditEvent) -> Result<(), eos_audit::AuditError> {
            self.events.lock().expect("audit lock").push(event.clone());
            Ok(())
        }
    }

    #[derive(Debug)]
    struct NoopLlmClient;

    #[async_trait]
    impl LlmClient for NoopLlmClient {
        async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[derive(Debug, Default)]
    struct NoopAgentRunStore;

    impl StateSealed for NoopAgentRunStore {}

    #[async_trait]
    impl AgentRunStore for NoopAgentRunStore {
        async fn create_run(
            &self,
            agent_run_id: &AgentRunId,
            task_id: &TaskId,
            agent_name: &str,
            initial_messages: Option<&[JsonObject]>,
        ) -> Result<AgentRun, CoreError> {
            Ok(AgentRun {
                id: agent_run_id.clone(),
                task_id: task_id.clone(),
                initial_messages: initial_messages.map(<[_]>::to_vec),
                agent_name: agent_name.to_owned(),
                message_history: None,
                terminal_tool_result: None,
                token_count: 0,
                error: None,
                created_at: UtcDateTime::now(),
                finished_at: None,
            })
        }

        async fn finish_run(
            &self,
            _agent_run_id: &AgentRunId,
            _message_history: Option<&[JsonObject]>,
            _terminal_tool_result: Option<&JsonObject>,
            _token_count: i64,
            _error: Option<&str>,
        ) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }

        async fn get(&self, _agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }

        async fn get_for_task(&self, _task_id: &TaskId) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }
    }

    fn handles(audit: Arc<dyn AuditSink>) -> EngineRunHandles {
        let transport: Arc<dyn SandboxTransport> = Arc::new(FakeTransport);
        EngineRunHandles {
            agent_run_store: Arc::new(NoopAgentRunStore),
            llm_client: Arc::new(NoopLlmClient),
            event_source_factory: None,
            agent_registry: Arc::new(Vec::new().into_iter().collect::<AgentRegistry>()),
            tool_config: Arc::new(
                ToolConfigSet::load_from_dir(&test_tools_root()).expect("tool config"),
            ),
            sandbox_service: SandboxToolService::new(transport),
            root_submission: None,
            skill_service: SkillToolService::new(Arc::new(SkillRegistry::new())),
            tool_registry_extender: None,
            audit,
            message_records: None,
            workspace_root: "/tmp".to_owned(),
        }
    }

    fn context() -> QueryContext {
        let mut tool_metadata = metadata();
        tool_metadata.request_id = Some("req-audit".parse().expect("request id"));
        tool_metadata.task_id = Some("task-audit".parse().expect("task id"));
        tool_metadata.sandbox_id = Some("sandbox-a".parse().expect("sandbox id"));
        QueryContext {
            tool_registry: Arc::new(ToolRegistry::new()),
            cwd: PathBuf::new(),
            model: "model".to_owned(),
            system_prompt: String::new(),
            max_tokens: 1,
            tool_call_limit: 8,
            agent_name: "root".to_owned(),
            agent_run_id: "run-audit".parse().expect("agent run id"),
            task_id: None,
            tool_calls_used: 0,
            text_only_no_terminal_turns: 0,
            tool_metadata,
            terminal_tools: BTreeSet::new(),
            exit_reason: Some(QueryExitReason::TerminalNotSubmitted),
            terminal_result: None,
            event_source: None,
            prompt_report: None,
            message_record: None,
            notification_rules: Vec::new(),
            notification_fired: BTreeSet::new(),
            notifier: NotificationService::new(),
            cancellation: crate::AgentRunCancellation::new(),
            foreground: Arc::new(
                crate::ForegroundExecutorFactory
                    .create("run-audit".parse().expect("agent run id")),
            ),
            audit: None,
            run_handles: None,
        }
    }

    #[test]
    fn agent_run_completed_audit_includes_ids_status_exit_reason_and_error() {
        let audit = Arc::new(RecordingAuditSink::default());
        let handles = handles(audit.clone());
        let ctx = context();

        publish_agent_run_completed(&handles, &ctx, 12.5, Some("provider failed"));

        let events = audit.events();
        assert_eq!(events.len(), 1);
        let obs = events[0].to_obs_envelope();
        assert_eq!(obs.event_type, AGENT_RUN_COMPLETED);
        assert_eq!(obs.ids.agent_run_id.as_deref(), Some("run-audit"));
        assert_eq!(obs.ids.request_id.as_deref(), Some("req-audit"));
        assert_eq!(obs.ids.task_id.as_deref(), Some("task-audit"));
        assert_eq!(obs.ids.sandbox_id.as_deref(), Some("sandbox-a"));
        assert_eq!(obs.payload["agent_run"]["status"], json!("error"));
        assert_eq!(
            obs.payload["agent_run"]["exit_reason"],
            json!("terminal_not_submitted")
        );
        assert_eq!(obs.payload["agent_run"]["error"], json!("provider failed"));
        assert_eq!(obs.payload["agent_run"]["duration_ms"], json!(12.5));
    }
}
