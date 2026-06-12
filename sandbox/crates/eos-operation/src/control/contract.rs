use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::request::require_path;
use crate::{CallerId, InvocationId};

use crate::core::request::ArgsError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeReadyInput {
    pub layer_stack_root: PathBuf,
}

impl RuntimeReadyInput {
    pub(crate) fn parse(args: &Value) -> Result<Self, ArgsError> {
        Ok(Self {
            layer_stack_root: require_path(args, "layer_stack_root")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatInput {
    pub invocation_ids: Vec<InvocationId>,
}

impl HeartbeatInput {
    pub(crate) fn parse(args: &Value) -> Self {
        Self {
            invocation_ids: args
                .get("invocation_ids")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .map(InvocationId::new)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelInvocationInput {
    pub invocation_id: InvocationId,
}

impl CancelInvocationInput {
    pub(crate) fn parse(args: &Value) -> Self {
        Self {
            invocation_id: InvocationId::new(
                args.get("invocation_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerCountInput {
    pub caller: CallerId,
}

impl CallerCountInput {
    pub(crate) fn parse(args: &Value) -> Self {
        Self {
            caller: CallerId::from_wire(args),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceExportInput {
    pub max_records: usize,
}

impl TraceExportInput {
    pub(crate) fn parse(args: &Value) -> Self {
        Self {
            max_records: args
                .get("max_records")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value > 0)
                .unwrap_or(64),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeReadyOutput {
    pub success: bool,
    pub ready: bool,
    pub probes: Vec<Value>,
    pub daemon_pid: u32,
    pub uptime_s: f64,
    pub timings: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatOutput {
    pub success: bool,
    pub touched: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelInvocationOutput {
    pub success: bool,
    pub invocation_id: String,
    pub cancelled: bool,
    pub already_done: bool,
    pub cleanup_done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InflightCountOutput {
    pub success: bool,
    pub caller_id: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceExportOutput {
    pub success: bool,
    pub record_count: usize,
    pub dropped_traces: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_batch_base64: Option<String>,
}
