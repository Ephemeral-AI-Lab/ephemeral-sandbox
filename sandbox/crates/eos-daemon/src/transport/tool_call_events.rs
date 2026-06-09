//! Tool-call lifecycle audit events emitted by the transport layer.
//!
//! The transport opens the lifecycle with `tool_call.started`; the rich
//! `tool_call.completed` is emitted once by the dispatcher's audit pass, so a
//! single completion lands per op on `Lane::Normal`.

use eos_protocol::audit::{build_event, Lane, ToolCallSection};

use crate::audit::buffer::safe_emit;

pub(super) fn emit_tool_call_event(
    event_type: &str,
    invocation_id: &str,
    op: &str,
    caller_id: &str,
    total_ms: Option<f64>,
    exit_status: Option<String>,
) {
    let section = ToolCallSection {
        tool_use_id: invocation_id.to_owned(),
        tool_name: op.to_owned(),
        caller_id: (!caller_id.is_empty()).then(|| caller_id.to_owned()),
        workspace_mode: None,
        workspace_handle_id: None,
        phase: None,
        duration_ms: None,
        total_ms,
        exit_status,
        bytes_in: None,
        bytes_out: None,
        phase_totals_rollup: None,
    };
    if let Ok(section) = serde_json::to_value(section) {
        safe_emit(build_event(event_type, "tool_call", section), Lane::Normal);
    }
}

pub(super) fn caller_id_from_args(args: &serde_json::Value) -> String {
    args.get("caller_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}
