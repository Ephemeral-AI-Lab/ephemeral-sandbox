use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use linux_namespace_subprocess::protocol::{Fd, NsFds};
#[cfg(target_os = "linux")]
use nix::errno::Errno;
#[cfg(target_os = "linux")]
use nix::fcntl::{fcntl, FcntlArg, FdFlag, OFlag};
#[cfg(target_os = "linux")]
use nix::unistd::read;

use crate::isolated_workspace::error::IsolatedError;

#[cfg(target_os = "linux")]
use super::setup_error;
use super::NamespaceRuntime;

impl NamespaceRuntime {
    pub(crate) fn open_ns_fds(
        &self,
        holder_pid: i32,
    ) -> Result<HashMap<String, i32>, IsolatedError> {
        if self.stub || holder_pid <= 0 {
            return Ok(HashMap::new());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = holder_pid;
            Ok(HashMap::new())
        }
        #[cfg(target_os = "linux")]
        {
            let paths = [
                ("user", format!("/proc/{holder_pid}/ns/user")),
                ("mnt", format!("/proc/{holder_pid}/ns/mnt")),
                ("pid", format!("/proc/{holder_pid}/ns/pid_for_children")),
                ("net", format!("/proc/{holder_pid}/ns/net")),
            ];
            paths
                .into_iter()
                .map(|(name, path)| Ok((name.to_owned(), open_inheritable_fd(path)?)))
                .collect()
        }
    }
}

#[cfg(target_os = "linux")]
fn open_inheritable_fd(path: impl AsRef<std::path::Path>) -> Result<RawFd, IsolatedError> {
    let file = File::open(path.as_ref()).map_err(setup_error)?;
    clear_cloexec(file.as_raw_fd())?;
    Ok(file.into_raw_fd())
}

#[cfg(target_os = "linux")]
pub(super) fn clear_cloexec(fd: RawFd) -> Result<(), IsolatedError> {
    fcntl(fd, FcntlArg::F_SETFD(FdFlag::empty())).map_err(setup_error)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn set_nonblocking(fd: RawFd) -> Result<(), IsolatedError> {
    let flags = fcntl(fd, FcntlArg::F_GETFL).map_err(setup_error)?;
    fcntl(
        fd,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    )
    .map_err(setup_error)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn expect_line(fd: RawFd, prefix: &[u8], timeout_s: f64) -> Result<(), IsolatedError> {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s.max(0.0));
    let mut buf = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(IsolatedError::SetupFailed {
                step: format!(
                    "ns_holder did not signal {}",
                    String::from_utf8_lossy(prefix)
                ),
            });
        }
        let mut chunk = [0_u8; 64];
        match read(fd, &mut chunk) {
            Ok(0) => {
                return Err(IsolatedError::SetupFailed {
                    step: "ns_holder closed pipe before signaling".to_owned(),
                });
            }
            Ok(read) => {
                buf.extend_from_slice(&chunk[..read]);
                if buf.contains(&b'\n') {
                    if buf.starts_with(prefix) {
                        return Ok(());
                    }
                    return Err(IsolatedError::SetupFailed {
                        step: format!("unexpected ns_holder signal: {buf:?}"),
                    });
                }
            }
            Err(Errno::EAGAIN) => thread::sleep(Duration::from_millis(10)),
            Err(Errno::EINTR) => {}
            Err(error) => return Err(setup_error(error)),
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn write_all_fd(fd: RawFd, bytes: &[u8]) -> Result<(), IsolatedError> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(format!("/proc/self/fd/{fd}"))
        .map_err(setup_error)?;
    file.write_all(bytes).map_err(setup_error)
}

#[cfg(target_os = "linux")]
pub(super) fn ns_fds_from_map(map: &HashMap<String, i32>) -> Option<NsFds> {
    (!map.is_empty()).then(|| NsFds {
        user: map.get("user").copied().map(Fd),
        mnt: map.get("mnt").copied().map(Fd),
        pid: map.get("pid").copied().map(Fd),
        net: map.get("net").copied().map(Fd),
    })
}
