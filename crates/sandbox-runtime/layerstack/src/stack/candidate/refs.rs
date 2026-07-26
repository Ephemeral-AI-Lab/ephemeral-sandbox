use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sandbox_runtime_layerstack_core::{
    decode_v3_record_as, encode_v3_record, BranchId, CanonicalRecordV3, CanonicalSink,
    CanonicalSource, Digest32, Error, ErrorKind, FieldClass, LeaseId, PinId, RawDigest,
    RecordKindV3, TlvV3, TypedDigest, ROOT_FORMAT_V3,
};

use super::object_store::{LooseObjectStore, ObjectStoreError};

const CONTROL_MAGIC: &[u8; 8] = b"EOSLS3CT";
const PAIR_REF_MAGIC: &[u8; 8] = b"EOSLS3RF";
const CONTROL_CHECKSUM_DOMAIN: &[u8] = b"EOS-LS3-CONTROL\0";
const PAIR_REF_CHECKSUM_DOMAIN: &[u8] = b"EOS-LS3-PAIR-REF\0";
const CONTROL_BYTES: usize = 8 + 2 + 8 + 32;
const PAIR_REF_BYTES: usize = 8 + 2 + 1 + 32 + 32 + 1 + 32;
const HEAD_MAX_BYTES: u64 = 256;
const SOURCE_LEASE_MAX_BYTES: u64 = 512;
const SUPPORTED_CAPABILITIES: u64 = 0x3f;
const STORAGE_WRITER_LOCK_FILE: &str = ".storage-writer.lock";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefClass {
    Head = 1,
    Checkpoint = 2,
    Pin = 3,
    Lease = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RefTarget {
    pub(crate) root: Digest32,
    pub(crate) attribution_root: Digest32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Head {
    pub(crate) target: RefTarget,
    pub(crate) generation: u64,
    pub(crate) publication_id: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Pin {
    pub(crate) target: RefTarget,
    pub(crate) reason_class: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BarrierSubject {
    pub(crate) class: RefClass,
    pub(crate) root: Digest32,
    pub(crate) attribution_root: Option<Digest32>,
}

pub(crate) trait CommitLock {
    fn with_exclusive<T, F>(&self, operation: F) -> Result<T, RefError>
    where
        F: FnOnce() -> Result<T, RefError>;
}

pub(crate) trait GcBarrier {
    fn participate(&self, subject: BarrierSubject) -> Result<(), RefError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NoGcBarrier;

impl GcBarrier for NoGcBarrier {
    fn participate(&self, _subject: BarrierSubject) -> Result<(), RefError> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefStage {
    TempCreated,
    BytesWritten,
    FileFsynced,
    LockAcquired,
    BarrierRegistered,
    BeforeVisibility,
    AfterVisibility,
    ParentFsynced,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RefCounters {
    pub(crate) prepared_files: u64,
    pub(crate) prepared_bytes: u64,
    pub(crate) lock_sections: u64,
    pub(crate) lock_wait_nanoseconds: u64,
    pub(crate) lock_hold_nanoseconds: u64,
    pub(crate) barrier_registrations: u64,
    pub(crate) visible_ref_writes: u64,
    pub(crate) payload_object_writes: u64,
    pub(crate) native_tree_writes: u64,
}

#[derive(Debug)]
pub(crate) enum RefError {
    Io(std::io::Error),
    Core(Error),
    ObjectStore(ObjectStoreError),
    Lock(String),
    Invalid(&'static str),
    IdentifierCollision,
    HeadMismatch,
    GenerationOverflow,
    Injected(RefStage),
}

impl RefError {
    pub(crate) const fn kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Core(error) => Some(error.kind()),
            Self::ObjectStore(error) => error.kind(),
            Self::IdentifierCollision => Some(ErrorKind::IdentifierCollision),
            Self::HeadMismatch => Some(ErrorKind::Conflict),
            Self::GenerationOverflow => Some(ErrorKind::GenerationOverflow),
            Self::Io(_) | Self::Lock(_) | Self::Invalid(_) | Self::Injected(_) => None,
        }
    }
}

impl fmt::Display for RefError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "candidate ref I/O failed: {error}"),
            Self::Core(error) => write!(formatter, "candidate ref codec failed: {error}"),
            Self::ObjectStore(error) => write!(formatter, "candidate ref target failed: {error}"),
            Self::Lock(error) => write!(formatter, "candidate ref lock failed: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid candidate ref: {message}"),
            Self::IdentifierCollision => write!(formatter, "candidate ref identifier collision"),
            Self::HeadMismatch => write!(formatter, "candidate head changed"),
            Self::GenerationOverflow => write!(formatter, "candidate head generation overflow"),
            Self::Injected(stage) => write!(formatter, "injected candidate ref stop at {stage:?}"),
        }
    }
}

impl std::error::Error for RefError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Core(error) => Some(error),
            Self::ObjectStore(error) => Some(error),
            Self::Lock(_)
            | Self::Invalid(_)
            | Self::IdentifierCollision
            | Self::HeadMismatch
            | Self::GenerationOverflow
            | Self::Injected(_) => None,
        }
    }
}

impl From<std::io::Error> for RefError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<Error> for RefError {
    fn from(error: Error) -> Self {
        Self::Core(error)
    }
}

impl From<ObjectStoreError> for RefError {
    fn from(error: ObjectStoreError) -> Self {
        Self::ObjectStore(error)
    }
}

pub(crate) struct RefStore<'a, L, B> {
    storage_root: PathBuf,
    objects: LooseObjectStore,
    lock: &'a L,
    barrier: &'a B,
    counters: RefCounters,
}

impl<'a, L, B> RefStore<'a, L, B>
where
    L: CommitLock,
    B: GcBarrier,
{
    pub(crate) fn open<D>(
        storage_root: PathBuf,
        lock: &'a L,
        barrier: &'a B,
        digest: &mut D,
    ) -> Result<Self, RefError>
    where
        D: RawDigest,
    {
        let objects = LooseObjectStore::new(storage_root.clone())?;
        let mut store = Self {
            storage_root,
            objects,
            lock,
            barrier,
            counters: RefCounters::default(),
        };
        store.ensure_control(digest)?;
        Ok(store)
    }

    pub(crate) const fn counters(&self) -> RefCounters {
        self.counters
    }

    pub(crate) fn head_path(&self, branch: &BranchId) -> PathBuf {
        self.storage_root
            .join("refs")
            .join("heads")
            .join(ascii_component(branch.as_bytes()))
    }

    pub(crate) fn checkpoint_path(&self, checkpoint: &BranchId) -> PathBuf {
        self.storage_root
            .join("refs")
            .join("checkpoints")
            .join(ascii_component(checkpoint.as_bytes()))
    }

    pub(crate) fn pin_path(&self, pin: &PinId) -> PathBuf {
        self.storage_root
            .join("refs")
            .join("pins")
            .join(hex_component(pin.as_bytes()))
    }

    pub(crate) fn lease_path(&self, lease: &LeaseId) -> PathBuf {
        self.storage_root
            .join("refs")
            .join("leases")
            .join(hex_component(lease.as_bytes()))
    }

    pub(crate) fn read_head<D>(
        &self,
        branch: &BranchId,
        digest: &mut D,
    ) -> Result<Option<Head>, RefError>
    where
        D: RawDigest,
    {
        read_optional(&self.head_path(branch), HEAD_MAX_BYTES)?
            .map(|bytes| decode_head(&bytes, digest))
            .transpose()
    }

    pub(crate) fn commit_head<D>(
        &mut self,
        branch: &BranchId,
        expected: Option<Head>,
        next: Head,
        digest: &mut D,
    ) -> Result<(), RefError>
    where
        D: RawDigest + TypedDigest,
    {
        self.commit_head_with_hook(branch, expected, next, digest, |_| Ok(()))
    }

    pub(crate) fn commit_head_with_hook<D, F>(
        &mut self,
        branch: &BranchId,
        expected: Option<Head>,
        next: Head,
        digest: &mut D,
        mut hook: F,
    ) -> Result<(), RefError>
    where
        D: RawDigest + TypedDigest,
        F: FnMut(RefStage) -> Result<(), RefError>,
    {
        validate_generation(expected, next)?;
        self.validate_pair(next.target, digest)?;
        let bytes = encode_head(next, digest)?;
        decode_head(&bytes, digest)?;
        let final_path = self.head_path(branch);
        let mut prepared = PreparedFile::new(&final_path, &bytes, &mut hook)?;
        self.counters.prepared_files = self.counters.prepared_files.saturating_add(1);
        self.counters.prepared_bytes = self
            .counters
            .prepared_bytes
            .saturating_add(bytes.len() as u64);

        let wait_started = Instant::now();
        let mut acquired = None;
        let subject = BarrierSubject {
            class: RefClass::Head,
            root: next.target.root,
            attribution_root: Some(next.target.attribution_root),
        };
        let result = self.lock.with_exclusive(|| {
            acquired = Some(Instant::now());
            hook(RefStage::LockAcquired)?;
            let current = read_optional(&final_path, HEAD_MAX_BYTES)?
                .map(|current| decode_head(&current, digest))
                .transpose()?;
            if current == Some(next) {
                fsync_parent(&final_path)?;
                return Ok(());
            }
            if current != expected {
                return Err(RefError::HeadMismatch);
            }
            self.barrier.participate(subject)?;
            self.counters.barrier_registrations =
                self.counters.barrier_registrations.saturating_add(1);
            hook(RefStage::BarrierRegistered)?;
            hook(RefStage::BeforeVisibility)?;
            prepared.replace(&final_path)?;
            self.counters.visible_ref_writes = self.counters.visible_ref_writes.saturating_add(1);
            hook(RefStage::AfterVisibility)?;
            fsync_parent(&final_path)?;
            hook(RefStage::ParentFsynced)
        });
        let finished = Instant::now();
        self.record_lock_timing(wait_started, acquired, finished);
        result
    }

    pub(crate) fn create_checkpoint<D>(
        &mut self,
        checkpoint: &BranchId,
        target: RefTarget,
        digest: &mut D,
    ) -> Result<(), RefError>
    where
        D: RawDigest + TypedDigest,
    {
        self.validate_pair(target, digest)?;
        let bytes = encode_pair_ref(RefClass::Checkpoint, target, 0, digest)?;
        decode_pair_ref(&bytes, RefClass::Checkpoint, digest)?;
        self.install_named_ref(
            self.checkpoint_path(checkpoint),
            bytes,
            BarrierSubject {
                class: RefClass::Checkpoint,
                root: target.root,
                attribution_root: Some(target.attribution_root),
            },
            |_| Ok(()),
        )
    }

    pub(crate) fn read_checkpoint<D>(
        &self,
        checkpoint: &BranchId,
        digest: &mut D,
    ) -> Result<Option<RefTarget>, RefError>
    where
        D: RawDigest,
    {
        read_optional(&self.checkpoint_path(checkpoint), PAIR_REF_BYTES as u64)?
            .map(|bytes| {
                decode_pair_ref(&bytes, RefClass::Checkpoint, digest).map(|(target, _)| target)
            })
            .transpose()
    }

    pub(crate) fn delete_checkpoint<D>(
        &mut self,
        checkpoint: &BranchId,
        digest: &mut D,
    ) -> Result<bool, RefError>
    where
        D: RawDigest,
    {
        let path = self.checkpoint_path(checkpoint);
        let wait_started = Instant::now();
        let mut acquired = None;
        let removed = self.lock.with_exclusive(|| {
            acquired = Some(Instant::now());
            let Some(bytes) = read_optional(&path, PAIR_REF_BYTES as u64)? else {
                return Ok(false);
            };
            let _ = decode_pair_ref(&bytes, RefClass::Checkpoint, digest)?;
            std::fs::remove_file(&path)?;
            fsync_parent(&path)?;
            Ok(true)
        });
        let finished = Instant::now();
        self.record_lock_timing(wait_started, acquired, finished);
        removed
    }

    pub(crate) fn create_pin<D>(
        &mut self,
        id: &PinId,
        pin: Pin,
        digest: &mut D,
    ) -> Result<(), RefError>
    where
        D: RawDigest + TypedDigest,
    {
        if pin.reason_class == 0 {
            return Err(RefError::Invalid("pin reason class must be nonzero"));
        }
        self.validate_pair(pin.target, digest)?;
        let bytes = encode_pair_ref(RefClass::Pin, pin.target, pin.reason_class, digest)?;
        decode_pair_ref(&bytes, RefClass::Pin, digest)?;
        self.install_named_ref(
            self.pin_path(id),
            bytes,
            BarrierSubject {
                class: RefClass::Pin,
                root: pin.target.root,
                attribution_root: Some(pin.target.attribution_root),
            },
            |_| Ok(()),
        )
    }

    pub(crate) fn read_pin<D>(&self, id: &PinId, digest: &mut D) -> Result<Option<Pin>, RefError>
    where
        D: RawDigest,
    {
        read_optional(&self.pin_path(id), PAIR_REF_BYTES as u64)?
            .map(|bytes| {
                decode_pair_ref(&bytes, RefClass::Pin, digest).map(|(target, reason_class)| Pin {
                    target,
                    reason_class,
                })
            })
            .transpose()
    }

    pub(crate) fn install_source_lease<D>(
        &mut self,
        id: &LeaseId,
        record: &CanonicalRecordV3,
        digest: &mut D,
    ) -> Result<(), RefError>
    where
        D: RawDigest + TypedDigest,
    {
        if record.kind() != RecordKindV3::SourceLease {
            return Err(RefError::Invalid("lease record has the wrong kind"));
        }
        let fields = record
            .fields()
            .ok_or(RefError::Invalid("lease record has no fields"))?;
        if fields.first().map(TlvV3::value) != Some(id.as_bytes()) {
            return Err(RefError::Invalid("lease path ID differs from record ID"));
        }
        let root = digest_field(fields, 1)?;
        self.objects.load(RecordKindV3::Root, root, digest)?;
        let bytes = encode_record(record)?;
        decode_record(&bytes, RecordKindV3::SourceLease, digest)?;
        self.install_named_ref(
            self.lease_path(id),
            bytes,
            BarrierSubject {
                class: RefClass::Lease,
                root,
                attribution_root: None,
            },
            |_| Ok(()),
        )
    }

    pub(crate) fn read_source_lease<D>(
        &self,
        id: &LeaseId,
        digest: &mut D,
    ) -> Result<Option<CanonicalRecordV3>, RefError>
    where
        D: RawDigest,
    {
        read_optional(&self.lease_path(id), SOURCE_LEASE_MAX_BYTES)?
            .map(|bytes| {
                let record = decode_record(&bytes, RecordKindV3::SourceLease, digest)?;
                let fields = record
                    .fields()
                    .ok_or(RefError::Invalid("lease record has no fields"))?;
                if fields.first().map(TlvV3::value) != Some(id.as_bytes()) {
                    return Err(RefError::Invalid("lease path ID differs from record ID"));
                }
                Ok(record)
            })
            .transpose()
    }

    pub(crate) fn delete_source_lease<D>(
        &mut self,
        id: &LeaseId,
        digest: &mut D,
    ) -> Result<bool, RefError>
    where
        D: RawDigest,
    {
        let path = self.lease_path(id);
        let wait_started = Instant::now();
        let mut acquired = None;
        let removed = self.lock.with_exclusive(|| {
            acquired = Some(Instant::now());
            let Some(bytes) = read_optional(&path, SOURCE_LEASE_MAX_BYTES)? else {
                return Ok(false);
            };
            let record = decode_record(&bytes, RecordKindV3::SourceLease, digest)?;
            let fields = record
                .fields()
                .ok_or(RefError::Invalid("lease record has no fields"))?;
            if fields.first().map(TlvV3::value) != Some(id.as_bytes()) {
                return Err(RefError::Invalid("lease path ID differs from record ID"));
            }
            std::fs::remove_file(&path)?;
            fsync_parent(&path)?;
            Ok(true)
        });
        let finished = Instant::now();
        self.record_lock_timing(wait_started, acquired, finished);
        removed
    }

    fn install_named_ref<F>(
        &mut self,
        final_path: PathBuf,
        bytes: Vec<u8>,
        subject: BarrierSubject,
        mut hook: F,
    ) -> Result<(), RefError>
    where
        F: FnMut(RefStage) -> Result<(), RefError>,
    {
        if let Some(existing) = read_optional(&final_path, bytes.len() as u64)? {
            if existing != bytes {
                return Err(RefError::IdentifierCollision);
            }
            fsync_parent(&final_path)?;
            return Ok(());
        }
        let mut prepared = PreparedFile::new(&final_path, &bytes, &mut hook)?;
        self.counters.prepared_files = self.counters.prepared_files.saturating_add(1);
        self.counters.prepared_bytes = self
            .counters
            .prepared_bytes
            .saturating_add(bytes.len() as u64);

        let wait_started = Instant::now();
        let mut acquired = None;
        let result = self.lock.with_exclusive(|| {
            acquired = Some(Instant::now());
            hook(RefStage::LockAcquired)?;
            if let Some(existing) = read_optional(&final_path, bytes.len() as u64)? {
                if existing != bytes {
                    return Err(RefError::IdentifierCollision);
                }
                fsync_parent(&final_path)?;
                return Ok(());
            }
            self.barrier.participate(subject)?;
            self.counters.barrier_registrations =
                self.counters.barrier_registrations.saturating_add(1);
            hook(RefStage::BarrierRegistered)?;
            hook(RefStage::BeforeVisibility)?;
            prepared.install_no_replace(&final_path)?;
            self.counters.visible_ref_writes = self.counters.visible_ref_writes.saturating_add(1);
            hook(RefStage::AfterVisibility)?;
            fsync_parent(&final_path)?;
            hook(RefStage::ParentFsynced)
        });
        let finished = Instant::now();
        self.record_lock_timing(wait_started, acquired, finished);
        result
    }

    fn validate_pair<D>(&self, target: RefTarget, digest: &mut D) -> Result<(), RefError>
    where
        D: RawDigest + TypedDigest,
    {
        self.objects.load(RecordKindV3::Root, target.root, digest)?;
        let attribution = self.objects.load(
            RecordKindV3::AttributionRoot,
            target.attribution_root,
            digest,
        )?;
        let fields = attribution
            .fields()
            .ok_or(RefError::Invalid("attribution root has no fields"))?;
        if digest_field(fields, 1)? != target.root {
            return Err(RefError::Invalid(
                "attribution root names another content root",
            ));
        }
        Ok(())
    }

    fn ensure_control<D>(&mut self, digest: &mut D) -> Result<(), RefError>
    where
        D: RawDigest,
    {
        let path = self.storage_root.join("CONTROL");
        if let Some(bytes) = read_optional(&path, CONTROL_BYTES as u64)? {
            return decode_control(&bytes, digest);
        }
        let bytes = encode_control(digest)?;
        let mut prepared = PreparedFile::new(&path, &bytes, &mut |_| Ok(()))?;
        self.counters.prepared_files = self.counters.prepared_files.saturating_add(1);
        self.counters.prepared_bytes = self
            .counters
            .prepared_bytes
            .saturating_add(bytes.len() as u64);
        let wait_started = Instant::now();
        let mut acquired = None;
        let result = self.lock.with_exclusive(|| {
            acquired = Some(Instant::now());
            if let Some(existing) = read_optional(&path, CONTROL_BYTES as u64)? {
                return decode_control(&existing, digest);
            }
            prepared.install_no_replace(&path)?;
            fsync_parent(&path)
        });
        let finished = Instant::now();
        self.record_lock_timing(wait_started, acquired, finished);
        result
    }

    fn record_lock_timing(
        &mut self,
        wait_started: Instant,
        acquired: Option<Instant>,
        finished: Instant,
    ) {
        let Some(acquired) = acquired else {
            return;
        };
        self.counters.lock_sections = self.counters.lock_sections.saturating_add(1);
        self.counters.lock_wait_nanoseconds = self
            .counters
            .lock_wait_nanoseconds
            .saturating_add(nanoseconds(acquired.duration_since(wait_started)));
        self.counters.lock_hold_nanoseconds = self
            .counters
            .lock_hold_nanoseconds
            .saturating_add(nanoseconds(finished.duration_since(acquired)));
    }
}

fn validate_generation(expected: Option<Head>, next: Head) -> Result<(), RefError> {
    match expected {
        None if next.generation == 0 => Ok(()),
        None => Err(RefError::Invalid("new head generation must be zero")),
        Some(current) => {
            let generation = current
                .generation
                .checked_add(1)
                .ok_or(RefError::GenerationOverflow)?;
            if next.generation == generation {
                Ok(())
            } else {
                Err(RefError::Invalid(
                    "head replacement must advance generation exactly once",
                ))
            }
        }
    }
}

fn encode_head<D>(head: Head, digest: &mut D) -> Result<Vec<u8>, RefError>
where
    D: RawDigest,
{
    let record = CanonicalRecordV3::mutable(
        RecordKindV3::Head,
        vec![
            TlvV3::new(1, head.target.root.into_bytes().to_vec()),
            TlvV3::new(2, head.target.attribution_root.into_bytes().to_vec()),
            TlvV3::new(3, head.generation.to_be_bytes().to_vec()),
            TlvV3::new(4, head.publication_id.to_vec()),
        ],
        digest,
    )?;
    encode_record(&record)
}

fn decode_head<D>(bytes: &[u8], digest: &mut D) -> Result<Head, RefError>
where
    D: RawDigest,
{
    let record = decode_record(bytes, RecordKindV3::Head, digest)?;
    let fields = record
        .fields()
        .ok_or(RefError::Invalid("head has no fields"))?;
    Ok(Head {
        target: RefTarget {
            root: digest_field(fields, 0)?,
            attribution_root: digest_field(fields, 1)?,
        },
        generation: u64::from_be_bytes(
            fields[2]
                .value()
                .try_into()
                .map_err(|_| RefError::Invalid("head generation"))?,
        ),
        publication_id: fields[3]
            .value()
            .try_into()
            .map_err(|_| RefError::Invalid("head publication ID"))?,
    })
}

fn encode_pair_ref<D>(
    class: RefClass,
    target: RefTarget,
    reason_class: u8,
    digest: &mut D,
) -> Result<Vec<u8>, RefError>
where
    D: RawDigest,
{
    if !matches!(class, RefClass::Checkpoint | RefClass::Pin) {
        return Err(RefError::Invalid("pair ref class"));
    }
    if class == RefClass::Checkpoint && reason_class != 0 {
        return Err(RefError::Invalid("checkpoint reason class"));
    }
    if class == RefClass::Pin && reason_class == 0 {
        return Err(RefError::Invalid("pin reason class"));
    }
    let mut bytes = Vec::with_capacity(PAIR_REF_BYTES);
    bytes.extend_from_slice(PAIR_REF_MAGIC);
    bytes.extend_from_slice(&ROOT_FORMAT_V3.get().to_be_bytes());
    bytes.push(class as u8);
    bytes.extend_from_slice(target.root.as_bytes());
    bytes.extend_from_slice(target.attribution_root.as_bytes());
    bytes.push(reason_class);
    let mut preimage = Vec::with_capacity(PAIR_REF_CHECKSUM_DOMAIN.len() + bytes.len());
    preimage.extend_from_slice(PAIR_REF_CHECKSUM_DOMAIN);
    preimage.extend_from_slice(&bytes);
    bytes.extend_from_slice(digest.digest_bytes(&preimage)?.as_bytes());
    Ok(bytes)
}

fn decode_pair_ref<D>(
    bytes: &[u8],
    expected: RefClass,
    digest: &mut D,
) -> Result<(RefTarget, u8), RefError>
where
    D: RawDigest,
{
    if bytes.len() != PAIR_REF_BYTES
        || bytes.get(..8) != Some(PAIR_REF_MAGIC)
        || bytes.get(8..10) != Some(ROOT_FORMAT_V3.get().to_be_bytes().as_slice())
        || bytes.get(10) != Some(&(expected as u8))
    {
        return Err(RefError::Invalid("pair ref framing"));
    }
    let reason_class = bytes[75];
    if (expected == RefClass::Checkpoint && reason_class != 0)
        || (expected == RefClass::Pin && reason_class == 0)
    {
        return Err(RefError::Invalid("pair ref reason class"));
    }
    let checksum_start = PAIR_REF_BYTES - 32;
    let mut preimage = Vec::with_capacity(PAIR_REF_CHECKSUM_DOMAIN.len() + checksum_start);
    preimage.extend_from_slice(PAIR_REF_CHECKSUM_DOMAIN);
    preimage.extend_from_slice(&bytes[..checksum_start]);
    let checksum = digest.digest_bytes(&preimage)?;
    if checksum.as_bytes().as_slice() != &bytes[checksum_start..] {
        return Err(RefError::Core(Error::new(
            ErrorKind::ChecksumMismatch,
            ROOT_FORMAT_V3,
            FieldClass::Checksum,
            255,
        )));
    }
    Ok((
        RefTarget {
            root: Digest32::new(
                bytes[11..43]
                    .try_into()
                    .map_err(|_| RefError::Invalid("pair content root"))?,
            ),
            attribution_root: Digest32::new(
                bytes[43..75]
                    .try_into()
                    .map_err(|_| RefError::Invalid("pair attribution root"))?,
            ),
        },
        reason_class,
    ))
}

fn encode_control<D>(digest: &mut D) -> Result<Vec<u8>, RefError>
where
    D: RawDigest,
{
    let mut bytes = Vec::with_capacity(CONTROL_BYTES);
    bytes.extend_from_slice(CONTROL_MAGIC);
    bytes.extend_from_slice(&ROOT_FORMAT_V3.get().to_be_bytes());
    bytes.extend_from_slice(&SUPPORTED_CAPABILITIES.to_be_bytes());
    let mut preimage = Vec::with_capacity(CONTROL_CHECKSUM_DOMAIN.len() + bytes.len());
    preimage.extend_from_slice(CONTROL_CHECKSUM_DOMAIN);
    preimage.extend_from_slice(&bytes);
    bytes.extend_from_slice(digest.digest_bytes(&preimage)?.as_bytes());
    Ok(bytes)
}

fn decode_control<D>(bytes: &[u8], digest: &mut D) -> Result<(), RefError>
where
    D: RawDigest,
{
    if bytes.len() != CONTROL_BYTES
        || bytes.get(..8) != Some(CONTROL_MAGIC)
        || bytes.get(8..10) != Some(ROOT_FORMAT_V3.get().to_be_bytes().as_slice())
        || bytes.get(10..18) != Some(SUPPORTED_CAPABILITIES.to_be_bytes().as_slice())
    {
        return Err(RefError::Invalid("CONTROL framing or capability set"));
    }
    let mut preimage = Vec::with_capacity(CONTROL_CHECKSUM_DOMAIN.len() + 18);
    preimage.extend_from_slice(CONTROL_CHECKSUM_DOMAIN);
    preimage.extend_from_slice(&bytes[..18]);
    let checksum = digest.digest_bytes(&preimage)?;
    if checksum.as_bytes().as_slice() != &bytes[18..] {
        return Err(RefError::Core(Error::new(
            ErrorKind::ChecksumMismatch,
            ROOT_FORMAT_V3,
            FieldClass::Checksum,
            255,
        )));
    }
    Ok(())
}

pub(crate) fn encode_record(record: &CanonicalRecordV3) -> Result<Vec<u8>, RefError> {
    let mut sink = VecSink::default();
    encode_v3_record(record, &mut sink)?;
    Ok(sink.bytes)
}

pub(crate) fn decode_record<D>(
    bytes: &[u8],
    expected: RecordKindV3,
    digest: &mut D,
) -> Result<CanonicalRecordV3, RefError>
where
    D: RawDigest,
{
    let mut source = SliceSource::new(bytes);
    Ok(decode_v3_record_as(&mut source, expected, digest)?)
}

fn digest_field(fields: &[TlvV3], index: usize) -> Result<Digest32, RefError> {
    Ok(Digest32::new(
        fields
            .get(index)
            .ok_or(RefError::Invalid("missing digest field"))?
            .value()
            .try_into()
            .map_err(|_| RefError::Invalid("digest field length"))?,
    ))
}

fn read_optional(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, RefError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(RefError::Invalid("ref is not a bounded regular file"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > maximum {
        return Err(RefError::Invalid("ref changed or exceeded its bound"));
    }
    Ok(Some(bytes))
}

struct PreparedFile {
    path: Option<PathBuf>,
}

impl PreparedFile {
    fn new<F>(final_path: &Path, bytes: &[u8], hook: &mut F) -> Result<Self, RefError>
    where
        F: FnMut(RefStage) -> Result<(), RefError>,
    {
        let parent = final_path
            .parent()
            .ok_or(RefError::Invalid("ref has no parent"))?;
        let parent_existed = parent.try_exists()?;
        std::fs::create_dir_all(parent)?;
        if !parent_existed {
            fsync_parent(parent)?;
        }
        let file_name = final_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("ref");
        let path = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let prepared = Self {
            path: Some(path.clone()),
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        hook(RefStage::TempCreated)?;
        file.write_all(bytes)?;
        hook(RefStage::BytesWritten)?;
        file.sync_all()?;
        hook(RefStage::FileFsynced)?;
        drop(file);
        if std::fs::read(&path)? != bytes {
            return Err(RefError::Invalid("prepared ref validation failed"));
        }
        Ok(prepared)
    }

    fn replace(&mut self, final_path: &Path) -> Result<(), RefError> {
        let path = self
            .path
            .take()
            .ok_or(RefError::Invalid("prepared ref already consumed"))?;
        std::fs::rename(path, final_path)?;
        Ok(())
    }

    fn install_no_replace(&mut self, final_path: &Path) -> Result<(), RefError> {
        let path = self
            .path
            .as_ref()
            .ok_or(RefError::Invalid("prepared ref already consumed"))?;
        std::fs::hard_link(path, final_path)?;
        let path = self
            .path
            .take()
            .ok_or(RefError::Invalid("prepared ref already consumed"))?;
        std::fs::remove_file(path)?;
        Ok(())
    }
}

impl Drop for PreparedFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[derive(Default)]
struct VecSink {
    bytes: Vec<u8>,
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
                FieldClass::Length,
                0,
            )
        })?;
        let bytes = self.bytes.get(self.position..end).ok_or_else(|| {
            Error::new(
                ErrorKind::CorruptRecord,
                ROOT_FORMAT_V3,
                FieldClass::Record,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            )
        })?;
        output.copy_from_slice(bytes);
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
                FieldClass::Record,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ))
        }
    }
}

fn ascii_component(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

fn hex_component(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn nanoseconds(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(not(windows))]
fn fsync_dir(path: &Path) -> Result<(), RefError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn fsync_dir(_path: &Path) -> Result<(), RefError> {
    Ok(())
}

fn fsync_parent(path: &Path) -> Result<(), RefError> {
    let parent = path
        .parent()
        .ok_or(RefError::Invalid("ref has no parent"))?;
    fsync_dir(parent)
}

pub(crate) fn storage_writer_lock_path(storage_root: &Path) -> PathBuf {
    storage_root.join(STORAGE_WRITER_LOCK_FILE)
}

pub(crate) fn root_has_pin_or_source_lease<D>(
    storage_root: &Path,
    root: Digest32,
    digest: &mut D,
) -> Result<bool, RefError>
where
    D: RawDigest,
{
    let pins = storage_root.join("refs").join("pins");
    for (name, bytes) in read_ref_directory(&pins, PAIR_REF_BYTES as u64)? {
        validate_hex_ref_name(&name)?;
        let (target, _) = decode_pair_ref(&bytes, RefClass::Pin, digest)?;
        if target.root == root {
            return Ok(true);
        }
    }

    let leases = storage_root.join("refs").join("leases");
    let lease_entries = match std::fs::read_dir(&leases) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    for entry in lease_entries.into_iter().flatten() {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RefError::Invalid("non-UTF-8 ref path"))?;
        if name.starts_with("materialization-") {
            continue;
        }
        validate_hex_ref_name(&name)?;
        let bytes = read_optional(&entry.path(), SOURCE_LEASE_MAX_BYTES)?
            .ok_or(RefError::Invalid("ref disappeared while scanning"))?;
        let record = decode_record(&bytes, RecordKindV3::SourceLease, digest)?;
        let fields = record
            .fields()
            .ok_or(RefError::Invalid("lease record has no fields"))?;
        if fields
            .first()
            .map(TlvV3::value)
            .map(hex_component)
            .as_deref()
            != Some(name.as_str())
        {
            return Err(RefError::Invalid("lease path ID differs from record ID"));
        }
        if digest_field(fields, 1)? == root {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_ref_directory(
    path: &Path,
    maximum_file_bytes: u64,
) -> Result<Vec<(String, Vec<u8>)>, RefError> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut refs = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RefError::Invalid("non-UTF-8 ref path"))?;
        let bytes = read_optional(&entry.path(), maximum_file_bytes)?
            .ok_or(RefError::Invalid("ref disappeared while scanning"))?;
        refs.push((name, bytes));
    }
    Ok(refs)
}

fn validate_hex_ref_name(name: &str) -> Result<(), RefError> {
    if name.len() != 64
        || !name
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(RefError::Invalid("ref path is not canonical lowercase hex"));
    }
    Ok(())
}
