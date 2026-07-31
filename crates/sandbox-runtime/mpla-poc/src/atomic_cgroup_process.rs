//! Linux process launch with atomic cgroup-v2 placement.
//!
//! This module contains the small unsafe syscall boundary needed for
//! `clone3(CLONE_INTO_CGROUP)`. Higher-level runtime crates remain entirely
//! safe and receive an owned child handle with ordinary pipe and wait methods.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Output};

use crate::STORAGE_ADMIN_TRUSTED_EXECUTABLE;

const CLONE_INTO_CGROUP: u64 = 1_u64 << 33;

pub struct AtomicCgroupChild {
    pid: libc::pid_t,
    status: Option<ExitStatus>,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
}

impl AtomicCgroupChild {
    pub fn id(&self) -> u32 {
        u32::try_from(self.pid).expect("kernel child PID is non-negative")
    }

    pub fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    #[allow(clippy::undocumented_unsafe_blocks)]
    pub fn kill(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        let result = unsafe { libc::kill(self.pid, libc::SIGKILL) };
        if result == 0 {
            Ok(())
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }

    #[allow(clippy::undocumented_unsafe_blocks)]
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        loop {
            let mut raw_status = 0;
            let result = unsafe { libc::waitpid(self.pid, &mut raw_status, 0) };
            if result == self.pid {
                let status = ExitStatus::from_raw(raw_status);
                self.status = Some(status);
                return Ok(status);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub fn wait_with_output(mut self) -> io::Result<Output> {
        drop(self.stdin.take());
        let stdout_reader = self.stdout.take().map(|mut stdout| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).map(|_| bytes)
            })
        });
        let stderr_reader = self.stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).map(|_| bytes)
            })
        });
        let status = self.wait()?;
        let stdout = join_reader(stdout_reader, "stdout")?;
        let stderr = join_reader(stderr_reader, "stderr")?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

fn join_reader(
    reader: Option<std::thread::JoinHandle<io::Result<Vec<u8>>>>,
    stream: &str,
) -> io::Result<Vec<u8>> {
    match reader {
        Some(reader) => reader
            .join()
            .map_err(|_| io::Error::other(format!("storage-admin {stream} reader panicked")))?,
        None => Ok(Vec::new()),
    }
}

impl Drop for AtomicCgroupChild {
    fn drop(&mut self) {
        let _ = self.kill();
        let _ = self.wait();
    }
}

/// Spawn the fixed ordinary helper directly in the bound workload cgroup.
pub fn spawn_storage_admin_helper_into_cgroup(
    workload_cgroup_procs: &Path,
) -> io::Result<AtomicCgroupChild> {
    spawn_storage_admin_helper(workload_cgroup_procs, None)
}

/// Spawn the fixed publication helper directly in the bound workload cgroup.
pub fn spawn_storage_admin_publication_helper_into_cgroup(
    workload_cgroup_procs: &Path,
) -> io::Result<AtomicCgroupChild> {
    spawn_storage_admin_helper(workload_cgroup_procs, Some("--publication-sequence"))
}

/// Spawn a fixed helper directly in the bound workload cgroup.
///
/// The child performs only async-signal-safe descriptor operations between
/// `clone3` and `execve`. The helper path and argument are compile-time fixed,
/// and its environment is empty.
#[allow(clippy::undocumented_unsafe_blocks)]
fn spawn_storage_admin_helper(
    workload_cgroup_procs: &Path,
    argument: Option<&str>,
) -> io::Result<AtomicCgroupChild> {
    let cgroup_dir = workload_cgroup_procs.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "workload cgroup.procs has no parent",
        )
    })?;
    let cgroup = fs::File::open(cgroup_dir)?;
    let (stdin_read, stdin_write) = pipe_cloexec()?;
    let (stdout_read, stdout_write) = pipe_cloexec()?;
    let (stderr_read, stderr_write) = pipe_cloexec()?;
    let executable = CString::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let argument = argument
        .map(CString::new)
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let argv = [
        executable.as_ptr(),
        argument
            .as_ref()
            .map_or(std::ptr::null(), |argument| argument.as_ptr()),
        std::ptr::null(),
    ];
    let envp: [*const libc::c_char; 1] = [std::ptr::null()];
    let args = libc::clone_args {
        flags: CLONE_INTO_CGROUP,
        pidfd: 0,
        child_tid: 0,
        parent_tid: 0,
        exit_signal: u64::try_from(libc::SIGCHLD).expect("SIGCHLD is non-negative"),
        stack: 0,
        stack_size: 0,
        tls: 0,
        set_tid: 0,
        set_tid_size: 0,
        cgroup: u64::try_from(cgroup.as_raw_fd()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "negative cgroup descriptor")
        })?,
    };
    let child_pid = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &args as *const libc::clone_args,
            std::mem::size_of::<libc::clone_args>(),
        )
    };
    if child_pid == -1 {
        return Err(io::Error::last_os_error());
    }
    if child_pid == 0 {
        unsafe {
            exec_storage_admin_publication_helper(
                stdin_read.as_raw_fd(),
                stdin_write.as_raw_fd(),
                stdout_read.as_raw_fd(),
                stdout_write.as_raw_fd(),
                stderr_read.as_raw_fd(),
                stderr_write.as_raw_fd(),
                executable.as_ptr(),
                argv.as_ptr(),
                envp.as_ptr(),
            )
        }
    }
    let pid = libc::pid_t::try_from(child_pid)
        .map_err(|_| io::Error::other("clone3 returned an out-of-range PID"))?;
    if pid < 0 {
        return Err(io::Error::other("clone3 returned a negative PID"));
    }
    drop(stdin_read);
    drop(stdout_write);
    drop(stderr_write);
    Ok(AtomicCgroupChild {
        pid,
        status: None,
        stdin: Some(File::from(stdin_write)),
        stdout: Some(File::from(stdout_read)),
        stderr: Some(File::from(stderr_read)),
    })
}

#[allow(clippy::undocumented_unsafe_blocks)]
fn pipe_cloexec() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [-1; 2];
    let result = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let read = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    Ok((read, write))
}

#[allow(clippy::too_many_arguments, clippy::undocumented_unsafe_blocks)]
unsafe fn exec_storage_admin_publication_helper(
    stdin_read: RawFd,
    stdin_write: RawFd,
    stdout_read: RawFd,
    stdout_write: RawFd,
    stderr_read: RawFd,
    stderr_write: RawFd,
    executable: *const libc::c_char,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
) -> ! {
    if unsafe { libc::dup2(stdin_read, libc::STDIN_FILENO) } == -1
        || unsafe { libc::dup2(stdout_write, libc::STDOUT_FILENO) } == -1
        || unsafe { libc::dup2(stderr_write, libc::STDERR_FILENO) } == -1
    {
        unsafe { libc::_exit(126) }
    }
    for descriptor in [
        stdin_read,
        stdin_write,
        stdout_read,
        stdout_write,
        stderr_read,
        stderr_write,
    ] {
        if descriptor > libc::STDERR_FILENO {
            unsafe {
                libc::close(descriptor);
            }
        }
    }
    unsafe {
        libc::execve(executable, argv, envp);
        libc::_exit(127)
    }
}
