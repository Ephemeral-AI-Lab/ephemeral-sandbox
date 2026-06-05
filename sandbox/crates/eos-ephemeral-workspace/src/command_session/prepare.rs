use serde_json::json;

use eos_workspace_api::{PrepareCommandRequest, PreparedCommandWorkspace, WorkspaceApiError};

use super::policy::EphemeralCommandWorkspace;
use super::types::EphemeralCommandSessionPort;
use crate::{EphemeralDirAllocator, EphemeralWorkspaceError, InvocationId};

pub(super) struct PreparedEphemeralCommand {
    pub prepared: PreparedCommandWorkspace,
    pub workspace: EphemeralCommandWorkspace,
}

pub(super) fn prepare_command_workspace<P>(
    port: &P,
    request: PrepareCommandRequest,
) -> Result<PreparedEphemeralCommand, WorkspaceApiError>
where
    P: EphemeralCommandSessionPort,
{
    let PrepareCommandRequest {
        caller_id,
        command_session_id,
        invocation_id,
        cmd,
        timeout_seconds,
    } = request;
    let context = port.prepare_context(&command_session_id)?;
    std::fs::create_dir_all(&context.session_dir).map_err(workspace_api_error)?;
    std::fs::write(
        context.session_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "command_session_id": &command_session_id,
            "caller_id": &caller_id,
            "invocation_id": &invocation_id,
            "workspace": "ephemeral",
            "command": &cmd,
            "status": "running",
        }))
        .map_err(workspace_api_error)?,
    )
    .map_err(workspace_api_error)?;
    let snapshot_request_id = format!("command_session:{caller_id}:{invocation_id}");
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
            "invocation_id": &invocation_id,
            "caller_id": &caller_id,
            "verb": "exec_command",
            "intent": "write_allowed",
            "args": {
                "command": &cmd,
                "cwd": ".",
            },
            "background": false,
        },
        "workspace_root": context.workspace_root.clone(),
        "layer_paths": snapshot.layer_paths.clone(),
        "upperdir": dirs.upperdir.clone(),
        "workdir": dirs.workdir.clone(),
        "ns_fds": null,
        "cgroup_path": null,
        "timeout_seconds": timeout_seconds,
    });

    let workspace = EphemeralCommandWorkspace {
        caller_id,
        invocation_id,
        root: context.layer_stack_root,
        lease_id: snapshot.lease_id,
        manifest_version: snapshot.manifest_version,
        manifest_root_hash: snapshot.manifest_root_hash,
        layer_paths: snapshot.layer_paths,
        workspace_root: context.workspace_root,
        dirs: dirs.clone(),
    };

    Ok(PreparedEphemeralCommand {
        prepared: PreparedCommandWorkspace {
            run_request,
            request_path,
            output_path: dirs.output_path.clone(),
            final_path: context.final_path,
            session_dir: context.session_dir.clone(),
            transcript_path: context.session_dir.join("transcript.log"),
        },
        workspace,
    })
}

fn prepare_error(error: EphemeralWorkspaceError) -> WorkspaceApiError {
    WorkspaceApiError::new("ephemeral_command_prepare_failed", error.to_string())
}

fn workspace_api_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("ephemeral_command_prepare_failed", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eos_workspace_api::CommandWorkspacePolicy;

    use super::*;
    use crate::command_session::types::EphemeralCommandPrepareContext;
    use crate::command_session::EphemeralCommandPolicy;
    use crate::EphemeralSnapshot;

    #[derive(Debug, Clone)]
    struct FakePort {
        context: EphemeralCommandPrepareContext,
    }

    impl EphemeralCommandSessionPort for FakePort {
        fn prepare_context(
            &self,
            command_session_id: &str,
        ) -> Result<EphemeralCommandPrepareContext, WorkspaceApiError> {
            assert_eq!(command_session_id, "cmd-1");
            Ok(self.context.clone())
        }

        fn acquire_snapshot(
            &self,
            request_id: &str,
        ) -> Result<EphemeralSnapshot, EphemeralWorkspaceError> {
            assert_eq!(request_id, "command_session:caller-1:inv-1");
            Ok(EphemeralSnapshot {
                lease_id: "lease-1".to_owned(),
                manifest_version: 7,
                manifest_root_hash: "hash".to_owned(),
                layer_paths: vec![PathBuf::from("/lower/a"), PathBuf::from("/lower/b")],
            })
        }

        fn release_snapshot(&self, lease_id: &str) -> Result<(), EphemeralWorkspaceError> {
            assert_eq!(lease_id, "lease-1");
            Ok(())
        }

        fn base_timings(&self) -> Result<eos_workspace_api::WorkspaceTimings, WorkspaceApiError> {
            Ok(Default::default())
        }

        fn publish_upperdir_changes(
            &self,
            _root: &crate::WorkspaceRoot,
            _snapshot: &EphemeralSnapshot,
            _changes: &[eos_protocol::LayerChange],
            _path_kinds: &[crate::PathChange],
        ) -> Result<crate::PublishOutcome, EphemeralWorkspaceError> {
            unreachable!("prepare test does not finalize command workspaces")
        }
    }

    #[test]
    fn prepare_builds_fresh_runner_request_and_session_metadata(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let writable_root = std::env::temp_dir().join(format!(
            "eos-ephemeral-command-prepare-{}",
            std::process::id()
        ));
        let session_dir = writable_root.join("sessions").join("cmd-1");
        let workspace_root = PathBuf::from("/configured-workspace");
        let _ = std::fs::remove_dir_all(&writable_root);
        let policy = EphemeralCommandPolicy::new(FakePort {
            context: EphemeralCommandPrepareContext {
                layer_stack_root: PathBuf::from("/layers"),
                workspace_root: workspace_root.clone(),
                writable_root: writable_root.clone(),
                session_dir: session_dir.clone(),
                final_path: session_dir.join("final.json"),
            },
        });

        let prepared = policy.prepare_command_workspace(PrepareCommandRequest {
            caller_id: "caller-1".to_owned(),
            command_session_id: "cmd-1".to_owned(),
            invocation_id: "inv-1".to_owned(),
            cmd: "printf ok".to_owned(),
            timeout_seconds: Some(2.5),
        })?;

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
        assert_eq!(prepared.final_path, session_dir.join("final.json"));
        assert_eq!(prepared.session_dir, session_dir);
        assert_eq!(
            prepared.transcript_path,
            prepared.session_dir.join("transcript.log")
        );
        let metadata = std::fs::read_to_string(prepared.session_dir.join("metadata.json"))?;
        assert!(metadata.contains("\"workspace\": \"ephemeral\""));

        let _ = std::fs::remove_dir_all(writable_root);
        Ok(())
    }
}
