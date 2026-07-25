use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_runtime_layerstack_core::{
    decode_v3_record, encode_digest_preimage_header, encode_v3_record, v3_record_id,
    CanonicalRecordV3, CanonicalSink, CanonicalSource, Digest32, DigestDomain, Error, ErrorKind,
    FieldClass, RawDigest, RecordKindV3, TypedDigest, MAX_V3_RECORD_BYTES, ROOT_FORMAT_V3,
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
            Self::Io(_) | Self::Injected(_) => None,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthenticatedChunk {
    encoded: Vec<u8>,
}

impl AuthenticatedChunk {
    pub(crate) fn payload(&self) -> &[u8] {
        &self.encoded[RECORD_HEADER_BYTES..]
    }

    pub(crate) const fn encoded_len(&self) -> usize {
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
        Ok(Self { storage_root })
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

    pub(crate) fn install<D>(
        &self,
        record: &CanonicalRecordV3,
        digest: &mut D,
    ) -> Result<StoredObject, ObjectStoreError>
    where
        D: TypedDigest + RawDigest,
    {
        self.install_with_hook(record, digest, |_| Ok(()))
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
        let path = self.object_path(kind, id);
        if path.exists() {
            self.verify_existing_chunk(&path, id, &header, first, second)?;
            return Ok(StoredObject {
                kind,
                id,
                path,
                disposition: InstallDisposition::AlreadyPresent,
            });
        }

        self.ensure_object_parent(kind, id)?;
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
        file.sync_all()?;
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
        fsync_dir(parent)?;
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
        let path = self.object_path(kind, id);
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
        let kind = RecordKindV3::Chunk;
        let path = self.object_path(kind, id);
        let encoded = read_chunk_bounded(&path).map_err(|_| self.collision(kind, id))?;
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
        Ok(AuthenticatedChunk { encoded })
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

fn read_chunk_bounded(path: &Path) -> Result<Vec<u8>, ObjectStoreError> {
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
    let mut encoded = Vec::with_capacity(length);
    File::open(path)?
        .take(u64::try_from(maximum + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut encoded)?;
    if encoded.len() != length {
        return Err(ObjectStoreError::Core(Error::new(
            ErrorKind::ObjectCollisionOrCorruption,
            ROOT_FORMAT_V3,
            FieldClass::Record,
            u32::try_from(encoded.len()).unwrap_or(u32::MAX),
        )));
    }
    Ok(encoded)
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
