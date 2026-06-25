use std::path::PathBuf;

use sandbox_runtime_overlay::OverlayHandle;

use crate::runner::protocol::NamespaceRunnerRequest;
use crate::runner::RunnerError;
use crate::timing;

/// Mount the overlay inside an existing workspace mount namespace.
pub(crate) fn setns_overlay_mount(
    request: &NamespaceRunnerRequest,
    hidden_paths: &[PathBuf],
) -> Result<(), RunnerError> {
    let total_started = std::time::Instant::now();
    let setns_started = std::time::Instant::now();
    super::namespaces::setns_user_mnt(request, "setns overlay mount")?;
    timing::duration("ns_runner.overlay.setns_user_mnt", setns_started);
    let upperdir = request.upperdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires upperdir".to_owned())
    })?;
    let workdir = request.workdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires workdir".to_owned())
    })?;
    let handle = OverlayHandle {
        layer_paths: if request.layer_paths.is_empty() {
            return Err(RunnerError::InvalidRequest(
                "setns overlay mount requires layer_paths".to_owned(),
            ));
        } else {
            request.layer_paths.clone()
        },
        upperdir: upperdir.clone(),
        workdir: workdir.clone(),
    };
    let mount_started = std::time::Instant::now();
    let guard = sandbox_runtime_overlay::mount_overlay(&request.workspace_root, &handle)?;
    timing::duration("ns_runner.overlay.mount_overlay", mount_started);
    let mask_started = std::time::Instant::now();
    crate::runner::mask_model_shell_paths(hidden_paths)?;
    timing::duration("ns_runner.overlay.mask_hidden_paths", mask_started);
    // The setns mount helper is a one-shot process. The mounted overlay must
    // outlive this helper and remain pinned by the target mount namespace until
    // isolated teardown, so the unmount-on-drop guard is deliberately leaked.
    std::mem::forget(guard);
    timing::duration("ns_runner.overlay.total", total_started);
    Ok(())
}
