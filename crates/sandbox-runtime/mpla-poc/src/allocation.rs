use std::fs::OpenOptions;
use std::path::Path;

use uuid::Uuid;

use crate::durable::{fsync_dir, read_json, write_immutable_json};
use crate::{
    AllocationDescriptor, AllocationHandle, AllocationId, DeletionCapability, OperationId,
    OwnerSubject, PocError, PocResult, SCHEMA_VERSION,
};

const DESCRIPTOR_FILE: &str = "ALLOCATION.json";
const OWNER_DIRECTORY: &str = "owner";
const OWNER_LOCK_FILE: &str = "LOCK";
const OWNER_JOURNAL_FILE: &str = "journal.bin";

pub fn create_allocation(
    arena_root: &Path,
    operation_id: &OperationId,
) -> PocResult<AllocationHandle> {
    prepare_arena(arena_root)?;
    for _ in 0..16 {
        let allocation_id = AllocationId::new();
        let prefix = allocation_prefix(&allocation_id)?;
        let prefix_root = arena_root.join(prefix);
        create_directory_if_missing(&prefix_root, arena_root)?;
        let allocation_root = prefix_root.join(allocation_id.as_str());
        match std::fs::create_dir(&allocation_root) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(PocError::io(
                    "create permanent allocation",
                    &allocation_root,
                    source,
                ));
            }
        }
        fsync_dir(&prefix_root)?;
        let descriptor = AllocationDescriptor {
            schema_version: SCHEMA_VERSION,
            allocation_id: allocation_id.clone(),
            created_by_operation: operation_id.clone(),
            created_unix_ms: crate::unix_time_ms()?,
        };
        if let Err(error) = initialize_allocation(&allocation_root, &descriptor) {
            let _ = std::fs::remove_dir_all(&allocation_root);
            let _ = fsync_dir(&prefix_root);
            return Err(error);
        }
        return open_allocation(arena_root, &allocation_id);
    }
    Err(PocError::Integrity(
        "failed to allocate a unique random AllocationId".to_owned(),
    ))
}

pub fn open_allocation(
    arena_root: &Path,
    allocation_id: &AllocationId,
) -> PocResult<AllocationHandle> {
    let prefix = allocation_prefix(allocation_id)?;
    let allocation_root = arena_root.join(prefix).join(allocation_id.as_str());
    require_directory(&allocation_root, "allocation root")?;
    let descriptor_path = allocation_root.join(DESCRIPTOR_FILE);
    require_regular_file(&descriptor_path, "allocation descriptor")?;
    let descriptor: AllocationDescriptor = read_json(&descriptor_path)?;
    if descriptor.schema_version != SCHEMA_VERSION || descriptor.allocation_id != *allocation_id {
        return Err(PocError::Integrity(format!(
            "allocation descriptor mismatch at {}",
            descriptor_path.display()
        )));
    }

    let upper_dir = allocation_root.join("upper");
    let work_dir = allocation_root.join("work");
    let owner_dir = allocation_root.join(OWNER_DIRECTORY);
    require_directory(&upper_dir, "allocation upper")?;
    require_directory(&work_dir, "allocation work")?;
    require_directory(&owner_dir, "allocation owner metadata")?;
    require_directory(&owner_dir.join("generations"), "owner generations")?;
    require_directory(&owner_dir.join("receipts"), "owner receipts")?;
    require_regular_file(&owner_dir.join(OWNER_LOCK_FILE), "owner lock")?;
    require_regular_file(&owner_dir.join(OWNER_JOURNAL_FILE), "owner journal")?;
    require_same_filesystem(&upper_dir, &work_dir)?;

    Ok(AllocationHandle {
        descriptor,
        allocation_root,
        upper_dir,
        work_dir,
        owner_dir,
    })
}

pub fn destroy_workspace_allocation(
    arena_root: &Path,
    allocation_id: &AllocationId,
    capability: &DeletionCapability,
) -> PocResult<()> {
    let allocation = open_allocation(arena_root, allocation_id)?;
    let expected_root = arena_root
        .join(allocation_prefix(allocation_id)?)
        .join(allocation_id.as_str());
    if allocation.allocation_root != expected_root || capability.allocation_id != *allocation_id {
        return Err(PocError::Integrity(
            "allocation deletion target does not match its capability".to_owned(),
        ));
    }

    let _lock =
        crate::durable::FileLock::exclusive(&crate::owner::owner_lock_path(&expected_root))?;
    crate::lease::validate_deleter_locked(&expected_root, capability)?;
    let owner = crate::owner::selected_owner_locked(&expected_root)?.ok_or_else(|| {
        PocError::RecoveryRequired("allocation deletion target has no selected owner".to_owned())
    })?;
    if !matches!(
        owner.subject,
        OwnerSubject::WorkspaceOwned {
            ref session_id,
            lease_epoch,
        } if session_id == &capability.session_id && lease_epoch == capability.lease_epoch
    ) {
        return Err(PocError::OwnerConflict(
            "only the exact workspace owner may delete an allocation".to_owned(),
        ));
    }
    reject_live_mount_reference(&expected_root)?;

    std::fs::remove_dir_all(&expected_root)
        .map_err(|source| PocError::io("delete workspace allocation", &expected_root, source))?;
    let prefix_root = expected_root
        .parent()
        .ok_or_else(|| PocError::Integrity("allocation has no prefix directory".to_owned()))?;
    fsync_dir(prefix_root)?;
    match std::fs::remove_dir(prefix_root) {
        Ok(()) => fsync_dir(arena_root),
        Err(source) if source.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(source) => Err(PocError::io(
            "remove empty allocation prefix",
            prefix_root,
            source,
        )),
    }
}

#[cfg(target_os = "linux")]
fn reject_live_mount_reference(allocation_root: &Path) -> PocResult<()> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|source| PocError::io("read process mountinfo", "/proc/self/mountinfo", source))?;
    let allocation_path = allocation_root.to_str().ok_or_else(|| {
        PocError::Integrity("allocation path is not valid UTF-8 for mount audit".to_owned())
    })?;
    if mountinfo.contains(allocation_path) {
        return Err(PocError::OwnerConflict(format!(
            "allocation is still referenced by a live mount: {}",
            allocation_root.display()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn reject_live_mount_reference(_allocation_root: &Path) -> PocResult<()> {
    Ok(())
}

fn initialize_allocation(
    allocation_root: &Path,
    descriptor: &AllocationDescriptor,
) -> PocResult<()> {
    for directory in ["upper", "work", OWNER_DIRECTORY] {
        let path = allocation_root.join(directory);
        std::fs::create_dir(&path)
            .map_err(|source| PocError::io("create allocation directory", &path, source))?;
        fsync_dir(allocation_root)?;
    }
    let owner_dir = allocation_root.join(OWNER_DIRECTORY);
    for directory in ["generations", "receipts"] {
        let path = owner_dir.join(directory);
        std::fs::create_dir(&path)
            .map_err(|source| PocError::io("create owner metadata directory", &path, source))?;
        fsync_dir(&owner_dir)?;
    }
    for file_name in [OWNER_LOCK_FILE, OWNER_JOURNAL_FILE] {
        let path = owner_dir.join(file_name);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|source| PocError::io("create owner metadata file", &path, source))?;
        file.sync_all()
            .map_err(|source| PocError::io("fsync owner metadata file", &path, source))?;
        fsync_dir(&owner_dir)?;
    }
    write_immutable_json(&allocation_root.join(DESCRIPTOR_FILE), descriptor)?;
    fsync_dir(allocation_root)
}

fn prepare_arena(arena_root: &Path) -> PocResult<()> {
    std::fs::create_dir_all(arena_root)
        .map_err(|source| PocError::io("create allocation arena", arena_root, source))?;
    require_directory(arena_root, "allocation arena")?;
    fsync_dir(arena_root)?;
    if let Some(parent) = arena_root.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

fn create_directory_if_missing(path: &Path, parent: &Path) -> PocResult<()> {
    match std::fs::create_dir(path) {
        Ok(()) => fsync_dir(parent),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            require_directory(path, "allocation prefix")
        }
        Err(source) => Err(PocError::io("create allocation prefix", path, source)),
    }
}

fn allocation_prefix(allocation_id: &AllocationId) -> PocResult<&str> {
    validate_allocation_id(allocation_id)?;
    allocation_id
        .as_str()
        .get(..2)
        .ok_or_else(|| PocError::Integrity(format!("AllocationId is too short: {allocation_id}")))
}

fn validate_allocation_id(allocation_id: &AllocationId) -> PocResult<()> {
    let parsed = Uuid::parse_str(allocation_id.as_str())
        .map_err(|_| PocError::Integrity(format!("invalid AllocationId: {allocation_id}")))?;
    if parsed.hyphenated().to_string() != allocation_id.as_str() {
        return Err(PocError::Integrity(format!(
            "non-canonical AllocationId: {allocation_id}"
        )));
    }
    Ok(())
}

fn require_directory(path: &Path, label: &str) -> PocResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat allocation directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PocError::Integrity(format!(
            "{label} is not a real directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_regular_file(path: &Path, label: &str) -> PocResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat allocation file", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PocError::Integrity(format!(
            "{label} is not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn require_same_filesystem(left: &Path, right: &Path) -> PocResult<()> {
    use std::os::unix::fs::MetadataExt;

    let left_device = std::fs::metadata(left)
        .map_err(|source| PocError::io("stat allocation upper", left, source))?
        .dev();
    let right_device = std::fs::metadata(right)
        .map_err(|source| PocError::io("stat allocation work", right, source))?
        .dev();
    if left_device != right_device {
        return Err(PocError::Integrity(format!(
            "allocation upper and work are on different filesystems: {} and {}",
            left.display(),
            right.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_same_filesystem(_left: &Path, _right: &Path) -> PocResult<()> {
    Ok(())
}
