//! Audit projection for engine stream events.

use eos_audit::{tool_completed, tool_started, AuditEvent, AuditNode};
use eos_types::Clock;

use crate::StreamEvent;

fn node_from_event(event: &StreamEvent) -> AuditNode {
    let mut builder = AuditNode::builder();
    match event {
        StreamEvent::ToolExecutionStarted {
            agent_name,
            agent_run_id,
            tool_name,
            tool_use_id,
            ..
        }
        | StreamEvent::ToolExecutionCompleted {
            agent_name,
            agent_run_id,
            tool_name,
            tool_use_id,
            ..
        }
        | StreamEvent::ToolExecutionProgress {
            agent_name,
            agent_run_id,
            tool_name,
            tool_use_id,
            ..
        }
        | StreamEvent::ToolExecutionCancelled {
            agent_name,
            agent_run_id,
            tool_name,
            tool_use_id,
            ..
        } => {
            if !agent_name.is_empty() {
                builder = builder.agent_name(agent_name.clone());
            }
            if let Some(id) = agent_run_id {
                builder = builder.agent_run_id(id.clone());
            }
            builder = builder
                .tool_name(tool_name.clone())
                .tool_use_id(tool_use_id.clone());
        }
        _ => {}
    }
    builder.build()
}

/// Project engine stream events into audit rows.
#[must_use]
pub fn audit_events_from_stream_event(event: &StreamEvent, clock: &dyn Clock) -> Vec<AuditEvent> {
    match event {
        StreamEvent::ToolExecutionStarted { tool_input, .. } => {
            vec![tool_started(node_from_event(event), tool_input, clock)]
        }
        StreamEvent::ToolExecutionCompleted {
            output,
            is_error,
            is_terminal,
            metadata,
            ..
        } => vec![tool_completed(
            node_from_event(event),
            output,
            *is_error,
            *is_terminal,
            metadata,
            clock,
        )],
        _ => Vec::new(),
    }
}
