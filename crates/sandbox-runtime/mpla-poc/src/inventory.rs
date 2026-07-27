use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::thread;

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
    let mut entries = Vec::new();
    walk_no_follow(&allocation.upper_dir, &allocation.upper_dir, &mut entries)?;
    entries.sort_by(|left, right| {
        raw_path_bytes(&left.relative_path).cmp(&raw_path_bytes(&right.relative_path))
    });

    let inventory_sha256 = digest_entries(&entries)?;
    let physical = summarize_physical(allocation, &entries)?;
    Ok(AllocationInventory {
        schema_version: crate::SCHEMA_VERSION,
        allocation_id: allocation.descriptor.allocation_id.clone(),
        allocation_root: allocation.allocation_root.clone(),
        inventory_sha256,
        entries,
        physical,
    })
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

fn walk_no_follow(
    root: &Path,
    directory: &Path,
    output: &mut Vec<InventoryEntry>,
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
        let content_sha256 = if metadata.is_file() {
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
            walk_no_follow(root, &path, output)?;
        }
    }
    Ok(())
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
    let mut reader = BufReader::with_capacity(32 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 32 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| PocError::io("hash inventory file", path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
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
    entries: &[InventoryEntry],
) -> PocResult<PhysicalSnapshot> {
    let root_metadata = fs::metadata(&allocation.upper_dir).map_err(|error| {
        PocError::io(
            "stat allocation upper for physical snapshot",
            &allocation.upper_dir,
            error,
        )
    })?;
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
        device: metadata_device(&root_metadata),
        representative_inodes,
        logical_bytes: entries.iter().map(|entry| entry.size).sum(),
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

#[cfg(unix)]
fn metadata_device(metadata: &fs::Metadata) -> u64 {
    metadata.dev()
}

#[cfg(not(unix))]
const fn metadata_device(_metadata: &fs::Metadata) -> u64 {
    0
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
