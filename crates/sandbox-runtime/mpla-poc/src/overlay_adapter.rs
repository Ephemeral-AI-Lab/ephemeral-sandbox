#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use rustix::fd::AsFd;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;

#[cfg(target_os = "linux")]
use crate::{AllocationHandle, MutableLease};
use crate::{AllocationId, PocError, PocResult, SessionId, SCHEMA_VERSION};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OverlayMountAttestation {
    pub schema_version: u32,
    pub allocation_id: AllocationId,
    pub session_id: SessionId,
    pub lease_epoch: u64,
    pub owner_epoch: u64,
    pub workspace_root: PathBuf,
    pub allocation_root: PathBuf,
    pub allocation_upper: PathBuf,
    pub allocation_work: PathBuf,
    pub allocation_root_device: u64,
    pub allocation_root_inode: u64,
    pub allocation_upper_device: u64,
    pub allocation_upper_inode: u64,
    pub allocation_work_device: u64,
    pub allocation_work_inode: u64,
    pub allocation_owner_device: u64,
    pub allocation_owner_inode: u64,
    pub cgroup_procs_path: Option<PathBuf>,
    pub cgroup_procs_device: Option<u64>,
    pub cgroup_procs_inode: Option<u64>,
    pub mount_namespace_inode: u64,
    pub mount_id: u64,
    pub mount_unique_id: u64,
    pub target_device: u64,
    pub target_inode: u64,
    pub covered_workspace_device: u64,
    pub covered_workspace_inode: u64,
    pub filesystem_type: String,
    pub source: String,
    pub mount_options: Vec<String>,
    pub super_options: Vec<String>,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
pub(crate) struct OverlayProcessAuditIdentity {
    pub(crate) workspace_root: PathBuf,
    pub(crate) mount_namespace_inode: u64,
    pub(crate) mount_id: u64,
    pub(crate) target_device: u64,
    filesystem_type: String,
    source: String,
    mount_options: Vec<String>,
    super_options: Vec<String>,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedOverlayMount {
    mount_id: u64,
    parent_mount_id: u64,
    target_device: u64,
    workspace_root: PathBuf,
    filesystem_type: String,
    source: String,
    mount_options: Vec<String>,
    super_options: Vec<String>,
    upper_dir: Option<PathBuf>,
    work_dir: Option<PathBuf>,
}

/// A real OverlayFS mount whose writable layer is the permanent allocation.
///
/// The mountpoint is disposable session state. `upper/` and its adjacent
/// `work/` directory remain at their allocation-time paths for the entire
/// allocation lifetime.
#[derive(Debug)]
pub struct PermanentOverlayMount {
    workspace_root: PathBuf,
    allocation_root: PathBuf,
    allocation_upper: PathBuf,
    allocation_work: PathBuf,
    #[cfg(target_os = "linux")]
    mount_upper_label: PathBuf,
    #[cfg(target_os = "linux")]
    mount_work_label: PathBuf,
    #[cfg(target_os = "linux")]
    allocation_upper_identity: DirectoryIdentity,
    #[cfg(target_os = "linux")]
    allocation_work_identity: DirectoryIdentity,
    mount: Option<AnchoredOverlayGuard>,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct AnchoredOverlayGuard {
    session: OwnedFd,
    mounted: Option<OwnedFd>,
    mount_id: u64,
    mount_unique_id: u64,
    target_identity: DirectoryIdentity,
    workspace_identity: DirectoryIdentity,
    workspace_root: PathBuf,
    armed: bool,
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
struct AnchoredOverlayGuard;

#[cfg(not(target_os = "linux"))]
impl AnchoredOverlayGuard {
    fn strict_unmount(self) -> PocResult<()> {
        Err(PocError::Unsupported(
            "anchored overlay unmount requires Linux mount descriptors".to_owned(),
        ))
    }
}

#[cfg(target_os = "linux")]
impl AnchoredOverlayGuard {
    fn open_named_mount(&self) -> PocResult<OwnedFd> {
        rustix::fs::openat(
            &self.session,
            "mount",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            PocError::io(
                "open protected anchored overlay mountpoint",
                &self.workspace_root,
                std::io::Error::from(error),
            )
        })
    }

    fn authenticate_named(&self) -> PocResult<OwnedFd> {
        let named = self.open_named_mount()?;
        if mount_id_for_fd(&named)? != self.mount_id
            || mount_unique_id_for_fd(&named)? != self.mount_unique_id
            || fd_directory_identity(
                &named,
                "stat protected anchored overlay mountpoint",
                &self.workspace_root,
            )? != self.target_identity
        {
            return Err(PocError::RecoveryRequired(
                "protected anchored overlay name is not the exact mounted root".to_owned(),
            ));
        }
        require_unstacked_named_mount(
            &self.workspace_root,
            self.mount_id,
            self.target_identity.device,
        )?;
        Ok(named)
    }

    fn authenticate(&self) -> PocResult<()> {
        let mounted = self.mounted.as_ref().ok_or_else(|| {
            PocError::RecoveryRequired(
                "exact anchored overlay descriptor was already released".to_owned(),
            )
        })?;
        if mount_id_for_fd(mounted)? != self.mount_id
            || mount_unique_id_for_fd(mounted)? != self.mount_unique_id
            || fd_directory_identity(
                mounted,
                "stat exact anchored overlay mounted root",
                &self.workspace_root,
            )? != self.target_identity
        {
            return Err(PocError::RecoveryRequired(
                "anchored overlay mount identity changed before strict unmount".to_owned(),
            ));
        }
        let observed = current_mount_by_id(self.mount_id)?.ok_or_else(|| {
            PocError::RecoveryRequired(
                "anchored overlay disappeared before strict unmount".to_owned(),
            )
        })?;
        if observed.target_device != self.target_identity.device {
            return Err(PocError::RecoveryRequired(
                "anchored overlay device changed before strict unmount".to_owned(),
            ));
        }
        drop(self.authenticate_named()?);
        Ok(())
    }

    fn authenticate_restored_workspace(&self) -> PocResult<OwnedFd> {
        let restored = self.open_named_mount()?;
        if fd_directory_identity(
            &restored,
            "stat restored covered workspace",
            &self.workspace_root,
        )? != self.workspace_identity
            || mount_unique_id_for_fd(&restored)? == self.mount_unique_id
        {
            return Err(PocError::RecoveryRequired(
                "covered workspace authority was not restored after strict unmount".to_owned(),
            ));
        }
        Ok(restored)
    }

    fn strict_unmount(mut self) -> PocResult<()> {
        self.authenticate()?;
        // The detached fsmount descriptor is a busy mount reference and must
        // be released before strict unmount.  The ordinary authenticated
        // directory descriptor remains usable as the exact syscall target.
        drop(self.mounted.take());
        let named = self.authenticate_named()?;
        strict_unmount_exact_mount(&named).map_err(|error| {
            PocError::io(
                "strictly unmount exact anchored overlay",
                &self.workspace_root,
                error,
            )
        })?;
        drop(named);
        if current_mount_by_id(self.mount_id)?.is_some() {
            return Err(PocError::RecoveryRequired(
                "anchored overlay remained visible after strict unmount".to_owned(),
            ));
        }
        drop(self.authenticate_restored_workspace()?);
        self.armed = false;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for AnchoredOverlayGuard {
    fn drop(&mut self) {
        if self.armed && self.authenticate().is_ok() {
            drop(self.mounted.take());
            if let Ok(named) = self.authenticate_named() {
                if strict_unmount_exact_mount(&named).is_ok() {
                    drop(named);
                    if let Ok(restored) = self.authenticate_restored_workspace() {
                        drop(restored);
                        self.armed = false;
                    }
                }
            }
        }
    }
}

impl PermanentOverlayMount {
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    #[must_use]
    pub fn allocation_root(&self) -> &Path {
        &self.allocation_root
    }

    #[must_use]
    pub fn allocation_upper(&self) -> &Path {
        &self.allocation_upper
    }

    #[must_use]
    pub fn allocation_work(&self) -> &Path {
        &self.allocation_work
    }

    #[must_use]
    pub const fn is_mounted(&self) -> bool {
        self.mount.is_some()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn anchored_runtime_workspace_root(&self) -> PocResult<PathBuf> {
        let guard = self
            .mount
            .as_ref()
            .ok_or_else(|| PocError::Integrity("anchored overlay guard is absent".to_owned()))?;
        Ok(descriptor_path(guard.mounted.as_ref().ok_or_else(
            || {
                PocError::RecoveryRequired(
                    "exact anchored overlay descriptor was already released".to_owned(),
                )
            },
        )?))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn process_audit_identity(&self) -> PocResult<OverlayProcessAuditIdentity> {
        let guard = self
            .mount
            .as_ref()
            .ok_or_else(|| PocError::Integrity("anchored overlay guard is absent".to_owned()))?;
        guard.authenticate()?;
        let observed = current_mount_by_id(guard.mount_id)?.ok_or_else(|| {
            PocError::RecoveryRequired(
                "anchored overlay disappeared before process audit".to_owned(),
            )
        })?;
        require_observed_layout(&observed)?;
        require_observed_source_labels(&observed, &self.mount_upper_label, &self.mount_work_label)?;
        if observed.target_device != guard.target_identity.device {
            return Err(PocError::RecoveryRequired(
                "anchored overlay device changed before process audit".to_owned(),
            ));
        }
        let mount_namespace_inode = std::fs::metadata("/proc/self/ns/mnt")
            .map_err(|error| {
                PocError::io(
                    "stat process-audit mount namespace",
                    "/proc/self/ns/mnt",
                    error,
                )
            })?
            .ino();
        Ok(OverlayProcessAuditIdentity {
            workspace_root: self.workspace_root.clone(),
            mount_namespace_inode,
            mount_id: observed.mount_id,
            target_device: observed.target_device,
            filesystem_type: observed.filesystem_type,
            source: observed.source,
            mount_options: observed.mount_options,
            super_options: observed.super_options,
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attest_anchored(
        &self,
        lease: &MutableLease,
        cgroup: Option<(&Path, u64, u64)>,
        session: &OwnedFd,
        workspace: &OwnedFd,
        allocation_root: &OwnedFd,
        allocation_owner: &OwnedFd,
    ) -> PocResult<OverlayMountAttestation> {
        let guard = self.mount.as_ref().ok_or_else(|| {
            PocError::Integrity("anchored overlay guard is absent during attestation".to_owned())
        })?;
        capture_mount_attestation_anchored(
            session,
            workspace,
            allocation_root,
            allocation_owner,
            guard.mounted.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "exact anchored overlay descriptor was released before attestation".to_owned(),
                )
            })?,
            guard.mount_id,
            guard.mount_unique_id,
            guard.workspace_identity,
            &self.workspace_root,
            &self.allocation_root,
            &self.allocation_upper,
            &self.allocation_work,
            &self.mount_upper_label,
            &self.mount_work_label,
            self.allocation_upper_identity,
            self.allocation_work_identity,
            lease,
            cgroup,
        )
    }

    /// Strictly unmount without a lazy-detach fallback.
    ///
    /// The production overlay guard's best-effort `Drop` remains armed until
    /// the strict syscall succeeds. After success, dropping the guard observes
    /// an already-unmounted directory and performs no payload operation.
    pub fn strict_unmount(mut self) -> PocResult<UnmountedOverlay> {
        let mount = self
            .mount
            .take()
            .ok_or_else(|| PocError::Integrity("overlay was already unmounted".to_owned()))?;
        mount.strict_unmount()?;
        Ok(UnmountedOverlay {
            workspace_root: self.workspace_root.clone(),
            allocation_root: self.allocation_root.clone(),
            allocation_upper: self.allocation_upper.clone(),
            allocation_work: self.allocation_work.clone(),
        })
    }
}

/// Paths retained after the only live workspace mount has been strictly
/// removed. This receipt contains physical facts for evidence, not identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnmountedOverlay {
    pub workspace_root: PathBuf,
    pub allocation_root: PathBuf,
    pub allocation_upper: PathBuf,
    pub allocation_work: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttestedMountCleanupState {
    MountedExact,
    AlreadyAbsent,
}

/// Authenticate a recovery mount through the descriptor opened before any
/// destructive work. This remains bound to the same mount when an ancestor is
/// concurrently renamed or replaced.
#[cfg(target_os = "linux")]
pub(crate) fn validate_attested_mount_for_cleanup_anchored(
    attestation: &OverlayMountAttestation,
    session: &OwnedFd,
    workspace: &OwnedFd,
) -> PocResult<AttestedMountCleanupState> {
    validate_attestation_shape(attestation)?;
    require_attested_mount_namespace(attestation)?;
    let pinned_mount_id = mount_id_for_fd(workspace)?;
    let pinned_mount_unique_id = mount_unique_id_for_fd(workspace)?;
    let named = open_session_workspace(session, &attestation.workspace_root)?;
    let named_mount_id = mount_id_for_fd(&named)?;
    let named_mount_unique_id = mount_unique_id_for_fd(&named)?;
    let observed = current_mount_by_id(attestation.mount_id)?;
    let Some(observed) = observed else {
        if pinned_mount_id == attestation.mount_id
            || named_mount_id == attestation.mount_id
            || pinned_mount_unique_id == attestation.mount_unique_id
            || named_mount_unique_id == attestation.mount_unique_id
        {
            return Err(PocError::RecoveryRequired(
                "pinned terminal workspace mount disappeared during authentication".to_owned(),
            ));
        }
        return Ok(AttestedMountCleanupState::AlreadyAbsent);
    };
    if pinned_mount_id != attestation.mount_id
        || named_mount_id != attestation.mount_id
        || pinned_mount_unique_id != attestation.mount_unique_id
        || named_mount_unique_id != attestation.mount_unique_id
    {
        return Err(PocError::RecoveryRequired(
            "pinned session mount name is not the attested terminal mount".to_owned(),
        ));
    }
    drop(authenticate_attested_named_mount(attestation, session)?);
    require_observed_attestation(&observed, attestation, false)?;
    let target = rustix::fs::fstat(workspace).map_err(|error| {
        PocError::io(
            "stat pinned terminal workspace",
            &attestation.workspace_root,
            std::io::Error::from(error),
        )
    })?;
    if target.st_dev != attestation.target_device || target.st_ino != attestation.target_inode {
        return Err(PocError::RecoveryRequired(
            "pinned terminal workspace inode differs from its durable attestation".to_owned(),
        ));
    }
    Ok(AttestedMountCleanupState::MountedExact)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn validate_attested_mount_for_cleanup_anchored(
    _attestation: &OverlayMountAttestation,
    _session: &std::os::fd::OwnedFd,
    _workspace: &std::os::fd::OwnedFd,
) -> PocResult<AttestedMountCleanupState> {
    Err(PocError::Unsupported(
        "descriptor-anchored mount authentication requires Linux mount IDs".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn freeze_attested_mount_read_only_anchored(
    attestation: &OverlayMountAttestation,
    session: &OwnedFd,
    workspace: &OwnedFd,
) -> PocResult<()> {
    if validate_attested_mount_for_cleanup_anchored(attestation, session, workspace)?
        != AttestedMountCleanupState::MountedExact
    {
        return Err(PocError::RecoveryRequired(
            "terminal mount disappeared before its read-only freeze".to_owned(),
        ));
    }
    let attributes = libc::mount_attr {
        attr_set: libc::MOUNT_ATTR_RDONLY,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    let empty_path = [0_i8];
    let flags = libc::AT_EMPTY_PATH | libc::AT_RECURSIVE;
    // SAFETY: mount_setattr consumes the valid directory descriptor, a static
    // empty C string, scalar flags, and the initialized version-zero structure.
    let result = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            workspace.as_raw_fd(),
            empty_path.as_ptr(),
            flags,
            &attributes as *const libc::mount_attr,
            libc::MOUNT_ATTR_SIZE_VER0 as usize,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if frozen_mount_operation_requires_retry(error.raw_os_error().unwrap_or_default()) {
            return Err(PocError::RecoveryRequired(
                "terminal mount freeze was busy with a writable reference; retry recovery"
                    .to_owned(),
            ));
        }
        return Err(PocError::io(
            "freeze pinned terminal mount read-only",
            &attestation.workspace_root,
            error,
        ));
    }
    validate_attested_mount_for_cleanup_anchored(attestation, session, workspace)?;
    require_attested_mount_tree_read_only(attestation)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn freeze_attested_mount_read_only_anchored(
    _attestation: &OverlayMountAttestation,
    _session: &std::os::fd::OwnedFd,
    _workspace: &std::os::fd::OwnedFd,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "descriptor-anchored read-only mount freeze requires Linux mount_setattr".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn strict_unmount_attested_frozen_anchored(
    attestation: &OverlayMountAttestation,
    session: &OwnedFd,
    workspace: OwnedFd,
) -> PocResult<UnmountedOverlay> {
    match validate_attested_mount_for_cleanup_anchored(attestation, session, &workspace)? {
        AttestedMountCleanupState::AlreadyAbsent => return Ok(unmounted_attestation(attestation)),
        AttestedMountCleanupState::MountedExact => {}
    }
    require_attested_mount_tree_read_only(attestation)?;
    drop(authenticate_attested_named_mount(attestation, session)?);
    require_attested_mount_tree_read_only(attestation)?;
    let named = authenticate_attested_named_mount(attestation, session)?;
    if let Err(error) = strict_unmount_exact_mount(&workspace) {
        if frozen_mount_operation_requires_retry(error.raw_os_error().unwrap_or_default()) {
            return Err(PocError::RecoveryRequired(
                "frozen terminal mount remained busy; retry recovery".to_owned(),
            ));
        }
        return Err(PocError::io(
            "strictly unmount exact pinned frozen terminal workspace",
            &attestation.workspace_root,
            error,
        ));
    }
    drop(named);
    drop(workspace);
    if current_mount_by_id(attestation.mount_id)?.is_some()
        || !attested_mount_tree_ids(attestation)?.is_empty()
    {
        return Err(PocError::RecoveryRequired(
            "attested terminal mount tree remained visible after strict unmount".to_owned(),
        ));
    }
    drop(require_restored_covered_workspace(attestation, session)?);
    Ok(unmounted_attestation(attestation))
}

#[cfg(target_os = "linux")]
fn strict_unmount_exact_mount(mount: &OwnedFd) -> std::io::Result<()> {
    let descriptor_path = CString::new(format!("/proc/self/fd/{}", mount.as_raw_fd()))
        .expect("numeric procfd mount path cannot contain NUL");
    // SAFETY: the C string names the exact mount held by the authenticated
    // descriptor, and zero flags deliberately exclude lazy detach.
    if unsafe { libc::umount2(descriptor_path.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn strict_unmount_attested_frozen_anchored(
    _attestation: &OverlayMountAttestation,
    _session: &std::os::fd::OwnedFd,
    _workspace: std::os::fd::OwnedFd,
) -> PocResult<UnmountedOverlay> {
    Err(PocError::Unsupported(
        "descriptor-anchored strict unmount requires Linux mount IDs".to_owned(),
    ))
}

#[doc(hidden)]
#[must_use]
pub const fn frozen_mount_operation_requires_retry(raw_os_error: i32) -> bool {
    raw_os_error == libc::EBUSY
}

#[cfg(target_os = "linux")]
pub(crate) fn require_attested_mount_absent_anchored(
    attestation: &OverlayMountAttestation,
    workspace: &OwnedFd,
) -> PocResult<()> {
    validate_attestation_shape(attestation)?;
    require_attested_mount_namespace(attestation)?;
    if current_mount_by_id(attestation.mount_id)?.is_some()
        || mount_id_for_fd(workspace)? == attestation.mount_id
        || mount_unique_id_for_fd(workspace)? == attestation.mount_unique_id
        || !attested_mount_tree_ids(attestation)?.is_empty()
    {
        return Err(PocError::RecoveryRequired(
            "completed terminal recovery still exposes the attested mount ID".to_owned(),
        ));
    }
    let restored = fd_directory_identity(
        workspace,
        "stat post-unmount covered workspace",
        &attestation.workspace_root,
    )?;
    if restored.device != attestation.covered_workspace_device
        || restored.inode != attestation.covered_workspace_inode
    {
        return Err(PocError::RecoveryRequired(
            "post-unmount workspace is not the attested covered directory".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn require_attested_mount_absent_anchored(
    _attestation: &OverlayMountAttestation,
    _workspace: &std::os::fd::OwnedFd,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "descriptor-anchored mount absence proof requires Linux mount IDs".to_owned(),
    ))
}

/// Match the durable overlay identity in another mount namespace without
/// trusting its mutable mountpoint spelling or namespace-local mount ID.
#[cfg(target_os = "linux")]
pub(crate) fn mountinfo_text_has_attested_mount(
    text: &str,
    attestation: &OverlayMountAttestation,
) -> PocResult<bool> {
    validate_attestation_shape(attestation)?;
    mountinfo_text_has_process_audit_mount(
        text,
        &process_audit_identity_from_attestation_unchecked(attestation),
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn mountinfo_text_has_process_audit_mount(
    text: &str,
    identity: &OverlayProcessAuditIdentity,
) -> PocResult<bool> {
    for line in text.lines() {
        let observed = parse_mountinfo_line(line)?;
        if observed_preserves_process_audit_identity(&observed, identity) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
pub(crate) fn attested_mount_tree_ids(
    attestation: &OverlayMountAttestation,
) -> PocResult<BTreeSet<u64>> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").map_err(|error| {
        PocError::io(
            "read mountinfo for anchored process audit",
            "/proc/self/mountinfo",
            error,
        )
    })?;
    attested_mount_tree_ids_from_mountinfo(&text, attestation)
}

#[cfg(target_os = "linux")]
pub(crate) fn process_audit_mount_tree_ids(
    identity: &OverlayProcessAuditIdentity,
) -> PocResult<BTreeSet<u64>> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").map_err(|error| {
        PocError::io(
            "read mountinfo for anchored process audit",
            "/proc/self/mountinfo",
            error,
        )
    })?;
    process_audit_mount_tree_ids_from_mountinfo(&text, identity)
}

#[cfg(target_os = "linux")]
fn require_attested_mount_tree_read_only(attestation: &OverlayMountAttestation) -> PocResult<()> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").map_err(|error| {
        PocError::io(
            "read mountinfo for terminal mount freeze",
            "/proc/self/mountinfo",
            error,
        )
    })?;
    require_attested_mount_tree_read_only_from_mountinfo(&text, attestation)
}

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub fn require_attested_mount_tree_read_only_from_mountinfo(
    text: &str,
    attestation: &OverlayMountAttestation,
) -> PocResult<()> {
    let mount_ids = attested_mount_tree_ids_from_mountinfo(text, attestation)?;
    if !mount_ids.contains(&attestation.mount_id) {
        return Err(PocError::RecoveryRequired(
            "attested terminal mount disappeared during its read-only proof".to_owned(),
        ));
    }
    for line in text.lines() {
        let observed = parse_mountinfo_line(line)?;
        if mount_ids.contains(&observed.mount_id)
            && !mount_options_are_read_only(&observed.mount_options)
        {
            return Err(PocError::RecoveryRequired(format!(
                "attested terminal mount tree member {} remains writable",
                observed.mount_id
            )));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub fn attested_mount_tree_ids_from_mountinfo(
    text: &str,
    attestation: &OverlayMountAttestation,
) -> PocResult<BTreeSet<u64>> {
    validate_attestation_shape(attestation)?;
    process_audit_mount_tree_ids_from_mountinfo(
        text,
        &process_audit_identity_from_attestation_unchecked(attestation),
    )
}

#[cfg(target_os = "linux")]
fn process_audit_mount_tree_ids_from_mountinfo(
    text: &str,
    identity: &OverlayProcessAuditIdentity,
) -> PocResult<BTreeSet<u64>> {
    let mut observed_mounts = Vec::new();
    let mut observed_ids = BTreeSet::new();
    for line in text.lines() {
        let observed = parse_mountinfo_line(line)?;
        if !observed_ids.insert(observed.mount_id) {
            return Err(PocError::RecoveryRequired(format!(
                "mountinfo contains duplicate mount ID {}",
                observed.mount_id
            )));
        }
        if observed.mount_id == identity.mount_id
            && !observed_matches_process_audit_identity(&observed, identity)
        {
            return Err(PocError::RecoveryRequired(
                "attested terminal mount ID was reused by a different live mount".to_owned(),
            ));
        }
        observed_mounts.push(observed);
    }
    let mut mount_ids = observed_mounts
        .iter()
        .filter(|observed| observed_preserves_process_audit_identity(observed, identity))
        .map(|observed| observed.mount_id)
        .collect::<BTreeSet<_>>();
    loop {
        let before = mount_ids.len();
        for observed in &observed_mounts {
            if mount_ids.contains(&observed.parent_mount_id) {
                mount_ids.insert(observed.mount_id);
            }
        }
        if mount_ids.len() == before {
            return Ok(mount_ids);
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn process_audit_identity_from_attestation(
    attestation: &OverlayMountAttestation,
) -> PocResult<OverlayProcessAuditIdentity> {
    validate_attestation_shape(attestation)?;
    Ok(process_audit_identity_from_attestation_unchecked(
        attestation,
    ))
}

#[cfg(target_os = "linux")]
fn process_audit_identity_from_attestation_unchecked(
    attestation: &OverlayMountAttestation,
) -> OverlayProcessAuditIdentity {
    OverlayProcessAuditIdentity {
        workspace_root: attestation.workspace_root.clone(),
        mount_namespace_inode: attestation.mount_namespace_inode,
        mount_id: attestation.mount_id,
        target_device: attestation.target_device,
        filesystem_type: attestation.filesystem_type.clone(),
        source: attestation.source.clone(),
        mount_options: attestation.mount_options.clone(),
        super_options: attestation.super_options.clone(),
    }
}

/// Pin the attested cgroup directory and reopen its membership file only
/// relative to that descriptor. Missing or replaced membership fails closed.
#[cfg(target_os = "linux")]
pub(crate) fn validated_attested_cgroup_path(
    attestation: &OverlayMountAttestation,
) -> PocResult<Option<crate::process_tree::AttestedCgroupMembership>> {
    let Some(path) = attestation.cgroup_procs_path.as_deref() else {
        if attestation.cgroup_procs_device.is_some() || attestation.cgroup_procs_inode.is_some() {
            return Err(PocError::RecoveryRequired(
                "terminal mount attestation has cgroup identity without a path".to_owned(),
            ));
        }
        return Ok(None);
    };
    let expected_device = attestation.cgroup_procs_device.ok_or_else(|| {
        PocError::RecoveryRequired("terminal mount attestation has no cgroup device".to_owned())
    })?;
    let expected_inode = attestation.cgroup_procs_inode.ok_or_else(|| {
        PocError::RecoveryRequired("terminal mount attestation has no cgroup inode".to_owned())
    })?;
    crate::process_tree::AttestedCgroupMembership::open(path, expected_device, expected_inode)
        .map(Some)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn validated_attested_cgroup_path(
    _attestation: &OverlayMountAttestation,
) -> PocResult<Option<crate::process_tree::AttestedCgroupMembership>> {
    Err(PocError::Unsupported(
        "restart-safe cgroup attestation requires Linux metadata".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn mount_permanent_overlay_anchored(
    allocation: &AllocationHandle,
    lower_dirs_newest_first: Vec<PathBuf>,
    workspace_root: &Path,
    session: &OwnedFd,
    workspace: &OwnedFd,
    allocation_upper: &OwnedFd,
    allocation_work: &OwnedFd,
) -> PocResult<PermanentOverlayMount> {
    require_stationary_layout(allocation)?;
    if lower_dirs_newest_first.is_empty() {
        return Err(PocError::Integrity(
            "permanent overlay requires at least one lower layer".to_owned(),
        ));
    }
    let (allocation_upper_identity, allocation_work_identity) =
        pinned_overlay_source_identities(allocation, allocation_upper, allocation_work)?;

    let mount_upper_label = descriptor_path(allocation_upper);
    let mount_work_label = descriptor_path(allocation_work);
    let fsfd = configured_anchored_overlay_fs(
        &lower_dirs_newest_first,
        &mount_upper_label,
        &mount_work_label,
    )?;
    rustix::mount::fsconfig_create(fsfd.as_fd())
        .map_err(|error| mount_syscall_error("create anchored overlay", workspace_root, error))?;
    let mount_fd = rustix::mount::fsmount(
        fsfd.as_fd(),
        rustix::mount::FsMountFlags::FSMOUNT_CLOEXEC,
        rustix::mount::MountAttrFlags::MOUNT_ATTR_NODEV
            | rustix::mount::MountAttrFlags::MOUNT_ATTR_NOSUID,
    )
    .map_err(|error| mount_syscall_error("fsmount anchored overlay", workspace_root, error))?;
    let workspace_identity = fd_directory_identity(
        workspace,
        "stat pinned workspace mountpoint",
        workspace_root,
    )?;
    let guard_session = rustix::io::dup(session).map_err(|error| {
        PocError::io(
            "duplicate anchored session for overlay guard",
            workspace_root,
            std::io::Error::from(error),
        )
    })?;
    let mount_id = mount_id_for_fd(&mount_fd)?;
    let mount_unique_id = mount_unique_id_for_fd(&mount_fd)?;
    let mounted_status = rustix::fs::fstat(&mount_fd).map_err(|error| {
        PocError::io(
            "stat exact detached overlay",
            workspace_root,
            std::io::Error::from(error),
        )
    })?;
    let guard = AnchoredOverlayGuard {
        session: guard_session,
        mounted: Some(mount_fd),
        mount_id,
        mount_unique_id,
        target_identity: DirectoryIdentity {
            device: mounted_status.st_dev,
            inode: mounted_status.st_ino,
        },
        workspace_identity,
        workspace_root: workspace_root.to_path_buf(),
        armed: true,
    };
    rustix::mount::move_mount(
        guard
            .mounted
            .as_ref()
            .ok_or_else(|| PocError::Integrity("detached overlay descriptor is absent".to_owned()))?
            .as_fd(),
        "",
        workspace.as_fd(),
        "",
        rustix::mount::MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH
            | rustix::mount::MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
    )
    .map_err(|error| mount_syscall_error("attach anchored overlay", workspace_root, error))?;
    if fd_directory_identity(
        workspace,
        "restat pinned workspace mountpoint",
        workspace_root,
    )? != workspace_identity
    {
        return Err(PocError::RecoveryRequired(
            "anchored workspace name changed during overlay attachment".to_owned(),
        ));
    }
    drop(guard.authenticate_named()?);
    let observed = current_mount_by_id(guard.mount_id)?.ok_or_else(|| {
        PocError::RecoveryRequired(
            "exact attached overlay is absent from the current mount namespace".to_owned(),
        )
    })?;
    if observed.target_device != guard.target_identity.device {
        return Err(PocError::RecoveryRequired(
            "attached overlay mountinfo device differs from its exact mount descriptor".to_owned(),
        ));
    }
    require_path_directory_identity(
        &allocation.upper_dir,
        allocation_upper_identity,
        "allocation upper changed during overlay mount",
    )?;
    require_path_directory_identity(
        &allocation.work_dir,
        allocation_work_identity,
        "allocation work changed during overlay mount",
    )?;
    Ok(PermanentOverlayMount {
        workspace_root: workspace_root.to_path_buf(),
        allocation_root: allocation.allocation_root.clone(),
        allocation_upper: allocation.upper_dir.clone(),
        allocation_work: allocation.work_dir.clone(),
        mount_upper_label,
        mount_work_label,
        allocation_upper_identity,
        allocation_work_identity,
        mount: Some(guard),
    })
}

#[cfg(target_os = "linux")]
fn pinned_overlay_source_identities(
    allocation: &AllocationHandle,
    allocation_upper: &OwnedFd,
    allocation_work: &OwnedFd,
) -> PocResult<(DirectoryIdentity, DirectoryIdentity)> {
    require_stationary_layout(allocation)?;
    let upper = fd_directory_identity(
        allocation_upper,
        "stat pinned allocation upper",
        &allocation.upper_dir,
    )?;
    let work = fd_directory_identity(
        allocation_work,
        "stat pinned allocation work",
        &allocation.work_dir,
    )?;
    if upper.device != work.device {
        return Err(PocError::Integrity(
            "pinned overlay upper and work are on different filesystems".to_owned(),
        ));
    }
    if upper == work {
        return Err(PocError::Integrity(
            "pinned overlay upper and work are the same directory".to_owned(),
        ));
    }
    require_path_directory_identity(
        &allocation.upper_dir,
        upper,
        "allocation upper changed before overlay mount",
    )?;
    require_path_directory_identity(
        &allocation.work_dir,
        work,
        "allocation work changed before overlay mount",
    )?;
    Ok((upper, work))
}

#[cfg(target_os = "linux")]
fn configured_anchored_overlay_fs(
    lower_dirs_newest_first: &[PathBuf],
    upper: &Path,
    work: &Path,
) -> PocResult<OwnedFd> {
    let fsfd = rustix::mount::fsopen("overlay", rustix::mount::FsOpenFlags::FSOPEN_CLOEXEC)
        .map_err(|error| mount_syscall_error("open overlay mount context", upper, error))?;
    for lower in lower_dirs_newest_first {
        rustix::mount::fsconfig_set_string(fsfd.as_fd(), "lowerdir+", lower)
            .map_err(|error| mount_syscall_error("configure overlay lowerdir+", lower, error))?;
    }
    rustix::mount::fsconfig_set_flag(fsfd.as_fd(), "userxattr")
        .map_err(|error| mount_syscall_error("configure overlay userxattr", upper, error))?;
    rustix::mount::fsconfig_set_string(fsfd.as_fd(), "upperdir", upper)
        .map_err(|error| mount_syscall_error("configure pinned overlay upper", upper, error))?;
    rustix::mount::fsconfig_set_string(fsfd.as_fd(), "workdir", work)
        .map_err(|error| mount_syscall_error("configure pinned overlay work", work, error))?;
    Ok(fsfd)
}

#[cfg(target_os = "linux")]
fn descriptor_path(descriptor: &OwnedFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", descriptor.as_raw_fd()))
}

#[cfg(target_os = "linux")]
fn mount_syscall_error(context: &'static str, path: &Path, error: rustix::io::Errno) -> PocError {
    PocError::io(context, path, std::io::Error::from(error))
}

#[cfg(target_os = "linux")]
fn fd_directory_identity(
    descriptor: &OwnedFd,
    context: &'static str,
    display_path: &Path,
) -> PocResult<DirectoryIdentity> {
    let status = rustix::fs::fstat(descriptor)
        .map_err(|error| PocError::io(context, display_path, std::io::Error::from(error)))?;
    if rustix::fs::FileType::from_raw_mode(status.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::Directory
    {
        return Err(PocError::Integrity(format!(
            "pinned overlay source is not a directory: {}",
            display_path.display()
        )));
    }
    Ok(DirectoryIdentity {
        device: status.st_dev,
        inode: status.st_ino,
    })
}

#[cfg(target_os = "linux")]
fn path_directory_identity(path: &Path) -> PocResult<DirectoryIdentity> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| PocError::io("stat overlay source", path, error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(PocError::Integrity(format!(
            "overlay source is not a real directory: {}",
            path.display()
        )));
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(target_os = "linux")]
fn require_path_directory_identity(
    path: &Path,
    expected: DirectoryIdentity,
    message: &'static str,
) -> PocResult<()> {
    if path_directory_identity(path)? == expected {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(message.to_owned()))
    }
}

#[cfg(target_os = "linux")]
fn require_stationary_layout(allocation: &AllocationHandle) -> PocResult<()> {
    if allocation.upper_dir.parent() != Some(allocation.allocation_root.as_path())
        || allocation.work_dir.parent() != Some(allocation.allocation_root.as_path())
        || allocation
            .upper_dir
            .file_name()
            .is_none_or(|name| name != "upper")
        || allocation
            .work_dir
            .file_name()
            .is_none_or(|name| name != "work")
    {
        return Err(PocError::Integrity(format!(
            "allocation {} does not have adjacent final-path upper/work directories",
            allocation.descriptor.allocation_id
        )));
    }
    Ok(())
}

fn validate_attestation_shape(attestation: &OverlayMountAttestation) -> PocResult<()> {
    if attestation.schema_version != SCHEMA_VERSION
        || attestation.workspace_root
            != attestation
                .workspace_root
                .parent()
                .ok_or_else(|| {
                    PocError::Integrity("attested workspace has no session directory".to_owned())
                })?
                .join("mount")
        || attestation.allocation_upper != attestation.allocation_root.join("upper")
        || attestation.allocation_work != attestation.allocation_root.join("work")
        || attestation.filesystem_type != "overlay"
        || !attestation
            .mount_options
            .iter()
            .any(|option| option == "rw")
        || attestation
            .mount_options
            .iter()
            .any(|option| option == "ro")
        || (attestation.cgroup_procs_path.is_none()
            && (attestation.cgroup_procs_device.is_some()
                || attestation.cgroup_procs_inode.is_some()))
        || (attestation.cgroup_procs_path.is_some()
            && (attestation.cgroup_procs_device.is_none()
                || attestation.cgroup_procs_inode.is_none()))
    {
        return Err(PocError::RecoveryRequired(
            "terminal overlay attestation has an invalid stationary shape".to_owned(),
        ));
    }
    require_attested_source_identities(attestation)
}

#[cfg(target_os = "linux")]
fn require_attested_source_identities(attestation: &OverlayMountAttestation) -> PocResult<()> {
    let allocation = rustix::fs::open(
        &attestation.allocation_root,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open attested allocation root",
            &attestation.allocation_root,
            std::io::Error::from(error),
        )
    })?;
    let upper = rustix::fs::openat(
        &allocation,
        "upper",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open attested allocation upper",
            &attestation.allocation_upper,
            std::io::Error::from(error),
        )
    })?;
    let work = rustix::fs::openat(
        &allocation,
        "work",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open attested allocation work",
            &attestation.allocation_work,
            std::io::Error::from(error),
        )
    })?;
    let owner_path = attestation.allocation_root.join("owner");
    let owner = rustix::fs::openat(
        &allocation,
        "owner",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open attested allocation owner",
            &owner_path,
            std::io::Error::from(error),
        )
    })?;
    let allocation = fd_directory_identity(
        &allocation,
        "stat attested allocation root",
        &attestation.allocation_root,
    )?;
    let upper = fd_directory_identity(
        &upper,
        "stat attested allocation upper",
        &attestation.allocation_upper,
    )?;
    let work = fd_directory_identity(
        &work,
        "stat attested allocation work",
        &attestation.allocation_work,
    )?;
    let owner = fd_directory_identity(&owner, "stat attested allocation owner", &owner_path)?;
    if allocation
        != (DirectoryIdentity {
            device: attestation.allocation_root_device,
            inode: attestation.allocation_root_inode,
        })
        || upper
            != (DirectoryIdentity {
                device: attestation.allocation_upper_device,
                inode: attestation.allocation_upper_inode,
            })
        || work
            != (DirectoryIdentity {
                device: attestation.allocation_work_device,
                inode: attestation.allocation_work_inode,
            })
        || owner
            != (DirectoryIdentity {
                device: attestation.allocation_owner_device,
                inode: attestation.allocation_owner_inode,
            })
    {
        return Err(PocError::RecoveryRequired(
            "durable allocation root/upper/work/owner differ from their mount attestation"
                .to_owned(),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn require_attested_source_identities(_attestation: &OverlayMountAttestation) -> PocResult<()> {
    Err(PocError::Unsupported(
        "overlay source identity requires Linux descriptors".to_owned(),
    ))
}

fn unmounted_attestation(attestation: &OverlayMountAttestation) -> UnmountedOverlay {
    UnmountedOverlay {
        workspace_root: attestation.workspace_root.clone(),
        allocation_root: attestation.allocation_root.clone(),
        allocation_upper: attestation.allocation_upper.clone(),
        allocation_work: attestation.allocation_work.clone(),
    }
}

#[cfg(target_os = "linux")]
fn capture_mount_attestation_anchored(
    session: &OwnedFd,
    workspace: &OwnedFd,
    allocation_root_descriptor: &OwnedFd,
    allocation_owner: &OwnedFd,
    mounted: &OwnedFd,
    expected_mount_id: u64,
    expected_mount_unique_id: u64,
    workspace_identity: DirectoryIdentity,
    workspace_root: &Path,
    allocation_root: &Path,
    allocation_upper: &Path,
    allocation_work: &Path,
    mount_upper_label: &Path,
    mount_work_label: &Path,
    allocation_upper_identity: DirectoryIdentity,
    allocation_work_identity: DirectoryIdentity,
    lease: &MutableLease,
    cgroup: Option<(&Path, u64, u64)>,
) -> PocResult<OverlayMountAttestation> {
    let allocation_root_identity = fd_directory_identity(
        allocation_root_descriptor,
        "stat pinned allocation root for mount attestation",
        allocation_root,
    )?;
    let allocation_owner_identity = fd_directory_identity(
        allocation_owner,
        "stat pinned allocation owner for mount attestation",
        &allocation_root.join("owner"),
    )?;
    let named_owner = rustix::fs::openat(
        allocation_root_descriptor,
        "owner",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open pinned allocation owner for mount attestation",
            allocation_root.join("owner"),
            std::io::Error::from(error),
        )
    })?;
    if fd_directory_identity(
        &named_owner,
        "stat named pinned allocation owner for mount attestation",
        &allocation_root.join("owner"),
    )? != allocation_owner_identity
    {
        return Err(PocError::RecoveryRequired(
            "pinned allocation owner changed before mount attestation".to_owned(),
        ));
    }
    let named = open_session_workspace(session, workspace_root)?;
    if fd_directory_identity(
        workspace,
        "restat exact covered workspace directory",
        workspace_root,
    )? != workspace_identity
        || mount_id_for_fd(mounted)? != expected_mount_id
        || mount_unique_id_for_fd(mounted)? != expected_mount_unique_id
        || mount_id_for_fd(&named)? != expected_mount_id
        || mount_unique_id_for_fd(&named)? != expected_mount_unique_id
    {
        return Err(PocError::RecoveryRequired(
            "anchored workspace binding changed before mount attestation".to_owned(),
        ));
    }
    let observed = current_mount_by_id(expected_mount_id)?.ok_or_else(|| {
        PocError::Integrity("new anchored workspace mount is absent from mountinfo".to_owned())
    })?;
    require_observed_layout(&observed)?;
    require_observed_source_labels(&observed, mount_upper_label, mount_work_label)?;
    require_path_directory_identity(
        allocation_upper,
        allocation_upper_identity,
        "allocation upper changed before mount attestation",
    )?;
    require_path_directory_identity(
        allocation_work,
        allocation_work_identity,
        "allocation work changed before mount attestation",
    )?;
    let target = rustix::fs::fstat(mounted).map_err(|error| {
        PocError::io(
            "stat newly mounted anchored workspace",
            workspace_root,
            std::io::Error::from(error),
        )
    })?;
    let named_target = fd_directory_identity(
        &named,
        "stat protected anchored workspace name",
        workspace_root,
    )?;
    if observed.target_device != target.st_dev
        || named_target.device != target.st_dev
        || named_target.inode != target.st_ino
    {
        return Err(PocError::RecoveryRequired(
            "anchored overlay mountinfo device differs from its exact descriptor".to_owned(),
        ));
    }
    let namespace = std::fs::metadata("/proc/self/ns/mnt")
        .map_err(|error| PocError::io("stat mount namespace", "/proc/self/ns/mnt", error))?;
    Ok(OverlayMountAttestation {
        schema_version: SCHEMA_VERSION,
        allocation_id: lease.allocation_id.clone(),
        session_id: lease.session_id.clone(),
        lease_epoch: lease.lease_epoch,
        owner_epoch: lease.owner_epoch,
        workspace_root: workspace_root.to_path_buf(),
        allocation_root: allocation_root.to_path_buf(),
        allocation_upper: allocation_upper.to_path_buf(),
        allocation_work: allocation_work.to_path_buf(),
        allocation_root_device: allocation_root_identity.device,
        allocation_root_inode: allocation_root_identity.inode,
        allocation_upper_device: allocation_upper_identity.device,
        allocation_upper_inode: allocation_upper_identity.inode,
        allocation_work_device: allocation_work_identity.device,
        allocation_work_inode: allocation_work_identity.inode,
        allocation_owner_device: allocation_owner_identity.device,
        allocation_owner_inode: allocation_owner_identity.inode,
        cgroup_procs_path: cgroup.map(|identity| identity.0.to_path_buf()),
        cgroup_procs_device: cgroup.map(|identity| identity.1),
        cgroup_procs_inode: cgroup.map(|identity| identity.2),
        mount_namespace_inode: namespace.ino(),
        mount_id: observed.mount_id,
        mount_unique_id: expected_mount_unique_id,
        target_device: target.st_dev,
        target_inode: target.st_ino,
        covered_workspace_device: workspace_identity.device,
        covered_workspace_inode: workspace_identity.inode,
        filesystem_type: observed.filesystem_type,
        source: observed.source,
        mount_options: observed.mount_options,
        super_options: observed.super_options,
    })
}

#[cfg(target_os = "linux")]
fn require_observed_layout(observed: &ObservedOverlayMount) -> PocResult<()> {
    if observed.filesystem_type == "overlay"
        && observed.upper_dir.is_some()
        && observed.work_dir.is_some()
    {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "workspace target is not the exact allocation OverlayFS mount".to_owned(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn require_observed_source_labels(
    observed: &ObservedOverlayMount,
    upper: &Path,
    work: &Path,
) -> PocResult<()> {
    if observed.upper_dir.as_deref() == Some(upper) && observed.work_dir.as_deref() == Some(work) {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "workspace overlay reports different upper/work mount labels".to_owned(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn current_mount(target: &Path) -> PocResult<Option<ObservedOverlayMount>> {
    let text = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| PocError::io("read mountinfo", "/proc/self/mountinfo", error))?;
    current_mount_from_mountinfo(&text, target)
}

#[cfg(target_os = "linux")]
fn current_mount_from_mountinfo(
    text: &str,
    target: &Path,
) -> PocResult<Option<ObservedOverlayMount>> {
    let mut matches = Vec::new();
    for line in text.lines() {
        let entry = parse_mountinfo_line(line)?;
        if entry.workspace_root == target {
            matches.push(entry);
        }
    }
    if matches.len() > 1 {
        return Err(PocError::RecoveryRequired(format!(
            "workspace target has {} stacked mounts",
            matches.len()
        )));
    }
    Ok(matches.pop())
}

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub fn require_attested_mount_unstacked_from_mountinfo(
    text: &str,
    attestation: &OverlayMountAttestation,
) -> PocResult<()> {
    validate_attestation_shape(attestation)?;
    let observed =
        current_mount_from_mountinfo(text, &attestation.workspace_root)?.ok_or_else(|| {
            PocError::RecoveryRequired(
                "protected overlay mount name is absent from mountinfo".to_owned(),
            )
        })?;
    if observed.mount_id != attestation.mount_id
        || observed.target_device != attestation.target_device
        || !observed_matches_attestation(&observed, attestation)
    {
        return Err(PocError::RecoveryRequired(
            "protected overlay mount name differs from its attestation".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_unstacked_named_mount(
    target: &Path,
    expected_mount_id: u64,
    expected_target_device: u64,
) -> PocResult<()> {
    let observed = current_mount(target)?.ok_or_else(|| {
        PocError::RecoveryRequired(
            "protected overlay mount name disappeared before strict unmount".to_owned(),
        )
    })?;
    if observed.mount_id != expected_mount_id || observed.target_device != expected_target_device {
        return Err(PocError::RecoveryRequired(
            "protected overlay mount name was redirected before strict unmount".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_session_workspace(session: &OwnedFd, display_path: &Path) -> PocResult<OwnedFd> {
    rustix::fs::openat(
        session,
        "mount",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open protected session workspace",
            display_path,
            std::io::Error::from(error),
        )
    })
}

#[cfg(target_os = "linux")]
fn authenticate_attested_named_mount(
    attestation: &OverlayMountAttestation,
    session: &OwnedFd,
) -> PocResult<OwnedFd> {
    let named = open_session_workspace(session, &attestation.workspace_root)?;
    let identity = fd_directory_identity(
        &named,
        "stat protected session workspace",
        &attestation.workspace_root,
    )?;
    if mount_id_for_fd(&named)? != attestation.mount_id
        || mount_unique_id_for_fd(&named)? != attestation.mount_unique_id
        || identity.device != attestation.target_device
        || identity.inode != attestation.target_inode
    {
        return Err(PocError::RecoveryRequired(
            "protected session workspace is not the exact attested mount".to_owned(),
        ));
    }
    require_unstacked_named_mount(
        &attestation.workspace_root,
        attestation.mount_id,
        attestation.target_device,
    )?;
    Ok(named)
}

#[cfg(target_os = "linux")]
fn require_restored_covered_workspace(
    attestation: &OverlayMountAttestation,
    session: &OwnedFd,
) -> PocResult<OwnedFd> {
    let restored = open_session_workspace(session, &attestation.workspace_root)?;
    let identity = fd_directory_identity(
        &restored,
        "stat restored covered session workspace",
        &attestation.workspace_root,
    )?;
    if identity.device != attestation.covered_workspace_device
        || identity.inode != attestation.covered_workspace_inode
        || mount_unique_id_for_fd(&restored)? == attestation.mount_unique_id
    {
        return Err(PocError::RecoveryRequired(
            "covered session workspace authority was not restored after strict unmount".to_owned(),
        ));
    }
    Ok(restored)
}

#[cfg(target_os = "linux")]
fn current_mount_by_id(mount_id: u64) -> PocResult<Option<ObservedOverlayMount>> {
    let text = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| PocError::io("read mountinfo", "/proc/self/mountinfo", error))?;
    let mut matches = Vec::new();
    for line in text.lines() {
        let entry = parse_mountinfo_line(line)?;
        if entry.mount_id == mount_id {
            matches.push(entry);
        }
    }
    if matches.len() > 1 {
        return Err(PocError::RecoveryRequired(format!(
            "mount namespace contains duplicate mount ID {mount_id}"
        )));
    }
    Ok(matches.pop())
}

#[cfg(target_os = "linux")]
pub(crate) fn mount_id_for_fd(directory: &OwnedFd) -> PocResult<u64> {
    let path = PathBuf::from(format!("/proc/self/fdinfo/{}", directory.as_raw_fd()));
    let text = std::fs::read_to_string(&path)
        .map_err(|error| PocError::io("read pinned workspace fdinfo", &path, error))?;
    let mut mount_ids = text.lines().filter_map(|line| {
        line.strip_prefix("mnt_id:")
            .map(str::trim)
            .and_then(|value| value.parse::<u64>().ok())
    });
    let mount_id = mount_ids.next().ok_or_else(|| {
        PocError::RecoveryRequired("pinned workspace fdinfo has no valid mount ID".to_owned())
    })?;
    if mount_ids.next().is_some() {
        return Err(PocError::RecoveryRequired(
            "pinned workspace fdinfo has multiple mount IDs".to_owned(),
        ));
    }
    Ok(mount_id)
}

#[cfg(target_os = "linux")]
const STATX_MNT_ID_UNIQUE_MASK: u32 = 0x0000_4000;

#[cfg(target_os = "linux")]
fn mount_unique_id_for_fd(directory: &OwnedFd) -> PocResult<u64> {
    statx_mount_unique_id(
        directory,
        std::ffi::OsStr::new(""),
        rustix::fs::AtFlags::EMPTY_PATH | rustix::fs::AtFlags::NO_AUTOMOUNT,
        Path::new("pinned mount descriptor"),
    )
}

#[cfg(target_os = "linux")]
fn statx_mount_unique_id(
    directory: &OwnedFd,
    path: &std::ffi::OsStr,
    flags: rustix::fs::AtFlags,
    display_path: &Path,
) -> PocResult<u64> {
    let path = CString::new(path.as_bytes())
        .map_err(|_| PocError::Integrity("mount identity path contains NUL".to_owned()))?;
    let mask = rustix::fs::StatxFlags::from_bits_retain(STATX_MNT_ID_UNIQUE_MASK);
    let status = rustix::fs::statx(directory, path.as_c_str(), flags, mask).map_err(|error| {
        PocError::io(
            "statx unique mount identity",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    if status.stx_mask & STATX_MNT_ID_UNIQUE_MASK == 0 {
        return Err(PocError::Unsupported(
            "Linux statx did not report STATX_MNT_ID_UNIQUE".to_owned(),
        ));
    }
    Ok(status.stx_mnt_id)
}

#[cfg(target_os = "linux")]
fn require_attested_mount_namespace(attestation: &OverlayMountAttestation) -> PocResult<()> {
    let namespace = std::fs::metadata("/proc/self/ns/mnt").map_err(|error| {
        PocError::io("stat recovery mount namespace", "/proc/self/ns/mnt", error)
    })?;
    if namespace.ino() != attestation.mount_namespace_inode {
        return Err(PocError::RecoveryRequired(
            "terminal recovery is not running in the attested mount namespace".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_observed_attestation(
    observed: &ObservedOverlayMount,
    attestation: &OverlayMountAttestation,
    require_original_mountpoint: bool,
) -> PocResult<()> {
    require_observed_layout(observed)?;
    if observed.mount_id != attestation.mount_id
        || observed.target_device != attestation.target_device
        || (require_original_mountpoint && observed.workspace_root != attestation.workspace_root)
        || observed.filesystem_type != attestation.filesystem_type
        || observed.source != attestation.source
        || !mount_options_match_attestation_or_frozen(
            &observed.mount_options,
            &attestation.mount_options,
        )
        || observed.super_options != attestation.super_options
    {
        return Err(PocError::RecoveryRequired(
            "live terminal workspace mount differs from its durable attestation".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn observed_preserves_attested_overlay(
    observed: &ObservedOverlayMount,
    attestation: &OverlayMountAttestation,
) -> bool {
    observed_preserves_process_audit_identity(
        observed,
        &process_audit_identity_from_attestation_unchecked(attestation),
    )
}

#[cfg(target_os = "linux")]
fn observed_preserves_process_audit_identity(
    observed: &ObservedOverlayMount,
    identity: &OverlayProcessAuditIdentity,
) -> bool {
    observed.filesystem_type == identity.filesystem_type
        && observed.target_device == identity.target_device
        && observed.source == identity.source
        && observed.super_options == identity.super_options
        && observed.upper_dir.is_some()
        && observed.work_dir.is_some()
}

#[cfg(target_os = "linux")]
fn observed_matches_process_audit_identity(
    observed: &ObservedOverlayMount,
    identity: &OverlayProcessAuditIdentity,
) -> bool {
    observed.mount_id == identity.mount_id
        && observed.workspace_root == identity.workspace_root
        && observed_preserves_process_audit_identity(observed, identity)
        && mount_options_match_attestation_or_frozen(
            &observed.mount_options,
            &identity.mount_options,
        )
}

#[cfg(target_os = "linux")]
fn observed_matches_attestation(
    observed: &ObservedOverlayMount,
    attestation: &OverlayMountAttestation,
) -> bool {
    observed.mount_id == attestation.mount_id
        && mount_options_match_attestation_or_frozen(
            &observed.mount_options,
            &attestation.mount_options,
        )
        && observed_preserves_attested_overlay(observed, attestation)
}

#[cfg(target_os = "linux")]
fn mount_options_match_attestation_or_frozen(observed: &[String], attested: &[String]) -> bool {
    if observed == attested {
        return true;
    }
    let mut expected_frozen = attested.to_vec();
    for option in &mut expected_frozen {
        if option == "rw" {
            *option = "ro".to_owned();
        }
    }
    expected_frozen != attested && observed == expected_frozen
}

#[cfg(target_os = "linux")]
fn mount_options_are_read_only(options: &[String]) -> bool {
    options.iter().any(|option| option == "ro") && !options.iter().any(|option| option == "rw")
}

#[cfg(target_os = "linux")]
fn parse_mountinfo_line(line: &str) -> PocResult<ObservedOverlayMount> {
    let (left, right) = line.split_once(" - ").ok_or_else(|| {
        PocError::Integrity("kernel mountinfo row has no field separator".to_owned())
    })?;
    let left_fields = left.split_whitespace().collect::<Vec<_>>();
    let right_fields = right.split_whitespace().collect::<Vec<_>>();
    if left_fields.len() < 6 || right_fields.len() < 3 {
        return Err(PocError::Integrity(
            "kernel mountinfo row has too few fields".to_owned(),
        ));
    }
    let mount_id = left_fields[0]
        .parse()
        .map_err(|_| PocError::Integrity("kernel mountinfo has invalid mount ID".to_owned()))?;
    let parent_mount_id = left_fields[1].parse().map_err(|_| {
        PocError::Integrity("kernel mountinfo has invalid parent mount ID".to_owned())
    })?;
    let (major, minor) = left_fields[2].split_once(':').ok_or_else(|| {
        PocError::Integrity("kernel mountinfo has invalid mount device".to_owned())
    })?;
    let major = major
        .parse::<u32>()
        .map_err(|_| PocError::Integrity("kernel mountinfo has invalid device major".to_owned()))?;
    let minor = minor
        .parse::<u32>()
        .map_err(|_| PocError::Integrity("kernel mountinfo has invalid device minor".to_owned()))?;
    let target_device = libc::makedev(major, minor) as u64;
    let workspace_root = PathBuf::from(unescape_mountinfo(left_fields[4]).ok_or_else(|| {
        PocError::Integrity("kernel mountinfo has invalid target escaping".to_owned())
    })?);
    let mount_options = left_fields[5].split(',').map(str::to_owned).collect();
    let filesystem_type = right_fields[0].to_owned();
    let source = unescape_mountinfo(right_fields[1]).ok_or_else(|| {
        PocError::Integrity("kernel mountinfo has invalid source escaping".to_owned())
    })?;
    let super_options = right_fields[2]
        .split(',')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let upper_dir = mount_option_path(&super_options, "upperdir=");
    let work_dir = mount_option_path(&super_options, "workdir=");
    Ok(ObservedOverlayMount {
        mount_id,
        parent_mount_id,
        target_device,
        workspace_root,
        filesystem_type,
        source,
        mount_options,
        super_options,
        upper_dir,
        work_dir,
    })
}

#[cfg(target_os = "linux")]
fn mount_option_path(options: &[String], prefix: &str) -> Option<PathBuf> {
    options
        .iter()
        .find_map(|option| option.strip_prefix(prefix))
        .and_then(unescape_mountinfo)
        .map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn unescape_mountinfo(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if index + 3 >= bytes.len()
                || !bytes[index + 1..=index + 3].iter().all(u8::is_ascii_digit)
            {
                return None;
            }
            let octal = std::str::from_utf8(&bytes[index + 1..=index + 3]).ok()?;
            output.push(u8::from_str_radix(octal, 8).ok()?);
            index += 4;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}
