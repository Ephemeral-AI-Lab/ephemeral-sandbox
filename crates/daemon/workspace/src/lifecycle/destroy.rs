use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use serde_json::{json, Value};

use crate::model::WorkspaceProfile;
use crate::namespace::HolderKillReport;
use crate::overlay::tree::TreeResourceStats;
use crate::profile::manager::IsolatedNetworkError;
use crate::profile::{WorkspaceModeHandle, WorkspaceModeId, WorkspaceModeManager};

use super::{monotonic_seconds, record_phase_ms};

#[derive(Debug, Clone, PartialEq)]
pub struct ExitOutcome {
    pub workspace_id: WorkspaceModeId,
    pub lease_id: String,
    pub evicted_upperdir_bytes: u64,
    pub lifetime_s: f64,
    pub total_ms: f64,
    pub phases_ms: HashMap<String, f64>,
    pub inspection: Value,
}

impl WorkspaceModeManager {
    pub(crate) fn teardown_handle(
        &mut self,
        handle: &WorkspaceModeHandle,
        grace_s: f64,
    ) -> (Value, HashMap<String, f64>) {
        let mut phases_ms = HashMap::new();
        let phase_start = Instant::now();
        let (holder_kill_report, holder_kill_error) = if handle.holder_pid > 0 {
            match self.runtime.kill_holder(handle.holder_pid, grace_s) {
                Ok(report) => (report, None),
                Err(err) => (HolderKillReport::default(), Some(err.to_string())),
            }
        } else {
            (HolderKillReport::default(), None)
        };
        record_phase_ms(&mut phases_ms, "kill_holder", phase_start);
        close_handle_fds(handle);
        self.teardown_isolated_network(handle, &mut phases_ms);
        let phase_start = Instant::now();
        if let Some(cgroup_path) = handle.cgroup_path.as_ref() {
            let _ = std::fs::remove_dir(cgroup_path);
        }
        record_phase_ms(&mut phases_ms, "cgroup_rmdir", phase_start);
        let phase_start = Instant::now();
        let _ = std::fs::remove_dir_all(&handle.dirs.run_dir);
        record_phase_ms(&mut phases_ms, "rmtree_scratch", phase_start);

        let cgroup_exists_after = handle.cgroup_path.as_ref().map(|path| path.exists());
        let inspection = json!({
            "handle_registered_after": self.handles.contains_key(&handle.workspace_id),
            "open_handle_count_after": self.handles.len(),
            "holder_pid": handle.holder_pid,
            "holder_was_alive": holder_kill_report.holder_was_alive,
            "holder_exit_status": holder_kill_report.exit_status,
            "holder_signal": holder_kill_report.signal,
            "holder_status_raw": holder_kill_report.status_raw,
            "holder_kill_error": holder_kill_error,
            "ns_fd_count": handle.ns_fds.len(),
            "readiness_fd_was_open": handle.readiness_fd >= 0,
            "control_fd_was_open": handle.control_fd >= 0,
            "veth_host_name": handle.veth.as_ref().map(|veth| veth.host_name.as_str()),
            "veth_ns_name": handle.veth.as_ref().map(|veth| veth.ns_name.as_str()),
            "cgroup_path": handle
                .cgroup_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "cgroup_exists_after": cgroup_exists_after,
            "scratch_dir": handle.dirs.run_dir.to_string_lossy(),
            "scratch_exists_after": handle.dirs.run_dir.exists(),
            "upperdir_exists_after": handle.dirs.upperdir.exists(),
            "workdir_exists_after": handle.dirs.workdir.exists(),
            "mountinfo_reference_count_after": mountinfo_reference_count(&[
                &handle.dirs.run_dir,
                &handle.dirs.upperdir,
                &handle.dirs.workdir,
            ]),
        });
        (inspection, phases_ms)
    }

    fn teardown_isolated_network(
        &mut self,
        handle: &WorkspaceModeHandle,
        phases_ms: &mut HashMap<String, f64>,
    ) {
        if handle.profile != WorkspaceProfile::Isolated {
            return;
        }
        let phase_start = Instant::now();
        if let Some(veth) = handle.veth.as_ref() {
            if self.runtime.bypasses_kernel_setup() {
                self.network.release_stub_veth(veth);
            } else {
                self.network.teardown_veth(veth);
            }
        }
        record_phase_ms(phases_ms, "teardown_veth", phase_start);
    }

    pub fn exit(
        &mut self,
        workspace_id: &WorkspaceModeId,
        grace_s: Option<f64>,
    ) -> Result<ExitOutcome, IsolatedNetworkError> {
        let Some(handle) = self.handles.remove(workspace_id) else {
            return Err(IsolatedNetworkError::NotOpen);
        };
        let timer = Instant::now();
        let upperdir_bytes = TreeResourceStats::collect(&handle.dirs.upperdir).bytes;
        let (mut inspection, mut phases_ms) =
            self.teardown_handle(&handle, grace_s.unwrap_or(self.caps.exit_grace_s));
        let phase_start = Instant::now();
        let persistence_error = self.persist_handles().err().map(|err| err.to_string());
        record_phase_ms(&mut phases_ms, "persist_handles", phase_start);
        if let (Some(error), Some(object)) = (persistence_error, inspection.as_object_mut()) {
            object.insert("persistence_error".to_owned(), json!(error));
        }
        let lifetime_s = (monotonic_seconds() - handle.created_at).max(0.0);
        Ok(ExitOutcome {
            workspace_id: handle.workspace_id,
            lease_id: handle.lease_id,
            evicted_upperdir_bytes: upperdir_bytes,
            lifetime_s,
            total_ms: timer.elapsed().as_secs_f64() * 1000.0,
            phases_ms,
            inspection,
        })
    }
}

fn close_handle_fds(handle: &WorkspaceModeHandle) {
    for fd in handle.ns_fds.values() {
        close_fd(fd);
    }
    close_fd(handle.readiness_fd);
    close_fd(handle.control_fd);
}

fn close_fd(fd: i32) {
    if fd >= 0 {
        let _ = nix::unistd::close(fd);
    }
}

fn mountinfo_reference_count(paths: &[&Path]) -> Option<usize> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let needles = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    Some(
        mountinfo
            .lines()
            .filter(|line| needles.iter().any(|needle| line.contains(needle)))
            .count(),
    )
}
