use std::collections::HashMap;

use serde_json::{json, Value};

use eos_workspace_api::{
    PrepareCommandRequest, PreparedCommandWorkspace, WorkspaceApiError, WorkspaceMode,
};

use super::types::IsolatedCommandSessionPort;

pub(super) fn prepare_command_workspace<P>(
    port: &P,
    request: PrepareCommandRequest,
) -> Result<PreparedCommandWorkspace, WorkspaceApiError>
where
    P: IsolatedCommandSessionPort,
{
    let context = port.prepare_context()?;
    let mode = if context.ns_fds.is_empty() {
        "fresh_ns"
    } else {
        "set_ns"
    };
    let ns_fds = ns_fds_value(&context.ns_fds);
    let PrepareCommandRequest {
        agent_id,
        command_session_id,
        invocation_id,
        cmd,
        timeout_seconds,
    } = request;
    let session_dir = context
        .scratch_dir
        .join("command-sessions")
        .join(&command_session_id);
    std::fs::create_dir_all(&session_dir).map_err(|error| {
        WorkspaceApiError::new(
            "isolated_command_prepare_failed",
            format!(
                "create command session dir {}: {error}",
                session_dir.display()
            ),
        )
    })?;
    let final_path = session_dir.join("final.json");
    let output_path = session_dir.join("runner-result.json");
    let request_path = session_dir.join("runner-request.json");
    let run_request = json!({
        "mode": mode,
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
        "layer_paths": context.layer_paths,
        "upperdir": context.upperdir,
        "workdir": context.workdir,
        "ns_fds": ns_fds,
        "cgroup_path": context.cgroup_path,
        "timeout_seconds": timeout_seconds,
    });

    Ok(PreparedCommandWorkspace {
        mode: WorkspaceMode::Isolated,
        run_request,
        request_path,
        output_path,
        final_path,
        finalize_context: json!({
            "session_dir": session_dir,
            "workspace_handle_id": context.workspace_handle_id,
            "published": false,
        }),
    })
}

fn ns_fds_value(map: &HashMap<String, i32>) -> Value {
    if map.is_empty() {
        Value::Null
    } else {
        json!({
            "user": namespace_fd(map, "user"),
            "mnt": namespace_fd(map, "mnt"),
            "pid": namespace_fd(map, "pid"),
            "net": namespace_fd(map, "net"),
        })
    }
}

fn namespace_fd(map: &HashMap<String, i32>, name: &str) -> Value {
    map.get(name).map_or(Value::Null, |fd| json!(*fd))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eos_workspace_api::CommandWorkspaceOps;

    use super::*;
    use crate::command_session::types::IsolatedCommandPrepareContext;
    use crate::IsolatedWorkspaceOps;

    #[derive(Debug, Clone)]
    struct FakePort {
        context: IsolatedCommandPrepareContext,
    }

    impl IsolatedCommandSessionPort for FakePort {
        fn prepare_context(&self) -> Result<IsolatedCommandPrepareContext, WorkspaceApiError> {
            Ok(self.context.clone())
        }
    }

    #[test]
    fn prepare_builds_setns_runner_request_without_publish(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let scratch_dir = std::env::temp_dir().join(format!(
            "eos-isolated-command-prepare-{}",
            std::process::id()
        ));
        let workspace_root = PathBuf::from("/configured-workspace");
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let ops = IsolatedWorkspaceOps::new(FakePort {
            context: IsolatedCommandPrepareContext {
                workspace_handle_id: "iws-1".to_owned(),
                workspace_root: workspace_root.clone(),
                scratch_dir: scratch_dir.clone(),
                layer_paths: vec![PathBuf::from("/lower/a")],
                upperdir: scratch_dir.join("upper"),
                workdir: scratch_dir.join("work"),
                ns_fds: HashMap::from([
                    ("user".to_owned(), 10),
                    ("mnt".to_owned(), 11),
                    ("pid".to_owned(), 12),
                    ("net".to_owned(), 13),
                ]),
                cgroup_path: Some(PathBuf::from("/sys/fs/cgroup/eos/iws-1")),
            },
        });

        let prepared = ops.prepare_command_workspace(PrepareCommandRequest {
            agent_id: "agent-1".to_owned(),
            command_session_id: "cmd-1".to_owned(),
            invocation_id: "inv-1".to_owned(),
            cmd: "pwd".to_owned(),
            timeout_seconds: Some(4.0),
        })?;

        assert_eq!(prepared.mode, WorkspaceMode::Isolated);
        assert_eq!(prepared.run_request["mode"], "set_ns");
        assert_eq!(
            prepared.run_request["workspace_root"],
            workspace_root.to_string_lossy().as_ref()
        );
        assert_eq!(prepared.run_request["ns_fds"]["user"], 10);
        assert_eq!(prepared.run_request["tool_call"]["intent"], "write_allowed");
        assert_eq!(prepared.run_request["tool_call"]["args"]["command"], "pwd");
        assert_eq!(prepared.run_request["layer_paths"][0], "/lower/a");
        assert_eq!(prepared.finalize_context["workspace_handle_id"], "iws-1");
        assert_eq!(prepared.finalize_context["published"], false);
        assert_eq!(
            prepared.request_path,
            scratch_dir
                .join("command-sessions")
                .join("cmd-1")
                .join("runner-request.json")
        );

        let _ = std::fs::remove_dir_all(scratch_dir);
        Ok(())
    }
}
