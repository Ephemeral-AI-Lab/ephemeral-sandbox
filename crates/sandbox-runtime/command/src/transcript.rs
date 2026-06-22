use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use time::OffsetDateTime;

pub(crate) const MAX_TRANSCRIPT_READ_BYTES: u64 = 1024 * 1024;
pub(crate) const TRANSCRIPT_TRUNCATED_NOTICE: &str =
    "[eos: transcript truncated to last 1048576 bytes]\n";

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

    pub(crate) fn prefix_at(&mut self, bytes: &[u8], now: OffsetDateTime) -> Vec<u8> {
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

pub(crate) fn format_timestamp_prefix_at(now: OffsetDateTime) -> String {
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

pub(crate) fn read_full_transcript_stdout(path: &Path) -> String {
    read_full_transcript_bytes(path).unwrap_or_default()
}

pub(crate) fn read_transcript_since(path: &Path, offset: u64) -> String {
    read_transcript_bytes(path, offset).unwrap_or_default()
}

fn read_transcript_bytes(path: &Path, offset: u64) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let requested_start = offset.min(len);
    let bounded_start = requested_start.max(len.saturating_sub(MAX_TRANSCRIPT_READ_BYTES));
    file.seek(SeekFrom::Start(bounded_start)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_TRANSCRIPT_READ_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let mut stdout = String::new();
    if bounded_start > requested_start {
        stdout.push_str(TRANSCRIPT_TRUNCATED_NOTICE);
    }
    stdout.push_str(&String::from_utf8_lossy(&bytes));
    Some(stdout)
}

fn read_full_transcript_bytes(path: &Path) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path).ok()?.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}
