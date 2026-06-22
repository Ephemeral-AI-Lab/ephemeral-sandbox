//! Setns mode: join holder namespaces, optionally mount overlay/DNS, run a command.

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::fd::RawFd;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::path::{Component, Path};

#[cfg(target_os = "linux")]
use sandbox_runtime_overlay::OverlayHandle;

use super::RunnerError;
#[cfg(target_os = "linux")]
use crate::runner::protocol::NsFds;
use crate::runner::protocol::{NamespaceRunnerRequest, RunResult};

#[cfg(target_os = "linux")]
pub(crate) fn run_setns(request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    let ns_fds = request
        .ns_fds
        .ok_or_else(|| RunnerError::InvalidRequest("setns mode requires ns_fds".to_owned()))?;
    join_cgroup(request)?;
    join_namespaces(&ns_fds)?;
    super::shell_exec::execute_shell(request)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn run_setns(_request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    Err(RunnerError::Unsupported)
}

/// Mount the overlay inside an existing workspace mount namespace.
#[cfg(target_os = "linux")]
pub fn setns_overlay_mount(
    request: &NamespaceRunnerRequest,
    hidden_paths: &[PathBuf],
) -> Result<(), RunnerError> {
    setns_user_mnt(request, "setns overlay mount")?;
    let upperdir = request.upperdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires upperdir".to_owned())
    })?;
    let workdir = request.workdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires workdir".to_owned())
    })?;
    let handle = OverlayHandle {
        layer_paths: if request.layer_paths.is_empty() {
            return Err(RunnerError::InvalidRequest(
                "setns overlay mount requires layer_paths".to_owned(),
            ));
        } else {
            request.layer_paths.clone()
        },
        upperdir: upperdir.clone(),
        workdir: workdir.clone(),
    };
    let guard = sandbox_runtime_overlay::mount_overlay(&request.workspace_root, &handle)?;
    super::mask_model_shell_paths(hidden_paths)?;
    // The setns mount helper is a one-shot process. The mounted overlay must
    // outlive this helper and remain pinned by the target mount namespace until
    // isolated teardown, so the unmount-on-drop guard is deliberately leaked.
    std::mem::forget(guard);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn setns_overlay_mount(
    _request: &NamespaceRunnerRequest,
    _hidden_paths: &[PathBuf],
) -> Result<(), RunnerError> {
    Err(RunnerError::Unsupported)
}

/// Remount an overlay inside the runner's current mount namespace.
#[cfg(target_os = "linux")]
pub fn remount_overlay(
    request: &NamespaceRunnerRequest,
    hidden_paths: &[PathBuf],
) -> Result<serde_json::Value, RunnerError> {
    setns_user_mnt(request, "remount overlay")?;
    let mut mask_guard = RemountMaskGuard::unmask(hidden_paths)?;
    let upperdir = request.upperdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("remount overlay requires upperdir".to_owned())
    })?;
    let workdir = request.workdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("remount overlay requires workdir".to_owned())
    })?;
    if request.layer_paths.is_empty() {
        return Err(RunnerError::InvalidRequest(
            "remount overlay requires layer_paths".to_owned(),
        ));
    }
    let handle = OverlayHandle {
        upperdir: upperdir.clone(),
        workdir: workdir.clone(),
        layer_paths: request.layer_paths.clone(),
    };
    let telemetry = staged_remount_overlay(request, &handle, &mut mask_guard)?;
    mask_guard.restore()?;
    let report = remount_verification_report(request, &request.workspace_root, &telemetry);
    Ok(report)
}

#[cfg(not(target_os = "linux"))]
pub fn remount_overlay(
    _request: &NamespaceRunnerRequest,
    _hidden_paths: &[PathBuf],
) -> Result<serde_json::Value, RunnerError> {
    Err(RunnerError::Unsupported)
}

#[cfg(target_os = "linux")]
struct RemountMaskGuard<'a> {
    hidden_paths: &'a [PathBuf],
    restored: bool,
}

#[cfg(target_os = "linux")]
impl<'a> RemountMaskGuard<'a> {
    fn unmask(hidden_paths: &'a [PathBuf]) -> Result<Self, RunnerError> {
        super::unmask_model_shell_paths(hidden_paths)?;
        Ok(Self {
            hidden_paths,
            restored: false,
        })
    }

    fn restore(&mut self) -> Result<(), RunnerError> {
        if self.restored {
            return Ok(());
        }
        super::mask_model_shell_paths(self.hidden_paths)?;
        self.restored = true;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for RemountMaskGuard<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RemountSwitchTelemetry {
    attempted: bool,
    staging_verified: Option<bool>,
    staged_switch: bool,
    rollback_unmounted: Option<bool>,
    rollback_unmount_error: Option<String>,
}

#[cfg(target_os = "linux")]
impl RemountSwitchTelemetry {
    fn fully_verified(&self) -> bool {
        if !self.attempted {
            return true;
        }
        self.staging_verified == Some(true)
            && self.staged_switch
            && self.rollback_unmounted == Some(true)
            && self.rollback_unmount_error.is_none()
    }
}

#[cfg(target_os = "linux")]
struct RemountStagingDirs {
    staging: PathBuf,
    rollback: PathBuf,
}

#[cfg(target_os = "linux")]
impl RemountStagingDirs {
    fn create(workdir: &Path) -> Result<Self, RunnerError> {
        let parent = workdir.parent().ok_or_else(|| {
            RunnerError::InvalidRequest(format!(
                "remount workdir has no parent: {}",
                workdir.display()
            ))
        })?;
        fs::create_dir_all(parent)?;
        let suffix = unique_suffix();
        let staging = parent.join(format!(".remount-staging-{}-{suffix}", std::process::id()));
        let rollback = parent.join(format!(".remount-rollback-{}-{suffix}", std::process::id()));
        fs::create_dir(&staging)?;
        if let Err(err) = fs::create_dir(&rollback) {
            let _ = fs::remove_dir(&staging);
            return Err(err.into());
        }
        Ok(Self { staging, rollback })
    }

    fn cleanup_dirs(&self) {
        let _ = fs::remove_dir(&self.staging);
        let _ = fs::remove_dir(&self.rollback);
    }
}

#[cfg(target_os = "linux")]
impl Drop for RemountStagingDirs {
    fn drop(&mut self) {
        self.cleanup_dirs();
    }
}

#[cfg(target_os = "linux")]
fn staged_remount_overlay(
    request: &NamespaceRunnerRequest,
    handle: &OverlayHandle,
    mask_guard: &mut RemountMaskGuard<'_>,
) -> Result<RemountSwitchTelemetry, RunnerError> {
    let mut telemetry = RemountSwitchTelemetry {
        attempted: true,
        ..RemountSwitchTelemetry::default()
    };
    let dirs = RemountStagingDirs::create(&handle.workdir)?;
    let staging_mount = mount_overlay_for_verified_remount(&dirs.staging, handle)?;
    telemetry.staging_verified = Some(overlay_mount_verified(request, &dirs.staging));
    if telemetry.staging_verified != Some(true) {
        return Ok(telemetry);
    }

    sandbox_runtime_overlay::move_mountpoint(&request.workspace_root, &dirs.rollback)?;
    if let Err(err) =
        sandbox_runtime_overlay::move_mountpoint(&dirs.staging, &request.workspace_root)
    {
        let rollback_error =
            sandbox_runtime_overlay::move_mountpoint(&dirs.rollback, &request.workspace_root).err();
        return Err(RunnerError::InvalidRequest(format!(
            "staged remount switch failed: {err}; rollback_error={rollback_error:?}"
        )));
    }
    telemetry.staged_switch = true;

    if let Err(err) = mask_guard.restore() {
        let rollback_error = rollback_staged_switch(&request.workspace_root, &dirs);
        return Err(RunnerError::InvalidRequest(format!(
            "staged remount mask restore failed: {err}; rollback_error={rollback_error:?}"
        )));
    }

    if !overlay_mount_verified(request, &request.workspace_root) {
        let rollback_error = rollback_staged_switch(&request.workspace_root, &dirs);
        telemetry.staged_switch = false;
        telemetry.rollback_unmount_error = rollback_error;
        return Ok(telemetry);
    }

    match sandbox_runtime_overlay::unmount_overlay(&dirs.rollback) {
        Ok(()) => {
            telemetry.rollback_unmounted = Some(true);
            // The runner is a one-shot process; the refreshed overlay now lives
            // at workspace_root and must outlive this helper.
            std::mem::forget(staging_mount);
        }
        Err(err) => {
            let cleanup_error = err.to_string();
            let rollback_error = rollback_staged_switch(&request.workspace_root, &dirs);
            telemetry.staged_switch = false;
            telemetry.rollback_unmounted = Some(false);
            telemetry.rollback_unmount_error = Some(match rollback_error {
                Some(rollback_error) => {
                    format!("{cleanup_error}; rollback_restore_error={rollback_error}")
                }
                None => cleanup_error,
            });
        }
    }
    Ok(telemetry)
}

#[cfg(target_os = "linux")]
fn mount_overlay_for_verified_remount(
    mountpoint: &Path,
    handle: &OverlayHandle,
) -> Result<sandbox_runtime_overlay::OverlayMount, RunnerError> {
    // The new mount API validates lowerdirs well but can hide the lowerdir list
    // from mountinfo on common kernels. Live remount must prove the target
    // mount uses the requested compact lower stack before lease retarget, so
    // this path uses validated mount options that remain visible in mountinfo.
    Ok(sandbox_runtime_overlay::mount_overlay_with_visible_options(
        mountpoint, handle,
    )?)
}

#[cfg(target_os = "linux")]
fn rollback_staged_switch(workspace_root: &Path, dirs: &RemountStagingDirs) -> Option<String> {
    if let Err(err) = sandbox_runtime_overlay::move_mountpoint(workspace_root, &dirs.staging) {
        return Some(format!("move new mount back to staging failed: {err}"));
    }
    if let Err(err) = sandbox_runtime_overlay::move_mountpoint(&dirs.rollback, workspace_root) {
        return Some(format!("restore old mount failed: {err}"));
    }
    None
}

#[cfg(target_os = "linux")]
fn remount_verification_report(
    request: &NamespaceRunnerRequest,
    workspace_root: &Path,
    telemetry: &RemountSwitchTelemetry,
) -> serde_json::Value {
    let mount_namespace = fs::read_link("/proc/self/ns/mnt")
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let mountinfo = workspace_mountinfo(workspace_root);
    let probe = read_probe_at_root(request, workspace_root);
    let overlay_mounted = mountinfo
        .as_ref()
        .is_some_and(|mountinfo| mountinfo.fs_type == "overlay");
    let lowerdir_expected_count = request.layer_paths.len();
    let lowerdir_count_matched =
        mountinfo_lowerdir_count_matched(mountinfo.as_ref(), lowerdir_expected_count);
    let lowerdir_verified = mountinfo_lowerdir_verified(mountinfo.as_ref(), &request.layer_paths);
    let probe_verified = probe.as_ref().is_none_or(RemountReadProbe::verified);
    let mount_verified = overlay_mounted
        && lowerdir_verified == Some(true)
        && probe_verified
        && telemetry.fully_verified();
    serde_json::json!({
        "success": true,
        "status": "ok",
        "mount_verified": mount_verified,
        "staged_switch": telemetry.staged_switch,
        "staging_verified": telemetry.staging_verified,
        "rollback_unmounted": telemetry.rollback_unmounted,
        "rollback_unmount_error": telemetry.rollback_unmount_error,
        "mount_namespace": mount_namespace,
        "mountinfo_mount_point": mountinfo.as_ref().map(|mountinfo| mountinfo.mount_point.clone()),
        "mountinfo_fs_type": mountinfo.as_ref().map(|mountinfo| mountinfo.fs_type.clone()),
        "mountinfo_lowerdir_count": mountinfo.as_ref().and_then(|mountinfo| mountinfo.lowerdir_count),
        "mountinfo_lowerdir": mountinfo.as_ref().and_then(|mountinfo| mountinfo.lowerdir.clone()),
        "mountinfo_lowerdir_expected_count": lowerdir_expected_count,
        "mountinfo_lowerdir_count_matched": lowerdir_count_matched,
        "mountinfo_lowerdir_verified": lowerdir_verified,
        "probe_path": probe.as_ref().map(|probe| probe.path.clone()),
        "probe_read_ok": probe.as_ref().map(|probe| probe.read_ok),
        "probe_content_matched": probe.as_ref().and_then(|probe| probe.content_matched),
        "probe_error": probe.as_ref().and_then(|probe| probe.error.clone()),
    })
}

#[cfg(target_os = "linux")]
fn overlay_mount_verified(request: &NamespaceRunnerRequest, workspace_root: &Path) -> bool {
    let mountinfo = workspace_mountinfo(workspace_root);
    let overlay_mounted = mountinfo
        .as_ref()
        .is_some_and(|mountinfo| mountinfo.fs_type == "overlay");
    let lowerdir_verified = mountinfo_lowerdir_verified(mountinfo.as_ref(), &request.layer_paths);
    let probe_verified = read_probe_at_root(request, workspace_root)
        .as_ref()
        .is_none_or(RemountReadProbe::verified);
    overlay_mounted && lowerdir_verified == Some(true) && probe_verified
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(target_os = "linux")]
pub(crate) struct WorkspaceMountInfo {
    pub(crate) mount_point: String,
    pub(crate) fs_type: String,
    pub(crate) lowerdir_count: Option<usize>,
    pub(crate) lowerdir: Option<String>,
}

#[cfg(target_os = "linux")]
fn workspace_mountinfo(workspace_root: &Path) -> Option<WorkspaceMountInfo> {
    let workspace_root = workspace_root.to_string_lossy();
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok()?;
    mountinfo.lines().find_map(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 10 {
            return None;
        }
        let mount_point = decode_mountinfo_field(fields.get(4)?);
        if mount_point != workspace_root {
            return None;
        }
        let separator = fields.iter().position(|field| *field == "-")?;
        let fs_type = fields.get(separator + 1)?.to_string();
        let super_options = fields.get(separator + 3).copied().unwrap_or_default();
        let lowerdir = overlay_option(super_options, "lowerdir");
        let lowerdir_count = lowerdir
            .as_deref()
            .map(|value| value.split(':').filter(|part| !part.is_empty()).count());
        Some(WorkspaceMountInfo {
            mount_point,
            fs_type,
            lowerdir_count,
            lowerdir,
        })
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn mountinfo_lowerdir_count_matched(
    mountinfo: Option<&WorkspaceMountInfo>,
    expected_count: usize,
) -> Option<bool> {
    mountinfo
        .and_then(|mountinfo| mountinfo.lowerdir_count)
        .map(|actual_count| actual_count == expected_count)
}

#[cfg(target_os = "linux")]
pub(crate) fn mountinfo_lowerdir_verified(
    mountinfo: Option<&WorkspaceMountInfo>,
    expected_layers: &[PathBuf],
) -> Option<bool> {
    let lowerdir = mountinfo?.lowerdir.as_deref()?;
    let actual_layers = lowerdir
        .split(':')
        .filter(|layer| !layer.is_empty())
        .collect::<Vec<_>>();
    Some(
        actual_layers.len() == expected_layers.len()
            && actual_layers
                .iter()
                .zip(expected_layers)
                .all(|(actual, expected)| *actual == expected.to_string_lossy().as_ref()),
    )
}

#[cfg(target_os = "linux")]
fn overlay_option(options: &str, key: &str) -> Option<String> {
    options
        .split(',')
        .find_map(|option| {
            option
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
        })
        .map(decode_mountinfo_field)
}

#[cfg(target_os = "linux")]
fn decode_mountinfo_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(target_os = "linux")]
struct RemountReadProbe {
    path: String,
    read_ok: bool,
    content_matched: Option<bool>,
    error: Option<String>,
}

#[cfg(target_os = "linux")]
impl RemountReadProbe {
    fn verified(&self) -> bool {
        self.read_ok && self.content_matched != Some(false)
    }
}

#[cfg(target_os = "linux")]
fn read_probe_at_root(
    request: &NamespaceRunnerRequest,
    workspace_root: &Path,
) -> Option<RemountReadProbe> {
    let path = request
        .args
        .get("probe_path")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    let expected = request
        .args
        .get("probe_content")
        .and_then(serde_json::Value::as_str);
    let relative = match validated_relative_probe_path(path) {
        Ok(relative) => relative,
        Err(error) => {
            return Some(RemountReadProbe {
                path: path.to_owned(),
                read_ok: false,
                content_matched: expected.map(|_| false),
                error: Some(error),
            });
        }
    };
    let full_path = workspace_root.join(relative);
    match fs::read_to_string(&full_path) {
        Ok(content) => Some(RemountReadProbe {
            path: path.to_owned(),
            read_ok: true,
            content_matched: expected.map(|expected| content == expected),
            error: None,
        }),
        Err(error) => Some(RemountReadProbe {
            path: path.to_owned(),
            read_ok: false,
            content_matched: expected.map(|_| false),
            error: Some(format!("{}: {error}", full_path.display())),
        }),
    }
}

#[cfg(target_os = "linux")]
fn validated_relative_probe_path(path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err("probe_path must be relative".to_owned());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("probe_path must stay inside workspace_root".to_owned());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err("probe_path must not be empty".to_owned());
    }
    Ok(normalized)
}

#[cfg(target_os = "linux")]
pub(crate) fn namespace_fd_order_with_types(
    ns_fds: &NsFds,
) -> Vec<(&'static str, RawFd, libc::c_int)> {
    [
        ("user", ns_fds.user, libc::CLONE_NEWUSER),
        ("mnt", ns_fds.mnt, libc::CLONE_NEWNS),
        ("pid", ns_fds.pid, libc::CLONE_NEWPID),
        ("net", ns_fds.net, libc::CLONE_NEWNET),
    ]
    .into_iter()
    .filter_map(|(name, fd, nstype)| fd.map(|fd| (name, fd.0, nstype)))
    .collect()
}

#[cfg(target_os = "linux")]
fn setns_user_mnt(request: &NamespaceRunnerRequest, operation: &str) -> Result<(), RunnerError> {
    let ns_fds = request
        .ns_fds
        .ok_or_else(|| RunnerError::InvalidRequest("setns mode requires ns_fds".to_owned()))?;
    let user = ns_fds
        .user
        .ok_or_else(|| RunnerError::InvalidRequest(format!("{operation} requires user ns fd")))?;
    let mnt = ns_fds
        .mnt
        .ok_or_else(|| RunnerError::InvalidRequest(format!("{operation} requires mnt ns fd")))?;
    setns_fd("user", user.0, libc::CLONE_NEWUSER)?;
    setns_fd("mnt", mnt.0, libc::CLONE_NEWNS)
}

#[cfg(target_os = "linux")]
fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(target_os = "linux")]
pub(crate) fn join_cgroup(request: &NamespaceRunnerRequest) -> Result<(), RunnerError> {
    let Some(cgroup_path) = request.cgroup_path.as_ref() else {
        return Ok(());
    };
    let procs = cgroup_path.join("cgroup.procs");
    fs::write(&procs, format!("{}\n", std::process::id())).map_err(|error| {
        let kind = error.kind();
        RunnerError::Syscall(std::io::Error::new(
            kind,
            format!(
                "join cgroup {} via {} failed: {error}",
                cgroup_path.display(),
                procs.display()
            ),
        ))
    })
}

#[cfg(target_os = "linux")]
fn join_namespaces(ns_fds: &NsFds) -> Result<(), RunnerError> {
    for (name, fd, nstype) in namespace_fd_order_with_types(ns_fds) {
        setns_fd(name, fd, nstype)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn setns_fd(name: &str, fd: RawFd, nstype: libc::c_int) -> Result<(), RunnerError> {
    // SAFETY: `fd` is a borrowed namespace file descriptor supplied by the
    // daemon to this dedicated single-threaded runner process. `nstype` is the
    // matching CLONE_NEW* constant for that descriptor, and no Rust references
    // are invalidated by the kernel changing the current task's namespace.
    let rc = unsafe { libc::setns(fd, nstype) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    let kind = err.kind();
    Err(RunnerError::Syscall(std::io::Error::new(
        kind,
        format!("setns({name}, fd={fd}, nstype=0x{nstype:x}) failed: {err}"),
    )))
}
