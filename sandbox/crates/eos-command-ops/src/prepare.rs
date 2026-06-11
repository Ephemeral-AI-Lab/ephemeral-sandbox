use std::path::{Path, PathBuf};

use eos_ephemeral_workspace::MountPlan;
use eos_namespace::protocol::{
    Intent, NsFds, RunMode, RunRequest, RunnerVerb, ToolCall, WorkspaceRoot,
};
use serde_json::{json, Value};

use crate::outcome::WorkspaceApiError;
use crate::CommandBinding;

pub(crate) struct PreparedCommand {
    pub(crate) run_request: Value,
    pub(crate) request_path: PathBuf,
    pub(crate) output_path: PathBuf,
    pub(crate) final_path: PathBuf,
    pub(crate) transcript_path: PathBuf,
}

pub(crate) struct PrepareInputs<'a> {
    pub(crate) caller_id: &'a str,
    pub(crate) command_id: &'a str,
    pub(crate) invocation_id: &'a str,
    pub(crate) cmd: &'a str,
    pub(crate) timeout_seconds: Option<f64>,
    pub(crate) session_dir: PathBuf,
    pub(crate) workspace_label: &'a str,
}

pub(crate) fn prepare_ephemeral(
    inputs: PrepareInputs<'_>,
    plan: MountPlan<'_>,
    scratch_run_dir: &Path,
) -> Result<PreparedCommand, WorkspaceApiError> {
    let tool_call = tool_call(&inputs);
    let run_request = RunRequest {
        mode: RunMode::FreshNs,
        tool_call,
        workspace_root: WorkspaceRoot(plan.workspace_root.to_path_buf()),
        layer_paths: plan.layer_paths.to_vec(),
        upperdir: Some(plan.upperdir.to_path_buf()),
        workdir: Some(plan.workdir.to_path_buf()),
        ns_fds: None,
        cgroup_path: None,
        timeout_seconds: inputs.timeout_seconds,
    };
    finish_prepare(
        inputs,
        run_request,
        scratch_run_dir.join("command-runner-request.json"),
        scratch_run_dir.join("command-runner-result.json"),
    )
}

pub(crate) fn prepare_isolated(
    inputs: PrepareInputs<'_>,
    binding: &CommandBinding,
) -> Result<PreparedCommand, WorkspaceApiError> {
    let ns_fds = ns_fds_from_map(&binding.ns_fds);
    let tool_call = tool_call(&inputs);
    let run_request = RunRequest {
        mode: if ns_fds.is_some() {
            RunMode::SetNs
        } else {
            RunMode::FreshNs
        },
        tool_call,
        workspace_root: WorkspaceRoot(binding.workspace_root.clone()),
        layer_paths: binding.layer_paths.clone(),
        upperdir: Some(binding.upperdir.clone()),
        workdir: Some(binding.workdir.clone()),
        ns_fds,
        cgroup_path: binding.cgroup_path.clone(),
        timeout_seconds: inputs.timeout_seconds,
    };
    let request_path = inputs.session_dir.join("runner-request.json");
    let output_path = inputs.session_dir.join("runner-result.json");
    finish_prepare(inputs, run_request, request_path, output_path)
}

fn tool_call(inputs: &PrepareInputs<'_>) -> ToolCall {
    ToolCall {
        invocation_id: inputs.invocation_id.to_owned(),
        caller_id: inputs.caller_id.to_owned(),
        verb: RunnerVerb::ExecCommand,
        intent: Intent::WriteAllowed,
        args: json!({ "command": inputs.cmd, "cwd": "." }),
        background: false,
    }
}

fn finish_prepare(
    inputs: PrepareInputs<'_>,
    run_request: RunRequest,
    request_path: PathBuf,
    output_path: PathBuf,
) -> Result<PreparedCommand, WorkspaceApiError> {
    std::fs::create_dir_all(&inputs.session_dir).map_err(prepare_error)?;
    std::fs::write(
        inputs.session_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "command_session_id": inputs.command_id,
            "caller_id": inputs.caller_id,
            "invocation_id": inputs.invocation_id,
            "workspace": inputs.workspace_label,
            "command": inputs.cmd,
            "status": "running",
        }))
        .map_err(prepare_error)?,
    )
    .map_err(prepare_error)?;
    Ok(PreparedCommand {
        run_request: serde_json::to_value(&run_request).map_err(prepare_error)?,
        request_path,
        output_path,
        final_path: inputs.session_dir.join("final.json"),
        transcript_path: inputs.session_dir.join("transcript.log"),
    })
}

fn ns_fds_from_map(map: &std::collections::HashMap<String, i32>) -> Option<NsFds> {
    if map.is_empty() {
        return None;
    }
    let fd = |name: &str| map.get(name).copied().map(eos_namespace::protocol::Fd);
    Some(NsFds {
        user: fd("user"),
        mnt: fd("mnt"),
        pid: fd("pid"),
        net: fd("net"),
    })
}

fn prepare_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("command_prepare_failed", error.to_string())
}
