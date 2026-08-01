#![cfg(target_os = "linux")]

use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use sandbox_runtime_mpla_poc::{FaultInjector, OperationId, PocError, SessionId};

const HELPER_TOKEN: &str = ".process-authority-helper-token";
const HOLDER_PID: &str = ".process-authority-holder-pid";
const HOLDER_READY: &str = ".process-authority-holder-ready";

#[test]
#[ignore = "requires Linux OverlayFS, mount namespaces, pidfds, and CAP_SYS_ADMIN"]
fn public_seal_drains_sets_id_unshare_and_writable_fd_without_a_cgroup() {
    let root = TestDirectory::new("process-authority");
    let lower = root.0.join("lower");
    fs::create_dir(&lower).expect("create lower layer");
    fs::write(lower.join("lower-sentinel"), b"lower").expect("write lower sentinel");
    let allocation_operation = OperationId::from_string("process-authority-allocation");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &allocation_operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        SessionId::new(),
        &allocation_operation,
    )
    .expect("issue workspace lease");
    let writer = lease.writer.clone();
    let mut session = match sandbox_runtime_mpla_poc::MplaSession::open(
        &root.0.join("control"),
        allocation,
        lease,
        vec![lower],
        None,
    ) {
        Ok(session) => session,
        Err(error) if overlay_mount_unavailable(&error) => return,
        Err(error) => panic!("open public overlay session: {error}"),
    };
    let workspace = session
        .workspace_root()
        .expect("open session has a workspace")
        .to_path_buf();
    fs::write(workspace.join(HELPER_TOKEN), b"ready").expect("write helper token");
    let helper = std::env::current_exe().expect("resolve process-authority test executable");
    let arguments = vec![
        "--exact".to_owned(),
        "escaped_holder_helper".to_owned(),
        "--ignored".to_owned(),
        "--test-threads=1".to_owned(),
    ];

    let receipt = session
        .execute(&writer, &helper, &arguments, Duration::from_secs(10))
        .expect("execute escaped-holder helper");
    assert!(receipt.success, "escaped-holder helper failed: {receipt:?}");
    let holder_pid = fs::read_to_string(workspace.join(HOLDER_PID))
        .expect("read escaped holder PID")
        .trim()
        .parse::<i32>()
        .expect("parse escaped holder PID");
    let mut cleanup = HolderCleanup::new(holder_pid);
    assert!(Path::new("/proc").join(holder_pid.to_string()).exists());

    let sealed = session
        .seal(
            &OperationId::from_string("process-authority-seal"),
            &mut FaultInjector::default(),
        )
        .expect("seal session with escaped holder");

    assert!(sealed
        .quiescence
        .killed_or_signaled_pids
        .contains(&holder_pid));
    assert!(sealed.quiescence.pre_unmount_audit.is_clear());
    assert!(sealed.quiescence.post_unmount_audit.is_clear());
    assert!(!Path::new("/proc").join(holder_pid.to_string()).exists());
    cleanup.disarm();
}

#[test]
#[ignore = "process-authority subprocess helper"]
fn escaped_holder_helper() {
    assert!(Path::new(HELPER_TOKEN).is_file(), "missing helper token");
    assert!(
        Path::new("/usr/bin/setsid").is_file(),
        "setsid is unavailable"
    );
    assert!(
        Path::new("/usr/bin/unshare").is_file(),
        "unshare is unavailable"
    );
    let script = format!(
        "exec 9<>authority-held; printf '%s\\n' \"$$\" > {HOLDER_PID}; : > {HOLDER_READY}; exec /bin/sleep 3600"
    );
    let mut holder = Command::new("/usr/bin/setsid")
        .arg("/usr/bin/unshare")
        .arg("--mount")
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn setsid/unshare holder");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !Path::new(HOLDER_READY).is_file() {
        if let Some(status) = holder.try_wait().expect("poll escaped holder") {
            panic!("escaped holder exited before readiness: {status}");
        }
        assert!(Instant::now() < deadline, "escaped holder was never ready");
        thread::sleep(Duration::from_millis(10));
    }
}

struct HolderCleanup {
    pidfd: OwnedFd,
    armed: bool,
}

impl HolderCleanup {
    fn new(pid: i32) -> Self {
        // SAFETY: pidfd_open consumes only scalar arguments and returns a new
        // descriptor on success.
        let raw_pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
        assert!(
            raw_pidfd >= 0,
            "open escaped holder pidfd: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the successful syscall returned a new descriptor owned by
        // this cleanup guard.
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw_pidfd) };
        Self { pidfd, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for HolderCleanup {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: pidfd_send_signal consumes a valid pidfd and scalar
            // signal arguments; the pidfd pins the exact holder process.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
}

fn overlay_mount_unavailable(error: &PocError) -> bool {
    match error {
        PocError::Unsupported(message) => {
            message == "Linux statx did not report STATX_MNT_ID_UNIQUE"
        }
        PocError::Io {
            operation, source, ..
        } => {
            matches!(
                *operation,
                "open overlay mount context"
                    | "configure overlay lowerdir+"
                    | "configure overlay userxattr"
                    | "configure pinned overlay upper"
                    | "configure pinned overlay work"
                    | "create anchored overlay"
                    | "fsmount anchored overlay"
                    | "attach anchored overlay"
                    | "statx unique mount identity"
            ) && matches!(
                source.raw_os_error(),
                Some(libc::EPERM | libc::EACCES | libc::ENOSYS | libc::EOPNOTSUPP)
            )
        }
        _ => false,
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mpla-poc-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create test directory");
        Self(fs::canonicalize(path).expect("canonicalize test directory"))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
