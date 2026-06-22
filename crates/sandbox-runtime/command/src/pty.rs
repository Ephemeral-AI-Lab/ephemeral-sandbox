use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use rustix::event::{poll, PollFd, PollFlags};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix::io::{fcntl_setfd, FdFlags};
use rustix::pipe::pipe;
#[cfg(target_os = "linux")]
use rustix::pty::ioctl_tiocgptpeer;
#[cfg(not(target_os = "linux"))]
use rustix::pty::ptsname;
use rustix::pty::{grantpt, openpt, unlockpt, OpenptFlags};
use sandbox_runtime_namespace_process::runner::protocol::NamespaceRunnerRequest;
use serde_json::Value;

use crate::{transcript::TranscriptTimestampPrefixer, CommandError};

/// Cap on how long a single `write_command_stdin` pushes bytes into the PTY before
/// returning a structured backpressure error. The master is non-blocking, so a
/// consumer that never drains its stdin cannot wedge the writer past this bound.
const STDIN_WRITE_DEADLINE: Duration = Duration::from_secs(2);

pub(crate) struct PtyProcess {
    pgid: Option<i32>,
    writer: Mutex<File>,
    reader_done: Mutex<Option<mpsc::Receiver<()>>>,
    runner_result_done: Mutex<Option<mpsc::Receiver<io::Result<Vec<u8>>>>>,
    child: Mutex<Option<Child>>,
}

pub(crate) struct PendingPtyProcess {
    process: PtyProcess,
    start_ack: std::os::fd::OwnedFd,
    request_payload: RequestPayload,
}

struct RequestPayload {
    writer: OwnedFd,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PtyProcessExitStatus {
    exit_code: Option<i64>,
    signal: Option<i32>,
}

impl PtyProcessExitStatus {
    #[must_use]
    pub(crate) fn unwaitable() -> Self {
        Self {
            exit_code: None,
            signal: None,
        }
    }

    #[must_use]
    pub(crate) fn from_status(status: ExitStatus) -> Self {
        let signal = status.signal();
        let exit_code = status
            .code()
            .map(i64::from)
            .or_else(|| signal.map(|signal| -i64::from(signal)));
        Self { exit_code, signal }
    }

    #[must_use]
    pub(crate) const fn exit_code(self) -> Option<i64> {
        self.exit_code
    }

    #[must_use]
    pub(crate) const fn signal(self) -> Option<i32> {
        self.signal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PtyProcessExit {
    Running,
    Exited(PtyProcessExitStatus),
}

/// Why the substrate killed a command process group. The owning run
/// maps this to the final status. A killed command DISCARDS the overlay and
/// never OCC-merges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillReason {
    /// A caller asked to cancel (Ctrl-C/Ctrl-D, the cancel op, or run teardown).
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCompletionStatus {
    status: String,
    exit_code: i64,
}

impl CommandCompletionStatus {
    #[must_use]
    pub(crate) fn from_process_and_runner(
        process_exit: PtyProcessExitStatus,
        runner: Option<&CommandRunnerResult>,
        kill: Option<KillReason>,
    ) -> Self {
        let mut exit_code = runner
            .map(CommandRunnerResult::exit_code)
            .or_else(|| process_exit.exit_code())
            .unwrap_or(1);
        let mut status = runner
            .and_then(CommandRunnerResult::status)
            .unwrap_or("error")
            .to_owned();
        match kill {
            Some(KillReason::Cancelled) => {
                status = "cancelled".to_owned();
                exit_code = 130;
            }
            None => {}
        }
        Self { status, exit_code }
    }

    #[must_use]
    pub(crate) fn status(&self) -> &str {
        &self.status
    }

    #[must_use]
    pub(crate) const fn exit_code(&self) -> i64 {
        self.exit_code
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommandRunnerResult {
    exit_code: i64,
    status: Option<String>,
}

impl CommandRunnerResult {
    #[must_use]
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let value = serde_json::from_slice::<Value>(bytes).ok()?;
        Self::from_value(value)
    }

    #[must_use]
    pub(crate) fn from_value(value: Value) -> Option<Self> {
        let exit_code = value.get("exit_code").and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })?;
        let status = value
            .get("payload")
            .and_then(Value::as_object)
            .and_then(|payload| payload.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Some(Self { exit_code, status })
    }

    #[must_use]
    pub(crate) const fn exit_code(&self) -> i64 {
        self.exit_code
    }

    #[must_use]
    pub(crate) fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }
}

impl PtyProcess {
    /// Process-free scaffold backing test-only inactive command processes.
    #[must_use]
    pub(crate) fn inactive(writer: File) -> Self {
        Self {
            pgid: None,
            writer: Mutex::new(writer),
            reader_done: Mutex::new(None),
            runner_result_done: Mutex::new(None),
            child: Mutex::new(None),
        }
    }

    #[must_use]
    pub(crate) fn inactive_with_process_group_for_test(writer: File, pgid: i32) -> Self {
        Self {
            pgid: Some(pgid),
            writer: Mutex::new(writer),
            reader_done: Mutex::new(None),
            runner_result_done: Mutex::new(None),
            child: Mutex::new(None),
        }
    }

    /// Push `bytes` to the command's stdin without blocking unbounded. The master
    /// is non-blocking; when the consumer stops draining, `write` returns
    /// `WouldBlock` and we wait for writability only up to `STDIN_WRITE_DEADLINE`
    /// before returning a structured backpressure error. Cancel/terminate is a
    /// separate (`killpg`) path, so the command stays controllable throughout.
    pub(crate) fn write_command_stdin(&self, bytes: &[u8]) -> io::Result<()> {
        let mut writer = lock(&self.writer);
        let deadline = Instant::now() + STDIN_WRITE_DEADLINE;
        let mut offset = 0;
        while offset < bytes.len() {
            match writer.write(&bytes[offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "command stdin closed",
                    ));
                }
                Ok(written) => offset += written,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    let timeout_ms = poll_timeout_ms(deadline);
                    if timeout_ms == 0 {
                        return Err(stdin_backpressure());
                    }
                    let mut fds = [PollFd::new(&*writer, PollFlags::OUT)];
                    match poll(&mut fds, timeout_ms) {
                        Ok(0) => return Err(stdin_backpressure()),
                        Ok(_) => {}
                        Err(rustix::io::Errno::INTR) => {}
                        Err(err) => return Err(io::Error::from(err)),
                    }
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    pub(crate) fn terminate(&self) {
        if let Some(pgid) = self.pgid {
            terminate_process_group(pgid);
        }
    }

    #[must_use]
    pub(crate) const fn process_group_id(&self) -> Option<i32> {
        self.pgid
    }

    #[must_use]
    pub(crate) fn take_exit(&self) -> PtyProcessExit {
        let mut child = lock(&self.child);
        match child.as_mut() {
            Some(handle) => match handle.try_wait() {
                Ok(Some(status)) => {
                    let _ = child.take();
                    PtyProcessExit::Exited(PtyProcessExitStatus::from_status(status))
                }
                Ok(None) => PtyProcessExit::Running,
                Err(_) => {
                    let _ = child.take();
                    PtyProcessExit::Exited(PtyProcessExitStatus::unwaitable())
                }
            },
            None => PtyProcessExit::Exited(PtyProcessExitStatus::unwaitable()),
        }
    }

    pub(crate) fn wait_for_reader_done(&self, timeout: Duration) {
        let reader_done = lock(&self.reader_done).take();
        if let Some(reader_done) = reader_done {
            let _ = reader_done.recv_timeout(timeout);
        }
    }

    #[must_use]
    pub(crate) fn take_runner_result(&self, timeout: Duration) -> Option<CommandRunnerResult> {
        let runner_result_done = lock(&self.runner_result_done).take();
        let bytes = runner_result_done?.recv_timeout(timeout).ok()?.ok()?;
        CommandRunnerResult::from_bytes(&bytes)
    }
}

impl PendingPtyProcess {
    pub(crate) fn allow_start(self) -> io::Result<PtyProcess> {
        let Self {
            process,
            start_ack,
            request_payload,
        } = self;
        let mut start_ack = File::from(start_ack);
        if let Err(error) = start_ack.write_all(b"1") {
            process.terminate();
            return Err(error);
        }
        if let Err(error) = request_payload.write() {
            process.terminate();
            return Err(error);
        }
        Ok(process)
    }
}

impl RequestPayload {
    fn write(self) -> io::Result<()> {
        let mut writer = File::from(self.writer);
        writer.write_all(&self.bytes)
    }
}

pub(crate) fn spawn_current_exe_ns_runner(
    runner_request: &NamespaceRunnerRequest,
    transcript_path: PathBuf,
) -> Result<PendingPtyProcess, CommandError> {
    let request_bytes = encode_runner_request(runner_request)?;
    let (request_read, request_write) = runner_request_pipe()?;
    let request_fd = request_read.as_raw_fd();
    let (result_read, result_write) = runner_result_pipe()?;
    let result_fd = result_write.as_raw_fd();
    let (master, slave) = open_pty_pair()?;
    let (start_ack_read, start_ack_write) = start_ack_pipe()?;
    let start_ack_fd = start_ack_read.as_raw_fd();
    // Non-blocking master OFD (shared by the writer dup and the reader): writes
    // can't wedge on a non-draining consumer, and the reader polls instead.
    set_nonblocking(&master)?;
    let mut child_command = Command::new(std::env::current_exe()?);
    child_command
        .arg("ns-runner")
        .arg("--request-fd")
        .arg(request_fd.to_string())
        .arg("--result-fd")
        .arg(result_fd.to_string())
        .arg("--start-ack-fd")
        .arg(start_ack_fd.to_string())
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave))
        .process_group(0);
    let child = child_command.spawn()?;
    drop(request_read);
    drop(result_write);
    drop(start_ack_read);
    let pgid = i32::try_from(child.id()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("child pid does not fit i32: {}", child.id()),
        )
    })?;
    let writer = master.try_clone()?;
    let transcript_prefixer = TranscriptTimestampPrefixer::new();
    let reader_done = spawn_command_output_reader(master, transcript_path, transcript_prefixer);
    let runner_result_done = spawn_runner_result_reader(File::from(result_read));

    Ok(PendingPtyProcess {
        process: PtyProcess {
            pgid: Some(pgid),
            writer: Mutex::new(writer),
            reader_done: Mutex::new(Some(reader_done)),
            runner_result_done: Mutex::new(Some(runner_result_done)),
            child: Mutex::new(Some(child)),
        },
        start_ack: start_ack_write,
        request_payload: RequestPayload {
            writer: request_write,
            bytes: request_bytes,
        },
    })
}

fn runner_request_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let (read, write) = pipe()?;
    fcntl_setfd(&read, FdFlags::empty())?;
    fcntl_setfd(&write, FdFlags::CLOEXEC)?;
    Ok((read, write))
}

fn runner_result_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let (read, write) = pipe()?;
    fcntl_setfd(&read, FdFlags::CLOEXEC)?;
    fcntl_setfd(&write, FdFlags::empty())?;
    Ok((read, write))
}

fn start_ack_pipe() -> io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    let (read, write) = pipe()?;
    fcntl_setfd(&read, FdFlags::empty())?;
    fcntl_setfd(&write, FdFlags::CLOEXEC)?;
    Ok((read, write))
}

fn spawn_command_output_reader(
    mut master: File,
    transcript_path: PathBuf,
    mut transcript_prefixer: TranscriptTimestampPrefixer,
) -> mpsc::Receiver<()> {
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut transcript = OpenOptions::new()
            .create(true)
            .append(true)
            .open(transcript_path)
            .ok();
        let mut buf = [0_u8; 8192];
        loop {
            // The master is non-blocking; block here until readable or hangup with
            // no busy-loop and no added latency (infinite poll wakes on the first
            // byte or on slave close), then drain what is available.
            {
                let mut fds = [PollFd::new(&master, PollFlags::IN)];
                match poll(&mut fds, -1) {
                    Ok(_) => {}
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(_) => break,
                }
            }
            match master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let transcript_bytes = transcript_prefixer.prefix(&buf[..n]);
                    if let Some(file) = transcript.as_mut() {
                        if file.write_all(&transcript_bytes).is_err() {
                            transcript = None;
                        }
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = done_tx.send(());
    });
    done_rx
}

fn spawn_runner_result_reader(mut reader: File) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = reader.read_to_end(&mut bytes).map(|_| bytes);
        let _ = done_tx.send(result);
    });
    done_rx
}

fn encode_runner_request(request: &NamespaceRunnerRequest) -> Result<Vec<u8>, CommandError> {
    serde_json::to_vec(request).map_err(|error| {
        CommandError::InvalidRequest(format!("serialize namespace runner request: {error}"))
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn open_pty_pair() -> io::Result<(File, File)> {
    let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY;
    #[cfg(target_os = "linux")]
    let flags = flags | OpenptFlags::CLOEXEC;
    let master = openpt(flags).map_err(io::Error::from)?;
    grantpt(&master).map_err(io::Error::from)?;
    unlockpt(&master).map_err(io::Error::from)?;

    #[cfg(target_os = "linux")]
    let slave = File::from(ioctl_tiocgptpeer(&master, flags).map_err(io::Error::from)?);
    #[cfg(not(target_os = "linux"))]
    let slave = {
        let slave_name = ptsname(&master, Vec::new()).map_err(io::Error::from)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(slave_name.to_string_lossy().as_ref())?
    };

    Ok((File::from(master), slave))
}

fn terminate_process_group(pgid: i32) {
    if killpg(Pid::from_raw(pgid), Signal::SIGTERM).is_ok() {
        thread::sleep(Duration::from_millis(50));
        let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
    }
}

/// Mark `file`'s open file description non-blocking so `read`/`write` return
/// `WouldBlock` instead of stalling. Applied to the PTY master before it is
/// shared between the writer dup and the reader thread.
fn set_nonblocking(file: &File) -> io::Result<()> {
    let flags = fcntl_getfl(file)?;
    fcntl_setfl(file, flags | OFlags::NONBLOCK)?;
    Ok(())
}

/// Milliseconds left until `deadline`, clamped to a non-negative `i32` for
/// `poll`. Returns 0 once the deadline has passed.
fn poll_timeout_ms(deadline: Instant) -> i32 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX)
}

fn stdin_backpressure() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "stdin_backpressure: command is not draining its stdin",
    )
}
