use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ObservabilityPathError {
    #[error("daemon socket path has no daemon runtime directory: {socket_path}")]
    MissingDaemonRuntimeDir { socket_path: PathBuf },
}

/// The one append-only log per sandbox, plus its single rotated sibling. Both
/// live under `<daemon-runtime-dir>/observability`; the `sandbox` id is encoded
/// by the path, so no record carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservabilityPaths {
    daemon_runtime_dir: PathBuf,
    observability_dir: PathBuf,
    log_path: PathBuf,
    rotated_log_path: PathBuf,
    resource_log_path: PathBuf,
    rotated_resource_log_path: PathBuf,
}

impl ObservabilityPaths {
    pub fn from_socket_path(socket_path: impl AsRef<Path>) -> Result<Self, ObservabilityPathError> {
        let socket_path = socket_path.as_ref();
        let daemon_runtime_dir = socket_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| ObservabilityPathError::MissingDaemonRuntimeDir {
                socket_path: socket_path.to_path_buf(),
            })?
            .to_path_buf();
        let observability_dir = daemon_runtime_dir.join("observability");
        let log_path = observability_dir.join("observability.ndjson");
        let rotated_log_path = observability_dir.join("observability.ndjson.1");
        let resource_log_path = observability_dir.join("resources.ndjson");
        let rotated_resource_log_path = observability_dir.join("resources.ndjson.1");

        Ok(Self {
            daemon_runtime_dir,
            observability_dir,
            log_path,
            rotated_log_path,
            resource_log_path,
            rotated_resource_log_path,
        })
    }

    pub fn daemon_runtime_dir(&self) -> &Path {
        &self.daemon_runtime_dir
    }

    pub fn observability_dir(&self) -> &Path {
        &self.observability_dir
    }

    /// The primary append-only log: `observability/observability.ndjson`.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// The single rotated log: `observability/observability.ndjson.1`.
    pub fn rotated_log_path(&self) -> &Path {
        &self.rotated_log_path
    }

    pub fn resource_log_path(&self) -> &Path {
        &self.resource_log_path
    }

    pub fn rotated_resource_log_path(&self) -> &Path {
        &self.rotated_resource_log_path
    }
}
