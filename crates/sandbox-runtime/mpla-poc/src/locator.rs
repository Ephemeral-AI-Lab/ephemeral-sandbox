use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{fsync_dir, read_json, write_immutable_json, FileLock};
use crate::recovery::reach_real_operation;
use crate::{
    AllocationId, LocatorDurabilityReceipt, LocatorGeneration, NamedFaultInjector, NamedFaultPoint,
    OperationId, PocError, PocResult, PublicationId, SCHEMA_VERSION,
};

const LOCATOR_FORMAT: &str = "mpla-poc-locator-v1";

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PayloadRootId(String);

impl PayloadRootId {
    pub fn parse(value: impl Into<String>) -> PocResult<Self> {
        let value = value.into();
        let valid = value.len() == 64
            && value
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
        if !valid {
            return Err(PocError::Integrity(
                "payload root must be 64 lowercase hexadecimal characters".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocatorExtent {
    pub relative_path: String,
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ForwardLocatorEntry {
    pub payload_root: PayloadRootId,
    pub allocation_id: AllocationId,
    pub owner_epoch: u64,
    pub extents: Vec<LocatorExtent>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReverseLocatorEntry {
    pub allocation_id: AllocationId,
    pub owner_epoch: u64,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub payload_roots: Vec<PayloadRootId>,
    pub accounted_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocatorDelta {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub expected_parent: Option<LocatorGeneration>,
    pub forward: Vec<ForwardLocatorEntry>,
    pub reverse: Vec<ReverseLocatorEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocatorReplacement {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub expected_parent: LocatorGeneration,
    pub payload_root: PayloadRootId,
    pub expected_source_allocation_id: AllocationId,
    pub expected_source_owner_epoch: u64,
    pub target: ForwardLocatorEntry,
    pub target_reverse: ReverseLocatorEntry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedLocatorGeneration {
    pub receipt: LocatorDurabilityReceipt,
    pub parent: Option<LocatorGeneration>,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub forward: Vec<ForwardLocatorEntry>,
    pub reverse: Vec<ReverseLocatorEntry>,
}

struct LocatorGenerationCandidate {
    operation_id: OperationId,
    publication_id: PublicationId,
    candidate_sha256: String,
    forward: Vec<ForwardLocatorEntry>,
    reverse: Vec<ReverseLocatorEntry>,
}

type GenerationCache = Arc<Mutex<BTreeMap<LocatorGeneration, SelectedLocatorGeneration>>>;

#[derive(Clone, Debug)]
pub struct LocatorStore {
    root: PathBuf,
    generation_cache: GenerationCache,
}

static GENERATION_CACHES: OnceLock<Mutex<BTreeMap<PathBuf, GenerationCache>>> = OnceLock::new();

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ForwardFile {
    schema_version: u32,
    format: String,
    generation: LocatorGeneration,
    entries: Vec<ForwardLocatorEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ReverseFile {
    schema_version: u32,
    format: String,
    generation: LocatorGeneration,
    entries: Vec<ReverseLocatorEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct GenerationManifest {
    schema_version: u32,
    format: String,
    generation: LocatorGeneration,
    parent: Option<LocatorGeneration>,
    operation_id: OperationId,
    publication_id: PublicationId,
    candidate_sha256: String,
    forward_sha256: String,
    reverse_sha256: String,
    forward_entries: u64,
    reverse_entries: u64,
    manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LocatorSelector {
    schema_version: u32,
    generation: LocatorGeneration,
    operation_id: OperationId,
    publication_id: PublicationId,
    generation_manifest_sha256: String,
    checksum_sha256: String,
}

impl LocatorStore {
    pub fn open(root: impl Into<PathBuf>) -> PocResult<Self> {
        let mut root = root.into();
        let generations_dir = root.join("generations");
        let lock_path = root.join("LOCK");
        let layout_ready = generations_dir.is_dir() && lock_path.is_file();
        if !layout_ready {
            std::fs::create_dir_all(&generations_dir).map_err(|source| {
                PocError::io(
                    "create locator generations directory",
                    &generations_dir,
                    source,
                )
            })?;
            create_lock_file(&lock_path)?;
            fsync_dir(&root)?;
        }
        if !root.is_absolute() {
            root = std::fs::canonicalize(&root)
                .map_err(|source| PocError::io("canonicalize locator root", &root, source))?;
        }
        let generation_cache = {
            let mut caches = GENERATION_CACHES
                .get_or_init(|| Mutex::new(BTreeMap::new()))
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            caches
                .entry(root.clone())
                .or_insert_with(|| Arc::new(Mutex::new(BTreeMap::new())))
                .clone()
        };
        Ok(Self {
            root,
            generation_cache,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn selected(&self) -> PocResult<Option<SelectedLocatorGeneration>> {
        let _lock = FileLock::shared(&self.lock_path())?;
        self.selected_locked()
    }

    pub fn resolve(&self, payload_root: &PayloadRootId) -> PocResult<Option<ForwardLocatorEntry>> {
        let selected = self.selected()?;
        Ok(selected.and_then(|generation| {
            generation
                .forward
                .into_iter()
                .find(|entry| &entry.payload_root == payload_root)
        }))
    }

    pub fn install(
        &self,
        delta: &LocatorDelta,
        faults: &mut NamedFaultInjector,
    ) -> PocResult<LocatorDurabilityReceipt> {
        validate_delta(delta)?;
        let _lock = FileLock::exclusive(&self.lock_path())?;
        let selected = self.selected_locked()?;
        if let Some(current) = selected.as_ref() {
            if generation_contains_delta(current, delta)? {
                fsync_dir(&self.root)?;
                return Ok(current.receipt.clone());
            }
        }
        if let Some(expected_parent) = delta.expected_parent {
            let observed_parent = selected.as_ref().map(|current| current.receipt.generation);
            if observed_parent != Some(expected_parent) {
                return Err(PocError::OwnerConflict(format!(
                    "locator expected parent {expected_parent}, observed {}",
                    observed_parent.map_or_else(|| "none".to_owned(), |value| value.to_string())
                )));
            }
        }

        let mut forward = selected
            .as_ref()
            .map_or_else(Vec::new, |current| current.forward.clone());
        let mut reverse = selected
            .as_ref()
            .map_or_else(Vec::new, |current| current.reverse.clone());
        merge_forward(&mut forward, &delta.forward)?;
        merge_reverse(&mut reverse, &delta.reverse)?;
        normalize_and_validate(&mut forward, &mut reverse)?;
        self.persist_generation(
            selected.as_ref(),
            LocatorGenerationCandidate {
                operation_id: delta.operation_id.clone(),
                publication_id: delta.publication_id.clone(),
                candidate_sha256: digest_json(delta)?,
                forward,
                reverse,
            },
            faults,
        )
    }

    pub fn replace_exact(
        &self,
        replacement: &LocatorReplacement,
        faults: &mut NamedFaultInjector,
    ) -> PocResult<LocatorDurabilityReceipt> {
        validate_replacement(replacement)?;
        let _lock = FileLock::exclusive(&self.lock_path())?;
        let selected = self.selected_locked()?.ok_or_else(|| {
            PocError::OwnerConflict("locator replacement has no selected source".to_owned())
        })?;
        if generation_contains_replacement(&selected, replacement) {
            fsync_dir(&self.root)?;
            return Ok(selected.receipt);
        }
        if selected.receipt.generation != replacement.expected_parent {
            return Err(PocError::OwnerConflict(format!(
                "locator replacement expected parent {}, observed {}",
                replacement.expected_parent, selected.receipt.generation
            )));
        }

        let source_index = selected
            .forward
            .iter()
            .position(|entry| entry.payload_root == replacement.payload_root)
            .ok_or_else(|| {
                PocError::OwnerConflict(format!(
                    "locator source root {} is not selected",
                    replacement.payload_root.as_str()
                ))
            })?;
        let source = &selected.forward[source_index];
        if source.allocation_id != replacement.expected_source_allocation_id
            || source.owner_epoch != replacement.expected_source_owner_epoch
        {
            return Err(PocError::OwnerConflict(format!(
                "locator source root {} changed allocation or owner epoch",
                replacement.payload_root.as_str()
            )));
        }
        if selected
            .reverse
            .iter()
            .any(|entry| entry.allocation_id == replacement.target.allocation_id)
        {
            return Err(PocError::OwnerConflict(format!(
                "locator target allocation {} is already selected",
                replacement.target.allocation_id
            )));
        }

        let mut forward = selected.forward.clone();
        forward[source_index] = replacement.target.clone();
        let source_reverse_index = selected
            .reverse
            .iter()
            .position(|entry| entry.allocation_id == replacement.expected_source_allocation_id)
            .ok_or_else(|| {
                PocError::Integrity(format!(
                    "locator source allocation {} has no reverse entry",
                    replacement.expected_source_allocation_id
                ))
            })?;
        let mut reverse = selected.reverse.clone();
        let source_reverse = &mut reverse[source_reverse_index];
        if source_reverse.owner_epoch != replacement.expected_source_owner_epoch
            || !source_reverse
                .payload_roots
                .contains(&replacement.payload_root)
        {
            return Err(PocError::Integrity(
                "locator source reverse entry disagrees with the exact replacement".to_owned(),
            ));
        }
        source_reverse
            .payload_roots
            .retain(|root| root != &replacement.payload_root);
        if source_reverse.payload_roots.is_empty() {
            reverse.remove(source_reverse_index);
        }
        reverse.push(replacement.target_reverse.clone());
        normalize_and_validate(&mut forward, &mut reverse)?;
        self.persist_generation(
            Some(&selected),
            LocatorGenerationCandidate {
                operation_id: replacement.operation_id.clone(),
                publication_id: replacement.publication_id.clone(),
                candidate_sha256: digest_json(replacement)?,
                forward,
                reverse,
            },
            faults,
        )
    }

    fn persist_generation(
        &self,
        selected: Option<&SelectedLocatorGeneration>,
        candidate: LocatorGenerationCandidate,
        faults: &mut NamedFaultInjector,
    ) -> PocResult<LocatorDurabilityReceipt> {
        let generation = selected.map_or(Ok(LocatorGeneration::INITIAL), |current| {
            current.receipt.generation.checked_next()
        })?;
        let forward_file = ForwardFile {
            schema_version: SCHEMA_VERSION,
            format: LOCATOR_FORMAT.to_owned(),
            generation,
            entries: candidate.forward,
        };
        let reverse_file = ReverseFile {
            schema_version: SCHEMA_VERSION,
            format: LOCATOR_FORMAT.to_owned(),
            generation,
            entries: candidate.reverse,
        };
        let forward_sha256 = digest_json(&forward_file)?;
        let reverse_sha256 = digest_json(&reverse_file)?;
        let generation_dir = self.generation_dir(generation);
        std::fs::create_dir_all(&generation_dir).map_err(|source| {
            PocError::io(
                "create locator generation directory",
                &generation_dir,
                source,
            )
        })?;
        write_immutable_json(&generation_dir.join("forward.json"), &forward_file)?;
        reach_real_operation(
            faults,
            NamedFaultPoint::LocatorAfterForward,
            &candidate.operation_id,
            [generation_dir.join("forward.json")],
            None,
            true,
        )?;
        write_immutable_json(&generation_dir.join("reverse.json"), &reverse_file)?;
        reach_real_operation(
            faults,
            NamedFaultPoint::LocatorAfterReverse,
            &candidate.operation_id,
            [
                generation_dir.join("forward.json"),
                generation_dir.join("reverse.json"),
            ],
            None,
            true,
        )?;

        let mut manifest = GenerationManifest {
            schema_version: SCHEMA_VERSION,
            format: LOCATOR_FORMAT.to_owned(),
            generation,
            parent: selected.map(|current| current.receipt.generation),
            operation_id: candidate.operation_id.clone(),
            publication_id: candidate.publication_id.clone(),
            candidate_sha256: candidate.candidate_sha256,
            forward_sha256: forward_sha256.clone(),
            reverse_sha256: reverse_sha256.clone(),
            forward_entries: usize_to_u64(forward_file.entries.len())?,
            reverse_entries: usize_to_u64(reverse_file.entries.len())?,
            manifest_sha256: String::new(),
        };
        manifest.manifest_sha256 = digest_json(&manifest)?;
        write_immutable_json(&generation_dir.join("MANIFEST.json"), &manifest)?;
        reach_real_operation(
            faults,
            NamedFaultPoint::LocatorAfterManifestFsync,
            &candidate.operation_id,
            [
                generation_dir.join("forward.json"),
                generation_dir.join("reverse.json"),
                generation_dir.join("MANIFEST.json"),
            ],
            None,
            true,
        )?;

        let mut selector = LocatorSelector {
            schema_version: SCHEMA_VERSION,
            generation,
            operation_id: candidate.operation_id,
            publication_id: candidate.publication_id,
            generation_manifest_sha256: manifest.manifest_sha256.clone(),
            checksum_sha256: String::new(),
        };
        selector.checksum_sha256 = digest_json(&selector)?;
        self.replace_selector(&selector, faults)?;
        Ok(receipt_from_manifest(&manifest, true))
    }

    pub fn validate_receipt(&self, receipt: &LocatorDurabilityReceipt) -> PocResult<()> {
        if !receipt.forward_durable
            || !receipt.reverse_durable
            || !receipt.manifest_durable
            || !receipt.selector_parent_synced
        {
            return Err(PocError::Integrity(
                "locator durability receipt is incomplete".to_owned(),
            ));
        }
        let selected = self.selected()?.ok_or_else(|| {
            PocError::RecoveryRequired("locator selector is not durable".to_owned())
        })?;
        if selected.receipt != *receipt {
            return Err(PocError::RecoveryRequired(format!(
                "locator generation {} is not the selected complete generation",
                receipt.generation
            )));
        }
        Ok(())
    }

    pub fn validate_generation_receipt(&self, receipt: &LocatorDurabilityReceipt) -> PocResult<()> {
        if !receipt.forward_durable
            || !receipt.reverse_durable
            || !receipt.manifest_durable
            || !receipt.selector_parent_synced
        {
            return Err(PocError::Integrity(
                "locator durability receipt is incomplete".to_owned(),
            ));
        }
        let _lock = FileLock::shared(&self.lock_path())?;
        let generation = self.load_generation(receipt.generation)?;
        if generation.receipt != *receipt {
            return Err(PocError::RecoveryRequired(format!(
                "locator generation {} durability receipt mismatch",
                receipt.generation
            )));
        }
        Ok(())
    }

    fn selected_locked(&self) -> PocResult<Option<SelectedLocatorGeneration>> {
        let selector_path = self.selector_path();
        if !selector_path.exists() {
            return Ok(None);
        }
        let selector: LocatorSelector = read_json(&selector_path)?;
        validate_selector(&selector)?;
        let selected = self.load_generation(selector.generation)?;
        if selector.operation_id != selected.operation_id
            || selector.publication_id != selected.publication_id
            || selector.generation_manifest_sha256 != selected.receipt.generation_manifest_sha256
        {
            return Err(PocError::RecoveryRequired(
                "locator selector disagrees with its generation manifest".to_owned(),
            ));
        }
        Ok(Some(selected))
    }

    fn load_generation(
        &self,
        generation: LocatorGeneration,
    ) -> PocResult<SelectedLocatorGeneration> {
        if let Some(selected) = self
            .generation_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&generation)
            .cloned()
        {
            return Ok(selected);
        }
        let generation_dir = self.generation_dir(generation);
        let forward: ForwardFile = read_json(&generation_dir.join("forward.json"))?;
        let reverse: ReverseFile = read_json(&generation_dir.join("reverse.json"))?;
        let manifest: GenerationManifest = read_json(&generation_dir.join("MANIFEST.json"))?;
        validate_generation_files(&forward, &reverse, &manifest)?;
        let selected = SelectedLocatorGeneration {
            receipt: receipt_from_manifest(&manifest, true),
            parent: manifest.parent,
            operation_id: manifest.operation_id,
            publication_id: manifest.publication_id,
            forward: forward.entries,
            reverse: reverse.entries,
        };
        self.generation_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(generation, selected.clone());
        Ok(selected)
    }

    fn replace_selector(
        &self,
        selector: &LocatorSelector,
        faults: &mut NamedFaultInjector,
    ) -> PocResult<()> {
        validate_path_component(selector.operation_id.as_str(), "operation ID")?;
        let temporary = self
            .root
            .join(format!(".CURRENT.{}.tmp", selector.operation_id.as_str()));
        let bytes = encoded_json(selector)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| {
                PocError::io("create locator selector temporary", &temporary, source)
            })?;
        file.write_all(&bytes).map_err(|source| {
            PocError::io("write locator selector temporary", &temporary, source)
        })?;
        file.sync_all().map_err(|source| {
            PocError::io("fsync locator selector temporary", &temporary, source)
        })?;
        drop(file);
        std::fs::rename(&temporary, self.selector_path()).map_err(|source| {
            PocError::io("replace locator selector", self.selector_path(), source)
        })?;
        reach_real_operation(
            faults,
            NamedFaultPoint::LocatorAfterSelectorRename,
            &selector.operation_id,
            [self.selector_path()],
            None,
            true,
        )?;
        fsync_dir(&self.root)?;
        reach_real_operation(
            faults,
            NamedFaultPoint::LocatorAfterSelectorDirFsync,
            &selector.operation_id,
            [self.selector_path()],
            None,
            true,
        )
    }

    fn generations_dir(&self) -> PathBuf {
        self.root.join("generations")
    }

    fn generation_dir(&self, generation: LocatorGeneration) -> PathBuf {
        self.generations_dir()
            .join(format!("{:020}", generation.get()))
    }

    fn selector_path(&self) -> PathBuf {
        self.root.join("CURRENT")
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join("LOCK")
    }
}

fn validate_delta(delta: &LocatorDelta) -> PocResult<()> {
    if delta.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(format!(
            "unsupported locator delta schema {}",
            delta.schema_version
        )));
    }
    validate_path_component(delta.operation_id.as_str(), "operation ID")?;
    if delta.forward.is_empty() || delta.reverse.is_empty() {
        return Err(PocError::Integrity(
            "locator delta must contain forward and reverse entries".to_owned(),
        ));
    }
    let mut forward = delta.forward.clone();
    let mut reverse = delta.reverse.clone();
    normalize_and_validate(&mut forward, &mut reverse)
}

fn validate_replacement(replacement: &LocatorReplacement) -> PocResult<()> {
    if replacement.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(format!(
            "unsupported locator replacement schema {}",
            replacement.schema_version
        )));
    }
    validate_path_component(replacement.operation_id.as_str(), "operation ID")?;
    if replacement.expected_source_owner_epoch == 0 {
        return Err(PocError::Integrity(
            "locator replacement source owner epoch must be non-zero".to_owned(),
        ));
    }
    if replacement.expected_source_allocation_id == replacement.target.allocation_id {
        return Err(PocError::Integrity(
            "locator replacement target must use a distinct allocation".to_owned(),
        ));
    }
    if replacement.target.payload_root != replacement.payload_root
        || replacement.target_reverse.allocation_id != replacement.target.allocation_id
        || replacement.target_reverse.owner_epoch != replacement.target.owner_epoch
        || replacement.target_reverse.operation_id != replacement.operation_id
        || replacement.target_reverse.publication_id != replacement.publication_id
        || replacement.target_reverse.payload_roots != [replacement.payload_root.clone()]
    {
        return Err(PocError::Integrity(
            "locator replacement target forward and reverse entries disagree".to_owned(),
        ));
    }
    let mut forward = vec![replacement.target.clone()];
    let mut reverse = vec![replacement.target_reverse.clone()];
    normalize_and_validate(&mut forward, &mut reverse)
}

fn validate_generation_files(
    forward: &ForwardFile,
    reverse: &ReverseFile,
    manifest: &GenerationManifest,
) -> PocResult<()> {
    if forward.schema_version != SCHEMA_VERSION
        || reverse.schema_version != SCHEMA_VERSION
        || manifest.schema_version != SCHEMA_VERSION
        || forward.format != LOCATOR_FORMAT
        || reverse.format != LOCATOR_FORMAT
        || manifest.format != LOCATOR_FORMAT
    {
        return Err(PocError::Integrity(
            "unsupported selected locator generation".to_owned(),
        ));
    }
    if forward.generation != manifest.generation || reverse.generation != manifest.generation {
        return Err(PocError::RecoveryRequired(
            "locator manifest resolves a mixed generation".to_owned(),
        ));
    }
    if digest_json(forward)? != manifest.forward_sha256
        || digest_json(reverse)? != manifest.reverse_sha256
    {
        return Err(PocError::RecoveryRequired(
            "selected locator forward/reverse checksum mismatch".to_owned(),
        ));
    }
    let mut expected_manifest = manifest.clone();
    let observed_manifest_sha256 = expected_manifest.manifest_sha256.clone();
    expected_manifest.manifest_sha256.clear();
    if digest_json(&expected_manifest)? != observed_manifest_sha256 {
        return Err(PocError::RecoveryRequired(
            "locator generation manifest checksum mismatch".to_owned(),
        ));
    }
    if usize_to_u64(forward.entries.len())? != manifest.forward_entries
        || usize_to_u64(reverse.entries.len())? != manifest.reverse_entries
    {
        return Err(PocError::RecoveryRequired(
            "selected locator manifest entry count mismatch".to_owned(),
        ));
    }
    let mut normalized_forward = forward.entries.clone();
    let mut normalized_reverse = reverse.entries.clone();
    normalize_and_validate(&mut normalized_forward, &mut normalized_reverse)?;
    if normalized_forward != forward.entries || normalized_reverse != reverse.entries {
        return Err(PocError::Integrity(
            "selected locator generation is not canonical".to_owned(),
        ));
    }
    Ok(())
}

fn validate_selector(selector: &LocatorSelector) -> PocResult<()> {
    let mut expected = selector.clone();
    let observed = expected.checksum_sha256.clone();
    expected.checksum_sha256.clear();
    if digest_json(&expected)? != observed {
        return Err(PocError::RecoveryRequired(
            "locator selector checksum mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn generation_contains_delta(
    selected: &SelectedLocatorGeneration,
    delta: &LocatorDelta,
) -> PocResult<bool> {
    for entry in &delta.forward {
        let Some(current) = selected
            .forward
            .iter()
            .find(|current| current.payload_root == entry.payload_root)
        else {
            return Ok(false);
        };
        if current != entry {
            return Err(PocError::Integrity(format!(
                "payload root {} resolves to conflicting physical allocation",
                entry.payload_root.as_str()
            )));
        }
    }
    for entry in &delta.reverse {
        let Some(current) = selected
            .reverse
            .iter()
            .find(|current| current.allocation_id == entry.allocation_id)
        else {
            return Ok(false);
        };
        if current.owner_epoch != entry.owner_epoch
            || current.operation_id != entry.operation_id
            || current.publication_id != entry.publication_id
            || current.accounted_bytes != entry.accounted_bytes
            || !entry
                .payload_roots
                .iter()
                .all(|root| current.payload_roots.contains(root))
        {
            return Err(PocError::Integrity(format!(
                "allocation {} has conflicting reverse locator attribution",
                entry.allocation_id
            )));
        }
    }
    Ok(true)
}

fn generation_contains_replacement(
    selected: &SelectedLocatorGeneration,
    replacement: &LocatorReplacement,
) -> bool {
    selected.operation_id == replacement.operation_id
        && selected.publication_id == replacement.publication_id
        && selected
            .forward
            .iter()
            .find(|entry| entry.payload_root == replacement.payload_root)
            == Some(&replacement.target)
        && selected
            .reverse
            .iter()
            .find(|entry| entry.allocation_id == replacement.target.allocation_id)
            == Some(&replacement.target_reverse)
}

fn merge_forward(
    current: &mut Vec<ForwardLocatorEntry>,
    delta: &[ForwardLocatorEntry],
) -> PocResult<()> {
    let mut by_root: BTreeMap<PayloadRootId, ForwardLocatorEntry> = current
        .drain(..)
        .map(|entry| (entry.payload_root.clone(), entry))
        .collect();
    for entry in delta {
        match by_root.get(&entry.payload_root) {
            Some(existing) if existing != entry => {
                return Err(PocError::Integrity(format!(
                    "payload root {} already has another locator",
                    entry.payload_root.as_str()
                )));
            }
            Some(_) => {}
            None => {
                by_root.insert(entry.payload_root.clone(), entry.clone());
            }
        }
    }
    current.extend(by_root.into_values());
    Ok(())
}

fn merge_reverse(
    current: &mut Vec<ReverseLocatorEntry>,
    delta: &[ReverseLocatorEntry],
) -> PocResult<()> {
    let mut by_allocation: BTreeMap<AllocationId, ReverseLocatorEntry> = current
        .drain(..)
        .map(|entry| (entry.allocation_id.clone(), entry))
        .collect();
    for entry in delta {
        match by_allocation.get_mut(&entry.allocation_id) {
            Some(existing) => {
                if existing.owner_epoch != entry.owner_epoch
                    || existing.operation_id != entry.operation_id
                    || existing.publication_id != entry.publication_id
                    || existing.accounted_bytes != entry.accounted_bytes
                {
                    return Err(PocError::Integrity(format!(
                        "allocation {} has conflicting reverse locator attribution",
                        entry.allocation_id
                    )));
                }
                existing
                    .payload_roots
                    .extend(entry.payload_roots.iter().cloned());
                existing.payload_roots.sort();
                existing.payload_roots.dedup();
            }
            None => {
                by_allocation.insert(entry.allocation_id.clone(), entry.clone());
            }
        }
    }
    current.extend(by_allocation.into_values());
    Ok(())
}

fn normalize_and_validate(
    forward: &mut [ForwardLocatorEntry],
    reverse: &mut [ReverseLocatorEntry],
) -> PocResult<()> {
    for entry in forward.iter_mut() {
        if entry.owner_epoch == 0 {
            return Err(PocError::Integrity(
                "forward locator owner epoch must be non-zero".to_owned(),
            ));
        }
        entry.extents.sort_by(|left, right| {
            (&left.relative_path, left.offset, left.length).cmp(&(
                &right.relative_path,
                right.offset,
                right.length,
            ))
        });
        let mut prior_end: BTreeMap<&str, u64> = BTreeMap::new();
        for extent in &entry.extents {
            validate_relative_path(&extent.relative_path)?;
            let end = extent
                .offset
                .checked_add(extent.length)
                .ok_or_else(|| PocError::Integrity("locator extent offset overflow".to_owned()))?;
            if extent.length == 0
                || prior_end
                    .get(extent.relative_path.as_str())
                    .is_some_and(|prior| extent.offset < *prior)
            {
                return Err(PocError::Integrity(
                    "locator extents must be nonempty and non-overlapping".to_owned(),
                ));
            }
            prior_end.insert(&extent.relative_path, end);
        }
    }
    forward.sort_by(|left, right| left.payload_root.cmp(&right.payload_root));
    if forward
        .windows(2)
        .any(|pair| pair[0].payload_root == pair[1].payload_root)
    {
        return Err(PocError::Integrity(
            "duplicate forward locator payload root".to_owned(),
        ));
    }

    for entry in reverse.iter_mut() {
        if entry.owner_epoch == 0 || entry.accounted_bytes == 0 {
            return Err(PocError::Integrity(
                "reverse locator ownership/accounting must be non-zero".to_owned(),
            ));
        }
        entry.payload_roots.sort();
        entry.payload_roots.dedup();
        if entry.payload_roots.is_empty() {
            return Err(PocError::Integrity(
                "reverse locator must inventory at least one payload root".to_owned(),
            ));
        }
    }
    reverse.sort_by(|left, right| left.allocation_id.cmp(&right.allocation_id));
    if reverse
        .windows(2)
        .any(|pair| pair[0].allocation_id == pair[1].allocation_id)
    {
        return Err(PocError::Integrity(
            "duplicate reverse locator allocation".to_owned(),
        ));
    }

    let reverse_by_allocation: BTreeMap<_, _> = reverse
        .iter()
        .map(|entry| (entry.allocation_id.clone(), entry))
        .collect();
    let mut forward_by_allocation: BTreeMap<AllocationId, BTreeSet<PayloadRootId>> =
        BTreeMap::new();
    for entry in forward.iter() {
        let reverse_entry = reverse_by_allocation
            .get(&entry.allocation_id)
            .ok_or_else(|| {
                PocError::Integrity(format!(
                    "forward locator {} has no reverse attribution",
                    entry.payload_root.as_str()
                ))
            })?;
        if reverse_entry.owner_epoch != entry.owner_epoch
            || !reverse_entry.payload_roots.contains(&entry.payload_root)
        {
            return Err(PocError::Integrity(format!(
                "forward locator {} disagrees with reverse ownership",
                entry.payload_root.as_str()
            )));
        }
        forward_by_allocation
            .entry(entry.allocation_id.clone())
            .or_default()
            .insert(entry.payload_root.clone());
    }
    for entry in reverse.iter() {
        let observed = forward_by_allocation
            .get(&entry.allocation_id)
            .ok_or_else(|| {
                PocError::Integrity(format!(
                    "reverse locator {} has no forward resolution",
                    entry.allocation_id
                ))
            })?;
        let expected: BTreeSet<_> = entry.payload_roots.iter().cloned().collect();
        if observed != &expected {
            return Err(PocError::Integrity(format!(
                "reverse locator {} does not exactly match forward roots",
                entry.allocation_id
            )));
        }
    }
    Ok(())
}

fn receipt_from_manifest(
    manifest: &GenerationManifest,
    selector_parent_synced: bool,
) -> LocatorDurabilityReceipt {
    LocatorDurabilityReceipt {
        generation: manifest.generation,
        forward_manifest_sha256: manifest.forward_sha256.clone(),
        reverse_manifest_sha256: manifest.reverse_sha256.clone(),
        generation_manifest_sha256: manifest.manifest_sha256.clone(),
        forward_durable: true,
        reverse_durable: true,
        manifest_durable: true,
        selector_parent_synced,
    }
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
        .map_err(|source| PocError::io("create locator lock", path, source))
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

fn validate_relative_path(path: &str) -> PocResult<()> {
    let candidate = Path::new(path);
    if path.is_empty()
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(PocError::Integrity(format!(
            "invalid locator relative path: {path}"
        )));
    }
    Ok(())
}

fn usize_to_u64(value: usize) -> PocResult<u64> {
    u64::try_from(value)
        .map_err(|_| PocError::Integrity("locator entry count does not fit u64".to_owned()))
}
