//! Daemon namespace-runner child launcher.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use eos_namespace::protocol::{RunRequest, RunResult};

use crate::error::DaemonError;
use crate::invocation_registry::InFlightRegistry;

pub(crate) fn run_ns_runner_child(
    request: &RunRequest,
    invocation_registry: Option<&InFlightRegistry>,
) -> Result<RunResult, DaemonError> {
    let payload =
        serde_json::to_vec(request).map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("ns-runner")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = command.spawn()?;
    if let Some(registry) = invocation_registry {
        if let Ok(pgid) = i32::try_from(child.id()) {
            registry.register_process_group(&request.tool_call.invocation_id, pgid);
        }
    }
    child
        .stdin
        .as_mut()
        .ok_or_else(|| DaemonError::OverlayPipeline("ns-runner stdin unavailable".to_owned()))?
        .write_all(&payload)?;
    let output = child.wait_with_output()?;
    if let Some(registry) = invocation_registry {
        registry.clear_process_group(&request.tool_call.invocation_id);
    }
    if !output.status.success() {
        return Err(DaemonError::OverlayPipeline(format!(
            "ns-runner exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    serde_json::from_slice::<RunResult>(&output.stdout)
        .map_err(|err| DaemonError::OverlayPipeline(format!("invalid ns-runner output: {err}")))
}
