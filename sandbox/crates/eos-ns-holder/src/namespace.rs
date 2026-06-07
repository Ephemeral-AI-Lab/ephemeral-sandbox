#[cfg(target_os = "linux")]
use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, IntoRawFd};
use std::os::fd::{OwnedFd, RawFd};
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use rustix::mount::{mount_change, MountPropagationFlags};
#[cfg(target_os = "linux")]
use rustix::thread::{set_thread_gid, set_thread_uid, unshare, UnshareFlags};

use crate::NsHolderError;

/// The namespace FDs the holder pins open for its whole lifetime.
///
/// Wrapping [`OwnedFd`] gives RAII close-on-drop for free with zero `unsafe`:
/// when the holder process exits the kernel tears the namespaces down once the
/// last referencing FD (and the holder task) is gone. The daemon reads the
/// matching `/proc/{holder_pid}/ns/*` symlinks while this struct keeps the
/// holder alive; the daemon side opens these symlinks against the live holder.
// The four namespace FDs are pinned solely for close-on-drop RAII (see the
// doc above): they are never read in-process — the daemon reads the matching
// `/proc/{holder_pid}/ns/*` symlinks instead — so `dead_code` is expected.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct HeldNamespaces {
    /// User namespace FD (`/proc/self/ns/user`).
    pub user: OwnedFd,
    /// Mount namespace FD (`/proc/self/ns/mnt`).
    pub mnt: OwnedFd,
    /// PID namespace-for-children FD (`/proc/self/ns/pid_for_children`).
    pub pid: OwnedFd,
    /// Network namespace FD (`/proc/self/ns/net`).
    pub net: OwnedFd,
    #[cfg(target_os = "linux")]
    _pid_init: Option<PidNamespaceInit>,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct PidNamespaceInit {
    pid: libc::pid_t,
}

#[cfg(target_os = "linux")]
impl Drop for PidNamespaceInit {
    fn drop(&mut self) {
        // SAFETY: `pid` came from `fork` in this process. Sending SIGTERM and
        // reaping with WNOHANG are best-effort cleanup for error paths; the
        // child also has PR_SET_PDEATHSIG for abrupt holder termination.
        unsafe {
            libc::kill(self.pid, libc::SIGTERM);
            let mut status = 0;
            libc::waitpid(self.pid, std::ptr::addr_of_mut!(status), libc::WNOHANG);
        }
    }
}

/// Recursively bind the parent's `/proc` over the inherited `/proc` so setns'd
/// shells inside the new mount namespace see a usable `/proc/self`.
///
/// Best-effort, shell-free: replaces the Rust `subprocess.run(["mount",
/// "--rbind", "/proc", "/proc"], check=False)` with a raw `mount(MS_BIND |
/// MS_REC)` syscall. Failure must NOT abort the holder.
#[cfg(target_os = "linux")]
pub(crate) fn rbind_proc() {
    let proc = b"/proc\0";
    // SAFETY: both source and target are static NUL-terminated strings, the
    // filesystem type and data pointers are null as required for a bind
    // mount, and failure is intentionally ignored to preserve Rust's
    // best-effort `mount --rbind /proc /proc` behavior.
    let _ = unsafe {
        libc::mount(
            proc.as_ptr().cast(),
            proc.as_ptr().cast(),
            std::ptr::null::<libc::c_char>(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null::<c_void>(),
        )
    };
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn rbind_proc() {}

/// `unshare` the full namespace stack on the calling (single-threaded) task and
/// pin the resulting `/proc/self/ns/*` FDs.
///
/// This is the Rust *consolidation* of the former Rust launcher's
/// `unshare(1)` flags. The previous path spawned the holder process via
/// `unshare --user --map-root-user --net --pid --mount --fork --kill-child
/// --propagation private`, so the namespaces were created by the `unshare`
/// binary, not inside the holder process. The Rust holder owns that step directly:
/// `unshare(CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWNET)` plus the
/// uid/gid map writes and `MS_PRIVATE` mount-propagation, then opens its own
/// `ns/{user,mnt,pid,net}` symlinks into a [`HeldNamespaces`].
///
/// # Safety
///
/// This function MUST run on a single-threaded process; the kernel rejects
/// `CLONE_NEWUSER` in a multithreaded process. The crate deliberately has no
/// tokio dependency and is invoked through the dedicated `eosd ns-holder`
/// subprocess.
#[cfg(target_os = "linux")]
pub(crate) fn unshare_namespace_stack(
    readiness_fd: RawFd,
    control_fd: RawFd,
) -> Result<HeldNamespaces, NsHolderError> {
    struct ParentIds {
        user: u32,
        group: u32,
    }

    let parent_ids = ParentIds {
        user: rustix::process::getuid().as_raw(),
        group: rustix::process::getgid().as_raw(),
    };
    unshare(
        UnshareFlags::NEWUSER | UnshareFlags::NEWNS | UnshareFlags::NEWPID | UnshareFlags::NEWNET,
    )
    .map_err(|_| NsHolderError::Unshare)?;
    write_if_exists("/proc/self/setgroups", b"deny\n")?;
    write_setup_file(
        "/proc/self/uid_map",
        format!("0 {} 1\n", parent_ids.user).as_bytes(),
    )?;
    write_setup_file(
        "/proc/self/gid_map",
        format!("0 {} 1\n", parent_ids.group).as_bytes(),
    )?;
    set_thread_gid(rustix::process::Gid::ROOT).map_err(|_| NsHolderError::Unshare)?;
    set_thread_uid(rustix::process::Uid::ROOT).map_err(|_| NsHolderError::Unshare)?;
    mount_change(
        "/",
        MountPropagationFlags::PRIVATE | MountPropagationFlags::REC,
    )
    .map_err(|_| NsHolderError::Unshare)?;
    let pid_init = fork_pid_namespace_init(readiness_fd, control_fd)?;
    wait_for_path("/proc/self/ns/pid_for_children")?;
    Ok(HeldNamespaces {
        user: open_owned_fd("/proc/self/ns/user")?,
        mnt: open_owned_fd("/proc/self/ns/mnt")?,
        pid: open_owned_fd("/proc/self/ns/pid_for_children")?,
        net: open_owned_fd("/proc/self/ns/net")?,
        _pid_init: Some(pid_init),
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn unshare_namespace_stack(
    _readiness_fd: RawFd,
    _control_fd: RawFd,
) -> Result<HeldNamespaces, NsHolderError> {
    Err(NsHolderError::Unshare)
}

#[cfg(target_os = "linux")]
fn write_if_exists(path: impl AsRef<Path>, value: &[u8]) -> Result<(), NsHolderError> {
    let path = path.as_ref();
    match fs::write(path, value) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(setup_io(path, err)),
    }
}

#[cfg(target_os = "linux")]
fn open_owned_fd(path: impl AsRef<Path>) -> Result<OwnedFd, NsHolderError> {
    let path = path.as_ref();
    let file = fs::File::open(path).map_err(|err| setup_io(path, err))?;
    let raw_fd = file.into_raw_fd();
    // SAFETY: `raw_fd` came from `File::into_raw_fd`, so this function becomes
    // the sole owner and closes it through `OwnedFd` drop.
    Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) })
}

#[cfg(target_os = "linux")]
fn write_setup_file(path: impl AsRef<Path>, value: &[u8]) -> Result<(), NsHolderError> {
    let path = path.as_ref();
    fs::write(path, value).map_err(|err| setup_io(path, err))
}

#[cfg(target_os = "linux")]
fn setup_io(path: &Path, source: std::io::Error) -> NsHolderError {
    NsHolderError::SetupIo {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(target_os = "linux")]
fn fork_pid_namespace_init(
    readiness_fd: RawFd,
    control_fd: RawFd,
) -> Result<PidNamespaceInit, NsHolderError> {
    // SAFETY: The holder is a dedicated single-threaded process. `fork` is used
    // here to reproduce `unshare --pid --fork`: the first child becomes PID 1 in
    // the new PID namespace, which materializes `/proc/self/ns/pid_for_children`
    // for the parent holder to pin and later hand to ns-runner children.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(NsHolderError::Unshare);
    }
    if pid == 0 {
        run_pid_namespace_init(readiness_fd, control_fd);
    }
    Ok(PidNamespaceInit { pid })
}

#[cfg(target_os = "linux")]
fn run_pid_namespace_init(readiness_fd: RawFd, control_fd: RawFd) -> ! {
    // SAFETY: runs in the freshly-forked, single-threaded child; `readiness_fd`
    // and `control_fd` are inherited raw fds valid to close here, and
    // `prctl`/`getppid`/`_exit` take no Rust references. The child does not
    // participate in the handshake and must not keep inherited pipe descriptors
    // open; closing the standard descriptors is not necessary because the daemon
    // starts ns-holder with stdio redirected.
    unsafe {
        libc::close(readiness_fd);
        libc::close(control_fd);
        install_exit_signal_handler(libc::SIGTERM);
        install_exit_signal_handler(libc::SIGINT);
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
        if libc::getppid() == 1 {
            libc::_exit(0);
        }
    }
    loop {
        // SAFETY: `pause` has no pointer arguments and simply waits for a
        // signal. The SIGTERM/SIGINT handler above exits this PID-namespace init.
        unsafe {
            libc::pause();
        }
    }
}

#[cfg(target_os = "linux")]
fn install_exit_signal_handler(signal: libc::c_int) {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

    let Ok(signal) = Signal::try_from(signal) else {
        return;
    };
    let action = SigAction::new(
        SigHandler::Handler(exit_signal_handler),
        SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: `exit_signal_handler` is an `extern "C"` handler that only calls
    // async-signal-safe `_exit(2)`, and the action uses an empty mask/flags.
    let _ = unsafe { sigaction(signal, &action) };
}

#[cfg(target_os = "linux")]
extern "C" fn exit_signal_handler(_signal: libc::c_int) {
    // SAFETY: `_exit` is async-signal-safe and terminates the PID-namespace init
    // without running Rust destructors from inside a signal handler.
    unsafe {
        libc::_exit(0);
    }
}

#[cfg(target_os = "linux")]
fn wait_for_path(path: impl AsRef<Path>) -> Result<(), NsHolderError> {
    let path = path.as_ref();
    for _ in 0..100 {
        if path.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    open_owned_fd(path).map(drop)
}

#[cfg(test)]
impl HeldNamespaces {
    pub(crate) fn for_test() -> std::io::Result<Self> {
        Ok(Self {
            user: dev_null_fd()?,
            mnt: dev_null_fd()?,
            pid: dev_null_fd()?,
            net: dev_null_fd()?,
            #[cfg(target_os = "linux")]
            _pid_init: None,
        })
    }
}

#[cfg(test)]
fn dev_null_fd() -> std::io::Result<OwnedFd> {
    Ok(std::fs::File::open("/dev/null")?.into())
}
