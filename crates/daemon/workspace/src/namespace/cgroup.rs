use std::path::PathBuf;

use crate::isolated_workspace::error::IsolatedError;
use crate::isolated_workspace::manager::WorkspaceHandle;

use super::{setup_error, NamespaceRuntime};

impl NamespaceRuntime {
    pub(crate) fn create_cgroup(&self, handle: &WorkspaceHandle) -> Result<PathBuf, IsolatedError> {
        if self.stub {
            return Ok(PathBuf::new());
        }
        let path = PathBuf::from(crate::isolated_workspace::caps::CGROUP_ROOT).join(format!(
            "{}{}",
            crate::isolated_workspace::caps::HANDLE_PREFIX,
            handle.workspace_id.0
        ));
        std::fs::create_dir_all(&path).map_err(setup_error)?;
        Ok(path)
    }
}
