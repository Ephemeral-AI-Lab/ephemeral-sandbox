use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, killpg, Signal};
use nix::unistd::Pid;
use rustix::event::{poll, PollFd, PollFlags};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
#[cfg(target_os = "linux")]
use rustix::pty::ioctl_tiocgptpeer;
#[cfg(not(target_os = "linux"))]
use rustix::pty::ptsname;
use rustix::pty::{grantpt, openpt, unlockpt, OpenptFlags};
use time::OffsetDateTime;

#[derive(Clone)]
enum TranscriptSink {
    Memory(Arc<AtomicU64>),
    File(PathBuf),
}

type OutputSink = Box<dyn FnMut(&[u8]) + Send + 'static>;

struct OutputReader {
    master: Arc<File>,
    sink: OutputSink,
    drain: OutputDrain,
    activity: Arc<OutputActivity>,
}

#[derive(Default)]
struct OutputQueue {
    readers: Vec<OutputReader>,
}

struct OutputReactor {
    queue: Arc<Mutex<OutputQueue>>,
    wake_writer: Arc<UnixStream>,
    active_readers: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct OutputDrain {
    complete: Arc<(Mutex<bool>, Condvar)>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutputActivitySnapshot {
    generation: u64,
    output_bytes: u64,
    closed: bool,
}

#[derive(Default)]
pub struct OutputActivity {
    state: Mutex<OutputActivitySnapshot>,
    ready: Condvar,
}

impl OutputActivitySnapshot {
    #[must_use]
    pub fn output_bytes(self) -> u64 {
        self.output_bytes
    }

    #[must_use]
    pub fn is_closed(self) -> bool {
        self.closed
    }
}

impl OutputActivity {
    #[must_use]
    pub fn snapshot(&self) -> OutputActivitySnapshot {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn wait_for_change(
        &self,
        observed: OutputActivitySnapshot,
        timeout: Duration,
    ) -> OutputActivitySnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.generation != observed.generation || state.closed {
            return *state;
        }
        let (state, _) = self
            .ready
            .wait_timeout_while(state, timeout, |state| {
                state.generation == observed.generation && !state.closed
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state
    }

    fn record_output(&self, bytes: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.generation = state.generation.saturating_add(1);
        state.output_bytes = state
            .output_bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        self.ready.notify_all();
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        self.ready.notify_all();
    }
}

impl OutputDrain {
    fn pending() -> Self {
        Self {
            complete: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    fn complete(&self) {
        let (state, ready) = &*self.complete;
        let mut complete = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *complete = true;
        ready.notify_all();
    }

    fn wait_timeout(&self, timeout: Duration) -> bool {
        let (state, ready) = &*self.complete;
        let complete = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *complete {
            return true;
        }
        let (complete, _) = ready
            .wait_timeout_while(complete, timeout, |complete| !*complete)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *complete
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputReactorSnapshot {
    pub(crate) worker_threads: usize,
    pub(crate) active_readers: usize,
}

static OUTPUT_REACTOR: OnceLock<OutputReactor> = OnceLock::new();

#[derive(Clone)]
pub struct PtyMaster {
    pgid: Option<i32>,
    writer: Arc<Mutex<Option<File>>>,
    sink: TranscriptSink,
    drain: OutputDrain,
    activity: Arc<OutputActivity>,
    cancel: Arc<dyn Fn() + Send + Sync>,
    stdin_write_deadline: Duration,
}

impl PtyMaster {
    pub fn spawn(
        master: File,
        pgid: Option<i32>,
        transcript_path: Option<PathBuf>,
        cancel: Box<dyn Fn() + Send + Sync>,
        stdin_write_deadline: Duration,
    ) -> io::Result<Self> {
        set_nonblocking(&master)?;
        let writer = master.try_clone()?;
        let activity = Arc::new(OutputActivity::default());
        let (sink, drain) = match transcript_path {
            Some(path) => {
                let drain = spawn_file_output_reader(master, &path, Arc::clone(&activity));
                (TranscriptSink::File(path), drain)
            }
            None => {
                let len = Arc::new(AtomicU64::new(0));
                let reader_len = Arc::clone(&len);
                let drain = spawn_output_reader(
                    master,
                    move |bytes| {
                        reader_len.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    },
                    Arc::clone(&activity),
                );
                (TranscriptSink::Memory(len), drain)
            }
        };
        Ok(Self {
            pgid,
            writer: Arc::new(Mutex::new(Some(writer))),
            sink,
            drain,
            activity,
            cancel: Arc::from(cancel),
            stdin_write_deadline,
        })
    }

    pub fn pgid(&self) -> Option<i32> {
        self.pgid
    }

    pub fn cancel_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        Arc::clone(&self.cancel)
    }

    pub fn cancel(&self) {
        (self.cancel)();
    }

    pub fn write_stdin(&self, bytes: &[u8]) -> io::Result<()> {
        let mut writer = self.writer.lock().expect("pty writer mutex poisoned");
        let writer = writer
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "pty stdin closed"))?;
        let deadline = Instant::now() + self.stdin_write_deadline;
        let mut offset = 0;
        while offset < bytes.len() {
            match writer.write(&bytes[offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(io::ErrorKind::WriteZero, "pty stdin closed"));
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

    pub(crate) fn terminal_release(&self) -> impl FnOnce() -> bool + Send + 'static {
        let writer = Arc::clone(&self.writer);
        let drain = self.drain.clone();
        let activity = Arc::clone(&self.activity);
        let deadline = self.stdin_write_deadline;
        move || {
            writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            let drained = drain.wait_timeout(deadline);
            activity.close();
            drained
        }
    }

    #[must_use]
    pub fn output_activity(&self) -> Arc<OutputActivity> {
        Arc::clone(&self.activity)
    }

    pub fn output_len(&self) -> u64 {
        match &self.sink {
            TranscriptSink::Memory(len) => len.load(Ordering::Relaxed),
            TranscriptSink::File(path) => {
                std::fs::metadata(path).map_or(0, |metadata| metadata.len())
            }
        }
    }
}

fn spawn_file_output_reader(
    master: File,
    transcript_path: &Path,
    activity: Arc<OutputActivity>,
) -> OutputDrain {
    let mut transcript = OpenOptions::new()
        .create(true)
        .append(true)
        .open(transcript_path)
        .ok();
    let mut prefixer = TranscriptTimestampPrefixer::new();
    spawn_output_reader(
        master,
        move |bytes| {
            let prefixed = prefixer.prefix(bytes);
            if transcript
                .as_mut()
                .is_some_and(|file| file.write_all(&prefixed).is_err())
            {
                transcript = None;
            }
        },
        activity,
    )
}

fn spawn_output_reader(
    master: File,
    sink: impl FnMut(&[u8]) + Send + 'static,
    activity: Arc<OutputActivity>,
) -> OutputDrain {
    let drain = OutputDrain::pending();
    output_reactor().register(master, Box::new(sink), drain.clone(), activity);
    drain
}

pub(crate) fn output_reactor_snapshot() -> OutputReactorSnapshot {
    match OUTPUT_REACTOR.get() {
        Some(reactor) => OutputReactorSnapshot {
            worker_threads: 1,
            active_readers: reactor.active_readers.load(Ordering::Acquire),
        },
        None => OutputReactorSnapshot {
            worker_threads: 0,
            active_readers: 0,
        },
    }
}

pub(crate) fn initialize_output_reactor() {
    let _ = output_reactor();
}

fn output_reactor() -> &'static OutputReactor {
    OUTPUT_REACTOR.get_or_init(OutputReactor::new)
}

impl OutputReactor {
    fn new() -> Self {
        let queue = Arc::new(Mutex::new(OutputQueue::default()));
        let (wake_reader, wake_writer) =
            UnixStream::pair().expect("create PTY output reactor wake socket");
        wake_reader
            .set_nonblocking(true)
            .expect("make PTY output reactor wake reader nonblocking");
        wake_writer
            .set_nonblocking(true)
            .expect("make PTY output reactor wake writer nonblocking");
        let active_readers = Arc::new(AtomicUsize::new(0));
        let worker_queue = Arc::clone(&queue);
        let worker_active = Arc::clone(&active_readers);
        thread::Builder::new()
            .name("eos-pty-reactor".to_owned())
            .spawn(move || run_output_reactor(&worker_queue, &worker_active, &wake_reader))
            .expect("spawn PTY output reactor");
        Self {
            queue,
            wake_writer: Arc::new(wake_writer),
            active_readers,
        }
    }

    fn register(
        &self,
        master: File,
        sink: OutputSink,
        drain: OutputDrain,
        activity: Arc<OutputActivity>,
    ) {
        self.active_readers.fetch_add(1, Ordering::Release);
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.readers.push(OutputReader {
            master: Arc::new(master),
            sink,
            drain,
            activity,
        });
        drop(queue);
        self.wake();
    }

    fn wake(&self) {
        let mut writer = &*self.wake_writer;
        loop {
            match writer.write(&[1]) {
                Ok(_) => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(_) => return,
            }
        }
    }
}

fn run_output_reactor(
    queue: &Mutex<OutputQueue>,
    active_readers: &AtomicUsize,
    wake_reader: &UnixStream,
) {
    loop {
        let readers: Vec<Arc<File>> = queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .readers
            .iter()
            .map(|reader| Arc::clone(&reader.master))
            .collect();
        let mut poll_fds = Vec::with_capacity(readers.len() + 1);
        poll_fds.push(PollFd::new(wake_reader, PollFlags::IN));
        poll_fds.extend(
            readers
                .iter()
                .map(|reader| PollFd::new(&**reader, PollFlags::IN)),
        );
        match poll(&mut poll_fds, -1) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
        }

        let wake_ready = !poll_fds[0].revents().is_empty();
        let ready_readers: HashSet<_> = readers
            .iter()
            .zip(poll_fds.iter().skip(1))
            .filter_map(|(reader, poll_fd)| {
                (!poll_fd.revents().is_empty()).then(|| reader.as_raw_fd())
            })
            .collect();
        drop(poll_fds);
        if wake_ready {
            drain_wake_reader(wake_reader);
        }
        if ready_readers.is_empty() {
            continue;
        }

        let mut queue = queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut index = 0;
        while index < queue.readers.len() {
            if !ready_readers.contains(&queue.readers[index].master.as_raw_fd()) {
                index += 1;
                continue;
            }
            if drain_output_reader(&mut queue.readers[index]) {
                index += 1;
            } else {
                let reader = queue.readers.swap_remove(index);
                reader.drain.complete();
                active_readers.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}

fn drain_wake_reader(wake_reader: &UnixStream) {
    let mut reader = wake_reader;
    let mut buf = [0_u8; 64];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(_) => return,
        }
    }
}

fn drain_output_reader(reader: &mut OutputReader) -> bool {
    let mut buf = [0_u8; 8192];
    let mut master = &*reader.master;
    loop {
        match master.read(&mut buf) {
            Ok(0) => return false,
            Ok(n) => {
                (reader.sink)(&buf[..n]);
                reader.activity.record_output(n);
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return true,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

pub fn open_pty_pair() -> io::Result<(File, File)> {
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

pub(crate) fn terminate_pgid(pgid: i32) {
    signal_pgid_and_pid(pgid, Signal::SIGTERM);
    thread::sleep(Duration::from_millis(100));
    signal_pgid_and_pid(pgid, Signal::SIGKILL);
}

fn signal_pgid_and_pid(pgid: i32, signal: Signal) {
    let pid = Pid::from_raw(pgid);
    let _ = killpg(pid, signal);
    let _ = kill(pid, signal);
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    let flags = fcntl_getfl(file)?;
    fcntl_setfl(file, flags | OFlags::NONBLOCK)?;
    Ok(())
}

fn poll_timeout_ms(deadline: Instant) -> i32 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX)
}

fn stdin_backpressure() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "stdin_backpressure: consumer is not draining its stdin",
    )
}

pub(crate) struct TranscriptTimestampPrefixer {
    at_line_start: bool,
}

impl TranscriptTimestampPrefixer {
    pub(crate) const fn new() -> Self {
        Self {
            at_line_start: true,
        }
    }

    pub(crate) fn prefix(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.prefix_at(bytes, OffsetDateTime::now_utc())
    }

    fn prefix_at(&mut self, bytes: &[u8], now: OffsetDateTime) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for byte in bytes {
            if self.at_line_start {
                out.extend_from_slice(format_timestamp_prefix_at(now).as_bytes());
                self.at_line_start = false;
            }
            out.push(*byte);
            if *byte == b'\n' {
                self.at_line_start = true;
            }
        }
        out
    }
}

fn format_timestamp_prefix_at(now: OffsetDateTime) -> String {
    format!(
        "[{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millisecond:03}Z] ",
        year = now.year(),
        month = now.month() as u8,
        day = now.day(),
        hour = now.hour(),
        minute = now.minute(),
        second = now.second(),
        millisecond = now.millisecond(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn output_reactor_drains_output_that_arrives_after_registration() {
        let reactor = OutputReactor::new();
        let (master, mut slave) = open_pty_pair().expect("open test PTY");
        set_nonblocking(&master).expect("make test PTY nonblocking");
        let (output_tx, output_rx) = mpsc::channel();
        let drain = OutputDrain::pending();
        let terminal_drain = drain.clone();
        let activity = Arc::new(OutputActivity::default());
        let observed = activity.snapshot();
        reactor.register(
            master,
            Box::new(move |bytes| {
                let _ = output_tx.send(bytes.to_vec());
            }),
            drain,
            Arc::clone(&activity),
        );

        thread::sleep(Duration::from_millis(20));
        slave
            .write_all(b"ready\n")
            .expect("write delayed PTY output");

        let output = output_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("readiness reactor did not drain delayed PTY output");
        assert_ne!(
            activity.wait_for_change(observed, Duration::from_millis(100)),
            observed,
            "output activity was not published after sink delivery"
        );
        assert!(
            output.windows(b"ready".len()).any(|part| part == b"ready"),
            "unexpected PTY output: {output:?}"
        );

        drop(slave);
        assert!(
            terminal_drain.wait_timeout(Duration::from_millis(100)),
            "readiness reactor did not observe PTY EOF"
        );
    }
}
