use std::io;
use std::path::{Path, PathBuf};

use sandbox_runtime_workspace::{
    build_cgroup_monitor_sample, command_cgroup_path, enable_cgroup_controllers_for_children,
    CgroupCleanupState, CgroupMonitorConfig, CgroupMonitorSample, CgroupSampleKind,
    CgroupSampleRequest,
};

use crate::CommandError;

#[derive(Debug, Clone)]
pub struct CommandCgroupTarget {
    pub cgroup_path: PathBuf,
    pub upperdir: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct CommandCgroup {
    target: CommandCgroupTarget,
}

impl CommandCgroup {
    pub(crate) fn prepare(
        command_session_id: &str,
        session_cgroup_path: Option<&Path>,
        upperdir: &Path,
    ) -> Result<Option<Self>, CommandError> {
        let Some(session_cgroup_path) = session_cgroup_path else {
            return Ok(None);
        };
        if session_cgroup_path.as_os_str().is_empty() {
            return Ok(None);
        }
        let cgroup_path = command_cgroup_path(session_cgroup_path, command_session_id);
        create_child_cgroup_if_parent_exists(session_cgroup_path, &cgroup_path)?;
        Ok(Some(Self {
            target: CommandCgroupTarget {
                cgroup_path,
                upperdir: upperdir.to_path_buf(),
            },
        }))
    }

    pub(crate) fn target(&self) -> CommandCgroupTarget {
        self.target.clone()
    }

    pub(crate) fn final_sample(&self, config: &CgroupMonitorConfig) -> Option<CgroupMonitorSample> {
        config.enabled.then(|| {
            build_cgroup_monitor_sample(CgroupSampleRequest {
                cgroup_path: &self.target.cgroup_path,
                upperdir: Some(&self.target.upperdir),
                sample_kind: CgroupSampleKind::CommandFinal,
                interval_ms: config.sample_interval_ms,
                previous: None,
                config,
            })
        })
    }

    pub(crate) fn cleanup(&self) -> CgroupCleanupState {
        let last_cleanup_error = match remove_command_cgroup(&self.target.cgroup_path) {
            Ok(()) => None,
            Err(error) => Some(error.to_string()),
        };
        CgroupCleanupState {
            final_sample_recorded: false,
            cgroup_exists_after_destroy: Some(self.target.cgroup_path.exists()),
            last_cleanup_error,
        }
    }
}

fn create_child_cgroup_if_parent_exists(
    session_cgroup_path: &Path,
    command_cgroup_path: &Path,
) -> Result<(), CommandError> {
    if !session_cgroup_path.exists() {
        return Err(CommandError::InvalidRequest(format!(
            "session cgroup path does not exist: {}",
            session_cgroup_path.display()
        )));
    }
    enable_cgroup_controllers_for_children(session_cgroup_path).map_err(|error| {
        CommandError::artifact_write("command_cgroup", session_cgroup_path, error)
    })?;
    if let Some(command_parent) = command_cgroup_path.parent() {
        std::fs::create_dir_all(command_parent).map_err(|error| {
            CommandError::artifact_write("command_cgroup", command_parent, error)
        })?;
        enable_cgroup_controllers_for_children(command_parent).map_err(|error| {
            CommandError::artifact_write("command_cgroup", command_parent, error)
        })?;
    }
    std::fs::create_dir_all(command_cgroup_path)
        .map_err(|error| CommandError::artifact_write("command_cgroup", command_cgroup_path, error))
}

fn remove_command_cgroup(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || !path.exists() {
        return Ok(());
    }
    std::fs::remove_dir(path)
}
