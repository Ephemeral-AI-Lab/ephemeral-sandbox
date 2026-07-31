use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;

use rustix::fs::SeekFrom;
use sandbox_runtime_mpla_poc::config::{MAX_DATA_WORKERS, SEMANTIC_SCAN_TRANSFER_BYTES};
use sandbox_runtime_mpla_poc::semantic::allocation::is_fully_allocated;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::oracle_record::{
    calculate_roots, validate_path, ExtentKind, NodeKind, NodeRecord, OracleResult, OracleSummary,
    Record, MAX_KEY_BYTES, MAX_PATH_BYTES, MAX_RECORD_BYTES, MAX_XATTR_BYTES, SCAN_WINDOW_BYTES,
};

const SORT_MEMORY_BYTES: usize = 2 * 1024 * 1024;
const MAX_XATTR_LIST_BYTES: usize = 1024 * 1024;
const MAX_XATTR_TRANSIENT_BYTES: usize = 2 * MAX_XATTR_LIST_BYTES + MAX_XATTR_BYTES;
const RUN_MAGIC: &[u8; 8] = b"MPLAORU1";
const QUEUE_MAGIC: &[u8; 8] = b"MPLAOQU1";
const OPAQUE_XATTRS: [&[u8]; 2] = [b"trusted.overlay.opaque", b"user.overlay.opaque"];
const OVERLAY_INTERNAL_XATTRS: [&[u8]; 2] = [b"trusted.overlay.uuid", b"user.overlay.uuid"];

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

pub fn scan(
    tree: &Path,
    record_stream: &Path,
    actor_id: &str,
    semantic_operation_id: &str,
) -> OracleResult<OracleSummary> {
    if !std::fs::metadata(tree)
        .map_err(|error| format!("stat oracle tree {}: {error}", tree.display()))?
        .is_dir()
    {
        return Err("oracle input tree is not a directory".to_owned());
    }
    let parent = record_stream
        .parent()
        .ok_or_else(|| "oracle record stream has no parent directory".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create oracle output parent {}: {error}", parent.display()))?;
    let canonical_tree = std::fs::canonicalize(tree)
        .map_err(|error| format!("canonicalize oracle tree {}: {error}", tree.display()))?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        format!(
            "canonicalize oracle output parent {}: {error}",
            parent.display()
        )
    })?;
    if canonical_parent.starts_with(&canonical_tree) {
        return Err("oracle output and work paths must be outside the semantic tree".to_owned());
    }
    if record_stream.exists() {
        return Err("oracle record stream output already exists".to_owned());
    }
    let work = parent.join(format!(".mpla-oracle-{}", Uuid::new_v4()));
    std::fs::create_dir(&work)
        .map_err(|error| format!("create oracle work directory {}: {error}", work.display()))?;
    let result = scan_inner(tree, record_stream, actor_id, semantic_operation_id, &work);
    if result.is_ok() {
        std::fs::remove_dir_all(&work)
            .map_err(|error| format!("remove oracle work directory {}: {error}", work.display()))?;
    }
    result
}

fn scan_inner(
    tree: &Path,
    record_stream: &Path,
    actor_id: &str,
    semantic_operation_id: &str,
    work: &Path,
) -> OracleResult<OracleSummary> {
    let mut records = DiskSorter::new(work.join("records"), SORT_MEMORY_BYTES)?;
    let mut hardlinks = DiskSorter::new(work.join("hardlinks"), SORT_MEMORY_BYTES)?;
    let (_filesystem_entry_count, bytes_read) = traverse(tree, &mut records, &mut hardlinks, work)?;
    let hardlinks = hardlinks.finish()?;
    let hardlink_stats = hardlinks.stats;
    append_hardlink_records(&mut records, hardlinks, work)?;
    let sorted = records.finish()?;
    let record_stats = sorted.stats;
    materialize(&sorted, record_stream)?;
    let (root_id, attribution_root_id, record_stream_sha256, record_count) =
        calculate_roots(record_stream, actor_id, semantic_operation_id)?;
    let root_record_debug = find_root_record_debug(record_stream)?;
    Ok(OracleSummary {
        semantic_format: "mpla-poc-semantic-v1".to_owned(),
        root_id,
        attribution_root_id,
        record_stream_sha256,
        record_stream_path: record_stream.display().to_string(),
        root_record_debug,
        entry_count: record_count,
        record_count,
        bytes_read,
        spool_runs: record_stats
            .initial_runs
            .saturating_add(hardlink_stats.initial_runs),
        spool_bytes: record_stats
            .bytes_written
            .saturating_add(hardlink_stats.bytes_written),
        peak_open_data_fds: 6,
        peak_managed_bytes: u64::try_from(
            2 * SORT_MEMORY_BYTES
                + usize::from(MAX_DATA_WORKERS) * SEMANTIC_SCAN_TRANSFER_BYTES
                + MAX_XATTR_TRANSIENT_BYTES,
        )
        .unwrap_or(u64::MAX),
    })
}

fn find_root_record_debug(record_stream: &Path) -> OracleResult<String> {
    let mut reader = BufReader::new(
        File::open(record_stream)
            .map_err(|error| format!("open oracle record stream for diagnosis: {error}"))?,
    );
    loop {
        let mut length = [0_u8; 4];
        let mut offset = 0;
        while offset < length.len() {
            let read = reader
                .read(&mut length[offset..])
                .map_err(|error| format!("read oracle record length for diagnosis: {error}"))?;
            if read == 0 {
                return if offset == 0 {
                    Err("oracle stream omitted root node record".to_owned())
                } else {
                    Err("oracle stream ended within a record length".to_owned())
                };
            }
            offset += read;
        }
        let length = usize::try_from(u32::from_be_bytes(length))
            .map_err(|_| "oracle diagnostic record length overflow".to_owned())?;
        if length == 0 || length > MAX_RECORD_BYTES {
            return Err("oracle diagnostic record exceeds fixed bound".to_owned());
        }
        let mut bytes = vec![0_u8; length];
        reader
            .read_exact(&mut bytes)
            .map_err(|error| format!("read oracle record for diagnosis: {error}"))?;
        let record = Record::decode(&bytes)?;
        if matches!(&record, Record::Node(node) if node.path.is_empty()) {
            return Ok(format!("{record:?}"));
        }
    }
}

fn traverse(
    root: &Path,
    records: &mut DiskSorter,
    hardlinks: &mut DiskSorter,
    work: &Path,
) -> OracleResult<(u64, u64)> {
    let mut queue = DirectoryQueue::create(work.join("directories.queue"))?;
    queue.push(&[])?;
    let mut entry_count = 0_u64;
    let mut bytes_read = 0_u64;
    while let Some(relative) = queue.pop()? {
        let physical = physical_path(root, &relative);
        let metadata = std::fs::symlink_metadata(&physical)
            .map_err(|error| format!("lstat oracle directory {}: {error}", physical.display()))?;
        if !metadata.is_dir() {
            return Err("oracle directory queue contains non-directory".to_owned());
        }
        bytes_read = bytes_read.saturating_add(scan_node(
            &physical, &relative, &metadata, records, hardlinks,
        )?);
        entry_count = entry_count.saturating_add(1);
        let opaque_from_xattr = directory_is_opaque(&physical)?;
        let entries = std::fs::read_dir(&physical)
            .map_err(|error| format!("read oracle directory {}: {error}", physical.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!("iterate oracle directory {}: {error}", physical.display())
            })?;
            let name = entry.file_name().into_vec();
            if name == b".wh..wh..opq" {
                if !opaque_from_xattr {
                    push_record(
                        records,
                        Record::OpaqueDirectory {
                            path: relative.clone(),
                        },
                    )?;
                }
                continue;
            }
            if let Some(target) = name.strip_prefix(b".wh.") {
                if target.is_empty() {
                    return Err("oracle whiteout marker has empty target".to_owned());
                }
                push_record(
                    records,
                    Record::Whiteout {
                        path: join_path(&relative, target)?,
                    },
                )?;
                entry_count = entry_count.saturating_add(1);
                continue;
            }
            let child = join_path(&relative, &name)?;
            let child_path = physical_path(root, &child);
            let child_metadata = std::fs::symlink_metadata(&child_path)
                .map_err(|error| format!("lstat oracle entry {}: {error}", child_path.display()))?;
            if is_kernel_whiteout(&child_metadata)? {
                push_record(records, Record::Whiteout { path: child })?;
                entry_count = entry_count.saturating_add(1);
            } else if child_metadata.is_dir() {
                queue.push(&child)?;
            } else {
                bytes_read = bytes_read.saturating_add(scan_node(
                    &child_path,
                    &child,
                    &child_metadata,
                    records,
                    hardlinks,
                )?);
                entry_count = entry_count.saturating_add(1);
            }
        }
    }
    Ok((entry_count, bytes_read))
}

fn scan_node(
    physical: &Path,
    relative: &[u8],
    metadata: &std::fs::Metadata,
    records: &mut DiskSorter,
    hardlinks: &mut DiskSorter,
) -> OracleResult<u64> {
    let kind = node_kind(metadata)?;
    let symlink_target = if kind == NodeKind::Symlink {
        std::fs::read_link(physical)
            .map_err(|error| format!("read oracle symlink {}: {error}", physical.display()))?
            .into_os_string()
            .into_vec()
    } else {
        Vec::new()
    };
    let logical_size = match kind {
        NodeKind::Regular => metadata.len(),
        NodeKind::Symlink => u64::try_from(symlink_target.len()).unwrap_or(u64::MAX),
        _ => 0,
    };
    let (device_major, device_minor) =
        if matches!(kind, NodeKind::CharacterDevice | NodeKind::BlockDevice) {
            device_components(metadata)?
        } else {
            (0, 0)
        };
    push_record(
        records,
        Record::Node(NodeRecord {
            path: relative.to_vec(),
            kind,
            mode: metadata.permissions().mode() & 0o7777,
            uid: metadata.uid(),
            gid: metadata.gid(),
            mtime_seconds: metadata.mtime(),
            mtime_nanoseconds: u32::try_from(metadata.mtime_nsec())
                .map_err(|_| "oracle mtime nanoseconds are negative".to_owned())?,
            logical_size,
            symlink_target,
            device_major,
            device_minor,
        }),
    )?;
    let opaque = scan_xattrs(physical, relative, records)?;
    if kind == NodeKind::Directory && opaque {
        push_record(
            records,
            Record::OpaqueDirectory {
                path: relative.to_vec(),
            },
        )?;
    }
    if kind != NodeKind::Regular {
        return Ok(0);
    }
    let (read, content_sha256) = scan_regular(physical, relative, logical_size, records)?;
    if metadata.nlink() > 1 {
        let mut key = Vec::with_capacity(16 + relative.len());
        key.extend_from_slice(&metadata.dev().to_be_bytes());
        key.extend_from_slice(&metadata.ino().to_be_bytes());
        key.extend_from_slice(relative);
        let mut payload = Vec::with_capacity(36 + relative.len());
        payload.extend_from_slice(&content_sha256);
        payload.extend_from_slice(
            &u32::try_from(relative.len())
                .map_err(|_| "oracle hardlink path length overflow".to_owned())?
                .to_be_bytes(),
        );
        payload.extend_from_slice(relative);
        hardlinks.push(key, payload)?;
    }
    Ok(read)
}

fn scan_regular(
    path: &Path,
    relative: &[u8],
    logical_size: u64,
    records: &mut DiskSorter,
) -> OracleResult<(u64, [u8; 32])> {
    let file = File::open(path)
        .map_err(|error| format!("open oracle file {}: {error}", path.display()))?;
    let mut semantic = Sha256::new();
    semantic.update(b"mpla-poc-semantic-v1/regular-content\0");
    semantic.update(logical_size.to_be_bytes());
    if logical_size > 0
        && is_fully_allocated(&file, path, logical_size).map_err(|error| {
            format!("oracle inspect file allocation {}: {error}", path.display())
        })?
    {
        emit_extent(
            records,
            &mut semantic,
            relative,
            0,
            logical_size,
            ExtentKind::Data,
        )?;
        let bytes_read = read_data_extent_parallel(
            &file,
            path,
            relative,
            0,
            logical_size,
            records,
            &mut semantic,
        )?;
        return Ok((bytes_read, semantic.finalize().into()));
    }
    let mut cursor = 0_u64;
    let mut bytes_read = 0_u64;
    while cursor < logical_size {
        let seek_cursor =
            i64::try_from(cursor).map_err(|_| "oracle file offset exceeds i64".to_owned())?;
        let data_start = match rustix::fs::seek(&file, SeekFrom::Data(seek_cursor)) {
            Ok(value) => value.min(logical_size),
            Err(error) if error == rustix::io::Errno::NXIO => logical_size,
            Err(error)
                if cursor == 0
                    && (error == rustix::io::Errno::INVAL
                        || error == rustix::io::Errno::NOTSUP) =>
            {
                return Err(format!(
                    "oracle filesystem lacks SEEK_DATA/SEEK_HOLE for {}",
                    path.display()
                ));
            }
            Err(error) => return Err(format!("oracle SEEK_DATA {}: {error}", path.display())),
        };
        if data_start > cursor {
            emit_extent(
                records,
                &mut semantic,
                relative,
                cursor,
                data_start - cursor,
                ExtentKind::Hole,
            )?;
        }
        if data_start == logical_size {
            break;
        }
        let data_offset =
            i64::try_from(data_start).map_err(|_| "oracle file offset exceeds i64".to_owned())?;
        let data_end = rustix::fs::seek(&file, SeekFrom::Hole(data_offset))
            .map_err(|error| format!("oracle SEEK_HOLE {}: {error}", path.display()))?
            .min(logical_size);
        if data_end <= data_start {
            return Err("oracle sparse extent did not progress".to_owned());
        }
        emit_extent(
            records,
            &mut semantic,
            relative,
            data_start,
            data_end - data_start,
            ExtentKind::Data,
        )?;
        bytes_read = bytes_read.saturating_add(read_data_extent_parallel(
            &file,
            path,
            relative,
            data_start,
            data_end,
            records,
            &mut semantic,
        )?);
        cursor = data_end;
    }
    Ok((bytes_read, semantic.finalize().into()))
}

fn read_data_extent_parallel(
    file: &File,
    path: &Path,
    relative: &[u8],
    start: u64,
    end: u64,
    records: &mut DiskSorter,
    semantic: &mut Sha256,
) -> OracleResult<u64> {
    let transfer = u64::try_from(SEMANTIC_SCAN_TRANSFER_BYTES)
        .map_err(|_| "oracle semantic transfer size overflows u64".to_owned())?;
    let spans = (end - start).div_ceil(transfer);
    let worker_count = usize::try_from(spans)
        .unwrap_or(usize::from(MAX_DATA_WORKERS))
        .min(usize::from(MAX_DATA_WORKERS))
        .max(1);
    if worker_count == 1 {
        let batch = read_chunk_batch(file, path, start, end)?;
        return emit_chunk_batch(batch, records, semantic, relative);
    }

    let mut worker_files = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        worker_files.push(
            file.try_clone()
                .map_err(|error| format!("duplicate oracle file {}: {error}", path.display()))?,
        );
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
                    let result = read_chunk_batch(&worker_file, path, offset, job_end);
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
                    let batch_start = batch.start;
                    if pending.insert(batch_start, batch).is_some() {
                        first_error =
                            Some("oracle parallel scan produced duplicate offsets".to_owned());
                        cancelled.store(true, Ordering::Release);
                        continue;
                    }
                    while let Some(batch) = pending.remove(&expected_offset) {
                        let batch_end = batch.end;
                        match emit_chunk_batch(batch, records, semantic, relative) {
                            Ok(read) => {
                                bytes_read = bytes_read.saturating_add(read);
                                expected_offset = batch_end;
                            }
                            Err(error) => {
                                first_error = Some(error);
                                cancelled.store(true, Ordering::Release);
                                break;
                            }
                        }
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
                first_error = Some("oracle parallel scan worker panicked".to_owned());
            }
        }
    });
    if let Some(error) = first_error {
        return Err(error);
    }
    if expected_offset != end || !pending.is_empty() {
        return Err("oracle parallel scan did not complete its data extent".to_owned());
    }
    Ok(bytes_read)
}

fn read_chunk_batch(file: &File, path: &Path, start: u64, end: u64) -> OracleResult<ChunkBatch> {
    let mut buffer = vec![0_u8; SEMANTIC_SCAN_TRANSFER_BYTES];
    let mut chunks = Vec::with_capacity(SEMANTIC_SCAN_TRANSFER_BYTES / SCAN_WINDOW_BYTES);
    let mut offset = start;
    while offset < end {
        let wanted = usize::try_from((end - offset).min(buffer.len() as u64))
            .map_err(|_| "oracle transfer length overflow".to_owned())?;
        read_file_at(file, &mut buffer[..wanted], offset, path)?;
        let mut chunk_offset = 0_usize;
        while chunk_offset < wanted {
            let length = (wanted - chunk_offset).min(SCAN_WINDOW_BYTES);
            let chunk_end = chunk_offset
                .checked_add(length)
                .ok_or_else(|| "oracle chunk end overflow".to_owned())?;
            let mut chunk = Sha256::new();
            chunk.update(b"mpla-poc-semantic-v1/chunk-bytes\0");
            chunk.update(&buffer[chunk_offset..chunk_end]);
            chunks.push(ChunkDigest {
                offset: offset.saturating_add(u64::try_from(chunk_offset).unwrap_or(u64::MAX)),
                length: u32::try_from(length)
                    .map_err(|_| "oracle chunk length exceeds u32".to_owned())?,
                sha256: chunk.finalize().into(),
            });
            chunk_offset = chunk_end;
        }
        offset = offset.saturating_add(u64::try_from(wanted).unwrap_or(u64::MAX));
    }
    Ok(ChunkBatch {
        start,
        end,
        bytes_read: end - start,
        chunks,
    })
}

fn emit_chunk_batch(
    batch: ChunkBatch,
    records: &mut DiskSorter,
    semantic: &mut Sha256,
    relative: &[u8],
) -> OracleResult<u64> {
    let bytes_read = batch.bytes_read;
    for chunk in batch.chunks {
        push_record(
            records,
            Record::Chunk {
                path: relative.to_vec(),
                offset: chunk.offset,
                length: chunk.length,
                sha256: chunk.sha256,
            },
        )?;
        semantic.update(b"chunk\0");
        semantic.update(chunk.offset.to_be_bytes());
        semantic.update(u64::from(chunk.length).to_be_bytes());
        semantic.update(chunk.sha256);
    }
    Ok(bytes_read)
}

fn emit_extent(
    records: &mut DiskSorter,
    semantic: &mut Sha256,
    path: &[u8],
    offset: u64,
    length: u64,
    kind: ExtentKind,
) -> OracleResult<()> {
    push_record(
        records,
        Record::Extent {
            path: path.to_vec(),
            offset,
            length,
            kind,
        },
    )?;
    semantic.update(b"extent\0");
    semantic.update([kind as u8]);
    semantic.update(offset.to_be_bytes());
    semantic.update(length.to_be_bytes());
    Ok(())
}

fn scan_xattrs(path: &Path, relative: &[u8], records: &mut DiskSorter) -> OracleResult<bool> {
    let size = rustix::fs::llistxattr(path, &mut [])
        .map_err(|error| format!("size oracle xattrs {}: {error}", path.display()))?;
    if size > MAX_XATTR_LIST_BYTES {
        return Err("oracle xattr name list exceeds fixed bound".to_owned());
    }
    let mut raw: Vec<libc::c_char> = vec![0; size];
    let listed = rustix::fs::llistxattr(path, &mut raw)
        .map_err(|error| format!("read oracle xattrs {}: {error}", path.display()))?;
    raw.truncate(listed);
    let list = raw
        .into_iter()
        .map(|byte| byte.to_ne_bytes()[0])
        .collect::<Vec<_>>();
    let mut opaque = false;
    for name in list
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let os_name = OsStr::from_bytes(name);
        let size = rustix::fs::lgetxattr(path, os_name, &mut [])
            .map_err(|error| format!("size oracle xattr {}: {error}", path.display()))?;
        if size > MAX_XATTR_BYTES {
            return Err("oracle xattr value exceeds fixed bound".to_owned());
        }
        let mut value = vec![0_u8; size];
        let read = rustix::fs::lgetxattr(path, os_name, &mut value)
            .map_err(|error| format!("read oracle xattr {}: {error}", path.display()))?;
        value.truncate(read);
        if OPAQUE_XATTRS.contains(&name) {
            opaque |= matches!(value.as_slice(), b"y" | b"Y" | b"1");
        } else if OVERLAY_INTERNAL_XATTRS.contains(&name) {
            continue;
        } else {
            push_record(
                records,
                Record::Xattr {
                    path: relative.to_vec(),
                    name: name.to_vec(),
                    value,
                },
            )?;
        }
    }
    Ok(opaque)
}

fn directory_is_opaque(path: &Path) -> OracleResult<bool> {
    for name in OPAQUE_XATTRS {
        let mut value = [0_u8; 8];
        match rustix::fs::lgetxattr(path, OsStr::from_bytes(name), &mut value) {
            Ok(count) if matches!(&value[..count], b"y" | b"Y" | b"1") => return Ok(true),
            Ok(_) => {}
            Err(error) if missing_xattr(error) => {}
            Err(error) => {
                return Err(format!(
                    "read oracle opaque xattr {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(false)
}

fn append_hardlink_records(
    records: &mut DiskSorter,
    hardlinks: SortedRun,
    work: &Path,
) -> OracleResult<()> {
    let mut groups = LinkGroups::new(records, work.join("members.tmp"))?;
    hardlinks.for_each(|key, payload| groups.accept(key, payload))?;
    groups.finish()
}

struct LinkGroups<'a> {
    records: &'a mut DiskSorter,
    path: PathBuf,
    physical: Option<[u8; 16]>,
    content: [u8; 32],
    count: u64,
    group: Sha256,
    members: Option<BufWriter<File>>,
}

impl<'a> LinkGroups<'a> {
    fn new(records: &'a mut DiskSorter, path: PathBuf) -> OracleResult<Self> {
        let mut value = Self {
            records,
            path,
            physical: None,
            content: [0; 32],
            count: 0,
            group: Sha256::new(),
            members: None,
        };
        value.reset()?;
        Ok(value)
    }

    fn accept(&mut self, key: &[u8], payload: &[u8]) -> OracleResult<()> {
        if key.len() < 16 || payload.len() < 36 {
            return Err("oracle hardlink claim is malformed".to_owned());
        }
        let physical: [u8; 16] = key[..16]
            .try_into()
            .map_err(|_| "oracle hardlink physical key is malformed".to_owned())?;
        let content: [u8; 32] = payload[..32]
            .try_into()
            .map_err(|_| "oracle hardlink content is malformed".to_owned())?;
        let length = usize::try_from(u32::from_be_bytes(
            payload[32..36]
                .try_into()
                .map_err(|_| "oracle hardlink length is malformed".to_owned())?,
        ))
        .map_err(|_| "oracle hardlink length overflow".to_owned())?;
        let member = payload
            .get(36..)
            .filter(|value| value.len() == length)
            .ok_or_else(|| "oracle hardlink member is malformed".to_owned())?;
        if key.get(16..) != Some(member) {
            return Err("oracle hardlink key and member disagree".to_owned());
        }
        if self.physical.is_some_and(|value| value != physical) {
            self.emit()?;
            self.reset()?;
        }
        if self.physical.is_none() {
            self.physical = Some(physical);
            self.content = content;
            self.group = Sha256::new();
            self.group.update(b"mpla-poc-semantic-v1/hardlink-group\0");
            self.group.update(content);
        } else if self.content != content {
            return Err("oracle hardlink aliases differ in content".to_owned());
        }
        self.group.update(
            u64::try_from(member.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        self.group.update(member);
        self.members
            .as_mut()
            .ok_or_else(|| "oracle hardlink member file is closed".to_owned())?
            .write_all(
                &u32::try_from(member.len())
                    .map_err(|_| "oracle hardlink member length overflow".to_owned())?
                    .to_be_bytes(),
            )
            .and_then(|()| {
                self.members
                    .as_mut()
                    .expect("member writer exists")
                    .write_all(member)
            })
            .map_err(|error| format!("write oracle hardlink members: {error}"))?;
        self.count = self.count.saturating_add(1);
        Ok(())
    }

    fn finish(mut self) -> OracleResult<()> {
        self.emit()
    }

    fn emit(&mut self) -> OracleResult<()> {
        if self.physical.is_none() {
            return Ok(());
        }
        let mut writer = self
            .members
            .take()
            .ok_or_else(|| "oracle hardlink member file is closed".to_owned())?;
        writer
            .flush()
            .and_then(|()| writer.get_ref().sync_all())
            .map_err(|error| format!("flush oracle hardlink members: {error}"))?;
        if self.count >= 2 {
            let group_sha256 = self.group.clone().finalize().into();
            push_record(
                self.records,
                Record::HardlinkGroup {
                    group_sha256,
                    content_sha256: self.content,
                    member_count: self.count,
                },
            )?;
            let mut reader = BufReader::new(
                File::open(&self.path)
                    .map_err(|error| format!("open oracle hardlink members: {error}"))?,
            );
            loop {
                let mut length = [0_u8; 4];
                if !read_exact_or_eof(&mut reader, &mut length)
                    .map_err(|error| format!("read oracle hardlink frame: {error}"))?
                {
                    break;
                }
                let length = usize::try_from(u32::from_be_bytes(length))
                    .map_err(|_| "oracle hardlink member length overflow".to_owned())?;
                if length > MAX_PATH_BYTES {
                    return Err("oracle hardlink member exceeds path bound".to_owned());
                }
                let mut member = vec![0_u8; length];
                reader
                    .read_exact(&mut member)
                    .map_err(|error| format!("read oracle hardlink member: {error}"))?;
                push_record(
                    self.records,
                    Record::HardlinkMember {
                        group_sha256,
                        path: member,
                    },
                )?;
            }
        }
        self.physical = None;
        self.count = 0;
        Ok(())
    }

    fn reset(&mut self) -> OracleResult<()> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.path)
            .map_err(|error| format!("create oracle hardlink members: {error}"))?;
        self.members = Some(BufWriter::new(file));
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct SortStats {
    initial_runs: u64,
    bytes_written: u64,
}

struct DiskSorter {
    root: PathBuf,
    limit: usize,
    buffered: usize,
    entries: Vec<Entry>,
    slots: [Option<PathBuf>; 64],
    sequence: u64,
    stats: SortStats,
}

impl DiskSorter {
    fn new(root: PathBuf, limit: usize) -> OracleResult<Self> {
        std::fs::create_dir(&root)
            .map_err(|error| format!("create oracle sorter {}: {error}", root.display()))?;
        Ok(Self {
            root,
            limit,
            buffered: 0,
            entries: Vec::new(),
            slots: std::array::from_fn(|_| None),
            sequence: 0,
            stats: SortStats::default(),
        })
    }

    fn push(&mut self, key: Vec<u8>, payload: Vec<u8>) -> OracleResult<()> {
        validate_entry(&key, &payload)?;
        let bytes = key.len().saturating_add(payload.len()).saturating_add(8);
        if bytes > self.limit {
            return Err("oracle sort entry exceeds memory run".to_owned());
        }
        if !self.entries.is_empty() && self.buffered.saturating_add(bytes) > self.limit {
            self.flush()?;
        }
        self.buffered = self.buffered.saturating_add(bytes);
        self.entries.push(Entry { key, payload });
        Ok(())
    }

    fn finish(mut self) -> OracleResult<SortedRun> {
        if !self.entries.is_empty() {
            self.flush()?;
        }
        let mut result: Option<PathBuf> = None;
        for level in 0..self.slots.len() {
            if let Some(path) = self.slots[level].take() {
                result = Some(if let Some(previous) = result {
                    let output = self.path("finish");
                    let written = merge_runs(&[previous.clone(), path.clone()], &output)?;
                    self.stats.bytes_written = self.stats.bytes_written.saturating_add(written);
                    remove_file(&previous)?;
                    remove_file(&path)?;
                    output
                } else {
                    path
                });
            }
        }
        let path = if let Some(path) = result {
            path
        } else {
            let path = self.path("empty");
            write_run(&path, std::iter::empty())?;
            path
        };
        Ok(SortedRun {
            path,
            stats: self.stats,
        })
    }

    fn flush(&mut self) -> OracleResult<()> {
        self.entries
            .sort_unstable_by(|left, right| left.key.cmp(&right.key));
        if self
            .entries
            .windows(2)
            .any(|pair| pair[0].key >= pair[1].key)
        {
            return Err("oracle sort contains duplicate key".to_owned());
        }
        let path = self.path("initial");
        let written = write_run(
            &path,
            self.entries
                .drain(..)
                .map(|entry| (entry.key, entry.payload)),
        )?;
        self.stats.initial_runs = self.stats.initial_runs.saturating_add(1);
        self.stats.bytes_written = self.stats.bytes_written.saturating_add(written);
        self.buffered = 0;
        self.add_run(path, 0)
    }

    fn add_run(&mut self, mut path: PathBuf, mut level: usize) -> OracleResult<()> {
        loop {
            if level >= self.slots.len() {
                return Err("oracle sorter exceeded fixed merge levels".to_owned());
            }
            if let Some(existing) = self.slots[level].take() {
                let output = self.path("merge");
                let written = merge_runs(&[existing.clone(), path.clone()], &output)?;
                self.stats.bytes_written = self.stats.bytes_written.saturating_add(written);
                remove_file(&existing)?;
                remove_file(&path)?;
                path = output;
                level += 1;
            } else {
                self.slots[level] = Some(path);
                return Ok(());
            }
        }
    }

    fn path(&mut self, class: &str) -> PathBuf {
        let sequence = self.sequence;
        self.sequence = self.sequence.saturating_add(1);
        self.root.join(format!("{class}-{sequence:016x}.run"))
    }
}

struct Entry {
    key: Vec<u8>,
    payload: Vec<u8>,
}

struct SortedRun {
    path: PathBuf,
    stats: SortStats,
}

impl SortedRun {
    fn for_each(
        &self,
        mut visitor: impl FnMut(&[u8], &[u8]) -> OracleResult<()>,
    ) -> OracleResult<()> {
        let mut reader = RunReader::open(&self.path)?;
        let mut previous = None;
        while let Some(entry) = reader.next()? {
            if previous
                .as_ref()
                .is_some_and(|value: &Vec<u8>| value >= &entry.key)
            {
                return Err("oracle sorted run is not strictly ordered".to_owned());
            }
            visitor(&entry.key, &entry.payload)?;
            previous = Some(entry.key);
        }
        Ok(())
    }
}

fn materialize(sorted: &SortedRun, output: &Path) -> OracleResult<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|error| format!("create oracle record stream {}: {error}", output.display()))?;
    let mut writer = BufWriter::new(file);
    sorted.for_each(|key, payload| {
        let record = Record::decode(payload)?;
        if record.key_digest()?.as_slice() != key {
            return Err("oracle sorted key and record disagree".to_owned());
        }
        writer
            .write_all(
                &u32::try_from(payload.len())
                    .map_err(|_| "oracle record length overflow".to_owned())?
                    .to_be_bytes(),
            )
            .and_then(|()| writer.write_all(payload))
            .map_err(|error| format!("write oracle record stream: {error}"))
    })?;
    writer
        .flush()
        .and_then(|()| writer.get_ref().sync_all())
        .map_err(|error| format!("flush oracle record stream: {error}"))
}

fn write_run(
    path: &Path,
    entries: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
) -> OracleResult<u64> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create oracle run {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(RUN_MAGIC)
        .map_err(|error| format!("write oracle run magic: {error}"))?;
    let mut written = 8_u64;
    for (key, payload) in entries {
        validate_entry(&key, &payload)?;
        writer
            .write_all(
                &u32::try_from(key.len())
                    .map_err(|_| "oracle sort key length overflow".to_owned())?
                    .to_be_bytes(),
            )
            .and_then(|()| {
                writer.write_all(
                    &u32::try_from(payload.len())
                        .expect("bounded oracle payload")
                        .to_be_bytes(),
                )
            })
            .and_then(|()| writer.write_all(&key))
            .and_then(|()| writer.write_all(&payload))
            .map_err(|error| format!("write oracle run entry: {error}"))?;
        written = written
            .saturating_add(u64::try_from(8 + key.len() + payload.len()).unwrap_or(u64::MAX));
    }
    writer
        .flush()
        .and_then(|()| writer.get_ref().sync_all())
        .map_err(|error| format!("flush oracle run {}: {error}", path.display()))?;
    Ok(written)
}

fn merge_runs(inputs: &[PathBuf], output: &Path) -> OracleResult<u64> {
    let mut readers = inputs
        .iter()
        .map(|path| RunReader::open(path))
        .collect::<OracleResult<Vec<_>>>()?;
    let mut heads = readers
        .iter_mut()
        .map(RunReader::next)
        .collect::<OracleResult<Vec<_>>>()?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|error| format!("create oracle merge {}: {error}", output.display()))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(RUN_MAGIC)
        .map_err(|error| format!("write oracle merge magic: {error}"))?;
    let mut previous = None;
    let mut written = 8_u64;
    loop {
        let next = heads
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.as_ref().map(|entry| (index, &entry.key)))
            .min_by(|left, right| left.1.cmp(right.1))
            .map(|(index, _)| index);
        let Some(index) = next else {
            break;
        };
        let entry = heads[index]
            .take()
            .ok_or_else(|| "oracle merge head disappeared".to_owned())?;
        if previous
            .as_ref()
            .is_some_and(|value: &Vec<u8>| value >= &entry.key)
        {
            return Err("oracle merge found duplicate key".to_owned());
        }
        writer
            .write_all(
                &u32::try_from(entry.key.len())
                    .map_err(|_| "oracle merge key overflow".to_owned())?
                    .to_be_bytes(),
            )
            .and_then(|()| {
                writer.write_all(
                    &u32::try_from(entry.payload.len())
                        .expect("bounded oracle payload")
                        .to_be_bytes(),
                )
            })
            .and_then(|()| writer.write_all(&entry.key))
            .and_then(|()| writer.write_all(&entry.payload))
            .map_err(|error| format!("write oracle merge entry: {error}"))?;
        written = written.saturating_add(
            u64::try_from(8 + entry.key.len() + entry.payload.len()).unwrap_or(u64::MAX),
        );
        previous = Some(entry.key);
        heads[index] = readers[index].next()?;
    }
    writer
        .flush()
        .and_then(|()| writer.get_ref().sync_all())
        .map_err(|error| format!("flush oracle merge: {error}"))?;
    Ok(written)
}

struct RunReader {
    path: PathBuf,
    reader: BufReader<File>,
}

impl RunReader {
    fn open(path: &Path) -> OracleResult<Self> {
        let mut reader = BufReader::new(
            File::open(path)
                .map_err(|error| format!("open oracle run {}: {error}", path.display()))?,
        );
        let mut magic = [0_u8; 8];
        reader
            .read_exact(&mut magic)
            .map_err(|error| format!("read oracle run magic: {error}"))?;
        if &magic != RUN_MAGIC {
            return Err("oracle run magic mismatch".to_owned());
        }
        Ok(Self {
            path: path.to_path_buf(),
            reader,
        })
    }

    fn next(&mut self) -> OracleResult<Option<Entry>> {
        let mut header = [0_u8; 8];
        if !read_exact_or_eof(&mut self.reader, &mut header)
            .map_err(|error| format!("read oracle run {}: {error}", self.path.display()))?
        {
            return Ok(None);
        }
        let key_length = usize::try_from(u32::from_be_bytes(
            header[..4].try_into().expect("fixed oracle header"),
        ))
        .map_err(|_| "oracle run key length overflow".to_owned())?;
        let payload_length = usize::try_from(u32::from_be_bytes(
            header[4..].try_into().expect("fixed oracle header"),
        ))
        .map_err(|_| "oracle run payload length overflow".to_owned())?;
        if key_length == 0 || key_length > MAX_KEY_BYTES || payload_length > MAX_RECORD_BYTES {
            return Err("oracle run entry exceeds fixed bounds".to_owned());
        }
        let mut key = vec![0_u8; key_length];
        let mut payload = vec![0_u8; payload_length];
        self.reader
            .read_exact(&mut key)
            .and_then(|()| self.reader.read_exact(&mut payload))
            .map_err(|error| format!("read oracle run payload: {error}"))?;
        Ok(Some(Entry { key, payload }))
    }
}

struct DirectoryQueue {
    path: PathBuf,
    file: File,
    read_offset: u64,
    write_offset: u64,
}

impl DirectoryQueue {
    fn create(path: PathBuf) -> OracleResult<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("create oracle directory queue: {error}"))?;
        write_at(&file, QUEUE_MAGIC, 0)?;
        Ok(Self {
            path,
            file,
            read_offset: 8,
            write_offset: 8,
        })
    }

    fn push(&mut self, path: &[u8]) -> OracleResult<()> {
        validate_path(path, true)?;
        write_at(
            &self.file,
            &u32::try_from(path.len())
                .map_err(|_| "oracle queue path length overflow".to_owned())?
                .to_be_bytes(),
            self.write_offset,
        )?;
        self.write_offset = self.write_offset.saturating_add(4);
        write_at(&self.file, path, self.write_offset)?;
        self.write_offset = self
            .write_offset
            .saturating_add(u64::try_from(path.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    fn pop(&mut self) -> OracleResult<Option<Vec<u8>>> {
        if self.read_offset == self.write_offset {
            return Ok(None);
        }
        let mut length = [0_u8; 4];
        read_at(&self.file, &mut length, self.read_offset, &self.path)?;
        self.read_offset = self.read_offset.saturating_add(4);
        let length = usize::try_from(u32::from_be_bytes(length))
            .map_err(|_| "oracle queue path length overflow".to_owned())?;
        if length > MAX_PATH_BYTES {
            return Err("oracle queue path exceeds fixed bound".to_owned());
        }
        let mut path = vec![0_u8; length];
        read_at(&self.file, &mut path, self.read_offset, &self.path)?;
        self.read_offset = self
            .read_offset
            .saturating_add(u64::try_from(length).unwrap_or(u64::MAX));
        Ok(Some(path))
    }
}

fn push_record(sorter: &mut DiskSorter, record: Record) -> OracleResult<()> {
    sorter.push(record.key_digest()?.to_vec(), record.encode()?)
}

fn validate_entry(key: &[u8], payload: &[u8]) -> OracleResult<()> {
    if key.is_empty()
        || key.len() > MAX_KEY_BYTES
        || payload.is_empty()
        || payload.len() > MAX_RECORD_BYTES
    {
        return Err("oracle sort entry exceeds fixed bounds".to_owned());
    }
    Ok(())
}

fn read_file_at(file: &File, bytes: &mut [u8], offset: u64, path: &Path) -> OracleResult<()> {
    let mut filled = 0;
    while filled < bytes.len() {
        let position = offset.saturating_add(u64::try_from(filled).unwrap_or(u64::MAX));
        let count = file
            .read_at(&mut bytes[filled..], position)
            .map_err(|error| format!("read oracle file {}: {error}", path.display()))?;
        if count == 0 {
            return Err("oracle file changed during scan".to_owned());
        }
        filled += count;
    }
    Ok(())
}

fn write_at(file: &File, bytes: &[u8], offset: u64) -> OracleResult<()> {
    let mut written = 0;
    while written < bytes.len() {
        let position = offset.saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        let count = file
            .write_at(&bytes[written..], position)
            .map_err(|error| format!("write oracle directory queue: {error}"))?;
        if count == 0 {
            return Err("oracle directory queue write made no progress".to_owned());
        }
        written += count;
    }
    Ok(())
}

fn read_at(file: &File, bytes: &mut [u8], offset: u64, path: &Path) -> OracleResult<()> {
    let mut filled = 0;
    while filled < bytes.len() {
        let position = offset.saturating_add(u64::try_from(filled).unwrap_or(u64::MAX));
        let count = file
            .read_at(&mut bytes[filled..], position)
            .map_err(|error| format!("read oracle queue {}: {error}", path.display()))?;
        if count == 0 {
            return Err("oracle directory queue is truncated".to_owned());
        }
        filled += count;
    }
    Ok(())
}

fn read_exact_or_eof(reader: &mut impl Read, bytes: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < bytes.len() {
        let count = reader.read(&mut bytes[filled..])?;
        if count == 0 {
            if filled == 0 {
                return Ok(false);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "partial oracle frame",
            ));
        }
        filled += count;
    }
    Ok(true)
}

fn device_components(metadata: &std::fs::Metadata) -> OracleResult<(u32, u32)> {
    let device: rustix::fs::Dev = metadata
        .rdev()
        .try_into()
        .map_err(|_| "oracle device id does not fit dev_t".to_owned())?;
    Ok((rustix::fs::major(device), rustix::fs::minor(device)))
}

fn missing_xattr(error: rustix::io::Errno) -> bool {
    if error == rustix::io::Errno::NODATA {
        return true;
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        if error == rustix::io::Errno::NOATTR {
            return true;
        }
    }
    false
}

fn is_kernel_whiteout(metadata: &std::fs::Metadata) -> OracleResult<bool> {
    Ok(metadata.file_type().is_char_device() && device_components(metadata)? == (0, 0))
}

fn node_kind(metadata: &std::fs::Metadata) -> OracleResult<NodeKind> {
    let kind = metadata.file_type();
    if kind.is_file() {
        Ok(NodeKind::Regular)
    } else if kind.is_dir() {
        Ok(NodeKind::Directory)
    } else if kind.is_symlink() {
        Ok(NodeKind::Symlink)
    } else if kind.is_fifo() {
        Ok(NodeKind::Fifo)
    } else if kind.is_char_device() {
        Ok(NodeKind::CharacterDevice)
    } else if kind.is_block_device() {
        Ok(NodeKind::BlockDevice)
    } else if kind.is_socket() {
        Ok(NodeKind::Socket)
    } else {
        Err("oracle encountered unsupported node type".to_owned())
    }
}

fn join_path(parent: &[u8], name: &[u8]) -> OracleResult<Vec<u8>> {
    let mut output = Vec::with_capacity(parent.len().saturating_add(name.len()).saturating_add(1));
    if !parent.is_empty() {
        output.extend_from_slice(parent);
        output.push(b'/');
    }
    output.extend_from_slice(name);
    validate_path(&output, false)?;
    Ok(output)
}

fn physical_path(root: &Path, relative: &[u8]) -> PathBuf {
    if relative.is_empty() {
        root.to_path_buf()
    } else {
        root.join(std::ffi::OsString::from_vec(relative.to_vec()))
    }
}

fn remove_file(path: &Path) -> OracleResult<()> {
    std::fs::remove_file(path)
        .map_err(|error| format!("remove oracle run {}: {error}", path.display()))
}
