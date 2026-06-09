//! Stats response DTOs.
//!
//! The `/api/stats/*` shapes are owned here and assembled by the Phase 6 stats
//! queries in `eos-backend-audit`, which read `obs_event` and `audit_cursor`. They
//! are derived from observability rows only: the richer agent-core state join
//! (agent name, token count, terminal outcome) is owned by the request/read
//! handlers through `eos-agent-core-server`.
//!
//! The response shapes are pinned for `OpenAPI` via `JsonSchema`, alongside the
//! API route shapes the Phase 7 crate serves.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use eos_types::AgentRunId;

/// Timing and resource summary derived from `obs_event` (`/api/stats/performance`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PerformanceStats {
    /// Count of `tool_call.completed` observability rows.
    pub tool_call_count: u64,
    /// Sum of tool-call `duration_ms` (falling back to `total_ms`).
    pub tool_call_total_ms: f64,
    /// Mean tool-call duration, or `None` when no tool calls were observed.
    pub tool_call_avg_ms: Option<f64>,
    /// Count of `os_resource.sampled` observability rows.
    pub resource_sample_count: u64,
    /// Largest observed `os_resource.rss_bytes`, when any sample carried one.
    pub rss_bytes_max: Option<i64>,
}

/// Correctness summary derived from observability rows (`/api/stats/correctness`).
///
/// `audit_matched` vs `audit_unmatched` keep daemon rows joined through
/// `sandbox_call_correlation` distinct from daemon rows with no bridge (AC7); an
/// unmatched row never borrows a model-facing `tool_use_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CorrectnessStats {
    /// Count of `agent_run.completed` observability rows.
    pub agent_runs_observed: u64,
    /// Count of `tool_call.completed` observability rows.
    pub tool_calls_observed: u64,
    /// Daemon audit rows joined to a correlation bridge (model ids populated).
    pub audit_matched: u64,
    /// Daemon audit rows with an invocation id but no bridge (model ids null).
    pub audit_unmatched: u64,
}

/// Per-agent-run rollup derived from `obs_event` (`/api/stats/agent-runs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentRunStat {
    /// The agent run these rows are attributed to.
    pub agent_run_id: AgentRunId,
    /// Count of `tool_call.completed` rows for this run.
    pub tool_call_count: u64,
    /// Sum of tool-call `duration_ms` (falling back to `total_ms`) for this run.
    pub tool_call_total_ms: f64,
    /// Count of `os_resource.sampled` rows for this run.
    pub resource_sample_count: u64,
}

/// Loss accounting across the observability pipeline.
///
/// The two `obs_*` counters are the live in-memory tallies of the
/// `PersistingSink` in `eos-backend-audit` (enqueue overflow and drainer write
/// failures); the two `audit_*` counters are durable, summed from `audit_cursor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ObsLossStats {
    /// Audit events dropped at `publish` because the bounded queue was full.
    pub obs_dropped_inflight: u64,
    /// Accepted events the drainer could not durably persist after retry.
    pub obs_persist_failed: u64,
    /// Sum of each sandbox's current-epoch `audit_cursor.dropped_count` (daemon ring
    /// drops). The daemon counter restarts on reboot, so this is not a lifetime
    /// total; `audit_sandboxes_with_loss` is the durable cross-epoch loss signal.
    pub audit_dropped: u64,
    /// Number of sandboxes whose cursor recorded a `lost_before_seq` boundary.
    pub audit_sandboxes_with_loss: u64,
}
