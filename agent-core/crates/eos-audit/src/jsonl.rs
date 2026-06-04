//! Append-only `JSONL` sinks.
//!
//! [`JsonlSink`] opens, appends one canonical `JSON` line, and closes on every
//! `publish` (append-mode write is the atomicity story, mirroring Python's
//! `os.open(..., O_APPEND)`); it suits tests and low-volume paths.
//! [`BufferedJsonlSink`] is the production sink: `publish` hands the event to a
//! bounded channel and a dedicated writer thread owns the open file handle, so
//! disk IO never runs on a Tokio worker. A full queue yields
//! [`AuditError::Backpressure`] rather than blocking. The
//! [`BufferedAuditShutdown`] guard flushes and joins the writer thread.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::thread::JoinHandle;

use crate::error::AuditError;
use crate::event::AuditEvent;
use crate::sink::AuditSink;

/// Serialize one event to its normalized `JSONL` row.
fn event_line(event: &AuditEvent) -> Result<String, AuditError> {
    Ok(eos_obs_contract::to_jsonl_line(&event.to_obs_envelope())?)
}

/// Open `path` for appending, creating parent directories as needed.
fn open_append(path: &Path) -> Result<File, AuditError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

/// Open-append-close `JSONL` sink. One canonical `JSON` object per line; never
/// truncates or rewrites.
#[derive(Debug, Clone)]
pub struct JsonlSink {
    path: PathBuf,
}

impl JsonlSink {
    /// Build a sink that appends to `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl AuditSink for JsonlSink {
    fn publish(&self, event: &AuditEvent) -> Result<(), AuditError> {
        let line = event_line(event)?;
        let mut file = open_append(&self.path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }
}

/// Control message to the buffered writer thread.
enum WriterMsg {
    /// One event to append.
    Event(Box<AuditEvent>),
    /// Flush and stop the writer.
    Shutdown,
}

/// Production file-backed sink: `publish` enqueues onto a bounded channel and a
/// dedicated thread writes. A full queue returns [`AuditError::Backpressure`].
#[derive(Debug, Clone)]
pub struct BufferedJsonlSink {
    tx: SyncSender<WriterMsg>,
}

impl BufferedJsonlSink {
    /// Build a buffered sink appending to `path` with a bounded `capacity`
    /// queue. Returns the sink and a [`BufferedAuditShutdown`] guard the
    /// composition root retains to flush and join the writer on shutdown.
    ///
    /// # Errors
    /// Returns [`AuditError`] if the file (or its parent directories) cannot be
    /// opened for appending.
    pub fn new(
        path: impl Into<PathBuf>,
        capacity: usize,
    ) -> Result<(Self, BufferedAuditShutdown), AuditError> {
        let file = open_append(&path.into())?;
        let (tx, rx) = sync_channel::<WriterMsg>(capacity);
        let handle = std::thread::spawn(move || writer_loop(&rx, file));
        Ok((
            Self { tx: tx.clone() },
            BufferedAuditShutdown {
                ctrl_tx: Some(tx),
                handle: Some(handle),
            },
        ))
    }
}

impl AuditSink for BufferedJsonlSink {
    fn publish(&self, event: &AuditEvent) -> Result<(), AuditError> {
        match self.tx.try_send(WriterMsg::Event(Box::new(event.clone()))) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(AuditError::Backpressure),
            // The writer thread is gone; treat as backpressure so the bus
            // records it rather than blocking or panicking.
            Err(TrySendError::Disconnected(_)) => Err(AuditError::Backpressure),
        }
    }
}

/// Drain the channel to the append-mode file until [`WriterMsg::Shutdown`] or a
/// disconnect, flushing before returning. Events are written in FIFO order, so
/// a `Shutdown` sent after the last event drains everything queued before it.
fn writer_loop(rx: &Receiver<WriterMsg>, file: File) {
    let mut writer = BufWriter::new(file);
    while let Ok(msg) = rx.recv() {
        match msg {
            WriterMsg::Event(event) => {
                if let Ok(line) = event_line(&event) {
                    // A write error here cannot be surfaced to the original
                    // caller (the event was accepted); drop it rather than
                    // panic on the writer thread.
                    let _ = writer.write_all(line.as_bytes());
                }
            }
            WriterMsg::Shutdown => break,
        }
    }
    let _ = writer.flush();
}

/// Shutdown guard for a [`BufferedJsonlSink`]'s writer thread.
///
/// Retained by the composition root (`eos-runtime`). On [`shutdown`] or `Drop`
/// it sends [`WriterMsg::Shutdown`] and joins the writer thread, flushing all
/// accepted events. This keeps the close path off the [`AuditSink`] trait.
///
/// [`shutdown`]: BufferedAuditShutdown::shutdown
#[derive(Debug)]
pub struct BufferedAuditShutdown {
    ctrl_tx: Option<SyncSender<WriterMsg>>,
    handle: Option<JoinHandle<()>>,
}

impl BufferedAuditShutdown {
    /// Flush and join the writer thread.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(tx) = self.ctrl_tx.take() {
            // A blocking send is fine: the writer only exits after it receives
            // this message, so it keeps draining and frees queue space.
            let _ = tx.send(WriterMsg::Shutdown);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for BufferedAuditShutdown {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // unwrap is permitted in tests (err-no-unwrap-prod)
    use super::*;
    use crate::event::AuditSource;
    use crate::node::AuditNode;
    use eos_types::{JsonObject, TestClock, UtcDateTime};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// RAII temp directory removed on drop (`test-fixture-raii`).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let mut path = std::env::temp_dir();
            let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            path.push(format!("eos-audit-test-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn event(event_type: &str) -> AuditEvent {
        let clock = TestClock::new(UtcDateTime::parse_rfc3339("2026-06-02T19:47:00Z").unwrap());
        AuditEvent::new(
            AuditSource::Engine,
            event_type,
            AuditNode::default(),
            JsonObject::new(),
            &clock,
        )
    }

    // AC-audit-09: JsonlSink appends untruncated lines, creates parent dirs, and
    // preserves prior content across writes.
    #[test]
    fn append_only_untruncated() {
        let dir = TempDir::new();
        // A not-yet-existing nested path exercises parent-dir creation.
        let path = dir.path.join("nested/audit.jsonl");
        let sink = JsonlSink::new(&path);

        sink.publish(&event("engine.tool.started")).unwrap();
        sink.publish(&event("engine.tool.completed")).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("engine.tool.started"));
        assert!(lines[1].contains("engine.tool.completed"));
        // Each line is a complete normalized JSON object.
        for line in lines {
            let row = eos_obs_contract::from_jsonl_line(line).unwrap();
            assert_eq!(row.source, eos_obs_contract::ObsSource::AgentCore);
        }
    }

    // AC-audit-10: BufferedJsonlSink + shutdown guard flushes all accepted events
    // before join.
    #[test]
    fn buffered_shutdown_flushes() {
        let dir = TempDir::new();
        let path = dir.path.join("buffered.jsonl");
        let (sink, shutdown) = BufferedJsonlSink::new(&path, 64).unwrap();

        const N: usize = 20;
        for _ in 0..N {
            sink.publish(&event("engine.tool.completed")).unwrap();
        }
        shutdown.shutdown();

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), N);
        for line in lines {
            let row = eos_obs_contract::from_jsonl_line(line).unwrap();
            assert_eq!(row.event_type, "engine.tool.completed");
        }
    }
}
