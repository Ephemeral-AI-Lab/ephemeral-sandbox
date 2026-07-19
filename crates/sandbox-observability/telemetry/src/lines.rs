use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

#[derive(Debug, Default)]
pub(crate) struct LineScan {
    pub(crate) complete_bytes: u64,
    pub(crate) skipped_oversized: u64,
    pub(crate) partial_tail: bool,
}

/// Stream complete newline-terminated lines with a fixed-capacity line buffer.
/// Oversized lines and the partial final line are consumed but never exposed.
pub(crate) fn for_each_complete_line(
    path: &Path,
    max_line_bytes: usize,
    mut visit: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<LineScan> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(LineScan::default()),
        Err(error) => return Err(error),
    };
    let payload_cap = max_line_bytes.saturating_sub(1);
    let mut line = Vec::with_capacity(payload_cap);
    let mut input = [0_u8; 8 * 1024];
    let mut oversized = false;
    let mut scan = LineScan::default();
    loop {
        let count = file.read(&mut input)?;
        if count == 0 {
            break;
        }
        for byte in &input[..count] {
            if *byte == b'\n' {
                scan.complete_bytes = scan.complete_bytes.saturating_add((line.len() + 1) as u64);
                if oversized {
                    scan.skipped_oversized = scan.skipped_oversized.saturating_add(1);
                } else {
                    visit(&line)?;
                }
                line.clear();
                oversized = false;
            } else if !oversized {
                if line.len() < payload_cap {
                    line.push(*byte);
                } else {
                    line.clear();
                    oversized = true;
                }
            }
        }
    }
    scan.partial_tail = oversized || !line.is_empty();
    Ok(scan)
}
