//! Validated physical scratch paths owned by one workspace session.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use sandbox_runtime_namespace_execution::NamespaceExecutionId;
use thiserror::Error;

use crate::WorkspaceSessionId;

pub const SCRATCH_LAYOUT_VERSION: u8 = 2;
pub const EXECUTIONS_DIRECTORY: &str = "executions";
pub const TRANSCRIPT_FILE: &str = "transcript.log";
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionScratchRoute {
    WorkspaceScoped,
    LegacyCompat,
}

impl ExecutionScratchRoute {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceScoped => "workspace_scoped",
            Self::LegacyCompat => "legacy_compat",
        }
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceScratchError {
    #[error("workspace scratch root must be an absolute non-root path: {0}")]
    InvalidRoot(PathBuf),
    #[error("workspace scratch path traverses a symbolic link: {0}")]
    Symlink(PathBuf),
    #[error("workspace scratch component is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("invalid {kind} identifier: {value:?}")]
    InvalidId { kind: &'static str, value: String },
    #[error("workspace scratch path escaped its configured root: {0}")]
    EscapedRoot(PathBuf),
    #[error("workspace execution scratch already exists: {0}")]
    ExecutionCollision(PathBuf),
    #[error("workspace scratch I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceScratchLocator {
    root: Arc<PathBuf>,
}

impl WorkspaceScratchLocator {
    /// Validate a workspace scratch root without creating it.
    pub fn new(root: PathBuf) -> Result<Self, WorkspaceScratchError> {
        let root = normalize_configured_root(root)?;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub fn session_root(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<PathBuf, WorkspaceScratchError> {
        validate_id("workspace session", &workspace_session_id.0)?;
        self.contained(self.root.join(&workspace_session_id.0))
    }

    /// Ensure the session and its execution namespace exist with private modes.
    pub fn ensure_session(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<PathBuf, WorkspaceScratchError> {
        ensure_directory(self.root(), true)?;
        let session_root = self.session_root(workspace_session_id)?;
        ensure_directory(&session_root, false)?;
        let executions = self.contained(session_root.join(EXECUTIONS_DIRECTORY))?;
        ensure_directory(&executions, false)?;
        Ok(session_root)
    }

    /// Allocate exactly one execution leaf and its private transcript.
    pub fn allocate_execution(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        execution_id: &NamespaceExecutionId,
    ) -> Result<ExecutionScratchLease, WorkspaceScratchError> {
        validate_namespace_execution_id(&execution_id.0)?;
        let session_root = self.ensure_session(workspace_session_id)?;
        let execution_root = self.contained(
            session_root
                .join(EXECUTIONS_DIRECTORY)
                .join(&execution_id.0),
        )?;
        match fs::create_dir(&execution_root) {
            Ok(()) => {}
            Err(source) if source.kind() == ErrorKind::AlreadyExists => {
                return Err(WorkspaceScratchError::ExecutionCollision(execution_root));
            }
            Err(source) => {
                return Err(WorkspaceScratchError::Io {
                    path: execution_root,
                    source,
                });
            }
        }
        if let Err(error) = set_private_directory_mode(&execution_root) {
            let _ = fs::remove_dir(&execution_root);
            return Err(error);
        }
        let transcript_path = execution_root.join(TRANSCRIPT_FILE);
        if let Err(source) = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .open(&transcript_path)
        {
            let _ = fs::remove_dir_all(&execution_root);
            return Err(WorkspaceScratchError::Io {
                path: transcript_path,
                source,
            });
        }
        Ok(ExecutionScratchLease {
            root: execution_root,
            transcript_path,
            route: ExecutionScratchRoute::WorkspaceScoped,
            released: false,
        })
    }

    fn contained(&self, path: PathBuf) -> Result<PathBuf, WorkspaceScratchError> {
        if path.starts_with(self.root()) {
            Ok(path)
        } else {
            Err(WorkspaceScratchError::EscapedRoot(path))
        }
    }
}

/// Same-revision compatibility locator used only by the internal Stage 01
/// benchmark adapter. Normal command admission never selects this route.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyExecutionScratchLocator {
    root: Arc<PathBuf>,
}

impl LegacyExecutionScratchLocator {
    pub fn new(root: PathBuf) -> Result<Self, WorkspaceScratchError> {
        let root = normalize_configured_root(root)?;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub fn allocate_execution(
        &self,
        execution_id: &NamespaceExecutionId,
    ) -> Result<ExecutionScratchLease, WorkspaceScratchError> {
        validate_namespace_execution_id(&execution_id.0)?;
        ensure_directory(self.root(), true)?;
        let execution_root = self.root.join(&execution_id.0);
        if !execution_root.starts_with(self.root()) {
            return Err(WorkspaceScratchError::EscapedRoot(execution_root));
        }
        match fs::create_dir(&execution_root) {
            Ok(()) => {}
            Err(source) if source.kind() == ErrorKind::AlreadyExists => {
                return Err(WorkspaceScratchError::ExecutionCollision(execution_root));
            }
            Err(source) => {
                return Err(WorkspaceScratchError::Io {
                    path: execution_root,
                    source,
                });
            }
        }
        if let Err(error) = set_private_directory_mode(&execution_root) {
            let _ = fs::remove_dir(&execution_root);
            return Err(error);
        }
        let transcript_path = execution_root.join(TRANSCRIPT_FILE);
        if let Err(source) = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .open(&transcript_path)
        {
            let _ = fs::remove_dir_all(&execution_root);
            return Err(WorkspaceScratchError::Io {
                path: transcript_path,
                source,
            });
        }
        Ok(ExecutionScratchLease {
            root: execution_root,
            transcript_path,
            route: ExecutionScratchRoute::LegacyCompat,
            released: false,
        })
    }
}

/// Ownership token for one execution leaf. Dropping it removes only that leaf.
#[derive(Debug)]
pub struct ExecutionScratchLease {
    root: PathBuf,
    transcript_path: PathBuf,
    route: ExecutionScratchRoute,
    released: bool,
}

impl ExecutionScratchLease {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn transcript_path(&self) -> &Path {
        &self.transcript_path
    }

    #[must_use]
    pub const fn route(&self) -> ExecutionScratchRoute {
        self.route
    }

    /// Explicitly release this execution leaf. The consumed lease cannot be
    /// reused, and the cleanup remains idempotent if unwinding reaches `Drop`.
    pub fn release(mut self) -> Result<(), WorkspaceScratchError> {
        self.remove_leaf()
    }

    /// Release the leaf while retaining the ownership token if cleanup fails,
    /// so a teardown retry can make progress instead of orphaning the path.
    pub fn release_in_place(&mut self) -> Result<(), WorkspaceScratchError> {
        self.remove_leaf()
    }

    fn remove_leaf(&mut self) -> Result<(), WorkspaceScratchError> {
        if self.released {
            return Ok(());
        }
        match fs::remove_dir_all(&self.root) {
            Ok(()) => {
                self.released = true;
                Ok(())
            }
            Err(source) if source.kind() == ErrorKind::NotFound => {
                self.released = true;
                Ok(())
            }
            Err(source) => Err(WorkspaceScratchError::Io {
                path: self.root.clone(),
                source,
            }),
        }
    }
}

impl Drop for ExecutionScratchLease {
    fn drop(&mut self) {
        let _ = self.remove_leaf();
    }
}

fn validate_root(root: &Path) -> Result<(), WorkspaceScratchError> {
    if !root.is_absolute() || root.parent().is_none() || root == Path::new("/") {
        return Err(WorkspaceScratchError::InvalidRoot(root.to_path_buf()));
    }
    if root.components().any(|component| {
        !matches!(
            component,
            Component::RootDir | Component::Normal(_) | Component::Prefix(_)
        )
    }) {
        return Err(WorkspaceScratchError::InvalidRoot(root.to_path_buf()));
    }
    Ok(())
}

fn normalize_configured_root(root: PathBuf) -> Result<PathBuf, WorkspaceScratchError> {
    validate_root(&root)?;
    if let Ok(metadata) = fs::symlink_metadata(&root) {
        if metadata.file_type().is_symlink() {
            return Err(WorkspaceScratchError::Symlink(root));
        }
        if !metadata.is_dir() {
            return Err(WorkspaceScratchError::NotDirectory(root));
        }
    }

    let mut existing = root.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .ok_or_else(|| WorkspaceScratchError::InvalidRoot(root.clone()))?;
                missing.push(name.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| WorkspaceScratchError::InvalidRoot(root.clone()))?;
            }
            Err(source) => {
                return Err(WorkspaceScratchError::Io {
                    path: existing.to_path_buf(),
                    source,
                });
            }
        }
    }
    let mut normalized =
        fs::canonicalize(existing).map_err(|source| WorkspaceScratchError::Io {
            path: existing.to_path_buf(),
            source,
        })?;
    for component in missing.iter().rev() {
        normalized.push(component);
    }
    validate_root(&normalized)?;
    reject_existing_symlink_components(&normalized)?;
    Ok(normalized)
}

fn validate_id(kind: &'static str, value: &str) -> Result<(), WorkspaceScratchError> {
    let valid = !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(WorkspaceScratchError::InvalidId {
            kind,
            value: value.to_owned(),
        })
    }
}

fn validate_namespace_execution_id(value: &str) -> Result<(), WorkspaceScratchError> {
    let canonical = value
        .strip_prefix("namespace_execution_")
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
                && (suffix == "0" || !suffix.starts_with('0'))
        });
    if canonical {
        Ok(())
    } else {
        Err(WorkspaceScratchError::InvalidId {
            kind: "namespace execution",
            value: value.to_owned(),
        })
    }
}

fn reject_existing_symlink_components(path: &Path) -> Result<(), WorkspaceScratchError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(WorkspaceScratchError::Symlink(current));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(WorkspaceScratchError::NotDirectory(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(WorkspaceScratchError::Io {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn ensure_directory(path: &Path, recursive: bool) -> Result<(), WorkspaceScratchError> {
    reject_existing_symlink_components(path)?;
    let result = if recursive {
        fs::create_dir_all(path)
    } else {
        fs::create_dir(path)
    };
    match result {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(WorkspaceScratchError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    reject_existing_symlink_components(path)?;
    set_private_directory_mode(path)
}

fn set_private_directory_mode(path: &Path) -> Result<(), WorkspaceScratchError> {
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).map_err(
        |source| WorkspaceScratchError::Io {
            path: path.to_path_buf(),
            source,
        },
    )
}
