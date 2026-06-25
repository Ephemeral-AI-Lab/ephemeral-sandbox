use std::fs;
use std::io::Read;
use std::os::fd::BorrowedFd;
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

use crate::runner::RunnerError;

const CHILD_WAIT_POLL: Duration = Duration::from_millis(5);

pub(super) fn wait_for_command_execution_scope(
    child: &mut std::process::Child,
    child_pgid: i32,
    timeout_seconds: Option<f64>,
    proc_dir: Option<BorrowedFd>,
) -> Result<i32, RunnerError> {
    let deadline = timeout_deadline(timeout_seconds);
    let self_pid = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    let mut root_exit_code = None;
    loop {
        if root_exit_code.is_none() {
            if let Some(status) = child.try_wait().map_err(RunnerError::Child)? {
                root_exit_code = Some(exit_code(status));
            }
        }
        if root_exit_code.is_some() {
            let has_other_live_members =
                pgid_has_other_live_members(child_pgid, self_pid, proc_dir);
            if !has_other_live_members {
                return Ok(root_exit_code.unwrap_or(0));
            }
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let _ = kill(Pid::from_raw(-child_pgid), Signal::SIGKILL);
            let _ = child.wait();
            return Err(RunnerError::TimedOut);
        }
        thread::sleep(CHILD_WAIT_POLL);
    }
}

fn timeout_deadline(timeout_seconds: Option<f64>) -> Option<Instant> {
    timeout_seconds
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| Instant::now() + Duration::from_secs_f64(seconds))
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .or_else(|| status.signal().map(|sig| -sig))
        .unwrap_or(128)
}

fn pgid_has_other_live_members(pgid: i32, self_pid: i32, proc_dir: Option<BorrowedFd>) -> bool {
    let Some(proc_dir) = proc_dir else {
        return pgid_has_other_live_members_by_path(pgid, self_pid);
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
        if proc_stat_pgid_at(proc_dir, pid)
            .is_some_and(|(entry_pgid, state)| entry_pgid == pgid && state != 'Z')
        {
            return true;
        }
    }
    false
}

fn pgid_has_other_live_members_by_path(pgid: i32, self_pid: i32) -> bool {
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

fn proc_stat_pgid_at(proc_dir: BorrowedFd, pid: i32) -> Option<(i32, char)> {
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

fn parse_proc_stat(stat: &str) -> Option<(i32, char)> {
    let close = stat.rfind(") ")?;
    let fields: Vec<&str> = stat[close + 2..].split_whitespace().collect();
    let state = fields.first()?.chars().next()?;
    let pgrp = fields.get(2)?.parse::<i32>().ok()?;
    Some((pgrp, state))
}
