use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{fsync_dir, read_json, write_immutable_json, FileLock};
use crate::locator::LocatorStore;
use crate::recovery::reach_real_operation;
use crate::{
    CanonicalDurabilityReceipt, LocatorDurabilityReceipt, LocatorRefCandidate, NamedFaultInjector,
    NamedFaultPoint, PairedRefValue, PocError, PocResult, RefSequence, SCHEMA_VERSION,
};

const REF_FORMAT: &str = "mpla-poc-paired-ref-v1";

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

impl PairedRefStore {
    pub fn open(root: impl Into<PathBuf>) -> PocResult<Self> {
        let store = Self { root: root.into() };
        std::fs::create_dir_all(store.branches_dir()).map_err(|source| {
            PocError::io(
                "create paired ref branches directory",
                store.branches_dir(),
                source,
            )
        })?;
        fsync_dir(&store.root)?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn read(&self, branch: &str) -> PocResult<Option<PairedRefValue>> {
        let branch_dir = self.prepare_branch(branch)?;
        let _lock = FileLock::shared(&branch_dir.join("LOCK"))?;
        read_head(&branch_dir)
    }

    pub fn read_resolved(
        &self,
        branch: &str,
        locator_store: &LocatorStore,
    ) -> PocResult<Option<ResolvedPairedRef>> {
        let branch_dir = self.prepare_branch(branch)?;
        let _lock = FileLock::shared(&branch_dir.join("LOCK"))?;
        let Some(value) = read_head(&branch_dir)? else {
            return Ok(None);
        };
        let prerequisite = read_prerequisite(&branch_dir, value.operation_id.as_str())?;
        validate_prerequisite(&prerequisite, &prerequisite.candidate, locator_store, false)?;
        if prerequisite.candidate.roots != value.roots
            || prerequisite.candidate.locator_generation != value.locator_generation
            || prerequisite.candidate.operation_id != value.operation_id
            || prerequisite.candidate.publication_id != value.publication_id
        {
            return Err(PocError::RecoveryRequired(
                "selected paired ref disagrees with its durable prerequisites".to_owned(),
            ));
        }
        Ok(Some(ResolvedPairedRef {
            value,
            canonical: prerequisite.canonical,
            locator: prerequisite.locator,
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
        validate_candidate(candidate)?;
        validate_canonical(canonical)?;
        locator_store.validate_generation_receipt(locator)?;
        if locator.generation != candidate.locator_generation {
            return Err(PocError::Integrity(format!(
                "candidate locator generation {} does not match durability receipt {}",
                candidate.locator_generation, locator.generation
            )));
        }

        let branch_dir = self.prepare_branch(branch)?;
        let _lock = FileLock::exclusive(&branch_dir.join("LOCK"))?;
        let candidate_sha256 = digest_json(candidate)?;
        if let Some(outcome) = read_outcome(&branch_dir, candidate.operation_id.as_str())? {
            validate_outcome(&outcome, candidate, &candidate_sha256)?;
            let current = read_head(&branch_dir)?.ok_or_else(|| {
                PocError::RecoveryRequired(
                    "stored paired ref outcome has no durable branch head".to_owned(),
                )
            })?;
            if current != outcome.value {
                return Err(PocError::RecoveryRequired(
                    "stored paired ref outcome disagrees with durable branch head".to_owned(),
                ));
            }
            return Ok(RefCommitOutcome::Committed(RefCommitReceipt {
                value: current,
                idempotent_replay: true,
                parent_directory_synced: true,
                outcome_path: outcome_path(&branch_dir, candidate.operation_id.as_str()),
            }));
        }

        if let Some(current) = read_head(&branch_dir)? {
            if current.operation_id == candidate.operation_id {
                validate_matching_head(&current, candidate)?;
                fsync_dir(&branch_dir)?;
                let prerequisite = read_prerequisite(&branch_dir, candidate.operation_id.as_str())?;
                validate_prerequisite(&prerequisite, candidate, locator_store, false)?;
                let outcome = RefOutcomeRecord {
                    schema_version: SCHEMA_VERSION,
                    format: REF_FORMAT.to_owned(),
                    candidate_sha256,
                    value: current.clone(),
                };
                let outcome_path = outcome_path(&branch_dir, candidate.operation_id.as_str());
                write_immutable_json(&outcome_path, &outcome)?;
                return Ok(RefCommitOutcome::Committed(RefCommitReceipt {
                    value: current,
                    idempotent_replay: true,
                    parent_directory_synced: true,
                    outcome_path,
                }));
            }
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
        let prerequisite = RefPrerequisiteRecord {
            schema_version: SCHEMA_VERSION,
            format: REF_FORMAT.to_owned(),
            candidate: candidate.clone(),
            candidate_sha256: candidate_sha256.clone(),
            canonical: canonical.clone(),
            locator: locator.clone(),
        };
        let prerequisite_path = prerequisite_path(&branch_dir, candidate.operation_id.as_str());
        write_immutable_json(&prerequisite_path, &prerequisite)?;

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
        reach_real_operation(
            faults,
            NamedFaultPoint::RefBeforeTemp,
            &candidate.operation_id,
            [prerequisite_path.clone()],
            None,
            true,
        )?;
        replace_head(&branch_dir, &value, &candidate.operation_id, faults)?;

        let outcome = RefOutcomeRecord {
            schema_version: SCHEMA_VERSION,
            format: REF_FORMAT.to_owned(),
            candidate_sha256,
            value: value.clone(),
        };
        let outcome_path = outcome_path(&branch_dir, candidate.operation_id.as_str());
        write_immutable_json(&outcome_path, &outcome)?;
        let response_point = match terminal_response {
            TerminalResponse::Publish => NamedFaultPoint::ResponseLossPublish,
            TerminalResponse::Rollback => NamedFaultPoint::ResponseLossRollback,
        };
        reach_real_operation(
            faults,
            response_point,
            &candidate.operation_id,
            [outcome_path.clone(), branch_dir.join("HEAD")],
            None,
            true,
        )?;
        Ok(RefCommitOutcome::Committed(RefCommitReceipt {
            value,
            idempotent_replay: false,
            parent_directory_synced: true,
            outcome_path,
        }))
    }

    pub fn recover_committed(
        &self,
        branch: &str,
        operation_id: &str,
        locator_store: &LocatorStore,
    ) -> PocResult<Option<RefCommitReceipt>> {
        validate_path_component(operation_id, "operation ID")?;
        let branch_dir = self.prepare_branch(branch)?;
        let _lock = FileLock::exclusive(&branch_dir.join("LOCK"))?;
        let Some(value) = read_head(&branch_dir)? else {
            return Ok(None);
        };
        if value.operation_id.as_str() != operation_id {
            return Ok(None);
        }
        let prerequisite = read_prerequisite(&branch_dir, operation_id)?;
        validate_prerequisite(&prerequisite, &prerequisite.candidate, locator_store, false)?;
        validate_matching_head(&value, &prerequisite.candidate)?;
        fsync_dir(&branch_dir)?;
        let outcome_path = outcome_path(&branch_dir, operation_id);
        let candidate_sha256 = digest_json(&prerequisite.candidate)?;
        if let Some(outcome) = read_outcome(&branch_dir, operation_id)? {
            validate_outcome(&outcome, &prerequisite.candidate, &candidate_sha256)?;
        } else {
            write_immutable_json(
                &outcome_path,
                &RefOutcomeRecord {
                    schema_version: SCHEMA_VERSION,
                    format: REF_FORMAT.to_owned(),
                    candidate_sha256,
                    value: value.clone(),
                },
            )?;
        }
        Ok(Some(RefCommitReceipt {
            value,
            idempotent_replay: true,
            parent_directory_synced: true,
            outcome_path,
        }))
    }

    fn prepare_branch(&self, branch: &str) -> PocResult<PathBuf> {
        validate_path_component(branch, "branch")?;
        let branch_dir = self.branches_dir().join(branch);
        std::fs::create_dir_all(branch_dir.join("prerequisites")).map_err(|source| {
            PocError::io(
                "create paired ref prerequisite directory",
                branch_dir.join("prerequisites"),
                source,
            )
        })?;
        std::fs::create_dir_all(branch_dir.join("outcomes")).map_err(|source| {
            PocError::io(
                "create paired ref outcome directory",
                branch_dir.join("outcomes"),
                source,
            )
        })?;
        create_lock_file(&branch_dir.join("LOCK"))?;
        fsync_dir(&branch_dir)?;
        fsync_dir(&self.branches_dir())?;
        Ok(branch_dir)
    }

    fn branches_dir(&self) -> PathBuf {
        self.root.join("branches")
    }
}

fn replace_head(
    branch_dir: &Path,
    value: &PairedRefValue,
    operation_id: &crate::OperationId,
    faults: &mut NamedFaultInjector,
) -> PocResult<()> {
    validate_path_component(operation_id.as_str(), "operation ID")?;
    let temporary = branch_dir.join(format!(".HEAD.{operation_id}.tmp"));
    let bytes = encoded_json(value)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|source| PocError::io("create paired ref temporary", &temporary, source))?;
    file.write_all(&bytes)
        .map_err(|source| PocError::io("write paired ref temporary", &temporary, source))?;
    file.sync_all()
        .map_err(|source| PocError::io("fsync paired ref temporary", &temporary, source))?;
    reach_real_operation(
        faults,
        NamedFaultPoint::RefAfterTempFsync,
        operation_id,
        [temporary.clone()],
        None,
        true,
    )?;
    drop(file);
    std::fs::rename(&temporary, branch_dir.join("HEAD"))
        .map_err(|source| PocError::io("replace paired ref", branch_dir.join("HEAD"), source))?;
    reach_real_operation(
        faults,
        NamedFaultPoint::RefAfterReplace,
        operation_id,
        [branch_dir.join("HEAD")],
        None,
        true,
    )?;
    fsync_dir(branch_dir)?;
    reach_real_operation(
        faults,
        NamedFaultPoint::RefAfterParentFsync,
        operation_id,
        [branch_dir.join("HEAD")],
        None,
        true,
    )
}

fn read_head(branch_dir: &Path) -> PocResult<Option<PairedRefValue>> {
    let path = branch_dir.join("HEAD");
    if !path.exists() {
        return Ok(None);
    }
    let value: PairedRefValue = read_json(&path)?;
    validate_paired_ref(&value)?;
    Ok(Some(value))
}

fn read_prerequisite(branch_dir: &Path, operation_id: &str) -> PocResult<RefPrerequisiteRecord> {
    let path = prerequisite_path(branch_dir, operation_id);
    if !path.exists() {
        return Err(PocError::RecoveryRequired(format!(
            "paired ref prerequisite is absent for operation {operation_id}"
        )));
    }
    read_json(&path)
}

fn read_outcome(branch_dir: &Path, operation_id: &str) -> PocResult<Option<RefOutcomeRecord>> {
    let path = outcome_path(branch_dir, operation_id);
    if !path.exists() {
        return Ok(None);
    }
    read_json(&path).map(Some)
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
    let file = File::open(&receipt.root_manifest).map_err(|source| {
        PocError::io(
            "open canonical root manifest",
            &receipt.root_manifest,
            source,
        )
    })?;
    file.sync_all().map_err(|source| {
        PocError::io(
            "fsync canonical root manifest",
            &receipt.root_manifest,
            source,
        )
    })
}

fn validate_prerequisite(
    prerequisite: &RefPrerequisiteRecord,
    candidate: &LocatorRefCandidate,
    locator_store: &LocatorStore,
    require_selected: bool,
) -> PocResult<()> {
    if prerequisite.schema_version != SCHEMA_VERSION || prerequisite.format != REF_FORMAT {
        return Err(PocError::Integrity(
            "unsupported paired ref prerequisite".to_owned(),
        ));
    }
    if prerequisite.candidate != *candidate
        || prerequisite.candidate_sha256 != digest_json(candidate)?
    {
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

fn prerequisite_path(branch_dir: &Path, operation_id: &str) -> PathBuf {
    branch_dir
        .join("prerequisites")
        .join(format!("{operation_id}.json"))
}

fn outcome_path(branch_dir: &Path, operation_id: &str) -> PathBuf {
    branch_dir
        .join("outcomes")
        .join(format!("{operation_id}.json"))
}

fn digest_json<T: Serialize>(value: &T) -> PocResult<String> {
    let bytes = serde_json::to_vec(value)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
}

fn encoded_json<T: Serialize>(value: &T) -> PocResult<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn create_lock_file(path: &Path) -> PocResult<()> {
    File::options()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| PocError::io("create paired ref lock", path, source))
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
