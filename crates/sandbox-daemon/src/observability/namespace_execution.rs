use sandbox_observability::{
    NamespaceExecutionSnapshotRecord, NamespaceExecutionTraceRecord, MAX_ERROR_MESSAGE_LENGTH,
    MAX_ID_LENGTH, MAX_OPERATION_LENGTH, MAX_SNAPSHOT_STATE_LENGTH,
};
use sandbox_runtime::{
    NamespaceExecutionRecord, NamespaceExecutionTerminalStatus, RuntimeNamespaceExecutionSnapshot,
};

const TRACE_PREFIX: &str = "namespace_execution:";
const SPAN_ID_SEPARATOR: &str = ":span:";
const MAX_CALL_INDEX_TEXT_LENGTH: usize = 20;
const MAX_TRACE_ID_LENGTH: usize =
    MAX_ID_LENGTH - SPAN_ID_SEPARATOR.len() - MAX_CALL_INDEX_TEXT_LENGTH;

pub(crate) fn snapshot_record(
    sandbox_id: &str,
    execution: &RuntimeNamespaceExecutionSnapshot,
    namespace_execution_id: String,
    workspace_session_id: String,
    sampled_at_unix_ms: i64,
) -> NamespaceExecutionSnapshotRecord {
    NamespaceExecutionSnapshotRecord {
        sandbox_id: sandbox_id.to_owned(),
        namespace_execution_id,
        workspace_session_id,
        operation: bound_operation(execution.operation_name.clone()),
        lifecycle_state: bound_state(execution.lifecycle_state.as_str().to_owned()),
        sampled_at_unix_ms,
        error_message: None,
    }
}

pub(crate) fn trace_record(
    sandbox_id: &str,
    execution: &NamespaceExecutionRecord,
) -> NamespaceExecutionTraceRecord {
    NamespaceExecutionTraceRecord {
        trace_id: trace_id(&execution.namespace_execution_id.0),
        sandbox_id: sandbox_id.to_owned(),
        namespace_execution_id: bound_id(execution.namespace_execution_id.0.clone()),
        workspace_session_id: bound_id(execution.workspace_session_id.0.clone()),
        operation: bound_operation(execution.operation_name.clone()),
        request_id: execution.request_id.clone().map(bound_id),
        status: execution
            .terminal_status
            .map(terminal_status)
            .unwrap_or_default()
            .to_owned(),
        exit_code: execution.exit_code,
        started_at_unix_ms: execution.started_at_unix_ms,
        finished_at_unix_ms: execution
            .finished_at_unix_ms
            .unwrap_or(execution.started_at_unix_ms),
        duration_ms: execution.duration_ms.unwrap_or(0.0),
        error_kind: execution.error_kind.clone().map(bound_operation),
        error_message: execution.error_message.clone().map(bound_error),
    }
}

fn trace_id(namespace_execution_id: &str) -> String {
    let max_namespace_execution_id_len = MAX_TRACE_ID_LENGTH - TRACE_PREFIX.len();
    format!(
        "{TRACE_PREFIX}{}",
        bound_string_with_hash(
            namespace_execution_id.to_owned(),
            max_namespace_execution_id_len
        )
    )
}

const fn terminal_status(status: NamespaceExecutionTerminalStatus) -> &'static str {
    match status {
        NamespaceExecutionTerminalStatus::Ok => "ok",
        NamespaceExecutionTerminalStatus::Error => "error",
        NamespaceExecutionTerminalStatus::TimedOut => "timed_out",
        NamespaceExecutionTerminalStatus::Cancelled => "cancelled",
    }
}

fn bound_id(value: String) -> String {
    bound_string_with_hash(value, MAX_ID_LENGTH)
}

fn bound_operation(value: String) -> String {
    bound_string(value, MAX_OPERATION_LENGTH)
}

fn bound_state(value: String) -> String {
    bound_string(value, MAX_SNAPSHOT_STATE_LENGTH)
}

fn bound_error(value: String) -> String {
    bound_string(value, MAX_ERROR_MESSAGE_LENGTH)
}

fn bound_string(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        value
    } else {
        let mut end = max_bytes;
        while !value.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        value[..end].to_owned()
    }
}

fn bound_string_with_hash(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let suffix = format!("~{:016x}", stable_hash(value.as_bytes()));
    let prefix_len = max_bytes.saturating_sub(suffix.len());
    if prefix_len == 0 {
        return bound_string(suffix, max_bytes);
    }
    let mut end = prefix_len.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{}", &value[..end], suffix)
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
