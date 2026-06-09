//! Runner audit/observability gates: evaluate normalized obs rows against the
//! state/transcript expectations a Rust runner supplies, producing a typed
//! [`RunnerGateReport`] of pass/fail plus metrics. Three input granularities
//! (source → batch → rows) feed one pure evaluator.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    normalize_agent_core_jsonl_line, normalize_sandbox_pull_response, ObsEnvelope,
    ObsNormalizationError, SandboxAuditLoss, SandboxPullBatch, OS_RESOURCE_SAMPLED,
    TOOL_CALL_COMPLETED,
};

const RESOURCE_METRIC_KEYS: &[&str] = &[
    "rss_bytes",
    "cpu_user_s",
    "cpu_system_s",
    "cpu_throttled_us",
    "io_read_bytes",
    "io_write_bytes",
    "io_read_ops",
    "io_write_ops",
];

/// External correctness evidence supplied by the Rust runner from state and transcript stores.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerCorrectnessEvidence {
    /// The runner checked expected tool-use ids and terminal tool outcomes against state/transcript.
    pub tool_use_verified: bool,
    /// Number of expected tool-use ids checked against state/transcript.
    pub tool_use_checked_count: usize,
    /// The runner checked model/user-facing message correctness against transcript state.
    pub message_correctness_verified: bool,
    /// Number of message/transcript assertions checked by the runner.
    pub message_checked_count: usize,
}

impl RunnerCorrectnessEvidence {
    /// Build evidence for successful state/transcript checks.
    #[must_use]
    pub const fn verified(tool_use_checked_count: usize, message_checked_count: usize) -> Self {
        Self {
            tool_use_verified: true,
            tool_use_checked_count,
            message_correctness_verified: true,
            message_checked_count,
        }
    }

    /// Return true when the runner supplied external tool correctness evidence.
    #[must_use]
    pub const fn has_tool_use_evidence(self) -> bool {
        self.tool_use_verified
    }

    /// Return true when the runner supplied external message correctness evidence.
    #[must_use]
    pub const fn has_message_correctness_evidence(self) -> bool {
        self.message_correctness_verified
    }
}

/// Tool-use expectation supplied by Rust state/transcript evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedToolUse {
    /// Provider/tool-call id that must have a `tool_call.completed` obs row.
    pub tool_use_id: String,
    /// Optional tool name copied from state/transcript for diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Whether state/transcript expected the call to be terminal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_expected: Option<bool>,
}

impl ExpectedToolUse {
    /// Build the minimum expected tool-use record.
    #[must_use]
    pub fn new(tool_use_id: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            tool_name: None,
            terminal_expected: None,
        }
    }

    /// Attach a state/transcript tool name for diagnostics.
    #[must_use]
    pub fn with_tool_name(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_name = Some(tool_name.into());
        self
    }

    /// Attach whether state/transcript expected a terminal tool call.
    #[must_use]
    pub const fn with_terminal_expected(mut self, terminal_expected: bool) -> Self {
        self.terminal_expected = Some(terminal_expected);
        self
    }
}

/// Runner gate switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerGateSettings {
    /// Fail when a bounded audit surface reports counted loss.
    pub strict_audit_loss: bool,
    /// Fail when no resource sample with a real metric is present.
    pub require_resource_sample: bool,
}

impl Default for RunnerGateSettings {
    fn default() -> Self {
        Self {
            strict_audit_loss: true,
            require_resource_sample: true,
        }
    }
}

/// Input for the Rust runner audit/observability gates.
#[derive(Debug, Clone, Copy)]
pub struct RunnerGateInput<'a> {
    /// Normalized observation rows from agent-core JSONL and sandbox pulls.
    pub rows: &'a [ObsEnvelope],
    /// Optional loss metadata from the sandbox audit ring.
    pub sandbox_loss: Option<&'a SandboxAuditLoss>,
    /// Tool-use records the Rust state/transcript says must be observed.
    pub expected_tool_uses: &'a [ExpectedToolUse],
    /// State/transcript correctness checks already performed by the runner.
    pub correctness: RunnerCorrectnessEvidence,
    /// Gate settings for this runner invocation.
    pub settings: RunnerGateSettings,
}

/// Batch-oriented input for runner gates after sandbox pull normalization.
#[derive(Debug, Clone, Copy)]
pub struct RunnerGateBatchInput<'a> {
    /// Normalized rows from agent-core JSONL.
    pub agent_core_rows: &'a [ObsEnvelope],
    /// Normalized sandbox pull/snapshot batches.
    pub sandbox_batches: &'a [SandboxPullBatch],
    /// Tool-use records the Rust state/transcript says must be observed.
    pub expected_tool_uses: &'a [ExpectedToolUse],
    /// State/transcript correctness checks already performed by the runner.
    pub correctness: RunnerCorrectnessEvidence,
    /// Gate settings for this runner invocation.
    pub settings: RunnerGateSettings,
}

/// Source-level input for the runner audit/observability gates.
#[derive(Debug, Clone, Copy)]
pub struct RunnerGateSourceInput<'a> {
    /// Agent-core `ObsEnvelope` JSONL contents.
    pub agent_core_jsonl: &'a str,
    /// Raw sandbox `api.audit.pull` / `api.audit.snapshot` response values.
    pub sandbox_pull_responses: &'a [Value],
    /// Tool-use records the Rust state/transcript says must be observed.
    pub expected_tool_uses: &'a [ExpectedToolUse],
    /// State/transcript correctness checks already performed by the runner.
    pub correctness: RunnerCorrectnessEvidence,
    /// Gate settings for this runner invocation.
    pub settings: RunnerGateSettings,
}

/// Aggregate metrics collected while evaluating runner gates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerGateMetrics {
    /// Unique expected tool-use ids supplied by state/transcript.
    pub expected_tool_use_count: usize,
    /// Expected tool-use ids that had a canonical `tool_call.completed` row.
    pub observed_expected_tool_use_count: usize,
    /// Total canonical `tool_call.completed` rows.
    pub tool_call_completed_count: usize,
    /// Total `os_resource.sampled` rows.
    pub resource_sample_count: usize,
    /// Total real resource metrics across resource rows.
    pub resource_metric_count: usize,
}

/// A failed runner gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerGateFailure {
    /// Machine-readable failure kind.
    pub kind: RunnerGateFailureKind,
    /// Human-readable detail for diagnostics.
    pub detail: String,
}

/// Machine-readable runner gate failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerGateFailureKind {
    /// A bounded audit surface reported counted loss.
    AuditLoss,
    /// State/transcript expected a tool-use id that had no obs row.
    MissingToolObs,
    /// A `tool_call.completed` row is missing required payload fields or has invalid values.
    InvalidToolPayload,
    /// Resource gates were enabled but no `os_resource.sampled` row was present.
    MissingResourceSample,
    /// Resource gates were enabled but no resource row carried a real metric.
    MissingResourceMetric,
    /// The runner did not supply external tool correctness evidence.
    ToolCorrectnessNotVerified,
    /// The runner did not supply external message correctness evidence.
    MessageCorrectnessNotVerified,
}

/// Typed result from evaluating the Rust runner audit/observability gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerGateReport {
    /// True when no gate failures were found.
    pub passed: bool,
    /// Failed gates, if any.
    pub failures: Vec<RunnerGateFailure>,
    /// Basic obs metrics useful for runner reports.
    pub metrics: RunnerGateMetrics,
    /// Tool-use records the Rust state/transcript expected to observe.
    pub expected_tool_uses: Vec<ExpectedToolUse>,
    /// Gate settings used for this evaluation.
    pub settings: RunnerGateSettings,
    /// State/transcript correctness evidence supplied by the runner.
    pub correctness: RunnerCorrectnessEvidence,
    /// Sandbox loss metadata used for this evaluation, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_loss: Option<SandboxAuditLoss>,
}

/// Evaluate Rust runner audit/observability gates over normalized rows.
#[must_use]
pub fn evaluate_runner_gates(input: RunnerGateInput<'_>) -> RunnerGateReport {
    let mut failures = Vec::new();
    let mut metrics = RunnerGateMetrics::default();
    let expected_tool_use_ids = unique_tool_use_ids(input.expected_tool_uses);
    metrics.expected_tool_use_count = expected_tool_use_ids.len();

    if input.settings.strict_audit_loss {
        if let Some(loss) = input.sandbox_loss {
            if loss.has_counted_loss() {
                failures.push(failure(
                    RunnerGateFailureKind::AuditLoss,
                    format!(
                        "sandbox audit loss reported lost_before_seq={:?} dropped_event_count={:?}",
                        loss.lost_before_seq, loss.dropped_event_count
                    ),
                ));
            }
        }
    }

    if !input.correctness.has_tool_use_evidence() {
        failures.push(failure(
            RunnerGateFailureKind::ToolCorrectnessNotVerified,
            "state/transcript tool correctness evidence was not supplied",
        ));
    } else if input.correctness.tool_use_checked_count < metrics.expected_tool_use_count {
        failures.push(failure(
            RunnerGateFailureKind::ToolCorrectnessNotVerified,
            format!(
                "state/transcript tool checks covered {} expected tool uses, expected {}",
                input.correctness.tool_use_checked_count, metrics.expected_tool_use_count
            ),
        ));
    }
    if !input.correctness.has_message_correctness_evidence() {
        failures.push(failure(
            RunnerGateFailureKind::MessageCorrectnessNotVerified,
            "state/transcript message correctness evidence was not supplied",
        ));
    } else if input.correctness.message_checked_count == 0 {
        failures.push(failure(
            RunnerGateFailureKind::MessageCorrectnessNotVerified,
            "state/transcript message correctness evidence had zero checked assertions",
        ));
    }

    let mut observed_tool_use_ids = BTreeSet::new();
    for row in input.rows {
        match row.event_type.as_str() {
            TOOL_CALL_COMPLETED => {
                metrics.tool_call_completed_count += 1;
                if let Some(tool_use_id) = tool_use_id(row) {
                    observed_tool_use_ids.insert(tool_use_id);
                }
                if !valid_tool_call(row) {
                    failures.push(failure(
                        RunnerGateFailureKind::InvalidToolPayload,
                        format!(
                            "invalid tool_call.completed payload for {:?}",
                            row.ids.tool_use_id
                        ),
                    ));
                }
            }
            OS_RESOURCE_SAMPLED => {
                metrics.resource_sample_count += 1;
                metrics.resource_metric_count += resource_metric_count(row);
            }
            _ => {}
        }
    }

    metrics.observed_expected_tool_use_count = expected_tool_use_ids
        .iter()
        .filter(|tool_use_id| observed_tool_use_ids.contains(**tool_use_id))
        .count();

    for tool_use_id in &expected_tool_use_ids {
        if !observed_tool_use_ids.contains(tool_use_id) {
            failures.push(failure(
                RunnerGateFailureKind::MissingToolObs,
                format!("missing tool_call.completed obs row for tool_use_id={tool_use_id}"),
            ));
        }
    }

    if input.settings.require_resource_sample {
        if metrics.resource_sample_count == 0 {
            failures.push(failure(
                RunnerGateFailureKind::MissingResourceSample,
                "no os_resource.sampled row was observed",
            ));
        } else if metrics.resource_metric_count == 0 {
            failures.push(failure(
                RunnerGateFailureKind::MissingResourceMetric,
                "os_resource.sampled rows did not include any resource metric",
            ));
        }
    }

    RunnerGateReport {
        passed: failures.is_empty(),
        failures,
        metrics,
        expected_tool_uses: input.expected_tool_uses.to_vec(),
        settings: input.settings,
        correctness: input.correctness,
        sandbox_loss: input.sandbox_loss.cloned(),
    }
}

/// Evaluate runner gates from normalized agent-core rows and sandbox pull batches.
#[must_use]
pub fn evaluate_runner_gate_batches(input: RunnerGateBatchInput<'_>) -> RunnerGateReport {
    let mut rows = Vec::with_capacity(
        input.agent_core_rows.len()
            + input
                .sandbox_batches
                .iter()
                .map(|batch| batch.rows.len())
                .sum::<usize>(),
    );
    rows.extend_from_slice(input.agent_core_rows);
    rows.extend(
        input
            .sandbox_batches
            .iter()
            .flat_map(|batch| batch.rows.iter().cloned()),
    );
    let sandbox_loss = if input.sandbox_batches.is_empty() {
        None
    } else {
        Some(SandboxAuditLoss::merge(
            input.sandbox_batches.iter().map(|batch| &batch.loss),
        ))
    };
    evaluate_runner_gates(RunnerGateInput {
        rows: &rows,
        sandbox_loss: sandbox_loss.as_ref(),
        expected_tool_uses: input.expected_tool_uses,
        correctness: input.correctness,
        settings: input.settings,
    })
}

/// Parse source artifacts, normalize them, and evaluate runner gates.
///
/// This is the narrow handoff a Rust runner can call after it has collected the
/// authoritative state/transcript expectations.
///
/// # Errors
///
/// Returns an error when any agent-core JSONL row or sandbox pull response is
/// invalid.
pub fn evaluate_runner_gate_sources(
    input: RunnerGateSourceInput<'_>,
) -> Result<RunnerGateReport, ObsNormalizationError> {
    let agent_core_rows = input
        .agent_core_jsonl
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(normalize_agent_core_jsonl_line)
        .collect::<Result<Vec<_>, _>>()?;
    let sandbox_batches = input
        .sandbox_pull_responses
        .iter()
        .map(normalize_sandbox_pull_response)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(evaluate_runner_gate_batches(RunnerGateBatchInput {
        agent_core_rows: &agent_core_rows,
        sandbox_batches: &sandbox_batches,
        expected_tool_uses: input.expected_tool_uses,
        correctness: input.correctness,
        settings: input.settings,
    }))
}

fn unique_tool_use_ids(ids: &[ExpectedToolUse]) -> BTreeSet<&str> {
    ids.iter()
        .map(|expected| expected.tool_use_id.as_str())
        .collect()
}

fn valid_tool_call(row: &ObsEnvelope) -> bool {
    let Some(Value::Object(tool_call)) = row.payload.get("tool_call") else {
        return false;
    };
    has_nonnegative_number(tool_call.get("duration_ms"))
        .or_else(|| has_nonnegative_number(tool_call.get("total_ms")))
        .unwrap_or(false)
        && non_empty_string(tool_call.get("status"))
            .or_else(|| non_empty_string(tool_call.get("exit_status")))
            .unwrap_or(false)
}

fn tool_use_id(row: &ObsEnvelope) -> Option<&str> {
    row.ids.tool_use_id.as_deref().or_else(|| {
        row.payload
            .get("tool_call")
            .and_then(|section| section.get("tool_use_id"))
            .and_then(Value::as_str)
    })
}

fn resource_metric_count(row: &ObsEnvelope) -> usize {
    let Some(Value::Object(os_resource)) = row.payload.get("os_resource") else {
        return 0;
    };
    RESOURCE_METRIC_KEYS
        .iter()
        .filter(|key| has_nonnegative_number(os_resource.get(**key)).unwrap_or(false))
        .count()
}

fn has_nonnegative_number(value: Option<&Value>) -> Option<bool> {
    value
        .and_then(Value::as_f64)
        .map(|number| number.is_finite() && number >= 0.0)
}

fn non_empty_string(value: Option<&Value>) -> Option<bool> {
    value
        .and_then(Value::as_str)
        .map(|value| !value.trim().is_empty())
}

fn failure(kind: RunnerGateFailureKind, detail: impl Into<String>) -> RunnerGateFailure {
    RunnerGateFailure {
        kind,
        detail: detail.into(),
    }
}

#[cfg(test)]
#[path = "../tests/gates/mod.rs"]
mod tests;
