use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{fsync_dir, FileLock};
use crate::locator::{LocatorStore, PayloadRootId, SealedLocatorStore};
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
const CURSOR_MAGIC: [u8; 8] = *b"MPRCURS3";
const CURSOR_VERSION: u32 = 3;
const CURSOR_SLOT_BYTES: u64 = 4096;
const CURSOR_SLOT_COUNT: u64 = 2;
const CURSOR_PREFIX_BYTES: usize = 72;
const CURSOR_DIGEST_END: usize = 104;
const JOURNAL_TOTAL_BYTES: u64 = MAX_JOURNAL_BYTES + CURSOR_SLOT_BYTES * CURSOR_SLOT_COUNT;
const LAYOUT_MARKER_V2: &[u8] =
    b"mpla-poc-paired-ref-layout-v2\njournal-preallocated-bytes=67108864\n";
const LAYOUT_MARKER_V3: &[u8] = b"mpla-poc-paired-ref-layout-v3\njournal-data-bytes=67108864\ncursor-slot-bytes=4096\ncursor-slots=2\njournal-total-bytes=67117056\n";
#[cfg(target_os = "linux")]
const JOURNAL_PREALLOCATED_BYTES: libc::off_t = JOURNAL_TOTAL_BYTES as libc::off_t;

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

#[derive(Clone)]
pub struct SealedPairedRefStore {
    root: PathBuf,
    state: Arc<JournalState>,
    layout: LayoutVersion,
    _lock: Arc<FileLock>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SealedPairedRefLayoutReceipt {
    pub format: String,
    pub journal_data_bytes: u64,
    pub journal_total_bytes: u64,
    pub cursor_generation: u64,
    pub cursor_slot: u64,
    pub logical_end: u64,
    pub record_count: u64,
    pub last_record_hash: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rollback_target_branch: Option<String>,
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
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Debug, Default)]
struct JournalState {
    records: Vec<RefJournalRecord>,
    heads: BTreeMap<String, usize>,
    operations: BTreeMap<(String, String), usize>,
    valid_bytes: u64,
    cursor_generation: u64,
    cursor_slot: usize,
}

#[derive(Debug, Default)]
struct JournalCache {
    stamp: Option<FileStamp>,
    state: JournalState,
}

static JOURNAL_CACHES: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<JournalCache>>>>> =
    OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct JournalCursor {
    generation: u64,
    logical_end: u64,
    record_count: u64,
    last_hash: [u8; 32],
    slot: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LayoutVersion {
    Missing,
    V2,
    V3,
}

impl PairedRefStore {
    pub fn open(root: impl Into<PathBuf>) -> PocResult<Self> {
        let mut root = root.into();
        let journal_path = root.join("JOURNAL");
        let lock_path = root.join("LOCK");
        let layout_path = root.join("LAYOUT");
        if !existing_v3_layout_is_ready(&root, &journal_path, &lock_path, &layout_path)? {
            std::fs::create_dir_all(&root)
                .map_err(|source| PocError::io("create paired ref root", &root, source))?;
            create_append_file(&lock_path, "create paired ref lock")?;
            let _layout_lock = FileLock::exclusive(&lock_path)?;
            ensure_v3_layout(&root, &journal_path, &layout_path)?;
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

    pub fn rollback_to_branch(
        &self,
        branch: &str,
        target_branch: &str,
        operation_id: &crate::OperationId,
        locator_store: &LocatorStore,
        faults: &mut NamedFaultInjector,
    ) -> PocResult<RefCommitReceipt> {
        validate_path_component(branch, "branch")?;
        validate_path_component(target_branch, "target branch")?;
        validate_path_component(operation_id.as_str(), "operation ID")?;

        let _lock = FileLock::exclusive(&self.lock_path())?;
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        refresh_cache(&self.journal_path(), &mut cache)?;

        let operation_key = (branch.to_owned(), operation_id.as_str().to_owned());
        if let Some(index) = cache.state.operations.get(&operation_key).copied() {
            let record = &cache.state.records[index];
            if record.rollback_target_branch.as_deref() != Some(target_branch) {
                return Err(PocError::Integrity(
                    "stable operation ID resolved to a different rollback target".to_owned(),
                ));
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
            let current = cache
                .state
                .heads
                .get(branch)
                .map(|head| &cache.state.records[*head].outcome.value)
                .ok_or_else(|| {
                    PocError::RecoveryRequired(
                        "stored rollback outcome has no durable branch head".to_owned(),
                    )
                })?;
            if current != &record.outcome.value {
                return Err(PocError::RecoveryRequired(
                    "stored rollback outcome was superseded by another branch head".to_owned(),
                ));
            }
            sync_journal_data(&self.journal_path())?;
            return Ok(RefCommitReceipt {
                value: record.outcome.value.clone(),
                idempotent_replay: true,
                parent_directory_synced: true,
                outcome_path: self.journal_path(),
            });
        }

        let target_index = cache
            .state
            .heads
            .get(target_branch)
            .copied()
            .ok_or_else(|| {
                PocError::Integrity(format!(
                    "MPLA rollback target branch {target_branch} does not exist"
                ))
            })?;
        let target = cache.state.records[target_index].clone();
        validate_prerequisite(
            &target.prerequisite,
            &target.prerequisite.candidate,
            locator_store,
            false,
        )?;
        validate_record_pair(&target)?;
        let current_sequence = cache
            .state
            .heads
            .get(branch)
            .map(|index| cache.state.records[*index].outcome.value.sequence)
            .ok_or_else(|| {
                PocError::Integrity(format!("MPLA rollback branch {branch} does not exist"))
            })?;
        locator_store.with_selected(|selected_locator| {
            let selected_locator = selected_locator.ok_or_else(|| {
                PocError::Integrity("MPLA locator has no selected generation".to_owned())
            })?;
            let target_payload_root =
                PayloadRootId::parse(target.outcome.value.roots.root_id.as_str())?;
            if !selected_locator
                .forward
                .iter()
                .any(|entry| entry.payload_root == target_payload_root)
            {
                return Err(PocError::Integrity(format!(
                    "rollback target payload root {} is absent from the current locator",
                    target_payload_root.as_str()
                )));
            }
            let candidate = LocatorRefCandidate {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                publication_id: target.outcome.value.publication_id.clone(),
                roots: target.outcome.value.roots.clone(),
                locator_generation: selected_locator.receipt.generation,
                expected_sequence: current_sequence,
            };
            match self.commit_locked(
                &mut cache,
                branch,
                &candidate,
                &target.prerequisite.canonical,
                &selected_locator.receipt,
                locator_store,
                true,
                faults,
                TerminalResponse::Rollback,
                Some(target_branch),
            )? {
                RefCommitOutcome::Committed(receipt) => Ok(receipt),
                RefCommitOutcome::ExpectedParent { expected, observed } => {
                    Err(PocError::RecoveryRequired(format!(
                        "atomic rollback expected sequence {expected}, observed {observed}"
                    )))
                }
            }
        })
    }

    pub fn squash_branch(
        &self,
        branch: &str,
        operation_id: &crate::OperationId,
        locator_store: &LocatorStore,
        faults: &mut NamedFaultInjector,
    ) -> PocResult<RefCommitReceipt> {
        validate_path_component(branch, "branch")?;
        validate_path_component(operation_id.as_str(), "operation ID")?;

        let _lock = FileLock::exclusive(&self.lock_path())?;
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        refresh_cache(&self.journal_path(), &mut cache)?;

        let operation_key = (branch.to_owned(), operation_id.as_str().to_owned());
        if let Some(index) = cache.state.operations.get(&operation_key).copied() {
            let record = &cache.state.records[index];
            if record.rollback_target_branch.is_some() {
                return Err(PocError::Integrity(
                    "stable operation ID resolved to a rollback ref".to_owned(),
                ));
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
            let current = cache
                .state
                .heads
                .get(branch)
                .map(|head| &cache.state.records[*head].outcome.value)
                .ok_or_else(|| {
                    PocError::RecoveryRequired(
                        "stored squash outcome has no durable branch head".to_owned(),
                    )
                })?;
            if current != &record.outcome.value {
                return Err(PocError::RecoveryRequired(
                    "stored squash outcome was superseded by another branch head".to_owned(),
                ));
            }
            sync_journal_data(&self.journal_path())?;
            return Ok(RefCommitReceipt {
                value: record.outcome.value.clone(),
                idempotent_replay: true,
                parent_directory_synced: true,
                outcome_path: self.journal_path(),
            });
        }

        let current_index = cache.state.heads.get(branch).copied().ok_or_else(|| {
            PocError::Integrity(format!("MPLA squash branch {branch} does not exist"))
        })?;
        let current = cache.state.records[current_index].clone();
        validate_prerequisite_record(&current.prerequisite)?;
        validate_canonical(&current.prerequisite.canonical)?;
        validate_record_pair(&current)?;

        locator_store.with_selected_validating_generation(
            &current.prerequisite.locator,
            |selected_locator| {
                let selected_locator = selected_locator.ok_or_else(|| {
                    PocError::Integrity("MPLA locator has no selected generation".to_owned())
                })?;
                let payload_root =
                    PayloadRootId::parse(current.outcome.value.roots.root_id.as_str())?;
                if !selected_locator
                    .forward
                    .iter()
                    .any(|entry| entry.payload_root == payload_root)
                {
                    return Err(PocError::Integrity(format!(
                        "selected payload root {} is absent from the current locator",
                        payload_root.as_str()
                    )));
                }
                let candidate = LocatorRefCandidate {
                    schema_version: SCHEMA_VERSION,
                    operation_id: operation_id.clone(),
                    publication_id: current.outcome.value.publication_id.clone(),
                    roots: current.outcome.value.roots.clone(),
                    locator_generation: selected_locator.receipt.generation,
                    expected_sequence: current.outcome.value.sequence,
                };
                match self.commit_locked(
                    &mut cache,
                    branch,
                    &candidate,
                    &current.prerequisite.canonical,
                    &selected_locator.receipt,
                    locator_store,
                    true,
                    faults,
                    TerminalResponse::Publish,
                    None,
                )? {
                    RefCommitOutcome::Committed(receipt) => Ok(receipt),
                    RefCommitOutcome::ExpectedParent { expected, observed } => {
                        Err(PocError::RecoveryRequired(format!(
                            "atomic squash expected sequence {expected}, observed {observed}"
                        )))
                    }
                }
            },
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
        self.commit_locked(
            &mut cache,
            branch,
            candidate,
            canonical,
            locator,
            locator_store,
            false,
            faults,
            terminal_response,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_locked(
        &self,
        cache: &mut JournalCache,
        branch: &str,
        candidate: &LocatorRefCandidate,
        canonical: &CanonicalDurabilityReceipt,
        locator: &LocatorDurabilityReceipt,
        locator_store: &LocatorStore,
        locator_selection_lock_held: bool,
        faults: &mut NamedFaultInjector,
        terminal_response: TerminalResponse,
        rollback_target_branch: Option<&str>,
    ) -> PocResult<RefCommitOutcome> {
        let operation_key = (
            branch.to_owned(),
            candidate.operation_id.as_str().to_owned(),
        );
        if let Some(index) = cache.state.operations.get(&operation_key).copied() {
            let record = &cache.state.records[index];
            if record.rollback_target_branch.as_deref() != rollback_target_branch {
                return Err(PocError::Integrity(
                    "stable operation ID resolved to a different rollback target".to_owned(),
                ));
            }
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
            sync_journal_data(&self.journal_path())?;
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

        if !locator_selection_lock_held {
            locator_store.validate_receipt(locator)?;
        }
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
            rollback_target_branch: rollback_target_branch.map(str::to_owned),
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
        let (stamp, cursor) = match append_record(&self.journal_path(), &record, &cache.state) {
            Ok(committed) => committed,
            Err(error) => {
                cache.stamp = None;
                cache.state = JournalState::default();
                return Err(error);
            }
        };
        apply_record(&mut cache.state, record);
        cache.state.valid_bytes = cursor.logical_end;
        cache.state.cursor_generation = cursor.generation;
        cache.state.cursor_slot = cursor.slot;
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
        sync_journal_data(&self.journal_path())?;
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

impl SealedPairedRefStore {
    pub fn open(root: impl Into<PathBuf>) -> PocResult<Self> {
        let root = root.into();
        require_existing_directory(&root)?;
        let root = std::fs::canonicalize(&root)
            .map_err(|source| PocError::io("canonicalize sealed paired ref root", &root, source))?;
        require_existing_directory(&root)?;
        let lock_path = root.join("LOCK");
        require_regular_file(&lock_path)?;
        let lock = Arc::new(FileLock::try_shared(&lock_path)?);
        require_existing_directory(&root)?;
        require_regular_file(&lock_path)?;
        require_regular_file(&root.join("JOURNAL"))?;
        let layout = read_sealed_layout_version(&root.join("LAYOUT"))?;
        require_exact_sealed_inventory(&root, layout)?;
        let state = match layout {
            LayoutVersion::V3 => read_journal(&root.join("JOURNAL"))?.1,
            LayoutVersion::V2 | LayoutVersion::Missing => {
                read_strict_legacy_journal(&root.join("JOURNAL"))?
            }
        };
        Ok(Self {
            root,
            state: Arc::new(state),
            layout,
            _lock: lock,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn branch_names(&self) -> Vec<String> {
        self.state.heads.keys().cloned().collect()
    }

    pub fn require_v3_layout(&self) -> PocResult<SealedPairedRefLayoutReceipt> {
        if self.layout != LayoutVersion::V3 {
            return Err(PocError::Integrity(
                "sealed paired ref store does not use the required v3 cursor layout".to_owned(),
            ));
        }
        Ok(SealedPairedRefLayoutReceipt {
            format: "mpla-poc-paired-ref-layout-v3".to_owned(),
            journal_data_bytes: MAX_JOURNAL_BYTES,
            journal_total_bytes: JOURNAL_TOTAL_BYTES,
            cursor_generation: self.state.cursor_generation,
            cursor_slot: u64::try_from(self.state.cursor_slot)
                .map_err(|_| PocError::Integrity("paired ref cursor slot overflow".to_owned()))?,
            logical_end: self.state.valid_bytes,
            record_count: u64::try_from(self.state.records.len()).map_err(|_| {
                PocError::Integrity("paired ref journal record count overflow".to_owned())
            })?,
            last_record_hash: self
                .state
                .records
                .last()
                .map(|record| record.record_hash.clone()),
        })
    }

    pub fn read(&self, branch: &str) -> PocResult<Option<PairedRefValue>> {
        validate_path_component(branch, "branch")?;
        Ok(self
            .state
            .heads
            .get(branch)
            .map(|index| self.state.records[*index].outcome.value.clone()))
    }

    pub fn read_resolved(
        &self,
        branch: &str,
        locator_store: &SealedLocatorStore,
    ) -> PocResult<Option<ResolvedPairedRef>> {
        validate_path_component(branch, "branch")?;
        let Some(index) = self.state.heads.get(branch).copied() else {
            return Ok(None);
        };
        let record = &self.state.records[index];
        validate_sealed_prerequisite(
            &record.prerequisite,
            &record.prerequisite.candidate,
            locator_store,
        )?;
        validate_record_pair(record)?;
        Ok(Some(ResolvedPairedRef {
            value: record.outcome.value.clone(),
            canonical: record.prerequisite.canonical.clone(),
            locator: record.prerequisite.locator.clone(),
        }))
    }
}

fn refresh_cache(path: &Path, cache: &mut JournalCache) -> PocResult<()> {
    let (file, stamp, cursor) = open_journal_snapshot(path)?;
    let cursor_matches = cache.state.cursor_generation == cursor.generation
        && cache.state.cursor_slot == cursor.slot
        && cache.state.valid_bytes == cursor.logical_end
        && u64::try_from(cache.state.records.len()).ok() == Some(cursor.record_count)
        && cache
            .state
            .records
            .last()
            .map(|record| decode_hash(&record.record_hash))
            .transpose()?
            .unwrap_or([0_u8; 32])
            == cursor.last_hash;
    if cache.stamp == Some(stamp) && cursor_matches {
        return Ok(());
    }
    let state = read_journal_from_snapshot(&file, path, cursor)?;
    cache.stamp = Some(stamp);
    cache.state = state;
    Ok(())
}

fn append_record(
    path: &Path,
    record: &RefJournalRecord,
    state: &JournalState,
) -> PocResult<(FileStamp, JournalCursor)> {
    let frame = encode_frame(record)?;
    let frame_length = u64::try_from(frame.len())
        .map_err(|_| PocError::Integrity("paired ref journal frame length overflow".to_owned()))?;
    let projected_length = state.valid_bytes.checked_add(frame_length).ok_or_else(|| {
        PocError::Integrity("paired ref journal projected length overflow".to_owned())
    })?;
    if projected_length > MAX_JOURNAL_BYTES {
        return Err(PocError::Integrity(format!(
            "paired ref journal would exceed {MAX_JOURNAL_BYTES} bytes"
        )));
    }
    let generation = state.cursor_generation.checked_add(1).ok_or_else(|| {
        PocError::Integrity("paired ref journal cursor generation overflow".to_owned())
    })?;
    let record_count = u64::try_from(state.records.len())
        .map_err(|_| PocError::Integrity("paired ref journal record count overflow".to_owned()))?
        .checked_add(1)
        .ok_or_else(|| {
            PocError::Integrity("paired ref journal record count overflow".to_owned())
        })?;
    let cursor = JournalCursor {
        generation,
        logical_end: projected_length,
        record_count,
        last_hash: decode_hash(&record.record_hash)?,
        slot: 1_usize
            .checked_sub(state.cursor_slot)
            .ok_or_else(|| PocError::Integrity("paired ref cursor slot is invalid".to_owned()))?,
    };
    let file = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| PocError::io("open paired ref journal for append", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| PocError::io("stat paired ref journal for append", path, source))?;
    if !metadata.is_file() || metadata.len() != JOURNAL_TOTAL_BYTES {
        return Err(PocError::Integrity(
            "paired ref journal fixed layout is invalid".to_owned(),
        ));
    }
    file.write_all_at(&frame, state.valid_bytes)
        .map_err(|source| PocError::io("append paired ref journal record", path, source))?;
    file.sync_data()
        .map_err(|source| PocError::io("fdatasync paired ref journal frame", path, source))?;
    file.write_all_at(&encode_cursor(&cursor), cursor_offset(cursor.slot)?)
        .map_err(|source| PocError::io("publish paired ref journal cursor", path, source))?;
    file.sync_data()
        .map_err(|source| PocError::io("fdatasync paired ref journal cursor", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| PocError::io("stat appended paired ref journal", path, source))?;
    Ok((stamp_from_metadata(&metadata), cursor))
}

fn sync_journal_data(path: &Path) -> PocResult<()> {
    let file = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| PocError::io("open paired ref journal for replay sync", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| PocError::io("stat paired ref journal for replay sync", path, source))?;
    if !metadata.is_file() || metadata.len() != JOURNAL_TOTAL_BYTES {
        return Err(PocError::Integrity(
            "paired ref journal fixed layout is invalid".to_owned(),
        ));
    }
    file.sync_data()
        .map_err(|source| PocError::io("fdatasync replayed paired ref journal", path, source))
}

fn read_journal(path: &Path) -> PocResult<(FileStamp, JournalState)> {
    let (file, stamp, cursor) = open_journal_snapshot(path)?;
    let state = read_journal_from_snapshot(&file, path, cursor)?;
    Ok((stamp, state))
}

fn open_journal_snapshot(path: &Path) -> PocResult<(File, FileStamp, JournalCursor)> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| PocError::io("open paired ref journal", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| PocError::io("stat paired ref journal", path, source))?;
    if !metadata.is_file() || metadata.len() != JOURNAL_TOTAL_BYTES {
        return Err(PocError::Integrity(
            "paired ref journal fixed layout is invalid".to_owned(),
        ));
    }
    let cursor = read_active_cursor(&file, path)?;
    Ok((file, stamp_from_metadata(&metadata), cursor))
}

fn read_journal_from_snapshot(
    file: &File,
    path: &Path,
    cursor: JournalCursor,
) -> PocResult<JournalState> {
    let mut state = read_record_chain(&file, path, cursor.logical_end)?;
    let record_count = u64::try_from(state.records.len())
        .map_err(|_| PocError::Integrity("paired ref journal record count overflow".to_owned()))?;
    let observed_last_hash = state
        .records
        .last()
        .map(|record| decode_hash(&record.record_hash))
        .transpose()?
        .unwrap_or([0_u8; 32]);
    if record_count != cursor.record_count || observed_last_hash != cursor.last_hash {
        return Err(PocError::Integrity(
            "paired ref journal cursor disagrees with its record chain".to_owned(),
        ));
    }
    state.valid_bytes = cursor.logical_end;
    state.cursor_generation = cursor.generation;
    state.cursor_slot = cursor.slot;
    Ok(state)
}

fn read_record_chain(file: &File, path: &Path, logical_end: u64) -> PocResult<JournalState> {
    let mut state = JournalState::default();
    let mut offset = 0_u64;
    while offset < logical_end {
        if logical_end - offset < JOURNAL_HEADER_BYTES as u64 {
            return Err(PocError::Integrity(
                "paired ref journal cursor ends inside a frame header".to_owned(),
            ));
        }
        let mut header = [0_u8; JOURNAL_HEADER_BYTES];
        file.read_exact_at(&mut header, offset)
            .map_err(|source| PocError::io("read paired ref journal header", path, source))?;
        if header[..4] != JOURNAL_MAGIC {
            return Err(PocError::Integrity(format!(
                "paired ref journal frame magic mismatch at byte {offset}"
            )));
        }
        let version = u32::from_le_bytes(
            header[4..8]
                .try_into()
                .map_err(|_| PocError::Integrity("paired ref journal version frame".to_owned()))?,
        );
        if version != JOURNAL_FRAME_VERSION {
            return Err(PocError::Integrity(format!(
                "unsupported paired ref journal frame version {version}"
            )));
        }
        let payload_length = u64::from_le_bytes(
            header[8..16]
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
        let payload_start = offset
            .checked_add(JOURNAL_HEADER_BYTES as u64)
            .ok_or_else(|| {
                PocError::Integrity("paired ref journal frame offset overflow".to_owned())
            })?;
        let Some(payload_end) = payload_start.checked_add(payload_length as u64) else {
            return Err(PocError::Integrity(
                "paired ref journal frame length overflow".to_owned(),
            ));
        };
        if payload_end > logical_end {
            return Err(PocError::Integrity(
                "paired ref journal cursor ends inside a record".to_owned(),
            ));
        }
        let mut payload = vec![0_u8; payload_length];
        file.read_exact_at(&mut payload, payload_start)
            .map_err(|source| PocError::io("read paired ref journal record", path, source))?;
        let record: RefJournalRecord = serde_json::from_slice(&payload)?;
        validate_journal_record(&record, &state)?;
        apply_record(&mut state, record);
        offset = payload_end;
    }
    state.valid_bytes = offset;
    Ok(state)
}

fn read_active_cursor(file: &File, path: &Path) -> PocResult<JournalCursor> {
    let mut cursors = Vec::with_capacity(CURSOR_SLOT_COUNT as usize);
    for slot in 0..CURSOR_SLOT_COUNT as usize {
        let mut bytes = [0_u8; CURSOR_SLOT_BYTES as usize];
        file.read_exact_at(&mut bytes, cursor_offset(slot)?)
            .map_err(|source| PocError::io("read paired ref journal cursor", path, source))?;
        if let Some(cursor) = decode_cursor(&bytes, slot) {
            cursors.push(cursor);
        }
    }
    cursors.sort_unstable_by_key(|cursor| cursor.generation);
    let Some(cursor) = cursors.pop() else {
        return Err(PocError::Integrity(
            "paired ref journal has no valid cursor".to_owned(),
        ));
    };
    if cursors
        .last()
        .is_some_and(|other| other.generation == cursor.generation)
    {
        return Err(PocError::Integrity(
            "paired ref journal cursor generation is ambiguous".to_owned(),
        ));
    }
    Ok(cursor)
}

fn encode_frame(record: &RefJournalRecord) -> PocResult<Vec<u8>> {
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
    Ok(frame)
}

fn encode_cursor(cursor: &JournalCursor) -> [u8; CURSOR_SLOT_BYTES as usize] {
    let mut bytes = [0_u8; CURSOR_SLOT_BYTES as usize];
    bytes[..8].copy_from_slice(&CURSOR_MAGIC);
    bytes[8..12].copy_from_slice(&CURSOR_VERSION.to_le_bytes());
    bytes[16..24].copy_from_slice(&cursor.generation.to_le_bytes());
    bytes[24..32].copy_from_slice(&cursor.logical_end.to_le_bytes());
    bytes[32..40].copy_from_slice(&cursor.record_count.to_le_bytes());
    bytes[40..CURSOR_PREFIX_BYTES].copy_from_slice(&cursor.last_hash);
    let digest = Sha256::digest(&bytes[..CURSOR_PREFIX_BYTES]);
    bytes[CURSOR_PREFIX_BYTES..CURSOR_DIGEST_END].copy_from_slice(&digest);
    bytes
}

fn decode_cursor(bytes: &[u8; CURSOR_SLOT_BYTES as usize], slot: usize) -> Option<JournalCursor> {
    if bytes[..8] != CURSOR_MAGIC
        || u32::from_le_bytes(bytes[8..12].try_into().ok()?) != CURSOR_VERSION
        || bytes[12..16] != [0_u8; 4]
        || bytes[CURSOR_DIGEST_END..].iter().any(|byte| *byte != 0)
    {
        return None;
    }
    let digest = Sha256::digest(&bytes[..CURSOR_PREFIX_BYTES]);
    if bytes[CURSOR_PREFIX_BYTES..CURSOR_DIGEST_END] != digest[..] {
        return None;
    }
    let generation = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    let logical_end = u64::from_le_bytes(bytes[24..32].try_into().ok()?);
    let record_count = u64::from_le_bytes(bytes[32..40].try_into().ok()?);
    let last_hash = bytes[40..CURSOR_PREFIX_BYTES].try_into().ok()?;
    if generation == 0
        || logical_end > MAX_JOURNAL_BYTES
        || record_count > logical_end / JOURNAL_HEADER_BYTES as u64
        || ((record_count == 0) != (logical_end == 0))
        || (record_count == 0 && last_hash != [0_u8; 32])
    {
        return None;
    }
    Some(JournalCursor {
        generation,
        logical_end,
        record_count,
        last_hash,
        slot,
    })
}

fn cursor_offset(slot: usize) -> PocResult<u64> {
    let slot = u64::try_from(slot)
        .map_err(|_| PocError::Integrity("paired ref cursor slot overflow".to_owned()))?;
    if slot >= CURSOR_SLOT_COUNT {
        return Err(PocError::Integrity(
            "paired ref cursor slot is invalid".to_owned(),
        ));
    }
    Ok(MAX_JOURNAL_BYTES + slot * CURSOR_SLOT_BYTES)
}

fn decode_hash(value: &str) -> PocResult<[u8; 32]> {
    if value.len() != 64 {
        return Err(PocError::Integrity(
            "paired ref journal record hash is invalid".to_owned(),
        ));
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_nibble(pair[0])?;
        let low = decode_hex_nibble(pair[1])?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

fn decode_hex_nibble(byte: u8) -> PocResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(PocError::Integrity(
            "paired ref journal record hash is invalid".to_owned(),
        )),
    }
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
    if let Some(target_branch) = record.rollback_target_branch.as_deref() {
        validate_path_component(target_branch, "rollback target branch")?;
    }
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

fn validate_sealed_prerequisite(
    prerequisite: &RefPrerequisiteRecord,
    candidate: &LocatorRefCandidate,
    locator_store: &SealedLocatorStore,
) -> PocResult<()> {
    validate_prerequisite_record(prerequisite)?;
    if prerequisite.candidate != *candidate {
        return Err(PocError::Integrity(
            "stable operation ID was reused for another paired ref candidate".to_owned(),
        ));
    }
    validate_canonical(&prerequisite.canonical)?;
    locator_store.validate_generation_receipt(&prerequisite.locator)
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
    let file = File::options()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| PocError::io(action, path, source))?;
    if !file
        .metadata()
        .map_err(|source| PocError::io("stat paired ref layout file", path, source))?
        .is_file()
    {
        return Err(PocError::Integrity(format!(
            "paired ref layout path is not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn existing_v3_layout_is_ready(
    root: &Path,
    journal_path: &Path,
    lock_path: &Path,
    layout_path: &Path,
) -> PocResult<bool> {
    if !root.is_dir() || read_layout_version(layout_path)? != LayoutVersion::V3 {
        return Ok(false);
    }
    if !regular_file_exists(lock_path)? || !regular_file_exists(journal_path)? {
        return Ok(false);
    }
    let metadata = std::fs::symlink_metadata(journal_path)
        .map_err(|source| PocError::io("stat paired ref journal", journal_path, source))?;
    if metadata.len() != JOURNAL_TOTAL_BYTES {
        return Err(PocError::Integrity(
            "paired ref v3 journal has an invalid fixed length".to_owned(),
        ));
    }
    Ok(true)
}

fn ensure_v3_layout(root: &Path, journal_path: &Path, layout_path: &Path) -> PocResult<()> {
    match read_layout_version(layout_path)? {
        LayoutVersion::V3 => {
            if !regular_file_exists(journal_path)? {
                return Err(PocError::Integrity(
                    "paired ref v3 layout is missing its journal".to_owned(),
                ));
            }
            read_journal(journal_path)?;
        }
        layout @ (LayoutVersion::Missing | LayoutVersion::V2) => {
            if !regular_file_exists(journal_path)? {
                if layout == LayoutVersion::V2 {
                    return Err(PocError::Integrity(
                        "paired ref v2 layout is missing its journal".to_owned(),
                    ));
                }
                replace_journal(root, journal_path, &[])?;
            } else {
                let metadata = std::fs::symlink_metadata(journal_path).map_err(|source| {
                    PocError::io("stat paired ref journal", journal_path, source)
                })?;
                if metadata.len() == JOURNAL_TOTAL_BYTES {
                    read_journal(journal_path)?;
                } else {
                    let state = read_legacy_journal(journal_path)?;
                    replace_journal(root, journal_path, &state.records)?;
                }
            }
            replace_layout_marker(root, layout_path)?;
        }
    }
    Ok(())
}

fn replace_journal(
    root: &Path,
    journal_path: &Path,
    records: &[RefJournalRecord],
) -> PocResult<()> {
    let temp_path = root.join("JOURNAL.v3.tmp");
    remove_stale_temp(&temp_path)?;
    provision_journal(&temp_path, records)?;
    std::fs::rename(&temp_path, journal_path)
        .map_err(|source| PocError::io("replace paired ref journal", journal_path, source))?;
    fsync_dir(root)
}

fn provision_journal(path: &Path, records: &[RefJournalRecord]) -> PocResult<()> {
    let file = File::options()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| PocError::io("create paired ref v3 journal", path, source))?;
    allocate_journal(&file, path)?;
    let mut logical_end = 0_u64;
    for record in records {
        let frame = encode_frame(record)?;
        let frame_length = u64::try_from(frame.len()).map_err(|_| {
            PocError::Integrity("paired ref journal frame length overflow".to_owned())
        })?;
        let next_end = logical_end.checked_add(frame_length).ok_or_else(|| {
            PocError::Integrity("paired ref journal projected length overflow".to_owned())
        })?;
        if next_end > MAX_JOURNAL_BYTES {
            return Err(PocError::Integrity(format!(
                "paired ref journal would exceed {MAX_JOURNAL_BYTES} bytes"
            )));
        }
        file.write_all_at(&frame, logical_end)
            .map_err(|source| PocError::io("write migrated paired ref record", path, source))?;
        logical_end = next_end;
    }
    let record_count = u64::try_from(records.len())
        .map_err(|_| PocError::Integrity("paired ref journal record count overflow".to_owned()))?;
    let last_hash = records
        .last()
        .map(|record| decode_hash(&record.record_hash))
        .transpose()?
        .unwrap_or([0_u8; 32]);
    let cursor = JournalCursor {
        generation: 1,
        logical_end,
        record_count,
        last_hash,
        slot: 0,
    };
    file.write_all_at(&encode_cursor(&cursor), cursor_offset(cursor.slot)?)
        .map_err(|source| PocError::io("initialize paired ref journal cursor", path, source))?;
    file.sync_all()
        .map_err(|source| PocError::io("fsync initialized paired ref journal", path, source))
}

#[cfg(target_os = "linux")]
fn allocate_journal(file: &File, path: &Path) -> PocResult<()> {
    loop {
        // SAFETY: the descriptor is owned by `file`, and the offset and length are valid.
        let result = unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, JOURNAL_PREALLOCATED_BYTES) };
        if result == 0 {
            break;
        }
        let source = std::io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(PocError::io("preallocate paired ref journal", path, source));
    }
    // This function is called only for a newly-created, zero-length inode.
    // Successful fallocate creates fixed-size unwritten extents that read as
    // zero, so rewriting all 64 MiB would add I/O without strengthening the
    // fixed-layout or zero-tail contract.
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn allocate_journal(file: &File, path: &Path) -> PocResult<()> {
    file.set_len(JOURNAL_TOTAL_BYTES)
        .map_err(|source| PocError::io("size paired ref journal", path, source))
}

fn regular_file_exists(path: &Path) -> PocResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(PocError::Integrity(format!(
            "paired ref layout path is not a regular file: {}",
            path.display()
        ))),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PocError::io("stat paired ref layout path", path, source)),
    }
}

fn require_existing_directory(path: &Path) -> PocResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat sealed paired ref root", path, source))?;
    if !metadata.file_type().is_dir() {
        return Err(PocError::Integrity(format!(
            "sealed paired ref root is not an existing directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_regular_file(path: &Path) -> PocResult<()> {
    if !regular_file_exists(path)? {
        return Err(PocError::Integrity(format!(
            "sealed paired ref layout is missing required file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_exact_sealed_inventory(root: &Path, layout: LayoutVersion) -> PocResult<()> {
    let mut observed = std::fs::read_dir(root)
        .map_err(|source| PocError::io("enumerate sealed paired ref root", root, source))?
        .map(|entry| {
            entry
                .map_err(|source| {
                    PocError::io("read sealed paired ref directory entry", root, source)
                })?
                .file_name()
                .into_string()
                .map_err(|_| {
                    PocError::Integrity(
                        "sealed paired ref root contains a non-Unicode entry".to_owned(),
                    )
                })
        })
        .collect::<PocResult<Vec<_>>>()?;
    observed.sort();
    let expected = match layout {
        LayoutVersion::Missing => vec!["JOURNAL".to_owned(), "LOCK".to_owned()],
        LayoutVersion::V2 | LayoutVersion::V3 => {
            vec!["JOURNAL".to_owned(), "LAYOUT".to_owned(), "LOCK".to_owned()]
        }
    };
    if observed != expected {
        return Err(PocError::Integrity(format!(
            "sealed paired ref root has an unexpected exact inventory: {}",
            root.display()
        )));
    }
    Ok(())
}

fn read_sealed_layout_version(path: &Path) -> PocResult<LayoutVersion> {
    match std::fs::symlink_metadata(path) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(LayoutVersion::Missing),
        Ok(metadata) if metadata.file_type().is_file() => read_layout_version(path),
        Ok(_) => Err(PocError::Integrity(format!(
            "sealed paired ref layout marker is not a regular file: {}",
            path.display()
        ))),
        Err(source) => Err(PocError::io(
            "stat sealed paired ref layout marker",
            path,
            source,
        )),
    }
}

fn read_layout_version(path: &Path) -> PocResult<LayoutVersion> {
    let mut file = match File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LayoutVersion::Missing);
        }
        Err(source) => {
            return Err(PocError::io("open paired ref layout marker", path, source));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|source| PocError::io("stat paired ref layout marker", path, source))?;
    if !metadata.is_file() {
        return Err(PocError::Integrity(format!(
            "paired ref layout path is not a regular file: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| PocError::io("read paired ref layout marker", path, source))?;
    if bytes == LAYOUT_MARKER_V2 {
        Ok(LayoutVersion::V2)
    } else if bytes == LAYOUT_MARKER_V3 {
        Ok(LayoutVersion::V3)
    } else {
        Err(PocError::Integrity(
            "paired ref layout marker is invalid".to_owned(),
        ))
    }
}

fn replace_layout_marker(root: &Path, path: &Path) -> PocResult<()> {
    let temp_path = root.join("LAYOUT.v3.tmp");
    remove_stale_temp(&temp_path)?;
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp_path)
        .map_err(|source| PocError::io("create paired ref layout marker", &temp_path, source))?;
    file.write_all(LAYOUT_MARKER_V3)
        .map_err(|source| PocError::io("write paired ref layout marker", &temp_path, source))?;
    file.sync_all()
        .map_err(|source| PocError::io("fsync paired ref layout marker", &temp_path, source))?;
    std::fs::rename(&temp_path, path)
        .map_err(|source| PocError::io("replace paired ref layout marker", path, source))?;
    fsync_dir(root)
}

fn remove_stale_temp(path: &Path) -> PocResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            std::fs::remove_file(path)
                .map_err(|source| PocError::io("remove stale paired ref temp file", path, source))
        }
        Ok(_) => Err(PocError::Integrity(format!(
            "paired ref temp path is not a regular file: {}",
            path.display()
        ))),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PocError::io("stat paired ref temp file", path, source)),
    }
}

fn read_legacy_journal(path: &Path) -> PocResult<JournalState> {
    read_legacy_journal_with_tail_policy(path, true)
}

fn read_strict_legacy_journal(path: &Path) -> PocResult<JournalState> {
    read_legacy_journal_with_tail_policy(path, false)
}

fn read_legacy_journal_with_tail_policy(
    path: &Path,
    recover_torn_tail: bool,
) -> PocResult<JournalState> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| PocError::io("open legacy paired ref journal", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| PocError::io("stat legacy paired ref journal", path, source))?;
    let length = metadata.len();
    if !metadata.is_file() || length > MAX_JOURNAL_BYTES {
        return Err(PocError::Integrity(
            "legacy paired ref journal layout is invalid".to_owned(),
        ));
    }
    let mut state = JournalState::default();
    let mut offset = 0_u64;
    while offset < length {
        if length - offset < JOURNAL_HEADER_BYTES as u64 {
            if length == MAX_JOURNAL_BYTES {
                require_zero_legacy_tail(&file, path, offset, length)?;
                break;
            }
            if recover_torn_tail {
                break;
            }
            return Err(PocError::Integrity(
                "sealed legacy paired ref journal ends inside a frame header".to_owned(),
            ));
        }
        let mut header = [0_u8; JOURNAL_HEADER_BYTES];
        file.read_exact_at(&mut header, offset).map_err(|source| {
            PocError::io("read legacy paired ref journal header", path, source)
        })?;
        if length == MAX_JOURNAL_BYTES && header.iter().all(|byte| *byte == 0) {
            require_zero_legacy_tail(&file, path, offset, length)?;
            break;
        }
        if header[..4] != JOURNAL_MAGIC {
            return Err(PocError::Integrity(format!(
                "legacy paired ref journal frame magic mismatch at byte {offset}"
            )));
        }
        let version = u32::from_le_bytes(header[4..8].try_into().map_err(|_| {
            PocError::Integrity("legacy paired ref journal version frame".to_owned())
        })?);
        if version != JOURNAL_FRAME_VERSION {
            return Err(PocError::Integrity(format!(
                "unsupported legacy paired ref journal frame version {version}"
            )));
        }
        let payload_length =
            usize::try_from(u64::from_le_bytes(header[8..16].try_into().map_err(
                |_| PocError::Integrity("legacy paired ref journal length frame".to_owned()),
            )?))
            .map_err(|_| {
                PocError::Integrity("legacy paired ref journal frame length overflow".to_owned())
            })?;
        if payload_length > MAX_JOURNAL_RECORD_BYTES {
            return Err(PocError::Integrity(format!(
                "legacy paired ref journal frame exceeds {MAX_JOURNAL_RECORD_BYTES} bytes"
            )));
        }
        let payload_start = offset
            .checked_add(JOURNAL_HEADER_BYTES as u64)
            .ok_or_else(|| {
                PocError::Integrity("legacy paired ref journal frame offset overflow".to_owned())
            })?;
        let payload_end = payload_start
            .checked_add(payload_length as u64)
            .ok_or_else(|| {
                PocError::Integrity("legacy paired ref journal frame length overflow".to_owned())
            })?;
        if payload_end > length {
            if recover_torn_tail {
                break;
            }
            return Err(PocError::Integrity(
                "sealed legacy paired ref journal ends inside a record".to_owned(),
            ));
        }
        let mut payload = vec![0_u8; payload_length];
        file.read_exact_at(&mut payload, payload_start)
            .map_err(|source| {
                PocError::io("read legacy paired ref journal record", path, source)
            })?;
        let record: RefJournalRecord = serde_json::from_slice(&payload)?;
        validate_journal_record(&record, &state)?;
        apply_record(&mut state, record);
        offset = payload_end;
    }
    state.valid_bytes = offset;
    Ok(state)
}

fn require_zero_legacy_tail(file: &File, path: &Path, offset: u64, length: u64) -> PocResult<()> {
    let mut cursor = offset;
    let mut buffer = vec![0_u8; 1024 * 1024];
    let zeros = vec![0_u8; buffer.len()];
    while cursor < length {
        let remaining = usize::try_from((length - cursor).min(buffer.len() as u64))
            .map_err(|_| PocError::Integrity("legacy journal tail length overflow".to_owned()))?;
        file.read_exact_at(&mut buffer[..remaining], cursor)
            .map_err(|source| PocError::io("read legacy paired ref journal tail", path, source))?;
        // SAFETY: both slices are valid for `remaining` bytes and do not overlap.
        if unsafe { libc::memcmp(buffer.as_ptr().cast(), zeros.as_ptr().cast(), remaining) } != 0 {
            return Err(PocError::Integrity(
                "sealed legacy paired ref journal has a nonzero preallocated tail".to_owned(),
            ));
        }
        cursor = cursor
            .checked_add(remaining as u64)
            .ok_or_else(|| PocError::Integrity("legacy journal tail offset overflow".to_owned()))?;
    }
    Ok(())
}

fn stamp_from_metadata(metadata: &std::fs::Metadata) -> FileStamp {
    FileStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
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
