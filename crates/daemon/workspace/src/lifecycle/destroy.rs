use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use serde_json::{json, Value};

use crate::isolated_workspace::error::IsolatedError;
use crate::isolated_workspace::manager::{IsolatedManager, IsolatedWorkspaceId, WorkspaceHandle};
use crate::namespace::HolderKillReport;
use crate::overlay::tree::directory_file_bytes;

use super::{close_handle_fds, monotonic_seconds, record_phase_ms};

#[derive(Debug, Clone, PartialEq)]
pub struct ExitOutcome {
    pub workspace_id: IsolatedWorkspaceId,
    pub caller_id: String,
    pub lease_id: String,
    pub evicted_upperdir_bytes: u64,
    pub lifetime_s: f64,
    pub total_ms: f64,
    pub phases_ms: HashMap<String, f64>,
    pub inspection: Value,
}

impl IsolatedManager {
    pub(crate) fn teardown_handle(
        &mut self,
        handle: &WorkspaceHandle,
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
        let phase_start = Instant::now();
        if let Some(veth) = handle.veth.as_ref() {
            self.network.teardown_veth(veth);
        }
        record_phase_ms(&mut phases_ms, "teardown_veth", phase_start);
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
            "agent_registered_after": self.by_caller.contains_key(&handle.caller_id),
            "open_handle_count_after": self.handles.len(),
            "open_agent_count_after": self.by_caller.len(),
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

    pub fn exit(
        &mut self,
        caller_id: &str,
        grace_s: Option<f64>,
    ) -> Result<ExitOutcome, IsolatedError> {
        if caller_id.trim().is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "caller_id is required".to_owned(),
            ));
        }
        let Some(workspace_id) = self.by_caller.remove(caller_id) else {
            return Err(IsolatedError::NotOpen);
        };
        let Some(handle) = self.handles.remove(&workspace_id) else {
            return Err(IsolatedError::NotOpen);
        };
        let timer = Instant::now();
        let upperdir_bytes = directory_file_bytes(&handle.dirs.upperdir);
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
            caller_id: handle.caller_id,
            lease_id: handle.lease_id,
            evicted_upperdir_bytes: upperdir_bytes,
            lifetime_s,
            total_ms: timer.elapsed().as_secs_f64() * 1000.0,
            phases_ms,
            inspection,
        })
    }

    pub fn evict_idle_workspaces(&mut self, active_callers: &HashSet<String>) -> Vec<ExitOutcome> {
        if self.caps.ttl_s <= 0.0 {
            return Vec::new();
        }
        let now = monotonic_seconds();
        let stale = self
            .handles
            .values()
            .filter(|handle| now - handle.last_activity > self.caps.ttl_s)
            .filter(|handle| !active_callers.contains(&handle.caller_id))
            .map(|handle| handle.caller_id.clone())
            .collect::<Vec<_>>();
        stale
            .into_iter()
            .filter_map(|caller_id| self.exit(&caller_id, None).ok())
            .collect()
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
