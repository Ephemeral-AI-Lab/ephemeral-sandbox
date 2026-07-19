//! Strictly bounded two-segment NDJSON storage.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rustix::fs::{flock, FlockOperation};
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;

use crate::lines::for_each_complete_line;
use crate::record::{Record, MAX_LINE_BYTES, TRUNCATED_KEY};

/// Default hard cap across the active and rotated segments.
pub const DEFAULT_MAX_DISK_BYTES: u64 = 4 * 1024 * 1024;
/// Maximum accepted hard cap across both segments.
pub const MAX_DISK_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SinkStats {
    pub dropped_storage: u64,
    pub dropped_oversized: u64,
    pub truncated_records: u64,
}

#[derive(Default)]
struct Counters {
    dropped_storage: AtomicU64,
    dropped_oversized: AtomicU64,
    truncated_records: AtomicU64,
}

/// A sink whose active and rotated files share one hard byte budget.
pub struct Sink {
    path: PathBuf,
    rotated: PathBuf,
    lock_path: PathBuf,
    max_line_bytes: usize,
    max_disk_bytes: u64,
    counters: Arc<Counters>,
}

impl Sink {
    #[must_use]
    pub fn new(path: PathBuf, max_line_bytes: usize) -> Self {
        Self::with_budget(path, max_line_bytes, DEFAULT_MAX_DISK_BYTES)
    }

    #[must_use]
    pub fn with_budget(path: PathBuf, max_line_bytes: usize, max_disk_bytes: u64) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self {
            rotated: suffixed(&path, ".1"),
            lock_path: suffixed(&path, ".lock"),
            path,
            max_line_bytes: max_line_bytes.min(MAX_LINE_BYTES),
            max_disk_bytes: max_disk_bytes.min(MAX_DISK_BYTES),
            counters: Arc::new(Counters::default()),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn max_line_bytes(&self) -> usize {
        self.max_line_bytes
    }

    #[must_use]
    pub fn stats(&self) -> SinkStats {
        SinkStats {
            dropped_storage: self.counters.dropped_storage.load(Ordering::Relaxed),
            dropped_oversized: self.counters.dropped_oversized.load(Ordering::Relaxed),
            truncated_records: self.counters.truncated_records.load(Ordering::Relaxed),
        }
    }

    /// Serialize into one fixed stack buffer, then perform one append write
    /// while holding the per-store cross-process lock. Storage errors are
    /// counted once and returned; observer call sites deliberately swallow them.
    pub fn append(&self, record: &Record) -> io::Result<()> {
        self.append_with_policy(record, false)
    }

    /// Append only when the original record fits the fixed line and segment
    /// bounds. Unlike [`Self::append`], this never replaces an oversized
    /// resource sample with a truncation marker.
    pub fn append_strict(&self, record: &Record) -> io::Result<()> {
        self.append_with_policy(record, true)
    }

    fn append_with_policy(&self, record: &Record, strict: bool) -> io::Result<()> {
        match self.append_inner(record, strict) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.counters
                    .dropped_storage
                    .fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    fn append_inner(&self, record: &Record, strict: bool) -> io::Result<()> {
        if self.max_line_bytes == 0 || self.max_disk_bytes < 2 {
            self.counters
                .dropped_oversized
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        let mut encoded = EncodedLine::new(self.max_line_bytes);
        if strict {
            encoded.serialize_strict(record)?;
        } else {
            encoded.serialize(record)?;
        }
        let segment_cap = self.max_disk_bytes / 2;
        if !strict && encoded.len() as u64 > segment_cap && !encoded.is_truncated() {
            encoded.serialize_marker(record, encoded.original_len())?;
        }
        if encoded.len() == 0
            || encoded.len() as u64 > segment_cap
            || encoded.len() > self.max_line_bytes
        {
            self.counters
                .dropped_oversized
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        if encoded.is_truncated() {
            self.counters
                .truncated_records
                .fetch_add(1, Ordering::Relaxed);
        }

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock_path)?;
        flock(&lock, FlockOperation::LockExclusive)?;
        let result = self.append_locked(encoded.as_bytes(), segment_cap);
        let _ = flock(&lock, FlockOperation::Unlock);
        result
    }

    fn append_locked(&self, line: &[u8], segment_cap: u64) -> io::Result<()> {
        let skipped_oversized =
            compact_segment(&self.rotated, segment_cap, self.max_line_bytes)?.saturating_add(
                compact_segment(&self.path, segment_cap, self.max_line_bytes)?,
            );
        self.counters
            .dropped_oversized
            .fetch_add(skipped_oversized, Ordering::Relaxed);

        let active_len = file_len(&self.path)?;
        if active_len.saturating_add(line.len() as u64) > segment_cap {
            match fs::remove_file(&self.rotated) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match fs::rename(&self.path, &self.rotated) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }

        let mut active = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        active.write_all(line)
    }
}

fn compact_segment(path: &Path, cap: u64, max_line_bytes: usize) -> io::Result<u64> {
    let input_len = file_len(path)?;
    if input_len == 0 {
        return Ok(0);
    }
    if input_len <= cap {
        let mut input = File::open(path)?;
        input.seek(SeekFrom::End(-1))?;
        let mut tail = [0_u8; 1];
        input.read_exact(&mut tail)?;
        if tail == [b'\n'] {
            return Ok(0);
        }
    }

    let mut accepted = 0_u64;
    let scan = for_each_complete_line(path, max_line_bytes, |line| {
        accepted = accepted.saturating_add((line.len() + 1) as u64);
        Ok(())
    })?;
    if input_len <= cap && scan.complete_bytes == input_len && scan.skipped_oversized == 0 {
        return Ok(0);
    }

    let skip = accepted.saturating_sub(cap);
    let temporary = suffixed(path, &format!(".compact.{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let mut skipped = 0_u64;
    for_each_complete_line(path, max_line_bytes, |line| {
        let bytes = (line.len() + 1) as u64;
        if skipped < skip {
            skipped = skipped.saturating_add(bytes);
            return Ok(());
        }
        output.write_all(line)?;
        output.write_all(b"\n")
    })?;
    output.sync_data()?;
    drop(output);
    fs::rename(&temporary, path)?;
    Ok(scan.skipped_oversized)
}

fn file_len(path: &Path) -> io::Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

struct EncodedLine {
    bytes: [u8; MAX_LINE_BYTES],
    len: usize,
    total: usize,
    limit: usize,
    truncated: bool,
}

impl EncodedLine {
    fn new(limit: usize) -> Self {
        Self {
            bytes: [0; MAX_LINE_BYTES],
            len: 0,
            total: 0,
            limit,
            truncated: false,
        }
    }

    fn serialize(&mut self, record: &Record) -> io::Result<()> {
        self.reset(false);
        serde_json::to_writer(&mut *self, record).map_err(io::Error::other)?;
        let original_len = self.total;
        if original_len.saturating_add(1) > self.limit {
            self.serialize_marker(record, original_len)?;
        } else {
            self.push_newline();
        }
        Ok(())
    }

    fn serialize_strict(&mut self, record: &Record) -> io::Result<()> {
        self.reset(false);
        serde_json::to_writer(&mut *self, record).map_err(io::Error::other)?;
        if self.total.saturating_add(1) > self.limit {
            self.len = 0;
        } else {
            self.push_newline();
        }
        Ok(())
    }

    fn serialize_marker(&mut self, record: &Record, original_len: usize) -> io::Result<()> {
        self.reset(true);
        serde_json::to_writer(&mut *self, &TruncatedRecord(record, original_len))
            .map_err(io::Error::other)?;
        if self.total.saturating_add(1) > self.limit {
            self.len = 0;
            return Ok(());
        }
        self.push_newline();
        Ok(())
    }

    fn reset(&mut self, truncated: bool) {
        self.len = 0;
        self.total = 0;
        self.truncated = truncated;
    }

    fn push_newline(&mut self) {
        if self.len < self.bytes.len() {
            self.bytes[self.len] = b'\n';
            self.len += 1;
        }
        self.total = self.total.saturating_add(1);
    }

    fn len(&self) -> usize {
        self.len
    }

    fn original_len(&self) -> usize {
        self.total.saturating_sub(usize::from(self.len > 0))
    }

    fn is_truncated(&self) -> bool {
        self.truncated
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl Write for EncodedLine {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.total = self.total.saturating_add(input.len());
        let usable = self.limit.saturating_sub(1).min(self.bytes.len());
        let copy = input.len().min(usable.saturating_sub(self.len));
        self.bytes[self.len..self.len + copy].copy_from_slice(&input[..copy]);
        self.len += copy;
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct TruncatedRecord<'a>(&'a Record, usize);

impl Serialize for TruncatedRecord<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0 {
            Record::Span(span) => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("kind", "span")?;
                map.serialize_entry("ts", &span.ts)?;
                map.serialize_entry("trace", &span.trace)?;
                map.serialize_entry("span", &span.span)?;
                if let Some(parent) = &span.parent {
                    map.serialize_entry("parent", parent)?;
                }
                map.serialize_entry("name", &span.name)?;
                map.serialize_entry("dur_ms", &span.dur_ms)?;
                map.serialize_entry("status", &span.status)?;
                map.serialize_entry("attrs", &Marker(self.1))?;
                map.end()
            }
            Record::Event(event) => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("kind", "event")?;
                map.serialize_entry("ts", &event.ts)?;
                map.serialize_entry("trace", &event.trace)?;
                if let Some(parent) = &event.parent {
                    map.serialize_entry("parent", parent)?;
                }
                map.serialize_entry("name", &event.name)?;
                map.serialize_entry("attrs", &Marker(self.1))?;
                map.end()
            }
            Record::Sample(sample) => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("kind", "sample")?;
                map.serialize_entry("ts", &sample.ts)?;
                map.serialize_entry("scope", &sample.scope)?;
                map.serialize_entry(TRUNCATED_KEY, &self.1)?;
                map.end()
            }
        }
    }
}

struct Marker(usize);

impl Serialize for Marker {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(TRUNCATED_KEY, &self.0)?;
        map.end()
    }
}
