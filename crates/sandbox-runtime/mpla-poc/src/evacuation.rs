use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{fsync_dir, read_json, replace_json, write_immutable_json, FileLock};
use crate::locator::{LocatorReplacement, LocatorStore, PayloadRootId};
use crate::{
    unix_time_ms, AllocationId, LocatorDurabilityReceipt, LocatorGeneration, NamedFaultInjector,
    OperationId, PocError, PocResult, PublicationId, SCHEMA_VERSION,
};

pub const EVACUATION_FORMAT: &str = "mpla-poc-evacuation-v1";
const READER_PIN_FORMAT: &str = "mpla-poc-evacuation-reader-pin-v1";
const COPY_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvacuationPhase {
    Building,
    Ready,
    LocatorPublished,
    Terminal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvacuationRequest {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub payload_root: PayloadRootId,
    pub source_allocation_id: AllocationId,
    pub source_owner_epoch: u64,
    pub source_generation: LocatorGeneration,
    pub source_payload_path: PathBuf,
    pub source_logical_bytes: u64,
    pub source_allocated_bytes: u64,
    pub target_allocation_id: AllocationId,
    pub target_owner_epoch: u64,
    pub target_payload_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvacuationLocatorReceipt {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub payload_root: PayloadRootId,
    pub source_allocation_id: AllocationId,
    pub source_owner_epoch: u64,
    pub source_generation: LocatorGeneration,
    pub target_allocation_id: AllocationId,
    pub target_owner_epoch: u64,
    pub target_generation: LocatorGeneration,
    pub locator_durability: LocatorDurabilityReceipt,
    pub forward_replacement_complete: bool,
    pub reverse_replacement_complete: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StageFiveRetirementAuthorization {
    pub schema_version: u32,
    pub authorization_id: OperationId,
    pub evacuation_operation_id: OperationId,
    pub payload_root: PayloadRootId,
    pub source_allocation_id: AllocationId,
    pub source_owner_epoch: u64,
    pub selected_generation: LocatorGeneration,
    pub deletion_authorized: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvacuationSnapshot {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub payload_root: PayloadRootId,
    pub phase: EvacuationPhase,
    pub request_sha256: String,
    pub payload_sha256: Option<String>,
    pub source_logical_bytes: u64,
    pub source_allocated_bytes: u64,
    pub target_logical_bytes: u64,
    pub target_allocated_bytes: u64,
    pub honest_old_plus_new_peak_bytes: u64,
    pub active_reader_pins: u64,
    pub retirement_debt_objects: u64,
    pub retirement_debt_bytes: u64,
    pub selected_generation: LocatorGeneration,
    pub source_present: bool,
    pub target_present: bool,
}

#[derive(Clone, Debug)]
pub struct EvacuationStore {
    root: PathBuf,
}

#[derive(Debug)]
pub struct PinnedEvacuationReader {
    file: File,
    pin_path: PathBuf,
    pin_parent: PathBuf,
    generation: LocatorGeneration,
    allocation_id: AllocationId,
    owner_epoch: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DurableEvacuationRecord {
    schema_version: u32,
    format: String,
    request_sha256: String,
    request: EvacuationRequest,
    phase: EvacuationPhase,
    payload_sha256: Option<String>,
    target_logical_bytes: u64,
    target_allocated_bytes: u64,
    honest_old_plus_new_peak_bytes: u64,
    locator_receipt: Option<EvacuationLocatorReceipt>,
    retirement_debt_bytes: u64,
    state_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DurableReaderPin {
    schema_version: u32,
    format: String,
    token: OperationId,
    evacuation_operation_id: OperationId,
    generation: LocatorGeneration,
    allocation_id: AllocationId,
    owner_epoch: u64,
    created_unix_ms: u64,
    checksum_sha256: String,
}

impl EvacuationStore {
    pub fn open(root: impl Into<PathBuf>) -> PocResult<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("operations")).map_err(|source| {
            PocError::io(
                "create evacuation operations directory",
                root.join("operations"),
                source,
            )
        })?;
        std::fs::create_dir_all(root.join("packs")).map_err(|source| {
            PocError::io(
                "create evacuation packs directory",
                root.join("packs"),
                source,
            )
        })?;
        std::fs::create_dir_all(root.join("pins")).map_err(|source| {
            PocError::io(
                "create evacuation pins directory",
                root.join("pins"),
                source,
            )
        })?;
        create_lock_file(&root.join("LOCK"))?;
        fsync_dir(&root)?;
        let canonical_root = std::fs::canonicalize(&root)
            .map_err(|source| PocError::io("canonicalize evacuation root", &root, source))?;
        Ok(Self {
            root: canonical_root,
        })
    }

    #[must_use]
    pub fn pack_path(&self, operation_id: &OperationId) -> PathBuf {
        self.root
            .join("packs")
            .join(operation_id.as_str())
            .join("payload.pack")
    }

    pub fn prepare(&self, request: &EvacuationRequest) -> PocResult<EvacuationSnapshot> {
        validate_request(request, &self.root)?;
        let _lock = FileLock::exclusive(&self.lock_path())?;
        let operation_dir = self.prepare_operation(request.operation_id.as_str())?;
        let state_path = operation_dir.join("STATE.json");
        let request_sha256 = digest_json(request)?;
        if state_path.exists() {
            let record = read_record(&state_path, &self.root)?;
            if record.request_sha256 != request_sha256 {
                return Err(PocError::Integrity(
                    "stable evacuation operation ID was reused for another request".to_owned(),
                ));
            }
            return self.snapshot_locked(&record);
        }
        let mut record = DurableEvacuationRecord {
            schema_version: SCHEMA_VERSION,
            format: EVACUATION_FORMAT.to_owned(),
            request_sha256,
            request: request.clone(),
            phase: EvacuationPhase::Building,
            payload_sha256: None,
            target_logical_bytes: 0,
            target_allocated_bytes: 0,
            honest_old_plus_new_peak_bytes: request.source_allocated_bytes,
            locator_receipt: None,
            retirement_debt_bytes: 0,
            state_sha256: String::new(),
        };
        persist_record(&state_path, &mut record)?;
        self.snapshot_locked(&record)
    }

    pub fn build_pack(&self, operation_id: &OperationId) -> PocResult<EvacuationSnapshot> {
        validate_path_component(operation_id.as_str(), "operation ID")?;
        let _lock = FileLock::exclusive(&self.lock_path())?;
        let state_path = self.state_path(operation_id);
        let mut record = read_record(&state_path, &self.root)?;
        if record.phase >= EvacuationPhase::Ready {
            return self.snapshot_locked(&record);
        }
        let source_metadata = record
            .request
            .source_payload_path
            .metadata()
            .map_err(|source| {
                PocError::io(
                    "stat evacuation source payload",
                    &record.request.source_payload_path,
                    source,
                )
            })?;
        if !source_metadata.is_file()
            || source_metadata.len() != record.request.source_logical_bytes
            || allocated_bytes(&source_metadata)? != record.request.source_allocated_bytes
        {
            return Err(PocError::RecoveryRequired(
                "stationary evacuation source size changed before pack build".to_owned(),
            ));
        }
        let target_parent = record
            .request
            .target_payload_path
            .parent()
            .ok_or_else(|| PocError::Integrity("evacuation target has no parent".to_owned()))?;
        std::fs::create_dir_all(target_parent).map_err(|source| {
            PocError::io("create evacuation pack directory", target_parent, source)
        })?;
        if record.request.target_payload_path.exists() {
            std::fs::remove_file(&record.request.target_payload_path).map_err(|source| {
                PocError::io(
                    "remove incomplete operation-private evacuation pack",
                    &record.request.target_payload_path,
                    source,
                )
            })?;
            fsync_dir(target_parent)?;
        }
        let mut source = File::open(&record.request.source_payload_path).map_err(|error| {
            PocError::io(
                "open evacuation source payload",
                &record.request.source_payload_path,
                error,
            )
        })?;
        let mut target = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&record.request.target_payload_path)
            .map_err(|error| {
                PocError::io(
                    "create operation-private evacuation pack",
                    &record.request.target_payload_path,
                    error,
                )
            })?;
        let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
        let mut hasher = Sha256::new();
        let mut copied = 0_u64;
        loop {
            let read = source.read(&mut buffer).map_err(|error| {
                PocError::io(
                    "read evacuation source payload",
                    &record.request.source_payload_path,
                    error,
                )
            })?;
            if read == 0 {
                break;
            }
            target.write_all(&buffer[..read]).map_err(|error| {
                PocError::io(
                    "write operation-private evacuation pack",
                    &record.request.target_payload_path,
                    error,
                )
            })?;
            hasher.update(&buffer[..read]);
            copied = copied
                .checked_add(u64::try_from(read).map_err(|_| {
                    PocError::Integrity("evacuation read length does not fit in u64".to_owned())
                })?)
                .ok_or_else(|| PocError::Integrity("evacuation byte count overflow".to_owned()))?;
        }
        target.sync_all().map_err(|error| {
            PocError::io(
                "fsync operation-private evacuation pack",
                &record.request.target_payload_path,
                error,
            )
        })?;
        drop(target);
        fsync_dir(target_parent)?;
        let target_metadata = record
            .request
            .target_payload_path
            .metadata()
            .map_err(|source| {
                PocError::io(
                    "stat durable evacuation pack",
                    &record.request.target_payload_path,
                    source,
                )
            })?;
        if copied != record.request.source_logical_bytes || target_metadata.len() != copied {
            return Err(PocError::RecoveryRequired(
                "evacuation pack is not a complete source copy".to_owned(),
            ));
        }
        record.payload_sha256 = Some(format!("{:x}", hasher.finalize()));
        record.target_logical_bytes = copied;
        record.target_allocated_bytes = allocated_bytes(&target_metadata)?;
        record.honest_old_plus_new_peak_bytes = record
            .request
            .source_allocated_bytes
            .checked_add(record.target_allocated_bytes)
            .ok_or_else(|| PocError::Integrity("evacuation physical peak overflow".to_owned()))?;
        record.phase = EvacuationPhase::Ready;
        persist_record(&state_path, &mut record)?;
        self.snapshot_locked(&record)
    }

    pub fn replace_locator(
        &self,
        operation_id: &OperationId,
        locator_store: &LocatorStore,
        replacement: &LocatorReplacement,
        faults: &mut NamedFaultInjector,
    ) -> PocResult<EvacuationSnapshot> {
        validate_path_component(operation_id.as_str(), "operation ID")?;
        let _lock = FileLock::exclusive(&self.lock_path())?;
        let state_path = self.state_path(operation_id);
        let mut record = read_record(&state_path, &self.root)?;
        validate_locator_replacement(&record, replacement)?;
        if let Some(existing) = record.locator_receipt.as_ref() {
            if existing.operation_id != replacement.operation_id
                || existing.publication_id != replacement.publication_id
                || existing.payload_root != replacement.payload_root
                || existing.source_allocation_id != replacement.expected_source_allocation_id
                || existing.source_owner_epoch != replacement.expected_source_owner_epoch
                || existing.target_allocation_id != replacement.target.allocation_id
                || existing.target_owner_epoch != replacement.target.owner_epoch
            {
                return Err(PocError::RecoveryRequired(
                    "evacuation already recorded another locator replacement".to_owned(),
                ));
            }
            locator_store.validate_receipt(&existing.locator_durability)?;
            return self.snapshot_locked(&record);
        }
        if record.phase != EvacuationPhase::Ready {
            return Err(PocError::RecoveryRequired(
                "evacuation pack is not ready for locator replacement".to_owned(),
            ));
        }
        let durability = locator_store.replace_exact(replacement, faults)?;
        let receipt = EvacuationLocatorReceipt {
            schema_version: SCHEMA_VERSION,
            operation_id: replacement.operation_id.clone(),
            publication_id: replacement.publication_id.clone(),
            payload_root: replacement.payload_root.clone(),
            source_allocation_id: replacement.expected_source_allocation_id.clone(),
            source_owner_epoch: replacement.expected_source_owner_epoch,
            source_generation: replacement.expected_parent,
            target_allocation_id: replacement.target.allocation_id.clone(),
            target_owner_epoch: replacement.target.owner_epoch,
            target_generation: durability.generation,
            locator_durability: durability,
            forward_replacement_complete: true,
            reverse_replacement_complete: true,
        };
        validate_locator_receipt(&record, &receipt)?;
        record.locator_receipt = Some(receipt);
        record.phase = EvacuationPhase::LocatorPublished;
        record.retirement_debt_bytes = record.request.source_allocated_bytes;
        persist_record(&state_path, &mut record)?;
        self.snapshot_locked(&record)
    }

    pub fn pin_selected(&self, operation_id: &OperationId) -> PocResult<PinnedEvacuationReader> {
        validate_path_component(operation_id.as_str(), "operation ID")?;
        let _lock = FileLock::exclusive(&self.lock_path())?;
        let record = read_record(&self.state_path(operation_id), &self.root)?;
        if record.phase >= EvacuationPhase::LocatorPublished {
            let receipt = record.locator_receipt.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "evacuation selected phase lacks locator receipt".to_owned(),
                )
            })?;
            self.pin_locked(
                &record,
                receipt.target_generation,
                &record.request.target_allocation_id,
                record.request.target_owner_epoch,
                &record.request.target_payload_path,
            )
        } else {
            self.pin_locked(
                &record,
                record.request.source_generation,
                &record.request.source_allocation_id,
                record.request.source_owner_epoch,
                &record.request.source_payload_path,
            )
        }
    }

    pub fn retire_source(
        &self,
        operation_id: &OperationId,
        authorization: &StageFiveRetirementAuthorization,
    ) -> PocResult<EvacuationSnapshot> {
        validate_path_component(operation_id.as_str(), "operation ID")?;
        let _lock = FileLock::exclusive(&self.lock_path())?;
        let state_path = self.state_path(operation_id);
        let mut record = read_record(&state_path, &self.root)?;
        validate_retirement_authorization(&record, authorization)?;
        if record.phase == EvacuationPhase::Terminal {
            return self.snapshot_locked(&record);
        }
        if record.phase != EvacuationPhase::LocatorPublished {
            return Err(PocError::RecoveryRequired(
                "source retirement requires a durable locator replacement".to_owned(),
            ));
        }
        let active_pins = self.pin_count_locked(
            &record.request.operation_id,
            record.request.source_generation,
        )?;
        if active_pins != 0 {
            return Err(PocError::RecoveryRequired(format!(
                "source generation {} has {active_pins} active reader pins",
                record.request.source_generation
            )));
        }
        if record.request.source_payload_path.exists() {
            std::fs::remove_file(&record.request.source_payload_path).map_err(|source| {
                PocError::io(
                    "retire authorized evacuation source",
                    &record.request.source_payload_path,
                    source,
                )
            })?;
            let parent =
                record.request.source_payload_path.parent().ok_or_else(|| {
                    PocError::Integrity("evacuation source has no parent".to_owned())
                })?;
            fsync_dir(parent)?;
        }
        record.phase = EvacuationPhase::Terminal;
        record.retirement_debt_bytes = 0;
        persist_record(&state_path, &mut record)?;
        self.snapshot_locked(&record)
    }

    pub fn inspect(&self, operation_id: &OperationId) -> PocResult<EvacuationSnapshot> {
        validate_path_component(operation_id.as_str(), "operation ID")?;
        let _lock = FileLock::shared(&self.lock_path())?;
        let record = read_record(&self.state_path(operation_id), &self.root)?;
        self.snapshot_locked(&record)
    }

    fn pin_locked(
        &self,
        record: &DurableEvacuationRecord,
        generation: LocatorGeneration,
        allocation_id: &AllocationId,
        owner_epoch: u64,
        payload_path: &Path,
    ) -> PocResult<PinnedEvacuationReader> {
        let file = File::open(payload_path).map_err(|source| {
            PocError::io("open pinned evacuation payload", payload_path, source)
        })?;
        let token = OperationId::new();
        let pin_parent = self
            .root
            .join("pins")
            .join(record.request.operation_id.as_str())
            .join(generation.to_string());
        std::fs::create_dir_all(&pin_parent).map_err(|source| {
            PocError::io(
                "create evacuation reader pin directory",
                &pin_parent,
                source,
            )
        })?;
        let pin_path = pin_parent.join(format!("{}.json", token.as_str()));
        let mut pin = DurableReaderPin {
            schema_version: SCHEMA_VERSION,
            format: READER_PIN_FORMAT.to_owned(),
            token,
            evacuation_operation_id: record.request.operation_id.clone(),
            generation,
            allocation_id: allocation_id.clone(),
            owner_epoch,
            created_unix_ms: unix_time_ms()?,
            checksum_sha256: String::new(),
        };
        pin.checksum_sha256 = reader_pin_digest(&pin)?;
        write_immutable_json(&pin_path, &pin)?;
        Ok(PinnedEvacuationReader {
            file,
            pin_path,
            pin_parent,
            generation,
            allocation_id: allocation_id.clone(),
            owner_epoch,
        })
    }

    fn pin_count_locked(
        &self,
        operation_id: &OperationId,
        generation: LocatorGeneration,
    ) -> PocResult<u64> {
        let pin_dir = self
            .root
            .join("pins")
            .join(operation_id.as_str())
            .join(generation.to_string());
        let Ok(entries) = std::fs::read_dir(&pin_dir) else {
            return Ok(0);
        };
        let mut count = 0_u64;
        for entry in entries {
            let entry = entry.map_err(|source| {
                PocError::io("read evacuation pin directory entry", &pin_dir, source)
            })?;
            if !entry
                .file_type()
                .map_err(|source| PocError::io("stat evacuation reader pin", entry.path(), source))?
                .is_file()
            {
                continue;
            }
            let pin: DurableReaderPin = read_json(&entry.path())?;
            validate_reader_pin(&pin, operation_id, generation)?;
            count = count
                .checked_add(1)
                .ok_or_else(|| PocError::Integrity("reader pin count overflow".to_owned()))?;
        }
        Ok(count)
    }

    fn snapshot_locked(&self, record: &DurableEvacuationRecord) -> PocResult<EvacuationSnapshot> {
        let active_reader_pins = self.pin_count_locked(
            &record.request.operation_id,
            record.request.source_generation,
        )?;
        Ok(EvacuationSnapshot {
            schema_version: record.schema_version,
            operation_id: record.request.operation_id.clone(),
            publication_id: record.request.publication_id.clone(),
            payload_root: record.request.payload_root.clone(),
            phase: record.phase,
            request_sha256: record.request_sha256.clone(),
            payload_sha256: record.payload_sha256.clone(),
            source_logical_bytes: record.request.source_logical_bytes,
            source_allocated_bytes: record.request.source_allocated_bytes,
            target_logical_bytes: record.target_logical_bytes,
            target_allocated_bytes: record.target_allocated_bytes,
            honest_old_plus_new_peak_bytes: record.honest_old_plus_new_peak_bytes,
            active_reader_pins,
            retirement_debt_objects: u64::from(record.retirement_debt_bytes != 0),
            retirement_debt_bytes: record.retirement_debt_bytes,
            selected_generation: record
                .locator_receipt
                .as_ref()
                .map_or(record.request.source_generation, |receipt| {
                    receipt.target_generation
                }),
            source_present: record.request.source_payload_path.exists(),
            target_present: record.request.target_payload_path.exists(),
        })
    }

    fn prepare_operation(&self, operation_id: &str) -> PocResult<PathBuf> {
        validate_path_component(operation_id, "operation ID")?;
        let operation_dir = self.root.join("operations").join(operation_id);
        std::fs::create_dir_all(&operation_dir).map_err(|source| {
            PocError::io(
                "create evacuation operation directory",
                &operation_dir,
                source,
            )
        })?;
        fsync_dir(&self.root.join("operations"))?;
        Ok(operation_dir)
    }

    fn state_path(&self, operation_id: &OperationId) -> PathBuf {
        self.root
            .join("operations")
            .join(operation_id.as_str())
            .join("STATE.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join("LOCK")
    }
}

impl PinnedEvacuationReader {
    #[must_use]
    pub const fn generation(&self) -> LocatorGeneration {
        self.generation
    }

    #[must_use]
    pub const fn allocation_id(&self) -> &AllocationId {
        &self.allocation_id
    }

    #[must_use]
    pub const fn owner_epoch(&self) -> u64 {
        self.owner_epoch
    }
}

impl Read for PinnedEvacuationReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buffer)
    }
}

impl Seek for PinnedEvacuationReader {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}

impl Drop for PinnedEvacuationReader {
    fn drop(&mut self) {
        if std::fs::remove_file(&self.pin_path).is_ok() {
            let _ = fsync_dir(&self.pin_parent);
        }
    }
}

fn validate_request(request: &EvacuationRequest, root: &Path) -> PocResult<()> {
    if request.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(
            "unsupported evacuation request".to_owned(),
        ));
    }
    validate_path_component(request.operation_id.as_str(), "operation ID")?;
    if request.source_owner_epoch == 0
        || request.target_owner_epoch == 0
        || request.source_logical_bytes == 0
        || request.source_allocated_bytes == 0
        || request.source_allocation_id == request.target_allocation_id
        || request.source_payload_path == request.target_payload_path
    {
        return Err(PocError::Integrity(
            "evacuation identities, epochs, or source accounting are invalid".to_owned(),
        ));
    }
    let expected_target = root
        .join("packs")
        .join(request.operation_id.as_str())
        .join("payload.pack");
    if request.target_payload_path != expected_target {
        return Err(PocError::Integrity(
            "evacuation target is not the exact operation-private pack path".to_owned(),
        ));
    }
    Ok(())
}

fn validate_locator_receipt(
    record: &DurableEvacuationRecord,
    receipt: &EvacuationLocatorReceipt,
) -> PocResult<()> {
    let request = &record.request;
    if receipt.schema_version != SCHEMA_VERSION
        || receipt.operation_id != request.operation_id
        || receipt.publication_id != request.publication_id
        || receipt.payload_root != request.payload_root
        || receipt.source_allocation_id != request.source_allocation_id
        || receipt.source_owner_epoch != request.source_owner_epoch
        || receipt.source_generation != request.source_generation
        || receipt.target_allocation_id != request.target_allocation_id
        || receipt.target_owner_epoch != request.target_owner_epoch
        || receipt.target_generation <= receipt.source_generation
        || receipt.locator_durability.generation != receipt.target_generation
        || !receipt.locator_durability.forward_durable
        || !receipt.locator_durability.reverse_durable
        || !receipt.locator_durability.manifest_durable
        || !receipt.locator_durability.selector_parent_synced
        || !receipt.forward_replacement_complete
        || !receipt.reverse_replacement_complete
    {
        return Err(PocError::RecoveryRequired(
            "locator replacement receipt is incomplete or not exact".to_owned(),
        ));
    }
    Ok(())
}

fn validate_locator_replacement(
    record: &DurableEvacuationRecord,
    replacement: &LocatorReplacement,
) -> PocResult<()> {
    let request = &record.request;
    let target_extent_bytes = replacement
        .target
        .extents
        .iter()
        .try_fold(0_u64, |total, extent| total.checked_add(extent.length))
        .ok_or_else(|| PocError::Integrity("locator replacement extent overflow".to_owned()))?;
    if replacement.schema_version != SCHEMA_VERSION
        || replacement.operation_id != request.operation_id
        || replacement.publication_id != request.publication_id
        || replacement.expected_parent != request.source_generation
        || replacement.payload_root != request.payload_root
        || replacement.expected_source_allocation_id != request.source_allocation_id
        || replacement.expected_source_owner_epoch != request.source_owner_epoch
        || replacement.target.payload_root != request.payload_root
        || replacement.target.allocation_id != request.target_allocation_id
        || replacement.target.owner_epoch != request.target_owner_epoch
        || replacement.target_reverse.allocation_id != request.target_allocation_id
        || replacement.target_reverse.owner_epoch != request.target_owner_epoch
        || replacement.target_reverse.operation_id != request.operation_id
        || replacement.target_reverse.publication_id != request.publication_id
        || replacement.target_reverse.payload_roots != [request.payload_root.clone()]
        || replacement.target_reverse.accounted_bytes != record.target_allocated_bytes
        || target_extent_bytes != record.target_logical_bytes
    {
        return Err(PocError::RecoveryRequired(
            "locator replacement does not exactly describe the durable evacuation pack".to_owned(),
        ));
    }
    Ok(())
}

fn validate_retirement_authorization(
    record: &DurableEvacuationRecord,
    authorization: &StageFiveRetirementAuthorization,
) -> PocResult<()> {
    let receipt = record.locator_receipt.as_ref().ok_or_else(|| {
        PocError::RecoveryRequired(
            "retirement authorization has no locator replacement receipt".to_owned(),
        )
    })?;
    if authorization.schema_version != SCHEMA_VERSION
        || !authorization.deletion_authorized
        || authorization.evacuation_operation_id != record.request.operation_id
        || authorization.payload_root != record.request.payload_root
        || authorization.source_allocation_id != record.request.source_allocation_id
        || authorization.source_owner_epoch != record.request.source_owner_epoch
        || authorization.selected_generation != receipt.target_generation
    {
        return Err(PocError::RecoveryRequired(
            "Stage Five retirement authorization is absent or not exact".to_owned(),
        ));
    }
    Ok(())
}

fn persist_record(path: &Path, record: &mut DurableEvacuationRecord) -> PocResult<()> {
    record.state_sha256.clear();
    record.state_sha256 = digest_json(record)?;
    replace_json(path, record)
}

fn read_record(path: &Path, root: &Path) -> PocResult<DurableEvacuationRecord> {
    let record: DurableEvacuationRecord = read_json(path)?;
    if record.schema_version != SCHEMA_VERSION || record.format != EVACUATION_FORMAT {
        return Err(PocError::Integrity(
            "unsupported evacuation record".to_owned(),
        ));
    }
    let mut expected = record.clone();
    let observed = expected.state_sha256.clone();
    expected.state_sha256.clear();
    if digest_json(&expected)? != observed || digest_json(&record.request)? != record.request_sha256
    {
        return Err(PocError::RecoveryRequired(
            "evacuation record checksum mismatch".to_owned(),
        ));
    }
    validate_request(&record.request, root)?;
    if record.phase >= EvacuationPhase::Ready
        && (record.payload_sha256.as_ref().is_none_or(|digest| {
            digest.len() != 64
                || !digest
                    .as_bytes()
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        }) || record.target_logical_bytes != record.request.source_logical_bytes)
    {
        return Err(PocError::RecoveryRequired(
            "ready evacuation record lacks a complete pack receipt".to_owned(),
        ));
    }
    if record.phase >= EvacuationPhase::LocatorPublished {
        let receipt = record.locator_receipt.as_ref().ok_or_else(|| {
            PocError::RecoveryRequired(
                "published evacuation record lacks a locator receipt".to_owned(),
            )
        })?;
        validate_locator_receipt(&record, receipt)?;
    }
    Ok(record)
}

fn validate_reader_pin(
    pin: &DurableReaderPin,
    operation_id: &OperationId,
    generation: LocatorGeneration,
) -> PocResult<()> {
    if pin.schema_version != SCHEMA_VERSION
        || pin.format != READER_PIN_FORMAT
        || pin.evacuation_operation_id != *operation_id
        || pin.generation != generation
        || pin.owner_epoch == 0
        || reader_pin_digest(pin)? != pin.checksum_sha256
    {
        return Err(PocError::RecoveryRequired(
            "evacuation reader pin is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn reader_pin_digest(pin: &DurableReaderPin) -> PocResult<String> {
    let mut expected = pin.clone();
    expected.checksum_sha256.clear();
    digest_json(&expected)
}

fn allocated_bytes(metadata: &std::fs::Metadata) -> PocResult<u64> {
    metadata
        .blocks()
        .checked_mul(512)
        .ok_or_else(|| PocError::Integrity("allocated-byte count overflow".to_owned()))
}

fn create_lock_file(path: &Path) -> PocResult<()> {
    File::options()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| PocError::io("create evacuation lock", path, source))
}

fn validate_path_component(value: &str, label: &str) -> PocResult<()> {
    let valid = !value.is_empty()
        && value.len() <= 255
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        return Err(PocError::Integrity(format!(
            "{label} is not a safe path component"
        )));
    }
    Ok(())
}

fn digest_json<T: Serialize>(value: &T) -> PocResult<String> {
    let bytes = serde_json::to_vec(value)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
}
