use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use uuid::Uuid;

use crate::{AllocationId, InodeWitness, PhysicalSnapshot, PocError, PocResult};

pub fn write_atomic_json<T: Serialize>(path: &Path, value: &T) -> PocResult<()> {
    let parent = path.parent().ok_or_else(|| {
        PocError::Integrity(format!(
            "evidence path has no parent directory: {}",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| PocError::io("create evidence parent", parent, error))?;
    let file_name = path.file_name().ok_or_else(|| {
        PocError::Integrity(format!(
            "evidence path has no file name: {}",
            path.display()
        ))
    })?;
    let temporary_path = parent.join(format!(
        ".{}.tmp.{}.{}",
        file_name.to_string_lossy(),
        std::process::id(),
        Uuid::new_v4()
    ));
    let result = write_and_install(path, &temporary_path, parent, value);
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> PocResult<T> {
    let file = File::open(path).map_err(|error| PocError::io("open JSON evidence", path, error))?;
    serde_json::from_reader(BufReader::new(file)).map_err(PocError::from)
}

#[cfg(unix)]
pub fn capture_physical_snapshot(
    allocation_id: &AllocationId,
    allocation_path: &Path,
) -> PocResult<PhysicalSnapshot> {
    let root_metadata = fs::symlink_metadata(allocation_path)
        .map_err(|error| PocError::io("stat allocation root", allocation_path, error))?;
    if !root_metadata.is_dir() {
        return Err(PocError::Integrity(format!(
            "allocation root is not a directory: {}",
            allocation_path.display()
        )));
    }

    let mut stack = vec![allocation_path.to_path_buf()];
    let mut representative_inodes = Vec::new();
    let mut logical_bytes = 0u64;
    let mut allocated_bytes = 0u64;
    let mut inode_count = 0u64;
    let mut file_count = 0u64;
    let mut directory_count = 0u64;

    while let Some(path) = stack.pop() {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| PocError::io("stat allocation entry", &path, error))?;
        inode_count = inode_count
            .checked_add(1)
            .ok_or_else(|| PocError::Integrity("allocation inode count overflow".to_owned()))?;
        allocated_bytes = allocated_bytes
            .checked_add(metadata.blocks().checked_mul(512).ok_or_else(|| {
                PocError::Integrity("allocation block accounting overflow".to_owned())
            })?)
            .ok_or_else(|| {
                PocError::Integrity("allocation allocated-byte total overflow".to_owned())
            })?;
        let relative_path = path
            .strip_prefix(allocation_path)
            .map_err(|error| PocError::Integrity(error.to_string()))?;
        representative_inodes.push(InodeWitness {
            relative_path: if relative_path.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                relative_path.to_path_buf()
            },
            device: metadata.dev(),
            inode: metadata.ino(),
        });
        if metadata.is_file() {
            file_count = file_count
                .checked_add(1)
                .ok_or_else(|| PocError::Integrity("allocation file count overflow".to_owned()))?;
            logical_bytes = logical_bytes.checked_add(metadata.len()).ok_or_else(|| {
                PocError::Integrity("allocation logical-byte total overflow".to_owned())
            })?;
        } else if metadata.is_dir() {
            directory_count = directory_count.checked_add(1).ok_or_else(|| {
                PocError::Integrity("allocation directory count overflow".to_owned())
            })?;
            let entries = fs::read_dir(&path)
                .map_err(|error| PocError::io("read allocation directory", &path, error))?;
            for entry in entries {
                let entry =
                    entry.map_err(|error| PocError::io("read allocation entry", &path, error))?;
                stack.push(entry.path());
            }
        }
    }
    representative_inodes.sort();

    Ok(PhysicalSnapshot {
        allocation_id: allocation_id.clone(),
        allocation_path: allocation_path.to_path_buf(),
        device: root_metadata.dev(),
        representative_inodes,
        logical_bytes,
        allocated_bytes,
        inode_count,
        file_count,
        directory_count,
    })
}

#[cfg(not(unix))]
pub fn capture_physical_snapshot(
    _allocation_id: &AllocationId,
    _allocation_path: &Path,
) -> PocResult<PhysicalSnapshot> {
    Err(PocError::Unsupported(
        "physical allocation snapshots require Unix metadata".to_owned(),
    ))
}

fn write_and_install<T: Serialize>(
    path: &Path,
    temporary_path: &Path,
    parent: &Path,
    value: &T,
) -> PocResult<()> {
    let mut temporary = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary_path)
        .map_err(|error| PocError::io("create temporary JSON evidence", temporary_path, error))?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary
        .write_all(b"\n")
        .map_err(|error| PocError::io("terminate JSON evidence", temporary_path, error))?;
    temporary
        .sync_all()
        .map_err(|error| PocError::io("fsync JSON evidence", temporary_path, error))?;
    drop(temporary);
    fs::rename(temporary_path, path)
        .map_err(|error| PocError::io("replace JSON evidence", path, error))?;
    let parent_dir =
        File::open(parent).map_err(|error| PocError::io("open evidence parent", parent, error))?;
    parent_dir
        .sync_all()
        .map_err(|error| PocError::io("fsync evidence parent", parent, error))
}
