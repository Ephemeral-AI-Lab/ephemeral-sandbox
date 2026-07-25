//! Workspace-owned capture for overlay upperdirs.
//!
//! Capture derives `Delete` and `OpaqueDir` changes exclusively from kernel
//! overlay metadata — char-device 0:0 whiteouts, `user.overlay.whiteout`
//! xattr files, and `{trusted,user}.overlay.opaque` xattr directories. Dirent
//! names are never interpreted as markers: `.wh.`-prefixed path components
//! are a reserved layerstack-internal namespace, so a user-created `.wh.`
//! name flows through as the ordinary write it is and publish admission
//! rejects it fail-closed as `protected_path`.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use sandbox_runtime_layerstack::{CasError, LayerChange, LayerPath};
use thiserror::Error;

use crate::model::{ProtectedPathDrop, ProtectedPathDropReason};

use super::tree::TreeResourceStats;

/// Captured upperdir changes and resource stats.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedChanges {
    pub changes: Vec<LayerChange>,
    pub protected_drops: Vec<ProtectedPathDrop>,
    pub stats: TreeResourceStats,
}

/// Raw Linux-byte mutation captured for the private Stage 03 publication path.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CandidateChange {
    /// Validated relative path bytes, with `/` separators.
    pub path: Vec<u8>,
    /// Final logical mutation at `path`.
    pub kind: CandidateChangeKind,
    /// Metadata for node-creating mutations.
    pub metadata: Option<CandidateMetadata>,
}

/// Final logical mutation kinds understood by the private candidate.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CandidateChangeKind {
    Remove,
    Directory,
    OpaqueDirectory,
    Regular {
        source_path: PathBuf,
        size: u64,
        device: u64,
        inode: u64,
        link_count: u64,
    },
    Symlink {
        target: Vec<u8>,
    },
    Device {
        major: u32,
        minor: u32,
    },
    Fifo,
}

/// Host metadata captured before canonical v3 metadata construction.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CandidateMetadata {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime_seconds: i64,
    pub mtime_nanoseconds: i64,
}

/// Bounded-capture counters used by Stage 03 resource assertions.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct CandidateCaptureStats {
    pub entries: u64,
    pub maximum_depth: usize,
    pub maximum_path_bytes: usize,
}

/// Error raised while capturing an overlay upperdir.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CaptureError {
    /// An upper-dir walk / capture I/O error.
    #[error("upperdir capture failed at {path}: {source}")]
    Capture {
        /// Path whose metadata, directory entries, xattrs, content, or link
        /// target could not be read.
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// A captured overlay path did not normalize to a valid relative layer path.
    #[error(transparent)]
    Path(#[from] CasError),

    /// A captured overlay path could not be expressed as a layer path.
    #[error("invalid overlay path change: {0}")]
    InvalidPathChange(String),
}

impl CaptureError {
    fn capture(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Capture {
            path: path.into(),
            source,
        }
    }
}

#[derive(Debug)]
enum PendingChange {
    Write {
        path: LayerPath,
        source_path: PathBuf,
        meta: std::fs::Metadata,
    },
    Delete {
        path: LayerPath,
    },
    Symlink {
        path: LayerPath,
        source_path: String,
    },
    Directory {
        path: LayerPath,
    },
    OpaqueDir {
        path: LayerPath,
    },
}

impl PendingChange {
    fn into_layer_change(self) -> LayerChange {
        match self {
            Self::Write {
                path,
                source_path,
                meta,
            } => LayerChange::WriteFile {
                path,
                source_path,
                size: meta.len(),
            },
            Self::Delete { path } => LayerChange::Delete { path },
            Self::Symlink { path, source_path } => LayerChange::Symlink { path, source_path },
            Self::Directory { path } => LayerChange::Directory { path },
            Self::OpaqueDir { path } => LayerChange::OpaqueDir { path },
        }
    }
}

/// Capture a workspace overlay upperdir into concrete layer changes.
///
/// Capture is metadata-only: file winners become [`LayerChange::WriteFile`]
/// source-path references and publish streams their content, so no payload
/// size bound applies here — a captured tree is limited only by the
/// sandbox's own storage.
///
/// # Errors
///
/// Returns [`CaptureError`] when metadata capture fails.
pub fn capture_upperdir(upperdir: &Path) -> std::result::Result<CapturedChanges, CaptureError> {
    std::fs::create_dir_all(upperdir).map_err(|err| CaptureError::capture(upperdir, err))?;
    let mut emitted_opaque_dirs = HashSet::new();
    let mut entries = Vec::new();
    let mut protected_drops = Vec::new();
    let mut stats = TreeResourceStats {
        dirs: 1,
        ..TreeResourceStats::default()
    };
    walk_upperdir(
        upperdir,
        upperdir,
        &mut emitted_opaque_dirs,
        &mut entries,
        &mut protected_drops,
        &mut stats,
    )?;
    let changes = entries
        .into_iter()
        .map(PendingChange::into_layer_change)
        .collect();
    Ok(CapturedChanges {
        changes,
        protected_drops,
        stats,
    })
}

/// Stream final upperdir mutations without UTF-8 conversion or a resident tree.
///
/// The callback is synchronous. Each path is validated before delivery and no
/// directory entry collection is retained between callback invocations.
#[cfg(unix)]
pub fn capture_upperdir_candidate(
    upperdir: &Path,
    mut emit: impl FnMut(CandidateChange) -> std::result::Result<(), CaptureError>,
) -> std::result::Result<CandidateCaptureStats, CaptureError> {
    std::fs::create_dir_all(upperdir).map_err(|error| CaptureError::capture(upperdir, error))?;
    let mut stats = CandidateCaptureStats::default();
    walk_candidate(upperdir, Vec::new(), 0, &mut emit, &mut stats)?;
    Ok(stats)
}

#[cfg(unix)]
fn walk_candidate(
    directory: &Path,
    parent: Vec<u8>,
    depth: usize,
    emit: &mut impl FnMut(CandidateChange) -> std::result::Result<(), CaptureError>,
    stats: &mut CandidateCaptureStats,
) -> std::result::Result<(), CaptureError> {
    use std::os::unix::ffi::OsStrExt as _;

    for result in
        std::fs::read_dir(directory).map_err(|error| CaptureError::capture(directory, error))?
    {
        let entry = result.map_err(|error| CaptureError::capture(directory, error))?;
        let component = entry.file_name();
        let path = candidate_path(&parent, component.as_bytes(), depth + 1)?;
        let source_path = entry.path();
        let metadata = std::fs::symlink_metadata(&source_path)
            .map_err(|error| CaptureError::capture(&source_path, error))?;
        stats.entries = stats.entries.saturating_add(1);
        stats.maximum_depth = stats.maximum_depth.max(depth + 1);
        stats.maximum_path_bytes = stats.maximum_path_bytes.max(path.len());

        if metadata.file_type().is_dir() {
            let kind = if is_overlay_opaque(&source_path)? {
                CandidateChangeKind::OpaqueDirectory
            } else {
                CandidateChangeKind::Directory
            };
            emit(CandidateChange {
                path: path.clone(),
                kind,
                metadata: Some(candidate_metadata(&metadata)),
            })?;
            walk_candidate(&source_path, path, depth + 1, emit, stats)?;
            continue;
        }
        if is_overlay_whiteout(&source_path, &metadata)? {
            emit(CandidateChange {
                path,
                kind: CandidateChangeKind::Remove,
                metadata: None,
            })?;
            continue;
        }
        let kind = candidate_kind(&source_path, &metadata)?;
        emit(CandidateChange {
            path,
            kind,
            metadata: Some(candidate_metadata(&metadata)),
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn candidate_path(
    parent: &[u8],
    component: &[u8],
    depth: usize,
) -> std::result::Result<Vec<u8>, CaptureError> {
    const MAX_PATH_BYTES: usize = 4_096;
    const MAX_COMPONENT_BYTES: usize = 255;
    const MAX_DEPTH: usize = 64;

    if component.is_empty()
        || component.len() > MAX_COMPONENT_BYTES
        || component.contains(&0)
        || component.contains(&b'/')
        || matches!(component, b"." | b"..")
    {
        return Err(CaptureError::InvalidPathChange(
            "candidate path contains an invalid component".to_owned(),
        ));
    }
    if depth > MAX_DEPTH {
        return Err(CaptureError::InvalidPathChange(
            "candidate path exceeds 64 components".to_owned(),
        ));
    }
    let capacity = parent
        .len()
        .checked_add(usize::from(!parent.is_empty()))
        .and_then(|length| length.checked_add(component.len()))
        .ok_or_else(|| {
            CaptureError::InvalidPathChange("candidate path length overflow".to_owned())
        })?;
    if capacity > MAX_PATH_BYTES {
        return Err(CaptureError::InvalidPathChange(
            "candidate path exceeds 4096 bytes".to_owned(),
        ));
    }
    let mut path = Vec::with_capacity(capacity);
    path.extend_from_slice(parent);
    if !parent.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(component);
    Ok(path)
}

#[cfg(unix)]
fn candidate_kind(
    source_path: &Path,
    metadata: &std::fs::Metadata,
) -> std::result::Result<CandidateChangeKind, CaptureError> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let file_type = metadata.file_type();
    if file_type.is_file() {
        return Ok(CandidateChangeKind::Regular {
            source_path: source_path.to_path_buf(),
            size: metadata.len(),
            device: metadata.dev(),
            inode: metadata.ino(),
            link_count: metadata.nlink(),
        });
    }
    if file_type.is_symlink() {
        let target = std::fs::read_link(source_path)
            .map_err(|error| CaptureError::capture(source_path, error))?;
        return Ok(CandidateChangeKind::Symlink {
            target: target.as_os_str().as_bytes().to_vec(),
        });
    }
    if file_type.is_fifo() {
        return Ok(CandidateChangeKind::Fifo);
    }
    if file_type.is_char_device() || file_type.is_block_device() {
        #[cfg(target_os = "linux")]
        {
            let major = u32::try_from(nix::sys::stat::major(metadata.rdev())).map_err(|_| {
                CaptureError::InvalidPathChange("device major exceeds the v3 field".to_owned())
            })?;
            let minor = u32::try_from(nix::sys::stat::minor(metadata.rdev())).map_err(|_| {
                CaptureError::InvalidPathChange("device minor exceeds the v3 field".to_owned())
            })?;
            return Ok(CandidateChangeKind::Device { major, minor });
        }
        #[cfg(not(target_os = "linux"))]
        return Err(CaptureError::InvalidPathChange(
            "candidate device capture requires Linux".to_owned(),
        ));
    }
    Err(CaptureError::InvalidPathChange(
        "candidate capture encountered an unsupported socket or node kind".to_owned(),
    ))
}

#[cfg(unix)]
fn candidate_metadata(metadata: &std::fs::Metadata) -> CandidateMetadata {
    use std::os::unix::fs::MetadataExt as _;

    CandidateMetadata {
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        mtime_seconds: metadata.mtime(),
        mtime_nanoseconds: metadata.mtime_nsec(),
    }
}

fn walk_upperdir(
    root: &Path,
    dir: &Path,
    emitted_opaque_dirs: &mut HashSet<String>,
    entries: &mut Vec<PendingChange>,
    protected_drops: &mut Vec<ProtectedPathDrop>,
    stats: &mut TreeResourceStats,
) -> std::result::Result<(), CaptureError> {
    let mut dir_entries = std::fs::read_dir(dir)
        .map_err(|err| CaptureError::capture(dir, err))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| CaptureError::capture(dir, err))?;
    dir_entries.sort_by_key(std::fs::DirEntry::file_name);

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in dir_entries {
        let path = entry.path();
        let meta =
            std::fs::symlink_metadata(&path).map_err(|err| CaptureError::capture(&path, err))?;
        let file_type = meta.file_type();
        if file_type.is_dir() {
            stats.dirs = stats.dirs.saturating_add(1);
            dirs.push(path);
        } else {
            record_file_stats(stats, &meta);
            files.push((path, meta));
        }
    }

    for (entry, meta) in files {
        capture_file_entry_metadata(root, &entry, &meta, entries, protected_drops)?;
    }
    for entry in dirs {
        let rel = relative_path(root, &entry)?;
        let layer_path = layer_path_from_relative_or_drop(&rel, protected_drops);
        if has_overlay_opaque_xattr(&entry) {
            if let Some(opaque_path) = layer_path {
                push_opaque_dir(opaque_path, emitted_opaque_dirs, entries);
            }
        } else if let Some(path) = layer_path {
            entries.push(PendingChange::Directory { path });
        }
        walk_upperdir(
            root,
            &entry,
            emitted_opaque_dirs,
            entries,
            protected_drops,
            stats,
        )?;
    }
    Ok(())
}

fn capture_file_entry_metadata(
    root: &Path,
    entry: &Path,
    meta: &std::fs::Metadata,
    entries: &mut Vec<PendingChange>,
    protected_drops: &mut Vec<ProtectedPathDrop>,
) -> std::result::Result<(), CaptureError> {
    let rel = relative_path(root, entry)?;
    if is_overlay_whiteout(entry, meta)? {
        if let Some(path) = layer_path_from_relative_or_drop(&rel, protected_drops) {
            entries.push(PendingChange::Delete { path });
        }
        return Ok(());
    }
    let Some(path) = layer_path_from_relative_or_drop(&rel, protected_drops) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        entries.push(symlink_entry(path, entry)?);
    } else if meta.is_file() {
        entries.push(PendingChange::Write {
            path,
            source_path: entry.to_path_buf(),
            meta: meta.clone(),
        });
    } else {
        protected_drops.push(ProtectedPathDrop {
            path: path.as_str().to_owned(),
            reason: ProtectedPathDropReason::UnsupportedSpecialFile,
        });
    }
    Ok(())
}

fn record_file_stats(stats: &mut TreeResourceStats, meta: &std::fs::Metadata) {
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        stats.symlinks = stats.symlinks.saturating_add(1);
    } else if file_type.is_file() {
        stats.files = stats.files.saturating_add(1);
        stats.bytes = stats.bytes.saturating_add(meta.len());
    }
}

fn push_opaque_dir(
    path: LayerPath,
    emitted_opaque_dirs: &mut HashSet<String>,
    entries: &mut Vec<PendingChange>,
) {
    if emitted_opaque_dirs.insert(path.as_str().to_owned()) {
        entries.push(PendingChange::OpaqueDir { path });
    }
}

fn symlink_entry(
    path: LayerPath,
    entry: &Path,
) -> std::result::Result<PendingChange, CaptureError> {
    Ok(PendingChange::Symlink {
        path,
        source_path: path_string(
            &std::fs::read_link(entry).map_err(|err| CaptureError::capture(entry, err))?,
        )?,
    })
}

fn layer_path(path: &str) -> std::result::Result<LayerPath, CaptureError> {
    LayerPath::parse(path).map_err(CaptureError::Path)
}

fn relative_path(root: &Path, entry: &Path) -> std::result::Result<PathBuf, CaptureError> {
    entry
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|err| CaptureError::InvalidPathChange(err.to_string()))
}

fn layer_path_from_relative_or_drop(
    path: &Path,
    protected_drops: &mut Vec<ProtectedPathDrop>,
) -> Option<LayerPath> {
    match relative_to_string(path).and_then(|path| layer_path(&path)) {
        Ok(path) => Some(path),
        Err(_) => {
            push_invalid_layer_path_drop(path, protected_drops);
            None
        }
    }
}

fn push_invalid_layer_path_drop(path: &Path, protected_drops: &mut Vec<ProtectedPathDrop>) {
    protected_drops.push(ProtectedPathDrop {
        path: invalid_layer_path_placeholder(path),
        reason: ProtectedPathDropReason::InvalidLayerPath,
    });
}

fn invalid_layer_path_placeholder(path: &Path) -> String {
    let encoded = hex_bytes(path.as_os_str().as_encoded_bytes());
    format!(".invalid-layer-path/{encoded}")
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2).max(1));
    if bytes.is_empty() {
        out.push_str("empty");
        return out;
    }
    for &byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn relative_to_string(path: &Path) -> std::result::Result<String, CaptureError> {
    let mut parts = Vec::new();
    for component in path.components() {
        parts.push(path_component_string(component.as_os_str())?);
    }
    Ok(parts.join("/"))
}

fn path_string(path: &Path) -> std::result::Result<String, CaptureError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        CaptureError::InvalidPathChange(format!(
            "overlay path is not valid UTF-8: {}",
            path.display()
        ))
    })
}

fn path_component_string(component: &std::ffi::OsStr) -> std::result::Result<String, CaptureError> {
    component.to_str().map(str::to_owned).ok_or_else(|| {
        let bytes = component.as_encoded_bytes();
        CaptureError::InvalidPathChange(format!(
            "overlay path component is not valid UTF-8: {bytes:?}"
        ))
    })
}

fn is_overlay_whiteout(
    entry: &Path,
    meta: &std::fs::Metadata,
) -> std::result::Result<bool, CaptureError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if meta.file_type().is_char_device() && meta.rdev() == 0 {
            return Ok(true);
        }
    }
    Ok(meta.is_file() && meta.len() == 0 && xattr_value(entry, "user.overlay.whiteout")?.is_some())
}

fn has_overlay_opaque_xattr(entry: &Path) -> bool {
    matches!(xattr_value(entry, "trusted.overlay.opaque"), Ok(Some(value)) if value == b"y")
        || matches!(xattr_value(entry, "user.overlay.opaque"), Ok(Some(value)) if value == b"y")
}

fn is_overlay_opaque(entry: &Path) -> std::result::Result<bool, CaptureError> {
    Ok(
        matches!(xattr_value(entry, "trusted.overlay.opaque")?, Some(value) if value == b"y")
            || matches!(xattr_value(entry, "user.overlay.opaque")?, Some(value) if value == b"y"),
    )
}

#[cfg(target_os = "linux")]
fn xattr_value(path: &Path, name: &str) -> std::result::Result<Option<Vec<u8>>, CaptureError> {
    use rustix::io::Errno;

    let mut buffer = vec![0_u8; 64];
    loop {
        match rustix::fs::lgetxattr(path, name, &mut buffer) {
            Ok(len) => {
                buffer.truncate(len);
                return Ok(Some(buffer));
            }
            Err(Errno::RANGE) => buffer.resize(buffer.len() * 2, 0),
            Err(Errno::NODATA | Errno::OPNOTSUPP) => return Ok(None),
            Err(err) => return Err(CaptureError::capture(path, std::io::Error::from(err))),
        }
    }
}

#[cfg(not(target_os = "linux"))]
// Keep the same fallible helper signature as Linux so whiteout/opaque detection
// call sites stay cfg-free; xattrs simply do not contribute off Linux.
#[expect(
    clippy::unnecessary_wraps,
    reason = "non-Linux parity keeps the Linux fallible helper signature"
)]
const fn xattr_value(
    _path: &Path,
    _name: &str,
) -> std::result::Result<Option<Vec<u8>>, CaptureError> {
    Ok(None)
}
