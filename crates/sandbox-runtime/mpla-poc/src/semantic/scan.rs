use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::MAX_DATA_WORKERS;
use crate::{PocError, PocResult};

use super::chunk;
use super::record::{validate_path, NodeKind, NodeRecord, SemanticRecord, MAX_PATH_BYTES};
use super::spool::{BoundedSpool, SortedSpool, SpoolSink};

const OPAQUE_XATTRS: [&[u8]; 2] = [b"trusted.overlay.opaque", b"user.overlay.opaque"];
const OVERLAY_INTERNAL_XATTRS: [&[u8]; 2] = [b"trusted.overlay.uuid", b"user.overlay.uuid"];
const OPAQUE_MARKER: &[u8] = b".wh..wh..opq";
const WHITEOUT_PREFIX: &[u8] = b".wh.";
const QUEUE_MAGIC: &[u8; 8] = b"MPLAQUE1";
const MAX_XATTR_LIST_BYTES: usize = 1024 * 1024;
pub(super) const MAX_XATTR_TRANSIENT_BYTES: usize =
    2 * MAX_XATTR_LIST_BYTES + super::record::MAX_XATTR_BYTES;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScanStats {
    pub bytes_read: u64,
    pub entry_count: u64,
    pub peak_open_data_fds: usize,
    pub peak_data_workers: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedPathScan {
    pub records: Vec<SemanticRecord>,
    pub bytes_read: u64,
    pub peak_open_data_fds: usize,
    pub peak_data_workers: u16,
}

pub fn scan_selected_paths(
    root: &Path,
    relative_paths: &[PathBuf],
    work_dir: &Path,
) -> PocResult<SelectedPathScan> {
    validate_selected_paths(relative_paths)?;
    std::fs::create_dir_all(work_dir)
        .map_err(|error| PocError::io("create selected-path work directory", work_dir, error))?;
    let scan_dir = work_dir.join(format!("selected-{}", Uuid::new_v4()));
    std::fs::create_dir(&scan_dir)
        .map_err(|error| PocError::io("create selected-path scan directory", &scan_dir, error))?;
    let result = scan_selected_paths_in(root, relative_paths, &scan_dir);
    let cleanup = std::fs::remove_dir_all(&scan_dir)
        .map_err(|error| PocError::io("remove selected-path scan directory", &scan_dir, error));
    match (result, cleanup) {
        (Ok(scan), Ok(())) => Ok(scan),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn scan_selected_paths_in(
    root: &Path,
    relative_paths: &[PathBuf],
    scan_dir: &Path,
) -> PocResult<SelectedPathScan> {
    let mut records = BoundedSpool::new(scan_dir.join("records"), 1024 * 1024)?;
    let hardlinks = BoundedSpool::new(scan_dir.join("hardlinks"), 1024 * 1024)?;
    let mut stats = ScanStats {
        peak_open_data_fds: 3,
        peak_data_workers: 1,
        ..ScanStats::default()
    };
    for relative_path in relative_paths {
        let relative = relative_path_bytes(relative_path)?;
        let physical = root.join(relative_path);
        let metadata = std::fs::symlink_metadata(&physical)
            .map_err(|error| PocError::io("lstat selected semantic path", &physical, error))?;
        if metadata.file_type().is_dir() && !relative.is_empty() {
            return Err(PocError::Integrity(format!(
                "selected semantic path is a directory: {}",
                relative_path.display()
            )));
        }
        if metadata.file_type().is_file() && metadata.nlink() > 1 {
            return Err(PocError::Integrity(format!(
                "receipt-hit selected path has hardlink aliases: {}",
                relative_path.display()
            )));
        }
        let node = scan_selected_node(&physical, &relative, &metadata, &mut records)?;
        stats.bytes_read = stats.bytes_read.saturating_add(node.bytes_read);
        stats.entry_count = stats.entry_count.saturating_add(1);
        stats.peak_data_workers = stats.peak_data_workers.max(node.data_workers);
        stats.peak_open_data_fds = stats
            .peak_open_data_fds
            .max(2_usize.saturating_add(usize::from(node.data_workers)));
    }
    let hardlinks = hardlinks.finish()?;
    if hardlinks.stats().records_out != 0 {
        return Err(PocError::Integrity(
            "receipt-hit selected scan produced hardlink claims".to_owned(),
        ));
    }
    let sorted = records.finish()?;
    let mut decoded = Vec::with_capacity(
        usize::try_from(sorted.stats().records_out)
            .map_err(|_| PocError::Integrity("selected record count overflow".to_owned()))?,
    );
    sorted.for_each(|_, payload| {
        decoded.push(SemanticRecord::decode(payload)?);
        Ok(())
    })?;
    Ok(SelectedPathScan {
        records: decoded,
        bytes_read: stats.bytes_read,
        peak_open_data_fds: stats.peak_open_data_fds,
        peak_data_workers: stats.peak_data_workers,
    })
}

fn validate_selected_paths(paths: &[PathBuf]) -> PocResult<()> {
    if paths.is_empty() || paths.len() > 64 {
        return Err(PocError::Integrity(
            "selected semantic path count is outside 1..=64".to_owned(),
        ));
    }
    let mut previous = None;
    for path in paths {
        let bytes = relative_path_bytes(path)?;
        if previous
            .as_ref()
            .is_some_and(|value: &Vec<u8>| value >= &bytes)
        {
            return Err(PocError::Integrity(
                "selected semantic paths must be unique and byte-sorted".to_owned(),
            ));
        }
        previous = Some(bytes);
    }
    Ok(())
}

fn relative_path_bytes(path: &Path) -> PocResult<Vec<u8>> {
    if path.is_absolute() {
        return Err(PocError::Integrity(
            "selected semantic path must be relative".to_owned(),
        ));
    }
    let bytes = path.as_os_str().as_bytes().to_vec();
    validate_path(&bytes, bytes.is_empty())?;
    Ok(bytes)
}

pub fn scan_tree(
    root: &Path,
    records: &mut BoundedSpool,
    hardlinks: &mut BoundedSpool,
) -> PocResult<ScanStats> {
    let queue_path = hardlinks.root().join("directory.queue");
    let mut queue = DirectoryQueue::create(queue_path)?;
    queue.push(&[])?;
    let worker_count = usize::from(MAX_DATA_WORKERS);
    let (sender, receiver) = std::sync::mpsc::sync_channel(worker_count.saturating_mul(2));
    let receiver = Mutex::new(receiver);
    let records_lock = Mutex::new(&mut *records);
    let hardlinks_lock = Mutex::new(&mut *hardlinks);
    let first_error = Mutex::new(None);
    let cancelled = AtomicBool::new(false);
    let bytes_read = AtomicU64::new(0);
    let mut stats = ScanStats {
        peak_open_data_fds: 5_usize.saturating_add(worker_count.saturating_sub(1)),
        peak_data_workers: MAX_DATA_WORKERS,
        ..ScanStats::default()
    };
    let traversal = std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            workers.push(scope.spawn(|| {
                scan_worker(
                    &receiver,
                    &records_lock,
                    &hardlinks_lock,
                    &cancelled,
                    &first_error,
                    &bytes_read,
                );
            }));
        }
        let result = traverse_tree(
            root,
            &mut queue,
            &sender,
            &records_lock,
            &hardlinks_lock,
            &cancelled,
            &mut stats,
        );
        drop(sender);
        for worker in workers {
            if worker.join().is_err() {
                record_first_error(
                    &first_error,
                    PocError::Integrity("semantic scan worker panicked".to_owned()),
                );
            }
        }
        result
    });
    traversal?;
    if let Some(error) = first_error
        .into_inner()
        .map_err(|_| PocError::Integrity("semantic scan error lock poisoned".to_owned()))?
    {
        return Err(error);
    }
    stats.bytes_read = bytes_read.load(Ordering::Relaxed);
    Ok(stats)
}

/// Scan a complete tree without creating worker threads.
///
/// The holder-namespace semantic snapshot runs inside the narrowly confined
/// storage-admin helper, whose post-bootstrap seccomp policy intentionally
/// denies process creation.  Full-tree regular-file scanning is already
/// sequential; this variant keeps that authority boundary intact while
/// preserving the exact record stream produced by `scan_tree`.
pub fn scan_tree_serial(
    root: &Path,
    records: &mut BoundedSpool,
    hardlinks: &mut BoundedSpool,
) -> PocResult<ScanStats> {
    let queue_path = hardlinks.root().join("directory.queue");
    let mut queue = DirectoryQueue::create(queue_path)?;
    queue.push(&[])?;
    let mut stats = ScanStats {
        peak_open_data_fds: 3,
        peak_data_workers: 1,
        ..ScanStats::default()
    };
    while let Some(relative) = queue.pop()? {
        let physical = physical_path(root, &relative);
        let metadata = std::fs::symlink_metadata(&physical)
            .map_err(|error| PocError::io("lstat semantic directory", &physical, error))?;
        if !metadata.file_type().is_dir() {
            return Err(PocError::Integrity(
                "semantic directory queue contains a non-directory".to_owned(),
            ));
        }
        let directory = scan_node(&physical, &relative, &metadata, records, hardlinks)?;
        stats.entry_count = stats.entry_count.saturating_add(1);
        let entries = std::fs::read_dir(&physical)
            .map_err(|error| PocError::io("read semantic directory", &physical, error))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| PocError::io("iterate semantic directory", &physical, error))?;
            let name = entry.file_name().into_vec();
            if name.is_empty() || name.contains(&b'/') || name.contains(&0) {
                return Err(PocError::Integrity(
                    "filesystem returned an invalid directory entry name".to_owned(),
                ));
            }
            if name == OPAQUE_MARKER {
                if !directory.opaque {
                    records.push_record(SemanticRecord::OpaqueDirectory {
                        path: relative.clone(),
                    })?;
                }
                continue;
            }
            if let Some(target) = name.strip_prefix(WHITEOUT_PREFIX) {
                if target.is_empty() {
                    return Err(PocError::Integrity(
                        "overlay whiteout marker has an empty target".to_owned(),
                    ));
                }
                records.push_record(SemanticRecord::Whiteout {
                    path: join_normalized(&relative, target)?,
                })?;
                stats.entry_count = stats.entry_count.saturating_add(1);
                continue;
            }
            let child = join_normalized(&relative, &name)?;
            let child_path = physical_path(root, &child);
            let child_type = entry
                .file_type()
                .map_err(|error| PocError::io("classify semantic entry", &child_path, error))?;
            if child_type.is_dir() {
                queue.push(&child)?;
                continue;
            }
            let metadata = std::fs::symlink_metadata(&child_path)
                .map_err(|error| PocError::io("lstat semantic entry", &child_path, error))?;
            if metadata.file_type().is_dir() {
                return Err(PocError::Integrity(
                    "semantic entry changed from non-directory to directory during scan".to_owned(),
                ));
            }
            let node = if is_kernel_whiteout(&metadata)? {
                records.push_record(SemanticRecord::Whiteout { path: child })?;
                NodeScan::default()
            } else {
                scan_node(&child_path, &child, &metadata, records, hardlinks)?
            };
            stats.entry_count = stats.entry_count.saturating_add(1);
            stats.bytes_read = stats.bytes_read.saturating_add(node.bytes_read);
            stats.peak_data_workers = stats.peak_data_workers.max(node.data_workers);
            stats.peak_open_data_fds = stats
                .peak_open_data_fds
                .max(2_usize.saturating_add(usize::from(node.data_workers)));
        }
    }
    Ok(stats)
}

struct ScanTask {
    physical: PathBuf,
    relative: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
struct NodeScan {
    bytes_read: u64,
    opaque: bool,
    data_workers: u16,
}

struct LockedSpool<'lock, 'spool> {
    inner: &'lock Mutex<&'spool mut BoundedSpool>,
}

impl SpoolSink for LockedSpool<'_, '_> {
    fn push(&mut self, key: Vec<u8>, payload: Vec<u8>) -> PocResult<()> {
        let mut spool = self
            .inner
            .lock()
            .map_err(|_| PocError::Integrity("semantic spool lock poisoned".to_owned()))?;
        spool.push(key, payload)
    }
}

fn traverse_tree(
    root: &Path,
    queue: &mut DirectoryQueue,
    sender: &SyncSender<ScanTask>,
    records_lock: &Mutex<&mut BoundedSpool>,
    hardlinks_lock: &Mutex<&mut BoundedSpool>,
    cancelled: &AtomicBool,
    stats: &mut ScanStats,
) -> PocResult<()> {
    let mut records = LockedSpool {
        inner: records_lock,
    };
    let mut hardlinks = LockedSpool {
        inner: hardlinks_lock,
    };
    while let Some(relative) = queue.pop()? {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        let physical = physical_path(root, &relative);
        let metadata = std::fs::symlink_metadata(&physical)
            .map_err(|error| PocError::io("lstat semantic directory", &physical, error))?;
        if !metadata.file_type().is_dir() {
            return Err(PocError::Integrity(
                "semantic directory queue contains a non-directory".to_owned(),
            ));
        }
        let directory = scan_node(
            &physical,
            &relative,
            &metadata,
            &mut records,
            &mut hardlinks,
        )?;
        stats.entry_count = stats.entry_count.saturating_add(1);
        let entries = std::fs::read_dir(&physical)
            .map_err(|error| PocError::io("read semantic directory", &physical, error))?;
        for entry in entries {
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            let entry = entry
                .map_err(|error| PocError::io("iterate semantic directory", &physical, error))?;
            let name = entry.file_name().into_vec();
            if name.is_empty() || name.contains(&b'/') || name.contains(&0) {
                return Err(PocError::Integrity(
                    "filesystem returned an invalid directory entry name".to_owned(),
                ));
            }
            if name == OPAQUE_MARKER {
                if !directory.opaque {
                    records.push_record(SemanticRecord::OpaqueDirectory {
                        path: relative.clone(),
                    })?;
                }
                continue;
            }
            if let Some(target) = name.strip_prefix(WHITEOUT_PREFIX) {
                if target.is_empty() {
                    return Err(PocError::Integrity(
                        "overlay whiteout marker has an empty target".to_owned(),
                    ));
                }
                records.push_record(SemanticRecord::Whiteout {
                    path: join_normalized(&relative, target)?,
                })?;
                stats.entry_count = stats.entry_count.saturating_add(1);
                continue;
            }
            let child = join_normalized(&relative, &name)?;
            let child_path = physical_path(root, &child);
            let child_type = entry
                .file_type()
                .map_err(|error| PocError::io("classify semantic entry", &child_path, error))?;
            if child_type.is_dir() {
                queue.push(&child)?;
            } else {
                sender
                    .send(ScanTask {
                        physical: child_path,
                        relative: child,
                    })
                    .map_err(|_| {
                        PocError::Integrity("semantic scan workers disconnected".to_owned())
                    })?;
                stats.entry_count = stats.entry_count.saturating_add(1);
            }
        }
    }
    Ok(())
}

fn scan_worker(
    receiver: &Mutex<Receiver<ScanTask>>,
    records_lock: &Mutex<&mut BoundedSpool>,
    hardlinks_lock: &Mutex<&mut BoundedSpool>,
    cancelled: &AtomicBool,
    first_error: &Mutex<Option<PocError>>,
    bytes_read: &AtomicU64,
) {
    loop {
        let task = match receiver.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => {
                record_first_error(
                    first_error,
                    PocError::Integrity("semantic scan receiver lock poisoned".to_owned()),
                );
                return;
            }
        };
        let Ok(task) = task else {
            return;
        };
        if cancelled.load(Ordering::Acquire) {
            continue;
        }
        let result = (|| {
            let metadata = std::fs::symlink_metadata(&task.physical)
                .map_err(|error| PocError::io("lstat semantic entry", &task.physical, error))?;
            if metadata.file_type().is_dir() {
                return Err(PocError::Integrity(
                    "semantic entry changed from non-directory to directory during scan".to_owned(),
                ));
            }
            let mut records = LockedSpool {
                inner: records_lock,
            };
            if is_kernel_whiteout(&metadata)? {
                records.push_record(SemanticRecord::Whiteout {
                    path: task.relative,
                })?;
                return Ok(NodeScan::default());
            }
            let mut hardlinks = LockedSpool {
                inner: hardlinks_lock,
            };
            scan_node(
                &task.physical,
                &task.relative,
                &metadata,
                &mut records,
                &mut hardlinks,
            )
        })();
        match result {
            Ok(node) => {
                bytes_read.fetch_add(node.bytes_read, Ordering::Relaxed);
            }
            Err(error) => {
                record_first_error(first_error, error);
                cancelled.store(true, Ordering::Release);
            }
        }
    }
}

fn record_first_error(slot: &Mutex<Option<PocError>>, error: PocError) {
    if let Ok(mut slot) = slot.lock() {
        if slot.is_none() {
            *slot = Some(error);
        }
    }
}

fn scan_node(
    physical: &Path,
    relative: &[u8],
    metadata: &std::fs::Metadata,
    records: &mut impl SpoolSink,
    hardlinks: &mut impl SpoolSink,
) -> PocResult<NodeScan> {
    let header = scan_node_header(physical, relative, metadata, records)?;
    if header.kind == NodeKind::Regular {
        let scanned = chunk::scan_regular(physical, relative, header.logical_size, records)?;
        if metadata.nlink() > 1 {
            append_hardlink_claim(
                hardlinks,
                metadata.dev(),
                metadata.ino(),
                relative,
                scanned.content_sha256,
            )?;
        }
        return Ok(NodeScan {
            bytes_read: scanned.bytes_read,
            opaque: header.opaque,
            data_workers: scanned.data_workers,
        });
    }
    Ok(NodeScan {
        bytes_read: 0,
        opaque: header.opaque,
        data_workers: 0,
    })
}

fn scan_selected_node(
    physical: &Path,
    relative: &[u8],
    metadata: &std::fs::Metadata,
    records: &mut impl SpoolSink,
) -> PocResult<NodeScan> {
    let header = scan_node_header(physical, relative, metadata, records)?;
    if header.kind != NodeKind::Regular {
        return Ok(NodeScan {
            bytes_read: 0,
            opaque: header.opaque,
            data_workers: 0,
        });
    }
    let scanned =
        chunk::scan_regular_selected_parallel(physical, relative, header.logical_size, records)?;
    Ok(NodeScan {
        bytes_read: scanned.bytes_read,
        opaque: header.opaque,
        data_workers: scanned.data_workers,
    })
}

struct NodeHeader {
    kind: NodeKind,
    logical_size: u64,
    opaque: bool,
}

fn scan_node_header(
    physical: &Path,
    relative: &[u8],
    metadata: &std::fs::Metadata,
    records: &mut impl SpoolSink,
) -> PocResult<NodeHeader> {
    let kind = node_kind(metadata)?;
    let symlink_target = if kind == NodeKind::Symlink {
        std::fs::read_link(physical)
            .map_err(|error| PocError::io("read semantic symlink", physical, error))?
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
    records.push_record(SemanticRecord::Node(NodeRecord {
        path: relative.to_vec(),
        kind,
        mode: metadata.permissions().mode() & 0o7777,
        uid: metadata.uid(),
        gid: metadata.gid(),
        mtime_seconds: metadata.mtime(),
        mtime_nanoseconds: u32::try_from(metadata.mtime_nsec()).map_err(|_| {
            PocError::Integrity(
                "filesystem returned an invalid negative mtime nanosecond".to_owned(),
            )
        })?,
        logical_size,
        symlink_target,
        device_major,
        device_minor,
    }))?;
    let opaque = scan_xattrs(physical, relative, records)?;
    if kind == NodeKind::Directory && opaque {
        records.push_record(SemanticRecord::OpaqueDirectory {
            path: relative.to_vec(),
        })?;
    }
    Ok(NodeHeader {
        kind,
        logical_size,
        opaque,
    })
}

fn scan_xattrs(physical: &Path, relative: &[u8], records: &mut impl SpoolSink) -> PocResult<bool> {
    let size = rustix::fs::llistxattr(physical, &mut []).map_err(|error| {
        PocError::io(
            "size semantic xattr list",
            physical,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    if size == 0 {
        return Ok(false);
    }
    if size > MAX_XATTR_LIST_BYTES {
        return Err(PocError::Integrity(
            "semantic xattr name list exceeds fixed bound".to_owned(),
        ));
    }
    let mut raw_list: Vec<libc::c_char> = vec![0; size];
    let listed = rustix::fs::llistxattr(physical, &mut raw_list).map_err(|error| {
        PocError::io(
            "read semantic xattr list",
            physical,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    raw_list.truncate(listed);
    let list = raw_list
        .into_iter()
        .map(|byte| byte.to_ne_bytes()[0])
        .collect::<Vec<_>>();
    let mut opaque = false;
    for name in list
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let os_name = OsStr::from_bytes(name);
        let value_size = rustix::fs::lgetxattr(physical, os_name, &mut []).map_err(|error| {
            PocError::io(
                "size semantic xattr value",
                physical,
                std::io::Error::from_raw_os_error(error.raw_os_error()),
            )
        })?;
        if value_size > super::record::MAX_XATTR_BYTES {
            return Err(PocError::Integrity(
                "semantic xattr value exceeds fixed bound".to_owned(),
            ));
        }
        let mut value = vec![0_u8; value_size];
        let read = rustix::fs::lgetxattr(physical, os_name, &mut value).map_err(|error| {
            PocError::io(
                "read semantic xattr value",
                physical,
                std::io::Error::from_raw_os_error(error.raw_os_error()),
            )
        })?;
        value.truncate(read);
        if OPAQUE_XATTRS.contains(&name) {
            opaque |= matches!(value.as_slice(), b"y" | b"Y" | b"1");
            continue;
        }
        if OVERLAY_INTERNAL_XATTRS.contains(&name) {
            continue;
        }
        records.push_record(SemanticRecord::Xattr {
            path: relative.to_vec(),
            name: name.to_vec(),
            value,
        })?;
    }
    Ok(opaque)
}

fn append_hardlink_claim(
    hardlinks: &mut impl SpoolSink,
    device: u64,
    inode: u64,
    path: &[u8],
    content_sha256: [u8; 32],
) -> PocResult<()> {
    let mut key = Vec::with_capacity(16 + path.len());
    key.extend_from_slice(&device.to_be_bytes());
    key.extend_from_slice(&inode.to_be_bytes());
    key.extend_from_slice(path);
    let mut payload = Vec::with_capacity(36 + path.len());
    payload.extend_from_slice(&content_sha256);
    payload.extend_from_slice(
        &u32::try_from(path.len())
            .map_err(|_| PocError::Integrity("hardlink path length overflow".to_owned()))?
            .to_be_bytes(),
    );
    payload.extend_from_slice(path);
    hardlinks.push(key, payload)
}

pub fn append_hardlink_records(
    records: &mut BoundedSpool,
    claims: SortedSpool,
    work_dir: &Path,
) -> PocResult<()> {
    let mut accumulator = HardlinkAccumulator::new(records, work_dir.join("hardlink-members.tmp"))?;
    claims.for_each(|key, payload| accumulator.accept(key, payload))?;
    accumulator.finish()
}

struct HardlinkAccumulator<'a> {
    records: &'a mut BoundedSpool,
    member_path: PathBuf,
    physical: Option<[u8; 16]>,
    content_sha256: [u8; 32],
    count: u64,
    group: Sha256,
    members: Option<BufWriter<File>>,
}

impl<'a> HardlinkAccumulator<'a> {
    fn new(records: &'a mut BoundedSpool, member_path: PathBuf) -> PocResult<Self> {
        let mut accumulator = Self {
            records,
            member_path,
            physical: None,
            content_sha256: [0; 32],
            count: 0,
            group: Sha256::new(),
            members: None,
        };
        accumulator.reset_file()?;
        Ok(accumulator)
    }

    fn accept(&mut self, key: &[u8], payload: &[u8]) -> PocResult<()> {
        if key.len() < 16 || payload.len() < 36 {
            return Err(PocError::Integrity("malformed hardlink claim".to_owned()));
        }
        let physical: [u8; 16] = key[..16]
            .try_into()
            .map_err(|_| PocError::Integrity("malformed hardlink identity".to_owned()))?;
        let content: [u8; 32] = payload[..32]
            .try_into()
            .map_err(|_| PocError::Integrity("malformed hardlink content".to_owned()))?;
        let path_length =
            usize::try_from(u32::from_be_bytes(payload[32..36].try_into().map_err(
                |_| PocError::Integrity("malformed hardlink path".to_owned()),
            )?))
            .map_err(|_| PocError::Integrity("hardlink path length overflow".to_owned()))?;
        let path = payload
            .get(36..)
            .filter(|path| path.len() == path_length)
            .ok_or_else(|| PocError::Integrity("malformed hardlink path payload".to_owned()))?;
        validate_path(path, true)?;
        if key.get(16..) != Some(path) {
            return Err(PocError::Integrity(
                "hardlink claim key and payload disagree".to_owned(),
            ));
        }
        if self.physical.is_some_and(|value| value != physical) {
            self.emit_group()?;
            self.reset_file()?;
        }
        if self.physical.is_none() {
            self.physical = Some(physical);
            self.content_sha256 = content;
            self.group = Sha256::new();
            self.group.update(b"mpla-poc-semantic-v1/hardlink-group\0");
            self.group.update(content);
        } else if self.content_sha256 != content {
            return Err(PocError::Integrity(
                "hardlink aliases produced different content digests".to_owned(),
            ));
        }
        self.group
            .update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_be_bytes());
        self.group.update(path);
        let writer = self
            .members
            .as_mut()
            .ok_or_else(|| PocError::Integrity("hardlink member spool is closed".to_owned()))?;
        writer
            .write_all(
                &u32::try_from(path.len())
                    .map_err(|_| PocError::Integrity("hardlink path overflow".to_owned()))?
                    .to_be_bytes(),
            )
            .and_then(|()| writer.write_all(path))
            .map_err(|error| {
                PocError::io("write hardlink member spool", &self.member_path, error)
            })?;
        self.count = self.count.saturating_add(1);
        Ok(())
    }

    fn finish(mut self) -> PocResult<()> {
        self.emit_group()?;
        match std::fs::remove_file(&self.member_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PocError::io(
                "remove hardlink member spool",
                &self.member_path,
                error,
            )),
        }
    }

    fn emit_group(&mut self) -> PocResult<()> {
        let Some(_) = self.physical else {
            return Ok(());
        };
        let mut writer = self
            .members
            .take()
            .ok_or_else(|| PocError::Integrity("hardlink member spool is closed".to_owned()))?;
        writer.flush().map_err(|error| {
            PocError::io("flush hardlink member spool", &self.member_path, error)
        })?;
        writer.get_ref().sync_all().map_err(|error| {
            PocError::io("fsync hardlink member spool", &self.member_path, error)
        })?;
        if self.count >= 2 {
            let group_sha256: [u8; 32] = self.group.clone().finalize().into();
            self.records.push_record(SemanticRecord::HardlinkGroup {
                group_sha256,
                content_sha256: self.content_sha256,
                member_count: self.count,
            })?;
            let file = File::open(&self.member_path).map_err(|error| {
                PocError::io("open hardlink member spool", &self.member_path, error)
            })?;
            let mut reader = BufReader::new(file);
            while let Some(length) = read_u32_or_eof(&mut reader).map_err(|error| {
                PocError::io("read hardlink member spool", &self.member_path, error)
            })? {
                let length = usize::try_from(length)
                    .map_err(|_| PocError::Integrity("hardlink path overflow".to_owned()))?;
                if length > MAX_PATH_BYTES {
                    return Err(PocError::Integrity(
                        "hardlink member path exceeds bound".to_owned(),
                    ));
                }
                let mut path = vec![0_u8; length];
                reader.read_exact(&mut path).map_err(|error| {
                    PocError::io("read hardlink member path", &self.member_path, error)
                })?;
                self.records
                    .push_record(SemanticRecord::HardlinkMember { group_sha256, path })?;
            }
        }
        self.physical = None;
        self.count = 0;
        Ok(())
    }

    fn reset_file(&mut self) -> PocResult<()> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.member_path)
            .map_err(|error| {
                PocError::io("create hardlink member spool", &self.member_path, error)
            })?;
        self.members = Some(BufWriter::new(file));
        Ok(())
    }
}

struct DirectoryQueue {
    path: PathBuf,
    file: File,
    read_offset: u64,
    write_offset: u64,
}

impl DirectoryQueue {
    fn create(path: PathBuf) -> PocResult<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| PocError::io("create semantic directory queue", &path, error))?;
        file.write_all_at(QUEUE_MAGIC, 0)
            .map_err(|error| PocError::io("write semantic directory queue", &path, error))?;
        Ok(Self {
            path,
            file,
            read_offset: 8,
            write_offset: 8,
        })
    }

    fn push(&mut self, path: &[u8]) -> PocResult<()> {
        validate_path(path, true)?;
        let length = u32::try_from(path.len())
            .map_err(|_| PocError::Integrity("semantic queue path overflow".to_owned()))?;
        write_all_at(
            &self.file,
            &length.to_be_bytes(),
            self.write_offset,
            &self.path,
        )?;
        self.write_offset = self.write_offset.saturating_add(4);
        write_all_at(&self.file, path, self.write_offset, &self.path)?;
        self.write_offset = self
            .write_offset
            .saturating_add(u64::try_from(path.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    fn pop(&mut self) -> PocResult<Option<Vec<u8>>> {
        if self.read_offset == self.write_offset {
            return Ok(None);
        }
        let mut length = [0_u8; 4];
        read_exact_at(&self.file, &mut length, self.read_offset, &self.path)?;
        self.read_offset = self.read_offset.saturating_add(4);
        let length = usize::try_from(u32::from_be_bytes(length))
            .map_err(|_| PocError::Integrity("semantic queue path overflow".to_owned()))?;
        if length > MAX_PATH_BYTES {
            return Err(PocError::Integrity(
                "semantic queue path exceeds bound".to_owned(),
            ));
        }
        let mut path = vec![0_u8; length];
        read_exact_at(&self.file, &mut path, self.read_offset, &self.path)?;
        self.read_offset = self
            .read_offset
            .saturating_add(u64::try_from(length).unwrap_or(u64::MAX));
        validate_path(&path, true)?;
        Ok(Some(path))
    }
}

fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64, path: &Path) -> PocResult<()> {
    let mut filled = 0;
    while filled < bytes.len() {
        let position = offset.saturating_add(u64::try_from(filled).unwrap_or(u64::MAX));
        let count = file
            .read_at(&mut bytes[filled..], position)
            .map_err(|error| PocError::io("read semantic directory queue", path, error))?;
        if count == 0 {
            return Err(PocError::Integrity(
                "truncated semantic directory queue".to_owned(),
            ));
        }
        filled += count;
    }
    Ok(())
}

fn write_all_at(file: &File, bytes: &[u8], offset: u64, path: &Path) -> PocResult<()> {
    let mut written = 0;
    while written < bytes.len() {
        let position = offset.saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        let count = file
            .write_at(&bytes[written..], position)
            .map_err(|error| PocError::io("write semantic directory queue", path, error))?;
        if count == 0 {
            return Err(PocError::Integrity(
                "semantic directory queue write made no progress".to_owned(),
            ));
        }
        written += count;
    }
    Ok(())
}

fn read_u32_or_eof(reader: &mut impl Read) -> std::io::Result<Option<u32>> {
    let mut bytes = [0_u8; 4];
    let mut filled = 0;
    while filled < bytes.len() {
        let count = reader.read(&mut bytes[filled..])?;
        if count == 0 {
            if filled == 0 {
                return Ok(None);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "partial hardlink member frame",
            ));
        }
        filled += count;
    }
    Ok(Some(u32::from_be_bytes(bytes)))
}

fn node_kind(metadata: &std::fs::Metadata) -> PocResult<NodeKind> {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        Ok(NodeKind::Regular)
    } else if file_type.is_dir() {
        Ok(NodeKind::Directory)
    } else if file_type.is_symlink() {
        Ok(NodeKind::Symlink)
    } else if file_type.is_fifo() {
        Ok(NodeKind::Fifo)
    } else if file_type.is_char_device() {
        Ok(NodeKind::CharacterDevice)
    } else if file_type.is_block_device() {
        Ok(NodeKind::BlockDevice)
    } else if file_type.is_socket() {
        Ok(NodeKind::Socket)
    } else {
        Err(PocError::Unsupported(
            "unsupported filesystem node type in semantic tree".to_owned(),
        ))
    }
}

fn is_kernel_whiteout(metadata: &std::fs::Metadata) -> PocResult<bool> {
    Ok(metadata.file_type().is_char_device() && device_components(metadata)? == (0, 0))
}

fn device_components(metadata: &std::fs::Metadata) -> PocResult<(u32, u32)> {
    let device: rustix::fs::Dev = metadata.rdev().try_into().map_err(|_| {
        PocError::Integrity("device identifier does not fit the platform dev_t".to_owned())
    })?;
    Ok((rustix::fs::major(device), rustix::fs::minor(device)))
}

fn join_normalized(parent: &[u8], name: &[u8]) -> PocResult<Vec<u8>> {
    let mut path = Vec::with_capacity(parent.len().saturating_add(name.len()).saturating_add(1));
    if !parent.is_empty() {
        path.extend_from_slice(parent);
        path.push(b'/');
    }
    path.extend_from_slice(name);
    validate_path(&path, false)?;
    Ok(path)
}

fn physical_path(root: &Path, relative: &[u8]) -> PathBuf {
    if relative.is_empty() {
        root.to_path_buf()
    } else {
        root.join(std::ffi::OsString::from_vec(relative.to_vec()))
    }
}
