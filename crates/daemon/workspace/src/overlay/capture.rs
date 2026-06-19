//! Workspace-owned capture for overlay upperdirs.

use std::collections::HashSet;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use layerstack::{CasError, LayerChange, LayerPath};
use thiserror::Error;

use crate::model::{ProtectedPathDrop, ProtectedPathDropReason};

use super::tree::TreeResourceStats;

const MAX_CAPTURE_FILE_BYTES: usize = 8 * 1024 * 1024;
const LOGICAL_WHITEOUT_PREFIX: &str = ".wh.";
const OPAQUE_MARKER: &str = ".wh..wh..opq";

/// Captured upperdir changes and resource stats.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedChanges {
    pub changes: Vec<LayerChange>,
    pub protected_drops: Vec<ProtectedPathDrop>,
    pub stats: TreeResourceStats,
    pub capture_s: f64,
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

    #[must_use]
    pub fn failing_path(&self) -> Option<&Path> {
        match self {
            Self::Capture { path, .. } => Some(path.as_path()),
            _ => None,
        }
    }
}

type Result<T> = std::result::Result<T, CaptureError>;

#[derive(Debug)]
struct CapturedUpperdirMetadata {
    entries: Vec<CapturedUpperdirEntry>,
    protected_drops: Vec<ProtectedPathDrop>,
    stats: TreeResourceStats,
}

#[derive(Debug)]
enum CapturedUpperdirEntry {
    Write {
        path: LayerPath,
        source_path: PathBuf,
        meta: RegularFileCaptureMeta,
    },
    Delete {
        path: LayerPath,
    },
    Symlink {
        path: LayerPath,
        source_path: String,
    },
    OpaqueDir {
        path: LayerPath,
    },
}

impl CapturedUpperdirEntry {
    fn materialize_in_memory(&self, max_bytes: usize) -> Result<LayerChange> {
        match self {
            Self::Write {
                path,
                source_path,
                meta,
            } => Ok(LayerChange::Write {
                path: path.clone(),
                content: read_regular_file(source_path, meta, max_bytes)?,
            }),
            Self::Delete { path } => Ok(LayerChange::Delete { path: path.clone() }),
            Self::Symlink { path, source_path } => Ok(LayerChange::Symlink {
                path: path.clone(),
                source_path: source_path.clone(),
            }),
            Self::OpaqueDir { path } => Ok(LayerChange::OpaqueDir { path: path.clone() }),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RegularFileCaptureMeta {
    len: u64,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl RegularFileCaptureMeta {
    fn from_metadata(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            #[cfg(unix)]
            dev: meta.dev(),
            #[cfg(unix)]
            ino: meta.ino(),
        }
    }
}

/// Capture a workspace overlay upperdir into concrete layer changes.
///
/// # Errors
///
/// Returns [`CaptureError`] when metadata capture or selected payload
/// materialization fails.
pub fn capture_upperdir(upperdir: &Path) -> Result<CapturedChanges> {
    let start = std::time::Instant::now();
    let metadata = capture_upperdir_metadata(upperdir)?;
    let changes = materialize_entries_in_memory(&metadata.entries, MAX_CAPTURE_FILE_BYTES)?;
    Ok(CapturedChanges {
        changes,
        protected_drops: metadata.protected_drops,
        stats: metadata.stats,
        capture_s: start.elapsed().as_secs_f64(),
    })
}

fn capture_upperdir_metadata(upperdir: &Path) -> Result<CapturedUpperdirMetadata> {
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
    Ok(CapturedUpperdirMetadata {
        entries,
        protected_drops,
        stats,
    })
}

fn walk_upperdir(
    root: &Path,
    dir: &Path,
    emitted_opaque_dirs: &mut HashSet<String>,
    entries: &mut Vec<CapturedUpperdirEntry>,
    protected_drops: &mut Vec<ProtectedPathDrop>,
    stats: &mut TreeResourceStats,
) -> Result<()> {
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
        capture_file_entry_metadata(
            root,
            &entry,
            &meta,
            emitted_opaque_dirs,
            entries,
            protected_drops,
        )?;
    }
    for entry in dirs {
        if has_overlay_opaque_xattr(&entry) {
            let rel = relative_path(root, &entry)?;
            if let Some(opaque_path) = layer_path_from_relative_or_drop(&rel, protected_drops) {
                push_opaque_dir(opaque_path, emitted_opaque_dirs, entries);
            }
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
    emitted_opaque_dirs: &mut HashSet<String>,
    entries: &mut Vec<CapturedUpperdirEntry>,
    protected_drops: &mut Vec<ProtectedPathDrop>,
) -> Result<()> {
    let rel = relative_path(root, entry)?;
    let name = entry
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name == OPAQUE_MARKER {
        let Some(parent) = rel.parent().filter(|parent| !parent.as_os_str().is_empty()) else {
            push_invalid_layer_path_drop(&rel, protected_drops);
            return Ok(());
        };
        if let Some(opaque_path) = layer_path_from_relative_or_drop(parent, protected_drops) {
            push_opaque_dir(opaque_path, emitted_opaque_dirs, entries);
        }
        return Ok(());
    }
    if is_whiteout_marker(name) {
        let target = whiteout_target(&rel);
        if let Some(path) = layer_path_from_relative_or_drop(&target, protected_drops) {
            entries.push(CapturedUpperdirEntry::Delete { path });
        }
        return Ok(());
    }
    if is_overlay_whiteout(entry, meta)? {
        if let Some(path) = layer_path_from_relative_or_drop(&rel, protected_drops) {
            entries.push(CapturedUpperdirEntry::Delete { path });
        }
        return Ok(());
    }
    let Some(path) = layer_path_from_relative_or_drop(&rel, protected_drops) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        entries.push(symlink_entry(path, entry)?);
    } else if meta.is_file() {
        entries.push(CapturedUpperdirEntry::Write {
            path,
            source_path: entry.to_path_buf(),
            meta: RegularFileCaptureMeta::from_metadata(meta),
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
    entries: &mut Vec<CapturedUpperdirEntry>,
) {
    if emitted_opaque_dirs.insert(path.as_str().to_owned()) {
        entries.push(CapturedUpperdirEntry::OpaqueDir { path });
    }
}

fn read_regular_file(
    entry: &Path,
    expected_meta: &RegularFileCaptureMeta,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    ensure_capture_file_size(entry, expected_meta.len, max_bytes)?;
    let file =
        open_regular_file_no_follow(entry).map_err(|err| CaptureError::capture(entry, err))?;
    let actual_meta = file
        .metadata()
        .map_err(|err| CaptureError::capture(entry, err))?;
    if !actual_meta.is_file() || !same_file(expected_meta, &actual_meta) {
        return Err(changed_during_capture(entry));
    }
    ensure_capture_file_size(entry, actual_meta.len(), max_bytes)?;

    let mut content = Vec::new();
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(limit)
        .read_to_end(&mut content)
        .map_err(|err| CaptureError::capture(entry, err))?;
    if content.len() > max_bytes {
        return Err(capture_file_too_large(
            entry,
            u64::try_from(content.len()).unwrap_or(u64::MAX),
            max_bytes,
        ));
    }
    Ok(content)
}

fn ensure_capture_file_size(entry: &Path, size: u64, max_bytes: usize) -> Result<()> {
    let max = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if size > max {
        return Err(capture_file_too_large(entry, size, max_bytes));
    }
    Ok(())
}

#[cfg(unix)]
fn open_regular_file_no_follow(entry: &Path) -> io::Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags};

    rustix::fs::open(
        entry,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(io::Error::from)
}

#[cfg(not(unix))]
fn open_regular_file_no_follow(entry: &Path) -> io::Result<std::fs::File> {
    std::fs::File::open(entry)
}

#[cfg(unix)]
fn same_file(left: &RegularFileCaptureMeta, right: &std::fs::Metadata) -> bool {
    left.dev == right.dev() && left.ino == right.ino()
}

#[cfg(not(unix))]
fn same_file(_left: &RegularFileCaptureMeta, _right: &std::fs::Metadata) -> bool {
    true
}

fn changed_during_capture(entry: &Path) -> CaptureError {
    CaptureError::capture(
        entry,
        io::Error::new(
            io::ErrorKind::InvalidData,
            "overlay regular file changed during capture",
        ),
    )
}

fn capture_file_too_large(entry: &Path, size: u64, max_bytes: usize) -> CaptureError {
    CaptureError::capture(
        entry,
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("overlay regular file too large: {size} > {max_bytes} bytes"),
        ),
    )
}

fn symlink_entry(path: LayerPath, entry: &Path) -> Result<CapturedUpperdirEntry> {
    Ok(CapturedUpperdirEntry::Symlink {
        path,
        source_path: path_string(
            &std::fs::read_link(entry).map_err(|err| CaptureError::capture(entry, err))?,
        )?,
    })
}

fn materialize_entries_in_memory(
    entries: &[CapturedUpperdirEntry],
    max_bytes: usize,
) -> Result<Vec<LayerChange>> {
    entries
        .iter()
        .map(|entry| entry.materialize_in_memory(max_bytes))
        .collect()
}

fn layer_path(path: &str) -> Result<LayerPath> {
    LayerPath::parse(path).map_err(CaptureError::Path)
}

fn relative_path(root: &Path, entry: &Path) -> Result<PathBuf> {
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

fn relative_to_string(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        parts.push(path_component_string(component.as_os_str())?);
    }
    Ok(parts.join("/"))
}

fn path_string(path: &Path) -> Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        CaptureError::InvalidPathChange(format!(
            "overlay path is not valid UTF-8: {}",
            path.display()
        ))
    })
}

fn path_component_string(component: &std::ffi::OsStr) -> Result<String> {
    component.to_str().map(str::to_owned).ok_or_else(|| {
        let bytes = component.as_encoded_bytes();
        CaptureError::InvalidPathChange(format!(
            "overlay path component is not valid UTF-8: {bytes:?}"
        ))
    })
}

fn is_whiteout_marker(name: &str) -> bool {
    name.starts_with(LOGICAL_WHITEOUT_PREFIX)
        && name != OPAQUE_MARKER
        && name.len() > LOGICAL_WHITEOUT_PREFIX.len()
}

fn whiteout_target(rel: &Path) -> PathBuf {
    let name = rel
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let target_name = &name[LOGICAL_WHITEOUT_PREFIX.len()..];
    rel.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(
            || PathBuf::from(target_name),
            |parent| parent.join(target_name),
        )
}

fn is_overlay_whiteout(entry: &Path, meta: &std::fs::Metadata) -> Result<bool> {
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

#[cfg(target_os = "linux")]
fn xattr_value(path: &Path, name: &str) -> Result<Option<Vec<u8>>> {
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
const fn xattr_value(_path: &Path, _name: &str) -> Result<Option<Vec<u8>>> {
    Ok(None)
}
