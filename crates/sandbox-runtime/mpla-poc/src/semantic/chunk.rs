use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc::sync_channel, OnceLock};

#[cfg(any(target_os = "linux", target_os = "android"))]
use rustix::fs::Advice;
use rustix::fs::SeekFrom;
use sha2::{Digest, Sha256};

use crate::config::{MAX_DATA_WORKERS, SEMANTIC_SCAN_TRANSFER_BYTES, SEMANTIC_SCAN_WINDOW_BYTES};
use crate::{PocError, PocResult};

use super::allocation::is_fully_allocated;
use super::record::{ExtentKind, SemanticRecord};
use super::spool::SpoolSink;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkScan {
    pub bytes_read: u64,
    pub content_sha256: [u8; 32],
    pub data_workers: u16,
}

#[derive(Clone, Copy)]
struct ChunkDigest {
    offset: u64,
    length: u32,
    sha256: [u8; 32],
}

struct ChunkBatch {
    start: u64,
    end: u64,
    bytes_read: u64,
    chunks: Vec<ChunkDigest>,
}

static ZERO_WINDOW_SHA256: OnceLock<[u8; 32]> = OnceLock::new();

pub(super) fn scan_regular(
    path: &Path,
    normalized_path: &[u8],
    logical_size: u64,
    records: &mut impl SpoolSink,
) -> PocResult<ChunkScan> {
    let file = File::open(path)
        .map_err(|error| PocError::io("open semantic regular file", path, error))?;
    let mut content = regular_content_hasher(logical_size);
    if logical_size > 0 && is_fully_allocated(&file, path, logical_size)? {
        emit_extent(
            records,
            &mut content,
            normalized_path,
            0,
            logical_size,
            ExtentKind::Data,
        )?;
        let bytes_read =
            read_data_extent(&file, path, 0, logical_size, |offset, length, sha256| {
                emit_chunk(
                    records,
                    &mut content,
                    normalized_path,
                    offset,
                    length,
                    sha256,
                )
            })?;
        advise_dont_need(&file, logical_size);
        return Ok(ChunkScan {
            bytes_read,
            content_sha256: content.finalize().into(),
            data_workers: 1,
        });
    }
    let mut cursor = 0_u64;
    let mut bytes_read = 0_u64;
    while cursor < logical_size {
        let data_start = seek_data_start(&file, path, cursor, logical_size)?;
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
        let data_end = seek_data_end(&file, path, data_start, logical_size)?;
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
            data_start,
            data_end,
            |offset, length, sha256| {
                emit_chunk(
                    records,
                    &mut content,
                    normalized_path,
                    offset,
                    length,
                    sha256,
                )
            },
        )?);
        cursor = data_end;
    }
    advise_dont_need(&file, logical_size);
    Ok(ChunkScan {
        bytes_read,
        content_sha256: content.finalize().into(),
        data_workers: 1,
    })
}

pub(super) fn scan_regular_selected_parallel(
    path: &Path,
    normalized_path: &[u8],
    logical_size: u64,
    records: &mut impl SpoolSink,
) -> PocResult<ChunkScan> {
    let file = File::open(path)
        .map_err(|error| PocError::io("open selected semantic regular file", path, error))?;
    let mut content = regular_content_hasher(logical_size);
    if logical_size > 0 && is_fully_allocated(&file, path, logical_size)? {
        emit_extent(
            records,
            &mut content,
            normalized_path,
            0,
            logical_size,
            ExtentKind::Data,
        )?;
        let (bytes_read, data_workers) = read_data_extent_parallel(
            &file,
            path,
            normalized_path,
            0,
            logical_size,
            records,
            &mut content,
        )?;
        advise_dont_need(&file, logical_size);
        return Ok(ChunkScan {
            bytes_read,
            content_sha256: content.finalize().into(),
            data_workers,
        });
    }
    let mut cursor = 0_u64;
    let mut bytes_read = 0_u64;
    let mut data_workers = 1_u16;
    while cursor < logical_size {
        let data_start = seek_data_start(&file, path, cursor, logical_size)?;
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
        let data_end = seek_data_end(&file, path, data_start, logical_size)?;
        emit_extent(
            records,
            &mut content,
            normalized_path,
            data_start,
            data_end - data_start,
            ExtentKind::Data,
        )?;
        let (read, workers) = read_data_extent_parallel(
            &file,
            path,
            normalized_path,
            data_start,
            data_end,
            records,
            &mut content,
        )?;
        bytes_read = bytes_read.saturating_add(read);
        data_workers = data_workers.max(workers);
        cursor = data_end;
    }
    advise_dont_need(&file, logical_size);
    Ok(ChunkScan {
        bytes_read,
        content_sha256: content.finalize().into(),
        data_workers,
    })
}

fn regular_content_hasher(logical_size: u64) -> Sha256 {
    let mut content = Sha256::new();
    content.update(b"mpla-poc-semantic-v1/regular-content\0");
    content.update(logical_size.to_be_bytes());
    content
}

fn seek_data_start(file: &File, path: &Path, cursor: u64, logical_size: u64) -> PocResult<u64> {
    let seek_cursor = i64::try_from(cursor)
        .map_err(|_| PocError::Integrity("regular file offset exceeds i64".to_owned()))?;
    match rustix::fs::seek(file, SeekFrom::Data(seek_cursor)) {
        Ok(value) => Ok(value.min(logical_size)),
        Err(error) if error == rustix::io::Errno::NXIO => Ok(logical_size),
        Err(error)
            if cursor == 0
                && (error == rustix::io::Errno::INVAL || error == rustix::io::Errno::NOTSUP) =>
        {
            Err(PocError::Unsupported(format!(
                "filesystem does not expose SEEK_DATA/SEEK_HOLE for {}",
                path.display()
            )))
        }
        Err(error) => Err(PocError::io(
            "seek semantic data extent",
            path,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )),
    }
}

fn seek_data_end(file: &File, path: &Path, data_start: u64, logical_size: u64) -> PocResult<u64> {
    let seek_data_start = i64::try_from(data_start)
        .map_err(|_| PocError::Integrity("regular file offset exceeds i64".to_owned()))?;
    let data_end = rustix::fs::seek(file, SeekFrom::Hole(seek_data_start))
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
    Ok(data_end)
}

fn emit_extent(
    records: &mut impl SpoolSink,
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

fn emit_chunk(
    records: &mut impl SpoolSink,
    content: &mut Sha256,
    path: &[u8],
    offset: u64,
    length: u32,
    sha256: [u8; 32],
) -> PocResult<()> {
    records.push_record(SemanticRecord::Chunk {
        path: path.to_vec(),
        offset,
        length,
        sha256,
    })?;
    content.update(b"chunk\0");
    content.update(offset.to_be_bytes());
    content.update(u64::from(length).to_be_bytes());
    content.update(sha256);
    Ok(())
}

fn read_data_extent(
    file: &File,
    physical_path: &Path,
    start: u64,
    end: u64,
    mut emit: impl FnMut(u64, u32, [u8; 32]) -> PocResult<()>,
) -> PocResult<u64> {
    let mut buffer = vec![0_u8; SEMANTIC_SCAN_TRANSFER_BYTES];
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
        let mut chunk_offset = 0_usize;
        while chunk_offset < wanted {
            let chunk_length = (wanted - chunk_offset).min(SEMANTIC_SCAN_WINDOW_BYTES);
            let chunk_end = chunk_offset
                .checked_add(chunk_length)
                .ok_or_else(|| PocError::Integrity("semantic chunk end overflow".to_owned()))?;
            let record_offset = offset
                .checked_add(u64::try_from(chunk_offset).unwrap_or(u64::MAX))
                .ok_or_else(|| PocError::Integrity("semantic chunk offset overflow".to_owned()))?;
            emit(
                record_offset,
                u32::try_from(chunk_length)
                    .map_err(|_| PocError::Integrity("semantic chunk exceeds u32".to_owned()))?,
                semantic_chunk_sha256(&buffer[chunk_offset..chunk_end]),
            )?;
            chunk_offset = chunk_end;
        }
        offset = offset
            .checked_add(u64::try_from(wanted).unwrap_or(u64::MAX))
            .ok_or_else(|| PocError::Integrity("semantic chunk offset overflow".to_owned()))?;
    }
    Ok(end - start)
}

fn semantic_chunk_sha256(bytes: &[u8]) -> [u8; 32] {
    if bytes.len() == SEMANTIC_SCAN_WINDOW_BYTES && bytes.iter().all(|byte| *byte == 0) {
        return *ZERO_WINDOW_SHA256.get_or_init(|| hash_semantic_chunk(bytes));
    }
    hash_semantic_chunk(bytes)
}

fn hash_semantic_chunk(bytes: &[u8]) -> [u8; 32] {
    let mut chunk = Sha256::new();
    chunk.update(b"mpla-poc-semantic-v1/chunk-bytes\0");
    chunk.update(bytes);
    chunk.finalize().into()
}

fn read_data_extent_parallel(
    file: &File,
    physical_path: &Path,
    normalized_path: &[u8],
    start: u64,
    end: u64,
    records: &mut impl SpoolSink,
    content: &mut Sha256,
) -> PocResult<(u64, u16)> {
    let transfer = u64::try_from(SEMANTIC_SCAN_TRANSFER_BYTES)
        .map_err(|_| PocError::Integrity("semantic transfer size overflow".to_owned()))?;
    let spans = (end - start).div_ceil(transfer);
    let worker_count = usize::try_from(spans)
        .unwrap_or(usize::from(MAX_DATA_WORKERS))
        .min(usize::from(MAX_DATA_WORKERS))
        .max(1);
    if worker_count == 1 {
        let bytes_read =
            read_data_extent(file, physical_path, start, end, |offset, length, sha256| {
                emit_chunk(records, content, normalized_path, offset, length, sha256)
            })?;
        return Ok((bytes_read, 1));
    }

    let mut worker_files = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        worker_files.push(file.try_clone().map_err(|error| {
            PocError::io("duplicate semantic regular file", physical_path, error)
        })?);
    }
    let (sender, receiver) = sync_channel(worker_count.saturating_mul(2));
    let next_offset = AtomicU64::new(start);
    let cancelled = AtomicBool::new(false);
    let mut first_error = None;
    let mut pending = BTreeMap::new();
    let mut expected_offset = start;
    let mut bytes_read = 0_u64;

    std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(worker_count);
        for worker_file in worker_files {
            let sender = sender.clone();
            let next_offset = &next_offset;
            let cancelled = &cancelled;
            workers.push(scope.spawn(move || {
                while !cancelled.load(Ordering::Acquire) {
                    let offset = match next_offset.fetch_update(
                        Ordering::AcqRel,
                        Ordering::Acquire,
                        |current| {
                            if current >= end {
                                None
                            } else {
                                Some(current.saturating_add(transfer).min(end))
                            }
                        },
                    ) {
                        Ok(offset) => offset,
                        Err(_) => return,
                    };
                    let job_end = offset.saturating_add(transfer).min(end);
                    let result = read_chunk_batch(&worker_file, physical_path, offset, job_end);
                    if result.is_err() {
                        cancelled.store(true, Ordering::Release);
                    }
                    if sender.send(result).is_err() {
                        return;
                    }
                    if cancelled.load(Ordering::Acquire) {
                        return;
                    }
                }
            }));
        }
        drop(sender);
        while let Ok(result) = receiver.recv() {
            match result {
                Ok(batch) if first_error.is_none() => {
                    let start = batch.start;
                    if pending.insert(start, batch).is_some() {
                        first_error = Some(PocError::Integrity(
                            "parallel semantic scan produced duplicate transfer offsets".to_owned(),
                        ));
                        cancelled.store(true, Ordering::Release);
                        continue;
                    }
                    while let Some(batch) = pending.remove(&expected_offset) {
                        let batch_end = batch.end;
                        for chunk in batch.chunks {
                            if let Err(error) = emit_chunk(
                                records,
                                content,
                                normalized_path,
                                chunk.offset,
                                chunk.length,
                                chunk.sha256,
                            ) {
                                first_error = Some(error);
                                cancelled.store(true, Ordering::Release);
                                break;
                            }
                        }
                        if first_error.is_some() {
                            break;
                        }
                        bytes_read = bytes_read.saturating_add(batch.bytes_read);
                        expected_offset = batch_end;
                    }
                }
                Ok(_) => {}
                Err(error) if first_error.is_none() => {
                    first_error = Some(error);
                    cancelled.store(true, Ordering::Release);
                }
                Err(_) => {}
            }
        }
        for worker in workers {
            if worker.join().is_err() && first_error.is_none() {
                first_error = Some(PocError::Integrity(
                    "parallel semantic scan worker panicked".to_owned(),
                ));
            }
        }
    });
    if let Some(error) = first_error {
        return Err(error);
    }
    if expected_offset != end || !pending.is_empty() {
        return Err(PocError::Integrity(
            "parallel semantic scan did not complete its data extent".to_owned(),
        ));
    }
    Ok((
        bytes_read,
        u16::try_from(worker_count)
            .map_err(|_| PocError::Integrity("semantic worker count overflow".to_owned()))?,
    ))
}

fn read_chunk_batch(
    file: &File,
    physical_path: &Path,
    start: u64,
    end: u64,
) -> PocResult<ChunkBatch> {
    let mut chunks = Vec::with_capacity(SEMANTIC_SCAN_TRANSFER_BYTES / SEMANTIC_SCAN_WINDOW_BYTES);
    let bytes_read =
        read_data_extent(file, physical_path, start, end, |offset, length, sha256| {
            chunks.push(ChunkDigest {
                offset,
                length,
                sha256,
            });
            Ok(())
        })?;
    Ok(ChunkBatch {
        start,
        end,
        bytes_read,
        chunks,
    })
}

fn advise_dont_need(_file: &File, _logical_size: u64) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let _ = rustix::fs::fadvise(_file, 0, _logical_size, Advice::DontNeed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_window_fast_path_matches_full_domain_separated_hash() {
        let zeroes = vec![0_u8; SEMANTIC_SCAN_WINDOW_BYTES];
        assert_eq!(semantic_chunk_sha256(&zeroes), hash_semantic_chunk(&zeroes));
    }

    #[test]
    fn nonzero_and_partial_windows_remain_fully_hashed() {
        let mut nonzero = vec![0_u8; SEMANTIC_SCAN_WINDOW_BYTES];
        nonzero[SEMANTIC_SCAN_WINDOW_BYTES / 2] = 1;
        let partial = &nonzero[..SEMANTIC_SCAN_WINDOW_BYTES - 1];
        assert_eq!(
            semantic_chunk_sha256(&nonzero),
            hash_semantic_chunk(&nonzero)
        );
        assert_eq!(semantic_chunk_sha256(partial), hash_semantic_chunk(partial));
    }
}
