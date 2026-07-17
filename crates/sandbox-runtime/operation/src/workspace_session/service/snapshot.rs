use crate::observability::RuntimeWorkspaceSnapshot;
use crate::workspace_session::WorkspaceSessionService;

impl WorkspaceSessionService {
    pub(crate) fn snapshot_workspaces(&self) -> (Vec<RuntimeWorkspaceSnapshot>, Vec<String>) {
        let sessions = match self.lock_sessions() {
            Ok(sessions) => sessions,
            Err(error) => return (Vec::new(), vec![error.to_string()]),
        };

        let mut errors = Vec::new();
        let mut snapshots = sessions
            .values()
            .map(|session| {
                let (upperdir, workdir, namespace_fd_count) = match session.handle.entry() {
                    Ok(entry) => (
                        Some(entry.upperdir),
                        Some(entry.workdir),
                        Some(3 + usize::from(entry.ns_fds.net.is_some())),
                    ),
                    Err(_) => {
                        errors.push(format!(
                            "workspace {} lacks launch material",
                            session.workspace_session_id.0
                        ));
                        (None, None, None)
                    }
                };

                RuntimeWorkspaceSnapshot {
                    workspace_id: session.workspace_session_id.clone(),
                    holder_pid: session.handle.holder_pid,
                    network: session.handle.network,
                    finalize_policy: session.finalize_policy,
                    workspace_root: session.handle.workspace_root.clone(),
                    upperdir,
                    workdir,
                    namespace_fd_count,
                    base_root_hash: Some(session.handle.snapshot.root_hash.clone()),
                    layer_count: Some(session.handle.snapshot.layer_paths.len()),
                    layer_ids: session
                        .handle
                        .snapshot
                        .manifest
                        .layers
                        .iter()
                        .rev()
                        .map(|layer| layer.layer_id.clone())
                        .collect(),
                    cgroup_path: session.cgroup_path.clone(),
                }
            })
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| left.workspace_id.0.cmp(&right.workspace_id.0));
        (snapshots, errors)
    }
}
