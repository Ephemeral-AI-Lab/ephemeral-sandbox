use serde_json::json;

use eos_workspace_api::{
    PrepareCommandRequest, PreparedCommandWorkspace, WorkspaceApiError, WorkspaceMode,
};

use super::types::EphemeralCommandSessionPort;
use crate::{EphemeralDirAllocator, EphemeralWorkspaceError, InvocationId};

pub(super) fn prepare_command_workspace<P>(
    port: &P,
    request: PrepareCommandRequest,
) -> Result<PreparedCommandWorkspace, WorkspaceApiError>
where
    P: EphemeralCommandSessionPort,
{
    let PrepareCommandRequest {
        agent_id,
        command_session_id: _,
        invocation_id,
        cmd,
        timeout_seconds,
    } = request;
    let context = port.prepare_context()?;
    let snapshot_request_id = format!("command_session:{agent_id}:{invocation_id}");
    let snapshot = port
        .acquire_snapshot(&snapshot_request_id)
        .map_err(prepare_error)?;
    let mut dirs = match EphemeralDirAllocator::new(context.writable_root)
        .allocate("sandbox-overlay", &InvocationId(invocation_id.clone()))
    {
        Ok(dirs) => dirs,
        Err(error) => {
            let _ = port.release_snapshot(&snapshot.lease_id);
            return Err(prepare_error(error));
        }
    };
    dirs.output_path = dirs.run_dir.join("command-runner-result.json");
    let request_path = dirs.run_dir.join("command-runner-request.json");
    dirs.request_path = Some(request_path.clone());
    dirs.final_path = context.final_path.clone();

    let run_request = json!({
        "mode": "fresh_ns",
        "tool_call": {
            "invocation_id": invocation_id,
            "agent_id": agent_id,
            "verb": "exec_command",
            "intent": "write_allowed",
            "args": {
                "command": cmd,
                "cwd": ".",
            },
            "background": false,
        },
        "workspace_root": context.workspace_root,
        "layer_paths": snapshot.layer_paths.clone(),
        "upperdir": dirs.upperdir.clone(),
        "workdir": dirs.workdir.clone(),
        "ns_fds": null,
        "cgroup_path": null,
        "timeout_seconds": timeout_seconds,
    });

    Ok(PreparedCommandWorkspace {
        mode: WorkspaceMode::Ephemeral,
        run_request,
        request_path,
        output_path: dirs.output_path.clone(),
        final_path: context.final_path.clone(),
        finalize_context: json!({
            "root": context.layer_stack_root,
            "session_dir": context.session_dir,
            "snapshot": snapshot,
            "dirs": dirs,
        }),
    })
}

fn prepare_error(error: EphemeralWorkspaceError) -> WorkspaceApiError {
    WorkspaceApiError::new("ephemeral_command_prepare_failed", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eos_workspace_api::CommandWorkspaceOps;

    use super::*;
    use crate::command_session::types::EphemeralCommandPrepareContext;
    use crate::{EphemeralSnapshot, EphemeralWorkspaceOps};

    #[derive(Debug, Clone)]
    struct FakePort {
        context: EphemeralCommandPrepareContext,
    }

    impl EphemeralCommandSessionPort for FakePort {
        fn prepare_context(&self) -> Result<EphemeralCommandPrepareContext, WorkspaceApiError> {
            Ok(self.context.clone())
        }

        fn acquire_snapshot(
            &self,
            request_id: &str,
        ) -> Result<EphemeralSnapshot, EphemeralWorkspaceError> {
            assert_eq!(request_id, "command_session:agent-1:inv-1");
            Ok(EphemeralSnapshot {
                lease_id: "lease-1".to_owned(),
                manifest_version: 7,
                manifest_root_hash: "hash".to_owned(),
                layer_paths: vec![PathBuf::from("/lower/a"), PathBuf::from("/lower/b")],
            })
        }
    }

    #[test]
    fn prepare_builds_fresh_runner_request_and_finalize_context(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let writable_root = std::env::temp_dir().join(format!(
            "eos-ephemeral-command-prepare-{}",
            std::process::id()
        ));
        let workspace_root = PathBuf::from("/configured-workspace");
        let _ = std::fs::remove_dir_all(&writable_root);
        let ops = EphemeralWorkspaceOps::new(FakePort {
            context: EphemeralCommandPrepareContext {
                layer_stack_root: PathBuf::from("/layers"),
                workspace_root: workspace_root.clone(),
                writable_root: writable_root.clone(),
                session_dir: PathBuf::from("/sessions/cmd-1"),
                final_path: PathBuf::from("/sessions/cmd-1/final.json"),
            },
        });

        let prepared = ops.prepare_command_workspace(PrepareCommandRequest {
            agent_id: "agent-1".to_owned(),
            command_session_id: "cmd-1".to_owned(),
            invocation_id: "inv-1".to_owned(),
            cmd: "printf ok".to_owned(),
            timeout_seconds: Some(2.5),
        })?;

        assert_eq!(prepared.mode, WorkspaceMode::Ephemeral);
        assert_eq!(prepared.run_request["mode"], "fresh_ns");
        assert_eq!(
            prepared.run_request["workspace_root"],
            workspace_root.to_string_lossy().as_ref()
        );
        assert_eq!(prepared.run_request["tool_call"]["intent"], "write_allowed");
        assert_eq!(
            prepared.run_request["tool_call"]["args"]["command"],
            "printf ok"
        );
        assert_eq!(prepared.run_request["layer_paths"][0], "/lower/a");
        assert_eq!(
            prepared
                .request_path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("command-runner-request.json")
        );
        assert_eq!(
            prepared
                .output_path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("command-runner-result.json")
        );
        assert_eq!(
            prepared.final_path,
            PathBuf::from("/sessions/cmd-1/final.json")
        );
        assert_eq!(prepared.finalize_context["root"], "/layers");
        assert_eq!(prepared.finalize_context["session_dir"], "/sessions/cmd-1");
        assert_eq!(prepared.finalize_context["snapshot"]["lease_id"], "lease-1");
        assert!(prepared.finalize_context.get("dirs").is_some());

        let _ = std::fs::remove_dir_all(writable_root);
        Ok(())
    }
}
