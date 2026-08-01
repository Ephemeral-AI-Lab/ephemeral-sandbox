#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
#[cfg(unix)]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(target_os = "linux")]
use crate::overlay_adapter::{
    mountinfo_text_has_process_audit_mount, process_audit_identity_from_attestation,
    process_audit_mount_tree_ids, OverlayProcessAuditIdentity,
};
use crate::overlay_adapter::{
    AttestedMountCleanupState, OverlayMountAttestation, PermanentOverlayMount,
};
use crate::{unix_time_ms, PocError, PocResult};

const POLL_INTERVAL: Duration = Duration::from_millis(1);
const TERM_GRACE: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandReceipt {
    pub schema_version: u32,
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub timed_out: bool,
    pub process_group_id: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessAudit {
    pub workspace_root: PathBuf,
    pub cgroup_members: Vec<i32>,
    pub cwd_or_root_pins: Vec<i32>,
    pub fd_pins: Vec<i32>,
    pub writable_map_pins: Vec<i32>,
    pub mount_namespace_pins: Vec<i32>,
}

#[derive(Debug)]
#[doc(hidden)]
pub struct AttestedCgroupMembership {
    path: PathBuf,
    #[cfg(target_os = "linux")]
    directory: OwnedFd,
    #[cfg(target_os = "linux")]
    file_name: OsString,
    expected_device: u64,
    expected_inode: u64,
}

impl AttestedCgroupMembership {
    #[cfg(target_os = "linux")]
    pub fn open(path: &Path, expected_device: u64, expected_inode: u64) -> PocResult<Self> {
        let (directory, file_name) = open_cgroup_parent(path)?;
        let membership = Self {
            path: path.to_path_buf(),
            directory,
            file_name,
            expected_device,
            expected_inode,
        };
        let _ = membership.read_exact()?;
        Ok(membership)
    }

    #[cfg(target_os = "linux")]
    fn open_current(path: &Path) -> PocResult<Self> {
        let (directory, file_name) = open_cgroup_parent(path)?;
        let descriptor = rustix::fs::openat(
            &directory,
            &file_name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            PocError::io("open live cgroup.procs", path, std::io::Error::from(error))
        })?;
        let status = rustix::fs::fstat(&descriptor).map_err(|error| {
            PocError::io("stat live cgroup.procs", path, std::io::Error::from(error))
        })?;
        let membership = Self {
            path: path.to_path_buf(),
            directory,
            file_name,
            expected_device: status.st_dev,
            expected_inode: status.st_ino,
        };
        let _ = membership.read_exact()?;
        Ok(membership)
    }

    #[cfg(target_os = "linux")]
    fn open_write_exact(&self) -> PocResult<File> {
        let descriptor = rustix::fs::openat(
            &self.directory,
            &self.file_name,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            PocError::io(
                "open anchored live cgroup.procs for child",
                &self.path,
                std::io::Error::from(error),
            )
        })?;
        let status = rustix::fs::fstat(&descriptor).map_err(|error| {
            PocError::io(
                "stat anchored live cgroup.procs for child",
                &self.path,
                std::io::Error::from(error),
            )
        })?;
        if status.st_dev != self.expected_device || status.st_ino != self.expected_inode {
            return Err(PocError::RecoveryRequired(
                "live cgroup.procs identity changed under its pinned directory".to_owned(),
            ));
        }
        Ok(File::from(descriptor))
    }

    #[cfg(target_os = "linux")]
    pub(crate) const fn device(&self) -> u64 {
        self.expected_device
    }

    #[cfg(target_os = "linux")]
    pub(crate) const fn inode(&self) -> u64 {
        self.expected_inode
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(target_os = "linux")]
    pub fn read_exact(&self) -> PocResult<String> {
        let descriptor = rustix::fs::openat(
            &self.directory,
            &self.file_name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            PocError::io(
                "open anchored attested cgroup.procs",
                &self.path,
                std::io::Error::from(error),
            )
        })?;
        let opened = rustix::fs::fstat(&descriptor).map_err(|error| {
            PocError::io(
                "stat anchored attested cgroup.procs",
                &self.path,
                std::io::Error::from(error),
            )
        })?;
        if opened.st_dev != self.expected_device || opened.st_ino != self.expected_inode {
            return Err(PocError::RecoveryRequired(
                "terminal cgroup.procs identity changed under its pinned directory".to_owned(),
            ));
        }
        let mut file = File::from(descriptor);
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(|error| {
            PocError::io("read anchored attested cgroup.procs", &self.path, error)
        })?;
        Ok(text)
    }
}

#[cfg(target_os = "linux")]
fn open_cgroup_parent(path: &Path) -> PocResult<(OwnedFd, OsString)> {
    if !path.is_absolute() {
        return Err(PocError::RecoveryRequired(format!(
            "attested cgroup.procs path is not absolute: {}",
            path.display()
        )));
    }
    let file_name = path.file_name().ok_or_else(|| {
        PocError::RecoveryRequired(format!(
            "attested cgroup.procs has no file name: {}",
            path.display()
        ))
    })?;
    let mut file_name_components = Path::new(file_name).components();
    if !matches!(file_name_components.next(), Some(std::path::Component::Normal(component)) if component == file_name)
        || file_name_components.next().is_some()
    {
        return Err(PocError::RecoveryRequired(format!(
            "attested cgroup.procs has no normalized file name: {}",
            path.display()
        )));
    }
    let file_name = file_name.to_os_string();
    let parent = path.parent().ok_or_else(|| {
        PocError::RecoveryRequired(format!(
            "attested cgroup.procs has no parent directory: {}",
            path.display()
        ))
    })?;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut directory = rustix::fs::open(Path::new("/"), flags, rustix::fs::Mode::empty())
        .map_err(|error| {
            PocError::io(
                "open cgroup path root",
                Path::new("/"),
                std::io::Error::from(error),
            )
        })?;
    for component in parent.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(component) => {
                directory =
                    rustix::fs::openat(&directory, component, flags, rustix::fs::Mode::empty())
                        .map_err(|error| {
                            PocError::io(
                                "open anchored cgroup directory",
                                parent,
                                std::io::Error::from(error),
                            )
                        })?;
            }
            _ => {
                return Err(PocError::RecoveryRequired(format!(
                    "attested cgroup.procs path is not normalized: {}",
                    path.display()
                )));
            }
        }
    }
    Ok((directory, file_name))
}

impl ProcessAudit {
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.cgroup_members.is_empty()
            && self.cwd_or_root_pins.is_empty()
            && self.fd_pins.is_empty()
            && self.writable_map_pins.is_empty()
            && self.mount_namespace_pins.is_empty()
    }
}

/// Immutable process-audit identity for a terminal workspace. Process
/// membership is decided by kernel mount IDs (and exact overlay identity in a
/// foreign mount namespace), never by a mutable absolute pathname.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct AnchoredWorkspaceAuditIdentity {
    mount: OverlayProcessAuditIdentity,
    recovery: bool,
}

#[cfg(not(target_os = "linux"))]
#[derive(Clone, Debug)]
pub(crate) struct AnchoredWorkspaceAuditIdentity;

#[cfg(target_os = "linux")]
pub(crate) fn anchored_workspace_audit_identity(
    attestation: &OverlayMountAttestation,
    _workspace: &OwnedFd,
    mount_state: AttestedMountCleanupState,
) -> PocResult<AnchoredWorkspaceAuditIdentity> {
    let current_mount_namespace_inode = fs::metadata("/proc/self/ns/mnt")
        .map_err(|error| PocError::io("stat current mount namespace", "/proc/self/ns/mnt", error))?
        .ino();
    if current_mount_namespace_inode != attestation.mount_namespace_inode {
        return Err(PocError::RecoveryRequired(
            "terminal process audit is not in the attested mount namespace".to_owned(),
        ));
    }
    let mount = process_audit_identity_from_attestation(attestation)?;
    let mount_ids = process_audit_mount_tree_ids(&mount)?;
    match mount_state {
        AttestedMountCleanupState::MountedExact if !mount_ids.contains(&attestation.mount_id) => {
            return Err(PocError::RecoveryRequired(
                "attested terminal mount disappeared before process audit".to_owned(),
            ));
        }
        AttestedMountCleanupState::AlreadyAbsent if mount_ids.contains(&attestation.mount_id) => {
            return Err(PocError::RecoveryRequired(
                "absent terminal mount remains visible before process audit".to_owned(),
            ));
        }
        _ => {}
    }
    Ok(AnchoredWorkspaceAuditIdentity {
        mount,
        recovery: true,
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn live_workspace_audit_identity(
    overlay: &PermanentOverlayMount,
) -> PocResult<AnchoredWorkspaceAuditIdentity> {
    let mount = overlay.process_audit_identity()?;
    let mount_ids = process_audit_mount_tree_ids(&mount)?;
    if !mount_ids.contains(&mount.mount_id) {
        return Err(PocError::RecoveryRequired(
            "live terminal mount disappeared before process audit".to_owned(),
        ));
    }
    Ok(AnchoredWorkspaceAuditIdentity {
        mount,
        recovery: false,
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn anchored_workspace_audit_identity(
    _attestation: &OverlayMountAttestation,
    _workspace: &std::os::fd::OwnedFd,
    _mount_state: AttestedMountCleanupState,
) -> PocResult<AnchoredWorkspaceAuditIdentity> {
    Err(PocError::Unsupported(
        "descriptor-anchored terminal process audit requires Linux mount IDs".to_owned(),
    ))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn live_workspace_audit_identity(
    _overlay: &PermanentOverlayMount,
) -> PocResult<AnchoredWorkspaceAuditIdentity> {
    Err(PocError::Unsupported(
        "descriptor-anchored terminal process audit requires Linux mount IDs".to_owned(),
    ))
}

/// Drain processes which reference the descriptor-pinned terminal mount.
/// Every signal is guarded by a fresh mount-ID audit through the already-open
/// pidfd, so ancestor replacement cannot redirect recovery at an unrelated
/// process tree.
#[cfg(target_os = "linux")]
pub(crate) fn terminate_terminal_workspace_references_anchored(
    identity: &AnchoredWorkspaceAuditIdentity,
    cgroup_membership: Option<&AttestedCgroupMembership>,
) -> PocResult<(Vec<i32>, ProcessAudit)> {
    let mut signaled = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(1);

    loop {
        let audit = audit_workspace_references_anchored(identity, cgroup_membership, true)?;
        if audit.is_clear() {
            for pidfd in signaled.values() {
                reap_pidfd_if_child(pidfd)?;
            }
            return Ok((signaled.into_keys().collect(), audit));
        }
        if Instant::now() >= deadline {
            return Err(PocError::RecoveryRequired(format!(
                "anchored terminal workspace process drain timed out: {audit:?}"
            )));
        }

        for pid in audit_pids(&audit) {
            let Some(pidfd) = open_pidfd_allow_missing(pid)? else {
                continue;
            };
            let fresh = audit_workspace_references_anchored(identity, cgroup_membership, true)?;
            if audit_contains_pid(&fresh, pid) {
                signal_pidfd(&pidfd, libc::SIGTERM)?;
                signaled.insert(pid, pidfd);
            }
        }

        let term_deadline = Instant::now() + TERM_GRACE;
        loop {
            let remaining = audit_workspace_references_anchored(identity, cgroup_membership, true)?;
            if remaining.is_clear() || Instant::now() >= term_deadline {
                break;
            }
            thread::sleep(POLL_INTERVAL);
        }

        let remaining = audit_workspace_references_anchored(identity, cgroup_membership, true)?;
        for pid in audit_pids(&remaining) {
            let Some(pidfd) = open_pidfd_allow_missing(pid)? else {
                continue;
            };
            let fresh = audit_workspace_references_anchored(identity, cgroup_membership, true)?;
            if audit_contains_pid(&fresh, pid) {
                signal_pidfd(&pidfd, libc::SIGKILL)?;
                signaled.insert(pid, pidfd);
            }
        }
        for pidfd in signaled.values() {
            reap_pidfd_if_child(pidfd)?;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn terminate_terminal_workspace_references_anchored(
    _identity: &AnchoredWorkspaceAuditIdentity,
    _cgroup_membership: Option<&AttestedCgroupMembership>,
) -> PocResult<(Vec<i32>, ProcessAudit)> {
    Err(PocError::Unsupported(
        "descriptor-anchored terminal process recovery requires Linux pidfds".to_owned(),
    ))
}

pub(crate) fn audit_terminal_workspace(
    workspace_root: &Path,
    cgroup_membership: Option<&AttestedCgroupMembership>,
    include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    #[cfg(target_os = "linux")]
    {
        audit_workspace_references(
            workspace_root,
            None,
            cgroup_membership,
            include_mount_namespaces,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (workspace_root, cgroup_membership, include_mount_namespaces);
        Err(PocError::Unsupported(
            "restart-safe terminal process audit requires Linux /proc".to_owned(),
        ))
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn audit_terminal_workspace_anchored(
    identity: &AnchoredWorkspaceAuditIdentity,
    cgroup_membership: Option<&AttestedCgroupMembership>,
    include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    audit_workspace_references_anchored(identity, cgroup_membership, include_mount_namespaces)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn audit_terminal_workspace_anchored(
    _identity: &AnchoredWorkspaceAuditIdentity,
    _cgroup_membership: Option<&AttestedCgroupMembership>,
    _include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    Err(PocError::Unsupported(
        "descriptor-anchored terminal process audit requires Linux /proc".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn audit_pids(audit: &ProcessAudit) -> BTreeSet<i32> {
    audit
        .cgroup_members
        .iter()
        .chain(&audit.cwd_or_root_pins)
        .chain(&audit.fd_pins)
        .chain(&audit.writable_map_pins)
        .chain(&audit.mount_namespace_pins)
        .copied()
        .filter(|pid| u32::try_from(*pid).ok() != Some(std::process::id()))
        .collect()
}

#[cfg(target_os = "linux")]
fn audit_contains_pid(audit: &ProcessAudit, pid: i32) -> bool {
    audit_pids(audit).contains(&pid)
}

#[cfg(target_os = "linux")]
fn open_pidfd_allow_missing(pid: i32) -> PocResult<Option<OwnedFd>> {
    // SAFETY: pidfd_open consumes a scalar PID and flags value and returns a
    // new owned descriptor on success.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    if fd >= 0 {
        // SAFETY: the successful syscall returned a new descriptor owned by
        // this function.
        return Ok(Some(unsafe { OwnedFd::from_raw_fd(fd) }));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(PocError::io(
            "open terminal process pidfd",
            Path::new("/proc"),
            error,
        ))
    }
}

#[cfg(target_os = "linux")]
fn signal_pidfd(pidfd: &OwnedFd, signal: i32) -> PocResult<()> {
    loop {
        // SAFETY: pidfd_send_signal consumes a valid pidfd, a platform signal,
        // and null siginfo with zero flags.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ESRCH) => return Ok(()),
            _ => {
                return Err(PocError::io(
                    "signal terminal process pidfd",
                    Path::new("/proc"),
                    error,
                ));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn reap_pidfd_if_child(pidfd: &OwnedFd) -> PocResult<()> {
    let mut status = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    loop {
        // SAFETY: waitid receives P_PIDFD with a live descriptor, writable
        // siginfo storage, and nonblocking child-reap flags. It cannot select
        // a different process if the numeric PID has been recycled.
        let result = unsafe {
            libc::waitid(
                libc::P_PIDFD,
                pidfd.as_raw_fd() as libc::id_t,
                status.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return Ok(()),
            _ => {
                return Err(PocError::io(
                    "reap terminal process pidfd",
                    Path::new("/proc"),
                    error,
                ));
            }
        }
    }
}

/// Direct process-group runner used by the PoC when the product namespace
/// protocol would add unrelated orchestration. The optional cgroup membership
/// file catches descendants that escape their original process group.
#[derive(Debug)]
pub struct ManagedProcessTree {
    workspace_root: PathBuf,
    #[cfg(not(target_os = "linux"))]
    cgroup_procs_path: Option<PathBuf>,
    #[cfg(target_os = "linux")]
    cgroup_membership: Option<AttestedCgroupMembership>,
    process_groups: BTreeSet<u32>,
    children: Vec<Child>,
    fenced: bool,
}

impl ManagedProcessTree {
    pub fn new(workspace_root: PathBuf, cgroup_procs_path: Option<PathBuf>) -> PocResult<Self> {
        #[cfg(target_os = "linux")]
        let cgroup_membership = cgroup_procs_path
            .as_deref()
            .map(AttestedCgroupMembership::open_current)
            .transpose()?;
        Ok(Self {
            workspace_root,
            #[cfg(not(target_os = "linux"))]
            cgroup_procs_path,
            #[cfg(target_os = "linux")]
            cgroup_membership,
            process_groups: BTreeSet::new(),
            children: Vec::new(),
            fenced: false,
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn cgroup_attestation(&self) -> Option<(&Path, u64, u64)> {
        self.cgroup_membership
            .as_ref()
            .map(|membership| (membership.path(), membership.device(), membership.inode()))
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) const fn cgroup_attestation(&self) -> Option<(&Path, u64, u64)> {
        None
    }

    #[must_use]
    pub const fn is_fenced(&self) -> bool {
        self.fenced
    }

    pub fn fence(&mut self) {
        self.fenced = true;
    }

    pub fn unfence(&mut self) {
        self.fenced = false;
    }

    /// Finish reaping commands admitted before the terminal fence. Normal
    /// command execution is synchronous, so this is an immediate proof on the
    /// ordinary path and a bounded recovery check if an earlier poll failed.
    pub fn drain_in_flight_commands(&mut self, timeout: Duration) -> PocResult<()> {
        if !self.fenced {
            return Err(PocError::Integrity(
                "command drain requires terminal admission fencing".to_owned(),
            ));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let mut index = 0;
            while index < self.children.len() {
                let status = self.children[index].try_wait().map_err(|error| {
                    PocError::io("poll fenced managed command", &self.workspace_root, error)
                })?;
                if status.is_none() {
                    index += 1;
                    continue;
                }
                let process_group_id = self.children[index].id();
                let mut child = self.children.swap_remove(index);
                child.wait().map_err(|error| {
                    PocError::io("reap fenced managed command", &self.workspace_root, error)
                })?;
                if !process_group_exists(process_group_id)? {
                    self.process_groups.remove(&process_group_id);
                }
            }
            if self.children.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(PocError::RecoveryRequired(
                    "in-flight command drain exceeded its terminal fence budget".to_owned(),
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn run(
        &mut self,
        program: &Path,
        arguments: &[String],
        timeout: Duration,
    ) -> PocResult<CommandReceipt> {
        if self.fenced {
            return Err(PocError::Integrity(
                "command admission is terminally fenced".to_owned(),
            ));
        }
        ensure_child_subreaper()?;
        let started_unix_ms = unix_time_ms()?;
        let mut command = Command::new(program);
        command
            .args(arguments)
            .current_dir(&self.workspace_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(target_os = "linux")]
        let _child_cgroup = install_child_isolation(&mut command, self.cgroup_membership.as_ref())?;
        #[cfg(not(target_os = "linux"))]
        let _child_cgroup =
            install_child_isolation(&mut command, self.cgroup_procs_path.as_deref())?;
        let child = command
            .spawn()
            .map_err(|error| PocError::io("spawn managed command", program, error))?;
        let process_group_id = child.id();
        self.process_groups.insert(process_group_id);
        self.children.push(child);

        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let status = loop {
            let child = self
                .children
                .last_mut()
                .ok_or_else(|| PocError::Integrity("managed child disappeared".to_owned()))?;
            if let Some(status) = child
                .try_wait()
                .map_err(|error| PocError::io("poll managed command", program, error))?
            {
                break status;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                signal_process_group(process_group_id, libc::SIGKILL)?;
                break child
                    .wait()
                    .map_err(|error| PocError::io("reap timed-out command", program, error))?;
            }
            thread::sleep(POLL_INTERVAL);
        };
        let _ = self.children.pop();
        if !process_group_exists(process_group_id)? {
            self.process_groups.remove(&process_group_id);
        }
        command_receipt(
            program,
            arguments,
            started_unix_ms,
            status,
            timed_out,
            process_group_id,
        )
    }

    #[cfg(unix)]
    #[allow(clippy::undocumented_unsafe_blocks)]
    pub fn probe_file(
        &mut self,
        relative_path: &Path,
        contains: Option<&[u8]>,
        timeout: Duration,
    ) -> PocResult<CommandReceipt> {
        if self.fenced {
            return Err(PocError::Integrity(
                "readiness admission is terminally fenced".to_owned(),
            ));
        }
        if relative_path.as_os_str().is_empty()
            || relative_path.is_absolute()
            || !relative_path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(PocError::Integrity(format!(
                "readiness path must be a normalized relative path: {}",
                relative_path.display()
            )));
        }
        if contains.is_some_and(<[u8]>::is_empty) {
            return Err(PocError::Integrity(
                "readiness content sentinel must not be empty".to_owned(),
            ));
        }

        ensure_child_subreaper()?;
        let workspace = File::open(&self.workspace_root).map_err(|error| {
            PocError::io("open readiness workspace", &self.workspace_root, error)
        })?;
        let relative = CString::new(relative_path.as_os_str().as_bytes()).map_err(|_| {
            PocError::Integrity("readiness path contains an interior NUL byte".to_owned())
        })?;
        let prefix = contains.map(build_prefix_table).unwrap_or_default();
        #[cfg(target_os = "linux")]
        let cgroup = open_direct_child_cgroup(self.cgroup_membership.as_ref())?;
        #[cfg(not(target_os = "linux"))]
        let cgroup = open_direct_child_cgroup(self.cgroup_procs_path.as_deref())?;
        let cgroup_fd = cgroup.as_ref().map(AsRawFd::as_raw_fd);
        let started_unix_ms = unix_time_ms()?;

        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(PocError::io(
                "fork external readiness probe",
                &self.workspace_root,
                std::io::Error::last_os_error(),
            ));
        }
        if pid == 0 {
            unsafe {
                if libc::setpgid(0, 0) != 0 {
                    libc::_exit(125);
                }
                if let Some(fd) = cgroup_fd {
                    let membership = b"0\n";
                    if libc::write(fd, membership.as_ptr().cast(), membership.len())
                        != membership.len() as isize
                    {
                        libc::_exit(126);
                    }
                }
                let fd = open_readiness_at(workspace.as_raw_fd(), relative.as_ptr());
                if fd < 0 {
                    libc::_exit(2);
                }
                let mut metadata: libc::stat = std::mem::zeroed();
                if libc::fstat(fd, std::ptr::addr_of_mut!(metadata)) != 0 {
                    libc::close(fd);
                    libc::_exit(3);
                }
                let passed = probe_fd(fd, contains, &prefix);
                libc::close(fd);
                libc::_exit(if passed { 0 } else { 4 });
            }
        }

        let process_group_id = u32::try_from(pid)
            .map_err(|_| PocError::Integrity(format!("readiness PID {pid} is invalid")))?;
        self.process_groups.insert(process_group_id);
        let wait_result = wait_direct_child(pid, timeout);
        self.process_groups.remove(&process_group_id);
        let (status, timed_out) = wait_result?;
        let exit_code = if libc::WIFEXITED(status) {
            Some(libc::WEXITSTATUS(status))
        } else {
            None
        };
        let mut arguments = vec!["--path".to_owned(), relative_path.display().to_string()];
        if let Some(needle) = contains {
            arguments.push("--contains".to_owned());
            arguments.push(String::from_utf8_lossy(needle).into_owned());
        }
        Ok(CommandReceipt {
            schema_version: crate::SCHEMA_VERSION,
            program: PathBuf::from("adapter-direct-open-read-metadata"),
            arguments,
            started_unix_ms,
            finished_unix_ms: unix_time_ms()?,
            exit_code,
            success: exit_code == Some(0) && !timed_out,
            timed_out,
            process_group_id,
        })
    }

    #[cfg(not(unix))]
    pub fn probe_file(
        &mut self,
        _relative_path: &Path,
        _contains: Option<&[u8]>,
        _timeout: Duration,
    ) -> PocResult<CommandReceipt> {
        Err(PocError::Unsupported(
            "external readiness requires unix".to_owned(),
        ))
    }

    /// Best-effort nonterminal cleanup for a live runner and its `Drop` path.
    /// Terminal sealing and recovery must use the anchored pidfd drain instead
    /// because these recorded numeric process-group and cgroup PIDs can race
    /// with reuse.
    pub fn stop_kill_reap(&mut self) -> PocResult<Vec<i32>> {
        self.fence();
        let mut signaled = BTreeSet::new();
        for process_group in &self.process_groups {
            if process_group_exists(*process_group)? {
                signal_process_group_allow_missing(*process_group, libc::SIGTERM)?;
                signaled.insert(i32::try_from(*process_group).map_err(|_| {
                    PocError::Integrity(format!("process group {process_group} does not fit i32"))
                })?);
            }
        }
        for pid in self.cgroup_members()? {
            signal_pid_allow_missing(pid, libc::SIGTERM)?;
            signaled.insert(pid);
        }
        let deadline = Instant::now() + TERM_GRACE;
        while !signaled.is_empty()
            && (self
                .process_groups
                .iter()
                .copied()
                .map(process_group_exists)
                .collect::<PocResult<Vec<_>>>()?
                .into_iter()
                .any(|exists| exists)
                || !self.cgroup_members()?.is_empty())
            && Instant::now() < deadline
        {
            thread::sleep(POLL_INTERVAL);
        }
        for process_group in &self.process_groups {
            if process_group_exists(*process_group)? {
                signal_process_group_allow_missing(*process_group, libc::SIGKILL)?;
            }
        }
        for pid in self.cgroup_members()? {
            signal_pid_allow_missing(pid, libc::SIGKILL)?;
            signaled.insert(pid);
        }
        for child in &mut self.children {
            child
                .wait()
                .map_err(|error| PocError::io("reap managed child", &self.workspace_root, error))?;
        }
        self.children.clear();
        for process_group in &self.process_groups {
            reap_process_group(*process_group)?;
        }
        for pid in &signaled {
            reap_process(*pid)?;
        }
        Ok(signaled.into_iter().collect())
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn stop_kill_reap_anchored(
        &mut self,
        identity: &AnchoredWorkspaceAuditIdentity,
    ) -> PocResult<Vec<i32>> {
        self.fence();
        let mut signaled = BTreeSet::new();
        let mut child_pidfds = Vec::with_capacity(self.children.len());
        for child in &self.children {
            let pid = i32::try_from(child.id()).map_err(|_| {
                PocError::Integrity(format!("managed child PID {} does not fit i32", child.id()))
            })?;
            if let Some(pidfd) = open_pidfd_allow_missing(pid)? {
                signal_pidfd(&pidfd, libc::SIGTERM)?;
                signaled.insert(pid);
                child_pidfds.push((pid, pidfd));
            }
        }
        let deadline = Instant::now() + TERM_GRACE;
        loop {
            let mut running = false;
            for child in &mut self.children {
                if child
                    .try_wait()
                    .map_err(|error| {
                        PocError::io("poll anchored managed child", &self.workspace_root, error)
                    })?
                    .is_none()
                {
                    running = true;
                }
            }
            if !running || Instant::now() >= deadline {
                break;
            }
            thread::sleep(POLL_INTERVAL);
        }
        for (pid, pidfd) in &child_pidfds {
            let running = self
                .children
                .iter_mut()
                .find(|child| u32::try_from(*pid).ok() == Some(child.id()))
                .map(|child| {
                    child.try_wait().map_err(|error| {
                        PocError::io("poll anchored managed child", &self.workspace_root, error)
                    })
                })
                .transpose()?
                .flatten()
                .is_none();
            if running {
                signal_pidfd(pidfd, libc::SIGKILL)?;
            }
        }
        for child in &mut self.children {
            child.wait().map_err(|error| {
                PocError::io("reap anchored managed child", &self.workspace_root, error)
            })?;
        }
        self.children.clear();
        self.process_groups.clear();

        let (workspace_pids, final_audit) = terminate_terminal_workspace_references_anchored(
            identity,
            self.cgroup_membership.as_ref(),
        )?;
        signaled.extend(workspace_pids);
        if !final_audit.is_clear() {
            return Err(PocError::RecoveryRequired(format!(
                "anchored managed process drain remained populated: {final_audit:?}"
            )));
        }
        Ok(signaled.into_iter().collect())
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn stop_kill_reap_anchored(
        &mut self,
        _identity: &AnchoredWorkspaceAuditIdentity,
    ) -> PocResult<Vec<i32>> {
        Err(PocError::Unsupported(
            "descriptor-anchored terminal process drain requires Linux pidfds".to_owned(),
        ))
    }

    pub(crate) fn audit_anchored(
        &self,
        identity: &AnchoredWorkspaceAuditIdentity,
        include_mount_namespaces: bool,
    ) -> PocResult<ProcessAudit> {
        #[cfg(target_os = "linux")]
        {
            return audit_workspace_references_anchored(
                identity,
                self.cgroup_membership.as_ref(),
                include_mount_namespaces,
            );
        }
        #[cfg(not(target_os = "linux"))]
        audit_terminal_workspace_anchored(identity, None, include_mount_namespaces)
    }

    pub fn audit(&self, include_mount_namespaces: bool) -> PocResult<ProcessAudit> {
        #[cfg(target_os = "linux")]
        {
            return audit_workspace_references(
                &self.workspace_root,
                None,
                self.cgroup_membership.as_ref(),
                include_mount_namespaces,
            );
        }
        #[cfg(not(target_os = "linux"))]
        audit_workspace_references(
            &self.workspace_root,
            self.cgroup_procs_path.as_deref(),
            None,
            include_mount_namespaces,
        )
    }

    fn cgroup_members(&self) -> PocResult<Vec<i32>> {
        #[cfg(target_os = "linux")]
        let Some(membership) = &self.cgroup_membership
        else {
            return Ok(Vec::new());
        };
        #[cfg(target_os = "linux")]
        let text = membership.read_exact()?;
        #[cfg(not(target_os = "linux"))]
        let Some(path) = &self.cgroup_procs_path
        else {
            return Ok(Vec::new());
        };
        #[cfg(not(target_os = "linux"))]
        let text = fs::read_to_string(path)
            .map_err(|error| PocError::io("read session cgroup.procs", path, error))?;
        #[cfg(target_os = "linux")]
        let members = parse_cgroup_members(&text)?;
        #[cfg(not(target_os = "linux"))]
        let members = text
            .lines()
            .map(|line| {
                line.trim().parse::<i32>().map_err(|_| {
                    PocError::RecoveryRequired(format!(
                        "session cgroup.procs contains an invalid PID: {line:?}"
                    ))
                })
            })
            .collect::<PocResult<Vec<_>>>()?;
        Ok(members
            .into_iter()
            .filter(|pid| u32::try_from(*pid).ok() != Some(std::process::id()))
            .collect())
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::undocumented_unsafe_blocks)]
unsafe fn open_readiness_at(directory_fd: i32, relative_path: *const libc::c_char) -> i32 {
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (libc::O_RDONLY | libc::O_CLOEXEC) as u64;
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS;
    unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory_fd,
            relative_path,
            std::ptr::addr_of!(how),
            std::mem::size_of::<libc::open_how>(),
        ) as i32
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
#[allow(clippy::undocumented_unsafe_blocks)]
unsafe fn open_readiness_at(directory_fd: i32, relative_path: *const libc::c_char) -> i32 {
    unsafe {
        libc::openat(
            directory_fd,
            relative_path,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    }
}

fn build_prefix_table(needle: &[u8]) -> Vec<usize> {
    let mut prefix = vec![0; needle.len()];
    let mut matched = 0;
    for index in 1..needle.len() {
        while matched > 0 && needle[index] != needle[matched] {
            matched = prefix[matched - 1];
        }
        if needle[index] == needle[matched] {
            matched += 1;
            prefix[index] = matched;
        }
    }
    prefix
}

#[cfg(unix)]
#[allow(clippy::undocumented_unsafe_blocks)]
unsafe fn probe_fd(fd: i32, needle: Option<&[u8]>, prefix: &[usize]) -> bool {
    let mut buffer = [0_u8; 4096];
    let mut matched = 0;
    loop {
        let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
        }
        if read == 0 {
            return needle.is_none() && matched > 0;
        }
        let bytes = &buffer[..usize::try_from(read).unwrap_or(0)];
        let Some(needle) = needle else {
            return !bytes.is_empty();
        };
        for byte in bytes {
            while matched > 0 && *byte != needle[matched] {
                matched = prefix[matched - 1];
            }
            if *byte == needle[matched] {
                matched += 1;
                if matched == needle.len() {
                    return true;
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn open_direct_child_cgroup(
    membership: Option<&AttestedCgroupMembership>,
) -> PocResult<Option<File>> {
    let Some(membership) = membership else {
        return Ok(None);
    };
    let members = membership.read_exact()?;
    if members
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .any(|pid| pid == std::process::id())
    {
        return Ok(None);
    }
    membership.open_write_exact().map(Some)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn open_direct_child_cgroup(_path: Option<&Path>) -> PocResult<Option<File>> {
    Ok(None)
}

#[cfg(target_os = "linux")]
#[allow(clippy::undocumented_unsafe_blocks)]
fn wait_direct_child(pid: i32, timeout: Duration) -> PocResult<(i32, bool)> {
    let deadline = Instant::now() + timeout;
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    if pidfd < 0 {
        return wait_direct_child_fallback(pid, deadline);
    }
    let mut descriptor = libc::pollfd {
        fd: pidfd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_ms = remaining
            .as_nanos()
            .div_ceil(1_000_000)
            .min(i32::MAX as u128) as i32;
        let result = unsafe { libc::poll(std::ptr::addr_of_mut!(descriptor), 1, timeout_ms) };
        if result > 0 {
            unsafe {
                libc::close(pidfd);
            }
            return wait_direct_child_blocking(pid).map(|status| (status, false));
        }
        if result == 0 {
            unsafe {
                libc::close(pidfd);
            }
            signal_pid_allow_missing(pid, libc::SIGKILL)?;
            return wait_direct_child_blocking(pid).map(|status| (status, true));
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        unsafe {
            libc::close(pidfd);
        }
        signal_pid_allow_missing(pid, libc::SIGKILL)?;
        let _ = wait_direct_child_blocking(pid);
        return Err(PocError::io(
            "poll external readiness child",
            Path::new("/proc"),
            error,
        ));
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn wait_direct_child(pid: i32, timeout: Duration) -> PocResult<(i32, bool)> {
    wait_direct_child_fallback(pid, Instant::now() + timeout)
}

#[cfg(unix)]
#[allow(clippy::undocumented_unsafe_blocks)]
fn wait_direct_child_fallback(pid: i32, deadline: Instant) -> PocResult<(i32, bool)> {
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, std::ptr::addr_of_mut!(status), libc::WNOHANG) };
        if result == pid {
            return Ok((status, false));
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(PocError::io(
                "poll external readiness child",
                Path::new("/proc"),
                error,
            ));
        }
        if Instant::now() >= deadline {
            signal_pid_allow_missing(pid, libc::SIGKILL)?;
            return wait_direct_child_blocking(pid).map(|status| (status, true));
        }
        thread::sleep(Duration::from_micros(100));
    }
}

#[cfg(unix)]
#[allow(clippy::undocumented_unsafe_blocks)]
fn wait_direct_child_blocking(pid: i32) -> PocResult<i32> {
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, std::ptr::addr_of_mut!(status), 0) };
        if result == pid {
            return Ok(status);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(PocError::io(
            "reap external readiness child",
            Path::new("/proc"),
            error,
        ));
    }
}

fn command_receipt(
    program: &Path,
    arguments: &[String],
    started_unix_ms: u64,
    status: ExitStatus,
    timed_out: bool,
    process_group_id: u32,
) -> PocResult<CommandReceipt> {
    Ok(CommandReceipt {
        schema_version: crate::SCHEMA_VERSION,
        program: program.to_path_buf(),
        arguments: arguments.to_vec(),
        started_unix_ms,
        finished_unix_ms: unix_time_ms()?,
        exit_code: status.code(),
        success: status.success() && !timed_out,
        timed_out,
        process_group_id,
    })
}

#[cfg(target_os = "linux")]
fn install_child_isolation(
    command: &mut Command,
    cgroup_membership: Option<&AttestedCgroupMembership>,
) -> PocResult<Option<File>> {
    let cgroup = cgroup_membership
        .map(AttestedCgroupMembership::open_write_exact)
        .transpose()?;
    let cgroup_fd = cgroup.as_ref().map(AsRawFd::as_raw_fd);
    // SAFETY: `pre_exec` executes in the forked child and calls only
    // async-signal-safe syscalls. Writing `0` to cgroup.procs enrolls the
    // writing child before it can exec or fork descendants.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(fd) = cgroup_fd {
                let membership = b"0\n";
                let written = libc::write(fd, membership.as_ptr().cast(), membership.len());
                if written != membership.len() as isize {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(cgroup)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn install_child_isolation(
    command: &mut Command,
    _cgroup_procs_path: Option<&Path>,
) -> PocResult<Option<()>> {
    // SAFETY: `pre_exec` executes in the forked child and calls only the
    // async-signal-safe `setpgid(2)` syscall with constant integer arguments.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    Ok(None)
}

#[cfg(not(unix))]
fn install_child_isolation(
    _command: &mut Command,
    _cgroup_procs_path: Option<&Path>,
) -> PocResult<Option<()>> {
    Ok(None)
}

fn signal_process_group(process_group: u32, signal: i32) -> PocResult<()> {
    let group = i32::try_from(process_group).map_err(|_| {
        PocError::Integrity(format!("process group {process_group} does not fit i32"))
    })?;
    signal_raw(-group, signal, false)
}

fn signal_process_group_allow_missing(process_group: u32, signal: i32) -> PocResult<()> {
    let group = i32::try_from(process_group).map_err(|_| {
        PocError::Integrity(format!("process group {process_group} does not fit i32"))
    })?;
    signal_raw(-group, signal, true)
}

fn signal_pid_allow_missing(pid: i32, signal: i32) -> PocResult<()> {
    signal_raw(pid, signal, true)
}

#[cfg(unix)]
#[allow(clippy::undocumented_unsafe_blocks)]
fn process_group_exists(process_group: u32) -> PocResult<bool> {
    let group = i32::try_from(process_group).map_err(|_| {
        PocError::Integrity(format!("process group {process_group} does not fit i32"))
    })?;
    let result = unsafe { libc::kill(-group, 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(PocError::io(
            "probe managed process group",
            Path::new("/proc"),
            error,
        )),
    }
}

#[cfg(not(unix))]
fn process_group_exists(_process_group: u32) -> PocResult<bool> {
    Ok(false)
}

#[cfg(target_os = "linux")]
fn ensure_child_subreaper() -> PocResult<()> {
    // SAFETY: `prctl` receives the fixed PR_SET_CHILD_SUBREAPER operation and
    // integer enable flag. It does not retain or dereference user memory.
    let result = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::io(
            "enable managed child subreaper",
            Path::new("/proc/self"),
            std::io::Error::last_os_error(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn ensure_child_subreaper() -> PocResult<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn reap_process_group(process_group: u32) -> PocResult<()> {
    let group = i32::try_from(process_group).map_err(|_| {
        PocError::Integrity(format!("process group {process_group} does not fit i32"))
    })?;
    reap_wait_target(-group, "reap managed process group")
}

#[cfg(not(target_os = "linux"))]
fn reap_process_group(_process_group: u32) -> PocResult<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn reap_process(pid: i32) -> PocResult<()> {
    reap_wait_target(pid, "reap managed process")
}

#[cfg(not(target_os = "linux"))]
fn reap_process(_pid: i32) -> PocResult<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn reap_wait_target(target: i32, operation: &'static str) -> PocResult<()> {
    let deadline = Instant::now() + TERM_GRACE;
    loop {
        let mut status = 0;
        // SAFETY: `waitpid` writes only to the valid local status integer. The
        // target is an exact adopted PID or a recorded process group.
        let result =
            unsafe { libc::waitpid(target, std::ptr::addr_of_mut!(status), libc::WNOHANG) };
        if result > 0 {
            continue;
        }
        if result == 0 {
            if Instant::now() >= deadline {
                return Err(PocError::RecoveryRequired(format!(
                    "{operation} timed out for target {target}"
                )));
            }
            thread::sleep(POLL_INTERVAL);
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECHILD) {
            return Ok(());
        }
        return Err(PocError::io(operation, Path::new("/proc"), error));
    }
}

#[cfg(unix)]
fn signal_raw(target: i32, signal: i32, allow_missing: bool) -> PocResult<()> {
    // SAFETY: `kill(2)` does not dereference memory; both arguments are plain
    // integers validated or supplied as platform signal constants.
    let result = unsafe { libc::kill(target, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if allow_missing && error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(PocError::io(
        "signal managed process",
        Path::new("/proc"),
        error,
    ))
}

#[cfg(not(unix))]
fn signal_raw(_target: i32, _signal: i32, _allow_missing: bool) -> PocResult<()> {
    Err(PocError::Unsupported(
        "managed process signaling requires unix".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn audit_workspace_references_anchored(
    identity: &AnchoredWorkspaceAuditIdentity,
    attested_cgroup: Option<&AttestedCgroupMembership>,
    include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    let current_namespace = fs::metadata("/proc/self/ns/mnt")
        .map_err(|error| PocError::io("stat current mount namespace", "/proc/self/ns/mnt", error))?
        .ino();
    if current_namespace != identity.mount.mount_namespace_inode {
        return Err(PocError::RecoveryRequired(
            "mount namespace changed during anchored terminal process audit".to_owned(),
        ));
    }
    let mount_ids = process_audit_mount_tree_ids(&identity.mount)?;
    let mut audit = ProcessAudit {
        workspace_root: identity.mount.workspace_root.clone(),
        ..ProcessAudit::default()
    };
    if let Some(attested) = attested_cgroup {
        return exact_cgroup_process_audit(
            identity.mount.workspace_root.clone(),
            &attested.read_exact()?,
        );
    }
    let candidates = proc_candidates()?;

    for (pid, proc_root) in candidates {
        if u32::try_from(pid).ok() == Some(std::process::id()) {
            continue;
        }
        let namespace_path = proc_root.join("ns/mnt");
        let namespace = match fs::metadata(&namespace_path) {
            Ok(namespace) => namespace,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(PocError::io(
                    "stat process mount namespace",
                    namespace_path,
                    error,
                ));
            }
        };
        if namespace.ino() != identity.mount.mount_namespace_inode {
            let visible_mount =
                foreign_mount_namespace_has_process_mount(&proc_root, &identity.mount)?;
            if include_mount_namespaces && visible_mount {
                audit.mount_namespace_pins.push(pid);
            }
            let object_references = audit_foreign_namespace_object_references(
                &proc_root,
                identity.mount.target_device,
            )?;
            if object_references.cwd_or_root {
                audit.cwd_or_root_pins.push(pid);
            }
            if object_references.fd {
                audit.fd_pins.push(pid);
            }
            if object_references.writable_map {
                audit.writable_map_pins.push(pid);
            }
            if identity.recovery
                && attested_cgroup.is_none()
                && !visible_mount
                && !object_references.any()
            {
                return Err(PocError::RecoveryRequired(format!(
                    "cannot exclude detached terminal references in foreign mount namespace for PID {pid} without attested cgroup authority"
                )));
            }
            continue;
        }
        let object_references =
            audit_foreign_namespace_object_references(&proc_root, identity.mount.target_device)?;
        if object_references.cwd_or_root
            || link_has_mount_id(&proc_root.join("cwd"), &mount_ids)?
            || link_has_mount_id(&proc_root.join("root"), &mount_ids)?
        {
            audit.cwd_or_root_pins.push(pid);
        }
        if object_references.fd || directory_has_mount_id(&proc_root.join("fd"), &mount_ids)? {
            audit.fd_pins.push(pid);
        }
        if object_references.writable_map || maps_have_writable_mount_id(&proc_root, &mount_ids)? {
            audit.writable_map_pins.push(pid);
        }
    }
    deduplicate_audit(&mut audit);
    Ok(audit)
}

#[cfg(target_os = "linux")]
fn parse_cgroup_members(text: &str) -> PocResult<Vec<i32>> {
    let mut members = Vec::new();
    for line in text.lines() {
        let pid = line.trim().parse::<i32>().map_err(|_| {
            PocError::RecoveryRequired(format!(
                "session cgroup.procs contains an invalid PID: {line:?}"
            ))
        })?;
        if pid <= 0 {
            return Err(PocError::RecoveryRequired(format!(
                "session cgroup.procs contains a nonpositive PID: {pid}"
            )));
        }
        members.push(pid);
    }
    members.sort_unstable();
    members.dedup();
    Ok(members)
}

#[cfg(target_os = "linux")]
fn exact_cgroup_process_audit(
    workspace_root: PathBuf,
    membership_text: &str,
) -> PocResult<ProcessAudit> {
    let members = parse_cgroup_members(membership_text)?;
    Ok(ProcessAudit {
        workspace_root,
        cgroup_members: members
            .into_iter()
            .filter(|pid| u32::try_from(*pid).ok() != Some(std::process::id()))
            .collect(),
        ..ProcessAudit::default()
    })
}

#[cfg(target_os = "linux")]
fn proc_candidates() -> PocResult<Vec<(i32, PathBuf)>> {
    let mut candidates = Vec::new();
    for entry in
        fs::read_dir("/proc").map_err(|error| PocError::io("enumerate proc", "/proc", error))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(PocError::io("enumerate proc entry", "/proc", error)),
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        candidates.push((pid, entry.path()));
    }
    Ok(candidates)
}

#[cfg(target_os = "linux")]
fn foreign_mount_namespace_has_process_mount(
    proc_root: &Path,
    identity: &OverlayProcessAuditIdentity,
) -> PocResult<bool> {
    let mountinfo_path = proc_root.join("mountinfo");
    let text = match fs::read_to_string(&mountinfo_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error)
            if error.raw_os_error() == Some(libc::EINVAL)
                && process_is_zombie_or_gone(proc_root)? =>
        {
            return Ok(false);
        }
        Err(error) => {
            return Err(PocError::io(
                "audit process mountinfo by attested identity",
                mountinfo_path,
                error,
            ));
        }
    };
    mountinfo_text_has_process_audit_mount(&text, identity)
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Default)]
struct ForeignObjectReferences {
    cwd_or_root: bool,
    fd: bool,
    writable_map: bool,
}

#[cfg(target_os = "linux")]
impl ForeignObjectReferences {
    const fn any(self) -> bool {
        self.cwd_or_root || self.fd || self.writable_map
    }
}

#[cfg(target_os = "linux")]
fn audit_foreign_namespace_object_references(
    proc_root: &Path,
    target_device: u64,
) -> PocResult<ForeignObjectReferences> {
    Ok(ForeignObjectReferences {
        cwd_or_root: link_has_device(&proc_root.join("cwd"), target_device)?
            || link_has_device(&proc_root.join("root"), target_device)?,
        fd: directory_has_device(&proc_root.join("fd"), target_device)?,
        writable_map: maps_have_writable_device(proc_root, target_device)?,
    })
}

#[cfg(target_os = "linux")]
fn link_has_mount_id(link: &Path, mount_ids: &BTreeSet<u64>) -> PocResult<bool> {
    Ok(statx_mount_id(link)?.is_some_and(|mount_id| mount_ids.contains(&mount_id)))
}

#[cfg(target_os = "linux")]
fn directory_has_mount_id(directory: &Path, mount_ids: &BTreeSet<u64>) -> PocResult<bool> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(PocError::io("audit process fds", directory, error)),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(PocError::io("audit process fd entry", directory, error)),
        };
        if statx_mount_id(&entry.path())?.is_some_and(|mount_id| mount_ids.contains(&mount_id)) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn maps_have_writable_mount_id(proc_root: &Path, mount_ids: &BTreeSet<u64>) -> PocResult<bool> {
    let maps_path = proc_root.join("maps");
    let text = match fs::read_to_string(&maps_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(PocError::io("audit process maps", maps_path, error)),
    };
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some(range) = fields.next() else {
            continue;
        };
        let Some(permissions) = fields.next() else {
            continue;
        };
        let _offset = fields.next();
        let _device = fields.next();
        let inode = fields.next();
        if !permissions.as_bytes().contains(&b'w') || inode == Some("0") {
            continue;
        }
        let map_file = proc_root.join("map_files").join(range);
        if statx_mount_id(&map_file)?.is_some_and(|mount_id| mount_ids.contains(&mount_id)) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn link_has_device(link: &Path, target_device: u64) -> PocResult<bool> {
    Ok(statx_device(link)?.is_some_and(|device| device == target_device))
}

#[cfg(target_os = "linux")]
fn directory_has_device(directory: &Path, target_device: u64) -> PocResult<bool> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(PocError::io("audit foreign process fds", directory, error)),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(PocError::io(
                    "audit foreign process fd entry",
                    directory,
                    error,
                ));
            }
        };
        if statx_device(&entry.path())?.is_some_and(|device| device == target_device) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn maps_have_writable_device(proc_root: &Path, target_device: u64) -> PocResult<bool> {
    let maps_path = proc_root.join("maps");
    let text = match fs::read_to_string(&maps_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(PocError::io("audit foreign process maps", maps_path, error)),
    };
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some(range) = fields.next() else {
            continue;
        };
        let Some(permissions) = fields.next() else {
            continue;
        };
        let _offset = fields.next();
        let _device = fields.next();
        let inode = fields.next();
        if !permissions.as_bytes().contains(&b'w') || inode == Some("0") {
            continue;
        }
        let map_file = proc_root.join("map_files").join(range);
        if statx_device(&map_file)?.is_some_and(|device| device == target_device) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn statx_device(path: &Path) -> PocResult<Option<u64>> {
    let path_c = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        PocError::RecoveryRequired(format!(
            "process audit path contains NUL: {}",
            path.display()
        ))
    })?;
    let status = match rustix::fs::statx(
        rustix::fs::CWD,
        path_c.as_c_str(),
        rustix::fs::AtFlags::NO_AUTOMOUNT,
        rustix::fs::StatxFlags::BASIC_STATS,
    ) {
        Ok(status) => status,
        Err(rustix::io::Errno::NOENT) | Err(rustix::io::Errno::SRCH) => return Ok(None),
        Err(error) => {
            return Err(PocError::io(
                "statx process device identity",
                path,
                std::io::Error::from(error),
            ));
        }
    };
    Ok(Some(rustix::fs::makedev(
        status.stx_dev_major,
        status.stx_dev_minor,
    )))
}

#[cfg(target_os = "linux")]
fn statx_mount_id(path: &Path) -> PocResult<Option<u64>> {
    const STATX_MNT_ID_MASK: u32 = 0x0000_1000;
    let path_c = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        PocError::RecoveryRequired(format!(
            "process audit path contains NUL: {}",
            path.display()
        ))
    })?;
    // Following procfs magic links is intentional: the returned mount ID
    // belongs to the pinned kernel object, not its textual target.
    let status = match rustix::fs::statx(
        rustix::fs::CWD,
        path_c.as_c_str(),
        rustix::fs::AtFlags::NO_AUTOMOUNT,
        rustix::fs::StatxFlags::MNT_ID,
    ) {
        Ok(status) => status,
        Err(rustix::io::Errno::NOENT) | Err(rustix::io::Errno::SRCH) => return Ok(None),
        Err(error) => {
            return Err(PocError::io(
                "statx process mount identity",
                path,
                std::io::Error::from(error),
            ));
        }
    };
    if status.stx_mask & STATX_MNT_ID_MASK == 0 {
        return Err(PocError::Unsupported(
            "Linux statx did not report STATX_MNT_ID for process audit".to_owned(),
        ));
    }
    Ok(Some(status.stx_mnt_id))
}

#[cfg(target_os = "linux")]
fn audit_workspace_references(
    workspace_root: &Path,
    cgroup_procs_path: Option<&Path>,
    attested_cgroup: Option<&AttestedCgroupMembership>,
    include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    let current_mount_namespace_inode = if include_mount_namespaces {
        Some(
            fs::metadata("/proc/self/ns/mnt")
                .map_err(|error| {
                    PocError::io("stat current mount namespace", "/proc/self/ns/mnt", error)
                })?
                .ino(),
        )
    } else {
        None
    };
    let mut audit = ProcessAudit {
        workspace_root: workspace_root.to_path_buf(),
        ..ProcessAudit::default()
    };
    let cgroup_text = match (cgroup_procs_path, attested_cgroup) {
        (Some(_), Some(_)) => {
            return Err(PocError::Integrity(
                "process audit received two cgroup membership sources".to_owned(),
            ));
        }
        (Some(path), None) => Some(
            fs::read_to_string(path)
                .map_err(|error| PocError::io("audit session cgroup.procs", path, error))?,
        ),
        (None, Some(attested)) => {
            return exact_cgroup_process_audit(
                workspace_root.to_path_buf(),
                &attested.read_exact()?,
            );
        }
        (None, None) => None,
    };
    let candidates = if let Some(text) = cgroup_text {
        let members = parse_cgroup_members(&text)?;
        audit.cgroup_members.extend(
            members
                .iter()
                .copied()
                .filter(|pid| u32::try_from(*pid).ok() != Some(std::process::id())),
        );
        members
            .into_iter()
            .map(|pid| (pid, PathBuf::from(format!("/proc/{pid}"))))
            .collect()
    } else {
        proc_candidates()?
    };

    for (pid, proc_root) in candidates {
        if link_pins(&proc_root.join("cwd"), workspace_root)?
            || link_pins(&proc_root.join("root"), workspace_root)?
        {
            audit.cwd_or_root_pins.push(pid);
        }
        if directory_has_pin(&proc_root.join("fd"), workspace_root)? {
            audit.fd_pins.push(pid);
        }
        if maps_have_writable_pin(&proc_root.join("maps"), workspace_root)? {
            audit.writable_map_pins.push(pid);
        }
        if let Some(current_namespace_inode) = current_mount_namespace_inode {
            if foreign_mount_namespace_has_mount(
                &proc_root,
                workspace_root,
                current_namespace_inode,
            )? {
                audit.mount_namespace_pins.push(pid);
            }
        }
    }
    deduplicate_audit(&mut audit);
    Ok(audit)
}

#[cfg(target_os = "linux")]
fn foreign_mount_namespace_has_mount(
    proc_root: &Path,
    workspace_root: &Path,
    current_namespace_inode: u64,
) -> PocResult<bool> {
    let namespace_path = proc_root.join("ns/mnt");
    let namespace = match fs::metadata(&namespace_path) {
        Ok(namespace) => namespace,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(PocError::io(
                "stat process mount namespace",
                namespace_path,
                error,
            ));
        }
    };
    if namespace.ino() == current_namespace_inode {
        return Ok(false);
    }
    mountinfo_has_mount(&proc_root.join("mountinfo"), workspace_root)
}

#[cfg(not(target_os = "linux"))]
fn audit_workspace_references(
    workspace_root: &Path,
    _cgroup_procs_path: Option<&Path>,
    _attested_cgroup: Option<&AttestedCgroupMembership>,
    _include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    Ok(ProcessAudit {
        workspace_root: workspace_root.to_path_buf(),
        ..ProcessAudit::default()
    })
}

#[cfg(target_os = "linux")]
fn link_pins(link: &Path, workspace_root: &Path) -> PocResult<bool> {
    match fs::read_link(link) {
        Ok(target) => Ok(target.starts_with(workspace_root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(PocError::io("audit process link", link, error)),
    }
}

#[cfg(target_os = "linux")]
fn directory_has_pin(directory: &Path, workspace_root: &Path) -> PocResult<bool> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(PocError::io("audit process fds", directory, error)),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(PocError::io("audit process fd entry", directory, error)),
        };
        match fs::read_link(entry.path()) {
            Ok(target) if target.starts_with(workspace_root) => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(PocError::io("audit process fd link", entry.path(), error));
            }
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn maps_have_writable_pin(maps_path: &Path, workspace_root: &Path) -> PocResult<bool> {
    let text = match fs::read_to_string(maps_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(PocError::io("audit process maps", maps_path, error)),
    };
    Ok(text
        .lines()
        .any(|line| map_line_has_writable_pin(line, workspace_root)))
}

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub fn map_line_has_writable_pin(line: &str, workspace_root: &Path) -> bool {
    let Some((permissions, pathname)) = proc_maps_permissions_and_pathname(line) else {
        return false;
    };
    if !permissions.as_bytes().contains(&b'w') {
        return false;
    }
    let pathname = pathname.strip_suffix(" (deleted)").unwrap_or(pathname);
    pathname.starts_with('/') && Path::new(pathname).starts_with(workspace_root)
}

#[cfg(target_os = "linux")]
fn proc_maps_permissions_and_pathname(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut cursor = 0;
    let mut permissions = None;
    for field_index in 0..5 {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let start = cursor;
        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if start == cursor {
            return None;
        }
        if field_index == 1 {
            permissions = Some(&line[start..cursor]);
        }
    }
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    let pathname = line.get(cursor..)?;
    if pathname.is_empty() || pathname.starts_with('[') {
        return None;
    }
    Some((permissions?, pathname))
}

#[cfg(target_os = "linux")]
fn mountinfo_has_mount(mountinfo_path: &Path, workspace_root: &Path) -> PocResult<bool> {
    let text = match fs::read_to_string(mountinfo_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error)
            if error.raw_os_error() == Some(libc::EINVAL)
                && process_is_zombie_or_gone(
                    mountinfo_path
                        .parent()
                        .unwrap_or_else(|| Path::new("/proc")),
                )? =>
        {
            return Ok(false);
        }
        Err(error) => {
            return Err(PocError::io(
                "audit process mountinfo",
                mountinfo_path,
                error,
            ));
        }
    };
    for line in text.lines() {
        if mountinfo_line_has_mount(line, workspace_root)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub fn mountinfo_line_has_mount(line: &str, workspace_root: &Path) -> PocResult<bool> {
    let mountpoint = line.split_ascii_whitespace().nth(4).ok_or_else(|| {
        PocError::RecoveryRequired(format!(
            "process mountinfo line has no mountpoint field: {line:?}"
        ))
    })?;
    Ok(decode_mountinfo_field(mountpoint)? == workspace_root.as_os_str().as_bytes())
}

#[cfg(target_os = "linux")]
fn decode_mountinfo_field(field: &str) -> PocResult<Vec<u8>> {
    let bytes = field.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'\\' {
            decoded.push(bytes[cursor]);
            cursor += 1;
            continue;
        }
        let escaped = bytes.get(cursor + 1..cursor + 4).ok_or_else(|| {
            PocError::RecoveryRequired(format!(
                "process mountinfo contains a truncated escape: {field:?}"
            ))
        })?;
        if !escaped.iter().all(|byte| matches!(*byte, b'0'..=b'7')) {
            return Err(PocError::RecoveryRequired(format!(
                "process mountinfo contains a non-octal escape: {field:?}"
            )));
        }
        let value = u16::from(escaped[0] - b'0') * 64
            + u16::from(escaped[1] - b'0') * 8
            + u16::from(escaped[2] - b'0');
        if value == 0 || value > u16::from(u8::MAX) {
            return Err(PocError::RecoveryRequired(
                "process mountinfo contains an impossible escaped path byte".to_owned(),
            ));
        }
        decoded.push(value as u8);
        cursor += 4;
    }
    Ok(decoded)
}

#[cfg(target_os = "linux")]
fn process_is_zombie_or_gone(proc_root: &Path) -> PocResult<bool> {
    let status_path = proc_root.join("status");
    let status = match fs::read_to_string(&status_path) {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(PocError::io("audit process status", status_path, error)),
    };
    Ok(status
        .lines()
        .find_map(|line| line.strip_prefix("State:"))
        .is_some_and(|state| state.trim_start().starts_with('Z')))
}

#[cfg(target_os = "linux")]
fn deduplicate_audit(audit: &mut ProcessAudit) {
    for pids in [
        &mut audit.cgroup_members,
        &mut audit.cwd_or_root_pins,
        &mut audit.fd_pins,
        &mut audit.writable_map_pins,
        &mut audit.mount_namespace_pins,
    ] {
        pids.sort_unstable();
        pids.dedup();
    }
}
