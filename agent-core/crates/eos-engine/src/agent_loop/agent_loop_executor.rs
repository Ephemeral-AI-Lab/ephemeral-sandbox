//! Full agent-loop executor.

use std::sync::Arc;

use eos_llm_client::{ContentBlock, LlmRequest, Message, UsageSnapshot};
use eos_tool::{RegisteredTool, ToolName, ToolResult};
use eos_types::{AgentRunApi, AgentRunId, AgentState, JsonObject, ToolUseId};
use futures::StreamExt;

use super::{
    AgentLoopCancelSignal, AgentLoopHooks, AgentLoopMessage, AgentLoopOutcome,
    AgentLoopOutcomeKind, AgentLoopProviderStream, AgentLoopRunServices, AgentLoopState,
    AgentLoopToolRegistryFactory, BackgroundSessionInputs, ExecutionMetadataBuildInput,
    StartAgentLoopRequest, ToolCallHookStores, ToolExecutionMetadataReader,
};
use crate::notifications::EngineNotificationQueue;
use crate::query::{provider_messages::build_provider_messages, ProviderStreamSource};
use crate::tool_call::{
    execute_tool_once, lifecycle_batch_decision, reject_terminal_batch, DispatchCall,
};
use crate::{stamp_identity, EngineError, StreamEvent};

/// Executes a full agent loop from request to terminal outcome.
pub(crate) struct AgentLoopExecutor {
    provider_stream_source: AgentLoopProviderStream,
    loop_hooks: Arc<dyn AgentLoopHooks>,
    tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
    metadata_service: Arc<dyn ToolExecutionMetadataReader>,
    cancel_signal: AgentLoopCancelSignal,
    background_dependencies: Option<BackgroundSessionInputs>,
    hook_dependencies: Option<ToolCallHookStores>,
    event_sink: Option<crate::query::EngineEventSink>,
    agent_run_api: Arc<dyn AgentRunApi>,
}

impl std::fmt::Debug for AgentLoopExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopExecutor").finish_non_exhaustive()
    }
}

impl AgentLoopExecutor {
    pub(crate) fn new(
        provider_stream_source: AgentLoopProviderStream,
        loop_hooks: Arc<dyn AgentLoopHooks>,
        tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
        metadata_service: Arc<dyn ToolExecutionMetadataReader>,
        cancel_signal: AgentLoopCancelSignal,
        background_dependencies: Option<BackgroundSessionInputs>,
        hook_dependencies: Option<ToolCallHookStores>,
        event_sink: Option<crate::query::EngineEventSink>,
        agent_run_api: Arc<dyn AgentRunApi>,
    ) -> Self {
        Self {
            provider_stream_source,
            loop_hooks,
            tool_registry_factory,
            metadata_service,
            cancel_signal,
            background_dependencies,
            hook_dependencies,
            event_sink,
            agent_run_api,
        }
    }

    pub(crate) async fn execute_agent_loop(
        self,
        request: StartAgentLoopRequest,
    ) -> AgentLoopOutcome {
        let event_identity = match self
            .metadata_service
            .agent_state(&request.agent_run_id)
            .await
        {
            Ok(identity) => identity,
            Err(error) => {
                return AgentLoopOutcome {
                    kind: AgentLoopOutcomeKind::LoopFailed {
                        error_summary: error.to_string(),
                    },
                    final_conversation_messages: Vec::new(),
                    total_token_count: None,
                };
            }
        };
        let provider_stream_source = self.resolve_provider_stream_source(&request, &event_identity);
        let run_services = self.build_run_services(&request.agent_run_id);
        let mut state = match AgentLoopState::from_request(
            request,
            &*self.tool_registry_factory,
            run_services,
            self.agent_run_api.clone(),
        ) {
            Ok(state) => state,
            Err(error) => {
                return AgentLoopOutcome {
                    kind: AgentLoopOutcomeKind::LoopFailed {
                        error_summary: error.to_string(),
                    },
                    final_conversation_messages: Vec::new(),
                    total_token_count: None,
                };
            }
        };

        self.loop_hooks.on_start(&state).await;

        loop {
            if let Some(reason) = self.cancel_signal.reason() {
                state
                    .teardown_background(&format!("agent loop cancelled: {reason}"))
                    .await;
                let outcome = state.loop_failed_summary(format!("agent loop cancelled: {reason}"));
                self.loop_hooks.on_complete(&outcome).await;
                return outcome;
            }
            if state.turn_limit_reached() {
                state
                    .teardown_background("agent loop exited without a terminal tool submission")
                    .await;
                let summary = state.terminal_not_submitted_summary();
                let outcome = state.loop_failed_summary(summary);
                self.loop_hooks.on_complete(&outcome).await;
                return outcome;
            }

            self.loop_hooks.on_step(&state).await;
            self.drain_notifications(&mut state, &event_identity).await;
            let turn_result = match self
                .execute_assistant_turn(&provider_stream_source, &event_identity, &mut state)
                .await
            {
                Ok(turn_result) => turn_result,
                Err(error) => {
                    state
                        .teardown_background(&format!("agent loop failed: {error}"))
                        .await;
                    let outcome = state.loop_failed(error);
                    self.loop_hooks.on_complete(&outcome).await;
                    return outcome;
                }
            };

            match turn_result {
                AssistantTurnResult::Continue => {}
                AssistantTurnResult::TerminalToolSubmitted { outcome } => {
                    state
                        .teardown_background("parent agent submitted its terminal")
                        .await;
                    let outcome = state.terminal_tool_submitted(outcome);
                    self.loop_hooks.on_complete(&outcome).await;
                    return outcome;
                }
            }
        }
    }

    async fn execute_assistant_turn(
        &self,
        provider_stream_source: &Arc<dyn ProviderStreamSource>,
        event_identity: &AgentState,
        state: &mut AgentLoopState,
    ) -> Result<AssistantTurnResult, EngineError> {
        let request = build_loop_provider_request(state);
        let mut stream = provider_stream_source.stream(&request).await?;
        let mut final_message: Option<Message> = None;
        let mut final_usage: Option<UsageSnapshot> = None;

        while let Some(item) = stream.next().await {
            let event = item?;
            let event = stamp_identity(
                event,
                &event_identity.agent_name,
                &event_identity.agent_run_id,
            );
            match &event {
                StreamEvent::AssistantMessageComplete { payload, .. } => {
                    final_usage = Some(payload.usage);
                    final_message = Some(payload.message.clone());
                }
                _ => {}
            }
            self.emit_event(&event);
        }

        let message = final_message.ok_or_else(|| {
            EngineError::Internal("provider stream ended without assistant completion".to_owned())
        })?;
        if let Some(usage) = final_usage {
            let turn_tokens = i64::from(usage.input_tokens) + i64::from(usage.output_tokens);
            state.total_token_count = Some(
                state
                    .total_token_count
                    .unwrap_or_default()
                    .saturating_add(turn_tokens),
            );
        }

        let tool_calls = tool_uses_from_message(&message);
        state.record_tool_calls(tool_calls.len());
        state
            .conversation_messages
            .push(AgentLoopMessage::AssistantMessage(message));
        if tool_calls.is_empty() {
            state.record_text_only_turn();
            return Ok(AssistantTurnResult::Continue);
        }

        let dispatch = self.dispatch_tool_batch(state, &tool_calls).await?;
        let result_message = Message {
            role: eos_llm_client::MessageRole::User,
            content: dispatch.tool_results,
        };
        state
            .conversation_messages
            .push(AgentLoopMessage::UserMessage(result_message));

        match dispatch.submission_outcome {
            Some(outcome) if outcome.is_terminal => {
                Ok(AssistantTurnResult::TerminalToolSubmitted { outcome })
            }
            _ => Ok(AssistantTurnResult::Continue),
        }
    }

    async fn dispatch_tool_batch(
        &self,
        state: &AgentLoopState,
        calls: &[ToolUseRequest],
    ) -> Result<LoopToolDispatchOutcome, EngineError> {
        let dispatch_calls: Vec<DispatchCall<'_>> = calls
            .iter()
            .map(|call| DispatchCall {
                tool_use_id: call.tool_use_id.as_str(),
                name: &call.name,
            })
            .collect();

        if let Some(rejections) = reject_terminal_batch(&dispatch_calls, &state.tool_registry) {
            let tool_results = calls
                .iter()
                .filter_map(|call| {
                    rejections
                        .iter()
                        .find(|rejection| rejection.tool_use_id == call.tool_use_id.as_str())
                        .map(|rejection| {
                            result_block(&call.tool_use_id, &rejection_result(&rejection.message))
                        })
                })
                .collect();
            return Ok(LoopToolDispatchOutcome {
                tool_results,
                submission_outcome: None,
            });
        }

        let lifecycle = lifecycle_batch_decision(&dispatch_calls, &state.tool_registry);
        let dispatched: std::collections::BTreeSet<&str> =
            lifecycle.dispatched.iter().map(String::as_str).collect();
        let rejected: std::collections::BTreeMap<String, ToolResult> = lifecycle
            .rejected
            .into_iter()
            .map(|rejection| (rejection.tool_use_id, rejection_result(&rejection.message)))
            .collect();

        let mut tool_results = Vec::new();
        let mut submission_outcome = None;
        let conversation = Arc::from(loop_messages_to_llm_messages(&state.conversation_messages));

        for call in calls {
            if let Some(result) = rejected.get(call.tool_use_id.as_str()) {
                tool_results.push(result_block(&call.tool_use_id, result));
                continue;
            }
            if !dispatched.contains(call.tool_use_id.as_str()) {
                continue;
            }

            let Some(tool) = state.tool_registry.get_wire(&call.name).cloned() else {
                let result = rejection_result(&format!("Unknown tool `{}`.", call.name));
                tool_results.push(result_block(&call.tool_use_id, &result));
                continue;
            };

            let result = self
                .execute_registered_tool(state, call, &tool, Arc::clone(&conversation))
                .await?;
            if tool.is_terminal && result.is_terminal {
                submission_outcome = Some(result.clone());
            }
            tool_results.push(result_block(&call.tool_use_id, &result));
        }

        Ok(LoopToolDispatchOutcome {
            tool_results,
            submission_outcome,
        })
    }

    async fn execute_registered_tool(
        &self,
        state: &AgentLoopState,
        call: &ToolUseRequest,
        tool: &RegisteredTool,
        conversation: Arc<[Message]>,
    ) -> Result<ToolResult, EngineError> {
        let tool_name = ToolName::from_wire(&call.name)
            .ok_or_else(|| EngineError::UnknownTool(call.name.clone()))?;
        let metadata = self
            .metadata_service
            .build_execution_metadata(ExecutionMetadataBuildInput {
                agent_run_id: state.agent_run_id.clone(),
                tool_name,
                tool_use_id: call.tool_use_id.clone(),
                conversation,
            })
            .await
            .map_err(|err| EngineError::Internal(err.to_string()))?;
        self.emit_event(&StreamEvent::ToolExecutionStarted {
            agent_name: metadata.agent_name.clone(),
            agent_run_id: metadata.agent_run_id.clone(),
            tool_name: call.name.clone(),
            tool_input: call.input.clone(),
            tool_use_id: call.tool_use_id.clone(),
        });
        let hooks = state.background().and_then(|background| {
            self.hook_dependencies
                .clone()
                .map(|dependencies| crate::tool_call::ToolCallHooks::new(background, dependencies))
        });
        let result = execute_tool_once(tool, &call.input, &metadata, hooks.as_ref()).await?;
        self.emit_event(&StreamEvent::ToolExecutionCompleted {
            agent_name: metadata.agent_name,
            agent_run_id: metadata.agent_run_id,
            tool_name: call.name.clone(),
            output: result.output.clone(),
            is_error: result.is_error,
            tool_use_id: call.tool_use_id.clone(),
            metadata: result.metadata.clone(),
            is_terminal: result.is_terminal,
        });
        Ok(result)
    }

    fn build_run_services(&self, agent_run_id: &AgentRunId) -> AgentLoopRunServices {
        let Some(dependencies) = &self.background_dependencies else {
            return AgentLoopRunServices::inert();
        };
        let notifier = EngineNotificationQueue::new();
        let background = dependencies.build_managers(
            agent_run_id.clone(),
            self.agent_run_api.clone(),
            notifier.clone(),
        );
        AgentLoopRunServices::from_background(&background, notifier)
    }

    fn resolve_provider_stream_source(
        &self,
        request: &StartAgentLoopRequest,
        agent_state: &AgentState,
    ) -> Arc<dyn ProviderStreamSource> {
        match &self.provider_stream_source {
            AgentLoopProviderStream::Static(source) => Arc::clone(source),
            AgentLoopProviderStream::Factory(factory) => factory(request, agent_state),
        }
    }

    async fn drain_notifications(&self, state: &mut AgentLoopState, event_identity: &AgentState) {
        for notification in state.drain_notifications().await {
            self.emit_event(&StreamEvent::SystemNotification {
                agent_name: event_identity.agent_name.clone(),
                agent_run_id: Some(event_identity.agent_run_id.clone()),
                text: notification.message,
            });
        }
    }

    fn emit_event(&self, event: &StreamEvent) {
        if let Some(sink) = &self.event_sink {
            sink(event);
        }
    }
}

/// Result of one private assistant turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AssistantTurnResult {
    /// Continue the agent loop.
    Continue,
    /// A terminal tool submitted successfully.
    TerminalToolSubmitted {
        /// Terminal tool result.
        outcome: ToolResult,
    },
}

struct LoopToolDispatchOutcome {
    tool_results: Vec<ContentBlock>,
    submission_outcome: Option<ToolResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolUseRequest {
    tool_use_id: ToolUseId,
    name: String,
    input: JsonObject,
}

fn build_loop_provider_request(state: &AgentLoopState) -> LlmRequest {
    let mut system_prompt = None;
    let messages: Vec<Message> = state
        .conversation_messages
        .iter()
        .filter_map(|message| match message {
            AgentLoopMessage::SystemPrompt(prompt) => {
                system_prompt = Some(prompt.clone());
                None
            }
            AgentLoopMessage::UserMessage(message)
            | AgentLoopMessage::AssistantMessage(message) => Some(message.clone()),
        })
        .collect();
    let mut builder = LlmRequest::builder(state.model_key.clone())
        .messages(build_provider_messages(&messages))
        .max_tokens(state.max_completion_tokens)
        .tools(state.tool_registry.specs());
    if let Some(prompt) = system_prompt {
        builder = builder.system_prompt(prompt);
    }
    builder.build()
}

fn tool_uses_from_message(message: &Message) -> Vec<ToolUseRequest> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                tool_use_id,
                name,
                input,
            } => Some(ToolUseRequest {
                tool_use_id: tool_use_id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn loop_messages_to_llm_messages(messages: &[AgentLoopMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| match message {
            AgentLoopMessage::SystemPrompt(_) => None,
            AgentLoopMessage::UserMessage(message)
            | AgentLoopMessage::AssistantMessage(message) => Some(message.clone()),
        })
        .collect()
}

fn result_block(tool_use_id: &ToolUseId, result: &ToolResult) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: tool_use_id.clone(),
        content: result.output.clone(),
        is_error: result.is_error,
        metadata: result.metadata.clone(),
        is_terminal: result.is_terminal,
    }
}

fn rejection_result(message: &str) -> ToolResult {
    ToolResult {
        output: message.to_owned(),
        is_error: true,
        metadata: JsonObject::new(),
        is_terminal: false,
    }
}
