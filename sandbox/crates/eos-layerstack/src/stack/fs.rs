use std::path::Path;
use std::sync::atomic::Ordering;

use crate::error::LayerStackError;
use crate::fsutil::remove_path;
use crate::storage_lock::STORAGE_WRITER_LOCK_FILE;

use super::NEXT_TMP_WRITE;

pub(super) fn replace_workspace_contents(
    destination: &Path,
    source: &Path,
) -> Result<(), LayerStackError> {
    std::fs::create_dir_all(destination)?;
    for child in std::fs::read_dir(destination)? {
        remove_path(&child?.path())?;
    }
    for child in std::fs::read_dir(source)? {
        let child = child?;
        move_path(&child.path(), &destination.join(child.file_name()))?;
    }
    Ok(())
}

fn move_path(source: &Path, destination: &Path) -> Result<(), LayerStackError> {
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(err) if err.raw_os_error() == Some(18) => {
            copy_path(source, destination)?;
            remove_path(source)
        }
        Err(err) => Err(err.into()),
    }
}

fn copy_path(source: &Path, destination: &Path) -> Result<(), LayerStackError> {
    let meta = std::fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let target = std::fs::read_link(source)?;
        remove_path(destination)?;
        std::os::unix::fs::symlink(target, destination)?;
    } else if meta.is_dir() {
        std::fs::create_dir_all(destination)?;
        for child in std::fs::read_dir(source)? {
            let child = child?;
            copy_path(&child.path(), &destination.join(child.file_name()))?;
        }
    } else if meta.is_file() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        remove_path(destination)?;
        std::fs::copy(source, destination)?;
    }
    Ok(())
}

pub(super) fn clear_storage_root_preserving_lock(
    storage_root: &Path,
) -> Result<(), LayerStackError> {
    std::fs::create_dir_all(storage_root)?;
    for child in std::fs::read_dir(storage_root)? {
        let child = child?;
        if child.file_name() == STORAGE_WRITER_LOCK_FILE {
            continue;
        }
        remove_path(&child.path())?;
    }
    Ok(())
}

pub(crate) fn write_atomic(path: impl AsRef<Path>, bytes: &[u8]) -> Result<(), LayerStackError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("layerstack"),
        std::process::id(),
        NEXT_TMP_WRITE.fetch_add(1, Ordering::Relaxed)
    ));
    // tmp+rename+fsync(parent): the directory entry is only durable once the
    // parent dir is fsynced after the rename, so a crash cannot leave the
    // manifest/digest pointer-swap half-applied (CAS linearization).
    let result = (|| -> Result<(), LayerStackError> {
        write_bytes_fsynced(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            fsync_dir(parent)?;
        }
        Ok(())
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// Write `bytes` to `path` (create/truncate) and fsync the file before
/// returning; the caller fsyncs the parent dir after any rename.
fn write_bytes_fsynced(path: &Path, bytes: &[u8]) -> Result<(), LayerStackError> {
    use std::io::Write as _;

    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// fsync a directory so a prior create/rename into it is persisted.
pub(crate) fn fsync_dir(path: &Path) -> Result<(), LayerStackError> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// fsync every non-symlink regular file under `root` (the staged layer tree),
/// matching `os.walk(followlinks=False)` + `is_file()` filtering. Special
/// files (char-device whiteouts) and symlinks are skipped — fsync is N/A.
pub(super) fn fsync_tree_files(root: &Path) -> Result<(), LayerStackError> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = std::fs::symlink_metadata(&path)?.file_type();
        if file_type.is_dir() {
            fsync_tree_files(&path)?;
        } else if file_type.is_file() {
            std::fs::File::open(&path)?.sync_all()?;
        }
    }
    Ok(())
}
