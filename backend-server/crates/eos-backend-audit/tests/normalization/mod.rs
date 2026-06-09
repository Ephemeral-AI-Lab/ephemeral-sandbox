#![allow(clippy::unwrap_used)]
use super::*;
use crate::{ObsSource, TOOL_CALL_COMPLETED};
use serde_json::json;

#[test]
fn agent_core_jsonl_row_parses_with_contract_helper() {
    let line = r#"{"schema":"eos.obs.v1","source":"agent_core","type":"agent_run.completed","ids":{"agent_run_id":"ar-1"},"payload":{"agent_run":{"status":"ok"}}}"#;

    let row = normalize_agent_core_jsonl_line(line).expect("parse agent-core row");

    assert_eq!(row.source, ObsSource::AgentCore);
    assert_eq!(row.ids.agent_run_id.as_deref(), Some("ar-1"));
    assert_eq!(row.payload["agent_run"]["status"], json!("ok"));
}

#[test]
fn sandbox_pull_normalizes_events_aliases_ids_and_loss() {
    let response = json!({
        "schema": eos_protocol::audit::SCHEMA_VERSION,
        "cursor": {"after_seq": 42, "lost_before_seq": 10},
        "buffer": {"dropped_event_count": 3, "lost_before_seq": 10},
        "events": [{
            "seq": 41,
            "lane": "normal",
            "type": "tool_call.finished",
            "payload": {
                "tool_call": {
                    "tool_use_id": "toolu-1",
                    "tool_name": "exec_command",
                    "duration_ms": 42.0
                }
            }
        }]
    });

    let batch = normalize_sandbox_pull_response(&response).expect("normalize pull response");

    assert_eq!(
        batch.loss,
        SandboxAuditLoss {
            cursor_after_seq: Some(42),
            lost_before_seq: Some(10),
            dropped_event_count: Some(3),
        }
    );
    let row = &batch.rows[0];
    assert_eq!(row.source, ObsSource::Sandbox);
    assert_eq!(row.event_type, TOOL_CALL_COMPLETED);
    assert_eq!(row.seq, Some(41));
    assert_eq!(row.lane.as_deref(), Some("normal"));
    assert_eq!(row.ids.tool_use_id.as_deref(), Some("toolu-1"));
    assert_eq!(row.payload["tool_call"]["tool_name"], json!("exec_command"));
}

#[test]
fn sandbox_resource_row_extracts_tool_use_id() {
    let event = json!({
        "seq": 7,
        "lane": "sample",
        "type": "os_resource.sampled",
        "payload": {
            "os_resource": {
                "tool_use_id": "toolu-2",
                "sampled_at_monotonic_s": 1.5,
                "cpu_user_s": 0.2
            }
        }
    });

    let row = normalize_sandbox_event(&event).expect("normalize resource row");

    assert_eq!(row.ids.tool_use_id.as_deref(), Some("toolu-2"));
    assert_eq!(row.payload["os_resource"]["cpu_user_s"], json!(0.2));
}

#[test]
fn sandbox_pull_rejects_wrong_schema() {
    let response = json!({"schema":"wrong","events":[]});

    match normalize_sandbox_pull_response(&response) {
        Err(ObsNormalizationError::SandboxSchema) => {}
        other => panic!("expected schema error, got {other:?}"),
    }
}

#[test]
fn sandbox_loss_merge_summarizes_multiple_pulls() {
    let first = SandboxAuditLoss {
        cursor_after_seq: Some(10),
        lost_before_seq: None,
        dropped_event_count: Some(2),
    };
    let second = SandboxAuditLoss {
        cursor_after_seq: Some(14),
        lost_before_seq: Some(7),
        dropped_event_count: Some(3),
    };

    let merged = SandboxAuditLoss::merge([&first, &second]);

    assert_eq!(
        merged,
        SandboxAuditLoss {
            cursor_after_seq: Some(14),
            lost_before_seq: Some(7),
            dropped_event_count: Some(5),
        }
    );
    assert!(merged.has_counted_loss());
}
