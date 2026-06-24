use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use rustix::event::{poll, PollFd, PollFlags};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
#[cfg(target_os = "linux")]
use rustix::pty::ioctl_tiocgptpeer;
#[cfg(not(target_os = "linux"))]
use rustix::pty::ptsname;
use rustix::pty::{grantpt, openpt, unlockpt, OpenptFlags};

use crate::transcript::TranscriptTimestampPrefixer;

const STDIN_WRITE_DEADLINE: Duration = Duration::from_secs(2);

enum TranscriptSink {
    Memory(Arc<Mutex<Vec<u8>>>),
    File(PathBuf),
}

pub struct PtyMaster {
    pgid: Option<i32>,
    writer: Mutex<File>,
    sink: TranscriptSink,
    cancel: Arc<dyn Fn() + Send + Sync>,
}

impl PtyMaster {
    pub fn spawn(
        master: File,
        pgid: Option<i32>,
        transcript_path: Option<PathBuf>,
        cancel: Box<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        set_nonblocking(&master)?;
        let writer = master.try_clone()?;
        let sink = match transcript_path {
            Some(path) => {
                spawn_file_output_reader(master, &path);
                TranscriptSink::File(path)
            }
            None => {
                let transcript = Arc::new(Mutex::new(Vec::new()));
                let reader_transcript = Arc::clone(&transcript);
                spawn_output_reader(master, move |bytes| {
                    reader_transcript
                        .lock()
                        .expect("pty transcript mutex poisoned")
                        .extend_from_slice(bytes);
                });
                TranscriptSink::Memory(transcript)
            }
        };
        Ok(Self {
            pgid,
            writer: Mutex::new(writer),
            sink,
            cancel: Arc::from(cancel),
        })
    }

    pub fn pgid(&self) -> Option<i32> {
        self.pgid
    }

    pub fn cancel_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        Arc::clone(&self.cancel)
    }

    pub fn write_stdin(&self, bytes: &[u8]) -> io::Result<()> {
        let mut writer = self.writer.lock().expect("pty writer mutex poisoned");
        let deadline = Instant::now() + STDIN_WRITE_DEADLINE;
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

    pub fn output_len(&self) -> u64 {
        match &self.sink {
            TranscriptSink::Memory(transcript) => {
                let transcript = transcript.lock().expect("pty transcript mutex poisoned");
                u64::try_from(transcript.len()).unwrap_or(u64::MAX)
            }
            TranscriptSink::File(path) => {
                std::fs::metadata(path).map_or(0, |metadata| metadata.len())
            }
        }
    }

    pub fn cancel(&self) {
        (self.cancel)();
    }
}

fn spawn_file_output_reader(master: File, transcript_path: &Path) {
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
    });
}

fn spawn_output_reader(mut master: File, mut sink: impl FnMut(&[u8]) + Send + 'static) {
    thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        while poll_readable(&master) {
            match master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => sink(&buf[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
}

fn poll_readable(master: &File) -> bool {
    loop {
        let mut fds = [PollFd::new(master, PollFlags::IN)];
        match poll(&mut fds, -1) {
            Ok(_) => return true,
            Err(rustix::io::Errno::INTR) => continue,
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

pub(crate) fn terminate_process_group(pgid: i32) {
    if killpg(Pid::from_raw(pgid), Signal::SIGTERM).is_ok() {
        thread::sleep(Duration::from_millis(50));
        let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
    }
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
