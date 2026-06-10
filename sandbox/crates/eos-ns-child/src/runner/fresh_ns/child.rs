//! Child-process wait loops and process-group liveness probes for fresh-ns mode.

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::fd::BorrowedFd;
#[cfg(target_os = "linux")]
use std::os::unix::process::ExitStatusExt;
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use rustix::process::{getpgrp, kill_process_group, Pid, Signal};

#[cfg(target_os = "linux")]
use crate::runner::error::RunnerError;

#[cfg(target_os = "linux")]
const CHILD_WAIT_POLL: Duration = Duration::from_millis(5);

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
pub(super) enum TimeoutKill {
    ProcessGroup,
}

#[cfg(target_os = "linux")]
pub(super) fn wait_for_child(
    child: &mut std::process::Child,
    timeout_seconds: Option<f64>,
    timeout_kill: TimeoutKill,
) -> Result<i32, RunnerError> {
    let deadline = timeout_seconds
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| Instant::now() + Duration::from_secs_f64(seconds));
    loop {
        if let Some(status) = child.try_wait().map_err(RunnerError::Child)? {
            return Ok(status
                .code()
                .or_else(|| status.signal().map(|sig| -sig))
                .unwrap_or(128));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            match timeout_kill {
                TimeoutKill::ProcessGroup => {
                    let pid = Pid::from_child(child);
                    let _ = kill_process_group(pid, Signal::Kill);
                }
            }
            let _ = child.wait();
            return Err(RunnerError::TimedOut);
        }
        thread::sleep(CHILD_WAIT_POLL);
    }
}

#[cfg(target_os = "linux")]
pub(super) fn wait_for_command_execution_scope(
    child: &mut std::process::Child,
    timeout_seconds: Option<f64>,
    proc_dir: Option<BorrowedFd>,
) -> Result<i32, RunnerError> {
    let deadline = timeout_seconds
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| Instant::now() + Duration::from_secs_f64(seconds));
    let pgid = getpgrp().as_raw_nonzero().get();
    let self_pid = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    let mut root_exit_code = None;
    loop {
        if root_exit_code.is_none() {
            if let Some(status) = child.try_wait().map_err(RunnerError::Child)? {
                root_exit_code = Some(
                    status
                        .code()
                        .or_else(|| status.signal().map(|sig| -sig))
                        .unwrap_or(128),
                );
            }
        }
        if root_exit_code.is_some()
            && !process_group_has_other_live_members(pgid, self_pid, proc_dir)
        {
            return Ok(root_exit_code.unwrap_or(0));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if let Some(pid) = Pid::from_raw(pgid) {
                let _ = kill_process_group(pid, Signal::Kill);
            }
            let _ = child.wait();
            return Err(RunnerError::TimedOut);
        }
        thread::sleep(CHILD_WAIT_POLL);
    }
}

/// True when a process other than the runner (`self_pid`) shares `pgid` and is
/// not a zombie. When `proc_dir` is set it enumerates through that pre-opened
/// `/proc` handle so the model-shell `/proc` mount mask cannot hide live
/// background members from the scope-wait; otherwise it reads `/proc` by path.
#[cfg(target_os = "linux")]
fn process_group_has_other_live_members(
    pgid: i32,
    self_pid: i32,
    proc_dir: Option<BorrowedFd>,
) -> bool {
    let Some(proc_dir) = proc_dir else {
        return process_group_has_other_live_members_by_path(pgid, self_pid);
    };
    let Ok(dir) = rustix::fs::Dir::read_from(proc_dir) else {
        return false;
    };
    for entry in dir {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .ok()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        if proc_stat_process_group_at(proc_dir, pid)
            .is_some_and(|(entry_pgid, state)| entry_pgid == pgid && state != 'Z')
        {
            return true;
        }
    }
    false
}

#[cfg(target_os = "linux")]
fn process_group_has_other_live_members_by_path(pgid: i32, self_pid: i32) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            return false;
        };
        if pid == self_pid {
            return false;
        }
        fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| parse_proc_stat(&stat))
            .is_some_and(|(entry_pgid, state)| entry_pgid == pgid && state != 'Z')
    })
}

#[cfg(target_os = "linux")]
fn proc_stat_process_group_at(proc_dir: BorrowedFd, pid: i32) -> Option<(i32, char)> {
    let fd = rustix::fs::openat(
        proc_dir,
        format!("{pid}/stat"),
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let mut stat = String::new();
    fs::File::from(fd).read_to_string(&mut stat).ok()?;
    parse_proc_stat(&stat)
}

#[cfg(target_os = "linux")]
fn parse_proc_stat(stat: &str) -> Option<(i32, char)> {
    let close = stat.rfind(") ")?;
    let fields: Vec<&str> = stat[close + 2..].split_whitespace().collect();
    let state = fields.first()?.chars().next()?;
    let pgrp = fields.get(2)?.parse::<i32>().ok()?;
    Some((pgrp, state))
}
