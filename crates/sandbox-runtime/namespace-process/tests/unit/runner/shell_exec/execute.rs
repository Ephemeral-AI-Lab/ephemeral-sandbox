use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::runner::protocol::NamespaceRunnerRequest;

#[test]
fn command_timeout_survives_as_timed_out_result() {
    let workspace_root = unique_workspace_root();
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let request = NamespaceRunnerRequest {
        request_id: "timeout-regression".to_owned(),
        args: json!({ "command": "sleep 5", "cwd": "." }),
        workspace_root: workspace_root.clone(),
        layer_paths: vec![],
        upperdir: None,
        workdir: None,
        ns_fds: None,
        timeout_seconds: Some(0.05),
        trace: None,
        parent: None,
        observability_log_path: None,
    };

    let result =
        crate::runner::shell_exec::execute_shell(&request).expect("timeout should produce a result");

    assert_eq!(result.exit_code, 124);
    assert_eq!(result.payload["status"], "timed_out");
    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn trace_handoff_writes_namespace_process_spawn_span() {
    let workspace_root = unique_workspace_root();
    let log_path = workspace_root
        .join("observability")
        .join("observability.ndjson");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let request = NamespaceRunnerRequest {
        request_id: "trace-regression".to_owned(),
        args: json!({ "command": "true", "cwd": "." }),
        workspace_root: workspace_root.clone(),
        layer_paths: vec![],
        upperdir: None,
        workdir: None,
        ns_fds: None,
        timeout_seconds: Some(5.0),
        trace: Some("req-child".to_owned()),
        parent: Some("d-5".to_owned()),
        observability_log_path: Some(log_path.clone()),
    };

    let result = crate::runner::shell_exec::execute_shell(&request).expect("command runs");

    assert_eq!(result.exit_code, 0);
    let line = std::fs::read_to_string(&log_path).expect("span log");
    let value: serde_json::Value = serde_json::from_str(line.trim()).expect("span json");
    assert_eq!(value["kind"], "span");
    assert_eq!(value["trace"], "req-child");
    assert_eq!(value["span"], "np-0");
    assert_eq!(value["parent"], "d-5");
    assert_eq!(value["name"], "namespace.runner.spawn_child");
    assert_eq!(value["status"], "completed");
    let _ = std::fs::remove_dir_all(workspace_root);
}

fn unique_workspace_root() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "eos-namespace-process-shell-timeout-{}-{nanos}",
        std::process::id()
    ))
}
