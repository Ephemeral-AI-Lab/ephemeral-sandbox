#[cfg(target_os = "linux")]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufReader, Read};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Component;
use std::path::{Path, PathBuf};
use std::thread;

#[cfg(target_os = "linux")]
use rustix::fd::AsFd;
#[cfg(target_os = "linux")]
use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat, StatExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};

use crate::{AllocationHandle, AllocationId, InodeWitness, PhysicalSnapshot, PocError, PocResult};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryEntryKind {
    Directory,
    Regular,
    Symlink,
    BlockDevice,
    CharacterDevice,
    Fifo,
    Socket,
    Other,
}

/// A stable physical inventory row. Device/inode facts are deliberately
/// evidence-only and must never be fed into semantic-v1 canonical identity.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct InventoryEntry {
    pub relative_path: PathBuf,
    pub kind: InventoryEntryKind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub allocated_bytes: u64,
    pub modified_ns: i128,
    pub device: u64,
    pub inode: u64,
    pub link_count: u64,
    pub device_number: u64,
    pub symlink_target: Option<PathBuf>,
    pub content_sha256: Option<String>,
    pub xattrs_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AllocationInventory {
    pub schema_version: u32,
    pub allocation_id: AllocationId,
    pub allocation_root: PathBuf,
    pub inventory_sha256: String,
    pub entries: Vec<InventoryEntry>,
    pub physical: PhysicalSnapshot,
}

/// Capture a deterministic, no-follow inventory of the permanent upper.
///
/// M0 intentionally hashes regular-file bytes as a conservative falsifier.
/// The M1 semantic engine replaces this with bounded semantic records and
/// receipt-aware scanning; this inventory remains the physical stability
/// witness.
pub fn capture_inventory(allocation: &AllocationHandle) -> PocResult<AllocationInventory> {
    capture_inventory_with_content_at(allocation, &allocation.upper_dir, true)
}

pub fn capture_metadata_inventory(allocation: &AllocationHandle) -> PocResult<AllocationInventory> {
    capture_inventory_with_content_at(allocation, &allocation.upper_dir, false)
}

#[cfg(target_os = "linux")]
pub(crate) fn capture_inventory_anchored(
    allocation: &AllocationHandle,
    upper: &OwnedFd,
) -> PocResult<AllocationInventory> {
    capture_inventory_from_descriptor(allocation, upper, true)
}

fn capture_inventory_with_content_at(
    allocation: &AllocationHandle,
    upper_dir: &Path,
    include_content_sha256: bool,
) -> PocResult<AllocationInventory> {
    let mut entries = Vec::new();
    walk_no_follow(upper_dir, upper_dir, &mut entries, include_content_sha256)?;
    entries.sort_by(|left, right| {
        raw_path_bytes(&left.relative_path).cmp(&raw_path_bytes(&right.relative_path))
    });

    let inventory_sha256 = digest_entries(&entries)?;
    let physical = summarize_physical(allocation, upper_dir, &entries)?;
    Ok(AllocationInventory {
        schema_version: crate::SCHEMA_VERSION,
        allocation_id: allocation.descriptor.allocation_id.clone(),
        allocation_root: allocation.allocation_root.clone(),
        inventory_sha256,
        entries,
        physical,
    })
}

#[cfg(target_os = "linux")]
fn capture_inventory_from_descriptor(
    allocation: &AllocationHandle,
    upper: &OwnedFd,
    include_content_sha256: bool,
) -> PocResult<AllocationInventory> {
    let root_before = anchored_fstat(
        upper,
        &allocation.upper_dir,
        "stat pinned allocation upper for inventory",
    )?;
    if raw_mode_file_type(root_before.st_mode as rustix::fs::RawMode) != FileType::Directory {
        return Err(PocError::Integrity(
            "pinned allocation upper is not a directory".to_owned(),
        ));
    }

    let mut entries = Vec::new();
    walk_anchored_directory(
        allocation,
        upper,
        Path::new(""),
        &mut entries,
        include_content_sha256,
    )?;
    let root_after = anchored_fstat(
        upper,
        &allocation.upper_dir,
        "revalidate pinned allocation upper after inventory",
    )?;
    require_stable_anchored_stat(
        &root_before,
        &root_after,
        &allocation.upper_dir,
        "allocation upper changed during inventory",
    )?;

    entries.sort_by(|left, right| {
        raw_path_bytes(&left.relative_path).cmp(&raw_path_bytes(&right.relative_path))
    });
    let inventory_sha256 = digest_entries(&entries)?;
    let physical = summarize_physical_with_device(allocation, root_before.st_dev as u64, &entries)?;
    Ok(AllocationInventory {
        schema_version: crate::SCHEMA_VERSION,
        allocation_id: allocation.descriptor.allocation_id.clone(),
        allocation_root: allocation.allocation_root.clone(),
        inventory_sha256,
        entries,
        physical,
    })
}

#[cfg(target_os = "linux")]
fn walk_anchored_directory(
    allocation: &AllocationHandle,
    directory: &OwnedFd,
    relative_directory: &Path,
    output: &mut Vec<InventoryEntry>,
    include_content_sha256: bool,
) -> PocResult<()> {
    let display_directory = allocation.upper_dir.join(relative_directory);
    let enumeration = rustix::fs::openat(
        directory,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open pinned inventory directory for enumeration",
            &display_directory,
            std::io::Error::from(error),
        )
    })?;
    let reader = rustix::fs::Dir::read_from(enumeration.as_fd()).map_err(|error| {
        PocError::io(
            "read pinned inventory directory",
            &display_directory,
            std::io::Error::from(error),
        )
    })?;
    let mut names = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|error| {
            PocError::io(
                "read pinned inventory entry",
                &display_directory,
                std::io::Error::from(error),
            )
        })?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
    names.sort_by_key(|name| name.as_bytes().to_vec());

    for name in names {
        let relative_path = relative_directory.join(&name);
        let display_path = allocation.upper_dir.join(&relative_path);
        let before = anchored_statat(
            directory,
            &name,
            &display_path,
            "stat pinned inventory entry",
        )?;
        let file_type = raw_mode_file_type(before.st_mode as rustix::fs::RawMode);
        let flags = anchored_open_flags(file_type);
        let entry =
            rustix::fs::openat(directory, &name, flags, Mode::empty()).map_err(|error| {
                PocError::io(
                    "open pinned inventory entry",
                    &display_path,
                    std::io::Error::from(error),
                )
            })?;
        let opened = anchored_fstat(&entry, &display_path, "stat opened inventory entry")?;
        require_same_anchored_identity(
            &before,
            &opened,
            &display_path,
            "inventory entry changed while it was opened",
        )?;

        let symlink_target = if file_type == FileType::Symlink {
            let target = rustix::fs::readlinkat(&entry, "", Vec::new()).map_err(|error| {
                PocError::io(
                    "read pinned inventory symlink",
                    &display_path,
                    std::io::Error::from(error),
                )
            })?;
            Some(PathBuf::from(OsString::from_vec(target.into_bytes())))
        } else {
            None
        };
        let content_sha256 = if include_content_sha256 && file_type == FileType::RegularFile {
            Some(hash_file_descriptor(&entry, &display_path)?)
        } else {
            None
        };
        let xattrs_sha256 = if matches!(file_type, FileType::Directory | FileType::RegularFile) {
            hash_xattrs_descriptor(&entry, &display_path)?
        } else {
            hash_xattrs(&descriptor_path(directory).join(&name))?
        };

        if file_type == FileType::Directory {
            walk_anchored_directory(
                allocation,
                &entry,
                &relative_path,
                output,
                include_content_sha256,
            )?;
        }

        let opened_after =
            anchored_fstat(&entry, &display_path, "revalidate opened inventory entry")?;
        let named_after = anchored_statat(
            directory,
            &name,
            &display_path,
            "revalidate pinned inventory entry",
        )?;
        require_stable_anchored_stat(
            &opened,
            &opened_after,
            &display_path,
            "inventory entry changed while it was read",
        )?;
        require_stable_anchored_stat(
            &opened,
            &named_after,
            &display_path,
            "inventory entry was replaced while it was read",
        )?;
        output.push(inventory_entry_from_stat(
            relative_path,
            file_type,
            &opened,
            symlink_target,
            content_sha256,
            xattrs_sha256,
            &display_path,
        )?);
    }
    Ok(())
}

/// Take two inventories separated by a scheduler yield and fail closed on any
/// physical or content change.
pub fn capture_stable_pair(
    allocation: &AllocationHandle,
) -> PocResult<(AllocationInventory, AllocationInventory)> {
    let before = capture_inventory(allocation)?;
    thread::yield_now();
    let after = capture_inventory(allocation)?;
    if before != after {
        return Err(PocError::RecoveryRequired(format!(
            "allocation {} changed between stability inventories",
            allocation.descriptor.allocation_id
        )));
    }
    Ok((before, after))
}

pub fn capture_stable_metadata_pair(
    allocation: &AllocationHandle,
) -> PocResult<(AllocationInventory, AllocationInventory)> {
    let before = capture_metadata_inventory(allocation)?;
    thread::yield_now();
    let after = capture_metadata_inventory(allocation)?;
    if before != after {
        return Err(PocError::RecoveryRequired(format!(
            "allocation {} changed between metadata stability inventories",
            allocation.descriptor.allocation_id
        )));
    }
    Ok((before, after))
}

pub fn capture_physical_witness(
    allocation: &AllocationHandle,
    affected_paths: &[PathBuf],
) -> PocResult<PhysicalSnapshot> {
    capture_physical_witness_at(allocation, &allocation.upper_dir, affected_paths)
}

#[cfg(target_os = "linux")]
pub(crate) fn capture_physical_witness_anchored(
    allocation: &AllocationHandle,
    upper: &OwnedFd,
    affected_paths: &[PathBuf],
) -> PocResult<PhysicalSnapshot> {
    capture_physical_witness_from_descriptor(allocation, upper, affected_paths)
}

fn capture_physical_witness_at(
    allocation: &AllocationHandle,
    upper_dir: &Path,
    affected_paths: &[PathBuf],
) -> PocResult<PhysicalSnapshot> {
    let root_metadata = fs::symlink_metadata(upper_dir).map_err(|error| {
        PocError::io(
            "stat allocation upper for receipt witness",
            upper_dir,
            error,
        )
    })?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(PocError::Integrity(
            "allocation upper is not a real directory".to_owned(),
        ));
    }

    let mut normalized = affected_paths.to_vec();
    normalized.sort_by_key(|path| raw_path_bytes(path));
    normalized.dedup();
    if normalized.is_empty() || normalized.len() > 64 {
        return Err(PocError::Integrity(
            "receipt witness must name between one and 64 affected paths".to_owned(),
        ));
    }

    let mut representative_inodes = vec![InodeWitness {
        relative_path: PathBuf::from("."),
        device: metadata_device(&root_metadata),
        inode: metadata_inode(&root_metadata),
    }];
    let mut logical_bytes = 0_u64;
    let mut allocated_bytes = 0_u64;
    let mut file_count = 0_u64;
    let mut directory_count = 1_u64;
    for relative_path in &normalized {
        validate_relative_witness_path(relative_path)?;
        let path = upper_dir.join(relative_path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| PocError::io("stat affected receipt path", &path, error))?;
        if metadata.file_type().is_symlink() {
            return Err(PocError::Integrity(format!(
                "receipt-hit witness does not accept symlink path {}",
                relative_path.display()
            )));
        }
        if metadata.is_file() {
            logical_bytes = logical_bytes.saturating_add(metadata.len());
        }
        allocated_bytes = allocated_bytes.saturating_add(metadata_allocated_bytes(&metadata));
        file_count = file_count.saturating_add(u64::from(metadata.is_file()));
        directory_count = directory_count.saturating_add(u64::from(metadata.is_dir()));
        if representative_inodes.len() < 16 {
            representative_inodes.push(InodeWitness {
                relative_path: relative_path.clone(),
                device: metadata_device(&metadata),
                inode: metadata_inode(&metadata),
            });
        }
    }

    Ok(PhysicalSnapshot {
        allocation_id: allocation.descriptor.allocation_id.clone(),
        allocation_path: allocation.allocation_root.clone(),
        device: metadata_device(&root_metadata),
        representative_inodes,
        logical_bytes,
        allocated_bytes,
        inode_count: u64::try_from(normalized.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1),
        file_count,
        directory_count,
    })
}

#[cfg(target_os = "linux")]
fn capture_physical_witness_from_descriptor(
    allocation: &AllocationHandle,
    upper: &OwnedFd,
    affected_paths: &[PathBuf],
) -> PocResult<PhysicalSnapshot> {
    let root_before = anchored_fstat(
        upper,
        &allocation.upper_dir,
        "stat pinned allocation upper for receipt witness",
    )?;
    if raw_mode_file_type(root_before.st_mode as rustix::fs::RawMode) != FileType::Directory {
        return Err(PocError::Integrity(
            "pinned allocation upper is not a directory".to_owned(),
        ));
    }

    let mut normalized = affected_paths.to_vec();
    normalized.sort_by_key(|path| raw_path_bytes(path));
    normalized.dedup();
    if normalized.is_empty() || normalized.len() > 64 {
        return Err(PocError::Integrity(
            "receipt witness must name between one and 64 affected paths".to_owned(),
        ));
    }

    let mut representative_inodes = vec![InodeWitness {
        relative_path: PathBuf::from("."),
        device: root_before.st_dev as u64,
        inode: root_before.st_ino as u64,
    }];
    let mut logical_bytes = 0_u64;
    let mut allocated_bytes = 0_u64;
    let mut file_count = 0_u64;
    let mut directory_count = 1_u64;
    for relative_path in &normalized {
        validate_relative_witness_path(relative_path)?;
        let metadata = stat_anchored_witness_path(allocation, upper, relative_path)?;
        let file_type = raw_mode_file_type(metadata.st_mode as rustix::fs::RawMode);
        if file_type == FileType::RegularFile {
            logical_bytes = logical_bytes.saturating_add(metadata.st_size as u64);
            file_count = file_count.saturating_add(1);
        }
        allocated_bytes =
            allocated_bytes.saturating_add((metadata.st_blocks as u64).saturating_mul(512));
        directory_count =
            directory_count.saturating_add(u64::from(file_type == FileType::Directory));
        if representative_inodes.len() < 16 {
            representative_inodes.push(InodeWitness {
                relative_path: relative_path.clone(),
                device: metadata.st_dev as u64,
                inode: metadata.st_ino as u64,
            });
        }
    }

    let root_after = anchored_fstat(
        upper,
        &allocation.upper_dir,
        "revalidate pinned allocation upper after receipt witness",
    )?;
    require_stable_anchored_stat(
        &root_before,
        &root_after,
        &allocation.upper_dir,
        "allocation upper changed during receipt witness",
    )?;
    Ok(PhysicalSnapshot {
        allocation_id: allocation.descriptor.allocation_id.clone(),
        allocation_path: allocation.allocation_root.clone(),
        device: root_before.st_dev as u64,
        representative_inodes,
        logical_bytes,
        allocated_bytes,
        inode_count: u64::try_from(normalized.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1),
        file_count,
        directory_count,
    })
}

#[cfg(target_os = "linux")]
struct PinnedWitnessDirectory {
    parent: OwnedFd,
    name: OsString,
    directory: OwnedFd,
    stat: Stat,
    display_path: PathBuf,
}

#[cfg(target_os = "linux")]
fn stat_anchored_witness_path(
    allocation: &AllocationHandle,
    upper: &OwnedFd,
    relative_path: &Path,
) -> PocResult<Stat> {
    let mut current = rustix::io::dup(upper).map_err(|error| {
        PocError::io(
            "duplicate pinned allocation upper for receipt witness",
            &allocation.upper_dir,
            std::io::Error::from(error),
        )
    })?;
    let components = relative_path.components().collect::<Vec<_>>();
    let mut pinned: Vec<PinnedWitnessDirectory> =
        Vec::with_capacity(components.len().saturating_sub(1));
    let mut traversed = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        let name = match component {
            Component::Normal(name) => *name,
            _ => {
                return Err(PocError::Integrity(format!(
                    "invalid affected receipt path {}",
                    relative_path.display()
                )))
            }
        };
        traversed.push(name);
        let display_path = allocation.upper_dir.join(&traversed);
        let before = anchored_statat(
            &current,
            name,
            &display_path,
            "stat pinned affected receipt path",
        )?;
        let file_type = raw_mode_file_type(before.st_mode as rustix::fs::RawMode);
        if file_type == FileType::Symlink {
            return Err(PocError::Integrity(format!(
                "receipt-hit witness does not accept symlink component {}",
                display_path.display()
            )));
        }
        let is_final = index + 1 == components.len();
        if !is_final && file_type != FileType::Directory {
            return Err(PocError::Integrity(format!(
                "receipt-hit witness component is not a directory: {}",
                display_path.display()
            )));
        }
        let flags = if is_final {
            anchored_open_flags(file_type)
        } else {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        };
        let opened = rustix::fs::openat(&current, name, flags, Mode::empty()).map_err(|error| {
            PocError::io(
                "open pinned affected receipt path",
                &display_path,
                std::io::Error::from(error),
            )
        })?;
        let opened_stat =
            anchored_fstat(&opened, &display_path, "stat opened affected receipt path")?;
        require_same_anchored_identity(
            &before,
            &opened_stat,
            &display_path,
            "affected receipt path changed while it was opened",
        )?;

        if is_final {
            let opened_after = anchored_fstat(
                &opened,
                &display_path,
                "revalidate opened affected receipt path",
            )?;
            let named_after = anchored_statat(
                &current,
                name,
                &display_path,
                "revalidate affected receipt path",
            )?;
            require_stable_anchored_stat(
                &opened_stat,
                &opened_after,
                &display_path,
                "affected receipt path changed while it was read",
            )?;
            require_stable_anchored_stat(
                &opened_stat,
                &named_after,
                &display_path,
                "affected receipt path was replaced while it was read",
            )?;
            for component in pinned.iter().rev() {
                let pinned_after = anchored_fstat(
                    &component.directory,
                    &component.display_path,
                    "revalidate opened receipt witness directory",
                )?;
                let named_after = anchored_statat(
                    &component.parent,
                    &component.name,
                    &component.display_path,
                    "revalidate receipt witness directory",
                )?;
                require_stable_anchored_stat(
                    &component.stat,
                    &pinned_after,
                    &component.display_path,
                    "receipt witness directory changed during traversal",
                )?;
                require_stable_anchored_stat(
                    &component.stat,
                    &named_after,
                    &component.display_path,
                    "receipt witness directory was replaced during traversal",
                )?;
            }
            return Ok(opened_stat);
        }

        let next = rustix::io::dup(&opened).map_err(|error| {
            PocError::io(
                "duplicate pinned receipt witness directory",
                &display_path,
                std::io::Error::from(error),
            )
        })?;
        pinned.push(PinnedWitnessDirectory {
            parent: current,
            name: name.to_os_string(),
            directory: opened,
            stat: opened_stat,
            display_path,
        });
        current = next;
    }
    Err(PocError::Integrity(format!(
        "invalid affected receipt path {}",
        relative_path.display()
    )))
}

fn validate_relative_witness_path(path: &Path) -> PocResult<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PocError::Integrity(format!(
            "invalid affected receipt path {}",
            path.display()
        )));
    }
    Ok(())
}

fn walk_no_follow(
    root: &Path,
    directory: &Path,
    output: &mut Vec<InventoryEntry>,
    include_content_sha256: bool,
) -> PocResult<()> {
    let mut children = fs::read_dir(directory)
        .map_err(|error| PocError::io("read inventory directory", directory, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| PocError::io("read inventory entry", directory, error))?;
    children.sort_by_key(|entry| raw_os_bytes(entry.file_name()));
    for child in children {
        let path = child.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| PocError::io("stat inventory entry", &path, error))?;
        let relative_path = path
            .strip_prefix(root)
            .map_err(|error| {
                PocError::Integrity(format!(
                    "inventory path {} escaped root {}: {error}",
                    path.display(),
                    root.display()
                ))
            })?
            .to_path_buf();
        let kind = entry_kind(&metadata);
        let symlink_target = if metadata.file_type().is_symlink() {
            Some(
                fs::read_link(&path)
                    .map_err(|error| PocError::io("read inventory symlink", &path, error))?,
            )
        } else {
            None
        };
        let content_sha256 = if include_content_sha256 && metadata.is_file() {
            Some(hash_file(&path)?)
        } else {
            None
        };
        output.push(inventory_entry(
            &path,
            relative_path,
            kind,
            &metadata,
            symlink_target,
            content_sha256,
        )?);
        if metadata.is_dir() {
            walk_no_follow(root, &path, output, include_content_sha256)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn anchored_open_flags(file_type: FileType) -> OFlags {
    match file_type {
        FileType::Directory => {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        }
        FileType::RegularFile => OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        _ => OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
    }
}

#[cfg(target_os = "linux")]
fn raw_mode_file_type(mode: rustix::fs::RawMode) -> FileType {
    FileType::from_raw_mode(mode)
}

#[cfg(target_os = "linux")]
fn anchored_fstat(
    descriptor: &OwnedFd,
    display_path: &Path,
    operation: &'static str,
) -> PocResult<Stat> {
    rustix::fs::fstat(descriptor)
        .map_err(|error| PocError::io(operation, display_path, std::io::Error::from(error)))
}

#[cfg(target_os = "linux")]
fn anchored_statat(
    directory: &OwnedFd,
    name: &OsStr,
    display_path: &Path,
    operation: &'static str,
) -> PocResult<Stat> {
    rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| PocError::io(operation, display_path, std::io::Error::from(error)))
}

#[cfg(target_os = "linux")]
fn require_same_anchored_identity(
    expected: &Stat,
    observed: &Stat,
    display_path: &Path,
    message: &str,
) -> PocResult<()> {
    if expected.st_dev != observed.st_dev
        || expected.st_ino != observed.st_ino
        || raw_mode_file_type(expected.st_mode as rustix::fs::RawMode)
            != raw_mode_file_type(observed.st_mode as rustix::fs::RawMode)
    {
        return Err(PocError::RecoveryRequired(format!(
            "{message}: {}",
            display_path.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_stable_anchored_stat(
    expected: &Stat,
    observed: &Stat,
    display_path: &Path,
    message: &str,
) -> PocResult<()> {
    if expected.st_dev != observed.st_dev
        || expected.st_ino != observed.st_ino
        || expected.st_mode != observed.st_mode
        || expected.st_uid != observed.st_uid
        || expected.st_gid != observed.st_gid
        || expected.st_nlink != observed.st_nlink
        || expected.st_rdev != observed.st_rdev
        || expected.st_size != observed.st_size
        || expected.st_blocks != observed.st_blocks
        || expected.mtime() != observed.mtime()
        || expected.st_mtime_nsec != observed.st_mtime_nsec
        || expected.ctime() != observed.ctime()
        || expected.st_ctime_nsec != observed.st_ctime_nsec
    {
        return Err(PocError::RecoveryRequired(format!(
            "{message}: {}",
            display_path.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn inventory_entry_from_stat(
    relative_path: PathBuf,
    file_type: FileType,
    metadata: &Stat,
    symlink_target: Option<PathBuf>,
    content_sha256: Option<String>,
    xattrs_sha256: String,
    display_path: &Path,
) -> PocResult<InventoryEntry> {
    let modified_ns = i128::from(metadata.mtime())
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(i128::from(metadata.st_mtime_nsec)))
        .ok_or_else(|| {
            PocError::Integrity(format!(
                "mtime overflow while inventorying {}",
                display_path.display()
            ))
        })?;
    Ok(InventoryEntry {
        relative_path,
        kind: inventory_kind_from_file_type(file_type),
        mode: metadata.st_mode as u32,
        uid: metadata.st_uid,
        gid: metadata.st_gid,
        size: metadata.st_size as u64,
        allocated_bytes: (metadata.st_blocks as u64).saturating_mul(512),
        modified_ns,
        device: metadata.st_dev as u64,
        inode: metadata.st_ino as u64,
        link_count: metadata.st_nlink as u64,
        device_number: metadata.st_rdev as u64,
        symlink_target,
        content_sha256,
        xattrs_sha256,
    })
}

#[cfg(target_os = "linux")]
fn inventory_kind_from_file_type(file_type: FileType) -> InventoryEntryKind {
    match file_type {
        FileType::Directory => InventoryEntryKind::Directory,
        FileType::RegularFile => InventoryEntryKind::Regular,
        FileType::Symlink => InventoryEntryKind::Symlink,
        FileType::BlockDevice => InventoryEntryKind::BlockDevice,
        FileType::CharacterDevice => InventoryEntryKind::CharacterDevice,
        FileType::Fifo => InventoryEntryKind::Fifo,
        FileType::Socket => InventoryEntryKind::Socket,
        FileType::Unknown => InventoryEntryKind::Other,
    }
}

#[cfg(unix)]
fn inventory_entry(
    path: &Path,
    relative_path: PathBuf,
    kind: InventoryEntryKind,
    metadata: &fs::Metadata,
    symlink_target: Option<PathBuf>,
    content_sha256: Option<String>,
) -> PocResult<InventoryEntry> {
    let modified_ns = i128::from(metadata.mtime())
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(i128::from(metadata.mtime_nsec())))
        .ok_or_else(|| {
            PocError::Integrity(format!(
                "mtime overflow while inventorying {}",
                path.display()
            ))
        })?;
    Ok(InventoryEntry {
        relative_path,
        kind,
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        size: metadata.size(),
        allocated_bytes: metadata.blocks().saturating_mul(512),
        modified_ns,
        device: metadata.dev(),
        inode: metadata.ino(),
        link_count: metadata.nlink(),
        device_number: metadata.rdev(),
        symlink_target,
        content_sha256,
        xattrs_sha256: hash_xattrs(path)?,
    })
}

#[cfg(not(unix))]
fn inventory_entry(
    path: &Path,
    relative_path: PathBuf,
    kind: InventoryEntryKind,
    metadata: &fs::Metadata,
    symlink_target: Option<PathBuf>,
    content_sha256: Option<String>,
) -> PocResult<InventoryEntry> {
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i128::try_from(duration.as_nanos()).ok())
        .unwrap_or_default();
    Ok(InventoryEntry {
        relative_path,
        kind,
        mode: 0,
        uid: 0,
        gid: 0,
        size: metadata.len(),
        allocated_bytes: metadata.len(),
        modified_ns,
        device: 0,
        inode: 0,
        link_count: 0,
        device_number: 0,
        symlink_target,
        content_sha256,
        xattrs_sha256: hash_xattrs(path)?,
    })
}

#[cfg(unix)]
fn entry_kind(metadata: &fs::Metadata) -> InventoryEntryKind {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        InventoryEntryKind::Directory
    } else if file_type.is_file() {
        InventoryEntryKind::Regular
    } else if file_type.is_symlink() {
        InventoryEntryKind::Symlink
    } else if file_type.is_block_device() {
        InventoryEntryKind::BlockDevice
    } else if file_type.is_char_device() {
        InventoryEntryKind::CharacterDevice
    } else if file_type.is_fifo() {
        InventoryEntryKind::Fifo
    } else if file_type.is_socket() {
        InventoryEntryKind::Socket
    } else {
        InventoryEntryKind::Other
    }
}

#[cfg(not(unix))]
fn entry_kind(metadata: &fs::Metadata) -> InventoryEntryKind {
    if metadata.is_dir() {
        InventoryEntryKind::Directory
    } else if metadata.is_file() {
        InventoryEntryKind::Regular
    } else if metadata.file_type().is_symlink() {
        InventoryEntryKind::Symlink
    } else {
        InventoryEntryKind::Other
    }
}

fn hash_file(path: &Path) -> PocResult<String> {
    let file =
        File::open(path).map_err(|error| PocError::io("open inventory file", path, error))?;
    hash_open_file(file, path)
}

fn hash_open_file(file: File, display_path: &Path) -> PocResult<String> {
    let mut reader = BufReader::with_capacity(32 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 32 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| PocError::io("hash inventory file", display_path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize()))
}

#[cfg(target_os = "linux")]
fn hash_file_descriptor(descriptor: &OwnedFd, display_path: &Path) -> PocResult<String> {
    let duplicate = rustix::io::dup(descriptor).map_err(|error| {
        PocError::io(
            "duplicate pinned inventory file",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    hash_open_file(File::from(duplicate), display_path)
}

#[cfg(target_os = "linux")]
fn hash_xattrs_descriptor(descriptor: &OwnedFd, display_path: &Path) -> PocResult<String> {
    let list_size = rustix::fs::flistxattr(descriptor, &mut []).map_err(|error| {
        PocError::io(
            "size pinned inventory xattrs",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let mut list = vec![0_u8; list_size];
    let listed = rustix::fs::flistxattr(descriptor, &mut list).map_err(|error| {
        PocError::io(
            "list pinned inventory xattrs",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    list.truncate(listed);

    let mut names = list
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| OsString::from_vec(name.to_vec()))
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.as_bytes().to_vec());

    let mut hasher = Sha256::new();
    for name in names {
        let value_size = rustix::fs::fgetxattr(descriptor, &name, &mut []).map_err(|error| {
            PocError::io(
                "size pinned inventory xattr",
                display_path,
                std::io::Error::from(error),
            )
        })?;
        let mut value = vec![0_u8; value_size];
        let read = rustix::fs::fgetxattr(descriptor, &name, &mut value).map_err(|error| {
            PocError::io(
                "read pinned inventory xattr",
                display_path,
                std::io::Error::from(error),
            )
        })?;
        value.truncate(read);
        hasher.update((name.as_bytes().len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update((value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    Ok(hex_digest(hasher.finalize()))
}

#[cfg(target_os = "linux")]
fn hash_xattrs(path: &Path) -> PocResult<String> {
    let list_size = rustix::fs::llistxattr(path, &mut [])
        .map_err(|error| PocError::io("size inventory xattrs", path, error.into()))?;
    let mut list = vec![0_u8; list_size];
    let listed = rustix::fs::llistxattr(path, &mut list)
        .map_err(|error| PocError::io("list inventory xattrs", path, error.into()))?;
    list.truncate(listed);

    let mut names = list
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| OsString::from_vec(name.to_vec()))
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.as_bytes().to_vec());

    let mut hasher = Sha256::new();
    for name in names {
        let value_size = rustix::fs::lgetxattr(path, &name, &mut [])
            .map_err(|error| PocError::io("size inventory xattr", path, error.into()))?;
        let mut value = vec![0_u8; value_size];
        let read = rustix::fs::lgetxattr(path, &name, &mut value)
            .map_err(|error| PocError::io("read inventory xattr", path, error.into()))?;
        value.truncate(read);
        hasher.update((name.as_bytes().len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update((value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    Ok(hex_digest(hasher.finalize()))
}

#[cfg(not(target_os = "linux"))]
fn hash_xattrs(_path: &Path) -> PocResult<String> {
    Ok(hex_digest(Sha256::digest([])))
}

fn digest_entries(entries: &[InventoryEntry]) -> PocResult<String> {
    let encoded = serde_json::to_vec(entries)?;
    Ok(hex_digest(Sha256::digest(encoded)))
}

fn summarize_physical(
    allocation: &AllocationHandle,
    upper_dir: &Path,
    entries: &[InventoryEntry],
) -> PocResult<PhysicalSnapshot> {
    let root_metadata = fs::metadata(upper_dir).map_err(|error| {
        PocError::io(
            "stat allocation upper for physical snapshot",
            upper_dir,
            error,
        )
    })?;
    summarize_physical_with_device(allocation, metadata_device(&root_metadata), entries)
}

fn summarize_physical_with_device(
    allocation: &AllocationHandle,
    device: u64,
    entries: &[InventoryEntry],
) -> PocResult<PhysicalSnapshot> {
    let representative_inodes = entries
        .iter()
        .take(16)
        .map(|entry| InodeWitness {
            relative_path: entry.relative_path.clone(),
            device: entry.device,
            inode: entry.inode,
        })
        .collect();
    Ok(PhysicalSnapshot {
        allocation_id: allocation.descriptor.allocation_id.clone(),
        allocation_path: allocation.allocation_root.clone(),
        device,
        representative_inodes,
        logical_bytes: entries
            .iter()
            .filter(|entry| entry.kind == InventoryEntryKind::Regular)
            .map(|entry| entry.size)
            .sum(),
        allocated_bytes: entries.iter().map(|entry| entry.allocated_bytes).sum(),
        inode_count: u64::try_from(entries.len())
            .map_err(|_| PocError::Integrity("inventory count does not fit u64".to_owned()))?,
        file_count: entries
            .iter()
            .filter(|entry| entry.kind == InventoryEntryKind::Regular)
            .count()
            .try_into()
            .map_err(|_| PocError::Integrity("file count does not fit u64".to_owned()))?,
        directory_count: entries
            .iter()
            .filter(|entry| entry.kind == InventoryEntryKind::Directory)
            .count()
            .try_into()
            .map_err(|_| PocError::Integrity("directory count does not fit u64".to_owned()))?,
    })
}

#[cfg(target_os = "linux")]
fn descriptor_path(descriptor: &OwnedFd) -> PathBuf {
    PathBuf::from("/proc/self/fd").join(descriptor.as_raw_fd().to_string())
}

#[cfg(unix)]
fn metadata_device(metadata: &fs::Metadata) -> u64 {
    metadata.dev()
}

#[cfg(not(unix))]
const fn metadata_device(_metadata: &fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn metadata_inode(metadata: &fs::Metadata) -> u64 {
    metadata.ino()
}

#[cfg(not(unix))]
const fn metadata_inode(_metadata: &fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn metadata_allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn metadata_allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(unix)]
fn raw_path_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn raw_path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn raw_os_bytes(value: OsString) -> Vec<u8> {
    value.into_vec()
}

#[cfg(not(unix))]
fn raw_os_bytes(value: OsString) -> Vec<u8> {
    value.to_string_lossy().into_owned().into_bytes()
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
