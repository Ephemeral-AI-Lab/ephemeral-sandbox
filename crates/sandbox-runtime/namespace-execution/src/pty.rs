use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
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
    master: File,
    sink: OutputSink,
    drain: OutputDrain,
}

#[derive(Default)]
struct OutputQueue {
    readers: Vec<OutputReader>,
}

struct OutputReactor {
    queue: Arc<(Mutex<OutputQueue>, Condvar)>,
    active_readers: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct OutputDrain {
    complete: Arc<(Mutex<bool>, Condvar)>,
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
        let (sink, drain) = match transcript_path {
            Some(path) => {
                let drain = spawn_file_output_reader(master, &path);
                (TranscriptSink::File(path), drain)
            }
            None => {
                let len = Arc::new(AtomicU64::new(0));
                let reader_len = Arc::clone(&len);
                let drain = spawn_output_reader(master, move |bytes| {
                    reader_len.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                });
                (TranscriptSink::Memory(len), drain)
            }
        };
        Ok(Self {
            pgid,
            writer: Arc::new(Mutex::new(Some(writer))),
            sink,
            drain,
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
        let deadline = self.stdin_write_deadline;
        move || {
            writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            drain.wait_timeout(deadline)
        }
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

fn spawn_file_output_reader(master: File, transcript_path: &Path) -> OutputDrain {
    let mut transcript = OpenOptions::new()
        .create(true)
        .append(true)
        .open(transcript_path)
        .ok();
    let mut prefixer = TranscriptTimestampPrefixer::new();
    spawn_output_reader(master, move |bytes| {
        let prefixed = prefixer.prefix(bytes);
        if transcript
            .as_mut()
            .is_some_and(|file| file.write_all(&prefixed).is_err())
        {
            transcript = None;
        }
    })
}

fn spawn_output_reader(master: File, sink: impl FnMut(&[u8]) + Send + 'static) -> OutputDrain {
    let drain = OutputDrain::pending();
    output_reactor().register(master, Box::new(sink), drain.clone());
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
        let queue = Arc::new((Mutex::new(OutputQueue::default()), Condvar::new()));
        let active_readers = Arc::new(AtomicUsize::new(0));
        let worker_queue = Arc::clone(&queue);
        let worker_active = Arc::clone(&active_readers);
        thread::Builder::new()
            .name("eos-pty-reactor".to_owned())
            .spawn(move || run_output_reactor(&worker_queue, &worker_active))
            .expect("spawn PTY output reactor");
        Self {
            queue,
            active_readers,
        }
    }

    fn register(&self, master: File, sink: OutputSink, drain: OutputDrain) {
        self.active_readers.fetch_add(1, Ordering::Release);
        let (queue, ready) = &*self.queue;
        let mut queue = queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.readers.push(OutputReader {
            master,
            sink,
            drain,
        });
        ready.notify_one();
    }
}

fn run_output_reactor(shared: &(Mutex<OutputQueue>, Condvar), active_readers: &AtomicUsize) {
    let (queue, ready) = shared;
    let mut queue = queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        while queue.readers.is_empty() {
            queue = ready
                .wait(queue)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }

        let mut index = 0;
        while index < queue.readers.len() {
            if drain_output_reader(&mut queue.readers[index]) {
                index += 1;
            } else {
                let reader = queue.readers.swap_remove(index);
                reader.drain.complete();
                active_readers.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let (next, _) = ready
            .wait_timeout(queue, Duration::from_millis(5))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue = next;
    }
}

fn drain_output_reader(reader: &mut OutputReader) -> bool {
    let mut buf = [0_u8; 8192];
    loop {
        match reader.master.read(&mut buf) {
            Ok(0) => return false,
            Ok(n) => (reader.sink)(&buf[..n]),
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
