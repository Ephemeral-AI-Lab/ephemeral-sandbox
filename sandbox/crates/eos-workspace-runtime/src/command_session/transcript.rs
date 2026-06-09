use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use time::{OffsetDateTime, UtcOffset};

use crate::command_session::output::tail_lines;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TranscriptTimestampTimezone {
    offset: UtcOffset,
}

impl TranscriptTimestampTimezone {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("UTC") || value == "Z" {
            return Ok(Self {
                offset: UtcOffset::UTC,
            });
        }
        parse_fixed_offset(value)
            .map(|offset| Self { offset })
            .ok_or_else(|| "timezone must be UTC, Z, or a fixed offset like +08:00".to_owned())
    }

    fn format_prefix_at(self, now: OffsetDateTime) -> String {
        let now = now.to_offset(self.offset);
        format!(
            "[{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millisecond:03}{offset}] ",
            year = now.year(),
            month = now.month() as u8,
            day = now.day(),
            hour = now.hour(),
            minute = now.minute(),
            second = now.second(),
            millisecond = now.millisecond(),
            offset = offset_suffix(self.offset),
        )
    }
}

pub(crate) struct TranscriptTimestampPrefixer {
    timezone: TranscriptTimestampTimezone,
    at_line_start: bool,
}

impl TranscriptTimestampPrefixer {
    pub(crate) fn new(timezone: &str) -> Result<Self, String> {
        Ok(Self {
            timezone: TranscriptTimestampTimezone::parse(timezone)?,
            at_line_start: true,
        })
    }

    pub(crate) fn prefix(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.prefix_at(bytes, OffsetDateTime::now_utc())
    }

    fn prefix_at(&mut self, bytes: &[u8], now: OffsetDateTime) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for byte in bytes {
            if self.at_line_start {
                out.extend_from_slice(self.timezone.format_prefix_at(now).as_bytes());
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

pub(crate) fn read_transcript_stdout(path: &Path) -> String {
    read_transcript_bytes(path, 0).unwrap_or_default()
}

pub(crate) fn read_transcript_since(path: &Path, offset: u64) -> String {
    read_transcript_bytes(path, offset).unwrap_or_default()
}

pub(crate) fn read_transcript_tail(path: &Path, last_n_lines: usize) -> String {
    tail_lines(&read_transcript_stdout(path), last_n_lines)
}

fn read_transcript_bytes(path: &Path, offset: u64) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_fixed_offset(value: &str) -> Option<UtcOffset> {
    let bytes = value.as_bytes();
    if bytes.len() != 6 || !matches!(bytes[0], b'+' | b'-') || bytes[3] != b':' {
        return None;
    }
    let hour = value[1..3].parse::<i32>().ok()?;
    let minute = value[4..6].parse::<i32>().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    let sign = if bytes[0] == b'-' { -1 } else { 1 };
    UtcOffset::from_whole_seconds(sign * ((hour * 60 * 60) + (minute * 60))).ok()
}

fn offset_suffix(offset: UtcOffset) -> String {
    let seconds = offset.whole_seconds();
    if seconds == 0 {
        return "Z".to_owned();
    }
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    let hours = abs / 3600;
    let minutes = (abs % 3600) / 60;
    format!("{sign}{hours:02}:{minutes:02}")
}

#[cfg(test)]
#[path = "../../tests/command_session/transcript_unit.rs"]
mod tests;
