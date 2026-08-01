use std::fs::OpenOptions;
use std::path::Path;

#[cfg(target_os = "linux")]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "linux")]
use std::ffi::{OsStr, OsString};
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use rustix::fs::{AtFlags, FlockOperation, ResolveFlags};

use uuid::Uuid;

use crate::durable::{fsync_dir, read_json, write_immutable_json};
#[cfg(target_os = "linux")]
use crate::OwnerSubject;
use crate::{
    AllocationDescriptor, AllocationHandle, AllocationId, DeletionCapability, OperationId,
    PocError, PocResult, SCHEMA_VERSION,
};

const DESCRIPTOR_FILE: &str = "ALLOCATION.json";
const OWNER_DIRECTORY: &str = "owner";
const OWNER_LOCK_FILE: &str = "LOCK";
const OWNER_JOURNAL_FILE: &str = "journal.bin";
#[cfg(target_os = "linux")]
const MAX_ALLOCATION_DESCRIPTOR_BYTES: u64 = 1024 * 1024;

#[cfg(target_os = "linux")]
struct PinnedDeletion {
    arena: OwnedFd,
    prefix: OwnedFd,
    allocation: OwnedFd,
    upper: OwnedFd,
    work: OwnedFd,
    owner: OwnedFd,
    prefix_name: String,
    allocation_name: String,
    prefix_path: PathBuf,
    allocation_path: PathBuf,
    upper_path: PathBuf,
    work_path: PathBuf,
    owner_path: PathBuf,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ObjectIdentity {
    device: u64,
    inode: u64,
    file_type: u32,
}

#[cfg(target_os = "linux")]
struct MountReferenceTarget {
    directories: BTreeSet<DirectoryIdentity>,
    objects: BTreeSet<ObjectIdentity>,
    device: u64,
    filesystem_path: PathBuf,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
struct AllocationMountInfo {
    mount_id: u64,
    target_device: u64,
    root: PathBuf,
    mountpoint: PathBuf,
    filesystem_type: String,
    super_options: Vec<String>,
}

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
        fsync_dir(&prefix_root)?;
        return Ok(AllocationHandle {
            descriptor,
            upper_dir: allocation_root.join("upper"),
            work_dir: allocation_root.join("work"),
            owner_dir: allocation_root.join(OWNER_DIRECTORY),
            allocation_root,
        });
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
    if capability.allocation_id != *allocation_id {
        return Err(PocError::Integrity(
            "allocation deletion target does not match its capability".to_owned(),
        ));
    }

    destroy_workspace_allocation_anchored(arena_root, allocation_id, capability)
}

#[cfg(target_os = "linux")]
fn destroy_workspace_allocation_anchored(
    arena_root: &Path,
    allocation_id: &AllocationId,
    capability: &DeletionCapability,
) -> PocResult<()> {
    let pinned = pin_allocation_for_deletion(arena_root, allocation_id)?;
    let owner_lock = lock_pinned_owner(&pinned)?;
    let anchored_root =
        PathBuf::from("/proc/self/fd").join(pinned.allocation.as_raw_fd().to_string());
    let owner = crate::owner::with_pinned_owner_directory(&anchored_root, &pinned.owner, || {
        crate::lease::validate_deleter_locked(&anchored_root, capability)?;
        crate::owner::selected_owner_locked(&anchored_root)?.ok_or_else(|| {
            PocError::RecoveryRequired(
                "allocation deletion target has no selected owner".to_owned(),
            )
        })
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

    revalidate_pinned_deletion(&pinned)?;
    remove_anchored_directory_tree(
        &pinned,
        owner_lock.as_raw_fd(),
        &pinned.prefix,
        OsStr::new(&pinned.allocation_name),
        &pinned.allocation,
        &pinned.allocation_path,
    )
}

#[cfg(not(target_os = "linux"))]
fn destroy_workspace_allocation_anchored(
    _arena_root: &Path,
    _allocation_id: &AllocationId,
    _capability: &DeletionCapability,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "descriptor-anchored allocation deletion requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn pin_allocation_for_deletion(
    arena_root: &Path,
    allocation_id: &AllocationId,
) -> PocResult<PinnedDeletion> {
    let prefix_name = allocation_prefix(allocation_id)?.to_owned();
    let allocation_name = allocation_id.as_str().to_owned();
    let prefix_path = arena_root.join(&prefix_name);
    let allocation_path = prefix_path.join(&allocation_name);
    let upper_path = allocation_path.join("upper");
    let work_path = allocation_path.join("work");
    let owner_path = allocation_path.join(OWNER_DIRECTORY);
    let arena = open_directory_no_symlink("allocation arena", arena_root)?;
    let prefix = open_child_directory_no_symlink(
        "allocation prefix",
        &arena,
        OsStr::new(&prefix_name),
        &prefix_path,
    )?;
    let allocation = open_child_directory_no_symlink(
        "allocation deletion target",
        &prefix,
        OsStr::new(&allocation_name),
        &allocation_path,
    )?;
    let upper = open_child_directory_no_symlink(
        "allocation upper",
        &allocation,
        OsStr::new("upper"),
        &upper_path,
    )?;
    let work = open_child_directory_no_symlink(
        "allocation work",
        &allocation,
        OsStr::new("work"),
        &work_path,
    )?;
    let owner = open_child_directory_no_symlink(
        "allocation owner metadata",
        &allocation,
        OsStr::new(OWNER_DIRECTORY),
        &owner_path,
    )?;
    let descriptor = read_descriptor_at(&allocation, &allocation_path.join(DESCRIPTOR_FILE))?;
    if descriptor.schema_version != SCHEMA_VERSION || descriptor.allocation_id != *allocation_id {
        return Err(PocError::RecoveryRequired(format!(
            "allocation descriptor does not match deletion target at {}",
            allocation_path.display()
        )));
    }
    let pinned = PinnedDeletion {
        arena,
        prefix,
        allocation,
        upper,
        work,
        owner,
        prefix_name,
        allocation_name,
        prefix_path,
        allocation_path,
        upper_path,
        work_path,
        owner_path,
    };
    revalidate_pinned_deletion(&pinned)?;
    let arena_mount = crate::overlay_adapter::mount_id_for_fd(&pinned.arena)?;
    if crate::overlay_adapter::mount_id_for_fd(&pinned.prefix)? != arena_mount
        || crate::overlay_adapter::mount_id_for_fd(&pinned.allocation)? != arena_mount
        || crate::overlay_adapter::mount_id_for_fd(&pinned.upper)? != arena_mount
        || crate::overlay_adapter::mount_id_for_fd(&pinned.work)? != arena_mount
        || crate::overlay_adapter::mount_id_for_fd(&pinned.owner)? != arena_mount
    {
        return Err(PocError::RecoveryRequired(format!(
            "allocation deletion target crosses a mount at {}",
            pinned.allocation_path.display()
        )));
    }
    Ok(pinned)
}

#[cfg(target_os = "linux")]
fn open_directory_no_symlink(label: &str, path: &Path) -> PocResult<OwnedFd> {
    if !path.is_absolute() {
        return Err(PocError::RecoveryRequired(format!(
            "{label} must be an absolute no-symlink path: {}",
            path.display()
        )));
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut current =
        rustix::fs::open(Path::new("/"), flags, rustix::fs::Mode::empty()).map_err(|error| {
            PocError::io(
                "open allocation deletion root",
                Path::new("/"),
                std::io::Error::from(error),
            )
        })?;
    for component in path.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(component) => {
                current = rustix::fs::openat(&current, component, flags, rustix::fs::Mode::empty())
                    .map_err(|error| {
                        PocError::io(
                            "open anchored allocation directory",
                            path,
                            std::io::Error::from(error),
                        )
                    })?;
            }
            _ => {
                return Err(PocError::RecoveryRequired(format!(
                    "{label} is not a normalized absolute path: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(current)
}

#[cfg(target_os = "linux")]
fn open_child_directory_no_symlink(
    label: &str,
    parent: &OwnedFd,
    name: &OsStr,
    display_path: &Path,
) -> PocResult<OwnedFd> {
    require_single_component(label, name)?;
    rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open anchored allocation child directory",
            display_path,
            std::io::Error::from(error),
        )
    })
}

#[cfg(target_os = "linux")]
fn read_descriptor_at(parent: &OwnedFd, display_path: &Path) -> PocResult<AllocationDescriptor> {
    let file = rustix::fs::openat(
        parent,
        OsStr::new(DESCRIPTOR_FILE),
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open anchored allocation descriptor",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let metadata = rustix::fs::fstat(&file).map_err(|error| {
        PocError::io(
            "stat anchored allocation descriptor",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    if raw_mode_file_type(metadata.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::RegularFile
        || metadata.st_size as u64 > MAX_ALLOCATION_DESCRIPTOR_BYTES
    {
        return Err(PocError::RecoveryRequired(format!(
            "allocation descriptor is not a bounded regular file at {}",
            display_path.display()
        )));
    }
    serde_json::from_reader(File::from(file)).map_err(PocError::from)
}

#[cfg(target_os = "linux")]
fn lock_pinned_owner(pinned: &PinnedDeletion) -> PocResult<File> {
    require_directory_entry_matches(
        &pinned.allocation,
        OsStr::new(OWNER_DIRECTORY),
        &pinned.owner,
        &pinned.owner_path,
        "allocation owner metadata",
    )?;
    let lock_path = pinned.owner_path.join(OWNER_LOCK_FILE);
    let lock_fd = rustix::fs::openat(
        &pinned.owner,
        OsStr::new(OWNER_LOCK_FILE),
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open pinned allocation owner lock",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    let metadata = rustix::fs::fstat(&lock_fd).map_err(|error| {
        PocError::io(
            "stat pinned allocation owner lock",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    if raw_mode_file_type(metadata.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::RegularFile
    {
        return Err(PocError::RecoveryRequired(
            "allocation owner lock is not a regular file".to_owned(),
        ));
    }
    let lock = File::from(lock_fd);
    rustix::fs::flock(&lock, FlockOperation::LockExclusive).map_err(|error| {
        PocError::io(
            "lock pinned allocation owner",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    let installed = rustix::fs::statat(
        &pinned.owner,
        OsStr::new(OWNER_LOCK_FILE),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|error| {
        PocError::io(
            "revalidate pinned allocation owner lock",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    if raw_mode_file_type(installed.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::RegularFile
        || installed.st_dev != metadata.st_dev
        || installed.st_ino != metadata.st_ino
    {
        return Err(PocError::RecoveryRequired(
            "allocation owner lock changed while it was acquired".to_owned(),
        ));
    }
    Ok(lock)
}

#[cfg(target_os = "linux")]
fn revalidate_pinned_deletion(pinned: &PinnedDeletion) -> PocResult<()> {
    require_directory_entry_matches(
        &pinned.arena,
        OsStr::new(&pinned.prefix_name),
        &pinned.prefix,
        &pinned.prefix_path,
        "allocation prefix",
    )?;
    require_directory_entry_matches(
        &pinned.prefix,
        OsStr::new(&pinned.allocation_name),
        &pinned.allocation,
        &pinned.allocation_path,
        "allocation deletion target",
    )?;
    require_directory_entry_matches(
        &pinned.allocation,
        OsStr::new("upper"),
        &pinned.upper,
        &pinned.upper_path,
        "allocation upper",
    )?;
    require_directory_entry_matches(
        &pinned.allocation,
        OsStr::new("work"),
        &pinned.work,
        &pinned.work_path,
        "allocation work",
    )?;
    require_directory_entry_matches(
        &pinned.allocation,
        OsStr::new(OWNER_DIRECTORY),
        &pinned.owner,
        &pinned.owner_path,
        "allocation owner metadata",
    )
}

#[cfg(target_os = "linux")]
fn remove_anchored_directory_tree(
    pinned: &PinnedDeletion,
    owner_lock_fd: i32,
    parent: &OwnedFd,
    name: &OsStr,
    directory: &OwnedFd,
    display_path: &Path,
) -> PocResult<()> {
    require_directory_entry_matches(
        parent,
        name,
        directory,
        display_path,
        "allocation deletion target",
    )?;
    let parent_mount_id = crate::overlay_adapter::mount_id_for_fd(parent)?;
    let mount_id = crate::overlay_adapter::mount_id_for_fd(directory)?;
    if parent_mount_id != mount_id {
        return Err(PocError::RecoveryRequired(format!(
            "allocation deletion refuses to enter a mounted directory at {}",
            display_path.display()
        )));
    }
    let quarantine = quarantine_anchored_entry(
        parent,
        name,
        directory,
        display_path,
        "allocation deletion target",
    )?;
    let quarantine_path = pinned.prefix_path.join(&quarantine);
    if let Err(error) = reject_live_mount_reference(pinned, owner_lock_fd, &quarantine_path) {
        if let Err(restore_error) =
            restore_quarantined_entry(parent, name, &quarantine, directory, display_path)
        {
            return Err(PocError::RecoveryRequired(format!(
                "allocation mount audit failed and the target remains quarantined as {}: {restore_error}",
                quarantine.to_string_lossy()
            )));
        }
        return Err(error);
    }
    remove_anchored_directory_contents(directory, mount_id, display_path)?;
    require_directory_entry_matches(
        parent,
        &quarantine,
        directory,
        display_path,
        "quarantined allocation deletion target",
    )?;
    rustix::fs::unlinkat(parent, &quarantine, AtFlags::REMOVEDIR).map_err(|error| {
        PocError::io(
            "remove anchored allocation directory",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    require_entry_absent(
        parent,
        &quarantine,
        display_path,
        "quarantined allocation deletion target",
    )?;
    fsync_anchor(parent, display_path.parent().unwrap_or(display_path))
}

#[cfg(target_os = "linux")]
fn restore_quarantined_entry(
    parent: &OwnedFd,
    original_name: &OsStr,
    quarantine: &OsStr,
    directory: &OwnedFd,
    display_path: &Path,
) -> PocResult<()> {
    require_directory_entry_matches(
        parent,
        quarantine,
        directory,
        display_path,
        "quarantined allocation deletion target",
    )?;
    require_entry_absent(
        parent,
        original_name,
        display_path,
        "allocation deletion target",
    )?;
    rustix::fs::renameat_with(
        parent,
        quarantine,
        parent,
        original_name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        PocError::io(
            "restore quarantined allocation after mount audit",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    require_directory_entry_matches(
        parent,
        original_name,
        directory,
        display_path,
        "restored allocation deletion target",
    )?;
    fsync_anchor(parent, display_path.parent().unwrap_or(display_path))
}

#[cfg(target_os = "linux")]
fn remove_anchored_directory_contents(
    directory: &OwnedFd,
    root_mount_id: u64,
    display_path: &Path,
) -> PocResult<()> {
    let reader = rustix::fs::Dir::read_from(directory.as_fd()).map_err(|error| {
        PocError::io(
            "read anchored allocation directory",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let mut names = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|error| {
            PocError::io(
                "read anchored allocation entry",
                display_path,
                std::io::Error::from(error),
            )
        })?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
    names.sort();
    for name in names {
        remove_anchored_entry(directory, &name, root_mount_id, &display_path.join(&name))?;
    }
    fsync_anchor(directory, display_path)
}

#[cfg(target_os = "linux")]
fn remove_anchored_entry(
    parent: &OwnedFd,
    name: &OsStr,
    root_mount_id: u64,
    display_path: &Path,
) -> PocResult<()> {
    let before = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
        PocError::io(
            "inspect anchored allocation entry",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let file_type = raw_mode_file_type(before.st_mode as rustix::fs::RawMode);
    if file_type == rustix::fs::FileType::Directory {
        let child = open_child_directory_no_symlink(
            "allocation deletion child",
            parent,
            name,
            display_path,
        )?;
        require_stat_matches_fd(&before, &child, display_path)?;
        if crate::overlay_adapter::mount_id_for_fd(&child)? != root_mount_id {
            return Err(PocError::RecoveryRequired(format!(
                "allocation deletion refuses to cross a mount at {}",
                display_path.display()
            )));
        }
        let quarantine = quarantine_anchored_entry(
            parent,
            name,
            &child,
            display_path,
            "allocation deletion child",
        )?;
        remove_anchored_directory_contents(&child, root_mount_id, display_path)?;
        require_directory_entry_matches(
            parent,
            &quarantine,
            &child,
            display_path,
            "quarantined allocation deletion child",
        )?;
        rustix::fs::unlinkat(parent, &quarantine, AtFlags::REMOVEDIR).map_err(|error| {
            PocError::io(
                "remove anchored allocation child directory",
                display_path,
                std::io::Error::from(error),
            )
        })?;
        require_entry_absent(
            parent,
            &quarantine,
            display_path,
            "quarantined allocation deletion child",
        )?;
        fsync_anchor(parent, display_path.parent().unwrap_or(display_path))?;
    } else {
        let child = rustix::fs::openat(
            parent,
            name,
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            PocError::io(
                "pin anchored allocation entry",
                display_path,
                std::io::Error::from(error),
            )
        })?;
        require_stat_matches_fd(&before, &child, display_path)?;
        if crate::overlay_adapter::mount_id_for_fd(&child)? != root_mount_id {
            return Err(PocError::RecoveryRequired(format!(
                "allocation deletion refuses to remove a mounted entry at {}",
                display_path.display()
            )));
        }
        let quarantine = quarantine_anchored_entry(
            parent,
            name,
            &child,
            display_path,
            "allocation deletion entry",
        )?;
        require_entry_matches(
            parent,
            &quarantine,
            &child,
            display_path,
            "quarantined allocation deletion entry",
        )?;
        rustix::fs::unlinkat(parent, &quarantine, AtFlags::empty()).map_err(|error| {
            PocError::io(
                "remove anchored allocation entry",
                display_path,
                std::io::Error::from(error),
            )
        })?;
        require_entry_absent(
            parent,
            &quarantine,
            display_path,
            "quarantined allocation deletion entry",
        )?;
        fsync_anchor(parent, display_path.parent().unwrap_or(display_path))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn quarantine_anchored_entry(
    parent: &OwnedFd,
    name: &OsStr,
    entry: &OwnedFd,
    display_path: &Path,
    label: &str,
) -> PocResult<OsString> {
    require_entry_matches(parent, name, entry, display_path, label)?;
    let quarantine = OsString::from(format!(".allocation-delete-{}", Uuid::new_v4()));
    rustix::fs::renameat_with(
        parent,
        name,
        parent,
        &quarantine,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        PocError::io(
            "atomically quarantine allocation entry",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    require_entry_matches(parent, &quarantine, entry, display_path, label)?;
    fsync_anchor(parent, display_path.parent().unwrap_or(display_path))?;
    Ok(quarantine)
}

#[cfg(target_os = "linux")]
fn require_entry_matches(
    parent: &OwnedFd,
    name: &OsStr,
    entry: &OwnedFd,
    display_path: &Path,
    label: &str,
) -> PocResult<()> {
    require_single_component(label, name)?;
    let expected = rustix::fs::fstat(entry).map_err(|error| {
        PocError::io(
            "stat pinned allocation entry",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let observed =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
            PocError::io(
                "revalidate pinned allocation entry",
                display_path,
                std::io::Error::from(error),
            )
        })?;
    if observed.st_dev != expected.st_dev
        || observed.st_ino != expected.st_ino
        || raw_mode_file_type(observed.st_mode as rustix::fs::RawMode)
            != raw_mode_file_type(expected.st_mode as rustix::fs::RawMode)
    {
        return Err(PocError::RecoveryRequired(format!(
            "{label} changed after it was pinned: {}",
            display_path.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_directory_entry_matches(
    parent: &OwnedFd,
    name: &OsStr,
    directory: &OwnedFd,
    display_path: &Path,
    label: &str,
) -> PocResult<()> {
    require_entry_matches(parent, name, directory, display_path, label)?;
    let metadata = rustix::fs::fstat(directory).map_err(|error| {
        PocError::io(
            "stat pinned allocation directory",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    if raw_mode_file_type(metadata.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::Directory
    {
        return Err(PocError::RecoveryRequired(format!(
            "{label} is not a directory: {}",
            display_path.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_stat_matches_fd(
    expected: &rustix::fs::Stat,
    observed_fd: &OwnedFd,
    display_path: &Path,
) -> PocResult<()> {
    let observed = rustix::fs::fstat(observed_fd).map_err(|error| {
        PocError::io(
            "stat pinned allocation entry",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    if observed.st_dev != expected.st_dev
        || observed.st_ino != expected.st_ino
        || raw_mode_file_type(observed.st_mode as rustix::fs::RawMode)
            != raw_mode_file_type(expected.st_mode as rustix::fs::RawMode)
    {
        return Err(PocError::RecoveryRequired(format!(
            "allocation entry changed while it was pinned: {}",
            display_path.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_entry_absent(
    parent: &OwnedFd,
    name: &OsStr,
    display_path: &Path,
    label: &str,
) -> PocResult<()> {
    require_single_component(label, name)?;
    match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Ok(_) => Err(PocError::RecoveryRequired(format!(
            "{label} reappeared after removal: {}",
            display_path.display()
        ))),
        Err(error) => Err(PocError::io(
            "revalidate removed allocation entry",
            display_path,
            std::io::Error::from(error),
        )),
    }
}

#[cfg(target_os = "linux")]
fn require_single_component(label: &str, value: &OsStr) -> PocResult<()> {
    let mut components = Path::new(value).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(component)) if component == value)
        || components.next().is_some()
    {
        return Err(PocError::Integrity(format!(
            "{label} is not one normalized path component"
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn fsync_anchor(anchor: &OwnedFd, display_path: &Path) -> PocResult<()> {
    rustix::fs::fsync(anchor).map_err(|error| {
        PocError::io(
            "fsync anchored allocation directory",
            display_path,
            std::io::Error::from(error),
        )
    })
}

#[cfg(target_os = "linux")]
fn raw_mode_file_type(mode: rustix::fs::RawMode) -> rustix::fs::FileType {
    rustix::fs::FileType::from_raw_mode(mode)
}

#[cfg(target_os = "linux")]
fn reject_live_mount_reference(
    pinned: &PinnedDeletion,
    owner_lock_fd: i32,
    quarantined_path: &Path,
) -> PocResult<()> {
    let target = mount_reference_target(pinned, quarantined_path)?;
    let before = process_mount_namespaces()?;
    audit_mount_namespaces(&before, &target)?;
    audit_live_descriptors(&before, &target, pinned, owner_lock_fd)?;
    let after = process_mount_namespaces()?;
    if before != after {
        return Err(PocError::RecoveryRequired(
            "process mount namespaces changed during allocation mount audit".to_owned(),
        ));
    }
    audit_mount_namespaces(&after, &target)?;
    let final_snapshot = process_mount_namespaces()?;
    if after != final_snapshot {
        return Err(PocError::RecoveryRequired(
            "process mount namespaces changed during allocation mount re-audit".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn mount_reference_target(
    pinned: &PinnedDeletion,
    quarantined_path: &Path,
) -> PocResult<MountReferenceTarget> {
    let allocation = directory_identity(&pinned.allocation, &pinned.allocation_path)?;
    let upper = directory_identity(&pinned.upper, &pinned.upper_path)?;
    let work = directory_identity(&pinned.work, &pinned.work_path)?;
    let objects = allocation_object_identities(pinned)?;
    if allocation.device != upper.device || allocation.device != work.device {
        return Err(PocError::RecoveryRequired(
            "pinned allocation directories span filesystem devices".to_owned(),
        ));
    }
    let mount_id = crate::overlay_adapter::mount_id_for_fd(&pinned.allocation)?;
    let mountinfo_path = Path::new("/proc/self/mountinfo");
    let text = std::fs::read_to_string(mountinfo_path).map_err(|source| {
        PocError::io("read allocation mountinfo identity", mountinfo_path, source)
    })?;
    let entries = parse_allocation_mountinfo(&text)?;
    let mut matches = entries.iter().filter(|entry| entry.mount_id == mount_id);
    let mount = matches.next().ok_or_else(|| {
        PocError::RecoveryRequired("pinned allocation mount ID is absent from mountinfo".to_owned())
    })?;
    if matches.next().is_some() || mount.target_device != allocation.device {
        return Err(PocError::RecoveryRequired(
            "pinned allocation mount identity is ambiguous".to_owned(),
        ));
    }
    let relative = quarantined_path
        .strip_prefix(&mount.mountpoint)
        .map_err(|_| {
            PocError::RecoveryRequired(
                "pinned allocation is outside its mountinfo mountpoint".to_owned(),
            )
        })?;
    let filesystem_path = mount.root.join(relative);
    if !filesystem_path.is_absolute() {
        return Err(PocError::RecoveryRequired(
            "pinned allocation has no absolute filesystem identity".to_owned(),
        ));
    }
    Ok(MountReferenceTarget {
        directories: [allocation, upper, work].into_iter().collect(),
        objects,
        device: allocation.device,
        filesystem_path,
    })
}

#[cfg(target_os = "linux")]
fn directory_identity(directory: &OwnedFd, display_path: &Path) -> PocResult<DirectoryIdentity> {
    let metadata = rustix::fs::fstat(directory).map_err(|error| {
        PocError::io(
            "stat allocation mount audit directory",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    if raw_mode_file_type(metadata.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::Directory
    {
        return Err(PocError::RecoveryRequired(format!(
            "allocation mount audit target is not a directory: {}",
            display_path.display()
        )));
    }
    Ok(DirectoryIdentity {
        device: metadata.st_dev,
        inode: metadata.st_ino,
    })
}

#[cfg(target_os = "linux")]
fn allocation_object_identities(pinned: &PinnedDeletion) -> PocResult<BTreeSet<ObjectIdentity>> {
    let mount_id = crate::overlay_adapter::mount_id_for_fd(&pinned.allocation)?;
    let mut identities = BTreeSet::new();
    collect_object_identities(
        &pinned.allocation,
        mount_id,
        &pinned.allocation_path,
        &mut identities,
    )?;
    Ok(identities)
}

#[cfg(target_os = "linux")]
fn collect_object_identities(
    directory: &OwnedFd,
    root_mount_id: u64,
    display_path: &Path,
    identities: &mut BTreeSet<ObjectIdentity>,
) -> PocResult<()> {
    let metadata = rustix::fs::fstat(directory).map_err(|error| {
        PocError::io(
            "stat allocation object during mount audit",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    identities.insert(object_identity_from_stat(&metadata));
    let reader = rustix::fs::Dir::read_from(directory.as_fd()).map_err(|error| {
        PocError::io(
            "read allocation objects during mount audit",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    for entry in reader {
        let entry = entry.map_err(|error| {
            PocError::io(
                "read allocation object during mount audit",
                display_path,
                std::io::Error::from(error),
            )
        })?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = OsString::from_vec(bytes.to_vec());
        let child_path = display_path.join(&name);
        let child_metadata = rustix::fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| {
                PocError::io(
                    "stat allocation child during mount audit",
                    &child_path,
                    std::io::Error::from(error),
                )
            })?;
        identities.insert(object_identity_from_stat(&child_metadata));
        if raw_mode_file_type(child_metadata.st_mode as rustix::fs::RawMode)
            != rustix::fs::FileType::Directory
        {
            continue;
        }
        let child = open_child_directory_no_symlink(
            "allocation mount audit child",
            directory,
            &name,
            &child_path,
        )?;
        require_stat_matches_fd(&child_metadata, &child, &child_path)?;
        if crate::overlay_adapter::mount_id_for_fd(&child)? != root_mount_id {
            return Err(PocError::RecoveryRequired(format!(
                "allocation mount audit encountered a mounted child at {}",
                child_path.display()
            )));
        }
        collect_object_identities(&child, root_mount_id, &child_path, identities)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn object_identity_from_stat(metadata: &rustix::fs::Stat) -> ObjectIdentity {
    ObjectIdentity {
        device: metadata.st_dev,
        inode: metadata.st_ino,
        file_type: (metadata.st_mode as u32) & (libc::S_IFMT as u32),
    }
}

#[cfg(target_os = "linux")]
fn process_mount_namespaces() -> PocResult<BTreeMap<(u64, u64), Vec<PathBuf>>> {
    let mut namespaces = BTreeMap::<(u64, u64), Vec<PathBuf>>::new();
    let entries = std::fs::read_dir("/proc")
        .map_err(|source| PocError::io("enumerate mount audit processes", "/proc", source))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(PocError::io(
                    "enumerate mount audit process",
                    "/proc",
                    source,
                ));
            }
        };
        if entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        let proc_root = entry.path();
        let namespace_path = proc_root.join("ns/mnt");
        let metadata = match std::fs::metadata(&namespace_path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(PocError::io(
                    "stat process mount namespace for allocation deletion",
                    namespace_path,
                    source,
                ));
            }
        };
        namespaces
            .entry((metadata.dev(), metadata.ino()))
            .or_default()
            .push(proc_root);
    }
    for processes in namespaces.values_mut() {
        processes.sort();
    }
    Ok(namespaces)
}

#[cfg(target_os = "linux")]
fn audit_mount_namespaces(
    namespaces: &BTreeMap<(u64, u64), Vec<PathBuf>>,
    target: &MountReferenceTarget,
) -> PocResult<()> {
    for processes in namespaces.values() {
        audit_mount_namespace(processes, target)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn audit_mount_namespace(processes: &[PathBuf], target: &MountReferenceTarget) -> PocResult<()> {
    let mut layer_paths = BTreeMap::<(u64, PathBuf), AllocationMountInfo>::new();
    for proc_root in processes {
        let mountinfo_path = proc_root.join("mountinfo");
        let text = match std::fs::read_to_string(&mountinfo_path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source)
                if source.raw_os_error() == Some(libc::EINVAL)
                    && !proc_root.join("ns/mnt").exists() =>
            {
                continue;
            }
            Err(source) => {
                return Err(PocError::io(
                    "read foreign mountinfo for allocation deletion",
                    mountinfo_path,
                    source,
                ));
            }
        };
        for entry in parse_allocation_mountinfo(&text)? {
            if entry.filesystem_type == "nsfs" {
                return Err(PocError::RecoveryRequired(
                    "an uninspectable namespace is retained by a live nsfs mount".to_owned(),
                ));
            }
            if entry.target_device == target.device
                && entry.root.starts_with(&target.filesystem_path)
            {
                return Err(live_mount_conflict(target));
            }
            if entry.filesystem_type == "overlay" {
                let paths = overlay_layer_paths(&entry)?;
                if paths.is_empty() && !mount_contains_allocation(&entry, target) {
                    return Err(PocError::RecoveryRequired(
                        "live overlay mount exposes no layer identities during allocation deletion"
                            .to_owned(),
                    ));
                }
                for path in paths {
                    layer_paths
                        .entry((entry.mount_id, path))
                        .or_insert_with(|| entry.clone());
                }
            }
        }
    }
    for ((_, path), entry) in layer_paths {
        let mut resolved = BTreeSet::new();
        let mut unresolved = false;
        for proc_root in processes {
            let Some(directory) = open_namespace_directory(proc_root, &path)? else {
                unresolved = true;
                continue;
            };
            resolved.insert(directory_identity(&directory, &path)?);
            if directory_is_at_or_below_target(&directory, target)? {
                return Err(live_mount_conflict(target));
            }
        }
        if (resolved.len() != 1 || unresolved) && !mount_contains_allocation(&entry, target) {
            return Err(PocError::RecoveryRequired(format!(
                "cannot prove one live overlay layer identity for {} during allocation deletion",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn mount_contains_allocation(entry: &AllocationMountInfo, target: &MountReferenceTarget) -> bool {
    entry.target_device == target.device && target.filesystem_path.starts_with(&entry.root)
}

#[cfg(target_os = "linux")]
fn live_mount_conflict(target: &MountReferenceTarget) -> PocError {
    PocError::OwnerConflict(format!(
        "allocation filesystem object {} is still referenced by a live mount",
        target.filesystem_path.display()
    ))
}

#[cfg(target_os = "linux")]
fn open_namespace_directory(proc_root: &Path, path: &Path) -> PocResult<Option<OwnedFd>> {
    if !path.is_absolute() {
        return Err(PocError::RecoveryRequired(format!(
            "live overlay layer path is not absolute: {}",
            path.display()
        )));
    }
    let process_root_path = proc_root.join("root");
    let process_root = match rustix::fs::open(
        &process_root_path,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => {
            return Err(PocError::io(
                "open process root for overlay layer audit",
                process_root_path,
                std::io::Error::from(error),
            ));
        }
    };
    let relative = path.strip_prefix(Path::new("/")).map_err(|_| {
        PocError::RecoveryRequired(format!(
            "live overlay layer path is not rooted: {}",
            path.display()
        ))
    })?;
    let relative = if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    };
    match rustix::fs::openat2(
        &process_root,
        relative,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
        ResolveFlags::IN_ROOT | ResolveFlags::NO_MAGICLINKS,
    ) {
        Ok(directory) => Ok(Some(directory)),
        Err(rustix::io::Errno::NOENT) | Err(rustix::io::Errno::SRCH) => Ok(None),
        Err(error) => Err(PocError::io(
            "resolve overlay layer object in process mount namespace",
            path,
            std::io::Error::from(error),
        )),
    }
}

#[cfg(target_os = "linux")]
fn directory_is_at_or_below_target(
    directory: &OwnedFd,
    target: &MountReferenceTarget,
) -> PocResult<bool> {
    let mut current = rustix::fs::openat(
        directory,
        Path::new("."),
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "duplicate overlay layer directory",
            Path::new("/proc"),
            std::io::Error::from(error),
        )
    })?;
    for _ in 0..4096 {
        let identity = directory_identity(&current, Path::new("/proc"))?;
        if target.directories.contains(&identity) {
            return Ok(true);
        }
        let parent = rustix::fs::openat(
            &current,
            Path::new(".."),
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            PocError::io(
                "walk overlay layer directory ancestry",
                Path::new("/proc"),
                std::io::Error::from(error),
            )
        })?;
        if directory_identity(&parent, Path::new("/proc"))? == identity {
            return Ok(false);
        }
        current = parent;
    }
    Err(PocError::RecoveryRequired(
        "overlay layer ancestry exceeded the allocation audit bound".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn audit_live_descriptors(
    namespaces: &BTreeMap<(u64, u64), Vec<PathBuf>>,
    target: &MountReferenceTarget,
    pinned: &PinnedDeletion,
    owner_lock_fd: i32,
) -> PocResult<()> {
    let current_pid = std::process::id();
    let pinned_descriptors = [
        pinned.arena.as_raw_fd(),
        pinned.prefix.as_raw_fd(),
        pinned.allocation.as_raw_fd(),
        pinned.upper.as_raw_fd(),
        pinned.work.as_raw_fd(),
        pinned.owner.as_raw_fd(),
        owner_lock_fd,
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    for proc_root in namespaces.values().flatten() {
        audit_live_directory_authority(&proc_root.join("cwd"), target)?;
        audit_live_directory_authority(&proc_root.join("root"), target)?;
        let pid = proc_root
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|value| value.parse::<u32>().ok());
        let directory = proc_root.join("fd");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(PocError::io(
                    "enumerate live descriptors for allocation deletion",
                    directory,
                    source,
                ));
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(PocError::io(
                        "enumerate live descriptor for allocation deletion",
                        &directory,
                        source,
                    ));
                }
            };
            let descriptor = entry
                .file_name()
                .to_str()
                .and_then(|value| value.parse::<i32>().ok());
            if pid == Some(current_pid)
                && descriptor.is_some_and(|value| pinned_descriptors.contains(&value))
            {
                continue;
            }
            let descriptor_path = entry.path();
            let metadata = match std::fs::metadata(&descriptor_path) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(PocError::io(
                        "stat live descriptor for allocation deletion",
                        &descriptor_path,
                        source,
                    ));
                }
            };
            let identity = ObjectIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
                file_type: metadata.mode() & (libc::S_IFMT as u32),
            };
            if is_mount_namespace_descriptor(&descriptor_path)?
                && !namespaces.contains_key(&(identity.device, identity.inode))
            {
                return Err(PocError::RecoveryRequired(
                    "an uninspectable mount namespace is retained by a live descriptor".to_owned(),
                ));
            }
            if target.objects.contains(&identity) {
                return Err(live_mount_conflict(target));
            }
            if !metadata.is_dir() {
                continue;
            }
            let directory = match rustix::fs::open(
                &descriptor_path,
                rustix::fs::OFlags::PATH
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            ) {
                Ok(directory) => directory,
                Err(rustix::io::Errno::NOENT) | Err(rustix::io::Errno::NOTDIR) => continue,
                Err(error) => {
                    return Err(PocError::io(
                        "open live directory descriptor for allocation deletion",
                        descriptor_path,
                        std::io::Error::from(error),
                    ));
                }
            };
            if directory_is_at_or_below_target(&directory, target)? {
                return Err(live_mount_conflict(target));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_mount_namespace_descriptor(descriptor_path: &Path) -> PocResult<bool> {
    let target = match std::fs::read_link(descriptor_path) {
        Ok(target) => target,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(PocError::io(
                "read live descriptor identity for allocation deletion",
                descriptor_path,
                source,
            ));
        }
    };
    let value = target.as_os_str().as_bytes();
    Ok(value.len() > 6
        && value.starts_with(b"mnt:[")
        && value.ends_with(b"]")
        && value[5..value.len() - 1]
            .iter()
            .all(|byte| byte.is_ascii_digit()))
}

#[cfg(target_os = "linux")]
fn audit_live_directory_authority(
    authority_path: &Path,
    target: &MountReferenceTarget,
) -> PocResult<()> {
    let metadata = match std::fs::metadata(authority_path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(PocError::io(
                "stat live process directory authority for allocation deletion",
                authority_path,
                source,
            ));
        }
    };
    let identity = ObjectIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        file_type: metadata.mode() & (libc::S_IFMT as u32),
    };
    if target.objects.contains(&identity) {
        return Err(live_mount_conflict(target));
    }
    let directory = match rustix::fs::open(
        authority_path,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => {
            return Err(PocError::io(
                "open live process directory authority for allocation deletion",
                authority_path,
                std::io::Error::from(error),
            ));
        }
    };
    if directory_is_at_or_below_target(&directory, target)? {
        return Err(live_mount_conflict(target));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn parse_allocation_mountinfo(text: &str) -> PocResult<Vec<AllocationMountInfo>> {
    let mut mount_ids = BTreeSet::new();
    let mut entries = Vec::new();
    for line in text.lines() {
        let (left, right) = line.split_once(" - ").ok_or_else(|| {
            PocError::RecoveryRequired("mountinfo row has no field separator".to_owned())
        })?;
        let left = left.split_ascii_whitespace().collect::<Vec<_>>();
        let right = right.split_ascii_whitespace().collect::<Vec<_>>();
        if left.len() < 6 || right.len() < 3 {
            return Err(PocError::RecoveryRequired(
                "mountinfo row has too few fields".to_owned(),
            ));
        }
        let mount_id = left[0].parse::<u64>().map_err(|_| {
            PocError::RecoveryRequired("mountinfo row has an invalid mount ID".to_owned())
        })?;
        if !mount_ids.insert(mount_id) {
            return Err(PocError::RecoveryRequired(format!(
                "mountinfo contains duplicate mount ID {mount_id}"
            )));
        }
        let (major, minor) = left[2].split_once(':').ok_or_else(|| {
            PocError::RecoveryRequired("mountinfo row has an invalid device".to_owned())
        })?;
        let major = major.parse::<u32>().map_err(|_| {
            PocError::RecoveryRequired("mountinfo row has an invalid device major".to_owned())
        })?;
        let minor = minor.parse::<u32>().map_err(|_| {
            PocError::RecoveryRequired("mountinfo row has an invalid device minor".to_owned())
        })?;
        entries.push(AllocationMountInfo {
            mount_id,
            target_device: libc::makedev(major, minor),
            root: decode_mountinfo_path(left[3])?,
            mountpoint: decode_mountinfo_path(left[4])?,
            filesystem_type: right[0].to_owned(),
            super_options: right[2].split(',').map(str::to_owned).collect(),
        });
    }
    Ok(entries)
}

#[cfg(target_os = "linux")]
fn overlay_layer_paths(entry: &AllocationMountInfo) -> PocResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for option in &entry.super_options {
        if let Some(path) = option.strip_prefix("upperdir=") {
            paths.push(decode_mountinfo_path(path)?);
        } else if let Some(path) = option.strip_prefix("workdir=") {
            paths.push(decode_mountinfo_path(path)?);
        } else if let Some(path) = option.strip_prefix("lowerdir=") {
            paths.extend(split_overlay_lower_paths(path)?);
        }
    }
    Ok(paths)
}

#[cfg(target_os = "linux")]
fn split_overlay_lower_paths(value: &str) -> PocResult<Vec<PathBuf>> {
    let bytes = value.as_bytes();
    let mut paths = Vec::new();
    let mut start = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            if bytes.get(cursor + 1..cursor + 4).is_none() {
                return Err(PocError::RecoveryRequired(
                    "overlay lowerdir has a truncated escape".to_owned(),
                ));
            }
            cursor += 4;
        } else if bytes[cursor] == b':' {
            if start != cursor {
                let value = std::str::from_utf8(&bytes[start..cursor]).map_err(|_| {
                    PocError::RecoveryRequired("overlay lowerdir is not textual".to_owned())
                })?;
                paths.push(decode_mountinfo_path(value)?);
            }
            cursor += 1;
            start = cursor;
        } else {
            cursor += 1;
        }
    }
    if start != bytes.len() {
        let value = std::str::from_utf8(&bytes[start..]).map_err(|_| {
            PocError::RecoveryRequired("overlay lowerdir is not textual".to_owned())
        })?;
        paths.push(decode_mountinfo_path(value)?);
    }
    Ok(paths)
}

#[cfg(target_os = "linux")]
fn decode_mountinfo_path(value: &str) -> PocResult<PathBuf> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'\\' {
            decoded.push(bytes[cursor]);
            cursor += 1;
            continue;
        }
        let escaped = bytes.get(cursor + 1..cursor + 4).ok_or_else(|| {
            PocError::RecoveryRequired("mountinfo path has a truncated escape".to_owned())
        })?;
        if !escaped.iter().all(|byte| matches!(*byte, b'0'..=b'7')) {
            return Err(PocError::RecoveryRequired(
                "mountinfo path has a non-octal escape".to_owned(),
            ));
        }
        let octet = u16::from(escaped[0] - b'0') * 64
            + u16::from(escaped[1] - b'0') * 8
            + u16::from(escaped[2] - b'0');
        if octet == 0 || octet > u16::from(u8::MAX) {
            return Err(PocError::RecoveryRequired(
                "mountinfo path contains NUL".to_owned(),
            ));
        }
        decoded.push(octet as u8);
        cursor += 4;
    }
    Ok(PathBuf::from(OsString::from_vec(decoded)))
}

fn initialize_allocation(
    allocation_root: &Path,
    descriptor: &AllocationDescriptor,
) -> PocResult<()> {
    for directory in ["upper", "work", OWNER_DIRECTORY] {
        let path = allocation_root.join(directory);
        std::fs::create_dir(&path)
            .map_err(|source| PocError::io("create allocation directory", &path, source))?;
    }
    let owner_dir = allocation_root.join(OWNER_DIRECTORY);
    for directory in ["generations", "receipts"] {
        let path = owner_dir.join(directory);
        std::fs::create_dir(&path)
            .map_err(|source| PocError::io("create owner metadata directory", &path, source))?;
    }
    for file_name in [OWNER_LOCK_FILE, OWNER_JOURNAL_FILE] {
        let path = owner_dir.join(file_name);
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|source| PocError::io("create owner metadata file", &path, source))?;
    }
    fsync_dir(&owner_dir)?;
    write_immutable_json(&allocation_root.join(DESCRIPTOR_FILE), descriptor)?;
    Ok(())
}

fn prepare_arena(arena_root: &Path) -> PocResult<()> {
    let existed = arena_root.exists();
    if !existed {
        std::fs::create_dir_all(arena_root)
            .map_err(|source| PocError::io("create allocation arena", arena_root, source))?;
    }
    require_directory(arena_root, "allocation arena")?;
    if !existed {
        fsync_dir(arena_root)?;
        if let Some(parent) = arena_root.parent() {
            fsync_dir(parent)?;
        }
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
