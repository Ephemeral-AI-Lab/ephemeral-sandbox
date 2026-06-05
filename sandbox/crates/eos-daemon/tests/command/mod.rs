use serde_json::json;

#[cfg(target_os = "linux")]
use eos_command_session::{
    CollectCompleted, CommandResponse, CommandSessionCompletion, CommandSessionRegistry,
};

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn exec_command_requires_string_wire_shape() {
    assert!(require_command_string(&json!({"cmd": "echo hi"}), "cmd").is_ok());
    assert!(require_command_string(&json!({"cmd": ["true"]}), "cmd").is_err());
}

#[test]
fn exec_command_preserves_shell_string_bytes_after_validation() -> TestResult {
    assert_eq!(
        require_command_string(&json!({"cmd": "  printf hi\n"}), "cmd")?,
        "  printf hi\n"
    );
    Ok(())
}

#[test]
fn optional_u64_accepts_unsigned_and_nonnegative_signed_numbers() {
    assert_eq!(optional_u64(&json!({"timeout": 7_u64}), "timeout"), Some(7));
    assert_eq!(optional_u64(&json!({"timeout": 7_i64}), "timeout"), Some(7));
    assert_eq!(optional_u64(&json!({"timeout": -1_i64}), "timeout"), None);
}

#[test]
fn command_session_cancel_suppresses_background_completion_publication() {
    assert!(should_publish_command_session_completion(true, false, true));
    assert!(!should_publish_command_session_completion(true, true, true));
    assert!(!should_publish_command_session_completion(
        true, false, false
    ));
    assert!(!should_publish_command_session_completion(
        false, false, true
    ));
    assert!(!should_publish_command_session_completion(
        false, true, false
    ));
}

#[test]
#[cfg(target_os = "linux")]
fn command_session_completion_result_can_be_claimed_by_control_tool() -> TestResult {
    let registry = CommandSessionRegistry::new();
    registry.push_completed(test_completion("cmd_keep", "caller", "keep\n"));
    registry.push_completed(test_completion("cmd_done", "caller", "done\n"));

    let result = registry
        .take_completed_result("cmd_done")
        .ok_or("matching completion should be returned")?;
    assert_eq!(result.status, "ok");
    assert!(registry.take_completed_result("cmd_done").is_none());

    let remaining = registry.collect_completed(&CollectCompleted {
        command_session_ids: Some(vec!["cmd_keep".to_owned()]),
        caller_id: None,
    });
    assert_eq!(remaining.completions.len(), 1);

    // Remove-on-deliver: a second collect finds nothing — the map is bounded,
    // not accumulating delivered entries forever.
    let redelivered = registry.collect_completed(&CollectCompleted {
        command_session_ids: Some(vec!["cmd_keep".to_owned()]),
        caller_id: None,
    });
    assert_eq!(redelivered.completions.len(), 0);
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn command_session_count_uses_runtime_manager() -> TestResult {
    let response = op_command_session_count(
        &json!({"caller_id": "no-live-session"}),
        DispatchContext::empty(),
    )?;

    assert_eq!(response["success"], true);
    assert_eq!(response["caller_id"], "no-live-session");
    assert_eq!(response["count"], 0);
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn command_session_write_stdin_returns_completed_result_when_live_session_is_gone() -> TestResult {
    let id = "cmd_stdin_done_unit";
    command_session_manager()
        .registry()
        .push_completed(test_completion(id, "caller", "written\n"));

    let response =
        command_session_write_stdin(&json!({"command_session_id": id, "chars": "ignored"}))?;

    assert_eq!(response["status"], "ok");
    assert_eq!(response["output"]["stdout"], "written\n");
    assert!(command_session_manager()
        .registry()
        .take_completed_result(id)
        .is_none());
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn command_session_cancel_returns_completed_result_when_live_session_is_gone() -> TestResult {
    let id = "command_session_cancel_done_unit";
    command_session_manager()
        .registry()
        .push_completed(test_completion(id, "caller", "already-finished\n"));

    let response = command_session_cancel(&json!({"command_session_id": id}))?;

    assert_eq!(response["status"], "ok");
    assert_eq!(response["output"]["stdout"], "already-finished\n");
    assert!(command_session_manager()
        .registry()
        .take_completed_result(id)
        .is_none());
    Ok(())
}

#[cfg(target_os = "linux")]
fn test_completion(id: &str, caller_id: &str, stdout: &str) -> CommandSessionCompletion {
    let result = CommandResponse {
        status: "ok".to_owned(),
        exit_code: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
        command_session_id: Some(id.to_owned()),
        workspace_mode: None,
        metadata: serde_json::Value::Null,
    };
    CommandSessionCompletion {
        command_session_id: id.to_owned(),
        caller_id: caller_id.to_owned(),
        command: "test".to_owned(),
        result: result.clone(),
        notification_result: result,
    }
}
