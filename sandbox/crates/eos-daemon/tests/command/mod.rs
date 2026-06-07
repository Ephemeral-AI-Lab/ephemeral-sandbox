use serde_json::json;

#[cfg(target_os = "linux")]
use eos_command_session::{
    CollectCompleted, CommandResponse, CommandSessionCompletion, CommandSessionManager,
    ReadCommandProgress,
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
fn exec_timeout_uses_config_default_only_when_omitted() {
    let config = crate::config::CommandSessionConfig {
        default_timeout_s: 600,
        ..crate::config::CommandSessionConfig::default()
    };

    assert_eq!(exec_timeout_seconds(&json!({}), &config), 600.0);
    assert_eq!(exec_timeout_seconds(&json!({"timeout": 12}), &config), 12.0);
    assert_eq!(
        exec_timeout_seconds(&json!({"timeout_seconds": 34}), &config),
        34.0
    );
    assert_eq!(
        exec_timeout_seconds(&json!({"timeout": 12, "timeout_seconds": 34}), &config),
        12.0
    );
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
fn command_session_completion_result_can_be_read_by_progress_tool() -> TestResult {
    let manager = CommandSessionManager::default();
    manager.push_completed(test_completion("cmd_keep", "caller", "keep\n"));
    manager.push_completed(test_completion("cmd_done", "caller", "a\ndone\n"));

    let result = manager.read_progress(ReadCommandProgress {
        command_session_id: "cmd_done".to_owned(),
        last_n_lines: 1,
    })?;
    assert_eq!(result.status, "ok");
    assert_eq!(result.stdout, "done\n");

    let redelivered = manager.read_progress(ReadCommandProgress {
        command_session_id: "cmd_done".to_owned(),
        last_n_lines: 2,
    })?;
    assert_eq!(redelivered.stdout, "a\ndone\n");

    let remaining = manager.collect_completed(&CollectCompleted {
        command_session_ids: Some(vec!["cmd_keep".to_owned()]),
        caller_id: None,
    });
    assert_eq!(remaining.completions.len(), 1);

    // Remove-on-deliver: a second collect finds nothing — the map is bounded,
    // not accumulating delivered entries forever.
    let redelivered = manager.collect_completed(&CollectCompleted {
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
fn command_session_read_progress_returns_completed_result_when_live_session_is_gone() -> TestResult
{
    let id = "cmd_progress_done_unit";
    command_session_manager().push_completed(test_completion(id, "caller", "written\n"));

    let response =
        command_session_read_progress(&json!({"command_session_id": id, "last_n_lines": 1}))?;

    assert_eq!(response["status"], "ok");
    assert_eq!(response["output"]["stdout"], "written\n");
    let remaining = command_session_manager().collect_completed(&CollectCompleted {
        command_session_ids: Some(vec![id.to_owned()]),
        caller_id: None,
    });
    assert_eq!(remaining.completions.len(), 1);
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn command_session_write_stdin_does_not_claim_parked_completion() -> TestResult {
    let id = "cmd_stdin_done_unit";
    command_session_manager().push_completed(test_completion(id, "caller", "written\n"));

    let response =
        command_session_write_stdin(&json!({"command_session_id": id, "chars": "ignored"}))?;

    assert_eq!(response["status"], "error");
    assert_eq!(response["output"]["stderr"], "command_session_not_found");
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn command_session_cancel_returns_completed_result_when_live_session_is_gone() -> TestResult {
    let id = "command_session_cancel_done_unit";
    command_session_manager().push_completed(test_completion(id, "caller", "already-finished\n"));

    let response = command_session_cancel(&json!({"command_session_id": id}))?;

    assert_eq!(response["status"], "ok");
    assert_eq!(response["output"]["stdout"], "already-finished\n");
    let remaining = command_session_manager().collect_completed(&CollectCompleted {
        command_session_ids: Some(vec![id.to_owned()]),
        caller_id: None,
    });
    assert_eq!(remaining.completions.len(), 0);
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
