use std::io;
use std::path::{Path, PathBuf};

use crate::profile::WorkspaceModeError;
use crate::profile::WorkspaceModeHandle;

#[cfg(target_os = "linux")]
use super::cgroup_monitor::session_cgroup_path;
#[cfg(not(target_os = "linux"))]
use super::NamespaceRuntime;
#[cfg(target_os = "linux")]
use super::{setup_error, NamespaceRuntime};

impl NamespaceRuntime {
    pub(crate) fn create_cgroup(
        &self,
        handle: &WorkspaceModeHandle,
    ) -> Result<PathBuf, WorkspaceModeError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = handle;
            Ok(PathBuf::new())
        }
        #[cfg(target_os = "linux")]
        {
            let root = PathBuf::from(crate::profile::CGROUP_ROOT);
            let eos_root = root.join("eos");
            let sessions_root = eos_root.join("sessions");
            std::fs::create_dir_all(&eos_root)
                .map_err(|error| setup_error(format!("create eos cgroup root: {error}")))?;
            enable_for_child_cgroups(&root).map_err(setup_error)?;
            enable_for_child_cgroups(&eos_root).map_err(setup_error)?;
            std::fs::create_dir_all(&sessions_root)
                .map_err(|error| setup_error(format!("create sessions cgroup root: {error}")))?;
            enable_for_child_cgroups(&sessions_root).map_err(setup_error)?;

            let path = session_cgroup_path(
                &root,
                &crate::model::WorkspaceSessionId(handle.workspace_id.0.clone()),
            );
            std::fs::create_dir_all(&path)
                .map_err(|error| setup_error(format!("create session cgroup: {error}")))?;
            enable_for_child_cgroups(&path).map_err(setup_error)?;
            std::fs::create_dir_all(holder_cgroup_path(&path))
                .map_err(|error| setup_error(format!("create holder cgroup: {error}")))?;
            Ok(path)
        }
    }

    pub(crate) fn join_holder_cgroup(
        &self,
        handle: &WorkspaceModeHandle,
    ) -> Result<(), WorkspaceModeError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = handle;
            Ok(())
        }
        #[cfg(target_os = "linux")]
        {
            let Some(cgroup_path) = handle.cgroup_path.as_ref() else {
                return Ok(());
            };
            if handle.holder_pid <= 0 {
                return Ok(());
            }
            let holder_path = holder_cgroup_path(cgroup_path);
            std::fs::create_dir_all(&holder_path)
                .map_err(|error| setup_error(format!("create holder cgroup: {error}")))?;
            let procs = holder_path.join("cgroup.procs");
            std::fs::write(&procs, format!("{}\n", handle.holder_pid))
                .map_err(|error| setup_error(format!("join holder cgroup: {error}")))
        }
    }
}

#[cfg(target_os = "linux")]
const CGROUP_CONTROLLERS_FOR_CHILDREN: &[&str] = &["cpu", "io", "memory", "pids"];

#[cfg(target_os = "linux")]
fn enable_for_child_cgroups(parent: &Path) -> Result<(), String> {
    enable_cgroup_controllers_for_children(parent)
        .map_err(|error| format!("enable cgroup controllers: {error}"))
}

#[cfg(target_os = "linux")]
fn holder_cgroup_path(session_cgroup_path: &Path) -> PathBuf {
    session_cgroup_path.join("holder")
}

pub fn enable_cgroup_controllers_for_children(parent: &Path) -> io::Result<()> {
    enable_cgroup_controllers_for_children_impl(parent)
}

#[cfg(target_os = "linux")]
fn enable_cgroup_controllers_for_children_impl(parent: &Path) -> io::Result<()> {
    let controllers_path = parent.join("cgroup.controllers");
    let subtree_control_path = parent.join("cgroup.subtree_control");
    let available = match std::fs::read_to_string(&controllers_path) {
        Ok(available) => available,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let enabled = match std::fs::read_to_string(&subtree_control_path) {
        Ok(enabled) => enabled,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    let available = available.split_whitespace().collect::<Vec<_>>();
    let enabled = enabled
        .split_whitespace()
        .map(|controller| controller.strip_prefix('+').unwrap_or(controller))
        .collect::<Vec<_>>();
    for controller in CGROUP_CONTROLLERS_FOR_CHILDREN {
        if available.contains(controller) && !enabled.contains(controller) {
            std::fs::write(&subtree_control_path, format!("+{controller}"))?;
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn enable_cgroup_controllers_for_children_impl(_parent: &Path) -> io::Result<()> {
    Ok(())
}
