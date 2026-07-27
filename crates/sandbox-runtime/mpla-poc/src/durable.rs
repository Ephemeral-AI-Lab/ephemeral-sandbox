use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{PocError, PocResult};

const MAX_JSON_BYTES: u64 = 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

pub(crate) struct FileLock {
    _file: File,
}

impl FileLock {
    pub(crate) fn exclusive(path: &Path) -> PocResult<Self> {
        Self::acquire(path, lock_exclusive)
    }

    pub(crate) fn shared(path: &Path) -> PocResult<Self> {
        Self::acquire(path, lock_shared)
    }

    fn acquire(path: &Path, lock: fn(&File) -> std::io::Result<()>) -> PocResult<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| PocError::io("open allocation lock", path, source))?;
        lock(&file).map_err(|source| PocError::io("lock allocation", path, source))?;
        Ok(Self { _file: file })
    }
}

pub fn replace_json<T: Serialize>(path: &Path, value: &T) -> PocResult<()> {
    let bytes = encoded_json(value)?;
    let parent = path
        .parent()
        .ok_or_else(|| PocError::Integrity("durable selector has no parent".to_owned()))?;
    std::fs::create_dir_all(parent)
        .map_err(|source| PocError::io("create durable selector directory", parent, source))?;
    let (temporary, mut file) = create_temporary(path)?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|source| PocError::io("write durable selector", &temporary, source))?;
        file.sync_all()
            .map_err(|source| PocError::io("fsync durable selector", &temporary, source))?;
        drop(file);
        std::fs::rename(&temporary, path)
            .map_err(|source| PocError::io("replace durable selector", path, source))?;
        fsync_dir(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub fn replace_with_synced_file(path: &Path, source: &Path) -> PocResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| PocError::Integrity("durable selector has no parent".to_owned()))?;
    let metadata = source
        .metadata()
        .map_err(|error| PocError::io("stat durable selector source", source, error))?;
    if !metadata.is_file() {
        return Err(PocError::Integrity(format!(
            "durable selector source is not a file at {}",
            source.display()
        )));
    }
    let temporary = create_hard_link_temporary(path, source)?;
    let result = (|| {
        std::fs::rename(&temporary, path)
            .map_err(|error| PocError::io("replace durable selector", path, error))?;
        fsync_dir(parent)
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> PocResult<T> {
    let file =
        File::open(path).map_err(|source| PocError::io("open durable JSON", path, source))?;
    let length = file
        .metadata()
        .map_err(|source| PocError::io("stat durable JSON", path, source))?
        .len();
    if length > MAX_JSON_BYTES {
        return Err(PocError::Integrity(format!(
            "durable JSON exceeds {MAX_JSON_BYTES} bytes at {}",
            path.display()
        )));
    }
    serde_json::from_reader(file).map_err(PocError::from)
}

pub(crate) fn write_immutable_json<T: Serialize>(path: &Path, value: &T) -> PocResult<()> {
    let bytes = encoded_json(value)?;
    let parent = path
        .parent()
        .ok_or_else(|| PocError::Integrity("immutable JSON has no parent".to_owned()))?;
    std::fs::create_dir_all(parent)
        .map_err(|source| PocError::io("create immutable JSON directory", parent, source))?;
    let (temporary, mut file) = create_temporary(path)?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|source| PocError::io("write immutable JSON", &temporary, source))?;
        file.sync_all()
            .map_err(|source| PocError::io("fsync immutable JSON", &temporary, source))?;
        drop(file);
        match std::fs::hard_link(&temporary, path) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut existing = Vec::new();
                File::open(path)
                    .and_then(|mut current| current.read_to_end(&mut existing))
                    .map_err(|source| PocError::io("read immutable JSON", path, source))?;
                if existing != bytes {
                    return Err(PocError::Integrity(format!(
                        "immutable JSON collision at {}",
                        path.display()
                    )));
                }
            }
            Err(source) => {
                return Err(PocError::io("install immutable JSON", path, source));
            }
        }
        std::fs::remove_file(&temporary).map_err(|source| {
            PocError::io("remove immutable JSON temporary", &temporary, source)
        })?;
        fsync_dir(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn fsync_dir(path: &Path) -> PocResult<()> {
    fsync_directory(path).map_err(|source| PocError::io("fsync directory", path, source))
}

fn encoded_json<T: Serialize>(value: &T) -> PocResult<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn create_temporary(path: &Path) -> PocResult<(PathBuf, File)> {
    for _ in 0..64 {
        let temporary = next_temporary_path(path)?;
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(PocError::io(
                    "create durable file temporary",
                    &temporary,
                    source,
                ));
            }
        }
    }
    Err(PocError::Integrity(format!(
        "unable to allocate durable temporary beside {}",
        path.display()
    )))
}

fn create_hard_link_temporary(path: &Path, source: &Path) -> PocResult<PathBuf> {
    for _ in 0..64 {
        let temporary = next_temporary_path(path)?;
        match std::fs::hard_link(source, &temporary) {
            Ok(()) => return Ok(temporary),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(PocError::io(
                    "link durable selector temporary",
                    &temporary,
                    error,
                ));
            }
        }
    }
    Err(PocError::Integrity(format!(
        "unable to link durable temporary beside {}",
        path.display()
    )))
}

fn next_temporary_path(path: &Path) -> PocResult<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| PocError::Integrity("durable file has no parent".to_owned()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    )))
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: flock only reads the valid descriptor borrowed from `file`.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn lock_shared(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: flock only reads the valid descriptor borrowed from `file`.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_shared(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn fsync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn fsync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
