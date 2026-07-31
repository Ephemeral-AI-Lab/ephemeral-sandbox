use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{fsync_dir, FileLock};
use crate::locator::LocatorStore;
use crate::recovery::reach_real_operation;
use crate::{
    CanonicalDurabilityReceipt, LocatorDurabilityReceipt, LocatorRefCandidate, NamedFaultInjector,
    NamedFaultPoint, PairedRefValue, PocError, PocResult, RefSequence, SCHEMA_VERSION,
};

const REF_FORMAT: &str = "mpla-poc-paired-ref-v1";
const JOURNAL_FORMAT: &str = "mpla-poc-paired-ref-journal-v1";
const JOURNAL_MAGIC: [u8; 4] = *b"MPRJ";
const JOURNAL_FRAME_VERSION: u32 = 1;
const JOURNAL_HEADER_BYTES: usize = 16;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JOURNAL_RECORD_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
enum TerminalResponse {
    Publish,
    Rollback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefCommitOutcome {
    Committed(RefCommitReceipt),
    ExpectedParent {
        expected: RefSequence,
        observed: RefSequence,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefCommitReceipt {
    pub value: PairedRefValue,
    pub idempotent_replay: bool,
    pub parent_directory_synced: bool,
    pub outcome_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPairedRef {
    pub value: PairedRefValue,
    pub canonical: CanonicalDurabilityReceipt,
    pub locator: LocatorDurabilityReceipt,
}

#[derive(Clone, Debug)]
pub struct PairedRefStore {
    root: PathBuf,
    cache: Arc<Mutex<JournalCache>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RefPrerequisiteRecord {
    schema_version: u32,
    format: String,
    candidate: LocatorRefCandidate,
    candidate_sha256: String,
    canonical: CanonicalDurabilityReceipt,
    locator: LocatorDurabilityReceipt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RefOutcomeRecord {
    schema_version: u32,
    format: String,
    candidate_sha256: String,
    value: PairedRefValue,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RefJournalRecord {
    schema_version: u32,
    format: String,
    sequence: u64,
    branch: String,
    prerequisite: RefPrerequisiteRecord,
    outcome: RefOutcomeRecord,
    previous_record_hash: Option<String>,
    record_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

#[derive(Debug, Default)]
struct JournalState {
    records: Vec<RefJournalRecord>,
    heads: BTreeMap<String, usize>,
    operations: BTreeMap<(String, String), usize>,
    valid_bytes: u64,
    torn_tail: bool,
}

#[derive(Debug, Default)]
struct JournalCache {
    stamp: Option<FileStamp>,
    state: JournalState,
}

static JOURNAL_CACHES: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<JournalCache>>>>> =
    OnceLock::new();

impl PairedRefStore {
    pub fn open(root: impl Into<PathBuf>) -> PocResult<Self> {
        let mut root = root.into();
        let journal_path = root.join("JOURNAL");
        let lock_path = root.join("LOCK");
        let layout_ready = root.is_dir() && journal_path.is_file() && lock_path.is_file();
        if !layout_ready {
            std::fs::create_dir_all(&root)
                .map_err(|source| PocError::io("create paired ref root", &root, source))?;
            create_append_file(&journal_path, "create paired ref journal")?;
            create_append_file(&lock_path, "create paired ref lock")?;
            fsync_dir(&root)?;
        }
        if !root.is_absolute() {
            root = std::fs::canonicalize(&root)
                .map_err(|source| PocError::io("canonicalize paired ref root", &root, source))?;
        }
        let cache = {
            let mut caches = JOURNAL_CACHES
                .get_or_init(|| Mutex::new(BTreeMap::new()))
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            caches
                .entry(root.clone())
                .or_insert_with(|| Arc::new(Mutex::new(JournalCache::default())))
                .clone()
        };
        let store = Self { root, cache };
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn read(&self, branch: &str) -> PocResult<Option<PairedRefValue>> {
        validate_path_component(branch, "branch")?;
        let _lock = FileLock::shared(&self.lock_path())?;
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        refresh_cache(&self.journal_path(), &mut cache)?;
        Ok(cache
            .state
            .heads
            .get(branch)
            .map(|index| cache.state.records[*index].outcome.value.clone()))
    }

    pub fn read_resolved(
        &self,
        branch: &str,
        locator_store: &LocatorStore,
    ) -> PocResult<Option<ResolvedPairedRef>> {
        validate_path_component(branch, "branch")?;
        let _lock = FileLock::shared(&self.lock_path())?;
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        refresh_cache(&self.journal_path(), &mut cache)?;
        let Some(index) = cache.state.heads.get(branch).copied() else {
            return Ok(None);
        };
        let record = &cache.state.records[index];
        validate_prerequisite(
            &record.prerequisite,
            &record.prerequisite.candidate,
            locator_store,
            false,
        )?;
        validate_record_pair(record)?;
        Ok(Some(ResolvedPairedRef {
            value: record.outcome.value.clone(),
            canonical: record.prerequisite.canonical.clone(),
            locator: record.prerequisite.locator.clone(),
        }))
    }

    pub fn commit(
        &self,
        branch: &str,
        candidate: &LocatorRefCandidate,
        canonical: &CanonicalDurabilityReceipt,
        locator: &LocatorDurabilityReceipt,
        locator_store: &LocatorStore,
        faults: &mut NamedFaultInjector,
    ) -> PocResult<RefCommitOutcome> {
        self.commit_with_response(
            branch,
            candidate,
            canonical,
            locator,
            locator_store,
            faults,
            TerminalResponse::Publish,
        )
    }

    pub fn commit_rollback(
        &self,
        branch: &str,
        candidate: &LocatorRefCandidate,
        canonical: &CanonicalDurabilityReceipt,
        locator: &LocatorDurabilityReceipt,
        locator_store: &LocatorStore,
        faults: &mut NamedFaultInjector,
    ) -> PocResult<RefCommitOutcome> {
        self.commit_with_response(
            branch,
            candidate,
            canonical,
            locator,
            locator_store,
            faults,
            TerminalResponse::Rollback,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_with_response(
        &self,
        branch: &str,
        candidate: &LocatorRefCandidate,
        canonical: &CanonicalDurabilityReceipt,
        locator: &LocatorDurabilityReceipt,
        locator_store: &LocatorStore,
        faults: &mut NamedFaultInjector,
        terminal_response: TerminalResponse,
    ) -> PocResult<RefCommitOutcome> {
        validate_path_component(branch, "branch")?;
        validate_candidate(candidate)?;
        validate_canonical(canonical)?;
        if locator.generation != candidate.locator_generation {
            return Err(PocError::Integrity(format!(
                "candidate locator generation {} does not match durability receipt {}",
                candidate.locator_generation, locator.generation
            )));
        }

        let _lock = FileLock::exclusive(&self.lock_path())?;
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        refresh_cache(&self.journal_path(), &mut cache)?;
        repair_torn_tail(&self.journal_path(), &mut cache)?;
        let operation_key = (
            branch.to_owned(),
            candidate.operation_id.as_str().to_owned(),
        );
        if let Some(index) = cache.state.operations.get(&operation_key).copied() {
            let record = &cache.state.records[index];
            validate_prerequisite(&record.prerequisite, candidate, locator_store, false)?;
            validate_outcome(
                &record.outcome,
                candidate,
                &record.prerequisite.candidate_sha256,
            )?;
            let current = cache
                .state
                .heads
                .get(branch)
                .map(|head| &cache.state.records[*head].outcome.value)
                .ok_or_else(|| {
                    PocError::RecoveryRequired(
                        "stored paired ref outcome has no durable branch head".to_owned(),
                    )
                })?;
            if current != &record.outcome.value {
                return Err(PocError::RecoveryRequired(
                    "stored paired ref outcome disagrees with durable branch head".to_owned(),
                ));
            }
            return Ok(RefCommitOutcome::Committed(RefCommitReceipt {
                value: record.outcome.value.clone(),
                idempotent_replay: true,
                parent_directory_synced: true,
                outcome_path: self.journal_path(),
            }));
        }

        let current = cache
            .state
            .heads
            .get(branch)
            .map(|index| &cache.state.records[*index].outcome.value);
        if let Some(current) = current {
            if current.sequence != candidate.expected_sequence {
                return Ok(RefCommitOutcome::ExpectedParent {
                    expected: candidate.expected_sequence,
                    observed: current.sequence,
                });
            }
        } else if candidate.expected_sequence != RefSequence::ZERO {
            return Ok(RefCommitOutcome::ExpectedParent {
                expected: candidate.expected_sequence,
                observed: RefSequence::ZERO,
            });
        }

        locator_store.validate_receipt(locator)?;
        let candidate_sha256 = digest_json(candidate)?;
        let prerequisite = RefPrerequisiteRecord {
            schema_version: SCHEMA_VERSION,
            format: REF_FORMAT.to_owned(),
            candidate: candidate.clone(),
            candidate_sha256: candidate_sha256.clone(),
            canonical: canonical.clone(),
            locator: locator.clone(),
        };
        let mut value = PairedRefValue {
            schema_version: SCHEMA_VERSION,
            operation_id: candidate.operation_id.clone(),
            publication_id: candidate.publication_id.clone(),
            roots: candidate.roots.clone(),
            locator_generation: candidate.locator_generation,
            sequence: candidate.expected_sequence.checked_next()?,
            checksum_sha256: String::new(),
        };
        value.checksum_sha256 = paired_ref_checksum(&value)?;
        let outcome = RefOutcomeRecord {
            schema_version: SCHEMA_VERSION,
            format: REF_FORMAT.to_owned(),
            candidate_sha256,
            value: value.clone(),
        };
        let sequence = u64::try_from(cache.state.records.len())
            .map_err(|_| PocError::Integrity("paired ref journal sequence overflow".to_owned()))?
            .checked_add(1)
            .ok_or_else(|| {
                PocError::Integrity("paired ref journal sequence overflow".to_owned())
            })?;
        let mut record = RefJournalRecord {
            schema_version: SCHEMA_VERSION,
            format: JOURNAL_FORMAT.to_owned(),
            sequence,
            branch: branch.to_owned(),
            prerequisite,
            outcome,
            previous_record_hash: cache
                .state
                .records
                .last()
                .map(|record| record.record_hash.clone()),
            record_hash: String::new(),
        };
        record.record_hash = journal_record_hash(&record)?;
        reach_real_operation(
            faults,
            NamedFaultPoint::RefBeforeTemp,
            &candidate.operation_id,
            [self.journal_path()],
            None,
            true,
        )?;
        let stamp = append_record(&self.journal_path(), &record)?;
        apply_record(&mut cache.state, record);
        cache.stamp = Some(stamp);
        for point in [
            NamedFaultPoint::RefAfterTempFsync,
            NamedFaultPoint::RefAfterReplace,
            NamedFaultPoint::RefAfterParentFsync,
        ] {
            reach_real_operation(
                faults,
                point,
                &candidate.operation_id,
                [self.journal_path()],
                None,
                true,
            )?;
        }
        let response_point = match terminal_response {
            TerminalResponse::Publish => NamedFaultPoint::ResponseLossPublish,
            TerminalResponse::Rollback => NamedFaultPoint::ResponseLossRollback,
        };
        reach_real_operation(
            faults,
            response_point,
            &candidate.operation_id,
            [self.journal_path()],
            None,
            true,
        )?;
        Ok(RefCommitOutcome::Committed(RefCommitReceipt {
            value,
            idempotent_replay: false,
            parent_directory_synced: true,
            outcome_path: self.journal_path(),
        }))
    }

    pub fn recover_committed(
        &self,
        branch: &str,
        operation_id: &str,
        locator_store: &LocatorStore,
    ) -> PocResult<Option<RefCommitReceipt>> {
        validate_path_component(branch, "branch")?;
        validate_path_component(operation_id, "operation ID")?;
        let _lock = FileLock::exclusive(&self.lock_path())?;
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        refresh_cache(&self.journal_path(), &mut cache)?;
        repair_torn_tail(&self.journal_path(), &mut cache)?;
        let Some(index) = cache.state.heads.get(branch).copied() else {
            return Ok(None);
        };
        let record = &cache.state.records[index];
        if record.outcome.value.operation_id.as_str() != operation_id {
            return Ok(None);
        }
        validate_prerequisite(
            &record.prerequisite,
            &record.prerequisite.candidate,
            locator_store,
            false,
        )?;
        validate_outcome(
            &record.outcome,
            &record.prerequisite.candidate,
            &record.prerequisite.candidate_sha256,
        )?;
        Ok(Some(RefCommitReceipt {
            value: record.outcome.value.clone(),
            idempotent_replay: true,
            parent_directory_synced: true,
            outcome_path: self.journal_path(),
        }))
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join("JOURNAL")
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join("LOCK")
    }
}

fn refresh_cache(path: &Path, cache: &mut JournalCache) -> PocResult<()> {
    let stamp = file_stamp(path)?;
    if cache.stamp == Some(stamp) {
        return Ok(());
    }
    cache.state = read_journal(path)?;
    cache.stamp = Some(stamp);
    Ok(())
}

fn repair_torn_tail(path: &Path, cache: &mut JournalCache) -> PocResult<()> {
    if !cache.state.torn_tail {
        return Ok(());
    }
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| PocError::io("open paired ref journal for repair", path, source))?;
    file.set_len(cache.state.valid_bytes)
        .map_err(|source| PocError::io("truncate paired ref journal torn tail", path, source))?;
    file.sync_data()
        .map_err(|source| PocError::io("fdatasync repaired paired ref journal", path, source))?;
    cache.state.torn_tail = false;
    cache.stamp = Some(file_stamp(path)?);
    Ok(())
}

fn append_record(path: &Path, record: &RefJournalRecord) -> PocResult<FileStamp> {
    let payload = serde_json::to_vec(record)?;
    if payload.len() > MAX_JOURNAL_RECORD_BYTES {
        return Err(PocError::Integrity(
            "paired ref journal record exceeds framing limit".to_owned(),
        ));
    }
    let length = u64::try_from(payload.len())
        .map_err(|_| PocError::Integrity("paired ref journal record length overflow".to_owned()))?;
    let mut frame = Vec::with_capacity(JOURNAL_HEADER_BYTES + payload.len());
    frame.extend_from_slice(&JOURNAL_MAGIC);
    frame.extend_from_slice(&JOURNAL_FRAME_VERSION.to_le_bytes());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|source| PocError::io("open paired ref journal for append", path, source))?;
    file.write_all(&frame)
        .map_err(|source| PocError::io("append paired ref journal record", path, source))?;
    file.sync_data()
        .map_err(|source| PocError::io("fdatasync paired ref journal", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| PocError::io("stat appended paired ref journal", path, source))?;
    Ok(stamp_from_metadata(&metadata))
}

fn read_journal(path: &Path) -> PocResult<JournalState> {
    let mut file =
        File::open(path).map_err(|source| PocError::io("open paired ref journal", path, source))?;
    let length = file
        .metadata()
        .map_err(|source| PocError::io("stat paired ref journal", path, source))?
        .len();
    if length > MAX_JOURNAL_BYTES {
        return Err(PocError::Integrity(format!(
            "paired ref journal exceeds {MAX_JOURNAL_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(length)
            .map_err(|_| PocError::Integrity("paired ref journal length overflow".to_owned()))?,
    );
    file.read_to_end(&mut bytes)
        .map_err(|source| PocError::io("read paired ref journal", path, source))?;
    let mut state = JournalState::default();
    let mut offset = 0_usize;
    while offset < bytes.len() {
        if bytes.len() - offset < JOURNAL_HEADER_BYTES {
            break;
        }
        if bytes[offset..offset + 4] != JOURNAL_MAGIC {
            return Err(PocError::Integrity(format!(
                "paired ref journal frame magic mismatch at byte {offset}"
            )));
        }
        let version = u32::from_le_bytes(
            bytes[offset + 4..offset + 8]
                .try_into()
                .map_err(|_| PocError::Integrity("paired ref journal version frame".to_owned()))?,
        );
        if version != JOURNAL_FRAME_VERSION {
            return Err(PocError::Integrity(format!(
                "unsupported paired ref journal frame version {version}"
            )));
        }
        let payload_length = u64::from_le_bytes(
            bytes[offset + 8..offset + 16]
                .try_into()
                .map_err(|_| PocError::Integrity("paired ref journal length frame".to_owned()))?,
        );
        let payload_length = usize::try_from(payload_length).map_err(|_| {
            PocError::Integrity("paired ref journal frame length overflow".to_owned())
        })?;
        if payload_length > MAX_JOURNAL_RECORD_BYTES {
            return Err(PocError::Integrity(format!(
                "paired ref journal frame exceeds {MAX_JOURNAL_RECORD_BYTES} bytes"
            )));
        }
        let payload_start = offset + JOURNAL_HEADER_BYTES;
        let Some(payload_end) = payload_start.checked_add(payload_length) else {
            return Err(PocError::Integrity(
                "paired ref journal frame length overflow".to_owned(),
            ));
        };
        if payload_end > bytes.len() {
            break;
        }
        let record: RefJournalRecord = serde_json::from_slice(&bytes[payload_start..payload_end])?;
        validate_journal_record(&record, &state)?;
        apply_record(&mut state, record);
        offset = payload_end;
    }
    state.valid_bytes = u64::try_from(offset)
        .map_err(|_| PocError::Integrity("paired ref journal offset overflow".to_owned()))?;
    state.torn_tail = offset != bytes.len();
    Ok(state)
}

fn validate_journal_record(record: &RefJournalRecord, state: &JournalState) -> PocResult<()> {
    if record.schema_version != SCHEMA_VERSION || record.format != JOURNAL_FORMAT {
        return Err(PocError::Integrity(
            "unsupported paired ref journal record".to_owned(),
        ));
    }
    let expected_sequence = u64::try_from(state.records.len())
        .map_err(|_| PocError::Integrity("paired ref journal sequence overflow".to_owned()))?
        .checked_add(1)
        .ok_or_else(|| PocError::Integrity("paired ref journal sequence overflow".to_owned()))?;
    if record.sequence != expected_sequence
        || record.previous_record_hash
            != state
                .records
                .last()
                .map(|previous| previous.record_hash.clone())
        || record.record_hash != journal_record_hash(record)?
    {
        return Err(PocError::Integrity(
            "paired ref journal chain is invalid".to_owned(),
        ));
    }
    validate_path_component(&record.branch, "branch")?;
    validate_prerequisite_record(&record.prerequisite)?;
    validate_outcome(
        &record.outcome,
        &record.prerequisite.candidate,
        &record.prerequisite.candidate_sha256,
    )?;
    let expected_parent = state
        .heads
        .get(&record.branch)
        .map_or(RefSequence::ZERO, |index| {
            state.records[*index].outcome.value.sequence
        });
    if record.prerequisite.candidate.expected_sequence != expected_parent {
        return Err(PocError::Integrity(
            "paired ref journal expected parent is not the selected branch head".to_owned(),
        ));
    }
    let operation_key = (
        record.branch.clone(),
        record
            .prerequisite
            .candidate
            .operation_id
            .as_str()
            .to_owned(),
    );
    if state.operations.contains_key(&operation_key) {
        return Err(PocError::Integrity(
            "paired ref journal repeats a stable operation ID".to_owned(),
        ));
    }
    Ok(())
}

fn apply_record(state: &mut JournalState, record: RefJournalRecord) {
    let index = state.records.len();
    state.heads.insert(record.branch.clone(), index);
    state.operations.insert(
        (
            record.branch.clone(),
            record
                .prerequisite
                .candidate
                .operation_id
                .as_str()
                .to_owned(),
        ),
        index,
    );
    state.records.push(record);
}

fn validate_record_pair(record: &RefJournalRecord) -> PocResult<()> {
    if record.prerequisite.candidate.roots != record.outcome.value.roots
        || record.prerequisite.candidate.locator_generation
            != record.outcome.value.locator_generation
        || record.prerequisite.candidate.operation_id != record.outcome.value.operation_id
        || record.prerequisite.candidate.publication_id != record.outcome.value.publication_id
    {
        return Err(PocError::RecoveryRequired(
            "selected paired ref disagrees with its durable prerequisites".to_owned(),
        ));
    }
    Ok(())
}

fn validate_candidate(candidate: &LocatorRefCandidate) -> PocResult<()> {
    if candidate.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(format!(
            "unsupported paired ref candidate schema {}",
            candidate.schema_version
        )));
    }
    validate_path_component(candidate.operation_id.as_str(), "operation ID")
}

fn validate_canonical(receipt: &CanonicalDurabilityReceipt) -> PocResult<()> {
    if !receipt.files_fsynced
        || !receipt.object_directory_fsynced
        || !receipt.manifest_fsynced
        || !receipt.manifest_directory_fsynced
    {
        return Err(PocError::Integrity(
            "canonical durability receipt is incomplete".to_owned(),
        ));
    }
    if receipt.object_set_sha256.len() != 64
        || !receipt
            .object_set_sha256
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(PocError::Integrity(
            "canonical object set digest is invalid".to_owned(),
        ));
    }
    let metadata = std::fs::metadata(&receipt.root_manifest).map_err(|source| {
        PocError::io(
            "stat canonical root manifest",
            &receipt.root_manifest,
            source,
        )
    })?;
    if !metadata.is_file() {
        return Err(PocError::Integrity(
            "canonical root manifest is not a regular file".to_owned(),
        ));
    }
    Ok(())
}

fn validate_prerequisite(
    prerequisite: &RefPrerequisiteRecord,
    candidate: &LocatorRefCandidate,
    locator_store: &LocatorStore,
    require_selected: bool,
) -> PocResult<()> {
    validate_prerequisite_record(prerequisite)?;
    if prerequisite.candidate != *candidate {
        return Err(PocError::Integrity(
            "stable operation ID was reused for another paired ref candidate".to_owned(),
        ));
    }
    validate_canonical(&prerequisite.canonical)?;
    if require_selected {
        locator_store.validate_receipt(&prerequisite.locator)
    } else {
        locator_store.validate_generation_receipt(&prerequisite.locator)
    }
}

fn validate_prerequisite_record(prerequisite: &RefPrerequisiteRecord) -> PocResult<()> {
    if prerequisite.schema_version != SCHEMA_VERSION || prerequisite.format != REF_FORMAT {
        return Err(PocError::Integrity(
            "unsupported paired ref prerequisite".to_owned(),
        ));
    }
    validate_candidate(&prerequisite.candidate)?;
    if prerequisite.candidate_sha256 != digest_json(&prerequisite.candidate)? {
        return Err(PocError::Integrity(
            "paired ref prerequisite candidate digest mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn validate_outcome(
    outcome: &RefOutcomeRecord,
    candidate: &LocatorRefCandidate,
    candidate_sha256: &str,
) -> PocResult<()> {
    if outcome.schema_version != SCHEMA_VERSION
        || outcome.format != REF_FORMAT
        || outcome.candidate_sha256 != candidate_sha256
    {
        return Err(PocError::Integrity(
            "stable operation ID was reused after paired ref commit".to_owned(),
        ));
    }
    validate_matching_head(&outcome.value, candidate)
}

fn validate_matching_head(
    current: &PairedRefValue,
    candidate: &LocatorRefCandidate,
) -> PocResult<()> {
    validate_paired_ref(current)?;
    if current.operation_id != candidate.operation_id
        || current.publication_id != candidate.publication_id
        || current.roots != candidate.roots
        || current.locator_generation != candidate.locator_generation
        || current.sequence != candidate.expected_sequence.checked_next()?
    {
        return Err(PocError::Integrity(
            "stable operation ID resolved to a different paired ref".to_owned(),
        ));
    }
    Ok(())
}

fn validate_paired_ref(value: &PairedRefValue) -> PocResult<()> {
    if value.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(format!(
            "unsupported paired ref schema {}",
            value.schema_version
        )));
    }
    let observed = paired_ref_checksum(value)?;
    if observed != value.checksum_sha256 {
        return Err(PocError::RecoveryRequired(
            "paired ref checksum mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn paired_ref_checksum(value: &PairedRefValue) -> PocResult<String> {
    let mut expected = value.clone();
    expected.checksum_sha256.clear();
    digest_json(&expected)
}

fn journal_record_hash(record: &RefJournalRecord) -> PocResult<String> {
    let mut expected = record.clone();
    expected.record_hash.clear();
    digest_json(&expected)
}

fn digest_json<T: Serialize>(value: &T) -> PocResult<String> {
    let bytes = serde_json::to_vec(value)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
}

fn create_append_file(path: &Path, action: &'static str) -> PocResult<()> {
    File::options()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| PocError::io(action, path, source))
}

fn file_stamp(path: &Path) -> PocResult<FileStamp> {
    let metadata = std::fs::metadata(path)
        .map_err(|source| PocError::io("stat paired ref journal", path, source))?;
    Ok(stamp_from_metadata(&metadata))
}

fn stamp_from_metadata(metadata: &std::fs::Metadata) -> FileStamp {
    FileStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
    }
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
