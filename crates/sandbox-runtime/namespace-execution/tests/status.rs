mod support;

use sandbox_runtime_namespace_execution::NamespaceExecutionTerminalStatus;
use serde_json::json;
use support::{outcome, run_result, run_result_payload, run_result_without_status};

#[test]
fn as_str_strings_match_the_wire_vocabulary() {
    assert_eq!(NamespaceExecutionTerminalStatus::Ok.as_str(), "ok");
    assert_eq!(NamespaceExecutionTerminalStatus::Error.as_str(), "error");
    assert_eq!(
        NamespaceExecutionTerminalStatus::TimedOut.as_str(),
        "timed_out"
    );
    assert_eq!(
        NamespaceExecutionTerminalStatus::Cancelled.as_str(),
        "cancelled"
    );
}

#[test]
fn status_projects_the_payload_status_string() {
    assert_eq!(
        outcome(run_result(0, "ok")).status(),
        NamespaceExecutionTerminalStatus::Ok
    );
    assert_eq!(
        outcome(run_result(1, "error")).status(),
        NamespaceExecutionTerminalStatus::Error
    );
    assert_eq!(
        outcome(run_result(0, "timed_out")).status(),
        NamespaceExecutionTerminalStatus::TimedOut
    );
    assert_eq!(
        outcome(run_result(0, "cancelled")).status(),
        NamespaceExecutionTerminalStatus::Cancelled
    );
}

#[test]
fn status_defaults_to_error_when_absent_or_unknown() {
    assert_eq!(
        outcome(run_result_without_status(1)).status(),
        NamespaceExecutionTerminalStatus::Error
    );
    assert_eq!(
        outcome(run_result(0, "bogus")).status(),
        NamespaceExecutionTerminalStatus::Error
    );
}

#[test]
fn status_defaults_to_error_for_non_string_or_non_object_payloads() {
    assert_eq!(
        outcome(run_result_payload(1, json!({ "status": 7 }))).status(),
        NamespaceExecutionTerminalStatus::Error
    );
    assert_eq!(
        outcome(run_result_payload(1, json!(42))).status(),
        NamespaceExecutionTerminalStatus::Error
    );
}

#[test]
fn payload_exposes_the_raw_value() {
    assert_eq!(
        outcome(run_result(0, "ok")).payload().to_string(),
        r#"{"status":"ok"}"#
    );
}
