#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::fd::IntoRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use crate::session::{HolderNsFds, WorkspaceManagerError};
#[cfg(target_os = "linux")]
use nix::errno::Errno;
#[cfg(target_os = "linux")]
use nix::fcntl::{fcntl, FcntlArg, FdFlag, OFlag};
#[cfg(target_os = "linux")]
use nix::unistd::read;
#[cfg(target_os = "linux")]
use rustix::event::{poll, PollFd, PollFlags};
#[cfg(target_os = "linux")]
use rustix::io::Errno as RustixErrno;

#[cfg(target_os = "linux")]
use super::setup_error;
#[cfg(target_os = "linux")]
use super::NamespaceFd;
use super::{NamespacePlan, NamespaceRuntime};

impl NamespaceRuntime {
    pub(crate) fn open_ns_fds(
        &self,
        holder_pid: i32,
        plan: NamespacePlan,
    ) -> Result<HolderNsFds, WorkspaceManagerError> {
        if holder_pid <= 0 {
            return Ok(HolderNsFds::default());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (holder_pid, plan);
            Ok(HolderNsFds::default())
        }
        #[cfg(target_os = "linux")]
        {
            let mut fds = HolderNsFds::default();
            for &fd in plan.fds() {
                set_fd(&mut fds, fd, open_inheritable_fd(fd.proc_path(holder_pid))?);
            }
            Ok(fds)
        }
    }
}

#[cfg(target_os = "linux")]
fn open_inheritable_fd(path: impl AsRef<std::path::Path>) -> Result<File, WorkspaceManagerError> {
    let file = File::open(path.as_ref()).map_err(setup_error)?;
    clear_cloexec(file.as_raw_fd())?;
    Ok(file)
}

#[cfg(target_os = "linux")]
fn set_fd(fds: &mut HolderNsFds, fd: NamespaceFd, file: File) {
    let raw_fd = file.into_raw_fd();
    match fd {
        NamespaceFd::User => fds.user = Some(raw_fd),
        NamespaceFd::Mnt => fds.mnt = Some(raw_fd),
        NamespaceFd::Pid => fds.pid = Some(raw_fd),
        NamespaceFd::Net => fds.net = Some(raw_fd),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn clear_cloexec(fd: RawFd) -> Result<(), WorkspaceManagerError> {
    fcntl(fd, FcntlArg::F_SETFD(FdFlag::empty())).map_err(setup_error)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn set_nonblocking(fd: RawFd) -> Result<(), WorkspaceManagerError> {
    let flags = fcntl(fd, FcntlArg::F_GETFL).map_err(setup_error)?;
    fcntl(
        fd,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    )
    .map_err(setup_error)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn expect_line(
    fd: RawFd,
    prefix: &[u8],
    timeout_s: f64,
) -> Result<(), WorkspaceManagerError> {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s.max(0.0));
    let mut buf = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(WorkspaceManagerError::SetupFailed {
                step: format!(
                    "ns_holder did not signal {}",
                    String::from_utf8_lossy(prefix)
                ),
            });
        }
        let mut chunk = [0_u8; 64];
        match read(fd, &mut chunk) {
            Ok(0) => {
                return Err(WorkspaceManagerError::SetupFailed {
                    step: "ns_holder closed pipe before signaling".to_owned(),
                });
            }
            Ok(read) => {
                buf.extend_from_slice(&chunk[..read]);
                if buf.contains(&b'\n') {
                    if buf.starts_with(prefix) {
                        return Ok(());
                    }
                    return Err(WorkspaceManagerError::SetupFailed {
                        step: format!("unexpected ns_holder signal: {buf:?}"),
                    });
                }
            }
            Err(Errno::EAGAIN) => wait_readable(fd, deadline)?,
            Err(Errno::EINTR) => {}
            Err(error) => return Err(setup_error(error)),
        }
    }
}

#[cfg(target_os = "linux")]
fn wait_readable(fd: RawFd, deadline: Instant) -> Result<(), WorkspaceManagerError> {
    let timeout_ms = poll_timeout_ms(deadline);
    let file = File::open(format!("/proc/self/fd/{fd}")).map_err(setup_error)?;
    let mut fds = [PollFd::new(&file, PollFlags::IN)];
    match poll(&mut fds, timeout_ms) {
        Ok(_) => Ok(()),
        Err(RustixErrno::INTR) => Ok(()),
        Err(error) => Err(setup_error(error)),
    }
}

#[cfg(target_os = "linux")]
fn poll_timeout_ms(deadline: Instant) -> i32 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return 0;
    }
    i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX)
}

#[cfg(target_os = "linux")]
pub(super) fn write_all_fd(fd: RawFd, bytes: &[u8]) -> Result<(), WorkspaceManagerError> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(format!("/proc/self/fd/{fd}"))
        .map_err(setup_error)?;
    file.write_all(bytes).map_err(setup_error)
}
