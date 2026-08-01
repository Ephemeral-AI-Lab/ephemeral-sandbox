use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sandbox_runtime_layerstack::{LayerChange, LayerPath};
use sandbox_runtime_mpla_poc::publication_qualification::{
    publication_is_fresh, qualify_publication_timings, validate_candidate_matched_boundary,
    validate_matched_control_boundary, MatchedPublicationReceipt, PublicationCandidateTiming,
    MATCHED_PUBLICATION_TIMING_BASIS,
};
use sandbox_runtime_mpla_poc::{
    bind_product_catalog, prepared_fixture_storage_requirement, read_prepared_fixture_manifest,
    validate_prepared_fixture_cache_layout, write_prepared_fixture_manifest, CatalogBinding,
    CatalogCoverageReceipt, ControlApiCoverage, ControlCacheMatch, ControlChangeSet, ControlIntent,
    ControlOperationReceipt, ControlPublicationOutcome, ControlSourceProfile, ControlVerdict,
    PreparedFixtureBranch, PreparedFixtureControlSource, PreparedFixtureManifest,
    SemanticBuildReceipt, MATCHED_PUBLICATION_START_BOUNDARY, MATCHED_PUBLICATION_STOP_BOUNDARY,
    PREPARED_FIXTURE_ALLOCATION_COUNT, PREPARED_FIXTURE_BASE_SHA256, PREPARED_FIXTURE_CHAIN_DEPTH,
    PREPARED_FIXTURE_CONTROL_ROOT, PREPARED_FIXTURE_CONTROL_SOURCE,
    PREPARED_FIXTURE_CONTROL_SOURCE_MANIFEST_SHA256, PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
    PREPARED_FIXTURE_DEPTH_FIVE_BYTES, PREPARED_FIXTURE_MANIFEST,
    PREPARED_FIXTURE_MARKER_LAYER_BYTES, PREPARED_FIXTURE_PROFILE, PREPARED_FIXTURE_ROOT,
    PREPARED_FIXTURE_RUN_ID, PREPARED_FIXTURE_SINGLE_FILE_LAYER_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::mpla_speed_scorecard::{
    approved_storage_profile, campaign_tool_path, campaign_tool_root, control_boundary,
    publication_roots_match, require_command_exit, require_regular_file, required_string,
    required_u64, sync_directory, validate_build_commit, validate_identifier,
    validate_merged_publication_oracle, CliInvocation, OracleValidation, RuntimeClient,
};

type PublicationResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const PREPARED_FIXTURE_BUILD_BRANCH: &str = "fixture-depth-8";
const PREPARATION_PATH: &str = "/workspace/scorecard-publication-preparation.json";
const CONTROL_PREPARATION_PATH: &str = "/workspace/scorecard-publication-control-preparation.json";
const PROGRESS_PATH: &str = "/workspace/scorecard-publication-progress.jsonl";
const PREPARED_CONTROL_BASE_SOURCE_MANIFEST_SHA256: &str =
    "7e8c0a26242671839f21c7f228696a981d6a0e50018d2140f7beef87df82ab0d";
const PREPARED_CONTROL_DELTA_SOURCE_MANIFEST_SHA256: &str =
    "9755b23ced9873adb986a1234783a5f6c618145c31a185cde62ff6abf6ee2beb";
const LAYER_STACK_BOOTSTRAP_DIRECTORIES: [&str; 4] =
    [".layer-metadata", "base", "layers", "staging"];
const LAYER_STACK_BOOTSTRAP_FILES: [&str; 3] =
    [".storage-writer.lock", "manifest.json", "workspace.json"];
const PROVIDER_MANAGER_SCHEMA_VERSION: u64 = 2;
const PROVIDER_MANAGER_MAX_BYTES: u64 = 64 * 1024;
const PROVIDER_MANAGER_RECORD_FIELDS: [&str; 18] = [
    "workspace_handle_id",
    "lease_id",
    "parked_lease_id",
    "candidate_admission",
    "manifest_version",
    "manifest_root_hash",
    "network_profile",
    "workspace_root",
    "scratch_dir",
    "upperdir",
    "workdir",
    "layer_paths",
    "holder_pid",
    "veth_host_name",
    "veth_ns_name",
    "ns_ip",
    "created_at",
    "last_activity",
];

pub(super) fn inspect_prepared_fixture_cache() -> PublicationResult<Value> {
    let manifest = read_prepared_fixture_manifest()?;
    let layout = validate_prepared_fixture_cache_layout(&manifest)?;
    let cache_run_root = Path::new(PREPARED_FIXTURE_CONTROL_ROOT)
        .join("runs")
        .join(PREPARED_FIXTURE_RUN_ID);
    let (branches, _) = inspect_sealed_prepared_fixture(&manifest, &cache_run_root, false)?;
    Ok(json!({
        "fixture_profile": manifest.profile,
        "fixture_run_id": manifest.run_id,
        "layout": layout,
        "branches": branches,
    }))
}

fn inspect_sealed_prepared_fixture(
    manifest: &PreparedFixtureManifest,
    cache_run_root: &Path,
    require_v3: bool,
) -> PublicationResult<(
    Vec<Value>,
    Option<sandbox_runtime_mpla_poc::ref_store::SealedPairedRefLayoutReceipt>,
)> {
    require_prepared_fixture_cache_layout(&cache_run_root)?;
    let locator_store = sandbox_runtime_mpla_poc::locator::SealedLocatorStore::open(
        cache_run_root.join("locators"),
    )?;
    let ref_store = sandbox_runtime_mpla_poc::ref_store::SealedPairedRefStore::open(
        cache_run_root.join("refs"),
    )?;
    let paired_ref_v3 = require_v3
        .then(|| ref_store.require_v3_layout())
        .transpose()?;
    let manifest_branches = manifest
        .branches
        .iter()
        .map(|branch| branch.branch.clone())
        .collect::<Vec<_>>();
    if ref_store.branch_names() != manifest_branches {
        return Err("prepared fixture paired refs do not have the exact manifest branches".into());
    }
    let mut branches = Vec::with_capacity(manifest.branches.len());
    for branch in &manifest.branches {
        let resolved = ref_store
            .read_resolved(&branch.branch, &locator_store)?
            .ok_or_else(|| format!("prepared fixture cache omitted branch {}", branch.branch))?;
        if resolved.value.roots != branch.roots || resolved.canonical != branch.canonical {
            return Err(format!(
                "prepared fixture cache branch {} differs from its manifest",
                branch.branch
            )
            .into());
        }
        let projection_path = cache_run_root
            .join("projections")
            .join(format!("{}.json", branch.roots.root_id.as_str()));
        let projection: sandbox_runtime_mpla_poc::ProjectionRecipe =
            sandbox_runtime_mpla_poc::durable::read_json(&projection_path)?;
        if projection != branch.projection {
            return Err(format!(
                "prepared fixture cache projection differs for {}",
                branch.branch
            )
            .into());
        }
        let payload_root =
            sandbox_runtime_mpla_poc::locator::PayloadRootId::parse(branch.roots.root_id.as_str())?;
        let locator = locator_store.resolve(&payload_root)?.ok_or_else(|| {
            format!(
                "prepared fixture cache has no locator for {}",
                branch.branch
            )
        })?;
        let accounted_bytes = locator.extents.iter().try_fold(0_u64, |total, extent| {
            total
                .checked_add(extent.length)
                .ok_or("prepared fixture locator accounting overflow")
        })?;
        branches.push(json!({
            "branch": branch.branch,
            "chain_depth": branch.chain_depth,
            "semantic_roots": resolved.value.roots,
            "semantic_attribution": resolved.canonical.semantic_attribution,
            "root_manifest": resolved.canonical.root_manifest,
            "projection_roots": projection.roots,
            "projection_lower_allocation_ids_newest_first": projection
                .lower_allocation_ids_newest_first()
                .into_iter()
                .map(|allocation| allocation.as_str())
                .collect::<Vec<_>>(),
            "projection_kernel_lower_count": projection.kernel_lower_count(),
            "locator_allocation_id": locator.allocation_id,
            "locator_extent_count": locator.extents.len(),
            "locator_accounted_bytes": accounted_bytes,
        }));
    }
    Ok((branches, paired_ref_v3))
}

fn require_prepared_fixture_cache_layout(cache_run_root: &Path) -> PublicationResult {
    for path in [
        cache_run_root.join("locators").join("LOCK"),
        cache_run_root.join("locators").join("CURRENT"),
        cache_run_root.join("refs").join("LOCK"),
        cache_run_root.join("refs").join("JOURNAL"),
    ] {
        if !path.is_file() {
            return Err(format!("prepared fixture cache is incomplete: {}", path.display()).into());
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct MountedSession {
    create: CliInvocation,
    mount: CliInvocation,
    workspace_session_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PreparedCandidate {
    branch: String,
    chain_depth: u64,
    accumulated_bytes_before: u64,
    prior_semantic_entry_count: u64,
    fork: CliInvocation,
    activation: CliInvocation,
    delta_write: CliInvocation,
    workspace_session_id: String,
}

#[derive(Debug, Serialize)]
struct CandidateSample {
    label: String,
    branch: String,
    chain_depth: u64,
    accumulated_bytes_before: u64,
    fork: CliInvocation,
    activation: CliInvocation,
    delta_write: CliInvocation,
    publication: CliInvocation,
    oracle: OracleValidation,
    fixture_verification: CliInvocation,
    outer_elapsed_ns: u64,
    service_elapsed_ns: u64,
    matched_publication: MatchedPublicationReceipt,
    semantic_build_elapsed_ns: u64,
    prior_node_bytes_read: u64,
    immutable_payload_bytes_read: u64,
}

#[derive(Debug, Serialize)]
struct MatchedPair {
    pair: u8,
    order: [&'static str; 2],
    candidate: CandidateSample,
    control_base: PreparedControlBase,
    control: MatchedControlSample,
    ratio_numerator: u64,
    ratio_denominator: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChainLayer {
    chain_depth: u64,
    accumulated_bytes: u64,
    activation: CliInvocation,
    write: CliInvocation,
    publication: CliInvocation,
}

#[derive(Debug, Serialize)]
struct PublicationGate {
    gate: String,
    timing_basis: &'static str,
    candidate_ns: Vec<u64>,
    matched_candidate_ns: Vec<u64>,
    control_ns: Vec<u64>,
    candidate_median_ns: u64,
    candidate_max_ns: u64,
    matched_candidate_median_ns: u64,
    control_median_ns: u64,
    median_ratio_numerator: u64,
    median_ratio_denominator: u64,
    required: bool,
    preferred: bool,
}

#[derive(Debug, Serialize)]
struct PublicationEvidence {
    schema_version: u32,
    kind: String,
    run_id: String,
    candidate_sandbox_id: String,
    build_commit: String,
    tool_root: String,
    authority: Value,
    backing: Value,
    cgroup: Value,
    resources: Value,
    resource_bounds: bool,
    catalog_binding: CatalogBinding,
    fixture: Value,
    fixture_preparation_path: String,
    fixture_preparation_elapsed_ns: u64,
    fixture_preparation_outside_measured_interval: bool,
    control_preparation: ControlPreparationBinding,
    fixture_profile: Option<String>,
    fixture_attachment: Option<CliInvocation>,
    initial: Option<MountedSession>,
    initial_write: Option<CliInvocation>,
    initial_publish: Option<CliInvocation>,
    matched_pairs: Vec<MatchedPair>,
    chain_layers: Vec<ChainLayer>,
    depth_five: CandidateSample,
    maximum_depth: CandidateSample,
    gate: PublicationGate,
    all_zero_immutable_payload_reads: bool,
    all_no_second_payload_allocation: bool,
    all_durable: bool,
    all_oracle_exact: bool,
    all_candidate_fixture_receipts_match: bool,
    all_matched_controls_use_expected_fixture: bool,
    final_chain_bytes_before_delta: u64,
    final_chain_below_ten_gib: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct PublicationFixture {
    evidence: Value,
    base_sha256: String,
    delta_sha256: Vec<String>,
    control_source_manifest_sha256: String,
    control_base_source_manifest_sha256: String,
    control_delta_source_manifest_sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PreparedPublicationFixture {
    schema_version: u32,
    kind: String,
    run_id: String,
    candidate_sandbox_id: String,
    build_commit: String,
    fixture: PublicationFixture,
    fixture_profile: Option<String>,
    fixture_attachment: Option<CliInvocation>,
    initial: Option<MountedSession>,
    initial_write: Option<CliInvocation>,
    initial_publish: Option<CliInvocation>,
    prepared_depth_one: Vec<PreparedCandidate>,
    chain_layers: Vec<ChainLayer>,
    depth_five: PreparedCandidate,
    maximum_depth: PreparedCandidate,
    preparation_elapsed_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PublicPublicationOutcome {
    workspace_session_id: String,
    manifest_version: u64,
    root_hash: String,
    layer_count: u64,
    source_count: u64,
    ignored_count: u64,
    destroyed: bool,
    matched_publication: MatchedPublicationReceipt,
}

#[derive(Debug, Deserialize, Serialize)]
struct PreparedControlBase {
    pair: u8,
    control_sandbox_id: String,
    workspace_session_id: String,
    create: CliInvocation,
    write: CliInvocation,
    publication: CliInvocation,
    outcome: PublicPublicationOutcome,
    publish_response_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
struct MatchedControlSample {
    pair: u8,
    control_sandbox_id: String,
    base_workspace_session_id: String,
    workspace_session_id: String,
    create: CliInvocation,
    base_verification: CliInvocation,
    delta_write: CliInvocation,
    publication: CliInvocation,
    outcome: PublicPublicationOutcome,
    fixture_verification_create: CliInvocation,
    fixture_verification: CliInvocation,
    fixture_verification_destroy: CliInvocation,
    publish_response_sha256: String,
    receipt: ControlOperationReceipt,
}

#[derive(Debug, Deserialize, Serialize)]
struct PreparedPublicationControls {
    schema_version: u32,
    kind: String,
    run_id: String,
    candidate_sandbox_id: String,
    control_sandbox_ids: Vec<String>,
    build_commit: String,
    fixture_profile: String,
    base_logical_bytes: u64,
    delta_file_count: u64,
    delta_logical_bytes: u64,
    base_source_manifest_sha256: String,
    delta_source_manifest_sha256: String,
    bases: Vec<PreparedControlBase>,
    preparation_elapsed_ns: u64,
    receipt_checksum_sha256: String,
}

#[derive(Debug, Serialize)]
struct ControlPreparationBinding {
    checksum_sha256: String,
}

struct CachedControlSourceSets {
    base: ControlChangeSet,
    delta: ControlChangeSet,
}

struct ProgressLedger {
    file: File,
}

struct CampaignTools {
    root: PathBuf,
    catalog_exporter: PathBuf,
    product_catalog: PathBuf,
}

fn campaign_tools() -> PublicationResult<CampaignTools> {
    let root = campaign_tool_root()?;
    let runtime_cli = campaign_tool_path("sandbox-runtime-cli")?;
    let token_file = campaign_tool_path("gateway.token")?;
    let catalog_exporter = campaign_tool_path("sandbox-catalog-export")?;
    let product_catalog = campaign_tool_path("product-catalog.json")?;
    let oracle = campaign_tool_path("mpla-poc-oracle")?;
    require_regular_file(&runtime_cli, "runtime CLI")?;
    require_regular_file(&token_file, "gateway token")?;
    require_regular_file(&catalog_exporter, "catalog exporter")?;
    require_regular_file(&product_catalog, "product catalog")?;
    // The fixture's first full-tree attestation is an independent holder-view
    // oracle. Preflight it before this builder writes any persistent state so
    // a staging omission cannot leave another partial fixture generation.
    require_regular_file(&oracle, "independent oracle")?;
    Ok(CampaignTools {
        root,
        catalog_exporter,
        product_catalog,
    })
}

impl ProgressLedger {
    fn create(
        run_id: &str,
        candidate_sandbox_id: &str,
        build_commit: &str,
    ) -> PublicationResult<Self> {
        let file = File::options()
            .create_new(true)
            .write(true)
            .open(PROGRESS_PATH)?;
        let mut ledger = Self { file };
        ledger.mark(
            "preparation_started",
            json!({
                "run_id": run_id,
                "candidate_sandbox_id": candidate_sandbox_id,
                "build_commit": build_commit,
            }),
        )?;
        ledger.sync()?;
        Ok(ledger)
    }

    fn open() -> PublicationResult<Self> {
        Ok(Self {
            file: OpenOptions::new().append(true).open(PROGRESS_PATH)?,
        })
    }

    fn mark(&mut self, stage: &str, details: Value) -> PublicationResult {
        serde_json::to_writer(&mut self.file, &json!({"stage": stage, "details": details}))?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }

    fn sync(&mut self) -> PublicationResult {
        self.file.sync_data()?;
        Ok(())
    }
}

/// Fast normal scorecard setup.  The depth-eight, under-10-GiB immutable chain is a prepared,
/// server-owned fixture; a run receives only fresh ref/locator/projection
/// metadata and normal fresh writable uppers during activation.
pub fn prepare_fixture(
    run_id: &str,
    candidate_sandbox_id: &str,
    build_commit: &str,
    fixture_profile: &str,
) -> PublicationResult<Value> {
    if fixture_profile != PREPARED_FIXTURE_PROFILE {
        return Err("publication preparation requires the fixed sparse prepared fixture".into());
    }
    prepare_cached_fixture(run_id, candidate_sandbox_id, build_commit)
}

fn prepare_cached_fixture(
    run_id: &str,
    candidate_sandbox_id: &str,
    build_commit: &str,
) -> PublicationResult<Value> {
    validate_identifier(run_id, "run_id")?;
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    validate_build_commit(build_commit)?;
    let _tools =
        campaign_tools().map_err(|error| format!("fixture builder tool preflight: {error}"))?;

    let run_root =
        Path::new("/eos/workspace/mpla-poc/scorecard").join(format!("{run_id}-publication"));
    fs::create_dir_all(
        run_root
            .parent()
            .ok_or("publication run root lacks a parent")?,
    )?;
    fs::create_dir(&run_root)?;
    if Path::new(PREPARATION_PATH).exists() {
        return Err(format!("publication preparation already exists: {PREPARATION_PATH}").into());
    }

    let started = Instant::now();
    let mut progress = ProgressLedger::create(run_id, candidate_sandbox_id, build_commit)?;
    let manifest = read_prepared_fixture_manifest()?;
    let fixture = fixture_from_prepared_manifest(&manifest)?;
    let control_changes = collect_cached_control_source(&fixture)?;
    let client = RuntimeClient::new(candidate_sandbox_id)?;
    let fixture_attachment = client.invoke(
        Some(&format!("{run_id}-prepared-fixture-attach")),
        "attach_mpla_prepared_fixture",
        &[
            "--run-id".to_owned(),
            run_id.to_owned(),
            "--fixture-profile".to_owned(),
            PREPARED_FIXTURE_PROFILE.to_owned(),
        ],
    )?;
    require_prepared_fixture_attachment(&fixture_attachment)?;
    let attachment_service_elapsed_ns = required_u64(
        &fixture_attachment.response,
        "service_elapsed_ns",
        "prepared fixture attachment",
    )?;
    let cached_allocation_count = required_u64(
        &fixture_attachment.response,
        "cached_allocation_count",
        "prepared fixture attachment",
    )?;
    let attached_branches = fixture_attachment
        .response
        .get("attached_branches")
        .cloned()
        .ok_or("prepared fixture attachment omitted attached branches")?;
    progress.mark(
        "prepared_fixture_attached",
        json!({
            "fixture_profile": PREPARED_FIXTURE_PROFILE,
            "fixture_logical_bytes": PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
            "attachment_operation": fixture_attachment.operation,
            "payload_bytes_copied": 0,
            "cached_allocation_count": cached_allocation_count,
            "attached_branches": attached_branches,
            "source_manifest_sha256": control_changes.profile.source_manifest_sha256,
            "service_elapsed_ns": attachment_service_elapsed_ns,
        }),
    )?;

    let mut prepared_depth_one = Vec::with_capacity(3);
    let depth_one_entries = manifest.branch("fixture-depth-1")?.semantic.entry_count;
    for pair in 1..=3_u8 {
        prepared_depth_one.push(prepare_small_candidate(
            &client,
            run_id,
            "fixture-depth-1",
            &format!("pair-{pair}"),
            1,
            GIB,
            depth_one_entries,
            &fixture.delta_sha256,
        )?);
    }
    let depth_five_entries = manifest.branch("fixture-depth-5")?.semantic.entry_count;
    let depth_five = prepare_small_candidate(
        &client,
        run_id,
        "fixture-depth-5",
        "depth-5",
        5,
        PREPARED_FIXTURE_DEPTH_FIVE_BYTES,
        depth_five_entries,
        &fixture.delta_sha256,
    )?;
    let depth_eight_entries = manifest.branch("fixture-depth-8")?.semantic.entry_count;
    let maximum_depth = prepare_small_candidate(
        &client,
        run_id,
        "fixture-depth-8",
        "depth-8",
        PREPARED_FIXTURE_CHAIN_DEPTH,
        PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
        depth_eight_entries,
        &fixture.delta_sha256,
    )?;
    let preparation_elapsed_ns = u64::try_from(started.elapsed().as_nanos())
        .map_err(|_| "publication preparation duration overflowed u64")?;
    let prepared = PreparedPublicationFixture {
        schema_version: 2,
        kind: "mpla_booster_prepared_s4_chain_v1".to_owned(),
        run_id: run_id.to_owned(),
        candidate_sandbox_id: candidate_sandbox_id.to_owned(),
        build_commit: build_commit.to_owned(),
        fixture,
        fixture_profile: Some(PREPARED_FIXTURE_PROFILE.to_owned()),
        fixture_attachment: Some(fixture_attachment),
        initial: None,
        initial_write: None,
        initial_publish: None,
        prepared_depth_one,
        chain_layers: Vec::new(),
        depth_five,
        maximum_depth,
        preparation_elapsed_ns,
    };
    write_prepared_publication_fixture(&mut progress, &prepared)
}

/// Prepare the three independent public workspace-session bases used by the
/// matched P4 controls. This is a distinct, pre-clock setup operation: each
/// control sandbox durably publishes one exact sparse one-GiB base, while the
/// measured P4 control later publishes the exact ten-file/one-MiB delta
/// through the normal public workspace-session operation.
pub fn prepare_control_bases(
    run_id: &str,
    candidate_sandbox_id: &str,
    control_sandbox_ids: &[String],
    build_commit: &str,
    fixture_profile: &str,
) -> PublicationResult<Value> {
    validate_identifier(run_id, "run_id")?;
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    require_control_sandbox_ids(control_sandbox_ids, candidate_sandbox_id)?;
    validate_build_commit(build_commit)?;
    if fixture_profile != PREPARED_FIXTURE_PROFILE {
        return Err("publication control preparation requires the sealed fixture profile".into());
    }
    if Path::new(CONTROL_PREPARATION_PATH).exists() {
        return Err(format!(
            "publication control preparation already exists: {CONTROL_PREPARATION_PATH}"
        )
        .into());
    }

    let prepared_fixture: PreparedPublicationFixture =
        serde_json::from_slice(&fs::read(PREPARATION_PATH)?)?;
    require_prepared_publication_fixture_identity(
        &prepared_fixture,
        run_id,
        candidate_sandbox_id,
        build_commit,
    )?;
    let run_root =
        Path::new("/eos/workspace/mpla-poc/scorecard").join(format!("{run_id}-publication"));
    if !run_root.is_dir() {
        return Err("publication control preparation lacks the prepared run root".into());
    }

    let manifest = read_prepared_fixture_manifest()?;
    validate_prepared_fixture_cache_layout(&manifest)?;
    let fixture = fixture_from_prepared_manifest(&manifest)?;

    let preparation_started = Instant::now();
    let mut bases = Vec::with_capacity(3);
    for (index, control_sandbox_id) in control_sandbox_ids.iter().enumerate() {
        let pair = u8::try_from(index + 1)?;
        bases.push(prepare_control_base(
            run_id,
            pair,
            control_sandbox_id,
            &fixture,
        )?);
    }
    let preparation_elapsed_ns = preparation_elapsed_ns(preparation_started)?;
    let receipt_checksum_sha256 = control_bases_checksum(&bases)?;
    let prepared = PreparedPublicationControls {
        schema_version: 2,
        kind: "mpla_booster_prepared_public_workspace_controls_v2".to_owned(),
        run_id: run_id.to_owned(),
        candidate_sandbox_id: candidate_sandbox_id.to_owned(),
        control_sandbox_ids: control_sandbox_ids.to_vec(),
        build_commit: build_commit.to_owned(),
        fixture_profile: fixture_profile.to_owned(),
        base_logical_bytes: GIB,
        delta_file_count: 10,
        delta_logical_bytes: MIB,
        base_source_manifest_sha256: fixture.control_base_source_manifest_sha256.clone(),
        delta_source_manifest_sha256: fixture.control_delta_source_manifest_sha256.clone(),
        bases,
        preparation_elapsed_ns,
        receipt_checksum_sha256,
    };
    write_prepared_publication_controls(&prepared)
}

fn prepare_control_base(
    run_id: &str,
    pair: u8,
    control_sandbox_id: &str,
    fixture: &PublicationFixture,
) -> PublicationResult<PreparedControlBase> {
    let client = RuntimeClient::new(control_sandbox_id)?;
    let create = create_public_workspace_session(
        &client,
        &format!("{run_id}-publication-control-{pair}-base-create"),
    )?;
    let workspace_session_id = require_public_workspace_create(&create, "control base create")?;
    let write = write_large_layer(
        &client,
        &workspace_session_id,
        "layer-000.bin",
        GIB,
        "write control base",
    )?;
    let publication = publish_public_workspace_session(
        &client,
        &format!("{run_id}-publication-control-{pair}-base-publish"),
        &workspace_session_id,
    )?;
    let outcome = require_public_workspace_publication(&publication, "control base publication")?;
    if outcome.workspace_session_id != workspace_session_id
        || outcome.source_count != 1
        || outcome.ignored_count != 0
        || outcome.manifest_version == 0
        || outcome.layer_count == 0
    {
        return Err(format!("public control base publication is invalid for pair {pair}").into());
    }
    if fixture.base_sha256 != PREPARED_FIXTURE_BASE_SHA256 {
        return Err("public control base fixture digest changed".into());
    }
    let publish_response_sha256 = sha256_serialized(&publication.response)?;
    Ok(PreparedControlBase {
        pair,
        control_sandbox_id: control_sandbox_id.to_owned(),
        workspace_session_id,
        create,
        write,
        publication,
        outcome,
        publish_response_sha256,
    })
}

fn write_prepared_publication_controls(
    prepared: &PreparedPublicationControls,
) -> PublicationResult<Value> {
    let bytes = serde_json::to_vec_pretty(prepared)?;
    let result_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(CONTROL_PREPARATION_PATH)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    sync_directory(
        Path::new(CONTROL_PREPARATION_PATH)
            .parent()
            .ok_or("publication control preparation path lacks a parent")?,
    )?;
    let bases = prepared
        .bases
        .iter()
        .map(|base| {
            json!({
                "pair": base.pair,
                "control_sandbox_id": base.control_sandbox_id,
                "workspace_session_id": base.workspace_session_id,
                "manifest_version": base.outcome.manifest_version,
                "root_hash": base.outcome.root_hash,
                "layer_count": base.outcome.layer_count,
                "source_count": base.outcome.source_count,
                "ignored_count": base.outcome.ignored_count,
                "destroyed": base.outcome.destroyed,
                "matched_publication": base.outcome.matched_publication,
                "publish_response_sha256": base.publish_response_sha256,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema_version": 2,
        "kind": "mpla_booster_publication_control_preparation_summary_v2",
        "result_path": CONTROL_PREPARATION_PATH,
        "result_sha256": result_sha256,
        "result_bytes": bytes.len(),
        "run_id": prepared.run_id,
        "candidate_sandbox_id": prepared.candidate_sandbox_id,
        "control_sandbox_ids": prepared.control_sandbox_ids,
        "build_commit": prepared.build_commit,
        "fixture_profile": prepared.fixture_profile,
        "base_count": prepared.bases.len(),
        "base_logical_bytes": prepared.base_logical_bytes,
        "delta_file_count": prepared.delta_file_count,
        "delta_logical_bytes": prepared.delta_logical_bytes,
        "base_source_manifest_sha256": prepared.base_source_manifest_sha256,
        "delta_source_manifest_sha256": prepared.delta_source_manifest_sha256,
        "bases": bases,
        "preparation_elapsed_ns": prepared.preparation_elapsed_ns,
        "receipt_checksum_sha256": prepared.receipt_checksum_sha256,
    }))
}

/// Build the closed, server-owned cache exactly once. This command is only
/// useful while the fixture-builder gateway configuration has its dedicated
/// persistent volume mounted read/write. Qualifying scorecards mount the same
/// volume read-only and cannot reach this path through their grammar.
pub fn build_prepared_fixture_cache(
    candidate_sandbox_id: &str,
    build_commit: &str,
) -> PublicationResult<Value> {
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    validate_build_commit(build_commit)?;
    let _tools = campaign_tools()?;

    let cache_root = Path::new(PREPARED_FIXTURE_ROOT);
    reject_existing_prepared_fixture_seal_at(Path::new(PREPARED_FIXTURE_MANIFEST), || {
        let manifest = read_prepared_fixture_manifest()?;
        validate_prepared_fixture_cache_layout(&manifest)?;
        Ok(())
    })?;
    let recovered_paths = recover_unsealed_prepared_fixture_cache(cache_root)
        .map_err(|error| format!("fixture builder recovery validation: {error}"))?;

    let mut progress =
        ProgressLedger::create(PREPARED_FIXTURE_RUN_ID, candidate_sandbox_id, build_commit)
            .map_err(|error| format!("fixture builder progress ledger: {error}"))?;
    progress
        .mark(
            "prepared_fixture_cache_recovery_complete",
            json!({
                "recovered": !recovered_paths.is_empty(),
                "removed_fixture_owned_paths": &recovered_paths,
            }),
        )
        .map_err(|error| format!("fixture builder recovery progress mark: {error}"))?;
    let capacity = require_prepared_fixture_builder_capacity(cache_root)
        .map_err(|error| format!("fixture builder capacity qualification: {error}"))?;
    progress
        .mark("prepared_fixture_cache_capacity_qualified", capacity)
        .map_err(|error| format!("fixture builder capacity progress mark: {error}"))?;
    // Validate the fixed runtime callback before creating any persistent cache
    // content. A rejected endpoint is a configuration error, not a partial
    // fixture generation; this ordering leaves the cache root retryable.
    let client = RuntimeClient::new(candidate_sandbox_id)
        .map_err(|error| format!("fixture builder runtime callback preflight: {error}"))?;
    let started = Instant::now();
    let (control_changes, fixture) =
        prepare_control_source_at(Path::new(PREPARED_FIXTURE_CONTROL_SOURCE))
            .map_err(|error| format!("fixture builder control source: {error}"))?;
    progress
        .mark(
            "prepared_fixture_cache_control_source_created",
            json!({
                "fixture_profile": PREPARED_FIXTURE_PROFILE,
                "source_manifest_sha256": control_changes.profile.source_manifest_sha256,
                "logical_bytes": control_changes.profile.logical_bytes,
                "elapsed_ns": preparation_elapsed_ns(started)?,
            }),
        )
        .map_err(|error| format!("fixture builder control-source progress mark: {error}"))?;
    let mut layer_timings = Vec::with_capacity(PREPARED_FIXTURE_CHAIN_DEPTH as usize);
    let initial = create_mounted_session(&client, PREPARED_FIXTURE_RUN_ID, "fixture-base")
        .map_err(|error| format!("fixture builder initial mounted session: {error}"))?;
    let initial_write = write_large_layer(
        &client,
        &initial.workspace_session_id,
        "layer-000.bin",
        GIB,
        "prepared fixture base write",
    )
    .map_err(|error| format!("fixture builder initial sparse write: {error}"))?;
    let initial_publish = publish(
        &client,
        PREPARED_FIXTURE_RUN_ID,
        "fixture-base-publish",
        &initial.workspace_session_id,
        PREPARED_FIXTURE_BUILD_BRANCH,
    )
    .map_err(|error| format!("fixture builder initial publication: {error}"))?;
    let depth_one_semantic = require_closed_sparse_fixture_publication(&initial_publish, 1, 0, GIB)
        .map_err(|error| format!("fixture builder initial receipt qualification: {error}"))?;
    progress
        .mark(
            "prepared_fixture_cache_initial_receipt_qualified",
            json!({
                "entry_count": depth_one_semantic.entry_count,
                "bytes_read": depth_one_semantic.bytes_read,
                "independent_layout_proof_at_seal": true,
            }),
        )
        .map_err(|error| format!("fixture builder initial-receipt progress mark: {error}"))?;
    let initial_timing = fixture_layer_timing(0, &initial, &initial_write, &initial_publish);
    progress
        .mark(
            "prepared_fixture_cache_layer_complete",
            initial_timing.clone(),
        )
        .map_err(|error| format!("fixture builder initial-layer progress mark: {error}"))?;
    layer_timings.push(initial_timing);
    let depth_one_fork = fork_fixture_branch(
        &client,
        PREPARED_FIXTURE_RUN_ID,
        PREPARED_FIXTURE_BUILD_BRANCH,
        "fixture-depth-1",
    )
    .map_err(|error| format!("fixture builder depth-one branch fork: {error}"))?;
    progress
        .mark(
            "prepared_fixture_cache_branch_forked",
            fixture_fork_timing("fixture-depth-1", &depth_one_fork),
        )
        .map_err(|error| format!("fixture builder depth-one fork progress mark: {error}"))?;
    let layer_one = grow_sparse_single_file_fixture_layer(&client, 1)
        .map_err(|error| format!("fixture builder layer one: {error}"))?;
    progress.mark(
        "prepared_fixture_cache_layer_complete",
        layer_one.timing.clone(),
    )?;
    layer_timings.push(layer_one.timing);
    let mut depth_five_semantic = layer_one.semantic;
    for layer in 2..=4_u8 {
        let built = grow_marker_fixture_layer(&client, layer)?;
        progress.mark(
            "prepared_fixture_cache_layer_complete",
            built.timing.clone(),
        )?;
        layer_timings.push(built.timing);
        depth_five_semantic = built.semantic;
    }
    let depth_five_fork = fork_fixture_branch(
        &client,
        PREPARED_FIXTURE_RUN_ID,
        PREPARED_FIXTURE_BUILD_BRANCH,
        "fixture-depth-5",
    )?;
    progress.mark(
        "prepared_fixture_cache_branch_forked",
        fixture_fork_timing("fixture-depth-5", &depth_five_fork),
    )?;
    let layer_five = grow_sparse_single_file_fixture_layer(&client, 5)?;
    progress.mark(
        "prepared_fixture_cache_layer_complete",
        layer_five.timing.clone(),
    )?;
    layer_timings.push(layer_five.timing);
    let mut depth_eight_semantic = layer_five.semantic;
    for layer in 6..=7_u8 {
        let built = grow_marker_fixture_layer(&client, layer)?;
        progress.mark(
            "prepared_fixture_cache_layer_complete",
            built.timing.clone(),
        )?;
        layer_timings.push(built.timing);
        depth_eight_semantic = built.semantic;
    }
    let cache_run_root = Path::new(PREPARED_FIXTURE_CONTROL_ROOT)
        .join("runs")
        .join(PREPARED_FIXTURE_RUN_ID);
    let locator_store =
        sandbox_runtime_mpla_poc::locator::LocatorStore::open(cache_run_root.join("locators"))?;
    let ref_store =
        sandbox_runtime_mpla_poc::ref_store::PairedRefStore::open(cache_run_root.join("refs"))?;
    let mut branches = Vec::with_capacity(3);
    for (branch, chain_depth, accumulated_bytes, semantic) in [
        ("fixture-depth-1", 1_u64, GIB, depth_one_semantic),
        (
            "fixture-depth-5",
            5_u64,
            PREPARED_FIXTURE_DEPTH_FIVE_BYTES,
            depth_five_semantic,
        ),
        (
            "fixture-depth-8",
            PREPARED_FIXTURE_CHAIN_DEPTH,
            PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
            depth_eight_semantic,
        ),
    ] {
        let resolved = ref_store
            .read_resolved(branch, &locator_store)?
            .ok_or_else(|| format!("prepared fixture build omitted branch {branch}"))?;
        let projection_path = cache_run_root
            .join("projections")
            .join(format!("{}.json", resolved.value.roots.root_id.as_str()));
        let projection: sandbox_runtime_mpla_poc::ProjectionRecipe =
            sandbox_runtime_mpla_poc::durable::read_json(&projection_path)?;
        if projection.roots != resolved.value.roots {
            return Err(format!("prepared fixture projection differs for {branch}").into());
        }
        branches.push(PreparedFixtureBranch {
            branch: branch.to_owned(),
            chain_depth,
            accumulated_bytes,
            roots: resolved.value.roots,
            projection,
            canonical: resolved.canonical,
            semantic,
        });
    }
    let manifest = PreparedFixtureManifest::new(
        build_commit.to_owned(),
        PreparedFixtureControlSource {
            base_sha256: fixture.base_sha256,
            delta_sha256: fixture.delta_sha256,
            source_manifest_sha256: fixture.control_source_manifest_sha256,
        },
        branches,
    );
    drop(ref_store);
    drop(locator_store);
    sync_fixture_filesystem(cache_root)?;
    let layout = validate_prepared_fixture_cache_layout(&manifest)?;
    let (sealed_branches, paired_ref_v3) =
        inspect_sealed_prepared_fixture(&manifest, &cache_run_root, true)?;
    let pre_seal_validation = json!({
        "read_only_reopen": true,
        "exact_branch_count": sealed_branches.len(),
        "branches": sealed_branches,
        "paired_ref": paired_ref_v3
            .ok_or("prepared fixture pre-seal validation omitted paired-ref v3 identity")?,
    });
    write_prepared_fixture_manifest(&manifest)?;
    let elapsed_ns = preparation_elapsed_ns(started)?;
    progress.mark(
        "prepared_fixture_cache_sealed",
        json!({
            "fixture_profile": PREPARED_FIXTURE_PROFILE,
            "manifest_path": PREPARED_FIXTURE_MANIFEST,
            "chain_depth": PREPARED_FIXTURE_CHAIN_DEPTH,
            "logical_bytes": PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
            "allocation_count": layout.allocation_count,
            "allocated_bytes": layout.allocated_bytes,
            "payload_bytes_read": layout.payload_bytes_read,
            "payload_bytes_copied": 0,
            "builder_elapsed_ns": elapsed_ns,
            "pre_seal_validation": pre_seal_validation.clone(),
        }),
    )?;
    progress.sync()?;
    Ok(json!({
        "fixture_profile": PREPARED_FIXTURE_PROFILE,
        "manifest_path": PREPARED_FIXTURE_MANIFEST,
        "chain_depth": PREPARED_FIXTURE_CHAIN_DEPTH,
        "logical_bytes": PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
        "control_source_logical_bytes": GIB + MIB,
        "allocation_count": layout.allocation_count,
        "allocated_bytes": layout.allocated_bytes,
        "payload_bytes_read": layout.payload_bytes_read,
        "payload_bytes_copied": 0,
        "recovered": !recovered_paths.is_empty(),
        "removed_fixture_owned_paths": &recovered_paths,
        "builder_elapsed_ns": elapsed_ns,
        "layer_timings": layer_timings,
        "pre_seal_validation": pre_seal_validation,
    }))
}

/// A seal is the final commit marker. Its presence always excludes recovery:
/// a valid seal makes the builder a no-op error, while an invalid seal fails
/// closed and remains available for operator inspection.
fn reject_existing_prepared_fixture_seal_at<F>(
    manifest_path: &Path,
    validate_seal: F,
) -> PublicationResult
where
    F: FnOnce() -> PublicationResult,
{
    if !manifest_path.exists() {
        return Ok(());
    }
    validate_seal().map_err(|error| {
        format!(
            "prepared fixture seal is present but invalid or corrupt; automatic recovery is forbidden: {error}"
        )
    })?;
    Err(format!(
        "prepared fixture cache is already sealed: {}",
        manifest_path.display()
    )
    .into())
}

/// A provider seeds its normal layer-stack, scratch, and audit roots before
/// the scorecard builder command is able to run. Those infrastructure paths
/// are safe to reuse. A prior unsealed generation may leave only the three
/// exact fixture-owned roots below; after the entire root is validated, those
/// roots are removed and the real lifecycle restarts from layer zero.
fn recover_unsealed_prepared_fixture_cache(cache_root: &Path) -> PublicationResult<Vec<String>> {
    if !cache_root.exists() {
        fs::create_dir_all(cache_root)?;
        return Ok(Vec::new());
    }
    require_cache_directory(cache_root, "prepared fixture cache root")?;

    let mut recoverable = Vec::new();
    for entry in fs::read_dir(cache_root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().into_string().map_err(|_| {
            format!(
                "prepared fixture cache has a non-UTF-8 root entry: {}",
                path.display()
            )
        })?;
        match name.as_str() {
            "layer-stack" => validate_layer_stack_bootstrap(&path, &mut recoverable)?,
            "workspace" => validate_reusable_scratch_root(&path, &mut recoverable)?,
            "storage" => require_cache_directory(&path, "prepared fixture audit root")?,
            "control-source" => {
                require_cache_directory(&path, "prepared fixture control source")?;
                recoverable.push(path);
            }
            _ => {
                return Err(format!(
                    "prepared fixture cache has unexpected or partial data at its root: {}",
                    path.display()
                )
                .into())
            }
        }
    }
    let mut removed = Vec::with_capacity(recoverable.len());
    for path in recoverable {
        let parent = path
            .parent()
            .ok_or("recoverable prepared fixture path has no parent")?;
        fs::remove_dir_all(&path).map_err(|error| {
            format!(
                "remove exact unsealed prepared fixture path {}: {error}",
                path.display()
            )
        })?;
        sync_directory(parent)?;
        removed.push(path.to_string_lossy().into_owned());
    }
    Ok(removed)
}

fn require_prepared_fixture_builder_capacity(cache_root: &Path) -> PublicationResult<Value> {
    let filesystem = rustix::fs::statvfs(cache_root).map_err(|error| {
        format!(
            "prepared fixture cache capacity statvfs failed for {}: {error}",
            cache_root.display()
        )
    })?;
    let allocation_unit = if filesystem.f_frsize == 0 {
        filesystem.f_bsize
    } else {
        filesystem.f_frsize
    };
    let available_bytes = filesystem
        .f_bavail
        .checked_mul(allocation_unit)
        .ok_or("prepared fixture cache free-byte accounting overflowed")?;
    let requirement = prepared_fixture_storage_requirement()?;
    if available_bytes < requirement.required_available_bytes
        || filesystem.f_favail < requirement.minimum_available_inodes
    {
        return Err(format!(
            "prepared fixture cache capacity insufficient: path={} available_bytes={} required_available_bytes={} available_inodes={} required_available_inodes={} chain_bytes={} control_source_bytes={} working_headroom_bytes={}",
            cache_root.display(),
            available_bytes,
            requirement.required_available_bytes,
            filesystem.f_favail,
            requirement.minimum_available_inodes,
            requirement.chain_bytes,
            requirement.control_source_bytes,
            requirement.working_headroom_bytes,
        )
        .into());
    }
    Ok(json!({
        "path": cache_root,
        "available_bytes": available_bytes,
        "available_inodes": filesystem.f_favail,
        "allocation_unit_bytes": allocation_unit,
        "requirement": requirement,
    }))
}

fn validate_layer_stack_bootstrap(
    layer_stack: &Path,
    recoverable: &mut Vec<PathBuf>,
) -> PublicationResult<()> {
    require_cache_directory(layer_stack, "prepared fixture layer-stack root")?;
    for entry in fs::read_dir(layer_stack)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().into_string().map_err(|_| {
            format!(
                "prepared fixture layer-stack has a non-UTF-8 bootstrap entry: {}",
                path.display()
            )
        })?;
        if LAYER_STACK_BOOTSTRAP_DIRECTORIES.contains(&name.as_str()) {
            require_cache_directory(&path, "prepared fixture layer-stack bootstrap directory")?;
        } else if LAYER_STACK_BOOTSTRAP_FILES.contains(&name.as_str()) {
            require_cache_regular_file(&path, "prepared fixture layer-stack bootstrap file")?;
        } else if name == "mpla-poc" {
            require_cache_directory(&path, "unsealed prepared fixture layer-stack state")?;
            recoverable.push(path);
        } else {
            return Err(format!(
                "prepared fixture cache has unexpected or partial layer-stack data: {}",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn validate_empty_cache_directory(path: &Path, label: &str) -> PublicationResult<()> {
    require_cache_directory(path, label)?;
    if fs::read_dir(path)?.next().transpose()?.is_some() {
        return Err(format!("{label} is not empty: {}", path.display()).into());
    }
    Ok(())
}

fn validate_reusable_scratch_root(
    scratch_root: &Path,
    recoverable: &mut Vec<PathBuf>,
) -> PublicationResult<()> {
    require_cache_directory(scratch_root, "prepared fixture scratch root")?;
    let active_workspace_session = scratch_root
        .join("manager.json")
        .exists()
        .then(|| validate_live_provider_manager_state(scratch_root))
        .transpose()?;
    for entry in fs::read_dir(scratch_root)? {
        let entry = entry?;
        let path = entry.path();
        let session_id = entry.file_name().into_string().map_err(|_| {
            format!(
                "prepared fixture scratch root has a non-UTF-8 session entry: {}",
                path.display()
            )
        })?;
        if session_id == "manager.json" {
            continue;
        }
        if session_id == "mpla-poc" {
            require_cache_directory(&path, "unsealed prepared fixture control state")?;
            recoverable.push(path);
            continue;
        }
        if !is_provider_workspace_session_id(&session_id) {
            return Err(format!(
                "prepared fixture scratch root has unexpected or partial data: {}",
                path.display()
            )
            .into());
        }
        require_cache_directory(&path, "prepared fixture scratch session")?;

        if active_workspace_session.as_deref() == Some(session_id.as_str()) {
            validate_live_provider_workspace_session(&path)?;
            continue;
        }
        validate_inactive_provider_workspace_session(&path)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn sync_fixture_filesystem(cache_root: &Path) -> PublicationResult {
    let root = File::open(cache_root)?;
    rustix::fs::syncfs(&root).map_err(|error| {
        format!(
            "sync prepared fixture filesystem {}: {error}",
            cache_root.display()
        )
        .into()
    })
}

#[cfg(not(target_os = "linux"))]
fn sync_fixture_filesystem(cache_root: &Path) -> PublicationResult {
    sync_directory(cache_root)
}

fn validate_inactive_provider_workspace_session(path: &Path) -> PublicationResult<()> {
    let mut contents = fs::read_dir(path)?;
    let Some(executions) = contents.next().transpose()? else {
        return Err(format!(
            "prepared fixture scratch session has no executions directory: {}",
            path.display()
        )
        .into());
    };
    if executions.file_name() != "executions" || contents.next().transpose()?.is_some() {
        return Err(format!(
            "prepared fixture scratch session has unexpected or partial data: {}",
            path.display()
        )
        .into());
    }
    validate_empty_cache_directory(
        &executions.path(),
        "prepared fixture scratch execution directory",
    )
}

fn validate_live_provider_workspace_session(path: &Path) -> PublicationResult<()> {
    let mut expected = ["executions", "upper", "work"];
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            format!(
                "prepared fixture active scratch session has a non-UTF-8 entry: {}",
                entry.path().display()
            )
        })?;
        let Some(index) = expected.iter().position(|expected| *expected == name) else {
            return Err(format!(
                "prepared fixture active scratch session has unexpected or partial data: {}",
                entry.path().display()
            )
            .into());
        };
        require_cache_directory(&entry.path(), "prepared fixture active scratch child")?;
        match name.as_str() {
            "executions" => validate_live_provider_execution_directory(&entry.path())?,
            "work" => validate_live_provider_work_directory(&entry.path())?,
            "upper" => validate_empty_cache_directory(
                &entry.path(),
                "prepared fixture active scratch upper",
            )?,
            _ => unreachable!("active scratch child name was validated above"),
        }
        expected[index] = "";
    }
    if expected.iter().any(|entry| !entry.is_empty()) {
        return Err(format!(
            "prepared fixture active scratch session is incomplete: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn validate_live_provider_work_directory(work: &Path) -> PublicationResult<()> {
    let mut entries = fs::read_dir(work)?;
    let Some(provider_work) = entries.next().transpose()? else {
        return Err(format!(
            "prepared fixture active scratch work directory has no provider child: {}",
            work.display()
        )
        .into());
    };
    if provider_work.file_name() != "work" || entries.next().transpose()?.is_some() {
        return Err(format!(
            "prepared fixture active scratch work directory has unexpected data: {}",
            work.display()
        )
        .into());
    }
    validate_empty_cache_directory(
        &provider_work.path(),
        "prepared fixture active scratch provider work directory",
    )
}

fn validate_live_provider_execution_directory(executions: &Path) -> PublicationResult<()> {
    let mut entries = fs::read_dir(executions)?;
    let Some(execution) = entries.next().transpose()? else {
        return Err(format!(
            "prepared fixture active scratch execution directory has no live command leaf: {}",
            executions.display()
        )
        .into());
    };
    if execution.file_name() != "namespace_execution_1" || entries.next().transpose()?.is_some() {
        return Err(format!(
            "prepared fixture active scratch execution directory has unexpected or stale data: {}",
            executions.display()
        )
        .into());
    }
    require_cache_directory(
        &execution.path(),
        "prepared fixture active scratch execution leaf",
    )?;
    let mut files = fs::read_dir(execution.path())?;
    let Some(transcript) = files.next().transpose()? else {
        return Err(format!(
            "prepared fixture active scratch execution leaf has no transcript: {}",
            execution.path().display()
        )
        .into());
    };
    if transcript.file_name() != "transcript.log" || files.next().transpose()?.is_some() {
        return Err(format!(
            "prepared fixture active scratch execution leaf has unexpected data: {}",
            execution.path().display()
        )
        .into());
    }
    require_cache_regular_file(
        &transcript.path(),
        "prepared fixture active scratch execution transcript",
    )?;
    if fs::metadata(transcript.path())?.len() != 0 {
        return Err(format!(
            "prepared fixture active scratch execution transcript is not empty: {}",
            transcript.path().display()
        )
        .into());
    }
    Ok(())
}

fn validate_live_provider_manager_state(scratch_root: &Path) -> PublicationResult<String> {
    let path = scratch_root.join("manager.json");
    require_cache_regular_file(&path, "prepared fixture workspace manager metadata")?;
    if fs::metadata(&path)?.len() > PROVIDER_MANAGER_MAX_BYTES {
        return Err(format!(
            "prepared fixture workspace manager metadata exceeds {} bytes: {}",
            PROVIDER_MANAGER_MAX_BYTES,
            path.display()
        )
        .into());
    }
    let payload: Value = serde_json::from_slice(&fs::read(&path)?)?;
    let object = payload.as_object().ok_or_else(|| {
        format!(
            "prepared fixture workspace manager metadata is not an object: {}",
            path.display()
        )
    })?;
    if object.len() != 2
        || object.get("schema_version").and_then(Value::as_u64)
            != Some(PROVIDER_MANAGER_SCHEMA_VERSION)
    {
        return Err(format!(
            "prepared fixture workspace manager metadata has an unexpected schema: {}",
            path.display()
        )
        .into());
    }
    let handles = object
        .get("handles")
        .and_then(Value::as_array)
        .filter(|handles| handles.len() == 1)
        .ok_or_else(|| {
            format!(
                "prepared fixture workspace manager metadata must contain one live handle: {}",
                path.display()
            )
        })?;
    let handle = handles[0].as_object().ok_or_else(|| {
        format!(
            "prepared fixture workspace manager handle is not an object: {}",
            path.display()
        )
    })?;
    if handle.len() != PROVIDER_MANAGER_RECORD_FIELDS.len()
        || PROVIDER_MANAGER_RECORD_FIELDS
            .iter()
            .any(|field| !handle.contains_key(*field))
    {
        return Err(format!(
            "prepared fixture workspace manager handle has an unexpected shape: {}",
            path.display()
        )
        .into());
    }
    let session_id = handle
        .get("workspace_handle_id")
        .and_then(Value::as_str)
        .filter(|value| is_provider_workspace_session_id(value))
        .ok_or_else(|| {
            format!(
                "prepared fixture workspace manager handle has an invalid session ID: {}",
                path.display()
            )
        })?;
    let session_root = scratch_root.join(session_id);
    validate_provider_manager_path(handle, "scratch_dir", &session_root, &path)?;
    validate_provider_manager_path(handle, "upperdir", &session_root.join("upper"), &path)?;
    validate_provider_manager_path(handle, "workdir", &session_root.join("work"), &path)?;
    if handle.get("workspace_root").and_then(Value::as_str) != Some("/workspace")
        || handle.get("network_profile").and_then(Value::as_str) != Some("shared")
        || handle
            .get("lease_id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        || handle
            .get("manifest_version")
            .and_then(Value::as_u64)
            .is_none()
        || handle
            .get("manifest_root_hash")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        || !handle
            .get("holder_pid")
            .and_then(Value::as_u64)
            .is_some_and(|pid| pid > 1)
        || !handle.get("layer_paths").is_some_and(|paths| {
            paths
                .as_array()
                .is_some_and(|paths| !paths.is_empty() && paths.iter().all(Value::is_string))
        })
        || !handle
            .get("parked_lease_id")
            .is_some_and(is_optional_string)
        || !handle
            .get("candidate_admission")
            .is_some_and(is_optional_object)
        || !handle.get("veth_host_name").is_some_and(is_optional_string)
        || !handle.get("veth_ns_name").is_some_and(is_optional_string)
        || !handle.get("ns_ip").is_some_and(is_optional_string)
        || handle.get("created_at").and_then(Value::as_f64).is_none()
        || handle
            .get("last_activity")
            .and_then(Value::as_f64)
            .is_none()
    {
        return Err(format!(
            "prepared fixture workspace manager handle has invalid provider fields: {}",
            path.display()
        )
        .into());
    }
    Ok(session_id.to_owned())
}

fn validate_provider_manager_path(
    handle: &serde_json::Map<String, Value>,
    field: &str,
    expected: &Path,
    manager_path: &Path,
) -> PublicationResult<()> {
    if handle
        .get(field)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .as_deref()
        != Some(expected)
    {
        return Err(format!(
            "prepared fixture workspace manager handle has an invalid {field}: {}",
            manager_path.display()
        )
        .into());
    }
    Ok(())
}

fn is_optional_string(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some()
}

fn is_optional_object(value: &Value) -> bool {
    value.is_null() || value.is_object()
}

fn is_provider_workspace_session_id(value: &str) -> bool {
    value.len() == 22
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn require_cache_directory(path: &Path, label: &str) -> PublicationResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{label} is not a real directory: {}", path.display()).into());
    }
    Ok(())
}

fn require_cache_regular_file(path: &Path, label: &str) -> PublicationResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label} is not a regular file: {}", path.display()).into());
    }
    Ok(())
}

fn fork_fixture_branch(
    client: &RuntimeClient,
    run_id: &str,
    source_branch: &str,
    branch: &str,
) -> PublicationResult<CliInvocation> {
    client.invoke(
        Some(&format!("{run_id}-{branch}-fork")),
        "fork_workspace_session",
        &[
            "--run-id".to_owned(),
            run_id.to_owned(),
            "--source-branch".to_owned(),
            source_branch.to_owned(),
            "--branch".to_owned(),
            branch.to_owned(),
        ],
    )
}

struct FixtureLayerBuild {
    timing: Value,
    semantic: SemanticBuildReceipt,
}

/// Fixture construction publishes a complete holder-tree semantic snapshot.
/// The closed sparse profile does not run a second merged-tree activation and
/// scan after each durable publication: that duplicated the lifecycle's
/// semantic work and made cold construction scale with cumulative chain
/// bytes. Instead each publication must return the complete stationary and
/// durability receipt here, and the seal independently validates the exact
/// allocation inventory, names, sizes, hole-only extents, nested projections,
/// canonical manifests, and branch roots in one bounded metadata pass.
fn validate_fixture_layer_publication(
    publication: &CliInvocation,
    affected_paths: u64,
    affected_payload_bytes: u64,
    logical_bytes: u64,
) -> PublicationResult<SemanticBuildReceipt> {
    require_closed_sparse_fixture_publication(
        publication,
        affected_paths,
        affected_payload_bytes,
        logical_bytes,
    )
}

fn grow_sparse_single_file_fixture_layer(
    client: &RuntimeClient,
    layer: u8,
) -> PublicationResult<FixtureLayerBuild> {
    let activation = client.invoke(
        Some(&format!(
            "{PREPARED_FIXTURE_RUN_ID}-layer-{layer:03}-activate"
        )),
        "activate_workspace_session",
        &[
            "--run-id".to_owned(),
            PREPARED_FIXTURE_RUN_ID.to_owned(),
            "--branch".to_owned(),
            PREPARED_FIXTURE_BUILD_BRANCH.to_owned(),
        ],
    )?;
    let workspace_session_id = required_string(
        &activation.response,
        "workspace_session_id",
        "prepared fixture chain activation",
    )?;
    let write = write_large_layer(
        client,
        &workspace_session_id,
        &format!("layer-{layer:03}.bin"),
        PREPARED_FIXTURE_SINGLE_FILE_LAYER_BYTES,
        "prepared fixture chain write",
    )?;
    let publication = publish(
        client,
        PREPARED_FIXTURE_RUN_ID,
        &format!("layer-{layer:03}-publish"),
        &workspace_session_id,
        PREPARED_FIXTURE_BUILD_BRANCH,
    )?;
    let semantic = validate_fixture_layer_publication(
        &publication,
        1,
        0,
        PREPARED_FIXTURE_SINGLE_FILE_LAYER_BYTES,
    )?;
    if write.outer_elapsed_ns == 0 || activation.outer_elapsed_ns == 0 {
        return Err("prepared fixture builder received a zero elapsed command receipt".into());
    }
    Ok(FixtureLayerBuild {
        timing: fixture_layer_timing_from_invocations(layer, &activation, &write, &publication),
        semantic,
    })
}

fn grow_marker_fixture_layer(
    client: &RuntimeClient,
    layer: u8,
) -> PublicationResult<FixtureLayerBuild> {
    let activation = client.invoke(
        Some(&format!(
            "{PREPARED_FIXTURE_RUN_ID}-layer-{layer:03}-activate"
        )),
        "activate_workspace_session",
        &[
            "--run-id".to_owned(),
            PREPARED_FIXTURE_RUN_ID.to_owned(),
            "--branch".to_owned(),
            PREPARED_FIXTURE_BUILD_BRANCH.to_owned(),
        ],
    )?;
    let workspace_session_id = required_string(
        &activation.response,
        "workspace_session_id",
        "prepared fixture marker activation",
    )?;
    let write =
        write_sparse_marker_layer(client, &workspace_session_id, &format!("marker-{layer:03}"))?;
    let publication = publish(
        client,
        PREPARED_FIXTURE_RUN_ID,
        &format!("layer-{layer:03}-publish"),
        &workspace_session_id,
        PREPARED_FIXTURE_BUILD_BRANCH,
    )?;
    let semantic = validate_fixture_layer_publication(
        &publication,
        10,
        0,
        PREPARED_FIXTURE_MARKER_LAYER_BYTES,
    )?;
    if write.outer_elapsed_ns == 0 || activation.outer_elapsed_ns == 0 {
        return Err("prepared fixture builder received a zero elapsed command receipt".into());
    }
    Ok(FixtureLayerBuild {
        timing: fixture_layer_timing_from_invocations(layer, &activation, &write, &publication),
        semantic,
    })
}

fn fixture_layer_timing(
    layer: u8,
    mounted: &MountedSession,
    write: &CliInvocation,
    publication: &CliInvocation,
) -> Value {
    json!({
        "layer": layer,
        "create": fixture_invocation_timing(&mounted.create),
        "mount": fixture_invocation_timing(&mounted.mount),
        "write": fixture_invocation_timing(write),
        "publication": fixture_invocation_timing(publication),
    })
}

fn fixture_layer_timing_from_invocations(
    layer: u8,
    activation: &CliInvocation,
    write: &CliInvocation,
    publication: &CliInvocation,
) -> Value {
    json!({
        "layer": layer,
        "activation": fixture_invocation_timing(activation),
        "write": fixture_invocation_timing(write),
        "publication": fixture_invocation_timing(publication),
    })
}

fn fixture_fork_timing(branch: &str, fork: &CliInvocation) -> Value {
    json!({
        "branch": branch,
        "fork": fixture_invocation_timing(fork),
    })
}

fn fixture_invocation_timing(invocation: &CliInvocation) -> Value {
    json!({
        "outer_elapsed_ns": invocation.outer_elapsed_ns,
        "service_elapsed_ns": invocation.response.get("service_elapsed_ns"),
        "phase_elapsed_ns": invocation.response.get("phase_elapsed_ns"),
        "semantic_phase_spans": invocation
            .response
            .get("semantic")
            .and_then(|semantic| semantic.get("phase_spans")),
    })
}

fn require_control_sandbox_ids(
    control_sandbox_ids: &[String],
    candidate_sandbox_id: &str,
) -> PublicationResult {
    if control_sandbox_ids.len() != 3 {
        return Err("publication requires exactly three control sandbox IDs".into());
    }
    for (index, sandbox_id) in control_sandbox_ids.iter().enumerate() {
        validate_identifier(sandbox_id, "control_sandbox_id")?;
        if sandbox_id == candidate_sandbox_id {
            return Err("control sandbox IDs must differ from the candidate sandbox ID".into());
        }
        if control_sandbox_ids[..index].contains(sandbox_id) {
            return Err("publication control sandbox IDs must be distinct".into());
        }
    }
    Ok(())
}

pub fn run(
    run_id: &str,
    candidate_sandbox_id: &str,
    control_sandbox_ids: &[String],
    build_commit: &str,
) -> PublicationResult<Value> {
    validate_identifier(run_id, "run_id")?;
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    require_control_sandbox_ids(control_sandbox_ids, candidate_sandbox_id)?;
    validate_build_commit(build_commit)?;
    let tools = campaign_tools()?;

    let run_root =
        Path::new("/eos/workspace/mpla-poc/scorecard").join(format!("{run_id}-publication"));
    if !run_root.is_dir() {
        return Err(format!(
            "publication preparation did not create run root: {}",
            run_root.display()
        )
        .into());
    }
    let result_path = Path::new("/workspace/scorecard-publication-result.json");
    if result_path.exists() {
        return Err(format!(
            "publication scorecard result already exists: {}",
            result_path.display()
        )
        .into());
    }

    let prepared_bytes = fs::read(PREPARATION_PATH)?;
    let prepared: PreparedPublicationFixture = serde_json::from_slice(&prepared_bytes)?;
    require_prepared_publication_fixture_identity(
        &prepared,
        run_id,
        candidate_sandbox_id,
        build_commit,
    )?;
    let prepared_controls: PreparedPublicationControls =
        serde_json::from_slice(&fs::read(CONTROL_PREPARATION_PATH)?)?;
    let mut progress = ProgressLedger::open()?;
    progress.mark(
        "measurement_started",
        json!({
            "fixture_preparation_elapsed_ns": prepared.preparation_elapsed_ns,
            "prepared_chain_depth": PREPARED_FIXTURE_CHAIN_DEPTH,
        }),
    )?;
    let authority = super::capability_receipt()?;
    let backing = super::persistent_backing(&run_root)?;
    let cgroup_dir = super::current_cgroup_v2_dir()?;
    let cgroup = json!({
        "path": cgroup_dir,
        "memory_high": super::read_limit(&cgroup_dir.join("memory.high"))?,
        "memory_max": super::read_limit(&cgroup_dir.join("memory.max"))?,
        "membership_proven": super::cgroup_contains_self(&cgroup_dir)?,
    });
    let monitor = super::ResourceMonitor::start(&cgroup_dir, &run_root)?;
    let catalog_binding = bind_product_catalog(
        &tools.catalog_exporter,
        &tools.product_catalog,
        build_commit,
    )?;
    let fixture = prepared.fixture;
    require_prepared_publication_controls(
        &prepared_controls,
        run_id,
        candidate_sandbox_id,
        control_sandbox_ids,
        build_commit,
        &fixture,
    )?;
    let control_preparation_checksum = prepared_controls.receipt_checksum_sha256.clone();
    let mut prepared_control_bases = prepared_controls
        .bases
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>();
    let client = RuntimeClient::new(candidate_sandbox_id)?;
    let initial = prepared.initial;
    let initial_write = prepared.initial_write;
    let initial_publish = prepared.initial_publish;
    let mut prepared_depth_one = prepared
        .prepared_depth_one
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>();

    let orders = [
        ["control", "candidate"],
        ["candidate", "control"],
        ["control", "candidate"],
    ];
    let mut matched_pairs = Vec::with_capacity(3);
    for pair in 1..=3_u8 {
        let index = usize::from(pair - 1);
        let order = orders[index];
        let control_base = prepared_control_bases[index]
            .take()
            .ok_or("publication control base was already consumed")?;
        let mut candidate = None;
        let mut control = None;
        for arm in order {
            match arm {
                "candidate" => {
                    candidate = Some(run_small_candidate(
                        &client,
                        run_id,
                        prepared_depth_one[index]
                            .take()
                            .ok_or("publication candidate was already consumed")?,
                        &format!("pair-{pair}"),
                        &fixture,
                    )?);
                }
                "control" => {
                    control = Some(run_control(
                        run_id,
                        pair,
                        &control_base,
                        &catalog_binding,
                        &fixture,
                    )?);
                }
                _ => return Err("publication pair contains an unsupported arm".into()),
            }
        }
        let candidate = candidate.ok_or("publication pair omitted candidate")?;
        let control = control.ok_or("publication pair omitted control")?;
        validate_matched_control_boundary(&control.receipt)?;
        if control.receipt.span.clock != candidate.matched_publication.span.clock {
            return Err("publication candidate/control clocks do not match".into());
        }
        let control_ns = control.receipt.span.elapsed_ns;
        let ratio_numerator = control_ns;
        let ratio_denominator = candidate.matched_publication.span.elapsed_ns;
        matched_pairs.push(MatchedPair {
            pair,
            order,
            candidate,
            control_base,
            control,
            ratio_numerator,
            ratio_denominator,
        });
        progress.mark(
            "depth_one_pair_complete",
            json!({"pair": pair, "candidate_elapsed_ns": matched_pairs.last().map(|value| value.candidate.outer_elapsed_ns), "control_elapsed_ns": matched_pairs.last().map(|value| value.control.receipt.span.elapsed_ns)}),
        )?;
    }

    let depth_five =
        run_small_candidate(&client, run_id, prepared.depth_five, "depth-5", &fixture)?;
    progress.mark(
        "depth_five_candidate_complete",
        json!({"candidate_elapsed_ns": depth_five.outer_elapsed_ns}),
    )?;
    let maximum_depth =
        run_small_candidate(&client, run_id, prepared.maximum_depth, "depth-8", &fixture)?;
    progress.mark(
        "depth_eight_candidate_complete",
        json!({"candidate_elapsed_ns": maximum_depth.outer_elapsed_ns}),
    )?;
    let chain_layers = prepared.chain_layers;

    let depth_one_timings = matched_pairs
        .iter()
        .map(|pair| PublicationCandidateTiming {
            outer_elapsed_ns: pair.candidate.outer_elapsed_ns,
            service_elapsed_ns: pair.candidate.service_elapsed_ns,
            matched_publication: pair.candidate.matched_publication.clone(),
        })
        .collect::<Vec<_>>();
    let controls = matched_pairs
        .iter()
        .map(|pair| pair.control.receipt.clone())
        .collect::<Vec<_>>();
    let depth_five_timing = PublicationCandidateTiming {
        outer_elapsed_ns: depth_five.outer_elapsed_ns,
        service_elapsed_ns: depth_five.service_elapsed_ns,
        matched_publication: depth_five.matched_publication.clone(),
    };
    let maximum_depth_timing = PublicationCandidateTiming {
        outer_elapsed_ns: maximum_depth.outer_elapsed_ns,
        service_elapsed_ns: maximum_depth.service_elapsed_ns,
        matched_publication: maximum_depth.matched_publication.clone(),
    };
    let timing = qualify_publication_timings(
        &depth_one_timings,
        &controls,
        &depth_five_timing,
        &maximum_depth_timing,
    )?;
    let all_zero_immutable_payload_reads = matched_pairs
        .iter()
        .map(|pair| &pair.candidate)
        .chain([&depth_five, &maximum_depth])
        .all(|sample| sample.immutable_payload_bytes_read == 0);
    let all_no_second_payload_allocation = matched_pairs
        .iter()
        .map(|pair| &pair.candidate)
        .chain([&depth_five, &maximum_depth])
        .all(|sample| {
            sample
                .publication
                .response
                .pointer("/stationary/no_second_payload_allocation")
                .and_then(Value::as_bool)
                == Some(true)
        });
    let all_durable = matched_pairs
        .iter()
        .map(|pair| &pair.candidate)
        .chain([&depth_five, &maximum_depth])
        .all(|sample| publication_is_durable(&sample.publication.response));
    let all_oracle_exact = matched_pairs
        .iter()
        .map(|pair| &pair.candidate)
        .chain([&depth_five, &maximum_depth])
        .all(|sample| sample.oracle.exact_match);
    let all_candidate_fixture_receipts_match = matched_pairs
        .iter()
        .map(|pair| &pair.candidate)
        .chain([&depth_five, &maximum_depth])
        .all(|sample| fixture_receipt_matches(&sample.fixture_verification, &fixture));
    let all_matched_controls_use_expected_fixture = matched_pairs
        .iter()
        .all(|pair| control_uses_fixture(&pair.control_base, &pair.control, &fixture));
    let common_preconditions = all_zero_immutable_payload_reads
        && all_no_second_payload_allocation
        && all_durable
        && all_oracle_exact
        && all_candidate_fixture_receipts_match
        && all_matched_controls_use_expected_fixture;
    let gate = PublicationGate {
        gate: "BG-PUBLISH-SMALL".to_owned(),
        timing_basis: MATCHED_PUBLICATION_TIMING_BASIS,
        required: common_preconditions && timing.required,
        preferred: common_preconditions && timing.preferred,
        candidate_median_ns: timing.candidate_median_ns,
        candidate_max_ns: timing.candidate_max_ns,
        matched_candidate_median_ns: timing.matched_candidate_median_ns,
        control_median_ns: timing.control_median_ns,
        median_ratio_numerator: timing.median_ratio_numerator,
        median_ratio_denominator: timing.median_ratio_denominator,
        candidate_ns: timing.candidate_ns,
        matched_candidate_ns: timing.matched_candidate_ns,
        control_ns: timing.control_ns,
    };
    progress.mark(
        "measurement_complete",
        json!({
            "publish_required": gate.required,
            "publish_preferred": gate.preferred,
            "candidate_median_ns": gate.candidate_median_ns,
            "candidate_max_ns": gate.candidate_max_ns,
            "control_median_ns": gate.control_median_ns,
        }),
    )?;
    let resources = monitor.finish()?;
    super::validate_resource_observation(&resources)?;
    let resource_bounds = true;
    let evidence = PublicationEvidence {
        schema_version: 1,
        kind: "mpla_booster_publication_scorecard_v1".to_owned(),
        run_id: run_id.to_owned(),
        candidate_sandbox_id: candidate_sandbox_id.to_owned(),
        build_commit: build_commit.to_owned(),
        tool_root: tools.root.display().to_string(),
        authority,
        backing,
        cgroup,
        resources: serde_json::to_value(resources)?,
        resource_bounds,
        catalog_binding,
        fixture: fixture.evidence,
        fixture_preparation_path: PREPARATION_PATH.to_owned(),
        fixture_preparation_elapsed_ns: prepared.preparation_elapsed_ns,
        fixture_preparation_outside_measured_interval: true,
        control_preparation: ControlPreparationBinding {
            checksum_sha256: control_preparation_checksum,
        },
        fixture_profile: prepared.fixture_profile,
        fixture_attachment: prepared.fixture_attachment,
        initial,
        initial_write,
        initial_publish,
        matched_pairs,
        chain_layers,
        depth_five,
        maximum_depth,
        gate,
        all_zero_immutable_payload_reads,
        all_no_second_payload_allocation,
        all_durable,
        all_oracle_exact,
        all_candidate_fixture_receipts_match,
        all_matched_controls_use_expected_fixture,
        final_chain_bytes_before_delta: PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
        final_chain_below_ten_gib: PREPARED_FIXTURE_DEPTH_EIGHT_BYTES + MIB < 10 * GIB,
    };
    let bytes = serde_json::to_vec_pretty(&evidence)?;
    let result_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(result_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    sync_directory(
        result_path
            .parent()
            .ok_or("publication scorecard result lacks a parent")?,
    )?;
    Ok(json!({
        "result_path": result_path,
        "result_sha256": result_sha256,
        "result_bytes": bytes.len(),
        "publish_required": evidence.gate.required,
        "publish_preferred": evidence.gate.preferred,
        "candidate_median_ns": evidence.gate.candidate_median_ns,
        "candidate_max_ns": evidence.gate.candidate_max_ns,
        "control_median_ns": evidence.gate.control_median_ns,
        "median_ratio_numerator": evidence.gate.median_ratio_numerator,
        "median_ratio_denominator": evidence.gate.median_ratio_denominator,
    }))
}

fn prepare_control_source_at(
    source: &Path,
) -> PublicationResult<(ControlChangeSet, PublicationFixture)> {
    fs::create_dir(&source)?;
    let base_sha256 = write_zero_file(&source.join("layer-000.bin"), GIB)?;
    let delta_paths = delta_paths();
    let delta_sha256 = write_zero_delta_files(&source, &delta_paths)?;
    if base_sha256 != PREPARED_FIXTURE_BASE_SHA256 {
        return Err("prepared fixture base zero digest constant changed".into());
    }
    let evidence = json!({
        "schema_version": 1,
        "base_bytes": GIB,
        "base_sha256": base_sha256,
        "delta_bytes": MIB,
        "delta_files": delta_paths.len(),
        "delta_paths": delta_paths,
        "delta_sha256": delta_sha256,
        "content_profile": "hole_only_sparse_zero_files_v1",
        "candidate_digest_receipts_required": true,
        "candidate_full_fixture_receipts_required": true,
        "chain_points": [
            {"depth": 1, "accumulated_bytes_before_delta": GIB},
            {"depth": 5, "accumulated_bytes_before_delta": PREPARED_FIXTURE_DEPTH_FIVE_BYTES},
            {"depth": PREPARED_FIXTURE_CHAIN_DEPTH, "accumulated_bytes_before_delta": PREPARED_FIXTURE_DEPTH_EIGHT_BYTES}
        ],
    });
    let fixture = PublicationFixture {
        evidence,
        base_sha256,
        delta_sha256,
        control_source_manifest_sha256: PREPARED_FIXTURE_CONTROL_SOURCE_MANIFEST_SHA256.to_owned(),
        control_base_source_manifest_sha256: PREPARED_CONTROL_BASE_SOURCE_MANIFEST_SHA256
            .to_owned(),
        control_delta_source_manifest_sha256: PREPARED_CONTROL_DELTA_SOURCE_MANIFEST_SHA256
            .to_owned(),
    };
    let changes = collect_cached_control_source_at(source, &fixture)?;
    Ok((changes, fixture))
}

fn fixture_from_prepared_manifest(
    manifest: &PreparedFixtureManifest,
) -> PublicationResult<PublicationFixture> {
    manifest.validate()?;
    let control_source = &manifest.control_source;
    Ok(PublicationFixture {
        evidence: json!({
            "schema_version": 2,
            "fixture_profile": PREPARED_FIXTURE_PROFILE,
            "fixture_manifest": sandbox_runtime_mpla_poc::PREPARED_FIXTURE_MANIFEST,
            "base_bytes": GIB,
            "base_sha256": control_source.base_sha256,
            "delta_bytes": MIB,
            "delta_files": control_source.delta_sha256.len(),
            "delta_paths": delta_paths(),
            "delta_sha256": control_source.delta_sha256,
            "content_profile": "hole_only_sparse_zero_files_v1",
            "candidate_digest_receipts_required": true,
            "candidate_full_fixture_receipts_required": true,
            "control_source_manifest_sha256": control_source.source_manifest_sha256,
            "chain_points": [
                {"depth": 1, "accumulated_bytes_before_delta": GIB},
                {"depth": 5, "accumulated_bytes_before_delta": PREPARED_FIXTURE_DEPTH_FIVE_BYTES},
                {"depth": PREPARED_FIXTURE_CHAIN_DEPTH, "accumulated_bytes_before_delta": PREPARED_FIXTURE_DEPTH_EIGHT_BYTES}
            ],
        }),
        base_sha256: control_source.base_sha256.clone(),
        delta_sha256: control_source.delta_sha256.clone(),
        control_source_manifest_sha256: control_source.source_manifest_sha256.clone(),
        control_base_source_manifest_sha256: PREPARED_CONTROL_BASE_SOURCE_MANIFEST_SHA256
            .to_owned(),
        control_delta_source_manifest_sha256: PREPARED_CONTROL_DELTA_SOURCE_MANIFEST_SHA256
            .to_owned(),
    })
}

/// Recreate the closed eleven-file builder list without rereading a GiB.
fn collect_cached_control_source(
    fixture: &PublicationFixture,
) -> PublicationResult<ControlChangeSet> {
    collect_cached_control_source_at(Path::new(PREPARED_FIXTURE_CONTROL_SOURCE), fixture)
}

fn collect_cached_control_source_at(
    source: &Path,
    fixture: &PublicationFixture,
) -> PublicationResult<ControlChangeSet> {
    let source_root = fs::canonicalize(source)?;
    let expected = expected_control_source_files();
    require_exact_cached_control_source_inventory(&source_root, &expected)?;
    cached_control_changes(
        &source_root,
        &expected,
        fixture.control_source_manifest_sha256.clone(),
    )
}

fn collect_cached_control_source_sets(
    fixture: &PublicationFixture,
) -> PublicationResult<CachedControlSourceSets> {
    collect_cached_control_source_sets_at(Path::new(PREPARED_FIXTURE_CONTROL_SOURCE), fixture)
}

fn collect_cached_control_source_sets_at(
    source: &Path,
    fixture: &PublicationFixture,
) -> PublicationResult<CachedControlSourceSets> {
    let source_root = fs::canonicalize(source)?;
    let base_expected = expected_control_base_files();
    let delta_expected = expected_control_delta_files();
    let full_expected = expected_control_source_files();
    require_exact_cached_control_source_inventory(&source_root, &full_expected)?;
    Ok(CachedControlSourceSets {
        base: cached_control_changes(
            &source_root,
            &base_expected,
            fixture.control_base_source_manifest_sha256.clone(),
        )?,
        delta: cached_control_changes(
            &source_root,
            &delta_expected,
            fixture.control_delta_source_manifest_sha256.clone(),
        )?,
    })
}

fn expected_control_base_files() -> Vec<(String, u64)> {
    vec![("layer-000.bin".to_owned(), GIB)]
}

fn expected_control_delta_files() -> Vec<(String, u64)> {
    (0..10)
        .map(|index| (format!("delta-{index:02}.bin"), delta_bytes(index)))
        .collect()
}

fn expected_control_source_files() -> Vec<(String, u64)> {
    expected_control_base_files()
        .into_iter()
        .chain(expected_control_delta_files())
        .collect()
}

fn require_exact_cached_control_source_inventory(
    source_root: &Path,
    expected: &[(String, u64)],
) -> PublicationResult {
    let mut observed = fs::read_dir(source_root)?
        .map(|entry| {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "prepared control cache contains a non-UTF-8 name")?;
            if !entry.file_type()?.is_file() {
                return Err(format!(
                    "prepared control cache entry is not a real regular file: {}",
                    entry.path().display()
                )
                .into());
            }
            Ok(name)
        })
        .collect::<PublicationResult<Vec<_>>>()?;
    observed.sort();
    let mut expected_names = expected
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    expected_names.sort();
    if observed != expected_names {
        return Err("prepared control cache inventory differs from the sealed fixture".into());
    }
    Ok(())
}

fn cached_control_changes(
    source_root: &Path,
    expected: &[(String, u64)],
    source_manifest_sha256: String,
) -> PublicationResult<ControlChangeSet> {
    let mut changes = Vec::with_capacity(expected.len());
    let mut logical_bytes = 0_u64;
    for (file, expected_bytes) in expected {
        let source_path = source_root.join(file);
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != *expected_bytes
        {
            return Err(format!(
                "prepared control cache has an invalid fixture file: {}",
                source_path.display()
            )
            .into());
        }
        logical_bytes = logical_bytes
            .checked_add(*expected_bytes)
            .ok_or("prepared control cache logical-byte total overflowed")?;
        changes.push(LayerChange::WriteFile {
            path: LayerPath::parse(file)?,
            source_path,
            size: *expected_bytes,
        });
    }
    let profile = ControlSourceProfile {
        source_root: source_root.to_path_buf(),
        entries: u64::try_from(expected.len())?,
        directories: 1,
        regular_files: u64::try_from(expected.len())?,
        symlinks: 0,
        logical_bytes,
        source_manifest_sha256,
    };
    Ok(ControlChangeSet { changes, profile })
}

fn require_control_source_split(
    sources: &CachedControlSourceSets,
    fixture: &PublicationFixture,
) -> PublicationResult {
    require_control_source_profile(
        &sources.base.profile,
        1,
        GIB,
        &fixture.control_base_source_manifest_sha256,
    )?;
    require_control_source_profile(
        &sources.delta.profile,
        10,
        MIB,
        &fixture.control_delta_source_manifest_sha256,
    )?;
    let base_paths = sources
        .base
        .changes
        .iter()
        .map(|change| change.path().as_str())
        .collect::<Vec<_>>();
    if base_paths != ["layer-000.bin"] {
        return Err("prepared control base is not the exact one-file profile".into());
    }
    let delta_paths = sources
        .delta
        .changes
        .iter()
        .map(|change| change.path().as_str().to_owned())
        .collect::<Vec<_>>();
    if delta_paths
        != (0..10)
            .map(|index| format!("delta-{index:02}.bin"))
            .collect::<Vec<_>>()
    {
        return Err("prepared control delta is not the exact ten-file profile".into());
    }
    let observed_delta_bytes = sources
        .delta
        .changes
        .iter()
        .enumerate()
        .map(|(index, change)| match change {
            LayerChange::WriteFile { size, .. } if *size == delta_bytes(index) => Ok(*size),
            _ => Err("prepared control delta file has the wrong operation or size"),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if observed_delta_bytes.iter().sum::<u64>() != MIB {
        return Err("prepared control delta does not total exactly one MiB".into());
    }
    Ok(())
}

fn require_control_source_profile(
    profile: &ControlSourceProfile,
    regular_files: u64,
    logical_bytes: u64,
    source_manifest_sha256: &str,
) -> PublicationResult {
    let expected_root = fs::canonicalize(PREPARED_FIXTURE_CONTROL_SOURCE)?;
    if profile.source_root != expected_root
        || profile.entries != regular_files
        || profile.directories != 1
        || profile.regular_files != regular_files
        || profile.symlinks != 0
        || profile.logical_bytes != logical_bytes
        || profile.source_manifest_sha256 != source_manifest_sha256
    {
        return Err(
            "current-I2 control source profile differs from the sealed fixture split".into(),
        );
    }
    Ok(())
}

fn require_prepared_publication_fixture_identity(
    prepared: &PreparedPublicationFixture,
    run_id: &str,
    candidate_sandbox_id: &str,
    build_commit: &str,
) -> PublicationResult {
    let uses_prepared_cache = prepared.fixture_profile.as_deref() == Some(PREPARED_FIXTURE_PROFILE);
    let valid_cached_shape = prepared.schema_version == 2
        && uses_prepared_cache
        && prepared.fixture_attachment.is_some()
        && prepared.initial.is_none()
        && prepared.initial_write.is_none()
        && prepared.initial_publish.is_none()
        && prepared.chain_layers.is_empty();
    if !valid_cached_shape
        || prepared.kind != "mpla_booster_prepared_s4_chain_v1"
        || prepared.run_id != run_id
        || prepared.candidate_sandbox_id != candidate_sandbox_id
        || prepared.build_commit != build_commit
        || prepared.prepared_depth_one.len() != 3
        || prepared.depth_five.chain_depth != 5
        || prepared.maximum_depth.chain_depth != PREPARED_FIXTURE_CHAIN_DEPTH
    {
        return Err("publication preparation identity or chain shape is invalid".into());
    }
    Ok(())
}

fn require_prepared_control_base(
    base: &PreparedControlBase,
    pair: u8,
    expected_control_sandbox_id: &str,
    fixture: &PublicationFixture,
) -> PublicationResult {
    let workspace_session_id =
        require_public_workspace_create(&base.create, "prepared control base create")?;
    if base.write.operation != "exec_command" {
        return Err("prepared control base write did not use exec_command".into());
    }
    require_command_exit(&base.write.response, "prepared control base write")?;
    require_sparse_file_receipt(
        &base.write,
        "layer-000.bin",
        GIB,
        "prepared control base write",
    )?;
    let outcome =
        require_public_workspace_publication(&base.publication, "prepared control base publish")?;
    if base.pair != pair
        || base.control_sandbox_id != expected_control_sandbox_id
        || base.workspace_session_id != workspace_session_id
        || base.workspace_session_id != outcome.workspace_session_id
        || base.outcome != outcome
        || base.outcome.manifest_version != 2
        || base.outcome.root_hash.is_empty()
        || base.outcome.layer_count != 2
        || base.outcome.source_count != 1
        || base.outcome.ignored_count != 0
        || base.publication.outer_elapsed_ns < base.outcome.matched_publication.span.elapsed_ns
        || sha256_serialized(&base.publication.response)? != base.publish_response_sha256
        || fixture.base_sha256 != PREPARED_FIXTURE_BASE_SHA256
    {
        return Err(
            format!("prepared public workspace control base is invalid for pair {pair}").into(),
        );
    }
    Ok(())
}

fn require_prepared_publication_controls(
    prepared: &PreparedPublicationControls,
    run_id: &str,
    candidate_sandbox_id: &str,
    control_sandbox_ids: &[String],
    build_commit: &str,
    fixture: &PublicationFixture,
) -> PublicationResult {
    if prepared.schema_version != 2
        || prepared.kind != "mpla_booster_prepared_public_workspace_controls_v2"
        || prepared.run_id != run_id
        || prepared.candidate_sandbox_id != candidate_sandbox_id
        || prepared.control_sandbox_ids != control_sandbox_ids
        || prepared.build_commit != build_commit
        || prepared.fixture_profile != PREPARED_FIXTURE_PROFILE
        || prepared.base_logical_bytes != GIB
        || prepared.delta_file_count != 10
        || prepared.delta_logical_bytes != MIB
        || prepared.base_source_manifest_sha256 != fixture.control_base_source_manifest_sha256
        || prepared.delta_source_manifest_sha256 != fixture.control_delta_source_manifest_sha256
        || prepared.bases.len() != 3
        || control_bases_checksum(&prepared.bases)? != prepared.receipt_checksum_sha256
    {
        return Err("publication control preparation identity or profile is invalid".into());
    }
    for (index, base) in prepared.bases.iter().enumerate() {
        let pair = u8::try_from(index + 1)?;
        require_prepared_control_base(base, pair, &control_sandbox_ids[index], fixture)?;
    }
    Ok(())
}

fn control_bases_checksum(bases: &[PreparedControlBase]) -> PublicationResult<String> {
    sha256_serialized(bases)
}

fn sha256_serialized<T: ?Sized + Serialize>(value: &T) -> PublicationResult<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn require_prepared_fixture_attachment(invocation: &CliInvocation) -> PublicationResult {
    if invocation.operation != "attach_mpla_prepared_fixture"
        || required_string(
            &invocation.response,
            "fixture_profile",
            "prepared fixture attachment",
        )? != PREPARED_FIXTURE_PROFILE
        || required_u64(
            &invocation.response,
            "payload_bytes_copied",
            "prepared fixture attachment",
        )? != 0
        || required_u64(
            &invocation.response,
            "cached_allocation_count",
            "prepared fixture attachment",
        )? != PREPARED_FIXTURE_ALLOCATION_COUNT
    {
        return Err("prepared fixture attachment receipt is invalid".into());
    }
    let branches = invocation
        .response
        .get("attached_branches")
        .and_then(Value::as_array)
        .ok_or("prepared fixture attachment omitted attached branches")?;
    let names = branches
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or("prepared fixture attachment branch is not a string")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if names != ["fixture-depth-1", "fixture-depth-5", "fixture-depth-8"] {
        return Err("prepared fixture attachment branches are invalid".into());
    }
    Ok(())
}

fn write_prepared_publication_fixture(
    progress: &mut ProgressLedger,
    prepared: &PreparedPublicationFixture,
) -> PublicationResult<Value> {
    let fixture_attachment = prepared
        .fixture_attachment
        .as_ref()
        .ok_or("prepared publication fixture omitted its attachment receipt")?;
    require_prepared_fixture_attachment(fixture_attachment)?;
    let attachment_service_elapsed_ns = required_u64(
        &fixture_attachment.response,
        "service_elapsed_ns",
        "prepared fixture attachment",
    )?;
    let payload_bytes_copied = required_u64(
        &fixture_attachment.response,
        "payload_bytes_copied",
        "prepared fixture attachment",
    )?;
    let cached_allocation_count = required_u64(
        &fixture_attachment.response,
        "cached_allocation_count",
        "prepared fixture attachment",
    )?;
    let attached_branches = fixture_attachment
        .response
        .get("attached_branches")
        .cloned()
        .ok_or("prepared fixture attachment omitted attached branches")?;
    let bytes = serde_json::to_vec_pretty(prepared)?;
    let result_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(PREPARATION_PATH)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    sync_directory(
        Path::new(PREPARATION_PATH)
            .parent()
            .ok_or("publication preparation path lacks a parent")?,
    )?;
    progress.mark(
        "preparation_complete",
        json!({
            "fixture_profile": prepared.fixture_profile,
            "fixture_logical_bytes": PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
            "chain_depth": PREPARED_FIXTURE_CHAIN_DEPTH,
            "attachment_operation": fixture_attachment.operation,
            "attachment_service_elapsed_ns": attachment_service_elapsed_ns,
            "payload_bytes_copied": payload_bytes_copied,
            "cached_allocation_count": cached_allocation_count,
            "attached_branches": attached_branches.clone(),
            "fixture_preparation_elapsed_ns": prepared.preparation_elapsed_ns,
            "preparation_sha256": result_sha256,
        }),
    )?;
    Ok(json!({
        "result_path": PREPARATION_PATH,
        "result_sha256": result_sha256,
        "result_bytes": bytes.len(),
        "fixture_profile": prepared.fixture_profile,
        "fixture_logical_bytes": PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
        "attachment_operation": fixture_attachment.operation,
        "attachment_service_elapsed_ns": attachment_service_elapsed_ns,
        "payload_bytes_copied": payload_bytes_copied,
        "cached_allocation_count": cached_allocation_count,
        "attached_branches": attached_branches,
        "fixture_preparation_elapsed_ns": prepared.preparation_elapsed_ns,
        "chain_depth": PREPARED_FIXTURE_CHAIN_DEPTH,
        "prepared_depth_one_candidates": 3,
    }))
}

fn create_mounted_session(
    client: &RuntimeClient,
    run_id: &str,
    label: &str,
) -> PublicationResult<MountedSession> {
    let create = client.invoke(
        Some(&format!("{run_id}-{label}-create")),
        "create_mpla_workspace_session",
        &["--run-id".to_owned(), run_id.to_owned()],
    )?;
    let workspace_session_id =
        required_string(&create.response, "workspace_session_id", "MPLA create")?;
    let profile = approved_storage_profile(
        &required_string(&create.response, "storage_admin_profile_id", "MPLA create")?,
        "MPLA create",
    )?;
    let operation_id = format!("{run_id}-{label}-mount");
    let request = json!({
        "schema_version": 1,
        "interface_version": "m2r-iface-v1",
        "profile_id": profile,
        "operation_id": operation_id,
        "action": "mount",
        "scope": create
            .response
            .get("storage_admin_scope")
            .ok_or("MPLA create omitted storage_admin_scope")?,
    });
    let mount = client.invoke(
        Some(&format!("{run_id}-{label}-mount")),
        "mpla_storage_admin",
        &[serde_json::to_string(&request)?],
    )?;
    Ok(MountedSession {
        create,
        mount,
        workspace_session_id,
    })
}

fn prepare_small_candidate(
    client: &RuntimeClient,
    run_id: &str,
    source_branch: &str,
    label: &str,
    chain_depth: u64,
    accumulated_bytes_before: u64,
    prior_semantic_entry_count: u64,
    expected_delta_sha256: &[String],
) -> PublicationResult<PreparedCandidate> {
    let branch = format!("publish-{label}");
    let fork = client.invoke(
        Some(&format!("{run_id}-{label}-fork")),
        "fork_workspace_session",
        &[
            "--run-id".to_owned(),
            run_id.to_owned(),
            "--source-branch".to_owned(),
            source_branch.to_owned(),
            "--branch".to_owned(),
            branch.clone(),
        ],
    )?;
    let activation = client.invoke(
        Some(&format!("{run_id}-{label}-activate")),
        "activate_workspace_session",
        &[
            "--run-id".to_owned(),
            run_id.to_owned(),
            "--branch".to_owned(),
            branch.clone(),
        ],
    )?;
    let workspace_session_id = required_string(
        &activation.response,
        "workspace_session_id",
        "small publication activation",
    )?;
    let delta_write = write_small_delta(
        client,
        &workspace_session_id,
        "delta",
        expected_delta_sha256,
    )?;
    Ok(PreparedCandidate {
        branch,
        chain_depth,
        accumulated_bytes_before,
        prior_semantic_entry_count,
        fork,
        activation,
        delta_write,
        workspace_session_id,
    })
}

fn run_small_candidate(
    client: &RuntimeClient,
    run_id: &str,
    prepared: PreparedCandidate,
    label: &str,
    fixture: &PublicationFixture,
) -> PublicationResult<CandidateSample> {
    let publication = publish(
        client,
        run_id,
        &format!("{label}-publish"),
        &prepared.workspace_session_id,
        &prepared.branch,
    )?;
    require_publication(
        &publication,
        &PublicationExpectation::incremental_affected_paths(
            10,
            0,
            MIB,
            prepared.prior_semantic_entry_count,
        ),
    )?;
    let fixture_command = fixture_verification_command();
    let oracle = validate_merged_publication_oracle(
        client,
        run_id,
        label,
        &prepared.branch,
        &publication,
        Some(&fixture_command),
    )?;
    let fixture_verification = oracle
        .fixture_verification
        .as_ref()
        .ok_or("publication oracle omitted full fixture verification")?
        .clone();
    require_full_fixture_digests(&fixture_verification, fixture)?;
    let matched_publication: MatchedPublicationReceipt = serde_json::from_value(
        publication
            .response
            .get("matched_publication")
            .ok_or("small publication omitted matched_publication")?
            .clone(),
    )?;
    validate_candidate_matched_boundary(&matched_publication)?;
    Ok(CandidateSample {
        label: label.to_owned(),
        branch: prepared.branch,
        chain_depth: prepared.chain_depth,
        accumulated_bytes_before: prepared.accumulated_bytes_before,
        outer_elapsed_ns: publication.outer_elapsed_ns,
        service_elapsed_ns: required_u64(
            &publication.response,
            "service_elapsed_ns",
            "small publication",
        )?,
        matched_publication,
        semantic_build_elapsed_ns: required_u64(
            publication
                .response
                .get("phase_elapsed_ns")
                .ok_or("small publication omitted phase_elapsed_ns")?,
            "semantic_build",
            "small publication phases",
        )?,
        prior_node_bytes_read: required_u64(
            &publication.response,
            "prior_node_bytes_read",
            "small publication",
        )?,
        immutable_payload_bytes_read: required_u64(
            &publication.response,
            "immutable_payload_bytes_read",
            "small publication",
        )?,
        fork: prepared.fork,
        activation: prepared.activation,
        delta_write: prepared.delta_write,
        publication,
        oracle,
        fixture_verification,
    })
}

fn run_control(
    run_id: &str,
    pair: u8,
    prepared_base: &PreparedControlBase,
    catalog_binding: &CatalogBinding,
    fixture: &PublicationFixture,
) -> PublicationResult<MatchedControlSample> {
    require_prepared_control_base(
        prepared_base,
        pair,
        &prepared_base.control_sandbox_id,
        fixture,
    )?;
    let client = RuntimeClient::new(&prepared_base.control_sandbox_id)?;
    let create = create_public_workspace_session(
        &client,
        &format!("{run_id}-publication-control-{pair}-delta-create"),
    )?;
    let workspace_session_id = require_public_workspace_create(&create, "control delta create")?;
    let base_verification = verify_public_control_base(
        &client,
        &workspace_session_id,
        fixture,
        "control base verification",
    )?;
    let delta_write = write_small_delta(
        &client,
        &workspace_session_id,
        "delta",
        &fixture.delta_sha256,
    )?;
    let started_unix_ms = sandbox_runtime_mpla_poc::unix_time_ms()?;
    let publication = publish_public_workspace_session(
        &client,
        &format!("{run_id}-publication-control-{pair}-delta-publish"),
        &workspace_session_id,
    )?;
    let outcome = require_public_workspace_publication(&publication, "control delta publication")?;
    if outcome.workspace_session_id != workspace_session_id
        || outcome.manifest_version
            != prepared_base
                .outcome
                .manifest_version
                .checked_add(1)
                .ok_or("public control manifest version overflowed")?
        || outcome.layer_count
            != prepared_base
                .outcome
                .layer_count
                .checked_add(1)
                .ok_or("public control layer count overflowed")?
        || outcome.root_hash == prepared_base.outcome.root_hash
        || outcome.source_count != 10
        || outcome.ignored_count != 0
    {
        return Err("measured public control did not advance the exact prepared base".into());
    }
    let receipt = public_control_receipt(catalog_binding, fixture, started_unix_ms, &outcome)?;
    validate_matched_control_boundary(&receipt)?;
    let fixture_verification_create = create_public_workspace_session(
        &client,
        &format!("{run_id}-publication-control-{pair}-verify-create"),
    )?;
    let verification_workspace_session_id = require_public_workspace_create(
        &fixture_verification_create,
        "control fixture verification create",
    )?;
    let fixture_verification =
        verify_full_public_control_fixture(&client, &verification_workspace_session_id, fixture)?;
    let fixture_verification_destroy = destroy_public_workspace_session(
        &client,
        &format!("{run_id}-publication-control-{pair}-verify-destroy"),
        &verification_workspace_session_id,
    )?;
    require_public_workspace_destroy(
        &fixture_verification_destroy,
        &verification_workspace_session_id,
        "control fixture verification destroy",
    )?;
    let publish_response_sha256 = sha256_serialized(&publication.response)?;
    Ok(MatchedControlSample {
        pair,
        control_sandbox_id: prepared_base.control_sandbox_id.clone(),
        base_workspace_session_id: prepared_base.workspace_session_id.clone(),
        workspace_session_id,
        create,
        base_verification,
        delta_write,
        publication,
        outcome,
        fixture_verification_create,
        fixture_verification,
        fixture_verification_destroy,
        publish_response_sha256,
        receipt,
    })
}

fn create_public_workspace_session(
    client: &RuntimeClient,
    request_id: &str,
) -> PublicationResult<CliInvocation> {
    client.invoke(Some(request_id), "create_workspace_session", &[])
}

fn require_public_workspace_create(
    invocation: &CliInvocation,
    label: &str,
) -> PublicationResult<String> {
    if invocation.operation != "create_workspace_session"
        || required_string(&invocation.response, "finalize_policy", label)? != "no_op"
        || !matches!(
            required_string(&invocation.response, "network_profile", label)?.as_str(),
            "shared" | "isolated"
        )
    {
        return Err(format!("{label} is not an exact public workspace-session create").into());
    }
    let workspace_session_id =
        required_string(&invocation.response, "workspace_session_id", label)?;
    validate_identifier(&workspace_session_id, "workspace_session_id")?;
    Ok(workspace_session_id)
}

fn publish_public_workspace_session(
    client: &RuntimeClient,
    request_id: &str,
    workspace_session_id: &str,
) -> PublicationResult<CliInvocation> {
    client.invoke(
        Some(request_id),
        "publish_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--grace-s".to_owned(),
            "0".to_owned(),
        ],
    )
}

fn require_public_workspace_publication(
    invocation: &CliInvocation,
    label: &str,
) -> PublicationResult<PublicPublicationOutcome> {
    if invocation.operation != "publish_workspace_session" {
        return Err(format!("{label} did not use publish_workspace_session").into());
    }
    let workspace_session_id =
        required_string(&invocation.response, "workspace_session_id", label)?;
    validate_identifier(&workspace_session_id, "workspace_session_id")?;
    let publish = invocation
        .response
        .get("publish")
        .ok_or_else(|| format!("{label} omitted publish"))?;
    if required_bool(publish, "no_op", label)?
        || !required_bool(&invocation.response, "destroyed", label)?
    {
        return Err(format!("{label} did not durably publish and close its session").into());
    }
    required_u64(&invocation.response, "evicted_upperdir_bytes", label)?;
    let revision = publish
        .get("revision")
        .ok_or_else(|| format!("{label} omitted revision"))?;
    let route_summary = publish
        .get("route_summary")
        .ok_or_else(|| format!("{label} omitted route_summary"))?;
    let matched_publication: MatchedPublicationReceipt = serde_json::from_value(
        invocation
            .response
            .get("matched_publication")
            .ok_or_else(|| format!("{label} omitted matched_publication"))?
            .clone(),
    )?;
    validate_candidate_matched_boundary(&matched_publication)?;
    if invocation.outer_elapsed_ns < matched_publication.span.elapsed_ns {
        return Err(format!("{label} matched span exceeds its public invocation").into());
    }
    let manifest_version = required_u64(revision, "manifest_version", label)?;
    let root_hash = required_string(revision, "root_hash", label)?;
    let layer_count = required_u64(revision, "layer_count", label)?;
    if manifest_version == 0 || root_hash.is_empty() || layer_count == 0 {
        return Err(format!("{label} omitted a durable content-addressed revision").into());
    }
    Ok(PublicPublicationOutcome {
        workspace_session_id,
        manifest_version,
        root_hash,
        layer_count,
        source_count: required_u64(route_summary, "source_count", label)?,
        ignored_count: required_u64(route_summary, "ignored_count", label)?,
        destroyed: true,
        matched_publication,
    })
}

fn required_bool(value: &Value, key: &str, label: &str) -> PublicationResult<bool> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{label} omitted boolean {key}").into())
}

fn verify_public_control_base(
    client: &RuntimeClient,
    workspace_session_id: &str,
    fixture: &PublicationFixture,
    label: &str,
) -> PublicationResult<CliInvocation> {
    let invocation = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--timeout-ms".to_owned(),
            "180000".to_owned(),
            "--yield-time-ms".to_owned(),
            "180000".to_owned(),
            "sha256sum -- layer-000.bin".to_owned(),
        ],
    )?;
    require_command_exit(&invocation.response, label)?;
    require_single_fixture_digest(&invocation, "layer-000.bin", &fixture.base_sha256, label)?;
    Ok(invocation)
}

fn require_single_fixture_digest(
    invocation: &CliInvocation,
    expected_file: &str,
    expected_digest: &str,
    label: &str,
) -> PublicationResult {
    if invocation.operation != "exec_command" {
        return Err(format!("{label} did not use exec_command").into());
    }
    require_command_exit(&invocation.response, label)?;
    let output = required_string(&invocation.response, "output", label)?;
    let mut lines = output.lines();
    let line = lines
        .next()
        .ok_or_else(|| format!("{label} omitted fixture digest"))?;
    if lines.next().is_some() {
        return Err(format!("{label} emitted multiple fixture digest lines").into());
    }
    let mut fields = line.split_whitespace();
    if fields.next() != Some(expected_digest)
        || fields.next() != Some(expected_file)
        || fields.next().is_some()
    {
        return Err(format!("{label} fixture digest is not exact").into());
    }
    Ok(())
}

fn public_control_receipt(
    catalog_binding: &CatalogBinding,
    fixture: &PublicationFixture,
    started_unix_ms: u64,
    outcome: &PublicPublicationOutcome,
) -> PublicationResult<ControlOperationReceipt> {
    if !catalog_binding.facts.publish_workspace_session {
        return Err("product catalog omitted publish_workspace_session".into());
    }
    let receipt = ControlOperationReceipt {
        schema_version: 1,
        implementation: "current_i2_public_workspace_session".to_owned(),
        intent: ControlIntent::ClosingPublication,
        catalog_binding_id: catalog_binding.binding_id.clone(),
        coverage: CatalogCoverageReceipt {
            classification: ControlApiCoverage::PublicIntentProgrammaticCurrentI2,
            product_operation: "publish_workspace_session".to_owned(),
            product_operation_present: true,
            direct_control_api: "publish_workspace_session".to_owned(),
        },
        boundary: control_boundary(
            ControlCacheMatch::NotApplicable,
            MATCHED_PUBLICATION_START_BOUNDARY,
            MATCHED_PUBLICATION_STOP_BOUNDARY,
        ),
        verdict: ControlVerdict::Matched,
        started_unix_ms,
        span: outcome.matched_publication.span.clone(),
        source: Some(ControlSourceProfile {
            source_root: PathBuf::from("/workspace"),
            entries: 10,
            directories: 1,
            regular_files: 10,
            symlinks: 0,
            logical_bytes: MIB,
            source_manifest_sha256: fixture.control_delta_source_manifest_sha256.clone(),
        }),
        publication: Some(ControlPublicationOutcome {
            correlation_id: outcome.workspace_session_id.clone(),
            candidate_generation: outcome.manifest_version,
            matched: true,
        }),
        materialization: None,
        readiness: None,
    };
    validate_matched_control_boundary(&receipt)?;
    Ok(receipt)
}

fn verify_full_public_control_fixture(
    client: &RuntimeClient,
    workspace_session_id: &str,
    fixture: &PublicationFixture,
) -> PublicationResult<CliInvocation> {
    let invocation = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--timeout-ms".to_owned(),
            "180000".to_owned(),
            "--yield-time-ms".to_owned(),
            "180000".to_owned(),
            fixture_verification_command(),
        ],
    )?;
    require_command_exit(&invocation.response, "control full fixture verification")?;
    require_full_fixture_digests(&invocation, fixture)?;
    Ok(invocation)
}

fn destroy_public_workspace_session(
    client: &RuntimeClient,
    request_id: &str,
    workspace_session_id: &str,
) -> PublicationResult<CliInvocation> {
    client.invoke(
        Some(request_id),
        "destroy_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--grace-s".to_owned(),
            "0".to_owned(),
        ],
    )
}

fn require_public_workspace_destroy(
    invocation: &CliInvocation,
    workspace_session_id: &str,
    label: &str,
) -> PublicationResult {
    if invocation.operation != "destroy_workspace_session"
        || required_string(&invocation.response, "workspace_session_id", label)?
            != workspace_session_id
        || !required_bool(&invocation.response, "destroyed", label)?
    {
        return Err(format!("{label} did not destroy the exact public workspace session").into());
    }
    required_u64(&invocation.response, "evicted_upperdir_bytes", label)?;
    Ok(())
}

fn fixture_verification_command() -> String {
    let mut files = Vec::with_capacity(11);
    files.push("layer-000.bin".to_owned());
    files.extend((0..10).map(|index| format!("delta-{index:02}.bin")));
    format!("sha256sum -- {}", files.join(" "))
}

fn require_full_fixture_digests(
    invocation: &CliInvocation,
    fixture: &PublicationFixture,
) -> PublicationResult {
    let output = required_string(
        &invocation.response,
        "output",
        "merged publication fixture verification",
    )?;
    let expected = std::iter::once(("layer-000.bin".to_owned(), fixture.base_sha256.clone()))
        .chain(
            fixture
                .delta_sha256
                .iter()
                .enumerate()
                .map(|(index, digest)| (format!("delta-{index:02}.bin"), digest.clone())),
        )
        .collect::<Vec<_>>();
    let observed = output
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let digest = fields.next().ok_or("fixture digest omitted hash")?;
            let file = fields.next().ok_or("fixture digest omitted path")?;
            if fields.next().is_some() {
                return Err("fixture digest emitted an unexpected field".into());
            }
            Ok((file, digest))
        })
        .collect::<PublicationResult<Vec<_>>>()?;
    if observed.len() != expected.len() {
        return Err(format!(
            "full fixture digest expected {} files, observed {}",
            expected.len(),
            observed.len()
        )
        .into());
    }
    for ((observed_file, observed_digest), (expected_file, expected_digest)) in
        observed.into_iter().zip(expected)
    {
        if observed_file != expected_file || observed_digest != expected_digest {
            return Err(format!(
                "full fixture digest mismatch for {expected_file}: observed {observed_digest} {observed_file}"
            )
            .into());
        }
    }
    Ok(())
}

fn fixture_receipt_matches(invocation: &CliInvocation, fixture: &PublicationFixture) -> bool {
    require_full_fixture_digests(invocation, fixture).is_ok()
}

fn control_uses_fixture(
    prepared_base: &PreparedControlBase,
    control: &MatchedControlSample,
    fixture: &PublicationFixture,
) -> bool {
    let Some(expected_manifest_version) = prepared_base.outcome.manifest_version.checked_add(1)
    else {
        return false;
    };
    let Some(expected_layer_count) = prepared_base.outcome.layer_count.checked_add(1) else {
        return false;
    };
    let publication = control.receipt.publication.as_ref();
    let source = control.receipt.source.as_ref();
    let verification_workspace_session_id = require_public_workspace_create(
        &control.fixture_verification_create,
        "control fixture verification create",
    )
    .ok();
    control.pair == prepared_base.pair
        && control.control_sandbox_id == prepared_base.control_sandbox_id
        && control.base_workspace_session_id == prepared_base.workspace_session_id
        && control.workspace_session_id == control.outcome.workspace_session_id
        && control.outcome.manifest_version == expected_manifest_version
        && control.outcome.layer_count == expected_layer_count
        && control.outcome.root_hash != prepared_base.outcome.root_hash
        && control.outcome.source_count == 10
        && control.outcome.ignored_count == 0
        && control.outcome.destroyed
        && control.outcome.matched_publication.span == control.receipt.span
        && validate_candidate_matched_boundary(&control.outcome.matched_publication).is_ok()
        && validate_matched_control_boundary(&control.receipt).is_ok()
        && publication.is_some_and(|publication| {
            publication.matched
                && publication.candidate_generation == expected_manifest_version
                && publication.correlation_id == control.workspace_session_id
        })
        && source.is_some_and(|source| {
            source.source_root.as_path() == Path::new("/workspace")
                && source.entries == 10
                && source.directories == 1
                && source.regular_files == 10
                && source.symlinks == 0
                && source.logical_bytes == MIB
                && source.source_manifest_sha256 == fixture.control_delta_source_manifest_sha256
        })
        && require_single_fixture_digest(
            &control.base_verification,
            "layer-000.bin",
            &fixture.base_sha256,
            "control base verification",
        )
        .is_ok()
        && require_full_fixture_digests(&control.fixture_verification, fixture).is_ok()
        && control.fixture_verification.operation == "exec_command"
        && require_command_exit(
            &control.fixture_verification.response,
            "control full fixture verification",
        )
        .is_ok()
        && verification_workspace_session_id.is_some_and(|workspace_session_id| {
            require_public_workspace_destroy(
                &control.fixture_verification_destroy,
                &workspace_session_id,
                "control fixture verification destroy",
            )
            .is_ok()
        })
        && sha256_serialized(&control.publication.response)
            .is_ok_and(|digest| digest == control.publish_response_sha256)
}

fn preparation_elapsed_ns(started: Instant) -> PublicationResult<u64> {
    u64::try_from(started.elapsed().as_nanos())
        .map_err(|_| "publication preparation duration overflowed u64".into())
}

fn write_large_layer(
    client: &RuntimeClient,
    workspace_session_id: &str,
    file: &str,
    bytes: u64,
    label: &str,
) -> PublicationResult<CliInvocation> {
    if bytes == 0 || bytes % MIB != 0 {
        return Err("large layer bytes must be a nonzero MiB multiple".into());
    }
    let invocation = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--timeout-ms".to_owned(),
            "180000".to_owned(),
            "--yield-time-ms".to_owned(),
            "180000".to_owned(),
            format!("truncate --size {bytes} -- {file} && stat -c 'sparse=%n:%s:%b' -- {file}"),
        ],
    )?;
    require_command_exit(&invocation.response, label)?;
    require_sparse_file_receipt(&invocation, file, bytes, label)?;
    Ok(invocation)
}

fn write_small_delta(
    client: &RuntimeClient,
    workspace_session_id: &str,
    file_prefix: &str,
    expected_sha256: &[String],
) -> PublicationResult<CliInvocation> {
    let files = (0..10)
        .map(|index| format!("{file_prefix}-{index:02}.bin"))
        .collect::<Vec<_>>();
    let invocation = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--timeout-ms".to_owned(),
            "120000".to_owned(),
            "--yield-time-ms".to_owned(),
            "120000".to_owned(),
            format!(
                "for i in 00 01 02 03 04 05; do truncate --size 104858 -- {file_prefix}-$i.bin; done; for i in 06 07 08 09; do truncate --size 104857 -- {file_prefix}-$i.bin; done; stat -c 'sparse=%n:%s:%b' -- {}",
                files.join(" ")
            ),
        ],
    )?;
    require_command_exit(&invocation.response, "write exact ten-file delta")?;
    require_sparse_set_receipts(
        &invocation,
        file_prefix,
        MIB,
        expected_sha256.len(),
        "write exact ten-file delta",
    )?;
    Ok(invocation)
}

fn write_sparse_marker_layer(
    client: &RuntimeClient,
    workspace_session_id: &str,
    file_prefix: &str,
) -> PublicationResult<CliInvocation> {
    let files = (0..10)
        .map(|index| format!("{file_prefix}-{index:02}.bin"))
        .collect::<Vec<_>>();
    let large_bytes = partition_bytes(PREPARED_FIXTURE_MARKER_LAYER_BYTES, 0);
    let small_bytes = partition_bytes(PREPARED_FIXTURE_MARKER_LAYER_BYTES, 9);
    let invocation = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--timeout-ms".to_owned(),
            "120000".to_owned(),
            "--yield-time-ms".to_owned(),
            "120000".to_owned(),
            format!(
                "for i in 00 01 02 03; do truncate --size {large_bytes} -- {file_prefix}-$i.bin; done; for i in 04 05 06 07 08 09; do truncate --size {small_bytes} -- {file_prefix}-$i.bin; done; stat -c 'sparse=%n:%s:%b' -- {}",
                files.join(" ")
            ),
        ],
    )?;
    require_command_exit(&invocation.response, "write exact sparse marker layer")?;
    require_sparse_set_receipts(
        &invocation,
        file_prefix,
        PREPARED_FIXTURE_MARKER_LAYER_BYTES,
        files.len(),
        "write exact sparse marker layer",
    )?;
    Ok(invocation)
}

fn write_zero_file(path: &Path, bytes: u64) -> PublicationResult<String> {
    if bytes == 0 {
        return Err("zero-filled fixture file must be nonempty".into());
    }
    let file = File::options()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)?;
    file.set_len(bytes)?;
    let metadata = file.metadata()?;
    if metadata.len() != bytes {
        return Err(format!(
            "sparse zero-filled fixture has the wrong logical size: {}",
            path.display()
        )
        .into());
    }
    #[cfg(target_os = "linux")]
    require_hole_only_file(&file, path)?;
    if bytes == GIB {
        return Ok(PREPARED_FIXTURE_BASE_SHA256.to_owned());
    }
    digest_zero_bytes(bytes)
}

fn digest_zero_bytes(bytes: u64) -> PublicationResult<String> {
    let mut remaining = bytes;
    let buffer = vec![0_u8; 1024 * 1024];
    let mut digest = Sha256::new();
    while remaining != 0 {
        let chunk_len = usize::try_from(remaining.min(buffer.len() as u64))?;
        digest.update(&buffer[..chunk_len]);
        remaining -= u64::try_from(chunk_len)?;
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(target_os = "linux")]
fn require_hole_only_file(file: &File, path: &Path) -> PublicationResult {
    use std::os::unix::fs::MetadataExt;

    if file.metadata()?.blocks() != 0 {
        return Err(format!(
            "sparse zero-filled fixture acquired data blocks: {}",
            path.display()
        )
        .into());
    }
    match rustix::fs::seek(file, rustix::fs::SeekFrom::Data(0)) {
        Err(error) if error == rustix::io::Errno::NXIO => Ok(()),
        Ok(offset) => Err(format!(
            "sparse zero-filled fixture acquired a data extent at {offset}: {}",
            path.display()
        )
        .into()),
        Err(error) => Err(format!(
            "inspect sparse zero-filled fixture {}: {error}",
            path.display()
        )
        .into()),
    }
}

fn write_zero_delta_files(root: &Path, paths: &[PathBuf]) -> PublicationResult<Vec<String>> {
    if paths.len() != 10 {
        return Err("publication delta fixture requires exactly ten files".into());
    }
    paths
        .iter()
        .enumerate()
        .map(|(index, relative)| write_zero_file(&root.join(relative), delta_bytes(index)))
        .collect()
}

fn delta_bytes(index: usize) -> u64 {
    partition_bytes(MIB, index)
}

fn partition_bytes(total_bytes: u64, index: usize) -> u64 {
    total_bytes / 10 + u64::from(index < usize::try_from(total_bytes % 10).unwrap_or(usize::MAX))
}

fn require_sparse_file_receipt(
    invocation: &CliInvocation,
    file: &str,
    bytes: u64,
    label: &str,
) -> PublicationResult {
    let output = required_string(&invocation.response, "output", label)?;
    let mut lines = output.lines();
    let receipt = lines
        .next()
        .ok_or_else(|| format!("{label} omitted the sparse-file receipt"))?;
    require_sparse_receipt(receipt, file, bytes, label)?;
    if lines.next().is_some() {
        return Err(format!("{label} produced unexpected fixture receipt output").into());
    }
    Ok(())
}

fn require_sparse_set_receipts(
    invocation: &CliInvocation,
    file_prefix: &str,
    total_bytes: u64,
    expected_count: usize,
    label: &str,
) -> PublicationResult {
    let output = required_string(&invocation.response, "output", label)?;
    let observed = output.lines().collect::<Vec<_>>();
    if expected_count != 10 || observed.len() != expected_count {
        return Err(format!(
            "sparse delta receipt expected {} files, observed {}",
            expected_count,
            observed.len()
        )
        .into());
    }
    for (index, receipt) in observed.into_iter().enumerate() {
        let expected_file = format!("{file_prefix}-{index:02}.bin");
        require_sparse_receipt(
            receipt,
            &expected_file,
            partition_bytes(total_bytes, index),
            label,
        )?;
    }
    Ok(())
}

fn require_sparse_receipt(
    receipt: &str,
    expected_file: &str,
    expected_bytes: u64,
    label: &str,
) -> PublicationResult {
    let mut fields = receipt
        .strip_prefix("sparse=")
        .ok_or_else(|| format!("{label} produced a malformed sparse receipt: {receipt}"))?
        .rsplitn(3, ':');
    let blocks = fields
        .next()
        .ok_or_else(|| format!("{label} sparse receipt omitted blocks: {receipt}"))?
        .parse::<u64>()
        .map_err(|_| format!("{label} sparse receipt has invalid blocks: {receipt}"))?;
    let bytes = fields
        .next()
        .ok_or_else(|| format!("{label} sparse receipt omitted size: {receipt}"))?
        .parse::<u64>()
        .map_err(|_| format!("{label} sparse receipt has invalid size: {receipt}"))?;
    let file = fields
        .next()
        .ok_or_else(|| format!("{label} sparse receipt omitted path: {receipt}"))?;
    if file != expected_file || bytes != expected_bytes || blocks != 0 {
        return Err(
            format!("{label} did not retain the required hole-only file: {receipt}").into(),
        );
    }
    Ok(())
}

fn publish(
    client: &RuntimeClient,
    run_id: &str,
    label: &str,
    workspace_session_id: &str,
    branch: &str,
) -> PublicationResult<CliInvocation> {
    client.invoke(
        Some(&format!("{run_id}-{label}")),
        "publish_mpla_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--branch".to_owned(),
            branch.to_owned(),
        ],
    )
}

#[derive(Clone, Copy, Debug)]
enum PublicationSemanticExpectation {
    /// The bootstrap publication seals the full holder namespace. Its receipt
    /// is compared to the independently scanned holder tree before the cache
    /// can be sealed.
    InitialHolderTree {
        expected_entry_count: u64,
        max_bytes_read: u64,
    },
    /// Incremental publication carries the complete post-apply entry count
    /// plus explicit affected-stream counters for the delta itself.
    IncrementalAffectedPaths {
        prior_entry_count: u64,
        minimum_affected_record_count: u64,
    },
}

impl PublicationSemanticExpectation {
    const fn expected_entry_count(self) -> u64 {
        match self {
            Self::InitialHolderTree {
                expected_entry_count,
                ..
            } => expected_entry_count,
            Self::IncrementalAffectedPaths {
                prior_entry_count, ..
            } => prior_entry_count,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PublicationExpectation {
    affected_paths: u64,
    affected_payload_bytes: u64,
    logical_bytes: u64,
    semantic: PublicationSemanticExpectation,
}

impl PublicationExpectation {
    const fn initial_holder_tree(
        affected_paths: u64,
        affected_payload_bytes: u64,
        logical_bytes: u64,
        semantic: PublicationSemanticExpectation,
    ) -> Self {
        Self {
            affected_paths,
            affected_payload_bytes,
            logical_bytes,
            semantic,
        }
    }

    const fn incremental_affected_paths(
        affected_paths: u64,
        affected_payload_bytes: u64,
        logical_bytes: u64,
        prior_entry_count: u64,
    ) -> Self {
        Self {
            affected_paths,
            affected_payload_bytes,
            logical_bytes,
            semantic: PublicationSemanticExpectation::IncrementalAffectedPaths {
                prior_entry_count,
                minimum_affected_record_count: affected_paths,
            },
        }
    }
}

fn require_closed_sparse_fixture_publication(
    publication: &CliInvocation,
    affected_paths: u64,
    affected_payload_bytes: u64,
    logical_bytes: u64,
) -> PublicationResult<SemanticBuildReceipt> {
    let semantic = publication_semantic_receipt(publication)?;
    if semantic.entry_count <= affected_paths {
        return Err(format!(
            "closed sparse fixture publication omitted its complete holder namespace: entries={}; affected_paths={affected_paths}",
            semantic.entry_count,
        )
        .into());
    }
    // The pinned shared base is smaller than 1 GiB. This bound admits that
    // immutable namespace plus the complete eight-layer logical fixture while
    // rejecting an unbounded or unrelated semantic scan.
    let maximum_semantic_bytes = PREPARED_FIXTURE_DEPTH_EIGHT_BYTES
        .checked_add(GIB)
        .ok_or("closed sparse fixture semantic-byte bound overflowed")?;
    if semantic.bytes_read > maximum_semantic_bytes {
        return Err(format!(
            "closed sparse fixture semantic scan exceeded its profile bound: bytes_read={}; maximum={maximum_semantic_bytes}",
            semantic.bytes_read,
        )
        .into());
    }
    let expectation = PublicationExpectation::initial_holder_tree(
        affected_paths,
        affected_payload_bytes,
        logical_bytes,
        PublicationSemanticExpectation::InitialHolderTree {
            expected_entry_count: semantic.entry_count,
            max_bytes_read: maximum_semantic_bytes,
        },
    );
    require_publication(publication, &expectation)?;
    Ok(semantic)
}

fn require_publication(
    publication: &CliInvocation,
    expectation: &PublicationExpectation,
) -> PublicationResult {
    let affected_paths = expectation.affected_paths;
    let affected_payload_bytes = expectation.affected_payload_bytes;
    let logical_bytes = expectation.logical_bytes;
    let semantic_input_bytes = publication
        .response
        .pointer("/semantic/bytes_read")
        .and_then(Value::as_u64)
        .ok_or("publication response omitted semantic bytes_read")?;
    let semantic_entry_count = publication
        .response
        .pointer("/semantic/entry_count")
        .and_then(Value::as_u64)
        .ok_or("publication response omitted semantic entry_count")?;
    let semantic_affected_record_count = publication
        .response
        .pointer("/semantic/affected_record_count")
        .and_then(Value::as_u64);
    let semantic_affected_stream_bytes_read = publication
        .response
        .pointer("/semantic/affected_stream_bytes_read")
        .and_then(Value::as_u64);
    let affected_path_count = required_u64(
        &publication.response,
        "affected_path_count",
        "publication response",
    )?;
    let immutable_payload_bytes = required_u64(
        &publication.response,
        "immutable_payload_bytes_read",
        "publication response",
    )?;
    let affected_payload_observed = publication
        .response
        .get("affected_payload_bytes_read")
        .and_then(Value::as_u64);
    let stable_logical_bytes = publication
        .response
        .pointer("/stationary/stable/after/logical_bytes")
        .and_then(Value::as_u64);
    let no_second_payload_allocation = publication
        .response
        .pointer("/stationary/no_second_payload_allocation")
        .and_then(Value::as_bool);
    let representative_inodes_unchanged = publication
        .response
        .pointer("/stationary/representative_inodes_unchanged")
        .and_then(Value::as_bool);
    let allocated_bytes_unchanged = publication
        .response
        .pointer("/stationary/allocated_bytes_unchanged")
        .and_then(Value::as_bool);
    let roots_match = publication_roots_match(&publication.response);
    let fresh = publication_is_fresh(&publication.response);
    let durable = publication_is_durable(&publication.response);
    let semantic_input_matches = match expectation.semantic {
        PublicationSemanticExpectation::InitialHolderTree {
            expected_entry_count,
            max_bytes_read,
        } => semantic_entry_count == expected_entry_count && semantic_input_bytes <= max_bytes_read,
        PublicationSemanticExpectation::IncrementalAffectedPaths {
            prior_entry_count,
            minimum_affected_record_count,
        } => {
            let expected_stream_read = if affected_payload_bytes == 0 {
                semantic_affected_stream_bytes_read.is_some_and(|bytes| bytes <= logical_bytes)
            } else {
                semantic_affected_stream_bytes_read.is_some_and(|bytes| bytes != 0)
            };
            semantic_entry_count > prior_entry_count
                && semantic_affected_record_count
                    .is_some_and(|count| count >= minimum_affected_record_count)
                && semantic_input_bytes == semantic_affected_stream_bytes_read.unwrap_or_default()
                && expected_stream_read
        }
    };
    let qualified = affected_path_count == affected_paths
        && immutable_payload_bytes == 0
        && roots_match
        && affected_payload_observed == Some(affected_payload_bytes)
        && semantic_input_matches
        && stable_logical_bytes == Some(logical_bytes)
        && no_second_payload_allocation == Some(true)
        && representative_inodes_unchanged == Some(true)
        && allocated_bytes_unchanged == Some(true)
        && fresh
        && durable;
    if !qualified {
        return Err(format!(
            "publication qualification failed: affected_paths={affected_path_count}/{affected_paths}; immutable_payload_bytes={immutable_payload_bytes}/0; roots_match={roots_match}; affected_payload_bytes={affected_payload_observed:?}/{affected_payload_bytes}; semantic_entries={semantic_entry_count}/prior_floor={}; semantic_bytes={semantic_input_bytes}; semantic_affected_records={semantic_affected_record_count:?}; semantic_affected_stream_bytes={semantic_affected_stream_bytes_read:?}; semantic_input_matches={semantic_input_matches}; stable_logical_bytes={stable_logical_bytes:?}/{logical_bytes}; no_second_payload_allocation={no_second_payload_allocation:?}; representative_inodes_unchanged={representative_inodes_unchanged:?}; allocated_bytes_unchanged={allocated_bytes_unchanged:?}; fresh={fresh}; durable={durable}",
            expectation.semantic.expected_entry_count(),
        )
        .into());
    }
    Ok(())
}

fn publication_semantic_receipt(
    publication: &CliInvocation,
) -> PublicationResult<SemanticBuildReceipt> {
    let semantic = publication
        .response
        .get("semantic")
        .cloned()
        .ok_or("publication response omitted semantic receipt")?;
    serde_json::from_value(semantic)
        .map_err(|error| format!("decode publication semantic receipt: {error}").into())
}

fn publication_is_durable(response: &Value) -> bool {
    [
        "files_fsynced",
        "object_directory_fsynced",
        "manifest_fsynced",
        "manifest_directory_fsynced",
    ]
    .into_iter()
    .all(|field| {
        response
            .pointer(&format!("/semantic/durability/{field}"))
            .and_then(Value::as_bool)
            == Some(true)
    })
}

fn delta_paths() -> Vec<PathBuf> {
    (0..10_u8)
        .map(|index| PathBuf::from(format!("delta-{index:02}.bin")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn fixture() -> PublicationFixture {
        PublicationFixture {
            evidence: json!({}),
            base_sha256: format!("{:064x}", 10),
            delta_sha256: (0..10).map(|index| format!("{:064x}", index)).collect(),
            control_source_manifest_sha256: format!("{:064x}", 11),
            control_base_source_manifest_sha256: PREPARED_CONTROL_BASE_SOURCE_MANIFEST_SHA256
                .to_owned(),
            control_delta_source_manifest_sha256: PREPARED_CONTROL_DELTA_SOURCE_MANIFEST_SHA256
                .to_owned(),
        }
    }

    fn create_sparse_control_source() -> (PathBuf, PublicationFixture) {
        let root =
            std::env::temp_dir().join(format!("mpla-scorecard-control-source-{}", Uuid::new_v4()));
        fs::create_dir(&root).expect("create control source root");
        for (name, bytes) in expected_control_source_files() {
            let file = File::create(root.join(name)).expect("create sparse control source file");
            file.set_len(bytes)
                .expect("size sparse control source file");
        }
        (root, fixture())
    }

    #[test]
    fn staged_control_profiles_are_exact() {
        assert_eq!(
            expected_control_base_files(),
            vec![("layer-000.bin".to_owned(), GIB)]
        );
        let delta = expected_control_delta_files();
        assert_eq!(delta.len(), 10);
        assert_eq!(delta.iter().map(|(_, bytes)| bytes).sum::<u64>(), MIB);
        assert_eq!(
            delta
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            (0..10)
                .map(|index| format!("delta-{index:02}.bin"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn staged_control_source_inventory_fails_closed() {
        let (root, fixture) = create_sparse_control_source();
        let sources =
            collect_cached_control_source_sets_at(&root, &fixture).expect("exact source split");
        assert_eq!(sources.base.profile.regular_files, 1);
        assert_eq!(sources.base.profile.logical_bytes, GIB);
        assert_eq!(sources.delta.profile.regular_files, 10);
        assert_eq!(sources.delta.profile.logical_bytes, MIB);

        File::create(root.join("unexpected.bin")).expect("create unexpected cache entry");
        assert!(collect_cached_control_source_sets_at(&root, &fixture).is_err());
        fs::remove_file(root.join("unexpected.bin")).expect("remove unexpected cache entry");

        fs::remove_file(root.join("delta-09.bin")).expect("remove required cache entry");
        assert!(collect_cached_control_source_sets_at(&root, &fixture).is_err());
        fs::remove_dir_all(root).expect("remove test control source");
    }

    fn receipt(output: String) -> CliInvocation {
        CliInvocation {
            operation: "fixture-verification".to_owned(),
            request_id: None,
            outer_elapsed_ns: 0,
            response: json!({"output": output}),
        }
    }

    fn prepared_fixture_attachment_receipt(response: Value) -> CliInvocation {
        CliInvocation {
            operation: "attach_mpla_prepared_fixture".to_owned(),
            request_id: Some("prepared-fixture-attachment-test".to_owned()),
            outer_elapsed_ns: 1,
            response,
        }
    }

    #[test]
    fn prepared_fixture_attachment_requires_exact_read_only_receipt() {
        let valid_response = json!({
            "fixture_profile": PREPARED_FIXTURE_PROFILE,
            "payload_bytes_copied": 0_u64,
            "cached_allocation_count": PREPARED_FIXTURE_ALLOCATION_COUNT,
            "attached_branches": [
                "fixture-depth-1",
                "fixture-depth-5",
                "fixture-depth-8",
            ],
        });
        require_prepared_fixture_attachment(&prepared_fixture_attachment_receipt(
            valid_response.clone(),
        ))
        .expect("exact read-only attachment receipt");

        let mut wrong_operation = prepared_fixture_attachment_receipt(valid_response.clone());
        wrong_operation.operation = "prepare_mpla_fixture".to_owned();
        let invalid = [
            ("wrong operation", wrong_operation),
            (
                "wrong profile",
                prepared_fixture_attachment_receipt(json!({
                    "fixture_profile": "untrusted-profile",
                    "payload_bytes_copied": 0_u64,
                    "cached_allocation_count": PREPARED_FIXTURE_ALLOCATION_COUNT,
                    "attached_branches": [
                        "fixture-depth-1",
                        "fixture-depth-5",
                        "fixture-depth-8",
                    ],
                })),
            ),
            (
                "boolean copied bytes",
                prepared_fixture_attachment_receipt(json!({
                    "fixture_profile": PREPARED_FIXTURE_PROFILE,
                    "payload_bytes_copied": false,
                    "cached_allocation_count": PREPARED_FIXTURE_ALLOCATION_COUNT,
                    "attached_branches": [
                        "fixture-depth-1",
                        "fixture-depth-5",
                        "fixture-depth-8",
                    ],
                })),
            ),
            (
                "nonzero copied bytes",
                prepared_fixture_attachment_receipt(json!({
                    "fixture_profile": PREPARED_FIXTURE_PROFILE,
                    "payload_bytes_copied": 1_u64,
                    "cached_allocation_count": PREPARED_FIXTURE_ALLOCATION_COUNT,
                    "attached_branches": [
                        "fixture-depth-1",
                        "fixture-depth-5",
                        "fixture-depth-8",
                    ],
                })),
            ),
            (
                "wrong allocation count",
                prepared_fixture_attachment_receipt(json!({
                    "fixture_profile": PREPARED_FIXTURE_PROFILE,
                    "payload_bytes_copied": 0_u64,
                    "cached_allocation_count": PREPARED_FIXTURE_ALLOCATION_COUNT - 1,
                    "attached_branches": [
                        "fixture-depth-1",
                        "fixture-depth-5",
                        "fixture-depth-8",
                    ],
                })),
            ),
            (
                "reordered branches",
                prepared_fixture_attachment_receipt(json!({
                    "fixture_profile": PREPARED_FIXTURE_PROFILE,
                    "payload_bytes_copied": 0_u64,
                    "cached_allocation_count": PREPARED_FIXTURE_ALLOCATION_COUNT,
                    "attached_branches": [
                        "fixture-depth-5",
                        "fixture-depth-1",
                        "fixture-depth-8",
                    ],
                })),
            ),
            (
                "malformed branch",
                prepared_fixture_attachment_receipt(json!({
                    "fixture_profile": PREPARED_FIXTURE_PROFILE,
                    "payload_bytes_copied": 0_u64,
                    "cached_allocation_count": PREPARED_FIXTURE_ALLOCATION_COUNT,
                    "attached_branches": [
                        "fixture-depth-1",
                        5,
                        "fixture-depth-8",
                    ],
                })),
            ),
        ];
        for (case, receipt) in invalid {
            assert!(
                require_prepared_fixture_attachment(&receipt).is_err(),
                "{case} must fail closed"
            );
        }
    }

    fn initial_publication_receipt(semantic_entries: u64, semantic_bytes: u64) -> CliInvocation {
        CliInvocation {
            operation: "publish_mpla_workspace_session".to_owned(),
            request_id: Some("initial-publication".to_owned()),
            outer_elapsed_ns: 1,
            response: json!({
                "affected_path_count": 1,
                "affected_payload_bytes_read": 0,
                "immutable_payload_bytes_read": 0,
                "roots": {
                    "root_id": "content-root",
                    "attribution_root_id": "attribution-root",
                },
                "semantic": {
                    "schema_version": 1,
                    "semantic_format": "mpla-poc-semantic-v1",
                    "operation_id": "fixture-test-publication",
                    "entry_count": semantic_entries,
                    "bytes_read": semantic_bytes,
                    "record_stream_sha256": format!("{:064x}", 1),
                    "spool_runs": 1,
                    "spool_bytes": 1,
                    "peak_open_data_fds": 1,
                    "peak_data_workers": 1,
                    "phase_spans": [],
                    "roots": {
                        "root_id": "content-root",
                        "attribution_root_id": "attribution-root",
                    },
                    "durability": {
                        "root_manifest": "/tmp/fixture-test-root.json",
                        "semantic_attribution": {
                            "actor_id": "fixture-test",
                            "semantic_operation_id": "fixture-test-publication",
                        },
                        "immutable_object_count": 1,
                        "immutable_object_bytes": 1,
                        "object_set_sha256": format!("{:064x}", 2),
                        "files_fsynced": true,
                        "object_directory_fsynced": true,
                        "manifest_fsynced": true,
                        "manifest_directory_fsynced": true,
                    },
                },
                "stationary": {
                    "stable": {"after": {"logical_bytes": GIB}},
                    "no_second_payload_allocation": true,
                    "representative_inodes_unchanged": true,
                    "allocated_bytes_unchanged": true,
                },
                "lifecycle": {"idempotent_replay": false},
            }),
        }
    }

    fn incremental_publication_receipt(
        semantic_entries: u64,
        semantic_stream_bytes: u64,
        affected_paths: u64,
        affected_records: u64,
        affected_payload_bytes: u64,
        logical_bytes: u64,
    ) -> CliInvocation {
        let mut publication = initial_publication_receipt(semantic_entries, semantic_stream_bytes);
        publication.response["affected_path_count"] = json!(affected_paths);
        publication.response["affected_payload_bytes_read"] = json!(affected_payload_bytes);
        publication.response["semantic"]["affected_record_count"] = json!(affected_records);
        publication.response["semantic"]["affected_stream_bytes_read"] =
            json!(semantic_stream_bytes);
        publication.response["stationary"]["stable"]["after"]["logical_bytes"] =
            json!(logical_bytes);
        publication
    }

    #[test]
    fn incremental_dense_zero_fixture_accepts_hole_only_semantic_read() {
        let publication = incremental_publication_receipt(1, 0, 1, 1, 0, GIB);
        let expectation = PublicationExpectation::incremental_affected_paths(1, 0, GIB, 0);
        require_publication(&publication, &expectation)
            .expect("a complete zero-file semantic entry may require no physical payload read");

        let missing_entry = incremental_publication_receipt(0, 0, 1, 1, 0, GIB);
        assert!(require_publication(&missing_entry, &expectation).is_err());

        let oversized_read = incremental_publication_receipt(1, GIB + 1, 1, 1, 0, GIB);
        assert!(require_publication(&oversized_read, &expectation).is_err());
    }

    #[test]
    fn incremental_sparse_ten_file_candidate_requires_zero_payload_read() {
        let publication = incremental_publication_receipt(23, 1_737, 10, 21, 0, MIB);
        let sparse_expectation = PublicationExpectation::incremental_affected_paths(10, 0, MIB, 3);
        require_publication(&publication, &sparse_expectation)
            .expect("ten hole-only delta files must require no physical payload read");

        let obsolete_dense_expectation =
            PublicationExpectation::incremental_affected_paths(10, MIB, MIB, 3);
        assert!(require_publication(&publication, &obsolete_dense_expectation).is_err());
    }

    #[test]
    fn initial_holder_tree_fixture_accepts_the_shared_base_and_new_layer() {
        let publication = initial_publication_receipt(32_928, GIB + 4_585_047);
        let expectation = PublicationExpectation::initial_holder_tree(
            1,
            0,
            GIB,
            PublicationSemanticExpectation::InitialHolderTree {
                expected_entry_count: 32_928,
                max_bytes_read: GIB + 4_585_047,
            },
        );
        require_publication(&publication, &expectation).expect(
            "the holder-view initial receipt must cover the shared base as well as the new layer",
        );

        let wrong_entry_count = initial_publication_receipt(32_927, GIB + 4_585_047);
        assert!(require_publication(&wrong_entry_count, &expectation).is_err());

        let wrong_read_count = initial_publication_receipt(32_928, GIB + 4_585_048);
        assert!(require_publication(&wrong_read_count, &expectation).is_err());
    }

    #[test]
    fn closed_sparse_fixture_qualifies_one_receipt_with_a_bounded_scan() {
        let publication = initial_publication_receipt(32_928, GIB + 4_585_047);
        let semantic = require_closed_sparse_fixture_publication(&publication, 1, 0, GIB)
            .expect("the sparse builder must accept one complete durable receipt");
        assert_eq!(semantic.entry_count, 32_928);

        let incomplete = initial_publication_receipt(1, 0);
        assert!(require_closed_sparse_fixture_publication(&incomplete, 1, 0, GIB).is_err());

        let oversized =
            initial_publication_receipt(32_928, PREPARED_FIXTURE_DEPTH_EIGHT_BYTES + GIB + 1);
        assert!(require_closed_sparse_fixture_publication(&oversized, 1, 0, GIB).is_err());
    }

    #[test]
    fn incremental_publication_keeps_total_tree_and_affected_path_counts_distinct() {
        let publication = incremental_publication_receipt(32_831, 4_096, 10, 60, MIB, MIB);
        let expectation = PublicationExpectation::incremental_affected_paths(10, MIB, MIB, 32_771);
        require_publication(&publication, &expectation)
            .expect("a ten-path delta may expand into more semantic records than paths");

        let stale_total = incremental_publication_receipt(32_771, 4_096, 10, 60, MIB, MIB);
        assert!(require_publication(&stale_total, &expectation).is_err());

        let missing_affected_record =
            incremental_publication_receipt(32_831, 4_096, 10, 9, MIB, MIB);
        assert!(require_publication(&missing_affected_record, &expectation).is_err());
    }

    #[test]
    fn incremental_publication_requires_the_server_reported_affected_record_counter() {
        let mut publication = incremental_publication_receipt(32_831, 4_096, 10, 60, MIB, MIB);
        let expectation = PublicationExpectation::incremental_affected_paths(10, MIB, MIB, 32_771);
        publication.response["semantic"]
            .as_object_mut()
            .expect("fixture semantic receipt is an object")
            .remove("affected_record_count");

        assert!(require_publication(&publication, &expectation).is_err());
    }

    #[test]
    fn full_fixture_receipt_requires_every_exact_digest() {
        let fixture = fixture();
        assert_eq!(
            fixture_verification_command(),
            "sha256sum -- layer-000.bin delta-00.bin delta-01.bin delta-02.bin delta-03.bin delta-04.bin delta-05.bin delta-06.bin delta-07.bin delta-08.bin delta-09.bin"
        );
        let mut lines = vec![format!("{}  layer-000.bin", fixture.base_sha256)];
        lines.extend(
            fixture
                .delta_sha256
                .iter()
                .enumerate()
                .map(|(index, digest)| format!("{digest}  delta-{index:02}.bin")),
        );
        let valid = receipt(lines.join("\n"));
        require_full_fixture_digests(&valid, &fixture).expect("exact fixture must pass");

        let invalid = receipt(valid.response["output"].as_str().unwrap().replacen(
            &fixture.delta_sha256[9],
            &format!("{:064x}", 12),
            1,
        ));
        assert!(require_full_fixture_digests(&invalid, &fixture).is_err());
    }

    #[test]
    fn sparse_fixture_receipt_requires_exact_size_and_zero_blocks() {
        let valid = receipt(format!("sparse=layer-000.bin:{MIB}:0"));
        require_sparse_file_receipt(&valid, "layer-000.bin", MIB, "test")
            .expect("hole-only exact receipt must pass");

        let allocated = receipt(format!("sparse=layer-000.bin:{MIB}:1"));
        assert!(require_sparse_file_receipt(&allocated, "layer-000.bin", MIB, "test").is_err());

        let extra = receipt(format!("sparse=layer-000.bin:{MIB}:0\nunexpected"));
        assert!(require_sparse_file_receipt(&extra, "layer-000.bin", MIB, "test").is_err());
    }

    #[test]
    fn sparse_marker_receipt_requires_exact_gib_partition_and_zero_blocks() {
        let output = (0..10)
            .map(|index| {
                format!(
                    "sparse=marker-002-{index:02}.bin:{}:0",
                    partition_bytes(PREPARED_FIXTURE_MARKER_LAYER_BYTES, index)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let valid = receipt(output.clone());
        require_sparse_set_receipts(
            &valid,
            "marker-002",
            PREPARED_FIXTURE_MARKER_LAYER_BYTES,
            10,
            "marker test",
        )
        .expect("exact hole-only marker layer must pass");

        let allocated = receipt(output.replacen(".bin:107374183:0", ".bin:107374183:1", 1));
        assert!(require_sparse_set_receipts(
            &allocated,
            "marker-002",
            PREPARED_FIXTURE_MARKER_LAYER_BYTES,
            10,
            "marker test",
        )
        .is_err());
    }

    #[test]
    fn zero_file_writer_produces_hole_only_zero_content() {
        let root = std::env::temp_dir().join(format!("mpla-sparse-zero-{}", Uuid::new_v4()));
        fs::create_dir(&root).expect("test root");
        let path = root.join("sparse.bin");
        let observed = write_zero_file(&path, MIB).expect("write sparse zero file");
        let expected = format!("{:x}", Sha256::digest(vec![0_u8; MIB as usize]));
        assert_eq!(observed, expected);
        let metadata = fs::metadata(&path).expect("sparse file metadata");
        assert_eq!(metadata.len(), MIB);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;

            assert_eq!(metadata.blocks(), 0, "fixture file must remain hole-only");
        }
        fs::remove_dir_all(&root).expect("test root cleanup");
    }

    #[test]
    fn prepared_fixture_cache_bootstrap_layout() {
        let root = std::env::temp_dir().join(format!("mpla-cache-layout-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("layer-stack/.layer-metadata")).expect("metadata root");
        fs::create_dir_all(root.join("layer-stack/base")).expect("base root");
        fs::create_dir_all(root.join("layer-stack/layers")).expect("layers root");
        fs::create_dir_all(root.join("layer-stack/staging")).expect("staging root");
        fs::create_dir_all(root.join("storage/file_auditability")).expect("audit root");
        fs::create_dir_all(root.join("workspace")).expect("scratch root");
        fs::create_dir_all(root.join("workspace/0000010123456789abcdef/executions"))
            .expect("provider scratch session");
        let active_session = "0000020123456789abcdef";
        let active_root = root.join("workspace").join(active_session);
        for child in ["executions", "upper", "work"] {
            fs::create_dir_all(active_root.join(child)).expect("active provider scratch child");
        }
        fs::create_dir(active_root.join("work/work")).expect("active provider work child");
        fs::create_dir(active_root.join("executions/namespace_execution_1"))
            .expect("active provider execution leaf");
        fs::write(
            active_root.join("executions/namespace_execution_1/transcript.log"),
            "",
        )
        .expect("active provider execution transcript");
        fs::write(root.join("layer-stack/manifest.json"), "{}").expect("manifest");
        fs::write(root.join("layer-stack/workspace.json"), "{}").expect("binding");
        fs::write(root.join("layer-stack/.storage-writer.lock"), "").expect("writer lock");
        fs::write(
            root.join("workspace/manager.json"),
            serde_json::to_vec(&json!({
                "schema_version": 2,
                "handles": [{
                    "workspace_handle_id": active_session,
                    "lease_id": "lease-test",
                    "parked_lease_id": null,
                    "candidate_admission": null,
                    "manifest_version": 1,
                    "manifest_root_hash": "0123456789abcdef",
                    "network_profile": "shared",
                    "workspace_root": "/workspace",
                    "scratch_dir": active_root,
                    "upperdir": active_root.join("upper"),
                    "workdir": active_root.join("work"),
                    "layer_paths": ["/eos/mpla-fixtures/s4-chain-sparse-v1/layer-stack/base/B000001-base"],
                    "holder_pid": 42,
                    "veth_host_name": null,
                    "veth_ns_name": null,
                    "ns_ip": null,
                    "created_at": 1.0,
                    "last_activity": 1.0,
                }],
            }))
            .expect("provider manager metadata"),
        )
        .expect("write provider manager metadata");

        assert!(recover_unsealed_prepared_fixture_cache(&root)
            .expect("provider bootstrap is reusable")
            .is_empty());
        let stale_execution =
            root.join("workspace/0000010123456789abcdef/executions/namespace_execution_1");
        fs::create_dir(&stale_execution).expect("stale execution leaf");
        assert!(recover_unsealed_prepared_fixture_cache(&root).is_err());
        fs::remove_dir(&stale_execution).expect("stale execution cleanup");
        fs::create_dir_all(root.join("layer-stack/mpla-poc/payload"))
            .expect("partial fixture payload");
        let recovered =
            recover_unsealed_prepared_fixture_cache(&root).expect("partial fixture recovery");
        assert_eq!(recovered.len(), 1);
        assert!(!root.join("layer-stack/mpla-poc").exists());
        fs::write(root.join("workspace/manager.json"), "{}").expect("corrupt manager metadata");
        assert!(recover_unsealed_prepared_fixture_cache(&root).is_err());

        fs::remove_dir_all(&root).expect("test root cleanup");
    }

    #[test]
    fn unsealed_recovery_rejects_symlinked_or_unknown_cache_roots_without_removal() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("mpla-cache-symlink-{}", Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("mpla-cache-outside-{}", Uuid::new_v4()));
        fs::create_dir(&root).expect("cache root");
        fs::create_dir(&outside).expect("outside root");
        fs::write(outside.join("sentinel"), "preserve").expect("outside sentinel");
        symlink(&outside, root.join("control-source")).expect("symlinked cache root");

        assert!(recover_unsealed_prepared_fixture_cache(&root).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("sentinel")).expect("outside sentinel survives"),
            "preserve"
        );

        fs::remove_file(root.join("control-source")).expect("symlink cleanup");
        fs::create_dir(root.join("unknown-partial")).expect("unknown partial root");
        assert!(recover_unsealed_prepared_fixture_cache(&root).is_err());
        assert!(root.join("unknown-partial").is_dir());

        fs::remove_dir_all(&root).expect("cache root cleanup");
        fs::remove_dir_all(&outside).expect("outside root cleanup");
    }

    #[test]
    fn unsealed_recovery_is_atomic_when_any_partial_root_is_invalid() {
        let root = std::env::temp_dir().join(format!("mpla-cache-atomic-{}", Uuid::new_v4()));
        let recoverable = root.join("control-source");
        let unexpected = root.join("unexpected-partial");
        fs::create_dir_all(&recoverable).expect("recoverable partial root");
        fs::write(recoverable.join("sentinel"), "preserve").expect("recoverable sentinel");
        fs::create_dir_all(&unexpected).expect("unexpected partial root");
        fs::write(unexpected.join("sentinel"), "preserve").expect("unexpected sentinel");

        assert!(recover_unsealed_prepared_fixture_cache(&root).is_err());
        assert_eq!(
            fs::read_to_string(recoverable.join("sentinel"))
                .expect("recoverable sentinel survives validation failure"),
            "preserve"
        );
        assert_eq!(
            fs::read_to_string(unexpected.join("sentinel"))
                .expect("unexpected sentinel survives validation failure"),
            "preserve"
        );

        fs::remove_dir_all(&root).expect("cache root cleanup");
    }

    #[test]
    fn unsealed_recovery_removes_only_exact_fixture_roots_and_is_idempotent() {
        let root = std::env::temp_dir().join(format!("mpla-cache-idempotent-{}", Uuid::new_v4()));
        let partials = [
            root.join("control-source"),
            root.join("layer-stack/mpla-poc"),
            root.join("workspace/mpla-poc"),
        ];
        for partial in &partials {
            fs::create_dir_all(partial).expect("create exact partial fixture root");
            fs::write(partial.join("sentinel"), "remove").expect("partial fixture sentinel");
        }

        let removed =
            recover_unsealed_prepared_fixture_cache(&root).expect("recover exact fixture roots");
        assert_eq!(removed.len(), partials.len());
        for partial in &partials {
            assert!(
                !partial.exists(),
                "exact fixture-owned partial must be removed: {}",
                partial.display()
            );
        }
        assert!(
            root.join("layer-stack").is_dir(),
            "provider layer-stack root must remain"
        );
        assert!(
            root.join("workspace").is_dir(),
            "provider workspace root must remain"
        );
        assert!(
            recover_unsealed_prepared_fixture_cache(&root)
                .expect("repeat recovery")
                .is_empty(),
            "repeat recovery must be an idempotent no-op"
        );

        fs::remove_dir_all(&root).expect("cache root cleanup");
    }

    #[test]
    fn existing_seal_is_never_rebuilt_or_repaired() {
        let root = std::env::temp_dir().join(format!("mpla-cache-sealed-{}", Uuid::new_v4()));
        let manifest_path = root.join("PREPARED-FIXTURE.json");
        let payload_sentinel = root.join("payload-sentinel");
        fs::create_dir_all(&root).expect("sealed cache root");
        fs::write(&manifest_path, "{}").expect("sealed cache manifest");
        fs::write(&payload_sentinel, "preserve").expect("sealed cache payload sentinel");

        let valid_error = reject_existing_prepared_fixture_seal_at(&manifest_path, || Ok(()))
            .expect_err("a valid seal must prevent a second build");
        assert!(valid_error.to_string().contains("already sealed"));
        assert_eq!(
            fs::read_to_string(&payload_sentinel).expect("valid sealed cache survives"),
            "preserve"
        );

        let corrupt_error =
            reject_existing_prepared_fixture_seal_at(&manifest_path, || -> PublicationResult {
                Err("corrupt layout".into())
            })
            .expect_err("a corrupt seal must fail closed");
        assert!(
            corrupt_error
                .to_string()
                .contains("automatic recovery is forbidden"),
            "unexpected corrupt-seal error: {corrupt_error}"
        );
        assert_eq!(
            fs::read_to_string(&payload_sentinel).expect("corrupt sealed cache survives"),
            "preserve"
        );
        assert!(
            manifest_path.exists(),
            "a corrupt seal must remain for operator inspection"
        );

        fs::remove_dir_all(&root).expect("sealed cache root cleanup");
    }
}
