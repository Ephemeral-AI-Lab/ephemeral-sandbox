use std::collections::HashMap;
use std::time::Instant;

use crate::namespace::NamespaceRuntime;
use crate::profile::common::record_phase_ms;
use crate::profile::manager::IsolatedNetworkError;
use crate::profile::WorkspaceModeHandle;

pub(crate) fn create_cgroup(
    runtime: &NamespaceRuntime,
    handle: &mut WorkspaceModeHandle,
    phases_ms: &mut HashMap<String, f64>,
) -> Result<(), IsolatedNetworkError> {
    let phase_start = Instant::now();
    let cgroup_path = runtime.create_cgroup(handle)?;
    record_phase_ms(phases_ms, "create_cgroup", phase_start);
    if !cgroup_path.as_os_str().is_empty() {
        handle.cgroup_path = Some(cgroup_path);
    }
    let phase_start = Instant::now();
    runtime.join_holder_cgroup(handle)?;
    record_phase_ms(phases_ms, "join_holder_cgroup", phase_start);
    Ok(())
}

pub(crate) fn remove_cgroup(
    handle: &WorkspaceModeHandle,
    phases_ms: &mut HashMap<String, f64>,
) -> Option<bool> {
    let phase_start = Instant::now();
    if let Some(cgroup_path) = handle.cgroup_path.as_ref() {
        let _ = std::fs::remove_dir(cgroup_path);
    }
    record_phase_ms(phases_ms, "cgroup_rmdir", phase_start);
    handle.cgroup_path.as_ref().map(|path| path.exists())
}
