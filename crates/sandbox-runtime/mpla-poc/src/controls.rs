use std::collections::VecDeque;
use std::fs::{self, DirEntry, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sandbox_runtime_layerstack::service::{
    materialize_hidden_candidate, CandidateMaterializationDisposition,
};
use sandbox_runtime_layerstack::{HiddenValidationPublication, LayerChange, LayerPath, LayerStack};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{unix_time_ms, PocError, PocResult, SCHEMA_VERSION};

const HASH_BUFFER_BYTES: usize = 32 * 1024;
const CATALOG_MAX_BYTES: u64 = 4 * 1024 * 1024;
const EXPORTER_MAX_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_CONTROL_ENTRIES: u64 = 16 * 1024;
const HARD_MAX_CONTROL_LOGICAL_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const HARD_MAX_CONTROL_PATH_BYTES: u64 = 64 * 1024;
const SOURCE_MANIFEST_DOMAIN: &[u8] = b"EOS-MPLA-CONTROL-SOURCE-V1\0";
const CATALOG_BINDING_DOMAIN: &[u8] = b"EOS-MPLA-CATALOG-BINDING-V1\0";
pub const MATCHED_PUBLICATION_START_BOUNDARY: &str =
    "immediately before closing publication admission";
pub const MATCHED_PUBLICATION_STOP_BOUNDARY: &str = "durable hidden root and closed session";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlCollectionLimits {
    pub max_entries: u64,
    pub max_logical_bytes: u64,
    pub max_path_bytes: u64,
}

impl Default for ControlCollectionLimits {
    fn default() -> Self {
        Self {
            max_entries: 8 * 1024,
            max_logical_bytes: HARD_MAX_CONTROL_LOGICAL_BYTES,
            max_path_bytes: 4 * 1024,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlSourceProfile {
    pub source_root: PathBuf,
    pub entries: u64,
    pub directories: u64,
    pub regular_files: u64,
    pub symlinks: u64,
    pub logical_bytes: u64,
    pub source_manifest_sha256: String,
}

#[derive(Clone, Debug)]
pub struct ControlChangeSet {
    pub changes: Vec<LayerChange>,
    pub profile: ControlSourceProfile,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlCatalogFacts {
    pub publish_workspace_session: bool,
    pub activate_workspace_session: bool,
    pub fork_workspace_session: bool,
    pub rollback_workspace_session: bool,
    pub squash_layerstacks: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogBinding {
    pub schema_version: u32,
    pub kind: String,
    pub bound_unix_ms: u64,
    pub build_commit: String,
    pub exporter_path: PathBuf,
    pub exporter_sha256: String,
    pub catalog_path: PathBuf,
    pub catalog_sha256: String,
    pub binding_id: String,
    pub facts: ControlCatalogFacts,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlIntent {
    ClosingPublication,
    ColdActivation,
    SameKeyActivation,
    Fork,
    Rollback,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCacheMatch {
    NotApplicable,
    Matched,
    Mismatched,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlBoundary {
    pub candidate_start: String,
    pub candidate_stop: String,
    pub current_i2_start: String,
    pub current_i2_stop: String,
    pub same_fixture: bool,
    pub same_intent: bool,
    pub same_durability: bool,
    pub same_readiness: bool,
    pub cache_state: ControlCacheMatch,
    pub unknown_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlVerdict {
    Matched,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlApiCoverage {
    PublicIntentProgrammaticCurrentI2,
    ProgrammaticCurrentControl,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogCoverageReceipt {
    pub classification: ControlApiCoverage,
    pub product_operation: String,
    pub product_operation_present: bool,
    pub direct_control_api: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MonotonicClock {
    MonotonicRaw,
    Monotonic,
}

#[derive(Debug)]
pub struct MonotonicTimer {
    clock: MonotonicClock,
    started_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MonotonicSpan {
    pub clock: MonotonicClock,
    pub started_ns: u64,
    pub finished_ns: u64,
    pub elapsed_ns: u64,
}

impl MonotonicTimer {
    pub fn start() -> PocResult<Self> {
        let (clock, started_ns) = monotonic_now_ns()?;
        Ok(Self { clock, started_ns })
    }

    pub fn finish(self) -> PocResult<MonotonicSpan> {
        let (_, finished_ns) = monotonic_now_ns()?;
        monotonic_span(self.clock, self.started_ns, finished_ns)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlPublicationOutcome {
    pub correlation_id: String,
    pub candidate_generation: u64,
    pub matched: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlSelectionKey {
    pub materialization_id: String,
    pub generation: u64,
    pub fence: u64,
    pub native_tree_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlMaterializationOutcome {
    pub disposition: String,
    pub materialization_id: String,
    pub root_id: String,
    pub attribution_root_id: String,
    pub backend_kind: String,
    pub backend_format_version: u16,
    pub target_profile: String,
    pub generation: u64,
    pub fence: u64,
    pub manifest_sha256: String,
    pub carrier_path: PathBuf,
    pub native_tree_sha256: String,
    pub build_operation_id: String,
    pub maximum_buffer_bytes: Option<u64>,
}

impl ControlMaterializationOutcome {
    #[must_use]
    pub fn selection_key(&self) -> ControlSelectionKey {
        ControlSelectionKey {
            materialization_id: self.materialization_id.clone(),
            generation: self.generation,
            fence: self.fence,
            native_tree_sha256: self.native_tree_sha256.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalReadinessReceipt {
    pub probe: String,
    pub passed: bool,
    pub observed: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlOperationReceipt {
    pub schema_version: u32,
    pub implementation: String,
    pub intent: ControlIntent,
    pub catalog_binding_id: String,
    pub coverage: CatalogCoverageReceipt,
    pub boundary: ControlBoundary,
    pub verdict: ControlVerdict,
    pub started_unix_ms: u64,
    pub span: MonotonicSpan,
    pub source: Option<ControlSourceProfile>,
    pub publication: Option<ControlPublicationOutcome>,
    pub materialization: Option<ControlMaterializationOutcome>,
    pub readiness: Option<ExternalReadinessReceipt>,
}

#[derive(Clone, Debug)]
pub struct CurrentI2ClosingRequest {
    pub state_root: PathBuf,
    pub publication_id: [u8; 16],
    pub public_root_hash: String,
    pub catalog_binding: CatalogBinding,
    pub boundary: ControlBoundary,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCacheExpectation {
    ColdBuilt,
    SameKeyReused,
    NaturallyProduced,
}

#[derive(Clone, Debug)]
pub struct CurrentI2MaterializationRequest {
    pub state_root: PathBuf,
    pub intent: ControlIntent,
    pub timeout: Duration,
    pub cache_expectation: ControlCacheExpectation,
    pub expected_selection: Option<ControlSelectionKey>,
    pub catalog_binding: CatalogBinding,
    pub boundary: ControlBoundary,
}

pub fn bind_product_catalog(
    exporter_path: &Path,
    catalog_path: &Path,
    build_commit: &str,
) -> PocResult<CatalogBinding> {
    validate_build_commit(build_commit)?;
    let exporter_sha256 = digest_regular_file(exporter_path, EXPORTER_MAX_BYTES)?;
    let catalog_sha256 = digest_regular_file(catalog_path, CATALOG_MAX_BYTES)?;
    let catalog = read_catalog(catalog_path)?;
    let schema_version = catalog
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| PocError::Integrity("product catalog lacks schema_version".to_owned()))?;
    if schema_version != 1 {
        return Err(PocError::Integrity(format!(
            "unsupported product catalog schema_version: {schema_version}"
        )));
    }
    let kind = catalog
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| PocError::Integrity("product catalog lacks kind".to_owned()))?;
    if kind != "ephemeral_sandbox_product_catalog" {
        return Err(PocError::Integrity(format!(
            "unexpected product catalog kind: {kind}"
        )));
    }
    let facts = ControlCatalogFacts {
        publish_workspace_session: operation_present(
            &catalog,
            "runtime",
            "publish_workspace_session",
        )?,
        activate_workspace_session: operation_present(
            &catalog,
            "runtime",
            "activate_workspace_session",
        )?,
        fork_workspace_session: operation_present(&catalog, "runtime", "fork_workspace_session")?,
        rollback_workspace_session: operation_present(
            &catalog,
            "runtime",
            "rollback_workspace_session",
        )?,
        squash_layerstacks: operation_present(&catalog, "manager", "squash_layerstacks")?,
    };
    let binding_id = catalog_binding_id(build_commit, &exporter_sha256, &catalog_sha256, &facts)?;
    Ok(CatalogBinding {
        schema_version: SCHEMA_VERSION,
        kind: "mpla-product-catalog-binding-v1".to_owned(),
        bound_unix_ms: unix_time_ms()?,
        build_commit: build_commit.to_owned(),
        exporter_path: exporter_path.to_path_buf(),
        exporter_sha256,
        catalog_path: catalog_path.to_path_buf(),
        catalog_sha256,
        binding_id,
        facts,
    })
}

pub fn collect_control_changes(
    source_root: &Path,
    limits: &ControlCollectionLimits,
) -> PocResult<ControlChangeSet> {
    validate_collection_limits(limits)?;
    require_real_directory(source_root, "control source root")?;
    let source_root = fs::canonicalize(source_root)
        .map_err(|source| PocError::io("canonicalize control source root", source_root, source))?;
    let mut pending = VecDeque::from([source_root.to_path_buf()]);
    let mut changes = Vec::new();
    let mut profile = ControlSourceProfile {
        source_root: source_root.to_path_buf(),
        entries: 0,
        directories: 1,
        regular_files: 0,
        symlinks: 0,
        logical_bytes: 0,
        source_manifest_sha256: String::new(),
    };
    let mut manifest = Sha256::new();
    manifest.update(SOURCE_MANIFEST_DOMAIN);
    while let Some(directory) = pending.pop_front() {
        let remaining = limits
            .max_entries
            .checked_sub(profile.entries)
            .ok_or_else(|| PocError::Integrity("control entry count overflow".to_owned()))?;
        let entries = read_sorted_entries(&directory, remaining)?;
        for entry in entries {
            profile.entries = checked_add(profile.entries, 1, "control entries")?;
            let path = entry.path();
            let relative = relative_layer_path(&source_root, &path, limits.max_path_bytes)?;
            let layer_path = LayerPath::parse(&relative)
                .map_err(|error| PocError::Integrity(error.to_string()))?;
            let file_type = entry
                .file_type()
                .map_err(|source| PocError::io("read control entry type", &path, source))?;
            if file_type.is_dir() {
                profile.directories = checked_add(profile.directories, 1, "control directories")?;
                hash_manifest_header(&mut manifest, b"directory", &relative, 0);
                changes.push(LayerChange::Directory { path: layer_path });
                pending.push_back(path);
            } else if file_type.is_file() {
                let size = entry
                    .metadata()
                    .map_err(|source| PocError::io("stat control file", &path, source))?
                    .len();
                profile.regular_files =
                    checked_add(profile.regular_files, 1, "control regular files")?;
                profile.logical_bytes =
                    checked_add(profile.logical_bytes, size, "control logical bytes")?;
                if profile.logical_bytes > limits.max_logical_bytes {
                    return Err(PocError::Integrity(format!(
                        "control source exceeds logical-byte limit: {} > {}",
                        profile.logical_bytes, limits.max_logical_bytes
                    )));
                }
                hash_manifest_header(&mut manifest, b"regular", &relative, size);
                hash_file_contents(&path, &mut manifest)?;
                changes.push(LayerChange::WriteFile {
                    path: layer_path,
                    source_path: path,
                    size,
                });
            } else if file_type.is_symlink() {
                let target = fs::read_link(&path)
                    .map_err(|source| PocError::io("read control symlink", &path, source))?;
                let target = target.to_str().ok_or_else(|| {
                    PocError::Integrity(format!(
                        "control symlink target is not UTF-8: {}",
                        path.display()
                    ))
                })?;
                profile.symlinks = checked_add(profile.symlinks, 1, "control symlinks")?;
                hash_manifest_header(
                    &mut manifest,
                    b"symlink",
                    &relative,
                    u64::try_from(target.len()).expect("symlink target length fits u64"),
                );
                hash_field(&mut manifest, target.as_bytes());
                changes.push(LayerChange::Symlink {
                    path: layer_path,
                    source_path: target.to_owned(),
                });
            } else {
                return Err(PocError::Integrity(format!(
                    "unsupported current-I2 control source entry: {}",
                    path.display()
                )));
            }
        }
    }
    profile.source_manifest_sha256 = format!("{:x}", manifest.finalize());
    Ok(ControlChangeSet { changes, profile })
}

pub fn run_current_i2_closing(
    request: &CurrentI2ClosingRequest,
    changes: &ControlChangeSet,
) -> PocResult<ControlOperationReceipt> {
    require_real_directory(&request.state_root, "current-I2 state root")?;
    require_disjoint_directories(&request.state_root, &changes.profile.source_root)?;
    validate_catalog_binding(&request.catalog_binding)?;
    if request.public_root_hash.is_empty() {
        return Err(PocError::Integrity(
            "current-I2 public root label is empty".to_owned(),
        ));
    }
    let verdict = request.boundary.verdict()?;
    let coverage = coverage_for(ControlIntent::ClosingPublication, &request.catalog_binding);
    let stack = LayerStack::open(request.state_root.clone())?;
    let started_unix_ms = unix_time_ms()?;
    let timer = MonotonicTimer::start()?;
    let outcome = stack.publish_hidden_validation(HiddenValidationPublication {
        publication_id: request.publication_id,
        changes: changes.changes.clone(),
        source_layer_dir: changes.profile.source_root.clone(),
        public_root_hash: request.public_root_hash.clone(),
    })?;
    let span = timer.finish()?;
    if !outcome.matched {
        return Err(PocError::Integrity(
            "current-I2 hidden publication did not match its source".to_owned(),
        ));
    }
    Ok(ControlOperationReceipt {
        schema_version: SCHEMA_VERSION,
        implementation: "current_i2_layerstack".to_owned(),
        intent: ControlIntent::ClosingPublication,
        catalog_binding_id: request.catalog_binding.binding_id.clone(),
        coverage,
        boundary: request.boundary.clone(),
        verdict,
        started_unix_ms,
        span,
        source: Some(changes.profile.clone()),
        publication: Some(ControlPublicationOutcome {
            correlation_id: outcome.correlation_id,
            candidate_generation: outcome.candidate_generation,
            matched: outcome.matched,
        }),
        materialization: None,
        readiness: None,
    })
}

pub fn run_current_i2_materialization<F>(
    request: &CurrentI2MaterializationRequest,
    readiness_probe: F,
) -> PocResult<ControlOperationReceipt>
where
    F: FnOnce(&Path) -> PocResult<ExternalReadinessReceipt>,
{
    if !matches!(
        request.intent,
        ControlIntent::ColdActivation
            | ControlIntent::SameKeyActivation
            | ControlIntent::Fork
            | ControlIntent::Rollback
    ) {
        return Err(PocError::Integrity(
            "current-I2 materialization received a closing intent".to_owned(),
        ));
    }
    require_real_directory(&request.state_root, "current-I2 state root")?;
    validate_catalog_binding(&request.catalog_binding)?;
    if request.timeout.is_zero() {
        return Err(PocError::Integrity(
            "current-I2 materialization timeout is zero".to_owned(),
        ));
    }
    let verdict = request.boundary.verdict()?;
    let coverage = coverage_for(request.intent, &request.catalog_binding);
    let started_unix_ms = unix_time_ms()?;
    let (clock, started_ns) = monotonic_now_ns()?;
    let result = materialize_hidden_candidate(&request.state_root, request.timeout)?;
    validate_cache_expectation(
        request.cache_expectation,
        result.disposition,
        request.expected_selection.as_ref(),
        &ControlSelectionKey {
            materialization_id: result.selection.materialization_id.clone(),
            generation: result.selection.generation,
            fence: result.selection.fence,
            native_tree_sha256: result.selection.native_tree_sha256.clone(),
        },
    )?;
    let readiness = readiness_probe(&result.selection.carrier_path)?;
    if !readiness.passed {
        return Err(PocError::Integrity(format!(
            "current-I2 external readiness failed: {}",
            readiness.observed
        )));
    }
    let (_, finished_ns) = monotonic_now_ns()?;
    let materialization = ControlMaterializationOutcome {
        disposition: disposition_name(result.disposition).to_owned(),
        materialization_id: result.selection.materialization_id,
        root_id: result.selection.root_id,
        attribution_root_id: result.selection.attribution_root_id,
        backend_kind: result.selection.backend_kind,
        backend_format_version: result.selection.backend_format_version,
        target_profile: result.selection.target_profile,
        generation: result.selection.generation,
        fence: result.selection.fence,
        manifest_sha256: result.selection.manifest_sha256,
        carrier_path: result.selection.carrier_path,
        native_tree_sha256: result.selection.native_tree_sha256,
        build_operation_id: result.selection.build_operation_id,
        maximum_buffer_bytes: result.maximum_buffer_bytes,
    };
    Ok(ControlOperationReceipt {
        schema_version: SCHEMA_VERSION,
        implementation: "current_i2_layerstack".to_owned(),
        intent: request.intent,
        catalog_binding_id: request.catalog_binding.binding_id.clone(),
        coverage,
        boundary: request.boundary.clone(),
        verdict,
        started_unix_ms,
        span: monotonic_span(clock, started_ns, finished_ns)?,
        source: None,
        publication: None,
        materialization: Some(materialization),
        readiness: Some(readiness),
    })
}

impl ControlBoundary {
    pub fn verdict(&self) -> PocResult<ControlVerdict> {
        for (name, value) in [
            ("candidate_start", self.candidate_start.as_str()),
            ("candidate_stop", self.candidate_stop.as_str()),
            ("current_i2_start", self.current_i2_start.as_str()),
            ("current_i2_stop", self.current_i2_stop.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(PocError::Integrity(format!(
                    "control boundary {name} is empty"
                )));
            }
        }
        let compatible = self.same_fixture
            && self.same_intent
            && self.same_durability
            && self.same_readiness
            && self.cache_state != ControlCacheMatch::Mismatched;
        match (compatible, self.unknown_reason.as_deref()) {
            (true, None) => Ok(ControlVerdict::Matched),
            (true, Some(reason)) if !reason.trim().is_empty() => Ok(ControlVerdict::Unknown),
            (false, Some(reason)) if !reason.trim().is_empty() => Ok(ControlVerdict::Unknown),
            (_, _) => Err(PocError::Integrity(
                "an unmatched control boundary requires an explicit unknown reason".to_owned(),
            )),
        }
    }
}

fn coverage_for(intent: ControlIntent, binding: &CatalogBinding) -> CatalogCoverageReceipt {
    match intent {
        ControlIntent::ClosingPublication => CatalogCoverageReceipt {
            classification: ControlApiCoverage::PublicIntentProgrammaticCurrentI2,
            product_operation: "publish_workspace_session".to_owned(),
            product_operation_present: binding.facts.publish_workspace_session,
            direct_control_api: "LayerStack::publish_hidden_validation".to_owned(),
        },
        ControlIntent::ColdActivation | ControlIntent::SameKeyActivation => {
            CatalogCoverageReceipt {
                classification: ControlApiCoverage::ProgrammaticCurrentControl,
                product_operation: "activate_workspace_session".to_owned(),
                product_operation_present: binding.facts.activate_workspace_session,
                direct_control_api: "service::materialize_hidden_candidate".to_owned(),
            }
        }
        ControlIntent::Fork => CatalogCoverageReceipt {
            classification: ControlApiCoverage::ProgrammaticCurrentControl,
            product_operation: "fork_workspace_session".to_owned(),
            product_operation_present: binding.facts.fork_workspace_session,
            direct_control_api: "service::materialize_hidden_candidate".to_owned(),
        },
        ControlIntent::Rollback => CatalogCoverageReceipt {
            classification: ControlApiCoverage::ProgrammaticCurrentControl,
            product_operation: "rollback_workspace_session".to_owned(),
            product_operation_present: binding.facts.rollback_workspace_session,
            direct_control_api: "service::materialize_hidden_candidate".to_owned(),
        },
    }
}

fn validate_cache_expectation(
    expectation: ControlCacheExpectation,
    disposition: CandidateMaterializationDisposition,
    expected: Option<&ControlSelectionKey>,
    observed: &ControlSelectionKey,
) -> PocResult<()> {
    match expectation {
        ControlCacheExpectation::ColdBuilt
            if disposition != CandidateMaterializationDisposition::Built =>
        {
            return Err(PocError::Integrity(format!(
                "cold current-I2 control was not built: {}",
                disposition_name(disposition)
            )));
        }
        ControlCacheExpectation::SameKeyReused
            if disposition != CandidateMaterializationDisposition::Reused =>
        {
            return Err(PocError::Integrity(format!(
                "same-key current-I2 control was not reused: {}",
                disposition_name(disposition)
            )));
        }
        ControlCacheExpectation::SameKeyReused => {
            let expected = expected.ok_or_else(|| {
                PocError::Integrity(
                    "same-key current-I2 control lacks the prior exact selection".to_owned(),
                )
            })?;
            if expected != observed {
                return Err(PocError::Integrity(
                    "same-key current-I2 control selected a different generation".to_owned(),
                ));
            }
        }
        ControlCacheExpectation::ColdBuilt | ControlCacheExpectation::NaturallyProduced => {}
    }
    Ok(())
}

const fn disposition_name(disposition: CandidateMaterializationDisposition) -> &'static str {
    match disposition {
        CandidateMaterializationDisposition::Built => "built",
        CandidateMaterializationDisposition::Reused => "reused",
        CandidateMaterializationDisposition::Shared => "shared",
    }
}

fn read_catalog(path: &Path) -> PocResult<Value> {
    let file =
        File::open(path).map_err(|source| PocError::io("open product catalog", path, source))?;
    let mut bounded = file.take(CATALOG_MAX_BYTES + 1);
    let value: Value = serde_json::from_reader(&mut bounded)?;
    let mut trailing = [0_u8; 1];
    if bounded
        .read(&mut trailing)
        .map_err(|source| PocError::io("check product catalog bound", path, source))?
        != 0
    {
        return Err(PocError::Integrity(format!(
            "product catalog exceeds {CATALOG_MAX_BYTES} bytes"
        )));
    }
    Ok(value)
}

fn operation_present(catalog: &Value, domain: &str, operation: &str) -> PocResult<bool> {
    let operations = catalog
        .get("domains")
        .and_then(|domains| domains.get(domain))
        .and_then(|domain| domain.get("operations"))
        .and_then(Value::as_array)
        .ok_or_else(|| PocError::Integrity(format!("product catalog lacks {domain} operations")))?;
    for candidate in operations {
        let name = candidate
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                PocError::Integrity(format!("product catalog {domain} operation lacks a name"))
            })?;
        if name == operation {
            return Ok(true);
        }
    }
    Ok(false)
}

fn catalog_binding_id(
    build_commit: &str,
    exporter_sha256: &str,
    catalog_sha256: &str,
    facts: &ControlCatalogFacts,
) -> PocResult<String> {
    let mut digest = Sha256::new();
    digest.update(CATALOG_BINDING_DOMAIN);
    hash_field(&mut digest, build_commit.as_bytes());
    hash_field(&mut digest, exporter_sha256.as_bytes());
    hash_field(&mut digest, catalog_sha256.as_bytes());
    hash_field(&mut digest, &serde_json::to_vec(facts)?);
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_catalog_binding(binding: &CatalogBinding) -> PocResult<()> {
    if binding.schema_version != SCHEMA_VERSION || binding.kind != "mpla-product-catalog-binding-v1"
    {
        return Err(PocError::Integrity(
            "current-I2 control has an unsupported catalog binding".to_owned(),
        ));
    }
    validate_build_commit(&binding.build_commit)?;
    validate_sha256(&binding.exporter_sha256, "catalog exporter")?;
    validate_sha256(&binding.catalog_sha256, "product catalog")?;
    let expected = catalog_binding_id(
        &binding.build_commit,
        &binding.exporter_sha256,
        &binding.catalog_sha256,
        &binding.facts,
    )?;
    if binding.binding_id != expected {
        return Err(PocError::Integrity(
            "current-I2 control catalog binding ID is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn digest_regular_file(path: &Path, max_bytes: u64) -> PocResult<String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat catalog binding file", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PocError::Integrity(format!(
            "catalog binding input is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > max_bytes {
        return Err(PocError::Integrity(format!(
            "catalog binding input exceeds limit: {} > {max_bytes}",
            metadata.len()
        )));
    }
    let mut file = File::open(path)
        .map_err(|source| PocError::io("open catalog binding file", path, source))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| PocError::io("hash catalog binding file", path, source))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_build_commit(commit: &str) -> PocResult<()> {
    if commit.len() != 40 || !commit.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(PocError::Integrity(format!(
            "catalog binding build commit is not a full Git SHA: {commit}"
        )));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> PocResult<()> {
    if value.len() != 64 || !value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PocError::Integrity(format!(
            "{label} SHA-256 is invalid: {value}"
        )));
    }
    Ok(())
}

fn validate_collection_limits(limits: &ControlCollectionLimits) -> PocResult<()> {
    for (name, value, hard_max) in [
        ("max_entries", limits.max_entries, HARD_MAX_CONTROL_ENTRIES),
        (
            "max_logical_bytes",
            limits.max_logical_bytes,
            HARD_MAX_CONTROL_LOGICAL_BYTES,
        ),
        (
            "max_path_bytes",
            limits.max_path_bytes,
            HARD_MAX_CONTROL_PATH_BYTES,
        ),
    ] {
        if value == 0 || value > hard_max {
            return Err(PocError::Integrity(format!(
                "invalid control collection {name}: {value}"
            )));
        }
    }
    Ok(())
}

fn require_real_directory(path: &Path, label: &str) -> PocResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PocError::Integrity(format!(
            "{label} is not a real directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_disjoint_directories(left: &Path, right: &Path) -> PocResult<()> {
    let left = fs::canonicalize(left)
        .map_err(|source| PocError::io("canonicalize current-I2 state root", left, source))?;
    let right = fs::canonicalize(right)
        .map_err(|source| PocError::io("canonicalize current-I2 source root", right, source))?;
    if left.starts_with(&right) || right.starts_with(&left) {
        return Err(PocError::Integrity(format!(
            "current-I2 state and source trees overlap: {} and {}",
            left.display(),
            right.display()
        )));
    }
    Ok(())
}

fn read_sorted_entries(directory: &Path, remaining: u64) -> PocResult<Vec<DirEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)
        .map_err(|source| PocError::io("read control directory", directory, source))?
    {
        if u64::try_from(entries.len()).expect("entry vector length fits u64") >= remaining {
            return Err(PocError::Integrity(format!(
                "control source exceeds entry limit while reading {}",
                directory.display()
            )));
        }
        entries.push(
            entry.map_err(|source| {
                PocError::io("read control directory entry", directory, source)
            })?,
        );
    }
    entries.sort_by_key(DirEntry::file_name);
    Ok(entries)
}

fn relative_layer_path(root: &Path, path: &Path, max_path_bytes: u64) -> PocResult<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|error| PocError::Integrity(error.to_string()))?;
    let relative = relative.to_str().ok_or_else(|| {
        PocError::Integrity(format!(
            "control source contains a non-UTF-8 path: {}",
            path.display()
        ))
    })?;
    if relative.as_bytes().contains(&b'\\') {
        return Err(PocError::Integrity(format!(
            "control source path contains an ambiguous backslash: {relative}"
        )));
    }
    if u64::try_from(relative.len()).expect("path length fits u64") > max_path_bytes {
        return Err(PocError::Integrity(format!(
            "control source path exceeds limit: {relative}"
        )));
    }
    Ok(relative.to_owned())
}

fn hash_manifest_header(digest: &mut Sha256, kind: &[u8], path: &str, size: u64) {
    hash_field(digest, kind);
    hash_field(digest, path.as_bytes());
    digest.update(size.to_le_bytes());
}

fn hash_field(digest: &mut Sha256, field: &[u8]) {
    digest.update(
        u64::try_from(field.len())
            .expect("hash field length fits u64")
            .to_le_bytes(),
    );
    digest.update(field);
}

fn hash_file_contents(path: &Path, digest: &mut Sha256) -> PocResult<()> {
    let mut file = File::open(path)
        .map_err(|source| PocError::io("open control source file", path, source))?;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| PocError::io("hash control source file", path, source))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(())
}

fn checked_add(left: u64, right: u64, label: &str) -> PocResult<u64> {
    left.checked_add(right)
        .ok_or_else(|| PocError::Integrity(format!("{label} overflow")))
}

#[cfg(target_os = "linux")]
fn monotonic_now_ns() -> PocResult<(MonotonicClock, u64)> {
    use rustix::time::{clock_gettime, ClockId};

    let time = clock_gettime(ClockId::MonotonicRaw);
    Ok((
        MonotonicClock::MonotonicRaw,
        timespec_ns(time.tv_sec, time.tv_nsec)?,
    ))
}

#[cfg(not(target_os = "linux"))]
fn monotonic_now_ns() -> PocResult<(MonotonicClock, u64)> {
    use rustix::time::{clock_gettime, ClockId};

    let time = clock_gettime(ClockId::Monotonic);
    Ok((
        MonotonicClock::Monotonic,
        timespec_ns(time.tv_sec, time.tv_nsec)?,
    ))
}

fn timespec_ns(seconds: i64, nanoseconds: i64) -> PocResult<u64> {
    let seconds = u64::try_from(seconds)
        .map_err(|_| PocError::Clock("negative monotonic time".to_owned()))?;
    let nanoseconds = u64::try_from(nanoseconds)
        .map_err(|_| PocError::Clock("negative monotonic nanoseconds".to_owned()))?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanoseconds))
        .ok_or_else(|| PocError::Clock("monotonic timestamp overflow".to_owned()))
}

fn monotonic_span(
    clock: MonotonicClock,
    started_ns: u64,
    finished_ns: u64,
) -> PocResult<MonotonicSpan> {
    let elapsed_ns = finished_ns
        .checked_sub(started_ns)
        .ok_or_else(|| PocError::Clock("monotonic clock moved backwards".to_owned()))?;
    Ok(MonotonicSpan {
        clock,
        started_ns,
        finished_ns,
        elapsed_ns,
    })
}
