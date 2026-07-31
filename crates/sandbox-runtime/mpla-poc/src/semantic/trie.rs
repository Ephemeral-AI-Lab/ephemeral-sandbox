use std::collections::{HashSet, VecDeque};
#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{AttributionInput, AttributionRootId, CanonicalRootPair, PocError, PocResult, RootId};

use super::attribution;
use super::record::{digest_key, RecordMutation, SemanticRecord, MAX_KEY_BYTES, MAX_RECORD_BYTES};
use super::spool::{BoundedSpool, SortedSpool};

const CONTENT_NODE_MAGIC: &[u8; 8] = b"MPLACND1";
const ATTR_NODE_MAGIC: &[u8; 8] = b"MPLAAND1";
const CONTENT_LEAF_MAGIC: &[u8; 8] = b"MPLACLE1";
const ATTR_LEAF_MAGIC: &[u8; 8] = b"MPLAALE1";
const OBJECT_DOMAIN: &[u8] = b"mpla-poc-semantic-v1/object\0";
const MAX_OBJECT_BYTES: u64 = 320 * 1024;
const TRIE_DEPTH: usize = 64;
const FAN_OUT: usize = 16;
const MAX_CACHED_EXISTING_OBJECTS: usize = 64 * 1024;
const MAX_CACHED_INSTALLED_OBJECTS: usize = 4 * 1024;
const MAX_INCREMENTAL_STAGE_BYTES: u64 = 640 * 1024 * 1024;
const DIGEST_SORT_MEMORY_BYTES: usize = 256 * 1024;
const PACK_MAGIC: &[u8; 8] = b"MPLAPAK1";
const PACK_INDEX_MAGIC: &[u8; 8] = b"MPLAIDX1";
const PACK_INDEX_ENTRY_BYTES: usize = 44;
const MAX_PACK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PACK_OBJECTS: usize = 64 * 1024;
const MAX_PACK_INDEX_BYTES: u64 = 4 * 1024 * 1024;
const PACK_WRITE_BUFFER_BYTES: usize = 64 * 1024;
const PACK_BLOOM_BITS: usize = 64 * 1024 * 8;
const PACK_BLOOM_WORDS: usize = PACK_BLOOM_BITS / u64::BITS as usize;
const PACK_INDEX_CACHE_PAGE_BYTES: usize = 64 * 1024;
const PACK_INDEX_CACHE_PAGES: usize = 20;
const ACTIVE_PACK_READERS: usize = 2;
// Incremental mutation revisits immutable compressed-trie nodes while it
// rebuilds adjacent branches. Keep the verified object bytes in a small,
// bounded LRU so those revisits do not each reopen, re-index, and re-read a
// pack. This is deliberately far below the semantic 8 MiB managed cache
// ceiling and never caches mutable staged objects.
const PACKED_OBJECT_CACHE_BYTES: usize = 512 * 1024;
// The gateway serves a sequence of publications from one long-lived process.
// Retain a small set of already digest-verified immutable objects across those
// requests so a repeated edit does not reopen the same source packs.  This is
// an optimization only: a miss follows the normal validated on-disk path and
// staged (therefore mutable) objects are always checked first.
const DAEMON_VERIFIED_OBJECT_CACHE_BYTES: usize = 1024 * 1024;
// Source publications are immutable after their root manifest is installed.
// Keeping their validated pack catalogs in the long-lived gateway avoids
// reopening and checking every historical pack for each subsequent edit. The
// per-request index-page budget is reduced by the same amount, so the existing
// 8 MiB managed-cache ceiling is unchanged.
const DAEMON_READ_ONLY_PACK_CATALOG_CACHE_BYTES: usize = 768 * 1024;
const PACK_CATALOG_FILE: &str = "catalog-v1";
const PACK_CATALOG_MAGIC: &[u8; 8] = b"MPLACAT1";
const PACK_CATALOG_VERSION: u32 = 1;
const PACK_CATALOG_HEADER_BYTES: usize = 16;
const PACK_CATALOG_TRAILER_BYTES: usize = 32;
const PACK_CATALOG_ENTRY_BYTES: usize = 16 + 4 + 8 + 8 + 32 + PACK_BLOOM_WORDS * 8;
const MAX_PACK_CATALOG_BYTES: usize = 16 * 1024 * 1024;
const STAGE_MEMORY_SLOT_COUNT: usize = 48 * 1024;
const STAGE_MEMORY_SLOT_EMPTY: u64 = u64::MAX;
const STAGE_DISK_SLOT_COUNT: usize = 1024 * 1024;
const STAGE_SLOT_BYTES: usize = 40;
const STAGE_MEMORY_OBJECT_BYTES: usize = 512 * 1024;
const INCREMENTAL_STAGE_TRANSIENT_BYTES: usize = 2 * 1024 * 1024;
const STAGE_MEMORY_INDEX_BYTES: usize = STAGE_MEMORY_SLOT_COUNT * STAGE_SLOT_BYTES;
const PACK_LOOKUP_CACHE_BYTES: usize =
    PACK_INDEX_CACHE_PAGE_BYTES * PACK_INDEX_CACHE_PAGES + PACKED_OBJECT_CACHE_BYTES;
pub(super) const INCREMENTAL_PEAK_DATA_FDS: usize = 14;
pub(super) const INCREMENTAL_DATA_WORKERS: u16 = 1;
pub(super) const EXISTING_OBJECT_CACHE_BYTES: usize = 8 * 1024 * 1024;

const _: () = assert!(std::mem::size_of::<MemoryStageSlot>() == STAGE_SLOT_BYTES);
const _: () = assert!(
    STAGE_MEMORY_INDEX_BYTES
        + STAGE_MEMORY_OBJECT_BYTES
        + PACK_LOOKUP_CACHE_BYTES
        + INCREMENTAL_STAGE_TRANSIENT_BYTES
        + DAEMON_VERIFIED_OBJECT_CACHE_BYTES
        + DAEMON_READ_ONLY_PACK_CATALOG_CACHE_BYTES
        <= EXISTING_OBJECT_CACHE_BYTES
);

#[cfg(test)]
static FULL_PACK_INDEX_VALIDATIONS: AtomicUsize = AtomicUsize::new(0);

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn new(class: &str, canonical_root: &Path) -> PocResult<Self> {
        let parent = canonical_root.parent().ok_or_else(|| {
            PocError::Integrity("semantic canonical root has no staging parent".to_owned())
        })?;
        let path = parent.join(format!(".{class}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path)
            .map_err(|error| PocError::io("create semantic temporary directory", &path, error))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
}

struct TemporaryFile {
    path: PathBuf,
    armed: bool,
}

#[cfg(target_os = "linux")]
fn install_temporary_no_replace(temporary: &Path, path: &Path) -> std::io::Result<()> {
    let temporary = CString::new(temporary.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: both paths are live NUL-terminated C strings for this syscall.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            temporary.as_ptr(),
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn install_temporary_no_replace(temporary: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::hard_link(temporary, path)
}

impl TemporaryFile {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn remove(&mut self, context: &'static str) -> PocResult<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {
                self.armed = false;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.armed = false;
                Ok(())
            }
            Err(error) => Err(PocError::io(context, &self.path, error)),
        }
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if self.armed {
            match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {}
            }
        }
    }
}

struct DigestJournal {
    _temporary: TemporaryDirectory,
    path: PathBuf,
    file: Option<File>,
}

impl DigestJournal {
    fn new(canonical_root: &Path) -> PocResult<Self> {
        let temporary = TemporaryDirectory::new("eos-mpla-object-set", canonical_root)?;
        let path = temporary.path().join("digests");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| PocError::io("create semantic digest journal", &path, error))?;
        Ok(Self {
            _temporary: temporary,
            path,
            file: Some(file),
        })
    }

    fn append(&mut self, digest: [u8; 32]) -> PocResult<()> {
        self.file
            .as_mut()
            .ok_or_else(|| {
                PocError::Integrity("semantic digest journal is already closed".to_owned())
            })?
            .write_all(&digest)
            .map_err(|error| PocError::io("append semantic digest journal", &self.path, error))
    }

    fn close(&mut self) {
        drop(self.file.take());
    }

    fn spool_into(&self, spool: &mut BoundedSpool) -> PocResult<()> {
        let mut file = File::open(&self.path)
            .map_err(|error| PocError::io("open semantic digest journal", &self.path, error))?;
        loop {
            let mut digest = [0_u8; 32];
            if !read_exact_or_eof(&mut file, &mut digest)
                .map_err(|error| PocError::io("read semantic digest journal", &self.path, error))?
            {
                return Ok(());
            }
            spool.push(digest.to_vec(), vec![1])?;
        }
    }
}

struct IncrementalStage {
    temporary: TemporaryDirectory,
    total_bytes: u64,
    data_bytes: u64,
    writer: File,
    reader: File,
    memory: Option<MemoryStage>,
    disk_index: Option<StageDiskIndex>,
}

struct MemoryStage {
    data: Vec<u8>,
    slots: Vec<MemoryStageSlot>,
    object_count: usize,
}

#[derive(Clone, Copy)]
struct MemoryStageSlot {
    digest: [u8; 32],
    offset: u64,
}

struct StageDiskIndex {
    writer: File,
    reader: File,
}

#[derive(Clone, Copy)]
struct StageSlot {
    digest: [u8; 32],
    offset: u64,
}

#[derive(Clone)]
struct PackedObjectEntry {
    digest: [u8; 32],
    offset: u64,
    length: u32,
}

#[derive(Clone)]
struct PackedObjectIndex {
    pack: PathBuf,
    index: PathBuf,
    count: usize,
    entries_end: u64,
    index_checksum: [u8; 32],
    pack_bytes: u64,
    bloom: Arc<[u64]>,
}

#[derive(Clone, Eq, PartialEq)]
struct PackCatalogIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

struct ReadOnlyPackCatalogCacheEntry {
    directory: PathBuf,
    identity: PackCatalogIdentity,
    indexes: Vec<PackedObjectIndex>,
    bytes: usize,
}

struct ReadOnlyPackCatalogCache {
    capacity_bytes: usize,
    bytes: usize,
    entries: VecDeque<ReadOnlyPackCatalogCacheEntry>,
}

impl ReadOnlyPackCatalogCache {
    const fn new(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes,
            bytes: 0,
            entries: VecDeque::new(),
        }
    }

    fn load(
        &mut self,
        directory: &Path,
        identity: &PackCatalogIdentity,
    ) -> Option<Vec<PackedObjectIndex>> {
        let position = self
            .entries
            .iter()
            .position(|entry| entry.directory == directory && entry.identity == *identity)?;
        let entry = self.entries.remove(position)?;
        let indexes = entry.indexes.clone();
        self.entries.push_back(entry);
        Some(indexes)
    }

    fn store(
        &mut self,
        directory: &Path,
        identity: PackCatalogIdentity,
        indexes: &[PackedObjectIndex],
    ) {
        let bytes = indexes.len().saturating_mul(PACK_CATALOG_ENTRY_BYTES);
        if indexes.is_empty() || bytes > self.capacity_bytes {
            return;
        }
        if let Some(position) = self
            .entries
            .iter()
            .position(|entry| entry.directory == directory)
        {
            if let Some(previous) = self.entries.remove(position) {
                self.bytes = self.bytes.saturating_sub(previous.bytes);
            }
        }
        while self.bytes.saturating_add(bytes) > self.capacity_bytes {
            let Some(evicted) = self.entries.pop_front() else {
                return;
            };
            self.bytes = self.bytes.saturating_sub(evicted.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.push_back(ReadOnlyPackCatalogCacheEntry {
            directory: directory.to_path_buf(),
            identity,
            indexes: indexes.to_vec(),
            bytes,
        });
    }
}

struct PackIndexCachePage {
    path: PathBuf,
    offset: u64,
    bytes: Box<[u8]>,
}

struct PackedObjectCacheEntry {
    digest: [u8; 32],
    bytes: Box<[u8]>,
}

struct VerifiedObjectCache {
    capacity_bytes: usize,
    bytes: usize,
    entries: VecDeque<PackedObjectCacheEntry>,
}

impl VerifiedObjectCache {
    const fn new(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes,
            bytes: 0,
            entries: VecDeque::new(),
        }
    }

    fn load(&mut self, digest: [u8; 32]) -> Option<Vec<u8>> {
        let position = self
            .entries
            .iter()
            .position(|entry| entry.digest == digest)?;
        let entry = self.entries.remove(position)?;
        let bytes = entry.bytes.to_vec();
        self.entries.push_back(entry);
        Some(bytes)
    }

    fn store(&mut self, digest: [u8; 32], bytes: &[u8]) {
        if bytes.is_empty() || bytes.len() > self.capacity_bytes {
            return;
        }
        if let Some(position) = self.entries.iter().position(|entry| entry.digest == digest) {
            let previous = self.entries.remove(position);
            if let Some(previous) = previous {
                self.bytes = self.bytes.saturating_sub(previous.bytes.len());
            }
        }
        while self.bytes.saturating_add(bytes.len()) > self.capacity_bytes {
            let Some(evicted) = self.entries.pop_front() else {
                return;
            };
            self.bytes = self.bytes.saturating_sub(evicted.bytes.len());
        }
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.entries.push_back(PackedObjectCacheEntry {
            digest,
            bytes: bytes.into(),
        });
    }
}

fn daemon_verified_object_cache() -> &'static Mutex<VerifiedObjectCache> {
    static CACHE: OnceLock<Mutex<VerifiedObjectCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VerifiedObjectCache::new(DAEMON_VERIFIED_OBJECT_CACHE_BYTES)))
}

fn daemon_read_only_pack_catalog_cache() -> &'static Mutex<ReadOnlyPackCatalogCache> {
    static CACHE: OnceLock<Mutex<ReadOnlyPackCatalogCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(ReadOnlyPackCatalogCache::new(
            DAEMON_READ_ONLY_PACK_CATALOG_CACHE_BYTES,
        ))
    })
}

fn load_daemon_read_only_pack_catalog(
    directory: &Path,
    identity: &PackCatalogIdentity,
) -> Option<Vec<PackedObjectIndex>> {
    let mut cache = daemon_read_only_pack_catalog_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.load(directory, identity)
}

fn cache_daemon_read_only_pack_catalog(
    directory: &Path,
    identity: PackCatalogIdentity,
    indexes: &[PackedObjectIndex],
) {
    let mut cache = daemon_read_only_pack_catalog_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.store(directory, identity, indexes);
}

fn load_daemon_verified_object(digest: [u8; 32]) -> Option<Vec<u8>> {
    let mut cache = daemon_verified_object_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.load(digest)
}

fn cache_daemon_verified_object(digest: [u8; 32], bytes: &[u8]) {
    let mut cache = daemon_verified_object_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.store(digest, bytes);
}

struct ActivePackReader {
    path: PathBuf,
    pack_bytes: u64,
    file: File,
}

impl IncrementalStage {
    fn new(canonical_root: &Path) -> PocResult<Self> {
        let temporary = TemporaryDirectory::new("eos-mpla-incremental-stage", canonical_root)?;
        let data_path = temporary.path().join("objects.stage");
        let writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&data_path)
            .map_err(|error| {
                PocError::io("create incremental semantic stage", &data_path, error)
            })?;
        let reader = File::open(&data_path)
            .map_err(|error| PocError::io("open incremental semantic stage", &data_path, error))?;
        Ok(Self {
            temporary,
            total_bytes: 0,
            data_bytes: 0,
            writer,
            reader,
            memory: Some(MemoryStage {
                data: Vec::with_capacity(STAGE_MEMORY_OBJECT_BYTES),
                slots: vec![
                    MemoryStageSlot {
                        digest: [0_u8; 32],
                        offset: STAGE_MEMORY_SLOT_EMPTY,
                    };
                    STAGE_MEMORY_SLOT_COUNT
                ],
                object_count: 0,
            }),
            disk_index: None,
        })
    }

    fn contains(&self, digest: &[u8; 32]) -> PocResult<bool> {
        if let Some(memory) = &self.memory {
            return Ok(Self::find_memory_offset(memory, digest).is_some());
        }
        Ok(self.find_disk_offset(digest)?.is_some())
    }

    fn store(&mut self, digest: [u8; 32], bytes: &[u8]) -> PocResult<()> {
        let byte_count = u64::try_from(bytes.len())
            .map_err(|_| PocError::Integrity("semantic staged object size overflow".to_owned()))?;
        let length = u32::try_from(bytes.len()).map_err(|_| {
            PocError::Integrity("incremental semantic staged object exceeds u32".to_owned())
        })?;
        if length == 0 || u64::from(length) > MAX_OBJECT_BYTES {
            return Err(PocError::Integrity(
                "incremental semantic staged object is outside its fixed bound".to_owned(),
            ));
        }
        if let Some(memory) = &mut self.memory {
            if let Some(offset) = Self::find_memory_offset(memory, &digest) {
                if Self::memory_object_bytes(memory, offset, digest)? == bytes {
                    return Ok(());
                }
                return Err(PocError::Integrity(
                    "semantic staged object digest collision".to_owned(),
                ));
            }
        } else if let Some(offset) = self.find_disk_offset(&digest)? {
            if self.read_object(offset, digest)? == bytes {
                return Ok(());
            }
            return Err(PocError::Integrity(
                "semantic staged object digest collision".to_owned(),
            ));
        }
        let total_bytes = self
            .total_bytes
            .checked_add(byte_count)
            .ok_or_else(|| PocError::Integrity("semantic staging size overflow".to_owned()))?;
        if total_bytes > MAX_INCREMENTAL_STAGE_BYTES {
            return Err(PocError::Integrity(
                "incremental semantic staging exceeds its disk bound".to_owned(),
            ));
        }
        let record_bytes = usize::try_from(length)
            .map_err(|_| {
                PocError::Integrity(
                    "incremental semantic stage record size overflows usize".to_owned(),
                )
            })?
            .checked_add(36)
            .ok_or_else(|| {
                PocError::Integrity("incremental semantic stage record size overflow".to_owned())
            })?;
        if let Some(memory) = &mut self.memory {
            if memory.object_count < STAGE_MEMORY_SLOT_COUNT
                && memory
                    .data
                    .len()
                    .checked_add(record_bytes)
                    .is_some_and(|size| size <= STAGE_MEMORY_OBJECT_BYTES)
            {
                let offset = u64::try_from(memory.data.len()).map_err(|_| {
                    PocError::Integrity("incremental semantic stage offset overflow".to_owned())
                })?;
                memory.data.extend_from_slice(&digest);
                memory.data.extend_from_slice(&length.to_be_bytes());
                memory.data.extend_from_slice(bytes);
                Self::insert_memory_offset(memory, digest, offset)?;
                self.data_bytes = u64::try_from(memory.data.len()).map_err(|_| {
                    PocError::Integrity("incremental semantic stage offset overflow".to_owned())
                })?;
                self.total_bytes = total_bytes;
                return Ok(());
            }
        }
        self.spill_memory()?;
        let offset = self.data_bytes;
        self.writer
            .write_all(&digest)
            .and_then(|()| self.writer.write_all(&length.to_be_bytes()))
            .and_then(|()| self.writer.write_all(bytes))
            .map_err(|error| {
                PocError::io(
                    "append incremental semantic staged object",
                    self.temporary.path(),
                    error,
                )
            })?;
        self.insert_disk_offset(digest, offset)?;
        self.data_bytes = self
            .data_bytes
            .checked_add(36)
            .and_then(|value| value.checked_add(u64::from(length)))
            .ok_or_else(|| {
                PocError::Integrity("incremental semantic stage offset overflow".to_owned())
            })?;
        self.total_bytes = total_bytes;
        Ok(())
    }

    fn load(&self, digest: [u8; 32]) -> PocResult<Option<Vec<u8>>> {
        if let Some(memory) = &self.memory {
            return Self::find_memory_offset(memory, &digest)
                .map(|offset| {
                    Self::memory_object_bytes(memory, offset, digest).map(ToOwned::to_owned)
                })
                .transpose();
        }
        self.find_disk_offset(&digest)?
            .map(|offset| self.read_object(offset, digest))
            .transpose()
    }

    fn find_memory_offset(memory: &MemoryStage, digest: &[u8; 32]) -> Option<u64> {
        let mut slot_index = stage_memory_slot_index(digest);
        for _ in 0..STAGE_MEMORY_SLOT_COUNT {
            let slot = memory.slots[slot_index];
            if slot.offset == STAGE_MEMORY_SLOT_EMPTY {
                return None;
            }
            if slot.digest == *digest {
                return Some(slot.offset);
            }
            slot_index = (slot_index + 1) % STAGE_MEMORY_SLOT_COUNT;
        }
        None
    }

    fn insert_memory_offset(
        memory: &mut MemoryStage,
        digest: [u8; 32],
        offset: u64,
    ) -> PocResult<()> {
        let mut slot_index = stage_memory_slot_index(&digest);
        for _ in 0..STAGE_MEMORY_SLOT_COUNT {
            if memory.slots[slot_index].offset == STAGE_MEMORY_SLOT_EMPTY {
                memory.slots[slot_index] = MemoryStageSlot { digest, offset };
                memory.object_count = memory.object_count.saturating_add(1);
                return Ok(());
            }
            slot_index = (slot_index + 1) % STAGE_MEMORY_SLOT_COUNT;
        }
        Err(PocError::Integrity(
            "incremental semantic memory stage object count exceeds its bounded index".to_owned(),
        ))
    }

    fn spill_memory(&mut self) -> PocResult<()> {
        let Some(memory) = self.memory.take() else {
            return Ok(());
        };
        self.disk_index = Some(StageDiskIndex::new(self.temporary.path())?);
        self.writer.write_all(&memory.data).map_err(|error| {
            PocError::io(
                "spill incremental semantic staged objects",
                self.temporary.path(),
                error,
            )
        })?;
        self.data_bytes = u64::try_from(memory.data.len()).map_err(|_| {
            PocError::Integrity("incremental semantic stage offset overflow".to_owned())
        })?;
        for slot in memory.slots {
            if slot.offset != STAGE_MEMORY_SLOT_EMPTY {
                self.insert_disk_offset(slot.digest, slot.offset)?;
            }
        }
        Ok(())
    }

    fn memory_object_bytes<'a>(
        memory: &'a MemoryStage,
        offset: u64,
        expected: [u8; 32],
    ) -> PocResult<&'a [u8]> {
        let offset = usize::try_from(offset).map_err(|_| {
            PocError::Integrity(
                "incremental semantic staged object offset overflows usize".to_owned(),
            )
        })?;
        let header_end = offset.checked_add(36).ok_or_else(|| {
            PocError::Integrity("incremental semantic staged object header overflows".to_owned())
        })?;
        let header = memory.data.get(offset..header_end).ok_or_else(|| {
            PocError::Integrity("incremental semantic staged object is truncated".to_owned())
        })?;
        let digest: [u8; 32] = header[..32].try_into().map_err(|_| {
            PocError::Integrity("incremental semantic staged object digest is truncated".to_owned())
        })?;
        let length = u32::from_be_bytes(header[32..36].try_into().map_err(|_| {
            PocError::Integrity("incremental semantic staged object length is truncated".to_owned())
        })?);
        if digest != expected || length == 0 || u64::from(length) > MAX_OBJECT_BYTES {
            return Err(PocError::Integrity(
                "incremental semantic staged object is invalid".to_owned(),
            ));
        }
        let length = usize::try_from(length).map_err(|_| {
            PocError::Integrity(
                "incremental semantic staged object length overflows usize".to_owned(),
            )
        })?;
        let bytes_end = header_end.checked_add(length).ok_or_else(|| {
            PocError::Integrity(
                "incremental semantic staged object length overflows usize".to_owned(),
            )
        })?;
        let bytes = memory.data.get(header_end..bytes_end).ok_or_else(|| {
            PocError::Integrity("incremental semantic staged object is truncated".to_owned())
        })?;
        if object_digest(bytes) != expected {
            return Err(PocError::Integrity(
                "incremental semantic staged object digest mismatch".to_owned(),
            ));
        }
        Ok(bytes)
    }

    fn find_disk_offset(&self, digest: &[u8; 32]) -> PocResult<Option<u64>> {
        let mut slot_index = stage_disk_slot_index(digest);
        for _ in 0..STAGE_DISK_SLOT_COUNT {
            let Some(slot) = self.read_disk_slot(slot_index)? else {
                return Ok(None);
            };
            if slot.digest == *digest {
                return Ok(Some(slot.offset));
            }
            slot_index = (slot_index + 1) % STAGE_DISK_SLOT_COUNT;
        }
        Err(PocError::Integrity(
            "incremental semantic stage object count exceeds its bounded index".to_owned(),
        ))
    }

    fn insert_disk_offset(&mut self, digest: [u8; 32], offset: u64) -> PocResult<()> {
        let mut slot_index = stage_disk_slot_index(&digest);
        for _ in 0..STAGE_DISK_SLOT_COUNT {
            if self.read_disk_slot(slot_index)?.is_none() {
                self.write_disk_slot(slot_index, StageSlot { digest, offset })?;
                return Ok(());
            }
            slot_index = (slot_index + 1) % STAGE_DISK_SLOT_COUNT;
        }
        Err(PocError::Integrity(
            "incremental semantic stage object count exceeds its bounded index".to_owned(),
        ))
    }

    fn read_disk_slot(&self, index: usize) -> PocResult<Option<StageSlot>> {
        let disk_index = self.disk_index.as_ref().ok_or_else(|| {
            PocError::Integrity("incremental semantic disk stage is absent".to_owned())
        })?;
        let offset = stage_disk_slot_offset(index)?;
        let mut bytes = [0_u8; STAGE_SLOT_BYTES];
        read_exact_at(
            &disk_index.reader,
            &mut bytes,
            offset,
            self.temporary.path(),
        )?;
        let encoded_offset = u64::from_be_bytes(bytes[..8].try_into().map_err(|_| {
            PocError::Integrity("incremental semantic stage index offset is truncated".to_owned())
        })?);
        if encoded_offset == 0 {
            return Ok(None);
        }
        let digest: [u8; 32] = bytes[8..].try_into().map_err(|_| {
            PocError::Integrity("incremental semantic stage index digest is truncated".to_owned())
        })?;
        Ok(Some(StageSlot {
            digest,
            offset: encoded_offset.saturating_sub(1),
        }))
    }

    fn write_disk_slot(&self, index: usize, slot: StageSlot) -> PocResult<()> {
        let disk_index = self.disk_index.as_ref().ok_or_else(|| {
            PocError::Integrity("incremental semantic disk stage is absent".to_owned())
        })?;
        let encoded_offset = slot.offset.checked_add(1).ok_or_else(|| {
            PocError::Integrity("incremental semantic stage index offset overflow".to_owned())
        })?;
        let mut bytes = [0_u8; STAGE_SLOT_BYTES];
        bytes[..8].copy_from_slice(&encoded_offset.to_be_bytes());
        bytes[8..].copy_from_slice(&slot.digest);
        write_all_at(
            &disk_index.writer,
            &bytes,
            stage_disk_slot_offset(index)?,
            self.temporary.path(),
        )
    }

    fn read_object(&self, offset: u64, expected: [u8; 32]) -> PocResult<Vec<u8>> {
        let mut header = [0_u8; 36];
        read_exact_at(&self.reader, &mut header, offset, self.temporary.path())?;
        let digest: [u8; 32] = header[..32].try_into().map_err(|_| {
            PocError::Integrity("incremental semantic staged object digest is truncated".to_owned())
        })?;
        let length = u32::from_be_bytes(header[32..36].try_into().map_err(|_| {
            PocError::Integrity("incremental semantic staged object length is truncated".to_owned())
        })?);
        if digest != expected || length == 0 || u64::from(length) > MAX_OBJECT_BYTES {
            return Err(PocError::Integrity(
                "incremental semantic staged object is invalid".to_owned(),
            ));
        }
        let mut bytes = vec![
            0_u8;
            usize::try_from(length).map_err(|_| {
                PocError::Integrity(
                    "incremental semantic staged object length overflows usize".to_owned(),
                )
            })?
        ];
        read_exact_at(
            &self.reader,
            &mut bytes,
            offset.saturating_add(36),
            self.temporary.path(),
        )?;
        if object_digest(&bytes) != expected {
            return Err(PocError::Integrity(
                "incremental semantic staged object digest mismatch".to_owned(),
            ));
        }
        Ok(bytes)
    }
}

impl StageDiskIndex {
    fn new(directory: &Path) -> PocResult<Self> {
        let path = directory.join("objects.index");
        let writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                PocError::io("create incremental semantic stage index", &path, error)
            })?;
        let bytes = u64::try_from(STAGE_DISK_SLOT_COUNT)
            .unwrap_or(u64::MAX)
            .checked_mul(u64::try_from(STAGE_SLOT_BYTES).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                PocError::Integrity("incremental semantic stage index size overflow".to_owned())
            })?;
        writer
            .set_len(bytes)
            .map_err(|error| PocError::io("size incremental semantic stage index", &path, error))?;
        let reader = File::open(&path)
            .map_err(|error| PocError::io("open incremental semantic stage index", &path, error))?;
        Ok(Self { writer, reader })
    }
}

struct IncrementalPackWriter {
    pack_path: PathBuf,
    index_path: PathBuf,
    pack_temporary: PathBuf,
    index_temporary: PathBuf,
    pack_cleanup: TemporaryFile,
    index_cleanup: TemporaryFile,
    pack: BufWriter<File>,
    index: File,
    object_count: u64,
    object_bytes: u64,
    offset: u64,
    previous_digest: Option<[u8; 32]>,
    bloom: Box<[u64]>,
}

impl IncrementalPackWriter {
    fn new(directory: &Path) -> PocResult<Self> {
        std::fs::create_dir_all(directory)
            .map_err(|error| PocError::io("create semantic pack directory", directory, error))?;
        let identifier = Uuid::new_v4();
        let base = format!("pack-{identifier}");
        let pack_path = directory.join(format!("{base}.pack"));
        let index_path = directory.join(format!("{base}.index"));
        let pack_temporary = directory.join(format!(".{base}-{}.pack.tmp", Uuid::new_v4()));
        let index_temporary = directory.join(format!(".{base}-{}.index.tmp", Uuid::new_v4()));
        let pack_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pack_temporary)
            .map_err(|error| {
                PocError::io("create incremental semantic pack", &pack_temporary, error)
            })?;
        let mut pack = BufWriter::with_capacity(PACK_WRITE_BUFFER_BYTES, pack_file);
        pack.write_all(PACK_MAGIC).map_err(|error| {
            PocError::io(
                "write incremental semantic pack header",
                &pack_temporary,
                error,
            )
        })?;
        let mut index = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&index_temporary)
            .map_err(|error| {
                PocError::io(
                    "create incremental semantic pack index",
                    &index_temporary,
                    error,
                )
            })?;
        index
            .write_all(PACK_INDEX_MAGIC)
            .and_then(|()| index.write_all(&0_u32.to_be_bytes()))
            .map_err(|error| {
                PocError::io(
                    "write incremental semantic pack index header",
                    &index_temporary,
                    error,
                )
            })?;
        Ok(Self {
            pack_path,
            index_path,
            pack_temporary: pack_temporary.clone(),
            index_temporary: index_temporary.clone(),
            pack_cleanup: TemporaryFile::new(pack_temporary),
            index_cleanup: TemporaryFile::new(index_temporary),
            pack,
            index,
            object_count: 0,
            object_bytes: 0,
            offset: u64::try_from(PACK_MAGIC.len()).unwrap_or(u64::MAX),
            previous_digest: None,
            bloom: vec![0_u64; PACK_BLOOM_WORDS].into_boxed_slice(),
        })
    }

    fn can_append(&self, bytes: usize) -> bool {
        self.object_count < u64::try_from(MAX_PACK_OBJECTS).unwrap_or(u64::MAX)
            && self
                .object_bytes
                .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX))
                <= MAX_PACK_BYTES
    }

    fn append(&mut self, digest: [u8; 32], bytes: &[u8]) -> PocResult<()> {
        if !self.can_append(bytes.len()) {
            return Err(PocError::Integrity(
                "incremental semantic pack segment exceeds its fixed bound".to_owned(),
            ));
        }
        let length = u32::try_from(bytes.len()).map_err(|_| {
            PocError::Integrity("incremental semantic packed object exceeds u32".to_owned())
        })?;
        if length == 0 || u64::from(length) > MAX_OBJECT_BYTES {
            return Err(PocError::Integrity(
                "incremental semantic packed object is outside its fixed bound".to_owned(),
            ));
        }
        if self
            .previous_digest
            .as_ref()
            .is_some_and(|previous| previous >= &digest)
        {
            return Err(PocError::Integrity(
                "incremental semantic pack entries are not strictly digest-sorted".to_owned(),
            ));
        }
        self.pack
            .write_all(&length.to_be_bytes())
            .and_then(|()| self.pack.write_all(bytes))
            .map_err(|error| {
                PocError::io(
                    "write incremental semantic packed object",
                    &self.pack_temporary,
                    error,
                )
            })?;
        self.offset = self.offset.checked_add(4).ok_or_else(|| {
            PocError::Integrity("incremental semantic pack offset overflow".to_owned())
        })?;
        self.index
            .write_all(&digest)
            .and_then(|()| self.index.write_all(&self.offset.to_be_bytes()))
            .and_then(|()| self.index.write_all(&length.to_be_bytes()))
            .map_err(|error| {
                PocError::io(
                    "write incremental semantic pack index entry",
                    &self.index_temporary,
                    error,
                )
            })?;
        self.offset = self.offset.checked_add(u64::from(length)).ok_or_else(|| {
            PocError::Integrity("incremental semantic pack offset overflow".to_owned())
        })?;
        self.object_count = self.object_count.saturating_add(1);
        self.object_bytes = self.object_bytes.saturating_add(u64::from(length));
        self.previous_digest = Some(digest);
        pack_bloom_insert(&mut self.bloom, &digest);
        Ok(())
    }

    fn finish(mut self) -> PocResult<PackedObjectIndex> {
        if self.object_count == 0 {
            return Err(PocError::Integrity(
                "incremental semantic pack cannot be empty".to_owned(),
            ));
        }
        self.pack.flush().map_err(|error| {
            PocError::io(
                "flush incremental semantic pack",
                &self.pack_temporary,
                error,
            )
        })?;
        self.pack.get_ref().sync_all().map_err(|error| {
            PocError::io(
                "fsync incremental semantic pack",
                &self.pack_temporary,
                error,
            )
        })?;
        drop(self.pack);

        let count = u32::try_from(self.object_count).map_err(|_| {
            PocError::Integrity("incremental semantic pack count overflow".to_owned())
        })?;
        self.index
            .seek(SeekFrom::Start(8))
            .and_then(|_| self.index.write_all(&count.to_be_bytes()))
            .and_then(|_| self.index.flush())
            .map_err(|error| {
                PocError::io(
                    "finalize incremental semantic pack index header",
                    &self.index_temporary,
                    error,
                )
            })?;
        let index_body_len = 12_u64
            .checked_add(
                self.object_count
                    .saturating_mul(u64::try_from(PACK_INDEX_ENTRY_BYTES).unwrap_or(u64::MAX)),
            )
            .ok_or_else(|| {
                PocError::Integrity("incremental semantic pack index length overflow".to_owned())
            })?;
        let checksum = sha256_prefix(&self.index_temporary, index_body_len)?;
        self.index
            .seek(SeekFrom::Start(index_body_len))
            .and_then(|_| self.index.write_all(&checksum))
            .and_then(|_| self.index.sync_all())
            .map_err(|error| {
                PocError::io(
                    "write incremental semantic pack index checksum",
                    &self.index_temporary,
                    error,
                )
            })?;
        drop(self.index);

        install_temporary_no_replace(&self.pack_temporary, &self.pack_path).map_err(|error| {
            PocError::io("install incremental semantic pack", &self.pack_path, error)
        })?;
        self.pack_cleanup
            .remove("remove installed incremental semantic pack temporary")?;
        install_temporary_no_replace(&self.index_temporary, &self.index_path).map_err(|error| {
            PocError::io(
                "install incremental semantic pack index",
                &self.index_path,
                error,
            )
        })?;
        self.index_cleanup
            .remove("remove installed incremental semantic pack index temporary")?;
        // This writer has already established every structural invariant the
        // general reader verifies: `append` enforces strict digest order and
        // bounded lengths, owns the sequential offsets, and constructs the
        // bloom; the separate checksum pass above verifies the exact persisted
        // index body. The pack and index are synced and atomically installed
        // before this metadata is returned, and the catalog/normal reopen
        // paths retain full validation for pre-existing on-disk input.
        Ok(PackedObjectIndex {
            pack: self.pack_path,
            index: self.index_path,
            count: usize::try_from(self.object_count).map_err(|_| {
                PocError::Integrity("incremental semantic pack count overflow".to_owned())
            })?,
            entries_end: index_body_len,
            index_checksum: checksum,
            pack_bytes: self.offset,
            bloom: self.bloom.into(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrieRoots {
    pub content: [u8; 32],
    pub attribution: [u8; 32],
}

impl TrieRoots {
    pub fn from_hex(content: &str, attribution: &str) -> PocResult<Self> {
        Ok(Self {
            content: parse_hex_digest(content)?,
            attribution: parse_hex_digest(attribution)?,
        })
    }

    pub fn content_hex(&self) -> String {
        super::hex_digest(self.content)
    }

    pub fn attribution_hex(&self) -> String {
        super::hex_digest(self.attribution)
    }

    pub fn record_stream_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"mpla-poc-semantic-v1/record-stream\0");
        digest.update(self.content);
        super::hex_digest(digest.finalize().into())
    }

    pub fn to_root_pair(&self) -> PocResult<CanonicalRootPair> {
        Ok(CanonicalRootPair {
            root_id: RootId::from_digest_bytes(self.content),
            attribution_root_id: AttributionRootId::from_digest_bytes(self.attribution),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationOutcome {
    pub roots: TrieRoots,
    pub existed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationBatchOutcome {
    pub roots: TrieRoots,
    pub entry_count_delta: i64,
}

pub struct ImmutableObjectStore {
    root: PathBuf,
    objects: PathBuf,
    objects_written: u64,
    bytes_written: u64,
    bytes_read: u64,
    object_set_journals: Vec<DigestJournal>,
    touched_prefixes: [bool; 256],
    existing_digests: Option<Arc<Vec<[u8; 32]>>>,
    installed_digests: HashSet<[u8; 32]>,
    source_objects: Vec<PathBuf>,
    packed_indexes: Vec<PackedObjectIndex>,
    read_packed_indexes: Vec<PackedObjectIndex>,
    pack_index_cache: VecDeque<PackIndexCachePage>,
    packed_object_cache: VecDeque<PackedObjectCacheEntry>,
    packed_object_cache_bytes: usize,
    active_pack_readers: VecDeque<ActivePackReader>,
    packs_touched: bool,
    incremental_stage: Option<IncrementalStage>,
}

impl ImmutableObjectStore {
    pub fn new(root: &Path) -> PocResult<Self> {
        Self::open(root, true)
    }

    pub(super) fn new_incremental(root: &Path, source_roots: &[PathBuf]) -> PocResult<Self> {
        let mut store = Self::open(root, false)?;
        store.add_read_only_sources(source_roots)?;
        Ok(store)
    }

    pub(super) fn new_with_read_only_sources(
        root: &Path,
        source_roots: &[PathBuf],
    ) -> PocResult<Self> {
        let mut store = Self::open(root, true)?;
        store.add_read_only_sources(source_roots)?;
        Ok(store)
    }

    fn open(root: &Path, preload_existing_digests: bool) -> PocResult<Self> {
        let objects = root.join("objects");
        std::fs::create_dir_all(&objects)
            .map_err(|error| PocError::io("create semantic object store", &objects, error))?;
        let packed_indexes = load_packed_indexes(&objects)?;
        let existing_digests = if preload_existing_digests {
            load_existing_digest_cache(&objects)?.map(Arc::new)
        } else {
            None
        };
        let incremental_stage = if preload_existing_digests {
            None
        } else {
            Some(IncrementalStage::new(root)?)
        };
        Ok(Self {
            root: root.to_path_buf(),
            objects,
            objects_written: 0,
            bytes_written: 0,
            bytes_read: 0,
            object_set_journals: vec![DigestJournal::new(root)?],
            touched_prefixes: [false; 256],
            existing_digests,
            installed_digests: HashSet::new(),
            source_objects: Vec::new(),
            read_packed_indexes: packed_indexes.clone(),
            packed_indexes,
            pack_index_cache: VecDeque::new(),
            packed_object_cache: VecDeque::new(),
            packed_object_cache_bytes: 0,
            active_pack_readers: VecDeque::new(),
            packs_touched: false,
            incremental_stage,
        })
    }

    fn add_read_only_sources(&mut self, source_roots: &[PathBuf]) -> PocResult<()> {
        for source_root in source_roots {
            if source_root == &self.root {
                continue;
            }
            let objects = source_root.join("objects");
            let metadata = std::fs::metadata(&objects).map_err(|error| {
                PocError::io("stat read-only semantic source objects", &objects, error)
            })?;
            if !metadata.is_dir() {
                return Err(PocError::Integrity(
                    "read-only semantic source objects is not a directory".to_owned(),
                ));
            }
            self.read_packed_indexes
                .extend(load_read_only_packed_indexes(&objects)?);
            self.source_objects.push(objects);
        }
        Ok(())
    }

    pub const fn objects_written(&self) -> u64 {
        self.objects_written
    }

    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    pub const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    pub fn object_set_sha256(&mut self) -> PocResult<String> {
        for journal in &mut self.object_set_journals {
            journal.close();
        }
        let temporary = TemporaryDirectory::new("eos-mpla-object-set-sort", &self.root)?;
        let mut spool =
            BoundedSpool::new_ephemeral(temporary.path().join("spool"), DIGEST_SORT_MEMORY_BYTES)?;
        for journal in &self.object_set_journals {
            journal.spool_into(&mut spool)?;
        }
        let sorted = spool.finish()?;
        let mut object_set = Sha256::new();
        object_set.update(b"mpla-poc-semantic-v1/installed-object-set\0");
        let mut count = 0_u64;
        sorted.for_each(|digest, _| {
            if digest.len() != 32 {
                return Err(PocError::Integrity(
                    "semantic object-set digest has invalid length".to_owned(),
                ));
            }
            object_set.update(digest);
            count = count.saturating_add(1);
            Ok(())
        })?;
        if count != self.objects_written {
            return Err(PocError::Integrity(
                "semantic object-set journal disagrees with installed object count".to_owned(),
            ));
        }
        Ok(super::hex_digest(object_set.finalize().into()))
    }

    pub fn sync_directory(&self) -> PocResult<()> {
        self.sync_touched_directories()
    }

    pub fn sync_files(&self) -> PocResult<()> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let filesystem = File::open(&self.objects).map_err(|error| {
                PocError::io(
                    "open semantic object filesystem for sync",
                    &self.objects,
                    error,
                )
            })?;
            rustix::fs::syncfs(&filesystem).map_err(|error| {
                PocError::io(
                    "sync semantic object filesystem",
                    &self.objects,
                    std::io::Error::from(error),
                )
            })?;
            Ok(())
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            Ok(())
        }
    }

    pub(super) fn commit_incremental_roots(&mut self, roots: &TrieRoots) -> PocResult<()> {
        let stage = self.incremental_stage.take().ok_or_else(|| {
            PocError::Integrity(
                "incremental semantic commit lacks its transactional stage".to_owned(),
            )
        })?;
        let temporary = TemporaryDirectory::new("eos-mpla-reachable-sort", &self.root)?;
        let mut reachable =
            BoundedSpool::new_ephemeral(temporary.path().join("spool"), DIGEST_SORT_MEMORY_BYTES)?;
        collect_staged_reachable(
            roots.content,
            TrieKind::Content,
            true,
            &stage,
            &mut reachable,
        )?;
        collect_staged_reachable(
            roots.attribution,
            TrieKind::Attribution,
            true,
            &stage,
            &mut reachable,
        )?;
        let reachable = reachable.finish()?;
        let mut pack_writer = None;
        reachable.for_each(|digest, _| {
            let digest: [u8; 32] = digest.try_into().map_err(|_| {
                PocError::Integrity("reachable semantic digest has invalid length".to_owned())
            })?;
            let bytes = stage.load(digest)?.ok_or_else(|| {
                PocError::Integrity("reachable semantic object escaped its stage".to_owned())
            })?;
            if pack_writer
                .as_ref()
                .is_some_and(|writer: &IncrementalPackWriter| !writer.can_append(bytes.len()))
            {
                self.finish_incremental_pack(&mut pack_writer)?;
            }
            if pack_writer.is_none() {
                pack_writer = Some(IncrementalPackWriter::new(&self.pack_directory())?);
            }
            pack_writer
                .as_mut()
                .ok_or_else(|| {
                    PocError::Integrity("incremental semantic pack writer is absent".to_owned())
                })?
                .append(digest, &bytes)?;
            self.record_new_link(digest)?;
            self.remember_installed(digest);
            Ok(())
        })?;
        self.finish_incremental_pack(&mut pack_writer)?;
        if self.packs_touched {
            write_pack_catalog(&self.pack_directory(), &self.packed_indexes)?;
        }
        Ok(())
    }

    fn finish_incremental_pack(
        &mut self,
        writer: &mut Option<IncrementalPackWriter>,
    ) -> PocResult<()> {
        let Some(writer) = writer.take() else {
            return Ok(());
        };
        let object_count = writer.object_count;
        let object_bytes = writer.object_bytes;
        let index = writer.finish()?;
        self.objects_written = self.objects_written.saturating_add(object_count);
        self.bytes_written = self.bytes_written.saturating_add(object_bytes);
        self.read_packed_indexes.push(index.clone());
        self.packed_indexes.push(index);
        self.packs_touched = true;
        Ok(())
    }

    fn install(&mut self, bytes: &[u8]) -> PocResult<[u8; 32]> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_OBJECT_BYTES {
            return Err(PocError::Integrity(
                "semantic immutable object exceeds fixed bound".to_owned(),
            ));
        }
        let digest = object_digest(bytes);
        if self.incremental_stage.is_some() {
            self.install_incremental(digest, bytes)?;
            return Ok(digest);
        }
        if self.installed_digests.contains(&digest) {
            return Ok(digest);
        }
        if let Some((pack, _, entry)) = self.find_packed_object(digest)? {
            read_packed_object(&pack, &entry, digest)?;
            self.remember_installed(digest);
            return Ok(digest);
        }
        if self
            .existing_digests
            .as_ref()
            .is_some_and(|digests| digests.binary_search(&digest).is_ok())
        {
            let prefix = usize::from(digest[0]);
            verify_existing_object(&self.object_path(digest), digest)?;
            self.touched_prefixes[prefix] = true;
            self.remember_installed(digest);
            return Ok(digest);
        }
        self.persist_object(digest, bytes, false)?;
        Ok(digest)
    }

    fn install_incremental(&mut self, digest: [u8; 32], bytes: &[u8]) -> PocResult<()> {
        self.incremental_stage
            .as_mut()
            .ok_or_else(|| {
                PocError::Integrity("incremental semantic stage is unexpectedly absent".to_owned())
            })?
            .store(digest, bytes)
    }

    fn persist_object(&mut self, digest: [u8; 32], bytes: &[u8], sync_file: bool) -> PocResult<()> {
        let prefix = usize::from(digest[0]);
        let directory = self.objects.join(format!("{prefix:02x}"));
        let path = directory.join(super::hex_digest(digest));
        std::fs::create_dir_all(&directory)
            .map_err(|error| PocError::io("create semantic object shard", &directory, error))?;
        let temporary = directory.join(format!(
            ".{}-{}.tmp",
            super::hex_digest(digest),
            Uuid::new_v4()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| PocError::io("create semantic immutable object", &temporary, error))?;
        let mut cleanup = TemporaryFile::new(temporary.clone());
        file.write_all(bytes)
            .map_err(|error| PocError::io("write semantic immutable object", &temporary, error))?;
        if sync_file {
            file.sync_all().map_err(|error| {
                PocError::io("fsync semantic immutable object", &temporary, error)
            })?;
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        if !sync_file {
            file.sync_all().map_err(|error| {
                PocError::io("fsync semantic immutable object", &temporary, error)
            })?;
        }
        drop(file);
        match install_temporary_no_replace(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_existing_object(&path, digest)?;
                cleanup.remove("remove redundant semantic object temporary")?;
                self.touched_prefixes[prefix] = true;
                self.remember_installed(digest);
                return Ok(());
            }
            Err(error) => {
                return Err(PocError::io(
                    "install semantic immutable object",
                    &path,
                    error,
                ));
            }
        }
        cleanup.remove("remove installed semantic object temporary")?;
        self.touched_prefixes[prefix] = true;
        self.objects_written = self.objects_written.saturating_add(1);
        self.bytes_written = self
            .bytes_written
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        self.record_new_link(digest)?;
        self.remember_installed(digest);
        Ok(())
    }

    fn load(&mut self, digest: [u8; 32]) -> PocResult<Vec<u8>> {
        if let Some(stage) = &self.incremental_stage {
            if let Some(bytes) = stage.load(digest)? {
                return Ok(bytes);
            }
        }
        if let Some(position) = self
            .packed_object_cache
            .iter()
            .position(|entry| entry.digest == digest)
        {
            let entry = self.packed_object_cache.remove(position).ok_or_else(|| {
                PocError::Integrity("semantic packed-object cache entry disappeared".to_owned())
            })?;
            let bytes = entry.bytes.to_vec();
            self.packed_object_cache.push_back(entry);
            return Ok(bytes);
        }
        if let Some(bytes) = load_daemon_verified_object(digest) {
            self.cache_verified_packed_object(digest, &bytes);
            return Ok(bytes);
        }
        let bytes = if let Some((pack, pack_bytes, entry)) = self.find_packed_object(digest)? {
            self.read_cached_packed_object(&pack, pack_bytes, &entry, digest)?
        } else {
            self.load_loose_object(digest)?
        };
        self.bytes_read = self
            .bytes_read
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        self.cache_verified_packed_object(digest, &bytes);
        cache_daemon_verified_object(digest, &bytes);
        Ok(bytes)
    }

    fn load_loose_object(&self, digest: [u8; 32]) -> PocResult<Vec<u8>> {
        for objects in std::iter::once(&self.objects).chain(self.source_objects.iter()) {
            let path = objects
                .join(format!("{:02x}", digest[0]))
                .join(super::hex_digest(digest));
            match read_bounded_object(&path) {
                Ok(bytes) => {
                    if object_digest(&bytes) != digest {
                        return Err(PocError::Integrity(
                            "semantic immutable object digest mismatch".to_owned(),
                        ));
                    }
                    return Ok(bytes);
                }
                Err(PocError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let path = self.object_path(digest);
        Err(PocError::io(
            "stat semantic object",
            &path,
            std::io::Error::from(std::io::ErrorKind::NotFound),
        ))
    }

    fn cache_verified_packed_object(&mut self, digest: [u8; 32], bytes: &[u8]) {
        if bytes.len() > PACKED_OBJECT_CACHE_BYTES {
            return;
        }
        while self.packed_object_cache_bytes.saturating_add(bytes.len()) > PACKED_OBJECT_CACHE_BYTES
        {
            let Some(evicted) = self.packed_object_cache.pop_front() else {
                return;
            };
            self.packed_object_cache_bytes = self
                .packed_object_cache_bytes
                .saturating_sub(evicted.bytes.len());
        }
        self.packed_object_cache_bytes = self.packed_object_cache_bytes.saturating_add(bytes.len());
        self.packed_object_cache.push_back(PackedObjectCacheEntry {
            digest,
            bytes: bytes.into(),
        });
    }

    fn read_cached_packed_object(
        &mut self,
        pack_path: &Path,
        pack_bytes: u64,
        entry: &PackedObjectEntry,
        expected: [u8; 32],
    ) -> PocResult<Vec<u8>> {
        let position = self
            .active_pack_readers
            .iter()
            .position(|reader| reader.path == pack_path && reader.pack_bytes == pack_bytes);
        let reader = match position {
            Some(position) => self.active_pack_readers.remove(position).ok_or_else(|| {
                PocError::Integrity("semantic active pack reader disappeared".to_owned())
            })?,
            None => ActivePackReader {
                path: pack_path.to_path_buf(),
                pack_bytes,
                file: open_validated_pack_reader(pack_path, pack_bytes)?,
            },
        };
        let result =
            read_validated_packed_object(&reader.file, pack_path, pack_bytes, entry, expected);
        if self.active_pack_readers.len() == ACTIVE_PACK_READERS {
            self.active_pack_readers.pop_front();
        }
        self.active_pack_readers.push_back(reader);
        result
    }

    fn object_path(&self, digest: [u8; 32]) -> PathBuf {
        self.objects
            .join(format!("{:02x}", digest[0]))
            .join(super::hex_digest(digest))
    }

    fn pack_directory(&self) -> PathBuf {
        self.objects.join("packs")
    }

    fn find_packed_object(
        &mut self,
        digest: [u8; 32],
    ) -> PocResult<Option<(PathBuf, u64, PackedObjectEntry)>> {
        for position in 0..self.read_packed_indexes.len() {
            let (pack, pack_bytes, index, entries_end, count, may_contain) = {
                let packed = &self.read_packed_indexes[position];
                (
                    packed.pack.clone(),
                    packed.pack_bytes,
                    packed.index.clone(),
                    packed.entries_end,
                    packed.count,
                    pack_bloom_contains(&packed.bloom, &digest),
                )
            };
            if !may_contain {
                continue;
            }
            let mut left = 0_usize;
            let mut right = count;
            while left < right {
                let middle = left + (right - left) / 2;
                let entry = self.read_cached_pack_index_entry(&index, entries_end, middle)?;
                match entry.digest.cmp(&digest) {
                    std::cmp::Ordering::Less => left = middle.saturating_add(1),
                    std::cmp::Ordering::Greater => right = middle,
                    std::cmp::Ordering::Equal => return Ok(Some((pack, pack_bytes, entry))),
                }
            }
        }
        Ok(None)
    }

    fn read_cached_pack_index_entry(
        &mut self,
        index_path: &Path,
        entries_end: u64,
        entry: usize,
    ) -> PocResult<PackedObjectEntry> {
        let entry_offset = pack_index_entry_offset(entry)?;
        let entry_end = entry_offset
            .checked_add(u64::try_from(PACK_INDEX_ENTRY_BYTES).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                PocError::Integrity("semantic pack index lookup end overflow".to_owned())
            })?;
        if entry_end > entries_end {
            return Err(PocError::Integrity(
                "semantic pack index lookup exceeds validated entries".to_owned(),
            ));
        }
        let entries_per_page = PACK_INDEX_CACHE_PAGE_BYTES / PACK_INDEX_ENTRY_BYTES;
        let page_first_entry = entry / entries_per_page * entries_per_page;
        let page_offset = pack_index_entry_offset(page_first_entry)?;
        let position = self
            .pack_index_cache
            .iter()
            .position(|page| page.path == index_path && page.offset == page_offset);
        if let Some(position) = position {
            let page = self.pack_index_cache.remove(position).ok_or_else(|| {
                PocError::Integrity("semantic pack index cache entry disappeared".to_owned())
            })?;
            let decoded = decode_cached_pack_index_entry(&page, entry_offset)?;
            self.pack_index_cache.push_back(page);
            return Ok(decoded);
        }
        let page_end = page_offset
            .saturating_add(
                u64::try_from(entries_per_page.saturating_mul(PACK_INDEX_ENTRY_BYTES))
                    .unwrap_or(u64::MAX),
            )
            .min(entries_end);
        let page_len = usize::try_from(page_end.saturating_sub(page_offset)).map_err(|_| {
            PocError::Integrity("semantic pack index cache page size overflow".to_owned())
        })?;
        if page_len == 0 {
            return Err(PocError::Integrity(
                "semantic pack index cache page is empty".to_owned(),
            ));
        }
        let mut bytes = vec![0_u8; page_len];
        let file = File::open(index_path).map_err(|error| {
            PocError::io(
                "open semantic pack index for cached lookup",
                index_path,
                error,
            )
        })?;
        read_exact_at(&file, &mut bytes, page_offset, index_path)?;
        let page = PackIndexCachePage {
            path: index_path.to_path_buf(),
            offset: page_offset,
            bytes: bytes.into_boxed_slice(),
        };
        let decoded = decode_cached_pack_index_entry(&page, entry_offset)?;
        if self.pack_index_cache.len() == PACK_INDEX_CACHE_PAGES {
            self.pack_index_cache.pop_front();
        }
        self.pack_index_cache.push_back(page);
        Ok(decoded)
    }

    fn sync_touched_directories(&self) -> PocResult<()> {
        for (prefix, touched) in self.touched_prefixes.iter().enumerate() {
            if *touched {
                sync_directory(&self.objects.join(format!("{prefix:02x}")))?;
            }
        }
        sync_directory(&self.objects)?;
        sync_directory(&self.root)
    }

    fn remember_installed(&mut self, digest: [u8; 32]) {
        if self.installed_digests.len() < MAX_CACHED_INSTALLED_OBJECTS {
            self.installed_digests.insert(digest);
        }
    }

    fn record_new_link(&mut self, digest: [u8; 32]) -> PocResult<()> {
        self.object_set_journals
            .last_mut()
            .ok_or_else(|| PocError::Integrity("semantic digest journal is absent".to_owned()))?
            .append(digest)
    }

    fn fork(&self) -> PocResult<Self> {
        if self.incremental_stage.is_some() {
            return Err(PocError::Integrity(
                "incremental semantic mutation cannot fork its transaction".to_owned(),
            ));
        }
        Ok(Self {
            root: self.root.clone(),
            objects: self.objects.clone(),
            objects_written: 0,
            bytes_written: 0,
            bytes_read: 0,
            object_set_journals: vec![DigestJournal::new(&self.root)?],
            touched_prefixes: [false; 256],
            existing_digests: self.existing_digests.clone(),
            installed_digests: self.installed_digests.clone(),
            source_objects: self.source_objects.clone(),
            packed_indexes: self.packed_indexes.clone(),
            read_packed_indexes: self.read_packed_indexes.clone(),
            pack_index_cache: VecDeque::new(),
            packed_object_cache: VecDeque::new(),
            packed_object_cache_bytes: 0,
            active_pack_readers: VecDeque::new(),
            packs_touched: false,
            incremental_stage: None,
        })
    }

    fn absorb_parallel(&mut self, mut other: Self) {
        self.objects_written = self.objects_written.saturating_add(other.objects_written);
        self.bytes_written = self.bytes_written.saturating_add(other.bytes_written);
        self.bytes_read = self.bytes_read.saturating_add(other.bytes_read);
        for (touched, other_touched) in self
            .touched_prefixes
            .iter_mut()
            .zip(other.touched_prefixes.iter())
        {
            *touched |= *other_touched;
        }
        self.object_set_journals
            .append(&mut other.object_set_journals);
        self.packs_touched |= other.packs_touched;
        for digest in std::mem::take(&mut other.installed_digests) {
            self.remember_installed(digest);
        }
    }
}

fn collect_staged_reachable(
    digest: [u8; 32],
    kind: TrieKind,
    root: bool,
    stage: &IncrementalStage,
    reachable: &mut BoundedSpool,
) -> PocResult<()> {
    if digest == empty_node_digest(kind)? || !stage.contains(&digest)? {
        return Ok(());
    }
    let bytes = stage.load(digest)?.ok_or_else(|| {
        PocError::Integrity("staged semantic object disappeared during commit".to_owned())
    })?;
    reachable.push(digest.to_vec(), vec![1])?;
    let frame = decode_node(&bytes, kind, root.then_some(0), root)?;
    for child in frame.children.into_iter().flatten() {
        child.validate_for_parent(frame.depth)?;
        if child.kind == ChildKind::Node {
            collect_staged_reachable(child.digest, kind, false, stage, reachable)?;
        }
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
                "partial semantic digest",
            ));
        }
        filled += count;
    }
    Ok(true)
}

fn stage_memory_slot_index(digest: &[u8; 32]) -> usize {
    usize::try_from(stage_hash(digest) % u64::try_from(STAGE_MEMORY_SLOT_COUNT).unwrap_or(1))
        .unwrap_or(0)
}

fn stage_disk_slot_index(digest: &[u8; 32]) -> usize {
    usize::try_from(stage_hash(digest) % u64::try_from(STAGE_DISK_SLOT_COUNT).unwrap_or(1))
        .unwrap_or(0)
}

fn stage_hash(digest: &[u8; 32]) -> u64 {
    let first = u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("digest prefix has fixed width"),
    );
    let second = u64::from_be_bytes(
        digest[8..16]
            .try_into()
            .expect("digest prefix has fixed width"),
    );
    first ^ second.rotate_left(29)
}

fn stage_disk_slot_offset(index: usize) -> PocResult<u64> {
    u64::try_from(index)
        .unwrap_or(u64::MAX)
        .checked_mul(u64::try_from(STAGE_SLOT_BYTES).unwrap_or(u64::MAX))
        .ok_or_else(|| PocError::Integrity("incremental semantic stage slot overflow".to_owned()))
}

#[cfg(unix)]
fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64, path: &Path) -> PocResult<()> {
    file.read_exact_at(bytes, offset)
        .map_err(|error| PocError::io("read incremental semantic stage", path, error))
}

#[cfg(unix)]
fn write_all_at(file: &File, bytes: &[u8], offset: u64, path: &Path) -> PocResult<()> {
    let mut written = 0_usize;
    while written < bytes.len() {
        let at = offset
            .checked_add(u64::try_from(written).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                PocError::Integrity("incremental semantic stage write offset overflow".to_owned())
            })?;
        let count = file
            .write_at(&bytes[written..], at)
            .map_err(|error| PocError::io("write incremental semantic stage", path, error))?;
        if count == 0 {
            return Err(PocError::io(
                "write incremental semantic stage",
                path,
                std::io::Error::from(std::io::ErrorKind::WriteZero),
            ));
        }
        written = written.saturating_add(count);
    }
    Ok(())
}

#[cfg(not(unix))]
fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64, path: &Path) -> PocResult<()> {
    let mut reader = file
        .try_clone()
        .map_err(|error| PocError::io("clone incremental semantic stage", path, error))?;
    reader
        .seek(SeekFrom::Start(offset))
        .and_then(|()| reader.read_exact(bytes))
        .map_err(|error| PocError::io("read incremental semantic stage", path, error))
}

#[cfg(not(unix))]
fn write_all_at(file: &File, bytes: &[u8], offset: u64, path: &Path) -> PocResult<()> {
    let mut writer = file
        .try_clone()
        .map_err(|error| PocError::io("clone incremental semantic stage", path, error))?;
    writer
        .seek(SeekFrom::Start(offset))
        .and_then(|()| writer.write_all(bytes))
        .map_err(|error| PocError::io("write incremental semantic stage", path, error))
}

fn sha256_prefix(path: &Path, length: u64) -> PocResult<[u8; 32]> {
    let mut file = File::open(path)
        .map_err(|error| PocError::io("open semantic pack index for checksum", path, error))?;
    let mut remaining = length;
    let mut buffer = [0_u8; PACK_WRITE_BUFFER_BYTES];
    let mut checksum = Sha256::new();
    while remaining > 0 {
        let take = usize::try_from(remaining.min(u64::try_from(buffer.len()).unwrap_or(u64::MAX)))
            .unwrap_or(buffer.len());
        file.read_exact(&mut buffer[..take])
            .map_err(|error| PocError::io("read semantic pack index for checksum", path, error))?;
        checksum.update(&buffer[..take]);
        remaining = remaining.saturating_sub(u64::try_from(take).unwrap_or(u64::MAX));
    }
    Ok(checksum.finalize().into())
}

fn decode_pack_index_entry(bytes: &[u8; PACK_INDEX_ENTRY_BYTES]) -> PocResult<PackedObjectEntry> {
    let digest: [u8; 32] = bytes[..32]
        .try_into()
        .map_err(|_| PocError::Integrity("semantic pack index digest is truncated".to_owned()))?;
    let offset =
        u64::from_be_bytes(bytes[32..40].try_into().map_err(|_| {
            PocError::Integrity("semantic pack index offset is truncated".to_owned())
        })?);
    let length =
        u32::from_be_bytes(bytes[40..44].try_into().map_err(|_| {
            PocError::Integrity("semantic pack index length is truncated".to_owned())
        })?);
    Ok(PackedObjectEntry {
        digest,
        offset,
        length,
    })
}

fn pack_bloom_positions(digest: &[u8; 32]) -> [usize; 3] {
    let first = u64::from_be_bytes(digest[..8].try_into().expect("digest has fixed width"));
    let second = u64::from_be_bytes(digest[8..16].try_into().expect("digest has fixed width"));
    let third = u64::from_be_bytes(digest[16..24].try_into().expect("digest has fixed width"));
    [
        first,
        second.rotate_left(21) ^ first,
        third.rotate_left(43) ^ second,
    ]
    .map(|value| usize::try_from(value % u64::try_from(PACK_BLOOM_BITS).unwrap_or(1)).unwrap_or(0))
}

fn pack_bloom_insert(bloom: &mut [u64], digest: &[u8; 32]) {
    for position in pack_bloom_positions(digest) {
        let word = position / u64::BITS as usize;
        let bit = position % u64::BITS as usize;
        bloom[word] |= 1_u64 << bit;
    }
}

fn pack_bloom_contains(bloom: &[u64], digest: &[u8; 32]) -> bool {
    pack_bloom_positions(digest).into_iter().all(|position| {
        let word = position / u64::BITS as usize;
        let bit = position % u64::BITS as usize;
        bloom
            .get(word)
            .is_some_and(|value| (*value & (1_u64 << bit)) != 0)
    })
}

fn pack_index_entry_offset(index: usize) -> PocResult<u64> {
    12_u64
        .checked_add(
            u64::try_from(index)
                .unwrap_or(u64::MAX)
                .saturating_mul(u64::try_from(PACK_INDEX_ENTRY_BYTES).unwrap_or(u64::MAX)),
        )
        .ok_or_else(|| PocError::Integrity("semantic pack index lookup offset overflow".to_owned()))
}

fn decode_cached_pack_index_entry(
    page: &PackIndexCachePage,
    entry_offset: u64,
) -> PocResult<PackedObjectEntry> {
    let relative = usize::try_from(entry_offset.saturating_sub(page.offset)).map_err(|_| {
        PocError::Integrity("semantic pack index cache entry offset overflow".to_owned())
    })?;
    let entry_end = relative.saturating_add(PACK_INDEX_ENTRY_BYTES);
    let bytes = page.bytes.get(relative..entry_end).ok_or_else(|| {
        PocError::Integrity("semantic pack index entry crosses its cache page".to_owned())
    })?;
    let bytes: &[u8; PACK_INDEX_ENTRY_BYTES] = bytes.try_into().map_err(|_| {
        PocError::Integrity("semantic pack index cache entry has an invalid length".to_owned())
    })?;
    decode_pack_index_entry(bytes)
}

fn load_packed_indexes(objects: &Path) -> PocResult<Vec<PackedObjectIndex>> {
    let directory = objects.join("packs");
    std::fs::create_dir_all(&directory)
        .map_err(|error| PocError::io("create semantic pack directory", &directory, error))?;
    let paths = list_pack_indexes(&directory)?;
    if let Some(indexes) = load_pack_catalog(&directory, &paths)? {
        return Ok(indexes);
    }
    let mut indexes = Vec::new();
    for (name, index_path) in paths {
        let pack_path = pack_path_for_index(&directory, &name)
            .ok_or_else(|| PocError::Integrity("semantic pack index name is invalid".to_owned()))?;
        indexes.push(read_pack_index(&index_path, pack_path)?);
    }
    write_pack_catalog(&directory, &indexes)?;
    Ok(indexes)
}

fn load_read_only_packed_indexes(objects: &Path) -> PocResult<Vec<PackedObjectIndex>> {
    let directory = objects.join("packs");
    match std::fs::metadata(&directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(PocError::Integrity(
                "read-only semantic pack directory is not a directory".to_owned(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(PocError::io(
                "stat read-only semantic pack directory",
                &directory,
                error,
            ));
        }
    }
    // A source named by the immutable source chain is a completed canonical
    // publication: it is never extended in place. Cache only a catalog that
    // has already passed the normal on-disk validation, and key it by the
    // catalog's stable file identity so replacement falls back to validation.
    let catalog_path = directory.join(PACK_CATALOG_FILE);
    if let Some(identity) = pack_catalog_identity(&catalog_path)? {
        if let Some(indexes) = load_daemon_read_only_pack_catalog(&directory, &identity) {
            return Ok(indexes);
        }
    }
    let paths = list_pack_indexes(&directory)?;
    if let Some(indexes) = load_pack_catalog(&directory, &paths)? {
        if let Some(identity) = pack_catalog_identity(&catalog_path)? {
            cache_daemon_read_only_pack_catalog(&directory, identity, &indexes);
        }
        return Ok(indexes);
    }
    paths
        .into_iter()
        .map(|(name, index_path)| {
            let pack_path = pack_path_for_index(&directory, &name).ok_or_else(|| {
                PocError::Integrity("semantic pack index name is invalid".to_owned())
            })?;
            read_pack_index(&index_path, pack_path)
        })
        .collect()
}

fn pack_catalog_identity(path: &Path) -> PocResult<Option<PackCatalogIdentity>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(PocError::io("stat semantic pack catalog", path, error)),
    };
    if !metadata.file_type().is_file()
        || metadata.len()
            < u64::try_from(PACK_CATALOG_HEADER_BYTES + PACK_CATALOG_TRAILER_BYTES)
                .unwrap_or(u64::MAX)
        || metadata.len() > u64::try_from(MAX_PACK_CATALOG_BYTES).unwrap_or(u64::MAX)
    {
        return Ok(None);
    }
    Ok(Some(PackCatalogIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }))
}

fn list_pack_indexes(directory: &Path) -> PocResult<Vec<(String, PathBuf)>> {
    let entries = std::fs::read_dir(&directory)
        .map_err(|error| PocError::io("read semantic pack directory", &directory, error))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| PocError::io("read semantic pack entry", &directory, error))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if pack_path_for_index(&directory, &name).is_some() {
            paths.push((name, entry.path()));
        }
    }
    paths.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(paths)
}

struct PackCatalogEntry {
    identifier: Uuid,
    count: usize,
    index_bytes: u64,
    pack_bytes: u64,
    index_checksum: [u8; 32],
    bloom: Box<[u64]>,
}

fn load_pack_catalog(
    directory: &Path,
    paths: &[(String, PathBuf)],
) -> PocResult<Option<Vec<PackedObjectIndex>>> {
    let path = directory.join(PACK_CATALOG_FILE);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(PocError::io("open semantic pack catalog", &path, error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| PocError::io("stat semantic pack catalog", &path, error))?;
    if !metadata.file_type().is_file()
        || metadata.len()
            < u64::try_from(PACK_CATALOG_HEADER_BYTES + PACK_CATALOG_TRAILER_BYTES)
                .unwrap_or(u64::MAX)
        || metadata.len() > u64::try_from(MAX_PACK_CATALOG_BYTES).unwrap_or(u64::MAX)
    {
        return Ok(None);
    }
    let length = usize::try_from(metadata.len()).map_err(|_| {
        PocError::Integrity("semantic pack catalog length overflows usize".to_owned())
    })?;
    let mut bytes = vec![0_u8; length];
    let mut file = file;
    file.read_exact(&mut bytes)
        .map_err(|error| PocError::io("read semantic pack catalog", &path, error))?;
    let Some(entries) = decode_pack_catalog(&bytes) else {
        return Ok(None);
    };
    let mut expected_names = entries
        .iter()
        .map(|entry| format!("pack-{}.index", entry.identifier))
        .collect::<Vec<_>>();
    expected_names.sort_unstable();
    if expected_names.len() != paths.len()
        || expected_names
            .iter()
            .zip(paths.iter())
            .any(|(expected, (actual, _))| expected != actual)
    {
        return Ok(None);
    }
    let mut indexes = Vec::with_capacity(entries.len());
    for entry in entries {
        let index_path = directory.join(format!("pack-{}.index", entry.identifier));
        let pack_path = directory.join(format!("pack-{}.pack", entry.identifier));
        if !catalog_entry_matches_disk(&index_path, &pack_path, &entry)? {
            return Ok(None);
        }
        let entries_end = 12_u64
            .checked_add(
                u64::try_from(entry.count)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(u64::try_from(PACK_INDEX_ENTRY_BYTES).unwrap_or(u64::MAX)),
            )
            .ok_or_else(|| {
                PocError::Integrity("semantic pack catalog entry length overflow".to_owned())
            })?;
        indexes.push(PackedObjectIndex {
            pack: pack_path,
            index: index_path,
            count: entry.count,
            entries_end,
            index_checksum: entry.index_checksum,
            pack_bytes: entry.pack_bytes,
            bloom: entry.bloom.into(),
        });
    }
    Ok(Some(indexes))
}

fn catalog_entry_matches_disk(
    index_path: &Path,
    pack_path: &Path,
    entry: &PackCatalogEntry,
) -> PocResult<bool> {
    let index_metadata = match std::fs::symlink_metadata(index_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(PocError::io(
                "stat cataloged semantic pack index",
                index_path,
                error,
            ))
        }
    };
    if !index_metadata.file_type().is_file()
        || index_metadata.len() != entry.index_bytes
        || index_metadata.len() > MAX_PACK_INDEX_BYTES
    {
        return Ok(false);
    }
    let pack_metadata = match std::fs::symlink_metadata(pack_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(PocError::io(
                "stat cataloged semantic object pack",
                pack_path,
                error,
            ))
        }
    };
    if !pack_metadata.file_type().is_file()
        || pack_metadata.len() != entry.pack_bytes
        || pack_metadata.len() < u64::try_from(PACK_MAGIC.len()).unwrap_or(u64::MAX)
        || pack_metadata.len() > max_pack_file_bytes()
    {
        return Ok(false);
    }
    let mut index = File::open(index_path)
        .map_err(|error| PocError::io("open cataloged semantic pack index", index_path, error))?;
    let mut index_header = [0_u8; 12];
    index.read_exact(&mut index_header).map_err(|error| {
        PocError::io(
            "read cataloged semantic pack index header",
            index_path,
            error,
        )
    })?;
    if &index_header[..8] != PACK_INDEX_MAGIC
        || u32::from_be_bytes(index_header[8..12].try_into().unwrap_or([0_u8; 4]))
            != u32::try_from(entry.count).unwrap_or(u32::MAX)
    {
        return Ok(false);
    }
    let mut pack = File::open(pack_path)
        .map_err(|error| PocError::io("open cataloged semantic object pack", pack_path, error))?;
    let mut pack_magic = [0_u8; 8];
    pack.read_exact(&mut pack_magic).map_err(|error| {
        PocError::io(
            "read cataloged semantic object pack header",
            pack_path,
            error,
        )
    })?;
    Ok(&pack_magic == PACK_MAGIC)
}

fn write_pack_catalog(directory: &Path, indexes: &[PackedObjectIndex]) -> PocResult<()> {
    if indexes.is_empty() {
        return Ok(());
    }
    let Some(total_bytes) = pack_catalog_len(indexes.len()) else {
        return Ok(());
    };
    if total_bytes > MAX_PACK_CATALOG_BYTES {
        return Ok(());
    }
    let mut ordered = indexes
        .iter()
        .map(|index| Ok((pack_identifier_from_index_path(&index.index)?, index)))
        .collect::<PocResult<Vec<_>>>()?;
    ordered.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    let mut bytes = Vec::with_capacity(total_bytes);
    bytes.extend_from_slice(PACK_CATALOG_MAGIC);
    bytes.extend_from_slice(&PACK_CATALOG_VERSION.to_be_bytes());
    bytes.extend_from_slice(
        &u32::try_from(ordered.len())
            .map_err(|_| PocError::Integrity("semantic pack catalog count overflow".to_owned()))?
            .to_be_bytes(),
    );
    for (identifier, index) in ordered {
        bytes.extend_from_slice(identifier.as_bytes());
        bytes.extend_from_slice(
            &u32::try_from(index.count)
                .map_err(|_| {
                    PocError::Integrity("semantic pack catalog count overflow".to_owned())
                })?
                .to_be_bytes(),
        );
        bytes.extend_from_slice(
            &index
                .entries_end
                .checked_add(32)
                .ok_or_else(|| {
                    PocError::Integrity("semantic pack catalog index length overflow".to_owned())
                })?
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&index.pack_bytes.to_be_bytes());
        bytes.extend_from_slice(&index.index_checksum);
        for word in index.bloom.iter() {
            bytes.extend_from_slice(&word.to_be_bytes());
        }
    }
    let checksum: [u8; 32] = Sha256::digest(&bytes).into();
    bytes.extend_from_slice(&checksum);
    if bytes.len() != total_bytes {
        return Err(PocError::Integrity(
            "semantic pack catalog encoded length disagrees with its bound".to_owned(),
        ));
    }
    replace_pack_catalog(&directory.join(PACK_CATALOG_FILE), &bytes)
}

fn decode_pack_catalog(bytes: &[u8]) -> Option<Vec<PackCatalogEntry>> {
    if bytes.len() < PACK_CATALOG_HEADER_BYTES + PACK_CATALOG_TRAILER_BYTES
        || &bytes[..8] != PACK_CATALOG_MAGIC
        || u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?) != PACK_CATALOG_VERSION
    {
        return None;
    }
    let count = usize::try_from(u32::from_be_bytes(bytes.get(12..16)?.try_into().ok()?)).ok()?;
    let expected = pack_catalog_len(count)?;
    if expected != bytes.len() || expected > MAX_PACK_CATALOG_BYTES {
        return None;
    }
    let checksum_at = bytes.len().checked_sub(PACK_CATALOG_TRAILER_BYTES)?;
    let expected_checksum: [u8; 32] = bytes.get(checksum_at..)?.try_into().ok()?;
    let actual_checksum: [u8; 32] = Sha256::digest(&bytes[..checksum_at]).into();
    if actual_checksum != expected_checksum {
        return None;
    }
    let mut offset = PACK_CATALOG_HEADER_BYTES;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let identifier = Uuid::from_bytes(
            bytes
                .get(offset..offset.checked_add(16)?)?
                .try_into()
                .ok()?,
        );
        offset = offset.checked_add(16)?;
        let count = usize::try_from(u32::from_be_bytes(
            bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
        ))
        .ok()?;
        offset = offset.checked_add(4)?;
        let index_bytes =
            u64::from_be_bytes(bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?);
        offset = offset.checked_add(8)?;
        let pack_bytes =
            u64::from_be_bytes(bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?);
        offset = offset.checked_add(8)?;
        let index_checksum: [u8; 32] = bytes
            .get(offset..offset.checked_add(32)?)?
            .try_into()
            .ok()?;
        offset = offset.checked_add(32)?;
        let expected_index_bytes = 12_u64
            .checked_add(
                u64::try_from(count)
                    .ok()?
                    .checked_mul(u64::try_from(PACK_INDEX_ENTRY_BYTES).ok()?)?,
            )?
            .checked_add(32)?;
        if count == 0
            || count > MAX_PACK_OBJECTS
            || index_bytes != expected_index_bytes
            || index_bytes > MAX_PACK_INDEX_BYTES
            || pack_bytes < u64::try_from(PACK_MAGIC.len()).ok()?
            || pack_bytes > max_pack_file_bytes()
        {
            return None;
        }
        let mut bloom = vec![0_u64; PACK_BLOOM_WORDS].into_boxed_slice();
        for word in bloom.iter_mut() {
            *word = u64::from_be_bytes(bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?);
            offset = offset.checked_add(8)?;
        }
        entries.push(PackCatalogEntry {
            identifier,
            count,
            index_bytes,
            pack_bytes,
            index_checksum,
            bloom,
        });
    }
    (offset == checksum_at).then_some(entries)
}

fn pack_catalog_len(count: usize) -> Option<usize> {
    PACK_CATALOG_HEADER_BYTES
        .checked_add(count.checked_mul(PACK_CATALOG_ENTRY_BYTES)?)?
        .checked_add(PACK_CATALOG_TRAILER_BYTES)
}

fn pack_identifier_from_index_path(index_path: &Path) -> PocResult<Uuid> {
    let name = index_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| PocError::Integrity("semantic pack index name is not UTF-8".to_owned()))?;
    let identifier = name
        .strip_prefix("pack-")
        .and_then(|name| name.strip_suffix(".index"))
        .ok_or_else(|| PocError::Integrity("semantic pack index name is invalid".to_owned()))?;
    let parsed = Uuid::parse_str(identifier)
        .map_err(|_| PocError::Integrity("semantic pack index UUID is invalid".to_owned()))?;
    if parsed.to_string() != identifier {
        return Err(PocError::Integrity(
            "semantic pack index UUID is not canonical".to_owned(),
        ));
    }
    Ok(parsed)
}

fn replace_pack_catalog(path: &Path, bytes: &[u8]) -> PocResult<()> {
    let directory = path.parent().ok_or_else(|| {
        PocError::Integrity("semantic pack catalog has no parent directory".to_owned())
    })?;
    let temporary = directory.join(format!(".{PACK_CATALOG_FILE}-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                PocError::io("create semantic pack catalog temporary", &temporary, error)
            })?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| PocError::io("write semantic pack catalog", &temporary, error))?;
        drop(file);
        std::fs::rename(&temporary, path)
            .map_err(|error| PocError::io("replace semantic pack catalog", path, error))?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn max_pack_file_bytes() -> u64 {
    MAX_PACK_BYTES
        .saturating_add(u64::try_from(PACK_MAGIC.len()).unwrap_or(u64::MAX))
        .saturating_add(
            u64::try_from(MAX_PACK_OBJECTS)
                .unwrap_or(u64::MAX)
                .saturating_mul(4),
        )
}

fn pack_path_for_index(directory: &Path, name: &str) -> Option<PathBuf> {
    let identifier = name.strip_prefix("pack-")?.strip_suffix(".index")?;
    let parsed = Uuid::parse_str(identifier).ok()?;
    if parsed.to_string() != identifier {
        return None;
    }
    Some(directory.join(format!("pack-{parsed}.pack")))
}

fn read_pack_index(index_path: &Path, pack_path: PathBuf) -> PocResult<PackedObjectIndex> {
    #[cfg(test)]
    FULL_PACK_INDEX_VALIDATIONS.fetch_add(1, Ordering::Relaxed);
    let index_metadata = std::fs::symlink_metadata(index_path)
        .map_err(|error| PocError::io("stat semantic pack index", index_path, error))?;
    if !index_metadata.file_type().is_file()
        || index_metadata.len() < 44
        || index_metadata.len() > MAX_PACK_INDEX_BYTES
    {
        return Err(PocError::Integrity(
            "semantic pack index is not a bounded regular file".to_owned(),
        ));
    }
    let mut file = File::open(index_path)
        .map_err(|error| PocError::io("open semantic pack index", index_path, error))?;
    let mut header = [0_u8; 12];
    file.read_exact(&mut header)
        .map_err(|error| PocError::io("read semantic pack index header", index_path, error))?;
    if &header[..8] != PACK_INDEX_MAGIC {
        return Err(PocError::Integrity(
            "semantic pack index has an invalid header".to_owned(),
        ));
    }
    let count = usize::try_from(u32::from_be_bytes(header[8..12].try_into().map_err(
        |_| PocError::Integrity("semantic pack index count is truncated".to_owned()),
    )?))
    .map_err(|_| PocError::Integrity("semantic pack index count overflow".to_owned()))?;
    let body_len = 12_u64
        .checked_add(
            u64::try_from(count)
                .unwrap_or(u64::MAX)
                .saturating_mul(u64::try_from(PACK_INDEX_ENTRY_BYTES).unwrap_or(u64::MAX)),
        )
        .ok_or_else(|| PocError::Integrity("semantic pack index length overflow".to_owned()))?;
    let total_len = body_len
        .checked_add(32)
        .ok_or_else(|| PocError::Integrity("semantic pack index checksum overflow".to_owned()))?;
    if count == 0 || count > MAX_PACK_OBJECTS || total_len != index_metadata.len() {
        return Err(PocError::Integrity(
            "semantic pack index has an invalid exact length".to_owned(),
        ));
    }
    let pack_metadata = std::fs::symlink_metadata(&pack_path)
        .map_err(|error| PocError::io("stat semantic object pack", &pack_path, error))?;
    let max_pack_size = MAX_PACK_BYTES
        .saturating_add(u64::try_from(PACK_MAGIC.len()).unwrap_or(u64::MAX))
        .saturating_add(
            u64::try_from(MAX_PACK_OBJECTS)
                .unwrap_or(u64::MAX)
                .saturating_mul(4),
        );
    if !pack_metadata.file_type().is_file()
        || pack_metadata.len() < u64::try_from(PACK_MAGIC.len()).unwrap_or(u64::MAX)
        || pack_metadata.len() > max_pack_size
    {
        return Err(PocError::Integrity(
            "semantic object pack is not a bounded regular file".to_owned(),
        ));
    }
    let mut pack = File::open(&pack_path)
        .map_err(|error| PocError::io("open semantic object pack", &pack_path, error))?;
    let mut magic = [0_u8; 8];
    pack.read_exact(&mut magic)
        .map_err(|error| PocError::io("read semantic object pack header", &pack_path, error))?;
    if &magic != PACK_MAGIC {
        return Err(PocError::Integrity(
            "semantic object pack has an invalid header".to_owned(),
        ));
    }
    let mut previous_digest = None;
    let mut bloom = vec![0_u64; PACK_BLOOM_WORDS].into_boxed_slice();
    let mut expected_offset = u64::try_from(PACK_MAGIC.len().saturating_add(4)).unwrap_or(u64::MAX);
    let mut last_object_end = 0_u64;
    let mut checksum = Sha256::new();
    checksum.update(header);
    for _ in 0..count {
        let mut entry_bytes = [0_u8; PACK_INDEX_ENTRY_BYTES];
        file.read_exact(&mut entry_bytes)
            .map_err(|error| PocError::io("read semantic pack index entry", index_path, error))?;
        checksum.update(entry_bytes);
        let entry = decode_pack_index_entry(&entry_bytes)?;
        let digest = entry.digest;
        let offset = entry.offset;
        let length = entry.length;
        if previous_digest
            .as_ref()
            .is_some_and(|previous| previous >= &digest)
            || length == 0
            || u64::from(length) > MAX_OBJECT_BYTES
            || offset != expected_offset
        {
            return Err(PocError::Integrity(
                "semantic pack index entries are not canonical".to_owned(),
            ));
        }
        last_object_end = offset.checked_add(u64::from(length)).ok_or_else(|| {
            PocError::Integrity("semantic pack object offset overflow".to_owned())
        })?;
        expected_offset = last_object_end.checked_add(4).ok_or_else(|| {
            PocError::Integrity("semantic pack object offset overflow".to_owned())
        })?;
        previous_digest = Some(digest);
        pack_bloom_insert(&mut bloom, &digest);
    }
    let mut expected_checksum = [0_u8; 32];
    file.read_exact(&mut expected_checksum)
        .map_err(|error| PocError::io("read semantic pack index checksum", index_path, error))?;
    if checksum.finalize().as_slice() != expected_checksum {
        return Err(PocError::Integrity(
            "semantic pack index checksum mismatch".to_owned(),
        ));
    }
    if last_object_end != pack_metadata.len() {
        return Err(PocError::Integrity(
            "semantic object pack length disagrees with its index".to_owned(),
        ));
    }
    Ok(PackedObjectIndex {
        pack: pack_path,
        index: index_path.to_path_buf(),
        count,
        entries_end: body_len,
        index_checksum: expected_checksum,
        pack_bytes: pack_metadata.len(),
        bloom: bloom.into(),
    })
}

fn open_validated_pack_reader(pack_path: &Path, expected_pack_bytes: u64) -> PocResult<File> {
    let metadata = std::fs::symlink_metadata(pack_path)
        .map_err(|error| PocError::io("stat semantic object pack", pack_path, error))?;
    if !metadata.file_type().is_file() || metadata.len() != expected_pack_bytes {
        return Err(PocError::Integrity(
            "semantic object pack changed outside its validated form".to_owned(),
        ));
    }
    let file = File::open(pack_path)
        .map_err(|error| PocError::io("open semantic object pack", pack_path, error))?;
    let mut magic = [0_u8; 8];
    read_packed_exact_at(
        &file,
        &mut magic,
        0,
        pack_path,
        "read semantic object pack header",
    )?;
    if &magic != PACK_MAGIC {
        return Err(PocError::Integrity(
            "semantic object pack has an invalid header".to_owned(),
        ));
    }
    Ok(file)
}

fn read_validated_packed_object(
    file: &File,
    pack_path: &Path,
    pack_bytes: u64,
    entry: &PackedObjectEntry,
    expected: [u8; 32],
) -> PocResult<Vec<u8>> {
    if entry.digest != expected || entry.length == 0 || u64::from(entry.length) > MAX_OBJECT_BYTES {
        return Err(PocError::Integrity(
            "semantic pack object index is invalid".to_owned(),
        ));
    }
    let object_end = entry
        .offset
        .checked_add(u64::from(entry.length))
        .ok_or_else(|| PocError::Integrity("semantic packed object end overflow".to_owned()))?;
    if entry.offset < 12 || object_end > pack_bytes {
        return Err(PocError::Integrity(
            "semantic packed object lies outside its pack".to_owned(),
        ));
    }
    let mut encoded_length = [0_u8; 4];
    read_packed_exact_at(
        file,
        &mut encoded_length,
        entry.offset.saturating_sub(4),
        pack_path,
        "read semantic packed object length",
    )?;
    if u32::from_be_bytes(encoded_length) != entry.length {
        return Err(PocError::Integrity(
            "semantic packed object length disagrees with its index".to_owned(),
        ));
    }
    let capacity = usize::try_from(entry.length)
        .map_err(|_| PocError::Integrity("semantic packed object size overflow".to_owned()))?;
    let mut bytes = vec![0_u8; capacity];
    read_packed_exact_at(
        file,
        &mut bytes,
        entry.offset,
        pack_path,
        "read semantic packed object",
    )?;
    if object_digest(&bytes) != expected {
        return Err(PocError::Integrity(
            "semantic packed object digest mismatch".to_owned(),
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn read_packed_exact_at(
    file: &File,
    bytes: &mut [u8],
    offset: u64,
    pack_path: &Path,
    context: &'static str,
) -> PocResult<()> {
    file.read_exact_at(bytes, offset)
        .map_err(|error| PocError::io(context, pack_path, error))
}

#[cfg(not(unix))]
fn read_packed_exact_at(
    file: &File,
    bytes: &mut [u8],
    offset: u64,
    pack_path: &Path,
    context: &'static str,
) -> PocResult<()> {
    let mut reader = file
        .try_clone()
        .map_err(|error| PocError::io("clone semantic object pack", pack_path, error))?;
    reader
        .seek(SeekFrom::Start(offset))
        .and_then(|()| reader.read_exact(bytes))
        .map_err(|error| PocError::io(context, pack_path, error))
}

fn read_packed_object(
    pack_path: &Path,
    entry: &PackedObjectEntry,
    expected: [u8; 32],
) -> PocResult<Vec<u8>> {
    if entry.digest != expected || entry.length == 0 || u64::from(entry.length) > MAX_OBJECT_BYTES {
        return Err(PocError::Integrity(
            "semantic pack object index is invalid".to_owned(),
        ));
    }
    let metadata = std::fs::symlink_metadata(pack_path)
        .map_err(|error| PocError::io("stat semantic object pack", pack_path, error))?;
    let max_pack_size = MAX_PACK_BYTES
        .saturating_add(u64::try_from(PACK_MAGIC.len()).unwrap_or(u64::MAX))
        .saturating_add(
            u64::try_from(MAX_PACK_OBJECTS)
                .unwrap_or(u64::MAX)
                .saturating_mul(4),
        );
    if !metadata.file_type().is_file() || metadata.len() > max_pack_size || entry.offset < 12 {
        return Err(PocError::Integrity(
            "semantic object pack changed outside its valid form".to_owned(),
        ));
    }
    let object_end = entry
        .offset
        .checked_add(u64::from(entry.length))
        .ok_or_else(|| PocError::Integrity("semantic packed object end overflow".to_owned()))?;
    if object_end > metadata.len() {
        return Err(PocError::Integrity(
            "semantic packed object lies outside its pack".to_owned(),
        ));
    }
    let mut file = File::open(pack_path)
        .map_err(|error| PocError::io("open semantic object pack", pack_path, error))?;
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)
        .map_err(|error| PocError::io("read semantic object pack header", pack_path, error))?;
    if &magic != PACK_MAGIC {
        return Err(PocError::Integrity(
            "semantic object pack has an invalid header".to_owned(),
        ));
    }
    file.seek(SeekFrom::Start(entry.offset.saturating_sub(4)))
        .map_err(|error| PocError::io("seek semantic packed object", pack_path, error))?;
    let mut encoded_length = [0_u8; 4];
    file.read_exact(&mut encoded_length)
        .map_err(|error| PocError::io("read semantic packed object length", pack_path, error))?;
    if u32::from_be_bytes(encoded_length) != entry.length {
        return Err(PocError::Integrity(
            "semantic packed object length disagrees with its index".to_owned(),
        ));
    }
    let capacity = usize::try_from(entry.length)
        .map_err(|_| PocError::Integrity("semantic packed object size overflow".to_owned()))?;
    let mut bytes = vec![0_u8; capacity];
    file.read_exact(&mut bytes)
        .map_err(|error| PocError::io("read semantic packed object", pack_path, error))?;
    if object_digest(&bytes) != expected {
        return Err(PocError::Integrity(
            "semantic packed object digest mismatch".to_owned(),
        ));
    }
    Ok(bytes)
}

fn load_existing_digest_cache(objects: &Path) -> PocResult<Option<Vec<[u8; 32]>>> {
    let mut digests = Vec::new();
    for prefix in 0..=u8::MAX {
        let directory = objects.join(format!("{prefix:02x}"));
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(PocError::io(
                    "read semantic object shard",
                    &directory,
                    error,
                ));
            }
        };
        for entry in entries {
            let entry = entry
                .map_err(|error| PocError::io("read semantic object entry", &directory, error))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(digest) = parse_hex_digest(&name) else {
                continue;
            };
            if digest[0] != prefix {
                continue;
            }
            digests.push(digest);
            if digests.len() > MAX_CACHED_EXISTING_OBJECTS {
                return Ok(None);
            }
        }
    }
    digests.sort_unstable();
    digests.dedup();
    Ok(Some(digests))
}

pub fn build_from_sorted_records(
    sorted: &SortedSpool,
    attribution_input: &AttributionInput,
    store: &mut ImmutableObjectStore,
) -> PocResult<TrieRoots> {
    let mut attribution_store = store.fork()?;
    let (content, attribution) = std::thread::scope(|scope| {
        let attribution_handle = scope.spawn(|| {
            build_kind(
                sorted,
                attribution_input,
                TrieKind::Attribution,
                &mut attribution_store,
            )
        });
        let content = build_kind(sorted, attribution_input, TrieKind::Content, store)?;
        let attribution = attribution_handle
            .join()
            .map_err(|_| PocError::Integrity("attribution trie worker panicked".to_owned()))??;
        Ok::<_, PocError>((content, attribution))
    })?;
    store.absorb_parallel(attribution_store);
    Ok(TrieRoots {
        content,
        attribution,
    })
}

/// Build both canonical tries without creating a worker thread.
///
/// The confined holder-namespace storage helper deliberately installs a
/// process-creation-denying seccomp policy before semantic work begins. Its
/// fixture-attestation path therefore uses this sequential reducer; the
/// regular publication path retains the parallel reducer above.
pub fn build_from_sorted_records_serial(
    sorted: &SortedSpool,
    attribution_input: &AttributionInput,
    store: &mut ImmutableObjectStore,
) -> PocResult<TrieRoots> {
    let content = build_kind(sorted, attribution_input, TrieKind::Content, store)?;
    let attribution = build_kind(sorted, attribution_input, TrieKind::Attribution, store)?;
    Ok(TrieRoots {
        content,
        attribution,
    })
}

fn build_kind(
    sorted: &SortedSpool,
    attribution_input: &AttributionInput,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<[u8; 32]> {
    let mut builder = StreamingTrieBuilder::new(kind);
    sorted.for_each(|key, payload| {
        if key.len() != 32 {
            return Err(PocError::Integrity(
                "semantic record spool key is not SHA-256".to_owned(),
            ));
        }
        let key_digest: [u8; 32] = key
            .try_into()
            .map_err(|_| PocError::Integrity("semantic key digest length mismatch".to_owned()))?;
        let record = SemanticRecord::decode(payload)?;
        if record.key_digest()? != key_digest {
            return Err(PocError::Integrity(
                "semantic record and sorted key disagree".to_owned(),
            ));
        }
        let leaf = match kind {
            TrieKind::Content => object_digest(&encode_content_leaf(
                key_digest,
                &record.canonical_key()?,
                payload,
            )?),
            TrieKind::Attribution => object_digest(&encode_attribution_leaf(
                key_digest,
                attribution::leaf_digest(record.record_digest()?, attribution_input),
            )),
        };
        builder.add(ChildRef::leaf(key_digest, leaf), store)
    })?;
    builder.finish(store)
}

pub fn apply_mutation(
    roots: &TrieRoots,
    mutation: &RecordMutation,
    attribution_input: &AttributionInput,
    store: &mut ImmutableObjectStore,
) -> PocResult<MutationOutcome> {
    let outcome = apply_mutation_batch(
        roots,
        std::slice::from_ref(mutation),
        attribution_input,
        store,
    )?;
    let existed = match mutation {
        RecordMutation::Replace(_) => outcome.entry_count_delta == 0,
        RecordMutation::Delete { .. } => outcome.entry_count_delta < 0,
    };
    Ok(MutationOutcome {
        roots: outcome.roots,
        existed,
    })
}

pub fn apply_mutation_batch(
    roots: &TrieRoots,
    mutations: &[RecordMutation],
    attribution_input: &AttributionInput,
    store: &mut ImmutableObjectStore,
) -> PocResult<MutationBatchOutcome> {
    if mutations.is_empty() {
        return Err(PocError::Integrity(
            "semantic mutation batch must not be empty".to_owned(),
        ));
    }
    let mut previous_key = None;
    let mut content_mutations = Vec::with_capacity(mutations.len());
    let mut attribution_mutations = Vec::with_capacity(mutations.len());
    for (index, mutation) in mutations.iter().enumerate() {
        let canonical_key = mutation.canonical_key()?;
        let key_digest = digest_key(&canonical_key)?;
        if previous_key
            .as_ref()
            .is_some_and(|previous| previous >= &key_digest)
        {
            return Err(PocError::Integrity(
                "semantic mutation batch is not strictly key-sorted".to_owned(),
            ));
        }
        previous_key = Some(key_digest);
        let content_replacement = match mutation {
            RecordMutation::Replace(record) => Some(ChildRef::leaf(
                key_digest,
                object_digest(&encode_content_leaf(
                    key_digest,
                    &canonical_key,
                    &record.encode()?,
                )?),
            )),
            RecordMutation::Delete { .. } => None,
        };
        let attribution_replacement = match mutation {
            RecordMutation::Replace(record) => Some(ChildRef::leaf(
                key_digest,
                object_digest(&encode_attribution_leaf(
                    key_digest,
                    attribution::leaf_digest(record.record_digest()?, attribution_input),
                )),
            )),
            RecordMutation::Delete { .. } => None,
        };
        content_mutations.push(PreparedMutation {
            key: key_digest,
            replacement: content_replacement,
            index,
        });
        attribution_mutations.push(PreparedMutation {
            key: key_digest,
            replacement: attribution_replacement,
            index,
        });
    }

    let (content, content_existed) =
        apply_prepared_mutation_batch(roots.content, &content_mutations, TrieKind::Content, store)?;
    let (attribution, attribution_existed) = apply_prepared_mutation_batch(
        roots.attribution,
        &attribution_mutations,
        TrieKind::Attribution,
        store,
    )?;
    if content_existed != attribution_existed {
        return Err(PocError::Integrity(
            "content and attribution tries disagree about canonical key existence".to_owned(),
        ));
    }

    let mut entry_count_delta = 0_i64;
    for (mutation, existed) in mutations.iter().zip(content_existed) {
        match (existed, mutation) {
            (false, RecordMutation::Replace(_)) => entry_count_delta += 1,
            (true, RecordMutation::Delete { .. }) => entry_count_delta -= 1,
            (false, RecordMutation::Delete { .. }) => {
                return Err(PocError::Integrity(
                    "incremental delete names a missing canonical key".to_owned(),
                ));
            }
            (true, RecordMutation::Replace(_)) => {}
        }
    }
    Ok(MutationBatchOutcome {
        roots: TrieRoots {
            content,
            attribution,
        },
        entry_count_delta,
    })
}

#[derive(Clone, Copy)]
struct PreparedMutation {
    key: [u8; 32],
    replacement: Option<ChildRef>,
    index: usize,
}

#[derive(Clone, Copy)]
struct BatchChild {
    child: ChildRef,
    prefix_depth: usize,
}

fn apply_prepared_mutation_batch(
    root: [u8; 32],
    mutations: &[PreparedMutation],
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<([u8; 32], Vec<bool>)> {
    let mut frame = load_root_frame(root, kind, store)?;
    let mut existed = vec![false; mutations.len()];
    update_frame_batch(&mut frame, mutations, kind, store, &mut existed)?;
    Ok((install_root_frame(frame, kind, store)?, existed))
}

fn update_frame_batch(
    frame: &mut Frame,
    mutations: &[PreparedMutation],
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
    existed: &mut [bool],
) -> PocResult<()> {
    let mut start = 0_usize;
    while start < mutations.len() {
        let index = nibble(&mutations[start].key, frame.depth);
        let mut end = start.saturating_add(1);
        while end < mutations.len() && nibble(&mutations[end].key, frame.depth) == index {
            end = end.saturating_add(1);
        }
        frame.children[index] = update_child_batch(
            frame.children[index],
            &mutations[start..end],
            frame.depth,
            kind,
            store,
            existed,
        )?
        .map(|child| child.child);
        start = end;
    }
    Ok(())
}

fn update_child_batch(
    existing: Option<ChildRef>,
    mutations: &[PreparedMutation],
    parent_depth: usize,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
    existed: &mut [bool],
) -> PocResult<Option<BatchChild>> {
    let Some(existing) = existing else {
        let mut additions = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            let replacement = mutation.replacement.ok_or_else(|| {
                PocError::Integrity("incremental delete names a missing canonical key".to_owned())
            })?;
            existed[mutation.index] = false;
            additions.push(BatchChild {
                child: replacement,
                prefix_depth: TRIE_DEPTH,
            });
        }
        return build_batch_child(additions, parent_depth, kind, store);
    };
    existing.validate_for_parent(parent_depth)?;
    match existing.kind {
        ChildKind::Leaf => {
            let mut children = Vec::with_capacity(mutations.len().saturating_add(1));
            let mut keep_existing = true;
            for mutation in mutations {
                if mutation.key == existing.min_key {
                    existed[mutation.index] = true;
                    keep_existing = false;
                    if let Some(replacement) = mutation.replacement {
                        children.push(BatchChild {
                            child: replacement,
                            prefix_depth: TRIE_DEPTH,
                        });
                    }
                } else {
                    let replacement = mutation.replacement.ok_or_else(|| {
                        PocError::Integrity(
                            "incremental delete names a missing canonical key".to_owned(),
                        )
                    })?;
                    existed[mutation.index] = false;
                    children.push(BatchChild {
                        child: replacement,
                        prefix_depth: TRIE_DEPTH,
                    });
                }
            }
            if keep_existing {
                children.push(BatchChild {
                    child: existing,
                    prefix_depth: TRIE_DEPTH,
                });
            }
            build_batch_child(children, parent_depth, kind, store)
        }
        ChildKind::Node => {
            let mut frame = decode_node(&store.load(existing.digest)?, kind, None, false)?;
            if frame.min_key()? != existing.min_key || frame.depth <= parent_depth {
                return Err(PocError::Integrity(
                    "semantic compressed trie child summary mismatch".to_owned(),
                ));
            }
            let mut inner_start = 0_usize;
            while inner_start < mutations.len()
                && common_nibbles(&existing.min_key, &mutations[inner_start].key) < frame.depth
            {
                inner_start = inner_start.saturating_add(1);
            }
            let mut inner_end = inner_start;
            while inner_end < mutations.len()
                && common_nibbles(&existing.min_key, &mutations[inner_end].key) >= frame.depth
            {
                inner_end = inner_end.saturating_add(1);
            }
            let retained = if inner_start == inner_end {
                Some(BatchChild {
                    child: existing,
                    prefix_depth: frame.depth,
                })
            } else {
                update_frame_batch(
                    &mut frame,
                    &mutations[inner_start..inner_end],
                    kind,
                    store,
                    existed,
                )?;
                normalize_batch_frame(frame, parent_depth, kind, store)?
            };
            let mut children = Vec::with_capacity(
                mutations
                    .len()
                    .saturating_sub(inner_end.saturating_sub(inner_start))
                    .saturating_add(usize::from(retained.is_some())),
            );
            for mutation in mutations[..inner_start]
                .iter()
                .chain(mutations[inner_end..].iter())
            {
                let replacement = mutation.replacement.ok_or_else(|| {
                    PocError::Integrity(
                        "incremental delete names a missing canonical key".to_owned(),
                    )
                })?;
                existed[mutation.index] = false;
                children.push(BatchChild {
                    child: replacement,
                    prefix_depth: TRIE_DEPTH,
                });
            }
            if let Some(retained) = retained {
                children.push(retained);
            }
            build_batch_child(children, parent_depth, kind, store)
        }
    }
}

fn normalize_batch_frame(
    frame: Frame,
    parent_depth: usize,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<Option<BatchChild>> {
    let count = frame.child_count();
    if count == 0 {
        return Ok(None);
    }
    if count == 1 {
        let child = frame.only_child().ok_or_else(|| {
            PocError::Integrity("semantic trie unary frame lost its child".to_owned())
        })?;
        child.validate_for_parent(parent_depth)?;
        return batch_child_from_ref(child, kind, store).map(Some);
    }
    if frame.depth <= parent_depth {
        return Err(PocError::Integrity(
            "semantic compressed trie branch escaped its parent".to_owned(),
        ));
    }
    let depth = frame.depth;
    let child = install_frame(frame, kind, store)?;
    Ok(Some(BatchChild {
        child,
        prefix_depth: depth,
    }))
}

fn build_batch_child(
    mut children: Vec<BatchChild>,
    parent_depth: usize,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<Option<BatchChild>> {
    if children.is_empty() {
        return Ok(None);
    }
    children.sort_unstable_by_key(|child| child.child.min_key);
    if children.len() == 1 {
        let child = children.pop().ok_or_else(|| {
            PocError::Integrity("semantic trie batch child disappeared".to_owned())
        })?;
        child.child.validate_for_parent(parent_depth)?;
        return Ok(Some(child));
    }
    for pair in children.windows(2) {
        if pair[0].child.min_key >= pair[1].child.min_key {
            return Err(PocError::Integrity(
                "semantic trie batch has duplicate canonical keys".to_owned(),
            ));
        }
    }
    let first = children[0].child.min_key;
    let common_depth = children.iter().skip(1).fold(TRIE_DEPTH, |depth, child| {
        depth.min(common_nibbles(&first, &child.child.min_key))
    });
    let depth = children
        .iter()
        .map(|child| child.prefix_depth)
        .fold(common_depth, usize::min);
    if depth <= parent_depth || depth >= TRIE_DEPTH {
        return Err(PocError::Integrity(
            "semantic trie batch branch depth is invalid".to_owned(),
        ));
    }
    let mut groups: [Vec<BatchChild>; FAN_OUT] = std::array::from_fn(|_| Vec::new());
    for child in children {
        if child.prefix_depth == depth && child.child.kind == ChildKind::Node {
            let frame = decode_node(&store.load(child.child.digest)?, kind, Some(depth), false)?;
            if frame.min_key()? != child.child.min_key {
                return Err(PocError::Integrity(
                    "semantic trie batch node summary mismatch".to_owned(),
                ));
            }
            for nested in frame.children.into_iter().flatten() {
                let nested = batch_child_from_ref(nested, kind, store)?;
                groups[nibble(&nested.child.min_key, depth)].push(nested);
            }
        } else {
            groups[nibble(&child.child.min_key, depth)].push(child);
        }
    }
    let mut frame = Frame::new(depth);
    for (index, group) in groups.into_iter().enumerate() {
        let Some(child) = build_batch_child(group, depth, kind, store)? else {
            continue;
        };
        frame.children[index] = Some(child.child);
    }
    normalize_batch_frame(frame, parent_depth, kind, store)
}

fn batch_child_from_ref(
    child: ChildRef,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<BatchChild> {
    let prefix_depth = match child.kind {
        ChildKind::Leaf => TRIE_DEPTH,
        ChildKind::Node => {
            let frame = decode_node(&store.load(child.digest)?, kind, None, false)?;
            if frame.min_key()? != child.min_key {
                return Err(PocError::Integrity(
                    "semantic trie child summary mismatch".to_owned(),
                ));
            }
            frame.depth
        }
    };
    Ok(BatchChild {
        child,
        prefix_depth,
    })
}

pub(super) fn validate_roots(roots: &TrieRoots, store: &mut ImmutableObjectStore) -> PocResult<()> {
    validate_root(roots.content, TrieKind::Content, store)?;
    validate_root(roots.attribution, TrieKind::Attribution, store)
}

pub fn visit_records(
    roots: &TrieRoots,
    store: &mut ImmutableObjectStore,
    mut visitor: impl FnMut(SemanticRecord) -> PocResult<()>,
) -> PocResult<()> {
    let mut previous = None;
    visit_root(
        roots.content,
        TrieKind::Content,
        store,
        &mut |key_digest, bytes| {
            if previous
                .as_ref()
                .is_some_and(|value: &[u8; 32]| value >= &key_digest)
            {
                return Err(PocError::Integrity(
                    "semantic trie traversal is not strictly ordered".to_owned(),
                ));
            }
            let record = SemanticRecord::decode(bytes)?;
            if record.key_digest()? != key_digest {
                return Err(PocError::Integrity(
                    "semantic trie leaf key does not match record".to_owned(),
                ));
            }
            previous = Some(key_digest);
            visitor(record)
        },
    )
}

fn validate_root(
    root: [u8; 32],
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<()> {
    if root != empty_node_digest(kind)? {
        decode_node(&store.load(root)?, kind, Some(0), true)?;
    }
    Ok(())
}

fn load_root_frame(
    root: [u8; 32],
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<Frame> {
    if root == empty_node_digest(kind)? {
        Ok(Frame::new(0))
    } else {
        decode_node(&store.load(root)?, kind, Some(0), true)
    }
}

fn install_root_frame(
    frame: Frame,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<[u8; 32]> {
    if frame.children.iter().all(Option::is_none) {
        empty_node_digest(kind)
    } else {
        store.install(&encode_node(&frame, kind, true)?)
    }
}

fn visit_root(
    digest: [u8; 32],
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
    visitor: &mut impl FnMut([u8; 32], &[u8]) -> PocResult<()>,
) -> PocResult<()> {
    if digest == empty_node_digest(kind)? {
        return Ok(());
    }
    let frame = decode_node(&store.load(digest)?, kind, Some(0), true)?;
    for child in frame.children.into_iter().flatten() {
        visit_child(child, 0, kind, store, visitor)?;
    }
    Ok(())
}

fn visit_child(
    child: ChildRef,
    parent_depth: usize,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
    visitor: &mut impl FnMut([u8; 32], &[u8]) -> PocResult<()>,
) -> PocResult<()> {
    child.validate_for_parent(parent_depth)?;
    match child.kind {
        ChildKind::Leaf => {
            if kind != TrieKind::Content {
                return Err(PocError::Integrity(
                    "attribution trie cannot materialize content records".to_owned(),
                ));
            }
            let leaf = store.load(child.digest)?;
            let decoded = decode_content_leaf(&leaf)?;
            if decoded.key_digest != child.min_key {
                return Err(PocError::Integrity(
                    "semantic trie leaf summary mismatch".to_owned(),
                ));
            }
            visitor(decoded.key_digest, decoded.record)
        }
        ChildKind::Node => {
            let frame = decode_node(&store.load(child.digest)?, kind, None, false)?;
            if frame.depth <= parent_depth || frame.min_key()? != child.min_key {
                return Err(PocError::Integrity(
                    "semantic compressed trie child node mismatch".to_owned(),
                ));
            }
            for grandchild in frame.children.into_iter().flatten() {
                visit_child(grandchild, frame.depth, kind, store, visitor)?;
            }
            Ok(())
        }
    }
}

struct StreamingTrieBuilder {
    kind: TrieKind,
    previous: Option<[u8; 32]>,
    frames: Vec<Frame>,
}

impl StreamingTrieBuilder {
    fn new(kind: TrieKind) -> Self {
        Self {
            kind,
            previous: None,
            frames: Vec::with_capacity(TRIE_DEPTH),
        }
    }

    fn add(&mut self, leaf: ChildRef, store: &mut ImmutableObjectStore) -> PocResult<()> {
        let key = leaf.min_key;
        if let Some(previous) = self.previous {
            if previous >= key {
                return Err(PocError::Integrity(
                    "semantic trie keys are not strictly ordered".to_owned(),
                ));
            }
            let common = common_nibbles(&previous, &key);
            while self.frames.len() > common + 1 {
                self.finish_deepest(previous, store)?;
            }
            while self.frames.len() < TRIE_DEPTH {
                self.frames.push(Frame::new(self.frames.len()));
            }
        } else {
            for depth in 0..TRIE_DEPTH {
                self.frames.push(Frame::new(depth));
            }
        }
        let last = self
            .frames
            .last_mut()
            .ok_or_else(|| PocError::Integrity("semantic trie frame stack is empty".to_owned()))?;
        last.insert(leaf)?;
        self.previous = Some(key);
        Ok(())
    }

    fn finish(mut self, store: &mut ImmutableObjectStore) -> PocResult<[u8; 32]> {
        let Some(previous) = self.previous else {
            return empty_node_digest(self.kind);
        };
        while self.frames.len() > 1 {
            self.finish_deepest(previous, store)?;
        }
        let root = self
            .frames
            .pop()
            .ok_or_else(|| PocError::Integrity("semantic trie root frame is absent".to_owned()))?;
        store.install(&encode_node(&root, self.kind, true)?)
    }

    fn finish_deepest(
        &mut self,
        previous: [u8; 32],
        store: &mut ImmutableObjectStore,
    ) -> PocResult<()> {
        let frame = self
            .frames
            .pop()
            .ok_or_else(|| PocError::Integrity("semantic trie frame underflow".to_owned()))?;
        let depth = frame.depth;
        if depth == 0 {
            return Err(PocError::Integrity(
                "semantic trie attempted to finalize root early".to_owned(),
            ));
        }
        let child = if frame.child_count() == 1 {
            frame.only_child().ok_or_else(|| {
                PocError::Integrity("semantic trie unary frame lost its child".to_owned())
            })?
        } else {
            install_frame(frame, self.kind, store)?
        };
        let parent = self
            .frames
            .last_mut()
            .ok_or_else(|| PocError::Integrity("semantic trie parent is absent".to_owned()))?;
        if child.min_key > previous {
            return Err(PocError::Integrity(
                "semantic trie child minimum exceeds finalized key".to_owned(),
            ));
        }
        parent.insert(child)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrieKind {
    Content,
    Attribution,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum ChildKind {
    Leaf = 1,
    Node = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChildRef {
    kind: ChildKind,
    min_key: [u8; 32],
    digest: [u8; 32],
}

impl ChildRef {
    const fn leaf(min_key: [u8; 32], digest: [u8; 32]) -> Self {
        Self {
            kind: ChildKind::Leaf,
            min_key,
            digest,
        }
    }

    fn validate_for_parent(&self, parent_depth: usize) -> PocResult<()> {
        if parent_depth >= TRIE_DEPTH {
            return Err(PocError::Integrity(
                "semantic trie parent depth exceeds bound".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Frame {
    depth: usize,
    children: [Option<ChildRef>; FAN_OUT],
}

impl Frame {
    fn new(depth: usize) -> Self {
        Self {
            depth,
            children: [None; FAN_OUT],
        }
    }

    fn insert(&mut self, child: ChildRef) -> PocResult<()> {
        let index = nibble(&child.min_key, self.depth);
        if self.children[index].replace(child).is_some() {
            return Err(PocError::Integrity(
                "duplicate semantic trie child edge".to_owned(),
            ));
        }
        Ok(())
    }

    fn child_count(&self) -> usize {
        self.children.iter().flatten().count()
    }

    fn only_child(&self) -> Option<ChildRef> {
        let mut children = self.children.iter().flatten().copied();
        let child = children.next()?;
        if children.next().is_some() {
            None
        } else {
            Some(child)
        }
    }

    fn min_key(&self) -> PocResult<[u8; 32]> {
        self.children
            .iter()
            .flatten()
            .map(|child| child.min_key)
            .min()
            .ok_or_else(|| PocError::Integrity("semantic trie node has no minimum key".to_owned()))
    }
}

struct ContentLeaf<'a> {
    key_digest: [u8; 32],
    record: &'a [u8],
}

fn install_frame(
    frame: Frame,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<ChildRef> {
    let min_key = frame.min_key()?;
    let digest = store.install(&encode_node(&frame, kind, false)?)?;
    Ok(ChildRef {
        kind: ChildKind::Node,
        min_key,
        digest,
    })
}

fn encode_node(frame: &Frame, kind: TrieKind, root: bool) -> PocResult<Vec<u8>> {
    if frame.depth >= TRIE_DEPTH {
        return Err(PocError::Integrity(
            "semantic trie node depth exceeds bound".to_owned(),
        ));
    }
    let count = frame.child_count();
    if count == 0 || (!root && count < 2) || (root && frame.depth != 0) {
        return Err(PocError::Integrity(
            "semantic compressed trie node cardinality is invalid".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(10 + count * 66);
    bytes.extend_from_slice(node_magic(kind));
    bytes.push(
        u8::try_from(frame.depth)
            .map_err(|_| PocError::Integrity("semantic trie depth overflow".to_owned()))?,
    );
    bytes.push(
        u8::try_from(count)
            .map_err(|_| PocError::Integrity("semantic trie fan-out overflow".to_owned()))?,
    );
    for (index, child) in frame.children.iter().enumerate() {
        if let Some(child) = child {
            bytes.push(
                u8::try_from(index)
                    .map_err(|_| PocError::Integrity("semantic trie index overflow".to_owned()))?,
            );
            bytes.push(child.kind as u8);
            bytes.extend_from_slice(&child.min_key);
            bytes.extend_from_slice(&child.digest);
        }
    }
    Ok(bytes)
}

fn decode_node(
    bytes: &[u8],
    kind: TrieKind,
    expected_depth: Option<usize>,
    root: bool,
) -> PocResult<Frame> {
    if bytes.len() < 10 || &bytes[..8] != node_magic(kind) {
        return Err(PocError::Integrity(
            "semantic trie node has wrong type or is truncated".to_owned(),
        ));
    }
    let depth = usize::from(bytes[8]);
    if expected_depth.is_some_and(|expected| depth != expected) {
        return Err(PocError::Integrity(
            "semantic trie node depth mismatch".to_owned(),
        ));
    }
    let count = usize::from(bytes[9]);
    if count == 0
        || count > FAN_OUT
        || (!root && count < 2)
        || (root && depth != 0)
        || bytes.len() != 10 + count * 66
    {
        return Err(PocError::Integrity(
            "semantic trie node fan-out is invalid".to_owned(),
        ));
    }
    let mut frame = Frame::new(depth);
    let mut offset = 10;
    let mut previous = None;
    for _ in 0..count {
        let index = usize::from(bytes[offset]);
        if index >= FAN_OUT || previous.is_some_and(|value| value >= index) {
            return Err(PocError::Integrity(
                "semantic trie node children are not canonical".to_owned(),
            ));
        }
        let child_kind = match bytes[offset + 1] {
            1 => ChildKind::Leaf,
            2 => ChildKind::Node,
            _ => {
                return Err(PocError::Integrity(
                    "semantic trie child kind is invalid".to_owned(),
                ))
            }
        };
        let min_key: [u8; 32] = bytes[offset + 2..offset + 34]
            .try_into()
            .map_err(|_| PocError::Integrity("semantic trie child key is truncated".to_owned()))?;
        let digest: [u8; 32] = bytes[offset + 34..offset + 66].try_into().map_err(|_| {
            PocError::Integrity("semantic trie child digest is truncated".to_owned())
        })?;
        if nibble(&min_key, depth) != index {
            return Err(PocError::Integrity(
                "semantic trie child edge and minimum key disagree".to_owned(),
            ));
        }
        frame.children[index] = Some(ChildRef {
            kind: child_kind,
            min_key,
            digest,
        });
        previous = Some(index);
        offset += 66;
    }
    Ok(frame)
}

fn encode_content_leaf(
    key_digest: [u8; 32],
    canonical_key: &[u8],
    record: &[u8],
) -> PocResult<Vec<u8>> {
    if canonical_key.is_empty()
        || canonical_key.len() > MAX_KEY_BYTES
        || record.is_empty()
        || record.len() > MAX_RECORD_BYTES
    {
        return Err(PocError::Integrity(
            "semantic content leaf exceeds bounds".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(48 + canonical_key.len() + record.len());
    bytes.extend_from_slice(CONTENT_LEAF_MAGIC);
    bytes.extend_from_slice(&key_digest);
    bytes.extend_from_slice(
        &u32::try_from(canonical_key.len())
            .map_err(|_| PocError::Integrity("semantic leaf key overflow".to_owned()))?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(canonical_key);
    bytes.extend_from_slice(
        &u32::try_from(record.len())
            .map_err(|_| PocError::Integrity("semantic leaf record overflow".to_owned()))?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(record);
    Ok(bytes)
}

fn decode_content_leaf(bytes: &[u8]) -> PocResult<ContentLeaf<'_>> {
    if bytes.len() < 48 || &bytes[..8] != CONTENT_LEAF_MAGIC {
        return Err(PocError::Integrity(
            "semantic content leaf has wrong type or is truncated".to_owned(),
        ));
    }
    let key_digest: [u8; 32] = bytes[8..40]
        .try_into()
        .map_err(|_| PocError::Integrity("semantic leaf key digest is truncated".to_owned()))?;
    let key_length =
        usize::try_from(u32::from_be_bytes(bytes[40..44].try_into().map_err(
            |_| PocError::Integrity("semantic leaf key length is truncated".to_owned()),
        )?))
        .map_err(|_| PocError::Integrity("semantic leaf key length overflow".to_owned()))?;
    let key_end = 44_usize
        .checked_add(key_length)
        .ok_or_else(|| PocError::Integrity("semantic leaf key offset overflow".to_owned()))?;
    let length_end = key_end
        .checked_add(4)
        .ok_or_else(|| PocError::Integrity("semantic leaf record offset overflow".to_owned()))?;
    let record_length = usize::try_from(u32::from_be_bytes(
        bytes
            .get(key_end..length_end)
            .ok_or_else(|| PocError::Integrity("semantic leaf record length missing".to_owned()))?
            .try_into()
            .map_err(|_| PocError::Integrity("semantic leaf record length invalid".to_owned()))?,
    ))
    .map_err(|_| PocError::Integrity("semantic leaf record length overflow".to_owned()))?;
    let record_end = length_end
        .checked_add(record_length)
        .ok_or_else(|| PocError::Integrity("semantic leaf record end overflow".to_owned()))?;
    if key_length == 0
        || key_length > MAX_KEY_BYTES
        || record_length == 0
        || record_length > MAX_RECORD_BYTES
        || record_end != bytes.len()
        || digest_key(&bytes[44..key_end])? != key_digest
    {
        return Err(PocError::Integrity(
            "semantic content leaf is not canonical".to_owned(),
        ));
    }
    Ok(ContentLeaf {
        key_digest,
        record: &bytes[length_end..record_end],
    })
}

fn encode_attribution_leaf(key_digest: [u8; 32], value_digest: [u8; 32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(72);
    bytes.extend_from_slice(ATTR_LEAF_MAGIC);
    bytes.extend_from_slice(&key_digest);
    bytes.extend_from_slice(&value_digest);
    bytes
}

fn empty_node_digest(kind: TrieKind) -> PocResult<[u8; 32]> {
    let mut bytes = Vec::with_capacity(10);
    bytes.extend_from_slice(node_magic(kind));
    bytes.push(0);
    bytes.push(0);
    Ok(object_digest(&bytes))
}

fn node_magic(kind: TrieKind) -> &'static [u8; 8] {
    match kind {
        TrieKind::Content => CONTENT_NODE_MAGIC,
        TrieKind::Attribution => ATTR_NODE_MAGIC,
    }
}

fn object_digest(bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(OBJECT_DOMAIN);
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
    digest.finalize().into()
}

fn nibble(key: &[u8; 32], depth: usize) -> usize {
    let byte = key[depth / 2];
    if depth % 2 == 0 {
        usize::from(byte >> 4)
    } else {
        usize::from(byte & 0x0f)
    }
}

fn common_nibbles(left: &[u8; 32], right: &[u8; 32]) -> usize {
    for depth in 0..TRIE_DEPTH {
        if nibble(left, depth) != nibble(right, depth) {
            return depth;
        }
    }
    TRIE_DEPTH
}

fn parse_hex_digest(value: &str) -> PocResult<[u8; 32]> {
    if value.len() != 64 {
        return Err(PocError::Integrity(
            "semantic digest must have 64 hexadecimal characters".to_owned(),
        ));
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(bytes)
}

fn verify_existing_object(path: &Path, expected: [u8; 32]) -> PocResult<()> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| PocError::io("stat concurrent semantic object", path, error))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_OBJECT_BYTES {
        return Err(PocError::Integrity(
            "concurrent semantic object is not a bounded regular file".to_owned(),
        ));
    }
    let mut file = File::open(path)
        .map_err(|error| PocError::io("open concurrent semantic object", path, error))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    Read::take(&mut file, MAX_OBJECT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| PocError::io("read concurrent semantic object", path, error))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_OBJECT_BYTES {
        return Err(PocError::Integrity(
            "concurrent semantic object changed outside fixed bounds while reading".to_owned(),
        ));
    }
    if object_digest(&bytes) != expected {
        return Err(PocError::Integrity(
            "immutable semantic object collision or corruption".to_owned(),
        ));
    }
    file.sync_all()
        .map_err(|error| PocError::io("fsync reused semantic object", path, error))?;
    Ok(())
}

fn read_bounded_object(path: &Path) -> PocResult<Vec<u8>> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| PocError::io("stat semantic object", path, error))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_OBJECT_BYTES {
        return Err(PocError::Integrity(
            "semantic object is outside fixed bounds".to_owned(),
        ));
    }
    let mut file =
        File::open(path).map_err(|error| PocError::io("open semantic object", path, error))?;
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| PocError::Integrity("semantic object size overflow".to_owned()))?;
    let mut bytes = Vec::with_capacity(capacity);
    Read::take(&mut file, MAX_OBJECT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| PocError::io("read semantic object", path, error))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_OBJECT_BYTES {
        return Err(PocError::Integrity(
            "semantic object changed outside fixed bounds while reading".to_owned(),
        ));
    }
    Ok(bytes)
}

fn hex_nibble(value: u8) -> PocResult<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(PocError::Integrity(
            "semantic digest contains non-lowercase-hex byte".to_owned(),
        )),
    }
}

fn sync_directory(path: &Path) -> PocResult<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| PocError::io("fsync semantic object directory", path, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!("eos-mpla-{label}-{}", Uuid::new_v4()));
            std::fs::create_dir(&path).expect("create pack catalog test directory");
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn verified_object_cache_is_digest_keyed_and_bounded() {
        let mut cache = VerifiedObjectCache::new(5);
        let first = [1_u8; 32];
        let second = [2_u8; 32];

        cache.store(first, b"abc");
        cache.store(second, b"def");

        assert!(cache.load(first).is_none(), "oldest entry must be evicted");
        assert_eq!(cache.load(second).as_deref(), Some(b"def".as_slice()));
        assert!(
            cache.load([3_u8; 32]).is_none(),
            "cache key is the full digest"
        );
    }

    #[test]
    fn read_only_pack_catalog_cache_is_identity_keyed_and_bounded() {
        let mut cache = ReadOnlyPackCatalogCache::new(PACK_CATALOG_ENTRY_BYTES * 2);
        let indexes = vec![PackedObjectIndex {
            pack: PathBuf::from("/immutable/pack-a.pack"),
            index: PathBuf::from("/immutable/pack-a.index"),
            count: 1,
            entries_end: 56,
            index_checksum: [1_u8; 32],
            pack_bytes: 64,
            bloom: vec![0_u64; PACK_BLOOM_WORDS].into(),
        }];
        let first = PackCatalogIdentity {
            device: 1,
            inode: 1,
            length: 64,
            modified_seconds: 1,
            modified_nanoseconds: 1,
            changed_seconds: 1,
            changed_nanoseconds: 1,
        };
        let second = PackCatalogIdentity {
            inode: 2,
            ..first.clone()
        };
        let third = PackCatalogIdentity {
            inode: 3,
            ..first.clone()
        };
        let first_path = PathBuf::from("/immutable/first");
        let second_path = PathBuf::from("/immutable/second");
        let third_path = PathBuf::from("/immutable/third");

        cache.store(&first_path, first.clone(), &indexes);
        cache.store(&second_path, second.clone(), &indexes);
        assert_eq!(
            cache.load(&first_path, &first).map(|entries| entries.len()),
            Some(1),
            "the cache must use both the source directory and catalog identity"
        );
        assert!(
            cache.load(&first_path, &second).is_none(),
            "a replaced catalog must not reuse prior metadata"
        );

        cache.store(&third_path, third.clone(), &indexes);
        assert!(
            cache.load(&second_path, &second).is_none(),
            "least-recently-used catalog metadata must be evicted at the fixed bound"
        );
        assert_eq!(
            cache.load(&first_path, &first).map(|entries| entries.len()),
            Some(1)
        );
        assert_eq!(
            cache.load(&third_path, &third).map(|entries| entries.len()),
            Some(1)
        );
    }

    #[test]
    fn validated_pack_catalog_prevents_revalidating_unchanged_indexes() {
        let root = TestDirectory::new("pack-catalog");
        let objects = root.path.join("objects");
        let packs = objects.join("packs");
        let bytes = b"durably cataloged immutable semantic object";
        let mut writer = IncrementalPackWriter::new(&packs).expect("create pack writer");
        writer
            .append(object_digest(bytes), bytes)
            .expect("append immutable object");
        writer.finish().expect("install immutable pack");

        FULL_PACK_INDEX_VALIDATIONS.store(0, Ordering::Relaxed);
        let first = load_packed_indexes(&objects).expect("validate historical pack once");
        assert_eq!(first.len(), 1);
        assert_eq!(
            FULL_PACK_INDEX_VALIDATIONS.load(Ordering::Relaxed),
            1,
            "the first legacy open must validate the complete pack index"
        );
        assert!(
            packs.join(PACK_CATALOG_FILE).is_file(),
            "the validated pack metadata must be retained durably"
        );

        FULL_PACK_INDEX_VALIDATIONS.store(0, Ordering::Relaxed);
        let second = load_packed_indexes(&objects).expect("reuse durable pack catalog");
        assert_eq!(second.len(), 1);
        assert_eq!(
            FULL_PACK_INDEX_VALIDATIONS.load(Ordering::Relaxed),
            0,
            "unchanged validated indexes must not be reread on every publication"
        );
    }

    #[test]
    fn incremental_pack_writer_returns_verified_metadata_without_self_rescan() {
        let root = TestDirectory::new("pack-writer-metadata");
        let packs = root.path.join("packs");
        let mut writer = IncrementalPackWriter::new(&packs).expect("create pack writer");
        writer
            .append([1_u8; 32], b"first immutable object")
            .expect("append first sorted object");
        writer
            .append([2_u8; 32], b"second immutable object")
            .expect("append second sorted object");

        FULL_PACK_INDEX_VALIDATIONS.store(0, Ordering::Relaxed);
        let written = writer.finish().expect("finish immutable pack");
        assert_eq!(
            FULL_PACK_INDEX_VALIDATIONS.load(Ordering::Relaxed),
            0,
            "a just-written pack must not re-read its own already-verified index"
        );

        let reopened = read_pack_index(&written.index, written.pack.clone())
            .expect("normal on-disk reader validates written pack");
        assert_eq!(written.count, reopened.count);
        assert_eq!(written.entries_end, reopened.entries_end);
        assert_eq!(written.index_checksum, reopened.index_checksum);
        assert_eq!(written.pack_bytes, reopened.pack_bytes);
        assert_eq!(written.bloom.as_ref(), reopened.bloom.as_ref());
        assert_eq!(
            FULL_PACK_INDEX_VALIDATIONS.load(Ordering::Relaxed),
            1,
            "normal reopen must retain complete on-disk validation"
        );
    }

    #[test]
    fn incremental_pack_writer_rejects_unsorted_digests() {
        let root = TestDirectory::new("pack-writer-order");
        let mut writer =
            IncrementalPackWriter::new(&root.path.join("packs")).expect("create pack writer");
        writer
            .append([2_u8; 32], b"first immutable object")
            .expect("append first object");
        assert!(
            writer
                .append([1_u8; 32], b"second immutable object")
                .is_err(),
            "writer must establish the canonical digest ordering it returns"
        );
    }
}
