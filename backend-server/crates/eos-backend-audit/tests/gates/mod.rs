#![allow(clippy::unwrap_used)]
use crate::{to_jsonl_line, JsonObject, ObsIds, ObsSource};
use serde_json::json;

use super::*;

#[test]
fn runner_gates_pass_with_state_evidence_tool_obs_and_resource_metric() {
    let expected_tool_uses = vec![ExpectedToolUse::new("toolu-1")
        .with_tool_name("exec_command")
        .with_terminal_expected(false)];
    let rows = vec![
        tool_row("toolu-1", json!({"duration_ms": 12.0, "status": "ok"})),
        resource_row(json!({"sampled_at_monotonic_s": 1.0, "rss_bytes": 1024})),
    ];

    let report = evaluate_runner_gates(RunnerGateInput {
        rows: &rows,
        sandbox_loss: Some(&SandboxAuditLoss::default()),
        expected_tool_uses: &expected_tool_uses,
        correctness: RunnerCorrectnessEvidence::verified(1, 1),
        settings: RunnerGateSettings::default(),
    });

    assert!(report.passed);
    assert_eq!(report.failures, Vec::new());
    assert_eq!(
        report.metrics,
        RunnerGateMetrics {
            expected_tool_use_count: 1,
            observed_expected_tool_use_count: 1,
            tool_call_completed_count: 1,
            resource_sample_count: 1,
            resource_metric_count: 1,
        }
    );
    assert_eq!(report.expected_tool_uses, expected_tool_uses);
}

#[test]
fn runner_gates_fail_on_missing_expected_tool_obs() {
    let expected_tool_uses = vec![ExpectedToolUse::new("toolu-expected")];
    let rows = vec![resource_row(
        json!({"sampled_at_monotonic_s": 1.0, "cpu_user_s": 0.2}),
    )];

    let report = evaluate_runner_gates(RunnerGateInput {
        rows: &rows,
        sandbox_loss: None,
        expected_tool_uses: &expected_tool_uses,
        correctness: verified_correctness(),
        settings: RunnerGateSettings::default(),
    });

    assert!(!report.passed);
    assert_failure(&report, RunnerGateFailureKind::MissingToolObs);
}

#[test]
fn runner_gates_fail_on_counted_audit_loss() {
    let expected_tool_uses = Vec::new();
    let rows = vec![resource_row(
        json!({"sampled_at_monotonic_s": 1.0, "io_read_bytes": 12}),
    )];
    let loss = SandboxAuditLoss {
        cursor_after_seq: Some(42),
        lost_before_seq: Some(10),
        dropped_event_count: Some(1),
    };

    let report = evaluate_runner_gates(RunnerGateInput {
        rows: &rows,
        sandbox_loss: Some(&loss),
        expected_tool_uses: &expected_tool_uses,
        correctness: verified_correctness(),
        settings: RunnerGateSettings::default(),
    });

    assert!(!report.passed);
    assert_failure(&report, RunnerGateFailureKind::AuditLoss);
}

#[test]
fn runner_gates_fail_without_external_correctness_evidence() {
    let expected_tool_uses = Vec::new();
    let rows = vec![resource_row(
        json!({"sampled_at_monotonic_s": 1.0, "io_write_ops": 2}),
    )];

    let report = evaluate_runner_gates(RunnerGateInput {
        rows: &rows,
        sandbox_loss: None,
        expected_tool_uses: &expected_tool_uses,
        correctness: RunnerCorrectnessEvidence::default(),
        settings: RunnerGateSettings::default(),
    });

    assert!(!report.passed);
    assert_failure(&report, RunnerGateFailureKind::ToolCorrectnessNotVerified);
    assert_failure(
        &report,
        RunnerGateFailureKind::MessageCorrectnessNotVerified,
    );
}

#[test]
fn runner_gates_fail_when_correctness_counts_do_not_cover_expectations() {
    let expected_tool_uses = vec![
        ExpectedToolUse::new("toolu-1"),
        ExpectedToolUse::new("toolu-2"),
    ];
    let rows = vec![
        tool_row("toolu-1", json!({"duration_ms": 12.0, "status": "ok"})),
        tool_row("toolu-2", json!({"duration_ms": 13.0, "status": "ok"})),
        resource_row(json!({"sampled_at_monotonic_s": 1.0, "rss_bytes": 1024})),
    ];

    let report = evaluate_runner_gates(RunnerGateInput {
        rows: &rows,
        sandbox_loss: None,
        expected_tool_uses: &expected_tool_uses,
        correctness: RunnerCorrectnessEvidence {
            tool_use_verified: true,
            tool_use_checked_count: 1,
            message_correctness_verified: true,
            message_checked_count: 0,
        },
        settings: RunnerGateSettings::default(),
    });

    assert!(!report.passed);
    assert_failure(&report, RunnerGateFailureKind::ToolCorrectnessNotVerified);
    assert_failure(
        &report,
        RunnerGateFailureKind::MessageCorrectnessNotVerified,
    );
}

#[test]
fn runner_gates_fail_on_invalid_tool_payload_and_empty_resource_sample() {
    let expected_tool_uses = vec![ExpectedToolUse::new("toolu-1")];
    let rows = vec![
        tool_row("toolu-1", json!({"duration_ms": -1.0, "status": ""})),
        resource_row(json!({"sampled_at_monotonic_s": 1.0})),
    ];

    let report = evaluate_runner_gates(RunnerGateInput {
        rows: &rows,
        sandbox_loss: None,
        expected_tool_uses: &expected_tool_uses,
        correctness: verified_correctness(),
        settings: RunnerGateSettings::default(),
    });

    assert!(!report.passed);
    assert_failure(&report, RunnerGateFailureKind::InvalidToolPayload);
    assert_failure(&report, RunnerGateFailureKind::MissingResourceMetric);
}

#[test]
fn runner_gate_report_serializes_stable_json() {
    let report = RunnerGateReport {
        passed: false,
        failures: vec![RunnerGateFailure {
            kind: RunnerGateFailureKind::AuditLoss,
            detail: "dropped rows".to_owned(),
        }],
        metrics: RunnerGateMetrics {
            expected_tool_use_count: 1,
            observed_expected_tool_use_count: 0,
            tool_call_completed_count: 0,
            resource_sample_count: 1,
            resource_metric_count: 1,
        },
        expected_tool_uses: vec![ExpectedToolUse::new("toolu-1")
            .with_tool_name("exec_command")
            .with_terminal_expected(false)],
        settings: RunnerGateSettings {
            strict_audit_loss: true,
            require_resource_sample: false,
        },
        correctness: verified_correctness(),
        sandbox_loss: Some(SandboxAuditLoss {
            cursor_after_seq: Some(12),
            lost_before_seq: Some(4),
            dropped_event_count: Some(2),
        }),
    };

    let value = serde_json::to_value(&report).expect("serialize runner gate report");
    let round_trip: RunnerGateReport =
        serde_json::from_value(value.clone()).expect("deserialize runner gate report");

    assert_eq!(value["failures"][0]["kind"], json!("audit_loss"));
    assert_eq!(value["metrics"]["resource_metric_count"], json!(1));
    assert_eq!(value["settings"]["require_resource_sample"], json!(false));
    assert_eq!(
        value["expected_tool_uses"][0]["tool_use_id"],
        json!("toolu-1")
    );
    assert_eq!(
        value["expected_tool_uses"][0]["tool_name"],
        json!("exec_command")
    );
    assert_eq!(
        value["expected_tool_uses"][0]["terminal_expected"],
        json!(false)
    );
    assert_eq!(value["correctness"]["tool_use_verified"], json!(true));
    assert_eq!(value["correctness"]["tool_use_checked_count"], json!(1));
    assert_eq!(value["correctness"]["message_checked_count"], json!(1));
    assert_eq!(value["sandbox_loss"]["dropped_event_count"], json!(2));
    assert_eq!(round_trip, report);
}

#[test]
fn runner_gate_batches_flatten_rows_and_merge_sandbox_loss() {
    let expected_tool_uses = vec![ExpectedToolUse::new("toolu-1")];
    let agent_rows = vec![tool_row(
        "toolu-1",
        json!({"duration_ms": 12.0, "status": "ok"}),
    )];
    let sandbox_batches = vec![
        SandboxPullBatch {
            rows: vec![resource_row(
                json!({"sampled_at_monotonic_s": 1.0, "rss_bytes": 1024}),
            )],
            loss: SandboxAuditLoss {
                cursor_after_seq: Some(10),
                lost_before_seq: None,
                dropped_event_count: Some(1),
            },
        },
        SandboxPullBatch {
            rows: Vec::new(),
            loss: SandboxAuditLoss {
                cursor_after_seq: Some(14),
                lost_before_seq: Some(7),
                dropped_event_count: Some(2),
            },
        },
    ];

    let report = evaluate_runner_gate_batches(RunnerGateBatchInput {
        agent_core_rows: &agent_rows,
        sandbox_batches: &sandbox_batches,
        expected_tool_uses: &expected_tool_uses,
        correctness: verified_correctness(),
        settings: RunnerGateSettings::default(),
    });

    assert!(!report.passed);
    assert_failure(&report, RunnerGateFailureKind::AuditLoss);
    assert_eq!(report.metrics.tool_call_completed_count, 1);
    assert_eq!(report.metrics.resource_sample_count, 1);
    assert_eq!(
        report.sandbox_loss,
        Some(SandboxAuditLoss {
            cursor_after_seq: Some(14),
            lost_before_seq: Some(7),
            dropped_event_count: Some(3),
        })
    );
}

#[test]
fn runner_gate_sources_parse_jsonl_and_sandbox_pull_responses() {
    let expected_tool_uses = vec![ExpectedToolUse::new("toolu-1")];
    let agent_core_jsonl = to_jsonl_line(&tool_row(
        "toolu-1",
        json!({"duration_ms": 12.0, "status": "ok"}),
    ))
    .expect("serialize obs row");
    let sandbox_pulls = vec![json!({
        "schema": eos_protocol::audit::SCHEMA_VERSION,
        "cursor": {"after_seq": 4},
        "buffer": {"dropped_event_count": 0},
        "events": [{
            "seq": 4,
            "lane": "sample",
            "type": "os_resource.sampled",
            "payload": {
                "os_resource": {
                    "tool_use_id": "toolu-1",
                    "sampled_at_monotonic_s": 1.0,
                    "rss_bytes": 1024
                }
            }
        }]
    })];

    let report = evaluate_runner_gate_sources(RunnerGateSourceInput {
        agent_core_jsonl: &agent_core_jsonl,
        sandbox_pull_responses: &sandbox_pulls,
        expected_tool_uses: &expected_tool_uses,
        correctness: verified_correctness(),
        settings: RunnerGateSettings::default(),
    })
    .expect("evaluate source artifacts");

    assert!(report.passed);
    assert_eq!(report.metrics.tool_call_completed_count, 1);
    assert_eq!(report.metrics.resource_sample_count, 1);
    assert_eq!(
        report.sandbox_loss,
        Some(SandboxAuditLoss {
            cursor_after_seq: Some(4),
            lost_before_seq: None,
            dropped_event_count: Some(0),
        })
    );
}

fn verified_correctness() -> RunnerCorrectnessEvidence {
    RunnerCorrectnessEvidence::verified(1, 1)
}

fn tool_row(tool_use_id: &str, section: Value) -> ObsEnvelope {
    let mut payload = JsonObject::new();
    payload.insert("tool_call".to_owned(), section);
    ObsEnvelope::new(ObsSource::AgentCore, TOOL_CALL_COMPLETED)
        .with_ids(ObsIds {
            tool_use_id: Some(tool_use_id.to_owned()),
            ..ObsIds::default()
        })
        .with_payload(payload)
}

fn resource_row(section: Value) -> ObsEnvelope {
    let mut payload = JsonObject::new();
    payload.insert("os_resource".to_owned(), section);
    ObsEnvelope::new(ObsSource::AgentCore, OS_RESOURCE_SAMPLED).with_payload(payload)
}

fn assert_failure(report: &RunnerGateReport, kind: RunnerGateFailureKind) {
    assert!(
        report.failures.iter().any(|failure| failure.kind == kind),
        "expected failure {kind:?}, got {:?}",
        report.failures
    );
}
