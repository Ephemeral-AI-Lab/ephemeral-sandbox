use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::BenchmarkPaths;

pub const OWNERSHIP_MARKER: &str = ".eos-benchmark-owned";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnedIdentity {
    RunTrial { run_id: String, trial_id: String },
    Runtime { runner_instance_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkerFile {
    schema_version: u32,
    identity: OwnedIdentity,
}

#[derive(Debug, Default)]
pub struct CleanupLedger {
    entries: BTreeMap<PathBuf, OwnedIdentity>,
}

#[derive(Debug, Error)]
pub enum CleanupError {
    #[error("cleanup target does not exist: {0}")]
    Missing(PathBuf),
    #[error("cleanup target is a symlink or crosses one: {0}")]
    Symlink(PathBuf),
    #[error("cleanup target is outside its allowed owned root: {0}")]
    OutsideRoot(PathBuf),
    #[error("cleanup target is a protected root: {0}")]
    ProtectedRoot(PathBuf),
    #[error("cleanup target crosses a filesystem device boundary: {0}")]
    DeviceBoundary(PathBuf),
    #[error("cleanup marker is missing or invalid at {0}")]
    InvalidMarker(PathBuf),
    #[error("cleanup target is absent from the active ownership ledger: {0}")]
    NotInLedger(PathBuf),
    #[error("cleanup identity does not match the marker or ledger at {0}")]
    IdentityMismatch(PathBuf),
    #[error("filesystem operation failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cleanup marker serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl CleanupLedger {
    pub fn register(
        &mut self,
        paths: &BenchmarkPaths,
        target: &Path,
        identity: OwnedIdentity,
    ) -> Result<PathBuf, CleanupError> {
        let canonical = validate_location(paths, target, &identity, false)?;
        write_marker(&canonical, &identity)?;
        self.entries.insert(canonical.clone(), identity);
        Ok(canonical)
    }

    pub fn remove_owned(
        &mut self,
        paths: &BenchmarkPaths,
        target: &Path,
        expected: &OwnedIdentity,
    ) -> Result<(), CleanupError> {
        let canonical = validate_location(paths, target, expected, true)?;
        let marker = read_marker(&canonical)?;
        if marker != *expected {
            return Err(CleanupError::IdentityMismatch(canonical));
        }
        match self.entries.get(&canonical) {
            Some(identity) if identity == expected => {}
            Some(_) => return Err(CleanupError::IdentityMismatch(canonical)),
            None => return Err(CleanupError::NotInLedger(canonical)),
        }
        fs::remove_dir_all(&canonical).map_err(|source| io_error(&canonical, source))?;
        self.entries.remove(&canonical);
        sync_parent(&canonical)?;
        Ok(())
    }

    /// Reconstitutes an in-memory ledger entry after a runner restart. The
    /// existing marker must pass the same containment, symlink, device, and
    /// identity checks as normal deletion; recovery never manufactures a new
    /// marker for an unknown path.
    pub fn adopt_existing(
        &mut self,
        paths: &BenchmarkPaths,
        target: &Path,
        expected: &OwnedIdentity,
    ) -> Result<PathBuf, CleanupError> {
        let canonical = validate_location(paths, target, expected, true)?;
        let marker = read_marker(&canonical)?;
        if marker != *expected {
            return Err(CleanupError::IdentityMismatch(canonical));
        }
        self.entries.insert(canonical.clone(), marker);
        Ok(canonical)
    }

    #[must_use]
    pub fn contains(&self, target: &Path) -> bool {
        target
            .canonicalize()
            .ok()
            .is_some_and(|canonical| self.entries.contains_key(&canonical))
    }
}

/// Reads a marker without changing cleanup authority. Callers must still use
/// `adopt_existing` before deletion so all path invariants are revalidated.
pub fn owned_identity(target: &Path) -> Result<OwnedIdentity, CleanupError> {
    read_marker(target)
}

fn validate_location(
    paths: &BenchmarkPaths,
    target: &Path,
    identity: &OwnedIdentity,
    require_marker: bool,
) -> Result<PathBuf, CleanupError> {
    let metadata = fs::symlink_metadata(target).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            CleanupError::Missing(target.to_path_buf())
        } else {
            io_error(target, source)
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(CleanupError::Symlink(target.to_path_buf()));
    }
    if !metadata.is_dir() {
        return Err(CleanupError::OutsideRoot(target.to_path_buf()));
    }

    let target = target
        .canonicalize()
        .map_err(|source| io_error(target, source))?;
    let (allowed_root, exact_runtime) = match identity {
        OwnedIdentity::RunTrial { .. } => (canonical(&paths.runs)?, None),
        OwnedIdentity::Runtime { runner_instance_id } => {
            let runtime_parent = canonical(&paths.runtime)?;
            let instance = runtime_parent.join(runner_instance_id);
            let instance = canonical(&instance)?;
            (runtime_parent, Some(instance))
        }
    };

    let allowed = match &exact_runtime {
        Some(instance) => target == *instance || is_strict_descendant(&target, instance),
        None => is_strict_descendant(&target, &allowed_root),
    };
    if !allowed {
        return Err(CleanupError::OutsideRoot(target));
    }
    if target == paths.root
        || target == paths.benchmark
        || target == paths.fixtures
        || target == paths.results
        || target == paths.runs
        || target == paths.runtime
    {
        return Err(CleanupError::ProtectedRoot(target));
    }

    reject_symlink_components(&allowed_root, &target)?;
    reject_device_boundary(&allowed_root, &target)?;
    if require_marker {
        let marker = target.join(OWNERSHIP_MARKER);
        let marker_metadata = fs::symlink_metadata(&marker)
            .map_err(|_| CleanupError::InvalidMarker(marker.clone()))?;
        if !marker_metadata.is_file() || marker_metadata.file_type().is_symlink() {
            return Err(CleanupError::InvalidMarker(marker));
        }
    }
    Ok(target)
}

fn write_marker(directory: &Path, identity: &OwnedIdentity) -> Result<(), CleanupError> {
    let path = directory.join(OWNERSHIP_MARKER);
    let marker = MarkerFile {
        schema_version: 1,
        identity: identity.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&marker)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|source| io_error(&path, source))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(&path, source))?;
    sync_parent(&path)
}

fn read_marker(directory: &Path) -> Result<OwnedIdentity, CleanupError> {
    let path = directory.join(OWNERSHIP_MARKER);
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| CleanupError::InvalidMarker(path.clone()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(CleanupError::InvalidMarker(path));
    }
    let bytes = fs::read(&path).map_err(|_| CleanupError::InvalidMarker(path.clone()))?;
    let marker: MarkerFile =
        serde_json::from_slice(&bytes).map_err(|_| CleanupError::InvalidMarker(path.clone()))?;
    if marker.schema_version != 1 {
        return Err(CleanupError::InvalidMarker(path));
    }
    Ok(marker.identity)
}

fn reject_symlink_components(root: &Path, target: &Path) -> Result<(), CleanupError> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| CleanupError::OutsideRoot(target.to_path_buf()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        let metadata =
            fs::symlink_metadata(&current).map_err(|source| io_error(&current, source))?;
        if metadata.file_type().is_symlink() {
            return Err(CleanupError::Symlink(current));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn reject_device_boundary(root: &Path, target: &Path) -> Result<(), CleanupError> {
    use std::os::unix::fs::MetadataExt;

    let root_device = fs::metadata(root)
        .map_err(|source| io_error(root, source))?
        .dev();
    let relative = target
        .strip_prefix(root)
        .map_err(|_| CleanupError::OutsideRoot(target.to_path_buf()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        let device = fs::metadata(&current)
            .map_err(|source| io_error(&current, source))?
            .dev();
        if device != root_device {
            return Err(CleanupError::DeviceBoundary(current));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_device_boundary(_root: &Path, _target: &Path) -> Result<(), CleanupError> {
    Ok(())
}

fn canonical(path: &Path) -> Result<PathBuf, CleanupError> {
    path.canonicalize().map_err(|source| io_error(path, source))
}

fn is_strict_descendant(path: &Path, root: &Path) -> bool {
    path != root && path.starts_with(root)
}

fn sync_parent(path: &Path) -> Result<(), CleanupError> {
    let parent = path
        .parent()
        .ok_or_else(|| CleanupError::OutsideRoot(path.to_path_buf()))?;
    let directory = OpenOptions::new()
        .read(true)
        .open(parent)
        .map_err(|source| io_error(parent, source))?;
    directory
        .sync_all()
        .map_err(|source| io_error(parent, source))
}

fn io_error(path: &Path, source: io::Error) -> CleanupError {
    CleanupError::Io {
        path: path.to_path_buf(),
        source,
    }
}
