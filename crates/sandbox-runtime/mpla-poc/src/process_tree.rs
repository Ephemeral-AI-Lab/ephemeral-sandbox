use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::{unix_time_ms, PocError, PocResult};

const POLL_INTERVAL: Duration = Duration::from_millis(5);
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

/// Direct process-group runner used by the PoC when the product namespace
/// protocol would add unrelated orchestration. The optional cgroup membership
/// file catches descendants that escape their original process group.
#[derive(Debug)]
pub struct ManagedProcessTree {
    workspace_root: PathBuf,
    cgroup_procs_path: Option<PathBuf>,
    process_groups: BTreeSet<u32>,
    children: Vec<Child>,
    fenced: bool,
}

impl ManagedProcessTree {
    #[must_use]
    pub fn new(workspace_root: PathBuf, cgroup_procs_path: Option<PathBuf>) -> Self {
        Self {
            workspace_root,
            cgroup_procs_path,
            process_groups: BTreeSet::new(),
            children: Vec::new(),
            fenced: false,
        }
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
        install_process_group(&mut command);
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
        command_receipt(
            program,
            arguments,
            started_unix_ms,
            status,
            timed_out,
            process_group_id,
        )
    }

    /// Stop every recorded process group plus every remaining cgroup member,
    /// then reap all direct children owned by this process.
    pub fn stop_kill_reap(&mut self) -> PocResult<Vec<i32>> {
        self.fence();
        let mut signaled = BTreeSet::new();
        for process_group in &self.process_groups {
            signal_process_group_allow_missing(*process_group, libc::SIGTERM)?;
            signaled.insert(i32::try_from(*process_group).map_err(|_| {
                PocError::Integrity(format!("process group {process_group} does not fit i32"))
            })?);
        }
        for pid in self.cgroup_members()? {
            signal_pid_allow_missing(pid, libc::SIGTERM)?;
            signaled.insert(pid);
        }
        thread::sleep(TERM_GRACE);
        for process_group in &self.process_groups {
            signal_process_group_allow_missing(*process_group, libc::SIGKILL)?;
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

    pub fn audit(&self, include_mount_namespaces: bool) -> PocResult<ProcessAudit> {
        audit_workspace_references(
            &self.workspace_root,
            self.cgroup_procs_path.as_deref(),
            include_mount_namespaces,
        )
    }

    fn cgroup_members(&self) -> PocResult<Vec<i32>> {
        let Some(path) = &self.cgroup_procs_path else {
            return Ok(Vec::new());
        };
        let text = fs::read_to_string(path)
            .map_err(|error| PocError::io("read session cgroup.procs", path, error))?;
        Ok(text
            .lines()
            .filter_map(|line| line.trim().parse::<i32>().ok())
            .filter(|pid| u32::try_from(*pid).ok() != Some(std::process::id()))
            .collect())
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

#[cfg(unix)]
fn install_process_group(command: &mut Command) {
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
}

#[cfg(not(unix))]
fn install_process_group(_command: &mut Command) {}

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
fn audit_workspace_references(
    workspace_root: &Path,
    cgroup_procs_path: Option<&Path>,
    include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    let mut audit = ProcessAudit {
        workspace_root: workspace_root.to_path_buf(),
        ..ProcessAudit::default()
    };
    if let Some(path) = cgroup_procs_path {
        let text = fs::read_to_string(path)
            .map_err(|error| PocError::io("audit session cgroup.procs", path, error))?;
        audit.cgroup_members = text
            .lines()
            .filter_map(|line| line.trim().parse::<i32>().ok())
            .filter(|pid| u32::try_from(*pid).ok() != Some(std::process::id()))
            .collect();
    }

    for entry in
        fs::read_dir("/proc").map_err(|error| PocError::io("enumerate proc", "/proc", error))?
    {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let proc_root = entry.path();
        if link_pins(proc_root.join("cwd"), workspace_root)
            || link_pins(proc_root.join("root"), workspace_root)
        {
            audit.cwd_or_root_pins.push(pid);
        }
        if directory_has_pin(&proc_root.join("fd"), workspace_root)? {
            audit.fd_pins.push(pid);
        }
        if maps_have_writable_pin(&proc_root.join("maps"), workspace_root)? {
            audit.writable_map_pins.push(pid);
        }
        if include_mount_namespaces
            && mountinfo_has_mount(&proc_root.join("mountinfo"), workspace_root)?
        {
            audit.mount_namespace_pins.push(pid);
        }
    }
    deduplicate_audit(&mut audit);
    Ok(audit)
}

#[cfg(not(target_os = "linux"))]
fn audit_workspace_references(
    workspace_root: &Path,
    _cgroup_procs_path: Option<&Path>,
    _include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    Ok(ProcessAudit {
        workspace_root: workspace_root.to_path_buf(),
        ..ProcessAudit::default()
    })
}

#[cfg(target_os = "linux")]
fn link_pins(link: PathBuf, workspace_root: &Path) -> bool {
    fs::read_link(link)
        .ok()
        .is_some_and(|target| target.starts_with(workspace_root))
}

#[cfg(target_os = "linux")]
fn directory_has_pin(directory: &Path, workspace_root: &Path) -> PocResult<bool> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(PocError::io("audit process fds", directory, error)),
    };
    for entry in entries.flatten() {
        if fs::read_link(entry.path())
            .ok()
            .is_some_and(|target| target.starts_with(workspace_root))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn maps_have_writable_pin(maps_path: &Path, workspace_root: &Path) -> PocResult<bool> {
    let text = match fs::read_to_string(maps_path) {
        Ok(text) => text,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(PocError::io("audit process maps", maps_path, error)),
    };
    Ok(text.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let _range = fields.next();
        let writable = fields.next().is_some_and(|perms| perms.contains('w'));
        writable && line.contains(workspace_root.to_string_lossy().as_ref())
    }))
}

#[cfg(target_os = "linux")]
fn mountinfo_has_mount(mountinfo_path: &Path, workspace_root: &Path) -> PocResult<bool> {
    let text = match fs::read_to_string(mountinfo_path) {
        Ok(text) => text,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(false);
        }
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
    let expected = workspace_root.to_string_lossy();
    Ok(text.lines().any(|line| {
        line.split_whitespace()
            .nth(4)
            .is_some_and(|mountpoint| mountpoint == expected)
    }))
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
