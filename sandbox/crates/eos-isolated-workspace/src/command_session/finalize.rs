use serde_json::{json, Value};

use eos_protocol::LayerChange;
use eos_workspace_api::{
    ChangedPathKinds, FinalizeCommandRequest, WorkspaceApiError, WorkspaceCommandOutcome,
    WorkspaceMode, WorkspaceTimings,
};

use super::types::IsolatedCommandSessionPort;

pub(super) fn finalize_command_workspace<P>(
    port: &P,
    request: FinalizeCommandRequest,
) -> Result<WorkspaceCommandOutcome, WorkspaceApiError>
where
    P: IsolatedCommandSessionPort,
{
    let context = port.finalize_context()?;
    let capture_start = std::time::Instant::now();
    let changes = eos_overlay::capture_upperdir(&context.upperdir)
        .map_err(|err| workspace_api_error(format!("capture isolated upperdir: {err}")))?;
    let capture_s = capture_start.elapsed().as_secs_f64();
    let path_kinds = path_changes_to_wire(&changes);
    let changed_paths: Vec<String> = path_kinds.iter().map(|(path, _)| path.clone()).collect();
    let changed_path_kinds = path_kinds.into_iter().collect::<ChangedPathKinds>();
    let mut timings = context.base_timings;
    merge_runner_timings(&mut timings, request.runner_result.as_ref());
    timings.insert(
        "resource.command_exec.changed_path_count".to_owned(),
        json!(usize_to_f64_saturating(changed_paths.len())),
    );
    timings.insert(
        "command_exec.capture_upperdir_s".to_owned(),
        json!(capture_s),
    );
    timings.insert("command_exec.occ_apply_s".to_owned(), json!(0.0));
    timings.insert(
        "command_exec.total_s".to_owned(),
        json!(request.command_elapsed_s),
    );
    timings.insert(
        "api.exec_command.total_s".to_owned(),
        json!(request.command_elapsed_s),
    );
    timings.insert(
        "api.exec_command.dispatch_total_s".to_owned(),
        json!(request.command_elapsed_s),
    );
    let exit_code = request.exit_code.unwrap_or(1);
    let duration_ms = request.command_elapsed_s * 1000.0;
    let status = request.status;
    let command_session_id = request.command_session_id;
    let audit_command_session_id = command_session_id.clone().unwrap_or_default();
    let caller_id = context.caller_id;
    let workspace_handle_id = context.workspace_handle_id;
    let manifest_version = context.manifest_version;
    let manifest_root_hash = context.manifest_root_hash;
    Ok(WorkspaceCommandOutcome {
        mode: WorkspaceMode::Isolated,
        success: true,
        status: status.clone(),
        exit_code: Some(exit_code),
        stdout: request.stdout,
        stderr: request.stderr,
        command_session_id,
        changed_paths,
        changed_path_kinds,
        mutation_source: "isolated_workspace".to_owned(),
        conflict: None,
        conflict_reason: None,
        timings,
        metadata: json!({
            "isolated_workspace": {
                "caller_id": caller_id,
                "workspace_handle_id": workspace_handle_id.clone(),
                "manifest_version": manifest_version,
                "manifest_root_hash": manifest_root_hash,
                "published": false,
            },
            "warnings": [],
            "spool_truncated": request.spool_truncated,
            "audit": {
                "workspace_handle_id": workspace_handle_id,
                "exit_code": exit_code,
                "argv0": "bash",
                "status": status,
                "published": false,
                "command_session_id": audit_command_session_id,
                "duration_s": request.command_elapsed_s,
                "total_ms": duration_ms,
                "phases_ms": {
                    "exec": duration_ms,
                },
            },
        }),
    })
}

fn workspace_api_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("isolated_command_finalize_failed", error.to_string())
}

fn path_changes_to_wire(changes: &[LayerChange]) -> Vec<(String, String)> {
    changes
        .iter()
        .map(|change| {
            (
                change.path().as_str().to_owned(),
                layer_change_kind(change).to_owned(),
            )
        })
        .collect()
}

const fn layer_change_kind(change: &LayerChange) -> &'static str {
    match change {
        LayerChange::Write { .. } => "write",
        LayerChange::Delete { .. } => "delete",
        LayerChange::Symlink { .. } => "symlink",
        LayerChange::OpaqueDir { .. } => "opaque_dir",
    }
}

fn merge_runner_timings(timings: &mut WorkspaceTimings, runner_result: Option<&Value>) {
    let Some(runner_timings) = runner_result
        .and_then(|runner| runner.get("tool_result"))
        .and_then(|tool_result| tool_result.get("timings"))
        .and_then(Value::as_object)
    else {
        return;
    };
    for (key, value) in runner_timings {
        timings.entry(key.clone()).or_insert_with(|| value.clone());
    }
    if let Some(value) = timings.get("workspace.mount_s").cloned() {
        timings
            .entry("command_exec.mount_workspace_s".to_owned())
            .or_insert(value);
    }
    if let Some(value) = timings.get("workspace.tool_s").cloned() {
        timings
            .entry("command_exec.run_command_s".to_owned())
            .or_insert(value);
    }
}

fn usize_to_f64_saturating(value: usize) -> f64 {
    u64::try_from(value).map_or(f64::MAX, u64_to_f64_saturating)
}

fn u64_to_f64_saturating(value: u64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use eos_workspace_api::WorkspaceMode;

    use super::*;
    use crate::command_session::types::IsolatedCommandFinalizeContext;

    #[derive(Debug, Clone)]
    struct FakePort {
        context: IsolatedCommandFinalizeContext,
    }

    impl IsolatedCommandSessionPort for FakePort {
        fn prepare_context(
            &self,
        ) -> Result<crate::command_session::types::IsolatedCommandPrepareContext, WorkspaceApiError>
        {
            unreachable!("finalize test does not prepare command workspaces")
        }

        fn finalize_context(&self) -> Result<IsolatedCommandFinalizeContext, WorkspaceApiError> {
            Ok(self.context.clone())
        }

        fn record_command_audit(&self, _payload: Value) {
            unreachable!("finalize test records audit in policy, not finalize helper")
        }
    }

    #[test]
    fn finalize_captures_audit_only_changes_without_publish(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "eos-isolated-command-finalize-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let upperdir = root.join("upper");
        std::fs::create_dir_all(&upperdir)?;
        std::fs::write(upperdir.join("private.txt"), b"private")?;
        let port = FakePort {
            context: IsolatedCommandFinalizeContext {
                caller_id: "caller-1".to_owned(),
                workspace_handle_id: "iws-1".to_owned(),
                manifest_version: 7,
                manifest_root_hash: "hash".to_owned(),
                upperdir,
                base_timings: BTreeMap::new(),
            },
        };

        let outcome = finalize_command_workspace(
            &port,
            FinalizeCommandRequest {
                runner_result: Some(json!({
                    "tool_result": {
                        "timings": {
                            "workspace.mount_s": 0.1,
                            "workspace.tool_s": 0.2,
                        }
                    },
                    "exit_code": 0,
                })),
                command_elapsed_s: 1.25,
                spool_truncated: true,
                status: "ok".to_owned(),
                exit_code: Some(0),
                stdout: "done".to_owned(),
                stderr: String::new(),
                command_session_id: Some("cmd-1".to_owned()),
            },
        )?;

        assert_eq!(outcome.mode, WorkspaceMode::Isolated);
        assert!(outcome.success);
        assert_eq!(outcome.changed_paths, vec!["private.txt"]);
        assert_eq!(outcome.changed_path_kinds["private.txt"], "write");
        assert_eq!(outcome.timings["command_exec.occ_apply_s"], 0.0);
        assert_eq!(outcome.timings["command_exec.mount_workspace_s"], 0.1);
        assert_eq!(outcome.metadata["isolated_workspace"]["published"], false);
        assert_eq!(outcome.metadata["audit"]["published"], false);
        assert_eq!(outcome.metadata["spool_truncated"], true);

        let _ = std::fs::remove_dir_all(root);
        Ok(())
    }
}
