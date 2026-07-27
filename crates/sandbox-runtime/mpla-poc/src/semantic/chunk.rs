use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

use rustix::fs::SeekFrom;
use sha2::{Digest, Sha256};

use crate::config::SEMANTIC_SCAN_WINDOW_BYTES;
use crate::{PocError, PocResult};

use super::record::{ExtentKind, SemanticRecord};
use super::spool::BoundedSpool;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkScan {
    pub bytes_read: u64,
    pub content_sha256: [u8; 32],
}

pub fn scan_regular(
    path: &Path,
    normalized_path: &[u8],
    logical_size: u64,
    records: &mut BoundedSpool,
) -> PocResult<ChunkScan> {
    let file = File::open(path)
        .map_err(|error| PocError::io("open semantic regular file", path, error))?;
    let mut content = Sha256::new();
    content.update(b"mpla-poc-semantic-v1/regular-content\0");
    content.update(logical_size.to_be_bytes());
    let mut cursor = 0_u64;
    let mut bytes_read = 0_u64;
    while cursor < logical_size {
        let seek_cursor = i64::try_from(cursor)
            .map_err(|_| PocError::Integrity("regular file offset exceeds i64".to_owned()))?;
        let data_start = match rustix::fs::seek(&file, SeekFrom::Data(seek_cursor)) {
            Ok(value) => value.min(logical_size),
            Err(error) if error == rustix::io::Errno::NXIO => logical_size,
            Err(error)
                if cursor == 0
                    && (error == rustix::io::Errno::INVAL
                        || error == rustix::io::Errno::NOTSUP) =>
            {
                return Err(PocError::Unsupported(format!(
                    "filesystem does not expose SEEK_DATA/SEEK_HOLE for {}",
                    path.display()
                )));
            }
            Err(error) => {
                return Err(PocError::io(
                    "seek semantic data extent",
                    path,
                    std::io::Error::from_raw_os_error(error.raw_os_error()),
                ));
            }
        };
        if data_start > cursor {
            emit_extent(
                records,
                &mut content,
                normalized_path,
                cursor,
                data_start - cursor,
                ExtentKind::Hole,
            )?;
        }
        if data_start == logical_size {
            break;
        }
        let seek_data_start = i64::try_from(data_start)
            .map_err(|_| PocError::Integrity("regular file offset exceeds i64".to_owned()))?;
        let data_end = rustix::fs::seek(&file, SeekFrom::Hole(seek_data_start))
            .map_err(|error| {
                PocError::io(
                    "seek semantic hole extent",
                    path,
                    std::io::Error::from_raw_os_error(error.raw_os_error()),
                )
            })?
            .min(logical_size);
        if data_end <= data_start {
            return Err(PocError::Integrity(
                "filesystem returned a non-progressing sparse extent".to_owned(),
            ));
        }
        emit_extent(
            records,
            &mut content,
            normalized_path,
            data_start,
            data_end - data_start,
            ExtentKind::Data,
        )?;
        bytes_read = bytes_read.saturating_add(read_data_extent(
            &file,
            path,
            normalized_path,
            data_start,
            data_end,
            records,
            &mut content,
        )?);
        cursor = data_end;
    }
    Ok(ChunkScan {
        bytes_read,
        content_sha256: content.finalize().into(),
    })
}

fn emit_extent(
    records: &mut BoundedSpool,
    content: &mut Sha256,
    path: &[u8],
    offset: u64,
    length: u64,
    kind: ExtentKind,
) -> PocResult<()> {
    records.push_record(SemanticRecord::Extent {
        path: path.to_vec(),
        offset,
        length,
        kind,
    })?;
    content.update(b"extent\0");
    content.update([kind as u8]);
    content.update(offset.to_be_bytes());
    content.update(length.to_be_bytes());
    Ok(())
}

fn read_data_extent(
    file: &File,
    physical_path: &Path,
    normalized_path: &[u8],
    start: u64,
    end: u64,
    records: &mut BoundedSpool,
    content: &mut Sha256,
) -> PocResult<u64> {
    let mut buffer = vec![0_u8; SEMANTIC_SCAN_WINDOW_BYTES];
    let mut offset = start;
    while offset < end {
        let wanted = usize::try_from((end - offset).min(buffer.len() as u64))
            .map_err(|_| PocError::Integrity("semantic chunk length overflow".to_owned()))?;
        let mut filled = 0;
        while filled < wanted {
            let position = offset
                .checked_add(u64::try_from(filled).unwrap_or(u64::MAX))
                .ok_or_else(|| PocError::Integrity("semantic read offset overflow".to_owned()))?;
            let count = file
                .read_at(&mut buffer[filled..wanted], position)
                .map_err(|error| {
                    PocError::io("read semantic regular extent", physical_path, error)
                })?;
            if count == 0 {
                return Err(PocError::Integrity(
                    "regular file changed while semantic scan was reading it".to_owned(),
                ));
            }
            filled += count;
        }
        let mut chunk = Sha256::new();
        chunk.update(b"mpla-poc-semantic-v1/chunk-bytes\0");
        chunk.update(&buffer[..wanted]);
        let chunk_sha256 = chunk.finalize().into();
        records.push_record(SemanticRecord::Chunk {
            path: normalized_path.to_vec(),
            offset,
            length: u32::try_from(wanted)
                .map_err(|_| PocError::Integrity("semantic chunk exceeds u32".to_owned()))?,
            sha256: chunk_sha256,
        })?;
        content.update(b"chunk\0");
        content.update(offset.to_be_bytes());
        content.update(u64::try_from(wanted).unwrap_or(u64::MAX).to_be_bytes());
        content.update(chunk_sha256);
        offset = offset
            .checked_add(u64::try_from(wanted).unwrap_or(u64::MAX))
            .ok_or_else(|| PocError::Integrity("semantic chunk offset overflow".to_owned()))?;
    }
    Ok(end - start)
}
