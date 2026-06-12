use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::budget::{BoundedJson, DetailBudget};
use crate::ids::{RequestId, SpanUid, TraceId};
use crate::resource_stats::ResourceStats;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRoute {
    EphemeralWorkspace,
    IsolatedWorkspace,
    FastPath,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceKind {
    OpRequest,
    CommandFinalize,
    ActiveCommandAdvance,
    IdleWorkspaceEvict,
    PluginService,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanSubsystem {
    Wire,
    Dispatch,
    Op,
    LayerStack,
    Overlay,
    Command,
    Workspace,
    Plugin,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    OpRequest,
    GatewayTransport,
    GatewayRoute,
    HostProtocol,
    HostTransport,
    DaemonTransport,
    Dispatch,
    Operation,
    LayerStack,
    Occ,
    Overlay,
    CommandProcessSpawn,
    CommandProcessWait,
    CommandFinalize,
    WorkspaceRoute,
    IsolatedWorkspace,
    Plugin,
    File,
    Checkpoint,
    Resource,
    Control,
}

impl SpanKind {
    #[must_use]
    pub const fn subsystem(self) -> SpanSubsystem {
        match self {
            Self::OpRequest
            | Self::GatewayTransport
            | Self::GatewayRoute
            | Self::HostProtocol
            | Self::HostTransport
            | Self::DaemonTransport => SpanSubsystem::Wire,
            Self::Dispatch => SpanSubsystem::Dispatch,
            Self::Operation | Self::File | Self::Checkpoint => SpanSubsystem::Op,
            Self::LayerStack | Self::Occ => SpanSubsystem::LayerStack,
            Self::Overlay => SpanSubsystem::Overlay,
            Self::CommandProcessSpawn | Self::CommandProcessWait | Self::CommandFinalize => {
                SpanSubsystem::Command
            }
            Self::WorkspaceRoute | Self::IsolatedWorkspace => SpanSubsystem::Workspace,
            Self::Plugin => SpanSubsystem::Plugin,
            Self::Resource | Self::Control => SpanSubsystem::Control,
        }
    }

    #[must_use]
    pub fn parse_label(label: &str) -> Option<Self> {
        Some(match label {
            "op_request" => Self::OpRequest,
            "gateway.transport" | "gateway_transport" => Self::GatewayTransport,
            "gateway.route" | "gateway_route" => Self::GatewayRoute,
            "host.protocol" | "host_protocol" => Self::HostProtocol,
            "host.transport" | "host_transport" => Self::HostTransport,
            "daemon.transport" | "daemon_transport" => Self::DaemonTransport,
            "dispatch" | "daemon.dispatch" => Self::Dispatch,
            "op" | "operation" => Self::Operation,
            "layerstack" => Self::LayerStack,
            "occ" => Self::Occ,
            "overlay" => Self::Overlay,
            "command.process.spawn" | "command_process_spawn" => Self::CommandProcessSpawn,
            "command.process.wait" | "command_process_wait" => Self::CommandProcessWait,
            "command.finalize" | "command_finalize" => Self::CommandFinalize,
            "workspace.route" | "workspace_route" => Self::WorkspaceRoute,
            "isolated_workspace" => Self::IsolatedWorkspace,
            "plugin" => Self::Plugin,
            "file" => Self::File,
            "checkpoint" => Self::Checkpoint,
            "resource" => Self::Resource,
            "control" => Self::Control,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    Ok,
    Rejected,
    Cancelled,
    TimedOut,
    Error,
}

impl SpanStatus {
    #[must_use]
    pub fn parse_label(label: &str) -> Option<Self> {
        Some(match label {
            "ok" => Self::Ok,
            "rejected" => Self::Rejected,
            "cancelled" => Self::Cancelled,
            "timed_out" => Self::TimedOut,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceLinkKind {
    Command,
    WorkspaceHandle,
    PluginService,
    ManifestVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceLink {
    pub kind: TraceLinkKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanRecord {
    pub span_id: SpanUid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<SpanUid>,
    pub name: String,
    pub kind: SpanKind,
    pub subsystem: SpanSubsystem,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub duration_us: u64,
    pub fields: BoundedJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SpanStatus>,
}

impl SpanRecord {
    #[must_use]
    pub fn new(
        span_id: SpanUid,
        parent_span_id: Option<SpanUid>,
        name: impl Into<String>,
        kind: SpanKind,
        fields: Value,
    ) -> Self {
        Self {
            span_id,
            parent_span_id,
            name: name.into(),
            kind,
            subsystem: kind.subsystem(),
            started_at_unix_ms: 0,
            finished_at_unix_ms: 0,
            duration_us: 0,
            fields: BoundedJson::capture(fields, DetailBudget::SpanFields),
            status: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub span_id: SpanUid,
    pub name: String,
    pub module: String,
    pub at_unix_ms: u64,
    pub details: BoundedJson,
}

impl EventRecord {
    #[must_use]
    pub fn new(
        span_id: SpanUid,
        name: impl Into<String>,
        module: impl Into<String>,
        details: Value,
    ) -> Self {
        Self {
            span_id,
            name: name.into(),
            module: module.into(),
            at_unix_ms: 0,
            details: BoundedJson::capture(details, DetailBudget::EventDetails),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceRecord {
    pub trace_id: TraceId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    pub kind: TraceKind,
    pub root_span_id: SpanUid,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub spans: Vec<SpanRecord>,
    pub events: Vec<EventRecord>,
    pub links: Vec<TraceLink>,
    pub resources: Vec<ResourceStats>,
    pub dropped_children: u64,
    pub truncated: bool,
}

impl TraceRecord {
    #[must_use]
    pub fn new(trace_id: TraceId, root_span_id: SpanUid) -> Self {
        Self {
            trace_id,
            request_id: None,
            kind: TraceKind::OpRequest,
            root_span_id,
            started_at_unix_ms: 0,
            finished_at_unix_ms: 0,
            spans: Vec::new(),
            events: Vec::new(),
            links: Vec::new(),
            resources: Vec::new(),
            dropped_children: 0,
            truncated: false,
        }
    }
}
