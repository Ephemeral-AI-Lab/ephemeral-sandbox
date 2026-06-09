//! Shared audit and observability contracts.
//!
//! This module owns the passive audit event, sink, and normalized observability
//! row shapes shared by agent-core producers and backend collectors. It does not
//! own producer policy, persistence, daemon rings, tracing, or report rendering.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

pub use eos_types::JsonObject;
use eos_types::{
    AgentRunId, AttemptId, Clock, IterationId, RequestId, SandboxId, TaskId, ToolUseId,
    UtcDateTime, WorkflowId,
};

/// Normalized observability contract schema.
pub const SCHEMA: &str = "eos.obs.v1";

/// Canonical event name for completed tool calls.
pub const TOOL_CALL_COMPLETED: &str = "tool_call.completed";
/// Canonical event name for completed agent runs.
pub const AGENT_RUN_COMPLETED: &str = "agent_run.completed";
/// Canonical event name for resource samples.
pub const OS_RESOURCE_SAMPLED: &str = "os_resource.sampled";

/// The serialized-row schema version for structured audit events.
pub const SCHEMA_VERSION: u32 = 1;

/// The source that produced the native event before collector normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ObsSource {
    /// Rust agent control-plane event.
    AgentCore,
    /// Rust sandbox daemon event.
    Sandbox,
}

/// Correlation ids shared by normalized audit/observability rows.
///
/// Every field is optional because a row should carry only ids the producer or
/// collector actually knows. Non-id labels such as `tool_name` belong in
/// `payload`, not here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObsIds {
    /// Owning request id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Owning task id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Owning agent-run id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_run_id: Option<String>,
    /// Provider/tool-call id used to join agent-core and sandbox rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Owning sandbox id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<String>,
}

/// A normalized audit/observability row consumed by collectors and reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObsEnvelope {
    /// Contract schema tag.
    #[serde(deserialize_with = "deserialize_obs_schema")]
    pub schema: String,
    /// Native source that produced the row.
    pub source: ObsSource,
    /// Canonical event type.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Common correlation ids.
    #[serde(default)]
    pub ids: ObsIds,
    /// Event-specific sections such as `tool_call`, `occ`, or `os_resource`.
    #[serde(default)]
    pub payload: JsonObject,
    /// Sandbox ring sequence, when the source has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    /// Sandbox ring lane, when the source has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
}

impl ObsEnvelope {
    /// Build a normalized row with the default schema.
    #[must_use]
    pub fn new(source: ObsSource, event_type: impl Into<String>) -> Self {
        let event_type = event_type.into();
        Self {
            schema: SCHEMA.to_owned(),
            source,
            event_type: canonical_event_type(&event_type).to_owned(),
            ids: ObsIds::default(),
            payload: JsonObject::new(),
            seq: None,
            lane: None,
        }
    }

    /// Set the row ids.
    #[must_use]
    pub fn with_ids(mut self, ids: ObsIds) -> Self {
        self.ids = ids;
        self
    }

    /// Set the row payload.
    #[must_use]
    pub fn with_payload(mut self, payload: JsonObject) -> Self {
        self.payload = payload;
        self
    }

    /// Set sandbox ring metadata.
    #[must_use]
    pub fn with_ring_metadata(mut self, seq: i64, lane: impl Into<String>) -> Self {
        self.seq = Some(seq);
        self.lane = Some(lane.into());
        self
    }
}

/// Return the canonical event type for a native or legacy event type.
#[must_use]
pub fn canonical_event_type(event_type: &str) -> &str {
    match event_type {
        "tool_call.finished" => TOOL_CALL_COMPLETED,
        other => other,
    }
}

/// Parse one normalized JSONL row.
///
/// The parser accepts any valid JSON object matching [`ObsEnvelope`]. Native
/// sandbox rows should be normalized by the collector before calling this.
///
/// # Errors
///
/// Returns a JSON parse error when the line is not a valid normalized row.
pub fn from_jsonl_line(line: &str) -> Result<ObsEnvelope, serde_json::Error> {
    let mut row: ObsEnvelope = serde_json::from_str(line)?;
    row.event_type = canonical_event_type(&row.event_type).to_owned();
    Ok(row)
}

/// Serialize one normalized row to a JSONL line.
///
/// # Errors
///
/// Returns a JSON serialization error if the payload contains a non-serializable
/// value.
pub fn to_jsonl_line(row: &ObsEnvelope) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(row)?;
    line.push('\n');
    Ok(line)
}

/// Errors reported by an [`AuditSink`].
///
/// These are recoverable failures surfaced through `Result` without interrupting
/// the emitting domain path. Sinks must not panic.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuditError {
    /// Appending the event to a `JSONL` file failed.
    #[error("audit jsonl write failed")]
    Jsonl(#[from] std::io::Error),
    /// Encoding the event to canonical JSON failed.
    #[error("audit event serialization failed")]
    Serialize(#[from] serde_json::Error),
    /// The bounded sink queue is full; the event was dropped rather than
    /// blocking the caller's runtime thread.
    #[error("audit sink queue is full")]
    Backpressure,
}

/// The behavior-owning package that emitted an event.
///
/// Serializes to the exact Rust literal strings: `workflow`, `engine`,
/// `sandbox`, and `live_e2e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditSource {
    /// Workflow-layer emitter.
    Workflow,
    /// Engine-layer emitter.
    Engine,
    /// Sandbox-layer emitter.
    Sandbox,
    /// Live end-to-end harness emitter.
    #[serde(rename = "live_e2e")]
    LiveE2e,
}

/// Correlation envelope for an [`AuditEvent`].
///
/// Every field defaults to `None`; build one with [`AuditNode::builder`] or
/// [`AuditNode::default`] plus field assignment. Typed ids come from
/// `eos-types`; `agent_name` is a human label and `tool_name` is downstream
/// owned, so both stay `String`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditNode {
    /// Owning request id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    /// Owning delegated-workflow id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<WorkflowId>,
    /// Owning iteration id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration_id: Option<IterationId>,
    /// Owning attempt id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    /// Owning task id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// Agent label, not an id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// Owning agent-run id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_run_id: Option<AgentRunId>,
    /// Owning sandbox id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<SandboxId>,
    /// Tool name, not an id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Owning tool-use id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<ToolUseId>,
}

impl AuditNode {
    /// Start a fresh [`AuditNodeBuilder`].
    pub fn builder() -> AuditNodeBuilder {
        AuditNodeBuilder::default()
    }
}

/// Fluent builder for [`AuditNode`]; set only the ids a producer knows.
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct AuditNodeBuilder {
    node: AuditNode,
}

impl AuditNodeBuilder {
    /// Set the request id.
    pub fn request_id(mut self, id: RequestId) -> Self {
        self.node.request_id = Some(id);
        self
    }

    /// Set the delegated-workflow id.
    pub fn workflow_id(mut self, id: WorkflowId) -> Self {
        self.node.workflow_id = Some(id);
        self
    }

    /// Set the iteration id.
    pub fn iteration_id(mut self, id: IterationId) -> Self {
        self.node.iteration_id = Some(id);
        self
    }

    /// Set the attempt id.
    pub fn attempt_id(mut self, id: AttemptId) -> Self {
        self.node.attempt_id = Some(id);
        self
    }

    /// Set the task id.
    pub fn task_id(mut self, id: TaskId) -> Self {
        self.node.task_id = Some(id);
        self
    }

    /// Set the agent label.
    pub fn agent_name(mut self, name: impl Into<String>) -> Self {
        self.node.agent_name = Some(name.into());
        self
    }

    /// Set the agent-run id.
    pub fn agent_run_id(mut self, id: AgentRunId) -> Self {
        self.node.agent_run_id = Some(id);
        self
    }

    /// Set the sandbox id.
    pub fn sandbox_id(mut self, id: SandboxId) -> Self {
        self.node.sandbox_id = Some(id);
        self
    }

    /// Set the tool name.
    pub fn tool_name(mut self, name: impl Into<String>) -> Self {
        self.node.tool_name = Some(name.into());
        self
    }

    /// Set the tool-use id.
    pub fn tool_use_id(mut self, id: ToolUseId) -> Self {
        self.node.tool_use_id = Some(id);
        self
    }

    /// Finish building the [`AuditNode`].
    #[must_use]
    pub fn build(self) -> AuditNode {
        self.node
    }
}

/// A structured audit event emitted by a behavior-owning package.
///
/// Construct via [`AuditEvent::new`] so `ts` is set once from the injected
/// [`Clock`]. Field order is the wire order; `payload` is always serialized and
/// `correlation_id` is omitted when `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditEvent {
    /// Serialized-row schema version.
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    /// The emitting package.
    pub source: AuditSource,
    /// The event-type string.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Correlation envelope of known ids.
    pub node: AuditNode,
    /// Event-specific payload object.
    #[serde(default)]
    pub payload: JsonObject,
    /// Optional cross-event correlation id.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub correlation_id: Option<String>,
    /// Emission timestamp, stamped once from the injected clock.
    pub ts: UtcDateTime,
}

impl AuditEvent {
    /// Build an event, stamping `ts` from `clock`.
    #[must_use]
    pub fn new(
        source: AuditSource,
        event_type: impl Into<String>,
        node: AuditNode,
        payload: JsonObject,
        clock: &dyn Clock,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            source,
            event_type: event_type.into(),
            node,
            payload,
            correlation_id: None,
            ts: clock.now(),
        }
    }

    /// Convert this producer event into the normalized collector row.
    #[must_use]
    pub fn to_obs_envelope(&self) -> ObsEnvelope {
        let mut payload = self.payload.clone();
        if let Some(agent_name) = &self.node.agent_name {
            payload
                .entry("agent_name".to_owned())
                .or_insert_with(|| serde_json::Value::String(agent_name.clone()));
        }
        if let Some(tool_name) = &self.node.tool_name {
            payload
                .entry("tool_name".to_owned())
                .or_insert_with(|| serde_json::Value::String(tool_name.clone()));
        }

        ObsEnvelope::new(obs_source(self.source), &self.event_type)
            .with_ids(obs_ids(&self.node))
            .with_payload(payload)
    }
}

/// Write-only audit side channel.
///
/// Implementations must not panic; recoverable failures are reported through
/// [`AuditError`]. The event is borrowed, not consumed.
pub trait AuditSink: Send + Sync {
    /// Whether this sink persists events.
    ///
    /// Emitters can use this to skip expensive audit-only sampling work when the
    /// composition root installed the no-op sink.
    fn enabled(&self) -> bool {
        true
    }

    /// Persist one event.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError`] when the sink cannot persist the event.
    fn publish(&self, event: &AuditEvent) -> Result<(), AuditError>;
}

/// Audit sink used when collection is disabled; every publish is a no-op.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopAuditSink;

impl AuditSink for NoopAuditSink {
    fn enabled(&self) -> bool {
        false
    }

    fn publish(&self, _event: &AuditEvent) -> Result<(), AuditError> {
        Ok(())
    }
}

const fn obs_source(source: AuditSource) -> ObsSource {
    match source {
        AuditSource::Sandbox => ObsSource::Sandbox,
        AuditSource::Workflow | AuditSource::Engine | AuditSource::LiveE2e => ObsSource::AgentCore,
    }
}

fn obs_ids(node: &AuditNode) -> ObsIds {
    ObsIds {
        request_id: node.request_id.as_ref().map(ToString::to_string),
        task_id: node.task_id.as_ref().map(ToString::to_string),
        agent_run_id: node.agent_run_id.as_ref().map(ToString::to_string),
        tool_use_id: node.tool_use_id.as_ref().map(ToString::to_string),
        sandbox_id: node.sandbox_id.as_ref().map(ToString::to_string),
    }
}

fn deserialize_obs_schema<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom(format_args!(
            "unsupported observability schema {schema:?}"
        )))
    }
}

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == SCHEMA_VERSION {
        Ok(version)
    } else {
        Err(D::Error::custom(format_args!(
            "unsupported audit schema version {version}"
        )))
    }
}
