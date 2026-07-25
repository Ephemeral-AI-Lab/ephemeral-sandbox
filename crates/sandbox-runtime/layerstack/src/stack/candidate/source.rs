use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_runtime_layerstack_core::{
    CanonicalRecordV3, CanonicalSink, Digest32, DigestDomain, Error, ErrorKind, LeaseId, RawDigest,
    RecordKindV3, TlvV3, TypedDigest, MAX_V3_RECORD_BYTES, ROOT_FORMAT_V3,
};

use super::refs::{decode_record, encode_record, CommitLock, GcBarrier, RefError, RefStore};

const CURRENT_MAGIC: &[u8; 8] = b"EOSLS3LC";
const CURRENT_CHECKSUM_DOMAIN: &[u8] = b"EOS-LS3-LOCATOR-CURRENT\0";
const CURRENT_BYTES: usize = 8 + 8 + 32 + 32;
const MUTABLE_MAX_BYTES: u64 = 512;
const MAX_SOURCE_LEASES: usize = 1_024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceRequirement {
    AllLoose,
    LastV1 {
        object_kind: RecordKindV3,
        object_id: Digest32,
        carrier_id: Digest32,
        offset: u64,
        length: u64,
        payload_sha256: Digest32,
        locator_generation: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SourceHold {
    pub(crate) lease_id: LeaseId,
    pub(crate) carrier_id: Digest32,
    pub(crate) locator_generation: u64,
    pub(crate) carrier_generation: u64,
    pub(crate) protected_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CleanupDecision {
    Unprotected,
    Protected { bytes: u64 },
}

pub(crate) struct OpenedCarrier {
    pub(crate) file: File,
    pub(crate) generation: u64,
}

pub(crate) trait CarrierCatalog {
    fn open(&self, carrier_id: Digest32) -> std::io::Result<OpenedCarrier>;
    fn generation(&self, carrier_id: Digest32) -> std::io::Result<u64>;
}

#[derive(Debug)]
pub(crate) enum SourceError {
    Io(std::io::Error),
    Core(Error),
    Ref(RefError),
    Missing,
    Corrupt(&'static str),
    LeaseLimit,
}

impl SourceError {
    pub(crate) const fn kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Core(error) => Some(error.kind()),
            Self::Ref(error) => error.kind(),
            Self::Missing => Some(ErrorKind::LastLocatorMissing),
            Self::Corrupt(_) => Some(ErrorKind::LastLocatorCorrupt),
            Self::LeaseLimit => Some(ErrorKind::ResourceExhausted),
            Self::Io(_) => None,
        }
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "candidate v1 source I/O failed: {error}"),
            Self::Core(error) => write!(formatter, "candidate v1 source codec failed: {error}"),
            Self::Ref(error) => write!(formatter, "candidate v1 source ref failed: {error}"),
            Self::Missing => write!(formatter, "candidate last v1 locator is missing"),
            Self::Corrupt(message) => {
                write!(formatter, "candidate last v1 locator is corrupt: {message}")
            }
            Self::LeaseLimit => write!(formatter, "candidate source lease bound exceeded"),
        }
    }
}

impl std::error::Error for SourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Core(error) => Some(error),
            Self::Ref(error) => Some(error),
            Self::Missing | Self::Corrupt(_) | Self::LeaseLimit => None,
        }
    }
}

impl From<std::io::Error> for SourceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<Error> for SourceError {
    fn from(error: Error) -> Self {
        Self::Core(error)
    }
}

impl From<RefError> for SourceError {
    fn from(error: RefError) -> Self {
        Self::Ref(error)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SourceProtector {
    storage_root: PathBuf,
}

impl SourceProtector {
    pub(crate) const fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
    }

    pub(crate) fn protect<D, C, L, B>(
        &self,
        requirement: SourceRequirement,
        holder_root: Digest32,
        lease_id: LeaseId,
        catalog: &C,
        refs: &mut RefStore<'_, L, B>,
        digest: &mut D,
    ) -> Result<Option<SourceHold>, SourceError>
    where
        D: RawDigest + TypedDigest,
        C: CarrierCatalog,
        L: CommitLock,
        B: GcBarrier,
    {
        let SourceRequirement::LastV1 {
            object_kind,
            object_id,
            carrier_id,
            offset,
            length,
            payload_sha256,
            locator_generation,
        } = requirement
        else {
            return Ok(None);
        };
        ensure_source_kind(object_kind)?;
        if locator_generation == 0 || length == 0 || length > u64::from(MAX_V3_RECORD_BYTES) {
            return Err(SourceError::Corrupt("invalid source range or generation"));
        }

        let locator = CanonicalRecordV3::mutable(
            RecordKindV3::Locator,
            vec![
                TlvV3::new(1, vec![object_kind as u8]),
                TlvV3::new(2, object_id.into_bytes().to_vec()),
                TlvV3::new(3, locator_generation.to_be_bytes().to_vec()),
                TlvV3::new(4, vec![1]),
                TlvV3::new(5, carrier_id.into_bytes().to_vec()),
                TlvV3::new(6, offset.to_be_bytes().to_vec()),
                TlvV3::new(7, length.to_be_bytes().to_vec()),
                TlvV3::new(8, payload_sha256.into_bytes().to_vec()),
            ],
            digest,
        )?;
        let run_id = self.install_locator(&locator, locator_generation, digest)?;
        let loaded = self.load_locator(digest)?;
        verify_locator(
            &loaded,
            object_kind,
            object_id,
            carrier_id,
            offset,
            length,
            payload_sha256,
            locator_generation,
        )?;
        let (payload, carrier_generation) =
            read_and_verify(catalog, &loaded, object_kind, object_id, digest)?;

        let lease = CanonicalRecordV3::mutable(
            RecordKindV3::SourceLease,
            vec![
                TlvV3::new(1, lease_id.as_bytes().to_vec()),
                TlvV3::new(2, holder_root.into_bytes().to_vec()),
                TlvV3::new(3, carrier_id.into_bytes().to_vec()),
                TlvV3::new(4, locator_generation.to_be_bytes().to_vec()),
                TlvV3::new(5, locator_generation.to_be_bytes().to_vec()),
                TlvV3::new(6, length.to_be_bytes().to_vec()),
            ],
            digest,
        )?;
        refs.install_source_lease(&lease_id, &lease, digest)?;

        let stable = self
            .load_locator(digest)
            .and_then(|record| {
                verify_locator(
                    &record,
                    object_kind,
                    object_id,
                    carrier_id,
                    offset,
                    length,
                    payload_sha256,
                    locator_generation,
                )
            })
            .and_then(|()| {
                let observed = catalog
                    .generation(carrier_id)
                    .map_err(|_| SourceError::Missing)?;
                if observed != carrier_generation {
                    Err(SourceError::Corrupt("carrier catalog generation changed"))
                } else {
                    Ok(())
                }
            });
        if let Err(error) = stable {
            let _ = refs.delete_source_lease(&lease_id, digest);
            return Err(error);
        }

        let _ = run_id;
        drop(payload);
        Ok(Some(SourceHold {
            lease_id,
            carrier_id,
            locator_generation,
            carrier_generation,
            protected_bytes: length,
        }))
    }

    pub(crate) fn reconstruct<D, C>(
        &self,
        expected_kind: RecordKindV3,
        expected_id: Digest32,
        catalog: &C,
        digest: &mut D,
    ) -> Result<Vec<u8>, SourceError>
    where
        D: RawDigest + TypedDigest,
        C: CarrierCatalog,
    {
        let locator = self.load_locator(digest)?;
        let fields = locator_fields(&locator)?;
        if fields.object_kind != expected_kind || fields.object_id != expected_id {
            return Err(SourceError::Corrupt("locator target mismatch"));
        }
        read_and_verify(catalog, &locator, expected_kind, expected_id, digest)
            .map(|(bytes, _)| bytes)
    }

    pub(crate) fn guard_cleanup<D>(
        &self,
        carrier_id: Digest32,
        digest: &mut D,
    ) -> Result<CleanupDecision, SourceError>
    where
        D: RawDigest,
    {
        let directory = self.storage_root.join("refs").join("leases");
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CleanupDecision::Unprotected);
            }
            Err(error) => return Err(error.into()),
        };
        let mut count = 0_usize;
        let mut protected = 0_u64;
        for entry in entries {
            count = count.checked_add(1).ok_or(SourceError::LeaseLimit)?;
            if count > MAX_SOURCE_LEASES {
                return Err(SourceError::LeaseLimit);
            }
            let entry = entry?;
            let metadata = entry.metadata()?;
            if !metadata.is_file() || metadata.len() > MUTABLE_MAX_BYTES {
                return Err(SourceError::Corrupt("unbounded source lease"));
            }
            let bytes = std::fs::read(entry.path())?;
            let record = decode_record(&bytes, RecordKindV3::SourceLease, digest)
                .map_err(|_| SourceError::Corrupt("invalid source lease"))?;
            let fields = record
                .fields()
                .ok_or(SourceError::Corrupt("source lease fields"))?;
            if digest_field(fields, 2)? == carrier_id {
                protected = protected
                    .checked_add(u64_field(fields, 5)?)
                    .ok_or(SourceError::Corrupt("protected byte overflow"))?;
            }
        }
        if protected == 0 {
            Ok(CleanupDecision::Unprotected)
        } else {
            Ok(CleanupDecision::Protected { bytes: protected })
        }
    }

    pub(crate) fn release<D, L, B>(
        &self,
        lease_id: &LeaseId,
        refs: &mut RefStore<'_, L, B>,
        digest: &mut D,
    ) -> Result<bool, SourceError>
    where
        D: RawDigest,
        L: CommitLock,
        B: GcBarrier,
    {
        Ok(refs.delete_source_lease(lease_id, digest)?)
    }

    pub(crate) fn locator_run_path<D>(&self, digest: &mut D) -> Result<PathBuf, SourceError>
    where
        D: RawDigest,
    {
        let (_, run_id) = self.read_current(digest)?;
        Ok(self.locators_dir().join(format!("{}.sst", hex(run_id))))
    }

    fn install_locator<D>(
        &self,
        locator: &CanonicalRecordV3,
        generation: u64,
        digest: &mut D,
    ) -> Result<Digest32, SourceError>
    where
        D: RawDigest,
    {
        let bytes = encode_record(locator)?;
        let run_id = digest.digest_bytes(&bytes)?;
        let directory = self.locators_dir();
        std::fs::create_dir_all(&directory)?;
        fsync_dir(&directory)?;
        install_immutable(&directory.join(format!("{}.sst", hex(run_id))), &bytes)?;

        let current = encode_current(generation, run_id, digest)?;
        let current_path = directory.join("CURRENT");
        if current_path.exists() {
            let (old_generation, old_run) = self.read_current(digest)?;
            if old_generation == generation && old_run == run_id {
                return Ok(run_id);
            }
            if generation != old_generation.saturating_add(1) {
                return Err(SourceError::Corrupt("locator generation is not monotonic"));
            }
        }
        replace_durable(&current_path, &current)?;
        Ok(run_id)
    }

    fn load_locator<D>(&self, digest: &mut D) -> Result<CanonicalRecordV3, SourceError>
    where
        D: RawDigest,
    {
        let (_, run_id) = self.read_current(digest)?;
        let path = self.locators_dir().join(format!("{}.sst", hex(run_id)));
        let bytes = read_bounded(&path, MUTABLE_MAX_BYTES)?;
        if digest.digest_bytes(&bytes)? != run_id {
            return Err(SourceError::Corrupt("locator run ID mismatch"));
        }
        decode_record(&bytes, RecordKindV3::Locator, digest)
            .map_err(|_| SourceError::Corrupt("invalid locator record"))
    }

    fn read_current<D>(&self, digest: &mut D) -> Result<(u64, Digest32), SourceError>
    where
        D: RawDigest,
    {
        let bytes = read_bounded(&self.locators_dir().join("CURRENT"), CURRENT_BYTES as u64)?;
        decode_current(&bytes, digest)
    }

    fn locators_dir(&self) -> PathBuf {
        self.storage_root.join("objects").join("locators")
    }
}

struct LocatorFields {
    object_kind: RecordKindV3,
    object_id: Digest32,
    carrier_id: Digest32,
    offset: u64,
    length: u64,
    payload_sha256: Digest32,
    generation: u64,
}

fn locator_fields(record: &CanonicalRecordV3) -> Result<LocatorFields, SourceError> {
    let fields = record
        .fields()
        .ok_or(SourceError::Corrupt("locator fields"))?;
    Ok(LocatorFields {
        object_kind: source_kind(
            *fields
                .first()
                .and_then(|field| field.value().first())
                .ok_or(SourceError::Corrupt("locator object kind"))?,
        )?,
        object_id: digest_field(fields, 1)?,
        generation: u64_field(fields, 2)?,
        carrier_id: digest_field(fields, 4)?,
        offset: u64_field(fields, 5)?,
        length: u64_field(fields, 6)?,
        payload_sha256: digest_field(fields, 7)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn verify_locator(
    record: &CanonicalRecordV3,
    object_kind: RecordKindV3,
    object_id: Digest32,
    carrier_id: Digest32,
    offset: u64,
    length: u64,
    payload_sha256: Digest32,
    generation: u64,
) -> Result<(), SourceError> {
    let fields = locator_fields(record)?;
    if fields.object_kind != object_kind
        || fields.object_id != object_id
        || fields.carrier_id != carrier_id
        || fields.offset != offset
        || fields.length != length
        || fields.payload_sha256 != payload_sha256
        || fields.generation != generation
    {
        return Err(SourceError::Corrupt("locator fields changed"));
    }
    Ok(())
}

fn read_and_verify<D, C>(
    catalog: &C,
    locator: &CanonicalRecordV3,
    expected_kind: RecordKindV3,
    expected_id: Digest32,
    digest: &mut D,
) -> Result<(Vec<u8>, u64), SourceError>
where
    D: RawDigest + TypedDigest,
    C: CarrierCatalog,
{
    let fields = locator_fields(locator)?;
    if fields.object_kind != expected_kind || fields.object_id != expected_id {
        return Err(SourceError::Corrupt("locator target mismatch"));
    }
    let mut opened = catalog
        .open(fields.carrier_id)
        .map_err(|_| SourceError::Missing)?;
    if opened.generation == 0 {
        return Err(SourceError::Corrupt("zero carrier catalog generation"));
    }
    opened.file.seek(SeekFrom::Start(fields.offset))?;
    let allocation = usize::try_from(fields.length)
        .map_err(|_| SourceError::Corrupt("source length does not fit memory"))?;
    let mut payload = vec![0; allocation];
    opened
        .file
        .read_exact(&mut payload)
        .map_err(|_| SourceError::Missing)?;
    if digest.digest_bytes(&payload)? != fields.payload_sha256 {
        return Err(SourceError::Corrupt("raw payload digest mismatch"));
    }
    let mut encode = |sink: &mut dyn CanonicalSink| sink.write_all(&payload);
    let typed = digest.digest(
        DigestDomain::V3Record(expected_kind as u8),
        ROOT_FORMAT_V3,
        fields.length,
        &mut encode,
    )?;
    if typed != expected_id {
        return Err(SourceError::Corrupt("typed payload identity mismatch"));
    }
    Ok((payload, opened.generation))
}

fn ensure_source_kind(kind: RecordKindV3) -> Result<(), SourceError> {
    match kind {
        RecordKindV3::Root
        | RecordKindV3::Metadata
        | RecordKindV3::TreePage
        | RecordKindV3::FileNode
        | RecordKindV3::SegmentPage
        | RecordKindV3::Chunk
        | RecordKindV3::AttributionRoot
        | RecordKindV3::AttributionPage
        | RecordKindV3::HardlinkGroup => Ok(()),
        RecordKindV3::Head
        | RecordKindV3::OperationState
        | RecordKindV3::Locator
        | RecordKindV3::SourceLease => Err(SourceError::Corrupt(
            "mutable object cannot use a v1 locator",
        )),
    }
}

fn source_kind(value: u8) -> Result<RecordKindV3, SourceError> {
    let kind = match value {
        0x10 => RecordKindV3::Root,
        0x13 => RecordKindV3::Metadata,
        0x20 => RecordKindV3::TreePage,
        0x21 => RecordKindV3::FileNode,
        0x22 => RecordKindV3::SegmentPage,
        0x23 => RecordKindV3::Chunk,
        0x24 => RecordKindV3::AttributionRoot,
        0x25 => RecordKindV3::AttributionPage,
        0x26 => RecordKindV3::HardlinkGroup,
        _ => return Err(SourceError::Corrupt("unsupported locator object kind")),
    };
    Ok(kind)
}

fn digest_field(fields: &[TlvV3], index: usize) -> Result<Digest32, SourceError> {
    Ok(Digest32::new(
        fields
            .get(index)
            .ok_or(SourceError::Corrupt("missing locator field"))?
            .value()
            .try_into()
            .map_err(|_| SourceError::Corrupt("invalid locator digest"))?,
    ))
}

fn u64_field(fields: &[TlvV3], index: usize) -> Result<u64, SourceError> {
    Ok(u64::from_be_bytes(
        fields
            .get(index)
            .ok_or(SourceError::Corrupt("missing numeric field"))?
            .value()
            .try_into()
            .map_err(|_| SourceError::Corrupt("invalid numeric field"))?,
    ))
}

fn encode_current<D>(
    generation: u64,
    run_id: Digest32,
    digest: &mut D,
) -> Result<Vec<u8>, SourceError>
where
    D: RawDigest,
{
    let mut bytes = Vec::with_capacity(CURRENT_BYTES);
    bytes.extend_from_slice(CURRENT_MAGIC);
    bytes.extend_from_slice(&generation.to_be_bytes());
    bytes.extend_from_slice(run_id.as_bytes());
    let mut preimage = Vec::with_capacity(CURRENT_CHECKSUM_DOMAIN.len() + bytes.len());
    preimage.extend_from_slice(CURRENT_CHECKSUM_DOMAIN);
    preimage.extend_from_slice(&bytes);
    bytes.extend_from_slice(digest.digest_bytes(&preimage)?.as_bytes());
    Ok(bytes)
}

fn decode_current<D>(bytes: &[u8], digest: &mut D) -> Result<(u64, Digest32), SourceError>
where
    D: RawDigest,
{
    if bytes.len() != CURRENT_BYTES || bytes.get(..8) != Some(CURRENT_MAGIC) {
        return Err(SourceError::Corrupt("locator CURRENT framing"));
    }
    let generation = u64::from_be_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| SourceError::Corrupt("locator CURRENT generation"))?,
    );
    if generation == 0 {
        return Err(SourceError::Corrupt("zero locator generation"));
    }
    let run_id = Digest32::new(
        bytes[16..48]
            .try_into()
            .map_err(|_| SourceError::Corrupt("locator CURRENT run ID"))?,
    );
    let mut preimage = Vec::with_capacity(CURRENT_CHECKSUM_DOMAIN.len() + 48);
    preimage.extend_from_slice(CURRENT_CHECKSUM_DOMAIN);
    preimage.extend_from_slice(&bytes[..48]);
    if digest.digest_bytes(&preimage)?.as_bytes().as_slice() != &bytes[48..] {
        return Err(SourceError::Corrupt("locator CURRENT checksum"));
    }
    Ok((generation, run_id))
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, SourceError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SourceError::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(SourceError::Corrupt("unbounded locator file"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > maximum {
        return Err(SourceError::Corrupt("locator file changed during read"));
    }
    Ok(bytes)
}

fn install_immutable(path: &Path, bytes: &[u8]) -> Result<(), SourceError> {
    if path.exists() {
        return if std::fs::read(path)? == bytes {
            Ok(())
        } else {
            Err(SourceError::Corrupt("locator run ID collision"))
        };
    }
    let mut temp = TempFile::new(path)?;
    temp.write(bytes)?;
    match std::fs::hard_link(temp.path(), path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(path)? != bytes {
                return Err(SourceError::Corrupt("locator run ID collision"));
            }
        }
        Err(error) => return Err(error.into()),
    }
    temp.remove()?;
    fsync_parent(path)
}

fn replace_durable(path: &Path, bytes: &[u8]) -> Result<(), SourceError> {
    let mut temp = TempFile::new(path)?;
    temp.write(bytes)?;
    std::fs::rename(temp.path(), path)?;
    temp.disarm();
    fsync_parent(path)
}

struct TempFile {
    path: Option<PathBuf>,
}

impl TempFile {
    fn new(final_path: &Path) -> Result<Self, SourceError> {
        let parent = final_path
            .parent()
            .ok_or(SourceError::Corrupt("locator path has no parent"))?;
        let name = final_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("locator");
        let path = parent.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        Ok(Self { path: Some(path) })
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("temporary path is armed")
    }

    fn write(&self, bytes: &[u8]) -> Result<(), SourceError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path())?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }

    fn remove(&mut self) -> Result<(), SourceError> {
        let path = self
            .path
            .take()
            .ok_or(SourceError::Corrupt("temporary path consumed"))?;
        std::fs::remove_file(path)?;
        Ok(())
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn fsync_parent(path: &Path) -> Result<(), SourceError> {
    let parent = path
        .parent()
        .ok_or(SourceError::Corrupt("locator path has no parent"))?;
    fsync_dir(parent)
}

#[cfg(unix)]
fn fsync_dir(path: &Path) -> Result<(), SourceError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn fsync_dir(_path: &Path) -> Result<(), SourceError> {
    Ok(())
}

fn hex(digest: Digest32) -> String {
    let mut result = String::with_capacity(64);
    for byte in digest.as_bytes() {
        use fmt::Write as _;
        let _ = write!(result, "{byte:02x}");
    }
    result
}
