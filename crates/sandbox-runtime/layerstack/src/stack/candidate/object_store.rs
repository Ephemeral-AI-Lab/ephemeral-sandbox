use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sandbox_runtime_layerstack_core::{
    decode_v3_record, encode_digest_preimage_header, encode_v3_record, v3_record_id,
    CanonicalRecordV3, CanonicalSink, CanonicalSource, Digest32, DigestDomain, Error, ErrorKind,
    FieldClass, FileNodeId, RawDigest, RecordKindV3, TypedDigest, MAX_V3_RECORD_BYTES,
    ROOT_FORMAT_V3,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
pub(crate) const RECORD_HEADER_BYTES: usize = 15;
pub(crate) const MAX_CHUNK_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallStage {
    TempCreated,
    BytesWritten,
    FileFsynced,
    BeforeInstall,
    AfterInstall,
    ParentFsynced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallDisposition {
    Installed,
    AlreadyPresent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredObject {
    kind: RecordKindV3,
    id: Digest32,
    path: PathBuf,
    disposition: InstallDisposition,
}

impl StoredObject {
    pub(crate) const fn kind(&self) -> RecordKindV3 {
        self.kind
    }

    pub(crate) const fn id(&self) -> Digest32 {
        self.id
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn disposition(&self) -> InstallDisposition {
        self.disposition
    }
}

#[derive(Debug)]
pub(crate) enum ObjectStoreError {
    Io(std::io::Error),
    Core(Error),
    ObjectCollisionOrCorruption { kind: RecordKindV3, id: Digest32 },
    UnsupportedObjectKind(RecordKindV3),
    InvalidBatch(&'static str),
    Injected(InstallStage),
}

impl ObjectStoreError {
    pub(crate) const fn kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Core(error) => Some(error.kind()),
            Self::ObjectCollisionOrCorruption { .. } => {
                Some(ErrorKind::ObjectCollisionOrCorruption)
            }
            Self::UnsupportedObjectKind(_) => Some(ErrorKind::WrongKind),
            Self::Io(_) | Self::InvalidBatch(_) | Self::Injected(_) => None,
        }
    }
}

impl fmt::Display for ObjectStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "loose object I/O failed: {error}"),
            Self::Core(error) => write!(formatter, "loose object codec failed: {error}"),
            Self::ObjectCollisionOrCorruption { kind, id } => {
                write!(
                    formatter,
                    "existing loose object is corrupt or collides: kind={kind:?}, id={id:?}"
                )
            }
            Self::UnsupportedObjectKind(kind) => {
                write!(
                    formatter,
                    "record kind {kind:?} is not an immutable loose object"
                )
            }
            Self::InvalidBatch(message) => {
                write!(formatter, "loose object commit batch is invalid: {message}")
            }
            Self::Injected(stage) => write!(formatter, "injected object install stop at {stage:?}"),
        }
    }
}

impl std::error::Error for ObjectStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Core(error) => Some(error),
            Self::ObjectCollisionOrCorruption { .. }
            | Self::UnsupportedObjectKind(_)
            | Self::InvalidBatch(_)
            | Self::Injected(_) => None,
        }
    }
}

impl From<std::io::Error> for ObjectStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<Error> for ObjectStoreError {
    fn from(error: Error) -> Self {
        Self::Core(error)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LooseObjectStore {
    storage_root: PathBuf,
    batch: Option<Arc<CommitBatch>>,
}

#[derive(Debug)]
struct CommitBatch {
    committed: AtomicBool,
    mutation_lock: Mutex<()>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthenticatedChunk {
    encoded: Vec<u8>,
}

impl AuthenticatedChunk {
    pub(crate) fn payload(&self) -> &[u8] {
        &self.encoded[RECORD_HEADER_BYTES..]
    }

    pub(crate) fn encoded_len(&self) -> usize {
        self.encoded.len()
    }
}

impl LooseObjectStore {
    pub(crate) fn new(storage_root: PathBuf) -> Result<Self, ObjectStoreError> {
        if !std::fs::metadata(&storage_root)?.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "layer-stack storage root is not a directory",
            )
            .into());
        }
        Ok(Self {
            storage_root,
            batch: None,
        })
    }

    /// Open an operation-local deferred-durability batch.
    ///
    /// Complete objects are installed atomically at their content-addressed
    /// paths but remain unreachable until the caller validates the candidate
    /// root and publishes its ref. `commit_batch` performs one storage-wide
    /// durability barrier before that publication. A crash before the barrier
    /// can leave only unreferenced objects, and every subsequent load
    /// authenticates their content. The ordinary constructor retains its
    /// per-object durability contract unchanged.
    pub(crate) fn new_commit_batch(storage_root: PathBuf) -> Result<Self, ObjectStoreError> {
        if !std::fs::metadata(&storage_root)?.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "layer-stack storage root is not a directory",
            )
            .into());
        }
        Ok(Self {
            storage_root,
            batch: Some(Arc::new(CommitBatch {
                committed: AtomicBool::new(false),
                mutation_lock: Mutex::new(()),
            })),
        })
    }

    pub(crate) fn object_path(&self, kind: RecordKindV3, id: Digest32) -> PathBuf {
        let digest = digest_hex(id);
        self.storage_root
            .join("objects")
            .join("loose")
            .join(kind_component(kind))
            .join(&digest[..2])
            .join(digest)
    }

    /// Install a whole-file native acceleration object for one immutable file
    /// node.
    ///
    /// The canonical chunk graph remains the source of truth. This extra file
    /// is addressed by the complete file-node digest and is used only as a
    /// copy-on-write extent source when constructing a native carrier. It is
    /// atomically installed and covered by the publication batch's single
    /// storage-wide durability barrier before the publication becomes
    /// reachable.
    pub(crate) fn install_native_file(
        &self,
        id: FileNodeId,
        source: &Path,
        logical_length: u64,
    ) -> Result<PathBuf, ObjectStoreError> {
        let batch = self.require_open_batch()?;
        let _batch_guard = batch
            .mutation_lock
            .lock()
            .map_err(|_| ObjectStoreError::InvalidBatch("batch lock is poisoned"))?;
        self.require_open_batch()?;

        let source_metadata = std::fs::symlink_metadata(source)?;
        if !source_metadata.file_type().is_file() || source_metadata.len() != logical_length {
            return Err(ObjectStoreError::InvalidBatch(
                "native acceleration source is not the exact regular file",
            ));
        }
        let final_path = self.native_file_path(id);
        if path_is_present(&final_path)? {
            validate_native_file(&final_path, logical_length)?;
            return Ok(final_path);
        }
        let parent = final_path.parent().ok_or(ObjectStoreError::InvalidBatch(
            "native acceleration path has no parent",
        ))?;
        ensure_real_directory_tree(&self.storage_root, parent)?;
        let temp = parent.join(format!(
            ".native.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut guard = TempGuard::new(temp.clone());
        let mut source_file = File::open(source)?;
        let mut native_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temp)?;
        if !clone_file_extents(&native_file, &source_file)? {
            source_file.seek(SeekFrom::Start(0))?;
            native_file.set_len(0)?;
            let copied = std::io::copy(&mut source_file, &mut native_file)?;
            if copied != logical_length {
                return Err(ObjectStoreError::InvalidBatch(
                    "native acceleration fallback copied the wrong length",
                ));
            }
        }
        if native_file.metadata()?.len() != logical_length {
            return Err(ObjectStoreError::InvalidBatch(
                "native acceleration object has the wrong length",
            ));
        }
        drop(native_file);
        drop(source_file);
        match std::fs::hard_link(&temp, &final_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_native_file(&final_path, logical_length)?;
            }
            Err(error) => return Err(error.into()),
        }
        guard.remove()?;
        Ok(final_path)
    }

    /// Open a native acceleration object without following a final symlink.
    ///
    /// A missing object is an ordinary cache miss and reconstruction falls
    /// back to authenticated chunks. A malformed object fails closed; it is
    /// never treated as canonical publication data.
    pub(crate) fn open_native_file(
        &self,
        id: FileNodeId,
        logical_length: u64,
    ) -> Result<Option<File>, ObjectStoreError> {
        let path = self.native_file_path(id);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(_) => validate_native_file(&path, logical_length)?,
        }
        let fd = rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let file = File::from(fd);
        if file.metadata()?.len() != logical_length {
            return Err(ObjectStoreError::InvalidBatch(
                "native acceleration object changed while opening",
            ));
        }
        Ok(Some(file))
    }

    pub(crate) fn native_file_path(&self, id: FileNodeId) -> PathBuf {
        let encoded = hex_digest(id.digest());
        self.storage_root
            .join("native-files-v1")
            .join(&encoded[..2])
            .join(encoded)
    }

    pub(crate) fn install<D>(
        &self,
        record: &CanonicalRecordV3,
        digest: &mut D,
    ) -> Result<StoredObject, ObjectStoreError>
    where
        D: TypedDigest + RawDigest,
    {
        if self.batch.is_some() {
            self.install_batched(record, digest)
        } else {
            self.install_with_hook(record, digest, |_| Ok(()))
        }
    }

    pub(crate) fn install_chunk_slices<D>(
        &self,
        first: &[u8],
        second: &[u8],
        digest: &mut D,
    ) -> Result<StoredObject, ObjectStoreError>
    where
        D: TypedDigest,
    {
        let _batch_guard = self
            .batch
            .as_ref()
            .map(|batch| {
                batch
                    .mutation_lock
                    .lock()
                    .map_err(|_| ObjectStoreError::InvalidBatch("batch lock is poisoned"))
            })
            .transpose()?;
        if self.batch.is_some() {
            self.require_open_batch()?;
        }
        let length = first.len().checked_add(second.len()).ok_or_else(|| {
            Error::new(
                ErrorKind::ArithmeticOverflow,
                ROOT_FORMAT_V3,
                FieldClass::Length,
                0,
            )
        })?;
        if !(1..=MAX_CHUNK_BYTES).contains(&length) {
            return Err(Error::new(
                ErrorKind::LengthLimit,
                ROOT_FORMAT_V3,
                FieldClass::Length,
                u32::try_from(length).unwrap_or(u32::MAX),
            )
            .into());
        }

        let header = chunk_header(length)?;
        let id = chunk_slices_id(first, second, digest)?;
        let kind = RecordKindV3::Chunk;
        let final_path = self.object_path(kind, id);
        if path_is_present(&final_path)? {
            self.verify_existing_chunk(&final_path, id, &header, first, second)?;
            return Ok(StoredObject {
                kind,
                id,
                path: final_path,
                disposition: InstallDisposition::AlreadyPresent,
            });
        }
        let path = final_path;
        if self.batch.is_some() {
            self.ensure_object_parent_unflushed(kind, id)?;
        } else {
            self.ensure_object_parent(kind, id)?;
        }
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "loose chunk path has no parent",
            )
        })?;
        let temp = parent.join(format!(
            ".object.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut guard = TempGuard::new(temp.clone());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&header)?;
        file.write_all(first)?;
        file.write_all(second)?;
        if self.batch.is_none() {
            file.sync_all()?;
        }
        drop(file);

        let disposition = match std::fs::hard_link(&temp, &path) {
            Ok(()) => InstallDisposition::Installed,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.verify_existing_chunk(&path, id, &header, first, second)?;
                InstallDisposition::AlreadyPresent
            }
            Err(error) => return Err(error.into()),
        };
        guard.remove()?;
        if self.batch.is_none() {
            fsync_dir(parent)?;
        }
        Ok(StoredObject {
            kind,
            id,
            path,
            disposition,
        })
    }

    pub(crate) fn install_with_hook<D, H>(
        &self,
        record: &CanonicalRecordV3,
        digest: &mut D,
        mut hook: H,
    ) -> Result<StoredObject, ObjectStoreError>
    where
        D: TypedDigest + RawDigest,
        H: FnMut(InstallStage) -> Result<(), ObjectStoreError>,
    {
        if self.batch.is_some() {
            return Err(ObjectStoreError::InvalidBatch(
                "failpoint installation is only supported by an ordinary store",
            ));
        }
        ensure_installable(record.kind())?;
        let bytes = canonical_bytes(record)?;
        let id = v3_record_id(record, digest)?;
        let path = self.object_path(record.kind(), id);

        if path.exists() {
            self.verify_existing(&path, record.kind(), id, &bytes, digest)?;
            return Ok(StoredObject {
                kind: record.kind(),
                id,
                path,
                disposition: InstallDisposition::AlreadyPresent,
            });
        }

        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "loose object path has no parent",
            )
        })?;
        self.ensure_object_parent(record.kind(), id)?;
        let temp = parent.join(format!(
            ".object.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut guard = TempGuard::new(temp.clone());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        hook(InstallStage::TempCreated)?;
        file.write_all(&bytes)?;
        hook(InstallStage::BytesWritten)?;
        file.sync_all()?;
        hook(InstallStage::FileFsynced)?;
        drop(file);
        hook(InstallStage::BeforeInstall)?;

        let disposition = match std::fs::hard_link(&temp, &path) {
            Ok(()) => {
                hook(InstallStage::AfterInstall)?;
                InstallDisposition::Installed
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.verify_existing(&path, record.kind(), id, &bytes, digest)?;
                InstallDisposition::AlreadyPresent
            }
            Err(error) => return Err(error.into()),
        };

        guard.remove()?;
        fsync_dir(parent)?;
        hook(InstallStage::ParentFsynced)?;
        Ok(StoredObject {
            kind: record.kind(),
            id,
            path,
            disposition,
        })
    }

    pub(crate) fn load<D>(
        &self,
        kind: RecordKindV3,
        id: Digest32,
        digest: &mut D,
    ) -> Result<CanonicalRecordV3, ObjectStoreError>
    where
        D: TypedDigest + RawDigest,
    {
        ensure_installable(kind)?;
        let path = self.resolve_object_path(kind, id)?;
        let bytes = read_bounded(&path)?;
        let mut source = SliceSource::new(&bytes);
        let record = decode_v3_record(&mut source, digest).map_err(|_| self.collision(kind, id))?;
        if record.kind() != kind {
            return Err(self.collision(kind, id));
        }
        let found = v3_record_id(&record, digest).map_err(|_| self.collision(kind, id))?;
        if found != id {
            return Err(self.collision(kind, id));
        }
        Ok(record)
    }

    /// Read and authenticate one canonical chunk while retaining exactly one
    /// owned buffer for both the encoded record and its borrowed payload.
    pub(crate) fn load_authenticated_chunk<D>(
        &self,
        id: Digest32,
        digest: &mut D,
    ) -> Result<AuthenticatedChunk, ObjectStoreError>
    where
        D: TypedDigest,
    {
        let mut encoded = Vec::new();
        self.load_authenticated_chunk_into(id, digest, &mut encoded)?;
        Ok(AuthenticatedChunk { encoded })
    }

    /// Read and authenticate one canonical chunk into an operation-owned
    /// buffer. Large graph traversals can reuse the allocation without
    /// retaining chunk payloads or cycling through allocator size classes.
    pub(crate) fn load_authenticated_chunk_into<D>(
        &self,
        id: Digest32,
        digest: &mut D,
        encoded: &mut Vec<u8>,
    ) -> Result<(), ObjectStoreError>
    where
        D: TypedDigest,
    {
        let kind = RecordKindV3::Chunk;
        let path = self.resolve_object_path(kind, id)?;
        read_chunk_bounded_into(&path, encoded).map_err(|_| self.collision(kind, id))?;
        let payload_length = encoded
            .len()
            .checked_sub(RECORD_HEADER_BYTES)
            .filter(|length| (1..=MAX_CHUNK_BYTES).contains(length))
            .ok_or_else(|| self.collision(kind, id))?;
        let expected_header = chunk_header(payload_length).map_err(|_| self.collision(kind, id))?;
        if encoded[..RECORD_HEADER_BYTES] != expected_header {
            return Err(self.collision(kind, id));
        }
        let payload = &encoded[RECORD_HEADER_BYTES..];
        let found = chunk_slices_id(payload, &[], digest).map_err(|_| self.collision(kind, id))?;
        if found != id {
            return Err(self.collision(kind, id));
        }
        Ok(())
    }

    pub(crate) fn commit_batch(&self) -> Result<(), ObjectStoreError> {
        let batch = self.batch.as_ref().ok_or(ObjectStoreError::InvalidBatch(
            "ordinary stores cannot commit a batch",
        ))?;
        if batch.committed.load(Ordering::Acquire) {
            return Err(ObjectStoreError::InvalidBatch(
                "batch was already committed",
            ));
        }
        let _batch_guard = batch
            .mutation_lock
            .lock()
            .map_err(|_| ObjectStoreError::InvalidBatch("batch lock is poisoned"))?;
        if batch.committed.load(Ordering::Acquire) {
            return Err(ObjectStoreError::InvalidBatch(
                "batch was already committed",
            ));
        }

        sync_storage_root(&self.storage_root)?;
        batch.committed.store(true, Ordering::Release);
        Ok(())
    }

    fn install_batched<D>(
        &self,
        record: &CanonicalRecordV3,
        digest: &mut D,
    ) -> Result<StoredObject, ObjectStoreError>
    where
        D: TypedDigest + RawDigest,
    {
        let batch = self.require_open_batch()?;
        let _batch_guard = batch
            .mutation_lock
            .lock()
            .map_err(|_| ObjectStoreError::InvalidBatch("batch lock is poisoned"))?;
        self.require_open_batch()?;
        ensure_installable(record.kind())?;
        let bytes = canonical_bytes(record)?;
        let id = v3_record_id(record, digest)?;
        let final_path = self.object_path(record.kind(), id);
        if path_is_present(&final_path)? {
            self.verify_existing(&final_path, record.kind(), id, &bytes, digest)?;
            return Ok(StoredObject {
                kind: record.kind(),
                id,
                path: final_path,
                disposition: InstallDisposition::AlreadyPresent,
            });
        }
        let path = final_path;
        self.ensure_object_parent_unflushed(record.kind(), id)?;
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "batched loose object path has no parent",
            )
        })?;
        let temp = parent.join(format!(
            ".object.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut guard = TempGuard::new(temp.clone());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        drop(file);
        let disposition = match std::fs::hard_link(&temp, &path) {
            Ok(()) => InstallDisposition::Installed,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.verify_existing(&path, record.kind(), id, &bytes, digest)?;
                InstallDisposition::AlreadyPresent
            }
            Err(error) => return Err(error.into()),
        };
        guard.remove()?;
        Ok(StoredObject {
            kind: record.kind(),
            id,
            path,
            disposition,
        })
    }

    fn require_open_batch(&self) -> Result<&CommitBatch, ObjectStoreError> {
        let batch = self
            .batch
            .as_deref()
            .ok_or(ObjectStoreError::InvalidBatch("store has no commit batch"))?;
        if batch.committed.load(Ordering::Acquire) {
            return Err(ObjectStoreError::InvalidBatch("batch is already committed"));
        }
        Ok(batch)
    }

    pub(crate) fn resolve_object_path(
        &self,
        kind: RecordKindV3,
        id: Digest32,
    ) -> Result<PathBuf, ObjectStoreError> {
        Ok(self.object_path(kind, id))
    }

    fn ensure_object_parent_unflushed(
        &self,
        kind: RecordKindV3,
        id: Digest32,
    ) -> Result<(), ObjectStoreError> {
        let path = self.object_path(kind, id);
        let parent = path.parent().ok_or(ObjectStoreError::InvalidBatch(
            "final object path has no parent",
        ))?;
        ensure_real_directory_tree(&self.storage_root, parent)
    }

    fn ensure_object_parent(
        &self,
        kind: RecordKindV3,
        id: Digest32,
    ) -> Result<(), ObjectStoreError> {
        let digest = digest_hex(id);
        let components = ["objects", "loose", kind_component(kind), &digest[..2]];
        let mut current = self.storage_root.clone();
        for component in components {
            let next = current.join(component);
            match std::fs::create_dir(&next) {
                Ok(()) => fsync_dir(&current)?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !std::fs::metadata(&next)?.is_dir() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::NotADirectory,
                            format!("loose object prefix {} is not a directory", next.display()),
                        )
                        .into());
                    }
                }
                Err(error) => return Err(error.into()),
            }
            current = next;
        }
        Ok(())
    }

    fn verify_existing<D>(
        &self,
        path: &Path,
        kind: RecordKindV3,
        id: Digest32,
        expected_bytes: &[u8],
        digest: &mut D,
    ) -> Result<(), ObjectStoreError>
    where
        D: TypedDigest + RawDigest,
    {
        let metadata = std::fs::symlink_metadata(path).map_err(ObjectStoreError::Io)?;
        if !metadata.file_type().is_file()
            || metadata.len() != u64::try_from(expected_bytes.len()).unwrap_or(u64::MAX)
        {
            return Err(self.collision(kind, id));
        }
        let bytes = read_bounded(path).map_err(|_| self.collision(kind, id))?;
        if bytes != expected_bytes {
            return Err(self.collision(kind, id));
        }
        let mut source = SliceSource::new(&bytes);
        let decoded =
            decode_v3_record(&mut source, digest).map_err(|_| self.collision(kind, id))?;
        if decoded.kind() != kind
            || v3_record_id(&decoded, digest).map_err(|_| self.collision(kind, id))? != id
        {
            return Err(self.collision(kind, id));
        }
        Ok(())
    }

    fn verify_existing_chunk(
        &self,
        path: &Path,
        id: Digest32,
        header: &[u8; RECORD_HEADER_BYTES],
        first: &[u8],
        second: &[u8],
    ) -> Result<(), ObjectStoreError> {
        let expected_len = RECORD_HEADER_BYTES
            .checked_add(first.len())
            .and_then(|length| length.checked_add(second.len()))
            .ok_or_else(|| self.collision(RecordKindV3::Chunk, id))?;
        let metadata =
            std::fs::symlink_metadata(path).map_err(|_| self.collision(RecordKindV3::Chunk, id))?;
        if !metadata.file_type().is_file()
            || metadata.len() != u64::try_from(expected_len).unwrap_or(u64::MAX)
        {
            return Err(self.collision(RecordKindV3::Chunk, id));
        }

        let mut file = File::open(path).map_err(|_| self.collision(RecordKindV3::Chunk, id))?;
        let mut found_header = [0_u8; RECORD_HEADER_BYTES];
        file.read_exact(&mut found_header)
            .map_err(|_| self.collision(RecordKindV3::Chunk, id))?;
        if found_header != *header
            || !matches_expected(&mut file, first)
            || !matches_expected(&mut file, second)
        {
            return Err(self.collision(RecordKindV3::Chunk, id));
        }
        let mut trailing = [0_u8; 1];
        if file
            .read(&mut trailing)
            .map_err(|_| self.collision(RecordKindV3::Chunk, id))?
            != 0
        {
            return Err(self.collision(RecordKindV3::Chunk, id));
        }
        Ok(())
    }

    const fn collision(&self, kind: RecordKindV3, id: Digest32) -> ObjectStoreError {
        ObjectStoreError::ObjectCollisionOrCorruption { kind, id }
    }
}

fn ensure_real_directory_tree(root: &Path, target: &Path) -> Result<(), ObjectStoreError> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| ObjectStoreError::InvalidBatch("directory escapes its trusted root"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(ObjectStoreError::InvalidBatch(
                "directory contains a non-normal component",
            ));
        };
        current.push(component);
        match std::fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&current)?;
                if !metadata.file_type().is_dir() {
                    return Err(ObjectStoreError::InvalidBatch(
                        "directory component is not a real directory",
                    ));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_native_file(path: &Path, logical_length: u64) -> Result<(), ObjectStoreError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() != logical_length {
        return Err(ObjectStoreError::InvalidBatch(
            "native acceleration object is not an exact regular file",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn clone_file_extents(destination: &File, source: &File) -> Result<bool, ObjectStoreError> {
    match rustix::fs::ioctl_ficlone(destination, source) {
        Ok(()) => Ok(true),
        Err(error)
            if matches!(
                error,
                rustix::io::Errno::XDEV
                    | rustix::io::Errno::NOTSUP
                    | rustix::io::Errno::INVAL
                    | rustix::io::Errno::NOSYS
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

#[cfg(not(target_os = "linux"))]
fn clone_file_extents(_destination: &File, _source: &File) -> Result<bool, ObjectStoreError> {
    Ok(false)
}

fn hex_digest(digest: Digest32) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn path_is_present(path: &Path) -> Result<bool, ObjectStoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn sync_storage_root(storage_root: &Path) -> Result<(), ObjectStoreError> {
    #[cfg(target_os = "linux")]
    {
        let root = File::open(storage_root)?;
        rustix::fs::syncfs(&root)
            .map_err(std::io::Error::from)
            .map_err(ObjectStoreError::Io)?;
    }
    #[cfg(not(target_os = "linux"))]
    File::open(storage_root)?.sync_all()?;
    Ok(())
}

pub(crate) fn decode_embedded_record<D>(
    bytes: &[u8],
    kind: RecordKindV3,
    digest: &mut D,
) -> Result<CanonicalRecordV3, ObjectStoreError>
where
    D: RawDigest,
{
    let mut source = SliceSource::new(bytes);
    let record = decode_v3_record(&mut source, digest)?;
    if record.kind() != kind {
        return Err(ObjectStoreError::UnsupportedObjectKind(record.kind()));
    }
    Ok(record)
}

fn ensure_installable(kind: RecordKindV3) -> Result<(), ObjectStoreError> {
    if matches!(
        kind,
        RecordKindV3::Root
            | RecordKindV3::TreePage
            | RecordKindV3::FileNode
            | RecordKindV3::SegmentPage
            | RecordKindV3::Chunk
            | RecordKindV3::AttributionRoot
            | RecordKindV3::AttributionPage
            | RecordKindV3::HardlinkGroup
    ) {
        Ok(())
    } else {
        Err(ObjectStoreError::UnsupportedObjectKind(kind))
    }
}

const fn kind_component(kind: RecordKindV3) -> &'static str {
    match kind {
        RecordKindV3::Root => "root",
        RecordKindV3::Metadata => "metadata",
        RecordKindV3::TreePage => "tree-page",
        RecordKindV3::FileNode => "file-node",
        RecordKindV3::SegmentPage => "segment-page",
        RecordKindV3::Chunk => "chunk",
        RecordKindV3::AttributionRoot => "attribution-root",
        RecordKindV3::AttributionPage => "attribution-page",
        RecordKindV3::HardlinkGroup => "hardlink-group",
        RecordKindV3::Head => "head",
        RecordKindV3::OperationState => "operation-state",
        RecordKindV3::Locator => "locator",
        RecordKindV3::SourceLease => "source-lease",
    }
}

fn digest_hex(digest: Digest32) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn canonical_bytes(record: &CanonicalRecordV3) -> Result<Vec<u8>, ObjectStoreError> {
    let mut sink = VecSink::default();
    encode_v3_record(record, &mut sink)?;
    Ok(sink.bytes)
}

fn chunk_header(length: usize) -> Result<[u8; RECORD_HEADER_BYTES], ObjectStoreError> {
    let mut sink = FixedSink::default();
    encode_digest_preimage_header(
        DigestDomain::V3Record(RecordKindV3::Chunk as u8),
        ROOT_FORMAT_V3,
        u64::try_from(length).map_err(|_| {
            Error::new(
                ErrorKind::ArithmeticOverflow,
                ROOT_FORMAT_V3,
                FieldClass::Length,
                0,
            )
        })?,
        &mut sink,
    )?;
    if sink.position != RECORD_HEADER_BYTES {
        return Err(Error::new(
            ErrorKind::DigestFailure,
            ROOT_FORMAT_V3,
            FieldClass::Header,
            u32::try_from(sink.position).unwrap_or(u32::MAX),
        )
        .into());
    }
    Ok(sink.bytes)
}

fn chunk_slices_id<D>(
    first: &[u8],
    second: &[u8],
    digest: &mut D,
) -> Result<Digest32, ObjectStoreError>
where
    D: TypedDigest,
{
    let payload_len = first
        .len()
        .checked_add(second.len())
        .and_then(|length| u64::try_from(length).ok())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::ArithmeticOverflow,
                ROOT_FORMAT_V3,
                FieldClass::Length,
                0,
            )
        })?;
    let mut invocations = 0_u8;
    let mut encode_payload = |sink: &mut dyn CanonicalSink| {
        invocations = invocations.checked_add(1).ok_or_else(|| {
            Error::new(
                ErrorKind::ArithmeticOverflow,
                ROOT_FORMAT_V3,
                FieldClass::Digest,
                0,
            )
        })?;
        sink.write_all(first)?;
        sink.write_all(second)
    };
    let id = digest.digest(
        DigestDomain::V3Record(RecordKindV3::Chunk as u8),
        ROOT_FORMAT_V3,
        payload_len,
        &mut encode_payload,
    )?;
    if invocations != 1 {
        return Err(Error::new(
            ErrorKind::DigestFailure,
            ROOT_FORMAT_V3,
            FieldClass::Digest,
            u32::from(invocations),
        )
        .into());
    }
    Ok(id)
}

fn matches_expected(file: &mut File, mut expected: &[u8]) -> bool {
    let mut buffer = [0_u8; 8 * 1024];
    while !expected.is_empty() {
        let count = expected.len().min(buffer.len());
        if file.read_exact(&mut buffer[..count]).is_err() || buffer[..count] != expected[..count] {
            return false;
        }
        expected = &expected[count..];
    }
    true
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ObjectStoreError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > u64::from(MAX_V3_RECORD_BYTES) {
        return Err(ObjectStoreError::Core(Error::new(
            ErrorKind::ObjectCollisionOrCorruption,
            ROOT_FORMAT_V3,
            FieldClass::Record,
            u32::try_from(metadata.len()).unwrap_or(u32::MAX),
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)?
        .take(u64::from(MAX_V3_RECORD_BYTES) + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_V3_RECORD_BYTES as usize
        || u64::try_from(bytes.len()).unwrap_or(u64::MAX) != metadata.len()
    {
        return Err(ObjectStoreError::Core(Error::new(
            ErrorKind::ObjectCollisionOrCorruption,
            ROOT_FORMAT_V3,
            FieldClass::Record,
            u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        )));
    }
    Ok(bytes)
}

fn read_chunk_bounded_into(path: &Path, encoded: &mut Vec<u8>) -> Result<(), ObjectStoreError> {
    let maximum = RECORD_HEADER_BYTES + MAX_CHUNK_BYTES;
    let metadata = std::fs::symlink_metadata(path)?;
    let length = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if !metadata.file_type().is_file() || !(RECORD_HEADER_BYTES + 1..=maximum).contains(&length) {
        return Err(ObjectStoreError::Core(Error::new(
            ErrorKind::ObjectCollisionOrCorruption,
            ROOT_FORMAT_V3,
            FieldClass::Record,
            u32::try_from(metadata.len()).unwrap_or(u32::MAX),
        )));
    }
    encoded.clear();
    encoded.reserve(length);
    File::open(path)?
        .take(u64::try_from(maximum + 1).unwrap_or(u64::MAX))
        .read_to_end(encoded)?;
    if encoded.len() != length {
        return Err(ObjectStoreError::Core(Error::new(
            ErrorKind::ObjectCollisionOrCorruption,
            ROOT_FORMAT_V3,
            FieldClass::Record,
            u32::try_from(encoded.len()).unwrap_or(u32::MAX),
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn fsync_dir(path: &Path) -> Result<(), ObjectStoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn fsync_dir(_path: &Path) -> Result<(), ObjectStoreError> {
    Ok(())
}

#[derive(Default)]
struct VecSink {
    bytes: Vec<u8>,
}

#[derive(Default)]
struct FixedSink {
    bytes: [u8; RECORD_HEADER_BYTES],
    position: usize,
}

impl CanonicalSink for FixedSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self.position.checked_add(bytes.len()).ok_or_else(|| {
            Error::new(
                ErrorKind::ArithmeticOverflow,
                ROOT_FORMAT_V3,
                FieldClass::Header,
                0,
            )
        })?;
        let output = self.bytes.get_mut(self.position..end).ok_or_else(|| {
            Error::new(
                ErrorKind::LengthLimit,
                ROOT_FORMAT_V3,
                FieldClass::Header,
                u32::try_from(end).unwrap_or(u32::MAX),
            )
        })?;
        output.copy_from_slice(bytes);
        self.position = end;
        Ok(())
    }
}

impl CanonicalSink for VecSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

struct SliceSource<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SliceSource<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
}

impl CanonicalSource for SliceSource<'_> {
    fn read_exact(&mut self, output: &mut [u8]) -> Result<(), Error> {
        let end = self.position.checked_add(output.len()).ok_or_else(|| {
            Error::new(
                ErrorKind::ArithmeticOverflow,
                ROOT_FORMAT_V3,
                FieldClass::Source,
                0,
            )
        })?;
        let input = self.bytes.get(self.position..end).ok_or_else(|| {
            Error::new(
                ErrorKind::CorruptRecord,
                ROOT_FORMAT_V3,
                FieldClass::Source,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            )
        })?;
        output.copy_from_slice(input);
        self.position = end;
        Ok(())
    }

    fn ensure_exhausted(&mut self) -> Result<(), Error> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::TrailingBytes,
                ROOT_FORMAT_V3,
                FieldClass::Source,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ))
        }
    }
}

struct TempGuard {
    path: Option<PathBuf>,
}

impl TempGuard {
    const fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn remove(&mut self) -> Result<(), ObjectStoreError> {
        if let Some(path) = self.path.take() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}
