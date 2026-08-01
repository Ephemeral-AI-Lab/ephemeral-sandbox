use std::collections::BTreeSet;
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sandbox_runtime_mpla_poc::{
    bind_product_catalog, collect_control_changes, run_current_i2_closing,
    run_current_i2_materialization, ControlBoundary, ControlCacheExpectation, ControlCacheMatch,
    ControlCollectionLimits, ControlIntent, ControlOperationReceipt, ControlSourceProfile,
    ControlVerdict, CurrentI2ClosingRequest, CurrentI2MaterializationRequest,
    ExternalReadinessReceipt, SCHEMA_VERSION,
    STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID, STORAGE_ADMIN_PROFILE_ID,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

type ScorecardResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const BASE_ROOT: &str = "/eos/layer-stack/base/B000001-base";
const TOOL_ROOT: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools";
const R0_ROOT: &str = "/eos/layer-stack/base/B000001-base/r0";
const RUNTIME_CLI: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/sandbox-runtime-cli";
const TOKEN_FILE: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/gateway.token";
const CATALOG_EXPORTER: &str =
    "/eos/layer-stack/base/B000001-base/_campaign-tools/sandbox-catalog-export";
const PRODUCT_CATALOG: &str =
    "/eos/layer-stack/base/B000001-base/_campaign-tools/product-catalog.json";
const FIXTURE_BUILDER_TOOL_ROOT: &str =
    "/eos/mpla-fixtures/s4-chain-sparse-v1/layer-stack/base/B000001-base/_campaign-tools";
const STAGED_SCORECARD_TOOL_ROOT: &str = "/workspace/_campaign-tools";
const CONTROL_PREPARATION_ROOT: &str = "/eos/workspace/mpla-poc/scorecard-control-preparations";
const CONTROL_PREPARATION_RECEIPT: &str = "receipt.json";
const CONTROL_PREPARATION_RECEIPT_MAX_BYTES: u64 = 1024 * 1024;
const CONTROL_PREPARATION_CHECKSUM_DOMAIN: &[u8] = b"EOS-MPLA-SCORECARD-CONTROL-PREPARATION-V1\0";
const DEFAULT_GATEWAY_SOCKET: &str = "host.docker.internal:7881";
const RUNTIME_GATEWAY_SOCKET_ENV: &str = "MPLA_RUNTIME_GATEWAY_SOCKET";
const ACTIVATE_REQUIRED_NS: u64 = 99_876_753;
const ACTIVATE_PREFERRED_NS: u64 = 19_975_350;
const SAME_REQUIRED_NS: u64 = 50_000_000;
const SAME_PREFERRED_NS: u64 = 20_000_000;
const FORK_REQUIRED_NS: u64 = 10_000_000;
const FORK_PREFERRED_NS: u64 = 2_000_000;
const ROLLBACK_REQUIRED_NS: u64 = 20_000_000;
const ROLLBACK_PREFERRED_NS: u64 = 10_000_000;
const SELECTOR_REQUIRED_NS: u64 = 1_000_000;
const SQUASH_REQUIRED_NS: u64 = 10_000_000;
const ACTIVE_COMMON_OPERATIONS_FILE: &[u8] = b"NONTERMINAL";
const ACTIVE_COMMON_OPERATIONS_MAGIC: &[u8] = b"EOS-LS3-NONTERMINAL-COMMON-OPERATIONS\0";
const ACTIVE_COMMON_OPERATIONS_CHECKSUM_DOMAIN: &[u8] =
    b"EOS-LS3-NONTERMINAL-COMMON-OPERATIONS-CHECKSUM\0";
const ACTIVE_COMMON_OPERATIONS_MAX_BYTES: u64 = 4_096;
const MAX_NONTERMINAL_COMMON_OPERATIONS: usize = 64;
/// Keep a single progress line comfortably below the public `file_read`
/// response bound even when a rejected operation includes a large JSON reply.
const PROGRESS_ERROR_SEGMENT_BYTES: usize = 24 * 1024;
const RUNTIME_CLI_BATCH_READY_KIND: &str = "sandbox_runtime_cli_batch_ready_v1";
const RUNTIME_CLI_BATCH_RESPONSE_KIND: &str = "sandbox_runtime_cli_batch_response_v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct CliInvocation {
    pub(super) operation: String,
    pub(super) request_id: Option<String>,
    pub(super) outer_elapsed_ns: u64,
    pub(super) response: Value,
}

#[derive(Debug, Serialize)]
pub(super) struct CliFailure {
    pub(super) operation: String,
    pub(super) request_id: Option<String>,
    pub(super) outer_elapsed_ns: u64,
    pub(super) exit_code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct OracleValidation {
    pub(super) oracle_tree: String,
    pub(super) activation: Option<CliInvocation>,
    pub(super) outer_elapsed_ns: u64,
    pub(super) exit_code: Option<i32>,
    pub(super) stderr: String,
    pub(super) summary: Value,
    pub(super) exact_match: bool,
    pub(super) fixture_verification: Option<CliInvocation>,
    pub(super) storage_cleanup: Vec<CliInvocation>,
    pub(super) destroy: Option<CliInvocation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CandidateR0Preparation {
    workspace_session_id: String,
    storage_admin_profile_id: String,
    storage_admin_scope: Value,
    create: CliInvocation,
    mount: CliInvocation,
    copy: CliInvocation,
    publication: CliInvocation,
    oracle: OracleValidation,
    elapsed_ns: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct ActivationSample {
    label: String,
    outer_elapsed_ns: u64,
    service_elapsed_ns: u64,
    workspace_session_id: String,
    fresh_allocation_id: String,
    selected_ref: String,
    projection: Value,
    timings: Value,
}

#[derive(Debug, Serialize)]
struct LifecycleGate {
    gate: String,
    candidate_ns: Vec<u64>,
    control_ns: Vec<u64>,
    candidate_median_ns: u64,
    candidate_max_ns: u64,
    control_median_ns: u64,
    median_ratio_numerator: u64,
    median_ratio_denominator: u64,
    required: bool,
    preferred: bool,
}

#[derive(Debug, Serialize)]
struct AbsoluteGate {
    gate: String,
    outer_ns: Vec<u64>,
    service_ns: Vec<u64>,
    required: bool,
}

#[derive(Debug, Serialize)]
struct CandidateChecks {
    selected_refs_stable: bool,
    projections_exact_zero_build: bool,
    allocations_fresh: bool,
    lower_allocations_stable: bool,
}

struct SquashPhaseSetup {
    candidate: CandidateR0Preparation,
    client: RuntimeClient,
    baseline_activation: ActivationSample,
    elapsed_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleControlPreparationPayload {
    schema_version: u32,
    kind: String,
    run_id: String,
    phase: String,
    candidate_sandbox_id: String,
    build_commit: String,
    state_root: PathBuf,
    catalog_binding_id: String,
    fixture: ControlSourceProfile,
    readiness_path: PathBuf,
    closing: ControlOperationReceipt,
    candidate: CandidateR0Preparation,
    collection_elapsed_ns: u64,
    closing_publication_elapsed_ns: u64,
    candidate_preparation_elapsed_ns: u64,
    preparation_elapsed_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleControlPreparationReceipt {
    payload: LifecycleControlPreparationPayload,
    checksum_sha256: String,
}

impl CandidateChecks {
    fn required(&self) -> bool {
        self.selected_refs_stable
            && self.projections_exact_zero_build
            && self.allocations_fresh
            && self.lower_allocations_stable
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LifecyclePhase {
    Activation,
    Fork,
    Rollback,
    Squash,
}

impl LifecyclePhase {
    fn from_control_preparation_name(value: &str) -> ScorecardResult<Self> {
        match value {
            "activation" => Ok(Self::Activation),
            "fork" => Ok(Self::Fork),
            "rollback" => Ok(Self::Rollback),
            _ => {
                Err(format!("phase does not support lifecycle control preparation: {value}").into())
            }
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Activation => "activation",
            Self::Fork => "fork",
            Self::Rollback => "rollback",
            Self::Squash => "squash",
        }
    }

    const fn runner(self) -> &'static str {
        match self {
            Self::Activation => "mpla_activation_scorecard",
            Self::Fork => "mpla_fork_scorecard",
            Self::Rollback => "mpla_rollback_scorecard",
            Self::Squash => "mpla_squash_scorecard",
        }
    }

    const fn result_file(self) -> &'static str {
        match self {
            Self::Activation => "scorecard-activation-result.json",
            Self::Fork => "scorecard-fork-result.json",
            Self::Rollback => "scorecard-rollback-result.json",
            Self::Squash => "scorecard-squash-result.json",
        }
    }

    const fn progress_file(self) -> &'static str {
        match self {
            Self::Activation => "scorecard-activation-progress.jsonl",
            Self::Fork => "scorecard-fork-progress.jsonl",
            Self::Rollback => "scorecard-rollback-progress.jsonl",
            Self::Squash => "scorecard-squash-progress.jsonl",
        }
    }

    const fn suggested_budget_seconds(self) -> u64 {
        match self {
            Self::Activation => 60,
            Self::Fork | Self::Rollback | Self::Squash => 30,
        }
    }

    const fn selected_multiplier_milli(self) -> u64 {
        2_000
    }

    const fn phase_cap_seconds(self) -> u64 {
        self.suggested_budget_seconds() * self.selected_multiplier_milli() / 1_000
    }

    const fn kind(self) -> &'static str {
        match self {
            Self::Activation => "mpla_booster_activation_scorecard_v1",
            Self::Fork => "mpla_booster_fork_scorecard_v1",
            Self::Rollback => "mpla_booster_rollback_scorecard_v1",
            Self::Squash => "mpla_booster_squash_scorecard_v1",
        }
    }

    const fn prepares_candidate_locally_before_phase(self) -> bool {
        matches!(self, Self::Squash)
    }
}

pub fn prepare_lifecycle_control(
    run_id: &str,
    phase_name: &str,
    candidate_sandbox_id: &str,
    build_commit: &str,
) -> ScorecardResult<Value> {
    let started = Instant::now();
    validate_identifier(run_id, "run_id")?;
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    validate_build_commit(build_commit)?;
    let phase = LifecyclePhase::from_control_preparation_name(phase_name)?;
    require_regular_file(Path::new(CATALOG_EXPORTER), "catalog exporter")?;
    require_regular_file(Path::new(PRODUCT_CATALOG), "product catalog")?;
    let r0_root = Path::new(R0_ROOT);
    require_real_directory(r0_root, "R0 fixture")?;

    let preparation_root = control_preparation_root(run_id, phase);
    let preparation_parent = preparation_root
        .parent()
        .ok_or("control preparation root lacks a parent")?;
    fs::create_dir_all(preparation_parent)?;
    require_real_directory(preparation_parent, "control preparation parent")?;
    fs::create_dir(&preparation_root)?;
    let state_root = preparation_root.join("state");
    fs::create_dir(&state_root)?;

    let catalog_binding = bind_product_catalog(
        Path::new(CATALOG_EXPORTER),
        Path::new(PRODUCT_CATALOG),
        build_commit,
    )?;
    let collection_started = Instant::now();
    let changes = collect_control_changes(
        r0_root,
        &ControlCollectionLimits {
            max_entries: 8 * 1024,
            max_logical_bytes: 2 * 1024 * 1024 * 1024,
            max_path_bytes: 4 * 1024,
        },
    )?;
    let collection_elapsed_ns = elapsed_ns(collection_started.elapsed());
    let readiness_path = select_readiness_path(r0_root)?;
    sandbox_runtime_layerstack::reset_process_state_for_tests();
    let closing_started = Instant::now();
    let closing = run_current_i2_closing(
        &CurrentI2ClosingRequest {
            state_root: state_root.clone(),
            publication_id: [1; 16],
            public_root_hash: changes.profile.source_manifest_sha256.clone(),
            catalog_binding: catalog_binding.clone(),
            boundary: control_boundary(
                ControlCacheMatch::NotApplicable,
                "closed R0 corpus",
                "durable hidden publication",
            ),
        },
        &changes,
    )?;
    let closing_publication_elapsed_ns = elapsed_ns(closing_started.elapsed());
    sandbox_runtime_layerstack::reset_process_state_for_tests();
    require_control_cache_cold(&state_root)?;
    let candidate = prepare_candidate_r0(run_id, phase, candidate_sandbox_id, &changes.profile)?;

    let payload = LifecycleControlPreparationPayload {
        schema_version: SCHEMA_VERSION,
        kind: "mpla_booster_lifecycle_control_preparation_v1".to_owned(),
        run_id: run_id.to_owned(),
        phase: phase.name().to_owned(),
        candidate_sandbox_id: candidate_sandbox_id.to_owned(),
        build_commit: build_commit.to_owned(),
        state_root,
        catalog_binding_id: catalog_binding.binding_id,
        fixture: changes.profile,
        readiness_path,
        closing,
        candidate_preparation_elapsed_ns: candidate.elapsed_ns,
        candidate,
        collection_elapsed_ns,
        closing_publication_elapsed_ns,
        preparation_elapsed_ns: elapsed_ns(started.elapsed()),
    };
    let receipt = LifecycleControlPreparationReceipt {
        checksum_sha256: control_preparation_checksum(&payload)?,
        payload,
    };
    write_control_preparation_receipt(&preparation_root, &receipt)?;
    Ok(json!({
        "schema_version": 1,
        "kind": "mpla_booster_lifecycle_control_preparation_summary_v1",
        "run_id": receipt.payload.run_id,
        "phase": receipt.payload.phase,
        "candidate_sandbox_id": receipt.payload.candidate_sandbox_id,
        "build_commit": receipt.payload.build_commit,
        "state_root": receipt.payload.state_root,
        "catalog_binding_id": receipt.payload.catalog_binding_id,
        "fixture_entries": receipt.payload.fixture.entries,
        "fixture_logical_bytes": receipt.payload.fixture.logical_bytes,
        "source_manifest_sha256": receipt.payload.fixture.source_manifest_sha256,
        "control_immutable_publication_count": 1,
        "candidate_immutable_publication_count": 1,
        "immutable_publication_count": 2,
        "control_pre_materialized_carrier_count": 0,
        "candidate_oracle_materialization_count": 1,
        "collection_elapsed_ns": receipt.payload.collection_elapsed_ns,
        "closing_publication_elapsed_ns": receipt.payload.closing_publication_elapsed_ns,
        "candidate_preparation_elapsed_ns": receipt.payload.candidate_preparation_elapsed_ns,
        "preparation_elapsed_ns": receipt.payload.preparation_elapsed_ns,
        "receipt_checksum_sha256": receipt.checksum_sha256,
    }))
}

fn prepare_candidate_r0(
    run_id: &str,
    phase: LifecyclePhase,
    candidate_sandbox_id: &str,
    fixture: &ControlSourceProfile,
) -> ScorecardResult<CandidateR0Preparation> {
    let started = Instant::now();
    let client = RuntimeClient::new(candidate_sandbox_id)?;
    let create = client.invoke(
        Some(&format!("{run_id}-create-r0")),
        "create_mpla_workspace_session",
        &["--run-id".to_owned(), run_id.to_owned()],
    )?;
    let workspace_session_id = required_string(
        &create.response,
        "workspace_session_id",
        "prepared R0 create",
    )?;
    validate_identifier(&workspace_session_id, "prepared R0 workspace session")?;
    let storage_admin_profile_id = approved_storage_profile(
        &required_string(
            &create.response,
            "storage_admin_profile_id",
            "prepared R0 create",
        )?,
        "prepared R0 create",
    )?;
    let storage_admin_scope = create
        .response
        .get("storage_admin_scope")
        .cloned()
        .ok_or("prepared R0 create omitted storage_admin_scope")?;
    let mount_operation_id = format!("{run_id}-mount-r0");
    let mount_request = json!({
        "schema_version": 1,
        "interface_version": "m2r-iface-v1",
        "profile_id": storage_admin_profile_id,
        "operation_id": mount_operation_id,
        "action": "mount",
        "scope": storage_admin_scope,
    });
    let mount = client.invoke(
        Some(&mount_operation_id),
        "mpla_storage_admin",
        &[serde_json::to_string(&mount_request)?],
    )?;
    let copy = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.clone(),
            "--timeout-ms".to_owned(),
            "120000".to_owned(),
            "--yield-time-ms".to_owned(),
            "120000".to_owned(),
            format!("cp -a {R0_ROOT}/. ."),
        ],
    )?;
    require_command_exit(&copy.response, "prepared R0 copy")?;
    let publication = client.invoke(
        Some(&format!("{run_id}-publish-r0")),
        "publish_mpla_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.clone(),
            "--branch".to_owned(),
            initial_publication_oracle_branch().to_owned(),
        ],
    )?;
    require_initial_r0_publication(&publication, fixture)?;
    let oracle = validate_merged_publication_oracle(
        &client,
        run_id,
        &format!("{}-r0", phase.name()),
        initial_publication_oracle_branch(),
        &publication,
        None,
    )?;
    let candidate = compact_candidate_r0_preparation(CandidateR0Preparation {
        workspace_session_id,
        storage_admin_profile_id,
        storage_admin_scope,
        create,
        mount,
        copy,
        publication,
        oracle,
        elapsed_ns: elapsed_ns(started.elapsed()),
    })?;
    validate_candidate_r0_preparation(&candidate, fixture)?;
    Ok(candidate)
}

fn control_preparation_root(run_id: &str, phase: LifecyclePhase) -> PathBuf {
    Path::new(CONTROL_PREPARATION_ROOT).join(format!("{run_id}-{}", phase.name()))
}

fn compact_candidate_r0_preparation(
    mut candidate: CandidateR0Preparation,
) -> ScorecardResult<CandidateR0Preparation> {
    candidate.create = compact_invocation(
        &candidate.create,
        json!({
            "workspace_session_id": candidate.workspace_session_id,
            "storage_admin_profile_id": candidate.storage_admin_profile_id,
            "storage_admin_scope": candidate.storage_admin_scope,
        }),
    )?;
    candidate.mount = compact_invocation(
        &candidate.mount,
        json!({
            "action": candidate.mount.response.get("action"),
            "cleanup_complete": candidate.mount.response.get("cleanup_complete"),
            "failure": candidate.mount.response.get("failure"),
            "operation_id": candidate.mount.response.get("operation_id"),
            "profile_id": candidate.mount.response.get("profile_id"),
            "scope": candidate.mount.response.get("scope"),
            "mount_attestation_sha256": json_sha256(
                candidate
                    .mount
                    .response
                    .get("mount_attestation")
                    .ok_or("prepared R0 mount omitted mount_attestation")?,
            )?,
        }),
    )?;
    candidate.copy = compact_invocation(
        &candidate.copy,
        json!({
            "status": candidate.copy.response.get("status"),
            "exit_code": candidate.copy.response.get("exit_code"),
            "end_offset": candidate.copy.response.get("end_offset"),
            "total_lines": candidate.copy.response.get("total_lines"),
        }),
    )?;
    candidate.publication = compact_invocation(
        &candidate.publication,
        json!({
            "affected_path_count": candidate.publication.response.get("affected_path_count"),
            "affected_payload_bytes_read": candidate
                .publication
                .response
                .get("affected_payload_bytes_read"),
            "roots": candidate.publication.response.get("roots"),
            "semantic": {
                "bytes_read": candidate.publication.response.pointer("/semantic/bytes_read"),
                "durability": candidate.publication.response.pointer("/semantic/durability"),
                "entry_count": candidate.publication.response.pointer("/semantic/entry_count"),
                "record_stream_sha256": candidate
                    .publication
                    .response
                    .pointer("/semantic/record_stream_sha256"),
                "roots": candidate.publication.response.pointer("/semantic/roots"),
            },
            "stationary": {
                "allocated_bytes_unchanged": candidate
                    .publication
                    .response
                    .pointer("/stationary/allocated_bytes_unchanged"),
                "no_second_payload_allocation": candidate
                    .publication
                    .response
                    .pointer("/stationary/no_second_payload_allocation"),
                "representative_inodes_unchanged": candidate
                    .publication
                    .response
                    .pointer("/stationary/representative_inodes_unchanged"),
                "stable": {
                    "after": {
                        "logical_bytes": candidate
                            .publication
                            .response
                            .pointer("/stationary/stable/after/logical_bytes"),
                    },
                },
            },
        }),
    )?;
    candidate.oracle.activation = candidate
        .oracle
        .activation
        .take()
        .map(|activation| {
            compact_invocation(
                &activation,
                json!({
                    "workspace_session_id": activation.response.get("workspace_session_id"),
                    "storage_admin_profile_id": activation
                        .response
                        .get("storage_admin_profile_id"),
                    "storage_admin_scope": activation.response.get("storage_admin_scope"),
                }),
            )
        })
        .transpose()?;
    candidate.oracle.fixture_verification = candidate
        .oracle
        .fixture_verification
        .take()
        .map(|verification| {
            compact_invocation(
                &verification,
                json!({
                    "status": verification.response.get("status"),
                    "exit_code": verification.response.get("exit_code"),
                    "end_offset": verification.response.get("end_offset"),
                    "total_lines": verification.response.get("total_lines"),
                }),
            )
        })
        .transpose()?;
    candidate.oracle.storage_cleanup = candidate
        .oracle
        .storage_cleanup
        .drain(..)
        .map(|cleanup| {
            compact_invocation(
                &cleanup,
                json!({
                    "action": cleanup.response.get("action"),
                    "cleanup_complete": cleanup.response.get("cleanup_complete"),
                    "failure": cleanup.response.get("failure"),
                    "operation_id": cleanup.response.get("operation_id"),
                    "profile_id": cleanup.response.get("profile_id"),
                }),
            )
        })
        .collect::<ScorecardResult<Vec<_>>>()?;
    candidate.oracle.destroy = candidate
        .oracle
        .destroy
        .take()
        .map(|destroy| {
            compact_invocation(
                &destroy,
                json!({
                    "destroyed": destroy.response.get("destroyed"),
                }),
            )
        })
        .transpose()?;
    Ok(candidate)
}

fn compact_invocation(
    invocation: &CliInvocation,
    mut proof: Value,
) -> ScorecardResult<CliInvocation> {
    let proof_object = proof
        .as_object_mut()
        .ok_or("compact invocation proof is not an object")?;
    proof_object.insert(
        "full_response_sha256".to_owned(),
        Value::String(json_sha256(&invocation.response)?),
    );
    proof_object.insert(
        "proof_kind".to_owned(),
        Value::String("mpla_compact_cli_invocation_proof_v1".to_owned()),
    );
    Ok(CliInvocation {
        operation: invocation.operation.clone(),
        request_id: invocation.request_id.clone(),
        outer_elapsed_ns: invocation.outer_elapsed_ns,
        response: proof,
    })
}

fn json_sha256(value: &Value) -> ScorecardResult<String> {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(value)?);
    Ok(format!("{:x}", digest.finalize()))
}

fn control_preparation_checksum(
    payload: &LifecycleControlPreparationPayload,
) -> ScorecardResult<String> {
    let encoded = serde_json::to_vec(payload)?;
    let mut digest = Sha256::new();
    digest.update(CONTROL_PREPARATION_CHECKSUM_DOMAIN);
    digest.update(encoded);
    Ok(format!("{:x}", digest.finalize()))
}

fn write_control_preparation_receipt(
    preparation_root: &Path,
    receipt: &LifecycleControlPreparationReceipt,
) -> ScorecardResult {
    let receipt_path = preparation_root.join(CONTROL_PREPARATION_RECEIPT);
    let temporary_path = preparation_root.join(".receipt.json.tmp");
    let encoded = serde_json::to_vec_pretty(receipt)?;
    if u64::try_from(encoded.len()).unwrap_or(u64::MAX) > CONTROL_PREPARATION_RECEIPT_MAX_BYTES {
        return Err("control preparation receipt exceeds its hard byte bound".into());
    }
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(&temporary_path)?;
    file.write_all(&encoded)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary_path, &receipt_path)?;
    sync_directory(preparation_root)
}

pub(super) struct RuntimeClient {
    batch: Mutex<RuntimeCliBatch>,
}

struct RuntimeCliBatch {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
}

#[derive(Debug, Serialize)]
struct RuntimeCliBatchRequest<'a> {
    schema_version: u32,
    request_id: Option<&'a str>,
    operation: &'a str,
    operation_argv: &'a [String],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeCliBatchResponse {
    kind: String,
    exit_code: u8,
    stdout: String,
    stderr: String,
}

struct ProgressLedger {
    file: File,
    kind: String,
}

impl ProgressLedger {
    fn create(
        run_id: &str,
        candidate_sandbox_id: &str,
        build_commit: &str,
        phase: LifecyclePhase,
    ) -> ScorecardResult<Self> {
        let path = Path::new("/workspace").join(phase.progress_file());
        let file = File::options().create_new(true).write(true).open(path)?;
        let mut ledger = Self {
            file,
            kind: format!("mpla_booster_{}_progress_v1", phase.name()),
        };
        ledger.record(
            "started",
            json!({
                "run_id": run_id,
                "candidate_sandbox_id": candidate_sandbox_id,
                "build_commit": build_commit,
                "phase": phase.name(),
                "runner": phase.runner(),
                "suggested_budget_seconds": phase.suggested_budget_seconds(),
                "selected_multiplier_milli": phase.selected_multiplier_milli(),
                "calculated_phase_cap_seconds": phase.phase_cap_seconds(),
                "bounded_work": format!("{} controls and candidate operations", phase.name()),
            }),
        )?;
        Ok(ledger)
    }

    fn record(&mut self, stage: &str, details: Value) -> ScorecardResult {
        serde_json::to_writer(
            &mut self.file,
            &json!({
                "schema_version": 1,
                "kind": self.kind,
                "stage": stage,
                "details": details,
            }),
        )?;
        self.file.write_all(b"\n")?;
        self.file.sync_data()?;
        Ok(())
    }
}

fn bounded_progress_error(error: impl std::fmt::Display) -> Value {
    let rendered = error.to_string();
    let rendered_len = rendered.len();
    let digest = format!("{:x}", Sha256::digest(rendered.as_bytes()));
    if rendered_len <= PROGRESS_ERROR_SEGMENT_BYTES * 2 {
        return json!({
            "error": rendered,
            "error_bytes": rendered_len,
            "error_sha256": digest,
        });
    }
    let head_end = utf8_prefix_end(&rendered, PROGRESS_ERROR_SEGMENT_BYTES);
    let tail_start = utf8_suffix_start(&rendered, PROGRESS_ERROR_SEGMENT_BYTES);
    json!({
        "error": "diagnostic truncated; verify the complete message with error_sha256",
        "error_bytes": rendered_len,
        "error_sha256": digest,
        "error_head": &rendered[..head_end],
        "error_tail": &rendered[tail_start..],
    })
}

fn utf8_prefix_end(value: &str, maximum: usize) -> usize {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn utf8_suffix_start(value: &str, maximum: usize) -> usize {
    let mut start = value.len().saturating_sub(maximum);
    while !value.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// The daemon chooses the helper profile; scorecards only accept the two
/// compiled, server-owned choices and echo the value returned in the session
/// receipt. This keeps a campaign from self-selecting arbitrary capabilities.
pub(super) fn approved_storage_profile(profile: &str, label: &str) -> ScorecardResult<String> {
    match profile {
        STORAGE_ADMIN_PROFILE_ID
        | STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID => Ok(profile.to_owned()),
        _ => Err(format!("{label} selected an unapproved storage-admin profile {profile}").into()),
    }
}

impl RuntimeClient {
    pub(super) fn new(candidate_sandbox_id: &str) -> ScorecardResult<Self> {
        let runtime_cli = campaign_tool_path("sandbox-runtime-cli")?;
        let token = fs::read_to_string(campaign_tool_path("gateway.token")?)?;
        let token = token.trim().to_owned();
        if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err("gateway token file is empty or malformed".into());
        }
        let gateway_socket = std::env::var(RUNTIME_GATEWAY_SOCKET_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_GATEWAY_SOCKET.to_owned());
        let gateway_socket = approved_runtime_gateway_socket(&gateway_socket)?;
        let mut command = Command::new(runtime_cli);
        command
            .arg("--gateway-socket")
            .arg(gateway_socket)
            .arg("--gateway-auth-token")
            .arg(token)
            .arg("--sandbox-id")
            .arg(candidate_sandbox_id)
            .arg("--batch-jsonl")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or("sandbox-runtime-cli batch stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("sandbox-runtime-cli batch stdout was not piped")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("sandbox-runtime-cli batch stderr was not piped")?;
        let mut batch = RuntimeCliBatch {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr: Some(stderr),
        };
        let mut ready_line = String::new();
        if batch.stdout.read_line(&mut ready_line)? == 0 {
            return Err(batch.terminated_error("before its ready receipt"));
        }
        let ready: Value = serde_json::from_str(&ready_line)?;
        if ready.get("kind").and_then(Value::as_str) != Some(RUNTIME_CLI_BATCH_READY_KIND) {
            return Err(format!(
                "sandbox-runtime-cli returned an invalid batch ready receipt: {ready_line:?}"
            )
            .into());
        }
        Ok(Self {
            batch: Mutex::new(batch),
        })
    }

    pub(super) fn invoke(
        &self,
        request_id: Option<&str>,
        operation: &str,
        operation_args: &[String],
    ) -> ScorecardResult<CliInvocation> {
        let started = Instant::now();
        let response = self
            .batch
            .lock()
            .map_err(|_| "sandbox-runtime-cli batch lock was poisoned")?
            .invoke(request_id, operation, operation_args)?;
        let outer_elapsed_ns = elapsed_ns(started.elapsed());
        if response.exit_code != 0 {
            return Err(format!(
                "{operation} failed with {}: stdout={} stderr={}",
                response.exit_code, response.stdout, response.stderr
            )
            .into());
        }
        if !response.stderr.is_empty() {
            return Err(
                format!("{operation} emitted stderr on success: {}", response.stderr).into(),
            );
        }
        let response_value: Value = serde_json::from_str(&response.stdout)?;
        Ok(CliInvocation {
            operation: operation.to_owned(),
            request_id: request_id.map(str::to_owned),
            outer_elapsed_ns,
            response: response_value,
        })
    }

    /// Execute a public operation that is expected to fail closed and retain
    /// its complete client-visible diagnostic as evidence.  This deliberately
    /// does not deserialize a failure response: the CLI contract may place a
    /// structured rejection on either stream, while a zero exit would mean
    /// the authorization probe unexpectedly succeeded.
    pub(super) fn invoke_expect_failure(
        &self,
        request_id: Option<&str>,
        operation: &str,
        operation_args: &[String],
    ) -> ScorecardResult<CliFailure> {
        let started = Instant::now();
        let response = self
            .batch
            .lock()
            .map_err(|_| "sandbox-runtime-cli batch lock was poisoned")?
            .invoke(request_id, operation, operation_args)?;
        let outer_elapsed_ns = elapsed_ns(started.elapsed());
        if response.exit_code == 0 {
            return Err(format!(
                "{operation} unexpectedly succeeded during negative authorization probe: stdout={} stderr={}",
                response.stdout, response.stderr,
            )
            .into());
        }
        Ok(CliFailure {
            operation: operation.to_owned(),
            request_id: request_id.map(str::to_owned),
            outer_elapsed_ns,
            exit_code: Some(i32::from(response.exit_code)),
            stdout: response.stdout,
            stderr: response.stderr,
        })
    }
}

impl RuntimeCliBatch {
    fn invoke(
        &mut self,
        request_id: Option<&str>,
        operation: &str,
        operation_args: &[String],
    ) -> ScorecardResult<RuntimeCliBatchResponse> {
        let request = RuntimeCliBatchRequest {
            schema_version: 1,
            request_id,
            operation,
            operation_argv: operation_args,
        };
        let stdin = self
            .stdin
            .as_mut()
            .ok_or("sandbox-runtime-cli batch stdin is closed")?;
        serde_json::to_writer(&mut *stdin, &request)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;

        let mut response_line = String::new();
        if self.stdout.read_line(&mut response_line)? == 0 {
            return Err(self.terminated_error("before returning an operation response"));
        }
        let response: RuntimeCliBatchResponse = serde_json::from_str(&response_line)?;
        if response.kind != RUNTIME_CLI_BATCH_RESPONSE_KIND {
            return Err(format!(
                "sandbox-runtime-cli returned an invalid batch operation receipt: {response_line:?}"
            )
            .into());
        }
        Ok(response)
    }

    fn terminated_error(&mut self, context: &str) -> Box<dyn Error + Send + Sync> {
        self.stdin.take();
        let _ = self.child.kill();
        let status = self.child.wait().ok();
        let mut stderr = String::new();
        if let Some(mut pipe) = self.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        format!("sandbox-runtime-cli terminated {context}: status={status:?} stderr={stderr}")
            .into()
    }
}

impl Drop for RuntimeCliBatch {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn campaign_tool_root() -> ScorecardResult<PathBuf> {
    let executable = std::env::current_exe()?;
    if executable.file_name().and_then(|name| name.to_str()) != Some("mpla-speed-poc-v1") {
        return Err(format!(
            "campaign tool root must be resolved from mpla-speed-poc-v1, got {}",
            executable.display()
        )
        .into());
    }
    let tool_root = executable
        .parent()
        .ok_or("campaign executable lacks a parent directory")?;
    if approved_campaign_tool_root(tool_root) {
        Ok(tool_root.to_path_buf())
    } else {
        Err(format!(
            "campaign executable is outside an approved tool root: {}",
            executable.display()
        )
        .into())
    }
}

fn approved_campaign_tool_root(tool_root: &Path) -> bool {
    [
        TOOL_ROOT,
        FIXTURE_BUILDER_TOOL_ROOT,
        STAGED_SCORECARD_TOOL_ROOT,
    ]
    .iter()
    .any(|allowed| tool_root == Path::new(allowed))
}

pub(super) fn campaign_tool_path(name: &str) -> ScorecardResult<PathBuf> {
    if name.is_empty()
        || name
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    {
        return Err(format!("campaign tool name is unsafe: {name}").into());
    }
    Ok(campaign_tool_root()?.join(name))
}

/// Resolves a tool executed after the candidate workspace has replaced `/workspace`.
///
/// The runner mounts its atomically staged campaign tools in the candidate's read-only
/// shared base, so this intentionally does not use the coordinator's current executable.
pub(super) fn candidate_campaign_tool_path(name: &str) -> ScorecardResult<PathBuf> {
    if name.is_empty()
        || name
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    {
        return Err(format!("campaign tool name is unsafe: {name}").into());
    }
    Ok(Path::new(TOOL_ROOT).join(name))
}

#[cfg(test)]
mod fixture_root_tests {
    use std::path::Path;

    use serde_json::json;

    use super::{
        approved_campaign_tool_root, approved_runtime_gateway_socket, candidate_campaign_tool_path,
        merged_publication_oracle_command, publication_semantic_attribution,
        FIXTURE_BUILDER_TOOL_ROOT, STAGED_SCORECARD_TOOL_ROOT, TOOL_ROOT,
    };

    #[test]
    fn fixture_builder_tool_root_is_closed_to_v13() {
        assert!(FIXTURE_BUILDER_TOOL_ROOT.contains("/s4-chain-sparse-v1/"));
        assert!(!FIXTURE_BUILDER_TOOL_ROOT.contains("s4-chain-v9"));
    }

    #[test]
    fn staged_scorecard_tool_root_is_exact_and_allowlisted() {
        assert_eq!(STAGED_SCORECARD_TOOL_ROOT, "/workspace/_campaign-tools");
        assert!(approved_campaign_tool_root(Path::new(
            STAGED_SCORECARD_TOOL_ROOT
        )));
        assert!(!approved_campaign_tool_root(Path::new("/workspace/tools")));
    }

    #[test]
    fn candidate_oracle_uses_the_shared_base_not_the_coordinator_workspace() {
        assert_eq!(
            candidate_campaign_tool_path("mpla-poc-oracle").expect("safe tool name"),
            Path::new(TOOL_ROOT).join("mpla-poc-oracle")
        );
    }

    #[test]
    fn oracle_command_uses_the_publication_receipt_attribution() {
        let publication = json!({
            "semantic": {
                "durability": {
                    "semantic_attribution": {
                        "actor_id": "sandbox-runtime-publication",
                        "semantic_operation_id": "fixture-s4-chain-sparse-v1"
                    }
                }
            }
        });
        let (actor_id, operation_id) = publication_semantic_attribution(&publication)
            .expect("publication semantic attribution");
        assert_eq!(
            merged_publication_oracle_command(
                Path::new("/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-poc-oracle"),
                "/tmp/oracle.records",
                &actor_id,
                &operation_id,
            )
            .expect("safe oracle command"),
            "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-poc-oracle --tree . --records /tmp/oracle.records --actor-id sandbox-runtime-publication --semantic-operation-id fixture-s4-chain-sparse-v1"
        );
    }

    #[test]
    fn dedicated_v13_builder_and_consumer_sockets_are_allowlisted() {
        for socket in ["host.docker.internal:7902", "host.docker.internal:7903"] {
            assert_eq!(approved_runtime_gateway_socket(socket).unwrap(), socket);
        }
        assert!(approved_runtime_gateway_socket("host.docker.internal:7904").is_err());
    }
}

/// The coordinator runs inside a sandbox while the authenticated gateway is
/// host-owned.  Permit only the dedicated scorecard listeners; an arbitrary
/// environment-provided endpoint would turn this fixed public API path into an
/// uncontrolled network hop.  The default preserves the sealed v2 consumer.
pub(super) fn approved_runtime_gateway_socket(value: &str) -> ScorecardResult<String> {
    let Some(port) = value.strip_prefix("host.docker.internal:") else {
        return Err(format!(
            "{RUNTIME_GATEWAY_SOCKET_ENV} must use host.docker.internal:7881-7903"
        )
        .into());
    };
    let Ok(port) = port.parse::<u16>() else {
        return Err(format!(
            "{RUNTIME_GATEWAY_SOCKET_ENV} must use host.docker.internal:7881-7903"
        )
        .into());
    };
    if !(7881..=7903).contains(&port) {
        return Err(format!(
            "{RUNTIME_GATEWAY_SOCKET_ENV} must use host.docker.internal:7881-7903"
        )
        .into());
    }
    Ok(value.to_owned())
}

pub(super) fn merged_publication_oracle_command(
    oracle: &Path,
    records: &str,
    actor_id: &str,
    semantic_operation_id: &str,
) -> ScorecardResult<String> {
    validate_identifier(actor_id, "publication semantic attribution actor")?;
    validate_identifier(
        semantic_operation_id,
        "publication semantic attribution operation",
    )?;
    let oracle = oracle
        .to_str()
        .ok_or("independent oracle path is not valid UTF-8")?;
    Ok(format!(
        "{oracle} --tree . --records {records} --actor-id {actor_id} --semantic-operation-id {semantic_operation_id}"
    ))
}

pub(super) fn publication_semantic_attribution(
    publication: &Value,
) -> ScorecardResult<(String, String)> {
    let actor_id = publication
        .pointer("/semantic/durability/semantic_attribution/actor_id")
        .and_then(Value::as_str)
        .ok_or("publication omitted semantic durability attribution actor_id")?;
    let semantic_operation_id = publication
        .pointer("/semantic/durability/semantic_attribution/semantic_operation_id")
        .and_then(Value::as_str)
        .ok_or("publication omitted semantic durability attribution semantic_operation_id")?;
    validate_identifier(actor_id, "publication semantic attribution actor")?;
    validate_identifier(
        semantic_operation_id,
        "publication semantic attribution operation",
    )?;
    Ok((actor_id.to_owned(), semantic_operation_id.to_owned()))
}

pub(super) fn initial_publication_oracle_branch() -> &'static str {
    "main"
}

pub(super) fn validate_merged_publication_oracle(
    client: &RuntimeClient,
    run_id: &str,
    label: &str,
    branch: &str,
    publication: &CliInvocation,
    fixture_verification_command: Option<&str>,
) -> ScorecardResult<OracleValidation> {
    let oracle_binary = candidate_campaign_tool_path("mpla-poc-oracle")?;
    require_regular_file(&oracle_binary, "independent oracle")?;
    validate_identifier(branch, "oracle branch")?;
    let (oracle_actor_id, oracle_operation_id) =
        publication_semantic_attribution(&publication.response)?;
    let activation = client.invoke(
        Some(&format!("{run_id}-{label}-oracle-activate")),
        "activate_workspace_session",
        &[
            "--run-id".to_owned(),
            run_id.to_owned(),
            "--branch".to_owned(),
            branch.to_owned(),
        ],
    )?;
    let storage_profile = approved_storage_profile(
        &required_string(
            &activation.response,
            "storage_admin_profile_id",
            "oracle activation",
        )?,
        "oracle activation",
    )?;
    let workspace_session_id = required_string(
        &activation.response,
        "workspace_session_id",
        "oracle activation",
    )?;
    let records = format!("/tmp/{run_id}-{label}.oracle.records");
    let oracle = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.clone(),
            "--timeout-ms".to_owned(),
            "180000".to_owned(),
            "--yield-time-ms".to_owned(),
            "180000".to_owned(),
            merged_publication_oracle_command(
                &oracle_binary,
                &records,
                &oracle_actor_id,
                &oracle_operation_id,
            )?,
        ],
    )?;
    require_command_exit(&oracle.response, "independent merged publication oracle")?;
    let summary: Value = serde_json::from_str(&required_string(
        &oracle.response,
        "output",
        "independent merged publication oracle",
    )?)?;
    let fixture_verification = fixture_verification_command
        .map(|command| -> ScorecardResult<CliInvocation> {
            let verification = client.invoke(
                None,
                "exec_command",
                &[
                    "--workspace-session-id".to_owned(),
                    workspace_session_id.clone(),
                    "--timeout-ms".to_owned(),
                    "180000".to_owned(),
                    "--yield-time-ms".to_owned(),
                    "180000".to_owned(),
                    command.to_owned(),
                ],
            )?;
            require_command_exit(&verification.response, "merged fixture verification")?;
            Ok(verification)
        })
        .transpose()?;
    let storage_cleanup = cleanup_mounted_workspace(
        &client,
        run_id,
        label,
        &storage_profile,
        activation
            .response
            .get("storage_admin_scope")
            .ok_or("oracle activation omitted storage_admin_scope")?,
    )?;
    let destroy = client.invoke(
        None,
        "destroy_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id,
            "--grace-s".to_owned(),
            "0".to_owned(),
        ],
    )?;
    if destroy.response.get("destroyed").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "oracle workspace session was not destroyed: {}",
            destroy.response
        )
        .into());
    }
    require_oracle_match(publication, &summary)?;
    Ok(OracleValidation {
        oracle_tree: format!("merged:{branch}"),
        activation: Some(activation),
        outer_elapsed_ns: oracle.outer_elapsed_ns,
        exit_code: oracle
            .response
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok()),
        stderr: String::new(),
        summary,
        exact_match: true,
        fixture_verification,
        storage_cleanup,
        destroy: Some(destroy),
    })
}

pub(super) fn cleanup_mounted_workspace(
    client: &RuntimeClient,
    run_id: &str,
    label: &str,
    profile: &str,
    scope: &Value,
) -> ScorecardResult<Vec<CliInvocation>> {
    let profile = approved_storage_profile(profile, "storage cleanup")?;
    let mut receipts = Vec::with_capacity(3);
    for action in ["quiesce", "strict_unmount", "cleanup"] {
        let operation_id = format!("{run_id}-{label}-oracle-{action}");
        let request = json!({
            "schema_version": 1,
            "interface_version": "m2r-iface-v1",
            "profile_id": profile,
            "operation_id": operation_id,
            "action": action,
            "scope": scope,
        });
        receipts.push(client.invoke(
            Some(&operation_id),
            "mpla_storage_admin",
            &[serde_json::to_string(&request)?],
        )?);
    }
    Ok(receipts)
}

pub(super) fn require_oracle_match(
    publication: &CliInvocation,
    summary: &Value,
) -> ScorecardResult {
    let publication_root_id = publication
        .response
        .pointer("/roots/root_id")
        .and_then(Value::as_str)
        .ok_or("publication omitted roots.root_id")?;
    let publication_attribution_root_id = publication
        .response
        .pointer("/roots/attribution_root_id")
        .and_then(Value::as_str)
        .ok_or("publication omitted roots.attribution_root_id")?;
    let semantic_root_id = publication
        .response
        .pointer("/semantic/roots/root_id")
        .and_then(Value::as_str)
        .ok_or("publication omitted semantic.roots.root_id")?;
    let semantic_attribution_root_id = publication
        .response
        .pointer("/semantic/roots/attribution_root_id")
        .and_then(Value::as_str)
        .ok_or("publication omitted semantic.roots.attribution_root_id")?;
    let semantic_record_stream_sha256 = publication
        .response
        .pointer("/semantic/record_stream_sha256")
        .and_then(Value::as_str)
        .ok_or("publication omitted semantic.record_stream_sha256")?;
    let semantic_entry_count = publication
        .response
        .pointer("/semantic/entry_count")
        .and_then(Value::as_u64)
        .ok_or("publication omitted semantic.entry_count")?;
    let oracle_root_id = summary
        .get("root_id")
        .and_then(Value::as_str)
        .ok_or("oracle omitted root_id")?;
    let oracle_attribution_root_id = summary
        .get("attribution_root_id")
        .and_then(Value::as_str)
        .ok_or("oracle omitted attribution_root_id")?;
    let oracle_record_stream_sha256 = summary
        .get("record_stream_sha256")
        .and_then(Value::as_str)
        .ok_or("oracle omitted record_stream_sha256")?;
    let oracle_entry_count = summary
        .get("entry_count")
        .and_then(Value::as_u64)
        .ok_or("oracle omitted entry_count")?;
    if publication_root_id != semantic_root_id
        || publication_attribution_root_id != semantic_attribution_root_id
        || semantic_root_id != oracle_root_id
        || semantic_attribution_root_id != oracle_attribution_root_id
        || semantic_record_stream_sha256 != oracle_record_stream_sha256
        || semantic_entry_count != oracle_entry_count
    {
        let mismatch = oracle_mismatch_detail(publication, summary);
        return Err(format!(
            "publication and independent oracle differ: {mismatch} publication={} oracle={summary}",
            publication.response,
        )
        .into());
    }
    Ok(())
}

fn oracle_mismatch_detail(publication: &CliInvocation, summary: &Value) -> String {
    let publication_record = publication
        .response
        .get("semantic_root_record_debug")
        .and_then(Value::as_str)
        .unwrap_or("<publisher omitted root record>");
    let oracle_record = summary
        .get("root_record_debug")
        .and_then(Value::as_str)
        .unwrap_or("<oracle omitted root record>");
    format!("publication_root_record={publication_record} oracle_root_record={oracle_record}")
}

pub fn run(
    phase: LifecyclePhase,
    run_id: &str,
    candidate_sandbox_id: &str,
    build_commit: &str,
) -> ScorecardResult<Value> {
    validate_identifier(run_id, "run_id")?;
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    validate_build_commit(build_commit)?;
    require_regular_file(Path::new(RUNTIME_CLI), "runtime CLI")?;
    require_regular_file(Path::new(TOKEN_FILE), "gateway token")?;
    require_regular_file(Path::new(CATALOG_EXPORTER), "catalog exporter")?;
    require_regular_file(Path::new(PRODUCT_CATALOG), "product catalog")?;
    let r0_root = Path::new(R0_ROOT);
    if !r0_root.is_dir() {
        return Err(format!("R0 fixture is not a directory: {R0_ROOT}").into());
    }

    let run_root =
        Path::new("/eos/workspace/mpla-poc/scorecard").join(format!("{run_id}-{}", phase.name()));
    fs::create_dir_all(
        run_root
            .parent()
            .ok_or("scorecard run root lacks a parent")?,
    )?;
    fs::create_dir(&run_root)?;
    let result_path = Path::new("/workspace").join(phase.result_file());
    if result_path.exists() {
        return Err(format!("scorecard result already exists: {}", result_path.display()).into());
    }
    let mut progress = ProgressLedger::create(run_id, candidate_sandbox_id, build_commit, phase)?;
    let authority = super::capability_receipt()?;
    let backing = super::persistent_backing(&run_root)?;
    let cgroup_dir = super::current_cgroup_v2_dir()?;
    let cgroup = json!({
        "path": cgroup_dir,
        "memory_high": super::read_limit(&cgroup_dir.join("memory.high"))?,
        "memory_max": super::read_limit(&cgroup_dir.join("memory.max"))?,
        "membership_proven": super::cgroup_contains_self(&cgroup_dir)?,
    });
    let catalog_binding = bind_product_catalog(
        Path::new(CATALOG_EXPORTER),
        Path::new(PRODUCT_CATALOG),
        build_commit,
    )?;
    let (fixture, readiness_path, control_preparation) = if phase == LifecyclePhase::Squash {
        let changes = collect_control_changes(
            r0_root,
            &ControlCollectionLimits {
                max_entries: 8 * 1024,
                max_logical_bytes: 2 * 1024 * 1024 * 1024,
                max_path_bytes: 4 * 1024,
            },
        )?;
        (changes.profile, None, None)
    } else {
        let preparation = load_control_preparation(
            run_id,
            phase,
            candidate_sandbox_id,
            build_commit,
            &catalog_binding.binding_id,
        )?;
        (
            preparation.payload.fixture.clone(),
            Some(preparation.payload.readiness_path.clone()),
            Some(preparation),
        )
    };
    progress.record(
        "catalog_and_fixture_bound",
        json!({
            "fixture_entries": fixture.entries,
            "fixture_logical_bytes": fixture.logical_bytes,
            "control_prepared_before_phase": control_preparation.is_some(),
        }),
    )?;

    let control_closing = control_preparation
        .as_ref()
        .map(|preparation| vec![preparation.payload.closing.clone()])
        .unwrap_or_default();
    let mut control_cold = Vec::with_capacity(3);
    let mut control_same = Vec::with_capacity(3);
    let mut control_fork = Vec::with_capacity(3);
    let mut control_rollback = Vec::with_capacity(3);
    let squash_setup = if phase.prepares_candidate_locally_before_phase() {
        let setup_started = Instant::now();
        let candidate = prepare_candidate_r0(run_id, phase, candidate_sandbox_id, &fixture)?;
        let client = RuntimeClient::new(candidate_sandbox_id)?;
        let baseline_activation = activation_sample(
            "squash-baseline",
            client.invoke(
                Some(&format!("{run_id}-squash-baseline")),
                "activate_workspace_session",
                &[
                    "--run-id".to_owned(),
                    run_id.to_owned(),
                    "--branch".to_owned(),
                    "main".to_owned(),
                ],
            )?,
        )?;
        let elapsed_ns = elapsed_ns(setup_started.elapsed());
        progress.record(
            "squash_setup_completed_before_phase",
            json!({
                "workspace_session_id": candidate.workspace_session_id,
                "candidate_preparation_elapsed_ns": candidate.elapsed_ns,
                "baseline_activation_outer_elapsed_ns": baseline_activation.outer_elapsed_ns,
                "baseline_activation_service_elapsed_ns": baseline_activation.service_elapsed_ns,
                "setup_elapsed_ns": elapsed_ns,
            }),
        )?;
        Some(SquashPhaseSetup {
            candidate,
            client,
            baseline_activation,
            elapsed_ns,
        })
    } else {
        None
    };
    let mut phase_started = None;
    let mut monitor = None;
    let mut finished_squash_measurement = None;
    if phase != LifecyclePhase::Squash {
        phase_started = Some(Instant::now());
        monitor = Some(super::ResourceMonitor::start_heavy(&cgroup_dir, &run_root)?);
    }
    if phase != LifecyclePhase::Squash {
        sandbox_runtime_layerstack::reset_process_state_for_tests();
        let preparation = control_preparation
            .as_ref()
            .ok_or("lifecycle control preparation is absent")?;
        let state_root = preparation.payload.state_root.clone();
        let readiness_path = readiness_path
            .as_ref()
            .ok_or("lifecycle control readiness path is absent")?;
        progress.record(
            "control_preparation_loaded",
            json!({
                "immutable_publication_count": control_closing.len(),
                "control_pre_materialized_carrier_count": 0,
                "candidate_oracle_materialization_count": 1,
                "receipt_checksum_sha256": preparation.checksum_sha256,
            }),
        )?;
        for sample in 0..3_u8 {
            sandbox_runtime_layerstack::reset_process_state_for_tests();
            progress.record(
                "control_pair_started",
                json!({
                    "sample": sample,
                    "process": process_checkpoint()?,
                }),
            )?;
            match phase {
                LifecyclePhase::Activation => {
                    let cold = run_current_i2_materialization(
                        &CurrentI2MaterializationRequest {
                            state_root: state_root.clone(),
                            intent: ControlIntent::ColdActivation,
                            timeout: Duration::from_secs(phase.phase_cap_seconds()),
                            cache_expectation: ControlCacheExpectation::ColdBuilt,
                            expected_selection: None,
                            catalog_binding: catalog_binding.clone(),
                            boundary: control_boundary(
                                ControlCacheMatch::Matched,
                                "durable hidden publication",
                                "externally usable R0 carrier",
                            ),
                        },
                        readiness_probe(readiness_path.clone()),
                    )?;
                    progress.record(
                        "control_cold_completed",
                        json!({
                            "sample": sample,
                            "elapsed_ns": cold.span.elapsed_ns,
                            "maximum_buffer_bytes": cold
                                .materialization
                                .as_ref()
                                .and_then(|materialization| materialization.maximum_buffer_bytes),
                            "process": process_checkpoint()?,
                        }),
                    )?;
                    let selection = cold
                        .materialization
                        .as_ref()
                        .ok_or("cold control omitted its materialization")?
                        .selection_key();
                    let same = run_current_i2_materialization(
                        &CurrentI2MaterializationRequest {
                            state_root: state_root.clone(),
                            intent: ControlIntent::SameKeyActivation,
                            timeout: Duration::from_secs(phase.phase_cap_seconds()),
                            cache_expectation: ControlCacheExpectation::SameKeyReused,
                            expected_selection: Some(selection),
                            catalog_binding: catalog_binding.clone(),
                            boundary: control_boundary(
                                ControlCacheMatch::Matched,
                                "selected R0 key",
                                "externally usable R0 carrier",
                            ),
                        },
                        readiness_probe(readiness_path.clone()),
                    )?;
                    progress.record(
                        "control_same_completed",
                        json!({
                            "sample": sample,
                            "elapsed_ns": same.span.elapsed_ns,
                            "maximum_buffer_bytes": same
                                .materialization
                                .as_ref()
                                .and_then(|materialization| materialization.maximum_buffer_bytes),
                            "process": process_checkpoint()?,
                        }),
                    )?;
                    control_cold.push(cold);
                    control_same.push(same);
                }
                LifecyclePhase::Fork => {
                    control_fork.push(run_current_i2_materialization(
                        &CurrentI2MaterializationRequest {
                            state_root: state_root.clone(),
                            intent: ControlIntent::Fork,
                            timeout: Duration::from_secs(phase.phase_cap_seconds()),
                            cache_expectation: ControlCacheExpectation::ColdBuilt,
                            expected_selection: None,
                            catalog_binding: catalog_binding.clone(),
                            boundary: control_boundary(
                                ControlCacheMatch::Matched,
                                "selected R0 branch",
                                "externally usable fork carrier",
                            ),
                        },
                        readiness_probe(readiness_path.clone()),
                    )?);
                }
                LifecyclePhase::Rollback => {
                    control_rollback.push(run_current_i2_materialization(
                        &CurrentI2MaterializationRequest {
                            state_root: state_root.clone(),
                            intent: ControlIntent::Rollback,
                            timeout: Duration::from_secs(phase.phase_cap_seconds()),
                            cache_expectation: ControlCacheExpectation::ColdBuilt,
                            expected_selection: None,
                            catalog_binding: catalog_binding.clone(),
                            boundary: control_boundary(
                                ControlCacheMatch::Matched,
                                "selected prior R0 branch",
                                "externally usable rollback carrier",
                            ),
                        },
                        readiness_probe(readiness_path.clone()),
                    )?);
                }
                LifecyclePhase::Squash => unreachable!(),
            }
            progress.record(
                "control_pair_operations_completed",
                json!({
                    "sample": sample,
                    "process": process_checkpoint()?,
                }),
            )?;
            sandbox_runtime_layerstack::reset_process_state_for_tests();
            if sample < 2 {
                reclaim_control_materializations(&state_root)?;
                progress.record(
                    "control_materialization_cache_reclaimed",
                    json!({
                        "sample": sample,
                        "process": process_checkpoint()?,
                    }),
                )?;
            }
        }
        sandbox_runtime_layerstack::reset_process_state_for_tests();
        reclaim_control_state(&state_root)?;
        reclaim_control_preparation_receipt(run_id, phase)?;
        progress.record("control_state_reclaimed", json!({"sample_count": 3}))?;
    }
    let control_sample_count = match phase {
        LifecyclePhase::Activation => control_cold.len(),
        LifecyclePhase::Fork => control_fork.len(),
        LifecyclePhase::Rollback => control_rollback.len(),
        LifecyclePhase::Squash => 0,
    };
    progress.record(
        "controls_completed",
        json!({
            "immutable_publication_count": control_closing.len(),
            "cache_cold_sample_count": control_sample_count,
        }),
    )?;

    let (
        client,
        initial_create,
        initial_mount,
        initial_copy,
        initial_publish,
        initial_oracle,
        candidate_prepared_before_phase,
        squash_baseline,
        squash_setup_elapsed_ns,
    ) = if let Some(setup) = squash_setup {
        let candidate = setup.candidate;
        (
            setup.client,
            candidate.create,
            candidate.mount,
            candidate.copy,
            candidate.publication,
            candidate.oracle,
            true,
            Some(setup.baseline_activation),
            Some(setup.elapsed_ns),
        )
    } else if let Some(preparation) = control_preparation.as_ref() {
        let client = RuntimeClient::new(candidate_sandbox_id)?;
        let candidate = &preparation.payload.candidate;
        progress.record(
            "prepared_candidate_loaded",
            json!({
                "workspace_session_id": candidate.workspace_session_id,
                "receipt_checksum_sha256": preparation.checksum_sha256,
                "candidate_preparation_elapsed_ns": candidate.elapsed_ns,
            }),
        )?;
        (
            client,
            candidate.create.clone(),
            candidate.mount.clone(),
            candidate.copy.clone(),
            candidate.publication.clone(),
            candidate.oracle.clone(),
            true,
            None,
            None,
        )
    } else {
        return Err(format!(
            "{} candidate preparation was not completed before its phase clock",
            phase.name()
        )
        .into());
    };

    let phase_result = match phase {
        LifecyclePhase::Activation => {
            let r0_03 = activation_sample(
                "R0-03",
                client.invoke(
                    Some(&format!("{run_id}-activate-r0-03")),
                    "activate_workspace_session",
                    &[
                        "--run-id".to_owned(),
                        run_id.to_owned(),
                        "--branch".to_owned(),
                        "main".to_owned(),
                    ],
                )?,
            )?;
            let mut r0_04 = Vec::with_capacity(5);
            for sample in 0..5_u8 {
                r0_04.push(activation_sample(
                    &format!("R0-04-{:02}", sample + 1),
                    client.invoke(
                        Some(&format!("{run_id}-activate-r0-04-{sample:02}")),
                        "activate_workspace_session",
                        &[
                            "--run-id".to_owned(),
                            run_id.to_owned(),
                            "--branch".to_owned(),
                            "main".to_owned(),
                        ],
                    )?,
                )?);
            }
            progress.record(
                "candidate_activations_completed",
                json!({
                    "sample_count": r0_04.len() + 1,
                    "r0_03": &r0_03,
                    "r0_04": &r0_04,
                    "process": process_checkpoint()?,
                }),
            )?;
            let all_activations = std::iter::once(&r0_03)
                .chain(r0_04.iter())
                .collect::<Vec<_>>();
            let candidate_checks = candidate_checks(&all_activations);
            let r0_04_ns = r0_04
                .iter()
                .map(|sample| sample.outer_elapsed_ns)
                .collect::<Vec<_>>();
            let activate_exact_gate = lifecycle_gate(
                "BG-ACTIVATE-EXACT",
                r0_04_ns.clone(),
                receipt_ns(&control_cold),
                ACTIVATE_REQUIRED_NS,
                ACTIVATE_PREFERRED_NS,
                r0_03.outer_elapsed_ns <= ACTIVATE_REQUIRED_NS && candidate_checks.required(),
                r0_03.outer_elapsed_ns <= ACTIVATE_PREFERRED_NS && candidate_checks.required(),
            );
            let activate_same_gate = lifecycle_gate(
                "BG-ACTIVATE-SAME",
                r0_04_ns.into_iter().take(3).collect(),
                receipt_ns(&control_same),
                SAME_REQUIRED_NS,
                SAME_PREFERRED_NS,
                candidate_checks.required(),
                candidate_checks.required(),
            );
            json!({
                "r0_03": r0_03,
                "r0_04": r0_04,
                "candidate_checks": candidate_checks,
                "activate_exact_gate": activate_exact_gate,
                "activate_same_gate": activate_same_gate,
            })
        }
        LifecyclePhase::Fork => {
            let baseline_activation = activation_sample(
                "fork-baseline",
                client.invoke(
                    Some(&format!("{run_id}-fork-baseline")),
                    "activate_workspace_session",
                    &[
                        "--run-id".to_owned(),
                        run_id.to_owned(),
                        "--branch".to_owned(),
                        "main".to_owned(),
                    ],
                )?,
            )?;
            let mut fork_outer_ns = Vec::with_capacity(1_000);
            let mut fork_service_ns = Vec::with_capacity(1_000);
            let mut fork_non_service_overhead_ns = Vec::with_capacity(1_000);
            let mut fork_selected_outer_ns = Vec::with_capacity(3);
            let mut fork_selected_service_ns = Vec::with_capacity(3);
            let mut fork_selected_non_service_overhead_ns = Vec::with_capacity(3);
            let mut fork_selected_activations = Vec::with_capacity(3);
            let mut fork_selected_counts = Vec::with_capacity(3);
            progress.record("forking_started", json!({"target_count": 1_000}))?;
            for index in 0..1_000_usize {
                let branch = format!("inactive-{index:04}");
                let fork = client.invoke(
                    Some(&format!("{run_id}-fork-{index:04}")),
                    "fork_workspace_session",
                    &[
                        "--run-id".to_owned(),
                        run_id.to_owned(),
                        "--source-branch".to_owned(),
                        "main".to_owned(),
                        "--branch".to_owned(),
                        branch.clone(),
                    ],
                )?;
                let service_elapsed_ns = fork
                    .response
                    .get("service_elapsed_ns")
                    .and_then(Value::as_u64)
                    .ok_or("fork response omitted service_elapsed_ns")?;
                let non_service_overhead_ns =
                    checked_non_service_overhead_ns(fork.outer_elapsed_ns, service_elapsed_ns)?;
                fork_outer_ns.push(fork.outer_elapsed_ns);
                fork_service_ns.push(service_elapsed_ns);
                fork_non_service_overhead_ns.push(non_service_overhead_ns);
                let count = index + 1;
                if matches!(count, 1 | 64 | 1_000) {
                    fork_selected_outer_ns.push(fork.outer_elapsed_ns);
                    fork_selected_service_ns.push(service_elapsed_ns);
                    fork_selected_non_service_overhead_ns.push(non_service_overhead_ns);
                    fork_selected_activations.push(activation_sample(
                        &format!("fork-activate-{count}"),
                        client.invoke(
                            Some(&format!("{run_id}-fork-activate-{count}")),
                            "activate_workspace_session",
                            &[
                                "--run-id".to_owned(),
                                run_id.to_owned(),
                                "--branch".to_owned(),
                                branch,
                            ],
                        )?,
                    )?);
                    fork_selected_counts.push(count);
                }
                if matches!(count, 1 | 64 | 128 | 256 | 512 | 768 | 1_000) {
                    progress.record("fork_batch_completed", json!({"completed_count": count}))?;
                }
            }
            let all_activations = std::iter::once(&baseline_activation)
                .chain(fork_selected_activations.iter())
                .collect::<Vec<_>>();
            let candidate_checks = candidate_checks(&all_activations);
            let selected_fork_samples_complete = fork_selected_counts == [1, 64, 1_000]
                && fork_selected_outer_ns.len() == fork_selected_counts.len();
            let fork_gate = lifecycle_gate(
                "BG-FORK",
                fork_selected_outer_ns.clone(),
                receipt_ns(&control_fork),
                FORK_REQUIRED_NS,
                FORK_PREFERRED_NS,
                selected_fork_samples_complete && candidate_checks.required(),
                selected_fork_samples_complete && candidate_checks.required(),
            );
            json!({
                "baseline_activation": baseline_activation,
                "fork_outer_ns": fork_outer_ns,
                "fork_service_ns": fork_service_ns,
                "fork_non_service_overhead_ns": fork_non_service_overhead_ns,
                "fork_selected_outer_ns": fork_selected_outer_ns,
                "fork_selected_service_ns": fork_selected_service_ns,
                "fork_selected_non_service_overhead_ns": fork_selected_non_service_overhead_ns,
                "fork_selected_activations": fork_selected_activations,
                "fork_selected_counts": fork_selected_counts,
                "candidate_checks": candidate_checks,
                "fork_gate": fork_gate,
            })
        }
        LifecyclePhase::Rollback => {
            let baseline_activation = activation_sample(
                "rollback-baseline",
                client.invoke(
                    Some(&format!("{run_id}-rollback-baseline")),
                    "activate_workspace_session",
                    &[
                        "--run-id".to_owned(),
                        run_id.to_owned(),
                        "--branch".to_owned(),
                        "main".to_owned(),
                    ],
                )?,
            )?;
            let target_fork = client.invoke(
                Some(&format!("{run_id}-rollback-target-fork")),
                "fork_workspace_session",
                &[
                    "--run-id".to_owned(),
                    run_id.to_owned(),
                    "--source-branch".to_owned(),
                    "main".to_owned(),
                    "--branch".to_owned(),
                    "rollback-target".to_owned(),
                ],
            )?;
            let mut rollback_samples = Vec::with_capacity(3);
            for sample in 0..3_u8 {
                rollback_samples.push(rollback_sample(
                    &format!("rollback-{sample:02}"),
                    client.invoke(
                        Some(&format!("{run_id}-rollback-{sample:02}")),
                        "rollback_workspace_session",
                        &[
                            "--run-id".to_owned(),
                            run_id.to_owned(),
                            "--branch".to_owned(),
                            "main".to_owned(),
                            "--target-branch".to_owned(),
                            "rollback-target".to_owned(),
                        ],
                    )?,
                )?);
                progress.record("rollback_completed", json!({"sample": sample}))?;
            }
            let all_activations = std::iter::once(&baseline_activation)
                .chain(rollback_samples.iter())
                .collect::<Vec<_>>();
            let candidate_checks = candidate_checks(&all_activations);
            let rollback_outer_ns = rollback_samples
                .iter()
                .map(|sample| sample.outer_elapsed_ns)
                .collect::<Vec<_>>();
            let rollback_service_ns = rollback_samples
                .iter()
                .map(|sample| sample.service_elapsed_ns)
                .collect::<Vec<_>>();
            let rollback_gate = lifecycle_gate(
                "BG-ROLLBACK",
                rollback_outer_ns,
                receipt_ns(&control_rollback),
                ROLLBACK_REQUIRED_NS,
                ROLLBACK_PREFERRED_NS,
                candidate_checks.required()
                    && rollback_service_ns
                        .iter()
                        .all(|elapsed| *elapsed <= SELECTOR_REQUIRED_NS),
                candidate_checks.required()
                    && rollback_service_ns
                        .iter()
                        .all(|elapsed| *elapsed <= SELECTOR_REQUIRED_NS),
            );
            json!({
                "baseline_activation": baseline_activation,
                "target_fork": target_fork,
                "rollback_samples": rollback_samples,
                "rollback_service_ns": rollback_service_ns,
                "candidate_checks": candidate_checks,
                "rollback_gate": rollback_gate,
            })
        }
        LifecyclePhase::Squash => {
            let baseline_activation =
                squash_baseline.ok_or("squash baseline was not prepared before its phase clock")?;
            let squash_monitor = super::ResourceMonitor::start_heavy(&cgroup_dir, &run_root)?;
            let mut squash_samples = Vec::with_capacity(3);
            let mut squash_sample_receipts = Vec::with_capacity(3);
            let mut squash_phase_elapsed_ns = 0_u64;
            for sample in 0..3_u8 {
                let invocation = client.invoke(
                    Some(&format!("{run_id}-squash-{sample:02}")),
                    "squash_mpla_branch",
                    &[
                        "--run-id".to_owned(),
                        run_id.to_owned(),
                        "--branch".to_owned(),
                        "main".to_owned(),
                    ],
                )?;
                squash_phase_elapsed_ns = squash_phase_elapsed_ns
                    .checked_add(invocation.outer_elapsed_ns)
                    .ok_or("squash operation elapsed time overflowed")?;
                let receipt = squash_sample_receipt(sample, &invocation)?;
                progress.record("squash_completed", receipt.clone())?;
                squash_sample_receipts.push(receipt);
                squash_samples.push(invocation);
            }
            let squash_resources = squash_monitor.finish()?;
            finished_squash_measurement = Some((squash_resources, squash_phase_elapsed_ns));
            let squash_outer_ns = squash_samples
                .iter()
                .map(|sample| sample.outer_elapsed_ns)
                .collect::<Vec<_>>();
            let squash_service_ns = response_ns(&squash_samples, "service_elapsed_ns")?;
            let identity_and_attribution_stable = squash_identity_and_attribution_stable(
                &baseline_activation.projection,
                &squash_samples,
            );
            let public_outcomes_exact = squash_public_outcomes_exact(run_id, &squash_samples);
            let selected_ref_progression_exact = squash_selected_ref_progression_exact(
                &baseline_activation.selected_ref,
                &squash_samples,
            );
            let squash_gate = AbsoluteGate {
                gate: "AG-SQUASH".to_owned(),
                required: identity_and_attribution_stable
                    && public_outcomes_exact
                    && selected_ref_progression_exact
                    && squash_outer_ns
                        .iter()
                        .all(|elapsed| *elapsed <= SQUASH_REQUIRED_NS)
                    && squash_service_ns
                        .iter()
                        .all(|elapsed| *elapsed <= SELECTOR_REQUIRED_NS),
                outer_ns: squash_outer_ns,
                service_ns: squash_service_ns,
            };
            json!({
                "baseline_activation": baseline_activation,
                "squash_samples": squash_samples,
                "squash_sample_receipts": squash_sample_receipts,
                "identity_and_attribution_stable": identity_and_attribution_stable,
                "public_outcomes_exact": public_outcomes_exact,
                "selected_ref_progression_exact": selected_ref_progression_exact,
                "squash_gate": squash_gate,
            })
        }
    };

    let (resources, phase_elapsed_ns) = if let Some(measurement) = finished_squash_measurement {
        measurement
    } else {
        let resources = monitor
            .ok_or("phase resource monitor was not started")?
            .finish()?;
        let phase_elapsed_ns = u64::try_from(
            phase_started
                .ok_or("phase operation clock was not started")?
                .elapsed()
                .as_nanos(),
        )
        .unwrap_or(u64::MAX);
        (resources, phase_elapsed_ns)
    };
    super::validate_resource_observation(&resources)?;
    let phase_cap_ns = phase.phase_cap_seconds().saturating_mul(1_000_000_000);
    let mut evidence = json!({
        "schema_version": 1,
        "kind": phase.kind(),
        "phase": phase.name(),
        "runner": phase.runner(),
        "run_id": run_id,
        "candidate_sandbox_id": candidate_sandbox_id,
        "build_commit": build_commit,
        "phase_timing": {
            "clock": "CLOCK_MONOTONIC",
            "suggested_budget_seconds": phase.suggested_budget_seconds(),
            "selected_multiplier_milli": phase.selected_multiplier_milli(),
            "calculated_phase_cap_seconds": phase.phase_cap_seconds(),
            "elapsed_ns": phase_elapsed_ns,
            "measurement_scope": if phase == LifecyclePhase::Squash {
                "exact sum of three public squash outer spans; durable journal syncs excluded"
            } else {
                "continuous matched-control and public-operation span"
            },
            "cap_pass": phase_elapsed_ns <= phase_cap_ns,
            "deadline_carryover_seconds": 0,
        },
        "base_root": BASE_ROOT,
        "tool_root": TOOL_ROOT,
        "r0_root": R0_ROOT,
        "authority": authority,
        "backing": backing,
        "cgroup": cgroup,
        "resources": resources,
        "resource_bounds": true,
        "catalog_binding": catalog_binding,
        "control_preparation": control_preparation,
        "fixture": fixture,
        "control_closing": control_closing,
        "control_cold": control_cold,
        "control_same": control_same,
        "control_fork": control_fork,
        "control_rollback": control_rollback,
        "initial_create": initial_create,
        "initial_mount": initial_mount,
        "initial_copy": initial_copy,
        "initial_publish": initial_publish,
        "initial_oracle": initial_oracle,
        "candidate_prepared_before_phase": candidate_prepared_before_phase,
        "squash_setup_elapsed_ns": squash_setup_elapsed_ns,
    });
    let evidence_object = evidence
        .as_object_mut()
        .ok_or("scorecard evidence is not an object")?;
    for (key, value) in phase_result
        .as_object()
        .ok_or("phase scorecard evidence is not an object")?
    {
        evidence_object.insert(key.clone(), value.clone());
    }
    let bytes = serde_json::to_vec_pretty(&evidence)?;
    let result_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(&result_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    progress.record("completed", json!({"result_sha256": result_sha256}))?;
    sync_directory(
        result_path
            .parent()
            .ok_or("scorecard result lacks a parent")?,
    )?;
    Ok(json!({
        "result_path": result_path,
        "result_sha256": result_sha256,
        "result_bytes": bytes.len(),
        "phase": phase.name(),
        "runner": phase.runner(),
        "phase_elapsed_ns": phase_elapsed_ns,
        "phase_cap_seconds": phase.phase_cap_seconds(),
        "phase_cap_pass": phase_elapsed_ns <= phase_cap_ns,
    }))
}

fn activation_sample(label: &str, invocation: CliInvocation) -> ScorecardResult<ActivationSample> {
    Ok(ActivationSample {
        label: label.to_owned(),
        outer_elapsed_ns: invocation.outer_elapsed_ns,
        service_elapsed_ns: required_u64(
            &invocation.response,
            "service_elapsed_ns",
            "activation response",
        )?,
        workspace_session_id: required_string(
            &invocation.response,
            "workspace_session_id",
            "activation response",
        )?,
        fresh_allocation_id: required_string(
            &invocation.response,
            "fresh_allocation_id",
            "activation response",
        )?,
        selected_ref: invocation
            .response
            .pointer("/lifecycle/selected_ref")
            .and_then(Value::as_str)
            .ok_or("activation response omitted lifecycle.selected_ref")?
            .to_owned(),
        projection: invocation
            .response
            .get("projection")
            .cloned()
            .ok_or("activation response omitted projection")?,
        timings: invocation
            .response
            .get("timings")
            .cloned()
            .ok_or("activation response omitted timings")?,
    })
}

pub(super) fn rollback_sample(
    label: &str,
    invocation: CliInvocation,
) -> ScorecardResult<ActivationSample> {
    Ok(ActivationSample {
        label: label.to_owned(),
        outer_elapsed_ns: invocation.outer_elapsed_ns,
        service_elapsed_ns: required_u64(
            &invocation.response,
            "service_elapsed_ns",
            "rollback response",
        )?,
        workspace_session_id: required_string(
            &invocation.response,
            "workspace_session_id",
            "rollback response",
        )?,
        fresh_allocation_id: required_string(
            &invocation.response,
            "fresh_allocation_id",
            "rollback response",
        )?,
        selected_ref: invocation
            .response
            .pointer("/lifecycle/selected_ref")
            .and_then(Value::as_str)
            .ok_or("rollback response omitted lifecycle.selected_ref")?
            .to_owned(),
        projection: invocation
            .response
            .get("projection")
            .cloned()
            .ok_or("rollback response omitted projection")?,
        timings: invocation
            .response
            .get("timings")
            .cloned()
            .ok_or("rollback response omitted timings")?,
    })
}

fn process_checkpoint() -> ScorecardResult<Value> {
    let status = fs::read_to_string("/proc/self/status")?;
    let threads = status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or("/proc/self/status lacks a valid Threads value")?;
    Ok(json!({
        "rss_bytes": super::process_rss_bytes()?,
        "threads": threads,
    }))
}

fn squash_sample_receipt(sample: u8, invocation: &CliInvocation) -> ScorecardResult<Value> {
    let request_id = invocation
        .request_id
        .as_deref()
        .ok_or("squash invocation omitted its request ID")?;
    let service_elapsed_ns = required_u64(
        &invocation.response,
        "service_elapsed_ns",
        "squash response",
    )?;
    let selected_ref = invocation
        .response
        .pointer("/lifecycle/selected_ref")
        .and_then(Value::as_str)
        .ok_or("squash response omitted lifecycle.selected_ref")?;
    let roots = invocation
        .response
        .get("roots")
        .filter(|value| canonical_root_pair(Some(value)).is_some())
        .cloned()
        .ok_or("squash response omitted valid canonical roots")?;
    let ref_sequence = required_u64(&invocation.response, "ref_sequence", "squash response")?;
    Ok(json!({
        "sample": sample,
        "operation": invocation.operation.as_str(),
        "request_id": request_id,
        "outer_elapsed_ns": invocation.outer_elapsed_ns,
        "service_elapsed_ns": service_elapsed_ns,
        "selected_ref": selected_ref,
        "roots": roots,
        "ref_sequence": ref_sequence,
        "full_response_sha256": json_sha256(&invocation.response)?,
    }))
}

fn canonical_root_pair(value: Option<&Value>) -> Option<(&str, &str)> {
    let value = value?;
    let root_id = value.get("root_id")?.as_str()?;
    let attribution_root_id = value.get("attribution_root_id")?.as_str()?;
    if valid_lower_hex_digest(root_id) && valid_lower_hex_digest(attribution_root_id) {
        Some((root_id, attribution_root_id))
    } else {
        None
    }
}

fn valid_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn selected_ref_parts<'a>(value: &'a str, expected_branch: &str) -> Option<(u64, &'a str)> {
    let (branch, revision) = value.split_once('@')?;
    if branch != expected_branch {
        return None;
    }
    let (sequence, digest) = revision.split_once('#')?;
    if sequence.is_empty()
        || (sequence.len() > 1 && sequence.starts_with('0'))
        || !valid_lower_hex_digest(digest)
    {
        return None;
    }
    let sequence = sequence.parse().ok()?;
    (sequence > 0).then_some((sequence, digest))
}

fn squash_public_outcomes_exact(run_id: &str, samples: &[CliInvocation]) -> bool {
    samples.iter().enumerate().all(|(sample, invocation)| {
        let expected_operation_id = format!("{run_id}-squash-{sample:02}");
        let response = &invocation.response;
        let lifecycle = response.get("lifecycle");
        invocation.operation == "squash_mpla_branch"
            && invocation.request_id.as_deref() == Some(expected_operation_id.as_str())
            && response.get("run_id").and_then(Value::as_str) == Some(run_id)
            && response.get("branch").and_then(Value::as_str) == Some("main")
            && lifecycle
                .and_then(|value| value.get("operation_id"))
                .and_then(Value::as_str)
                == Some(expected_operation_id.as_str())
            && lifecycle
                .and_then(|value| value.get("committed"))
                .and_then(Value::as_bool)
                == Some(true)
            && lifecycle
                .and_then(|value| value.get("idempotent_replay"))
                .and_then(Value::as_bool)
                == Some(false)
            && lifecycle
                .and_then(|value| value.get("service_elapsed_ns"))
                .and_then(Value::as_u64)
                == response.get("service_elapsed_ns").and_then(Value::as_u64)
    })
}

fn squash_identity_and_attribution_stable(
    baseline_projection: &Value,
    samples: &[CliInvocation],
) -> bool {
    let baseline_roots = baseline_projection.get("roots");
    canonical_root_pair(baseline_roots).is_some()
        && samples.iter().all(|sample| {
            canonical_root_pair(sample.response.get("roots")) == canonical_root_pair(baseline_roots)
        })
}

fn squash_selected_ref_progression_exact(
    baseline_selected_ref: &str,
    samples: &[CliInvocation],
) -> bool {
    let Some((baseline_sequence, baseline_digest)) =
        selected_ref_parts(baseline_selected_ref, "main")
    else {
        return false;
    };
    let mut digests = BTreeSet::from([baseline_digest]);
    samples.iter().enumerate().all(|(sample, invocation)| {
        let Some(expected_sequence) = u64::try_from(sample)
            .ok()
            .and_then(|sample| sample.checked_add(1))
            .and_then(|offset| baseline_sequence.checked_add(offset))
        else {
            return false;
        };
        let response_sequence = invocation
            .response
            .get("ref_sequence")
            .and_then(Value::as_u64);
        let selected_ref = invocation
            .response
            .pointer("/lifecycle/selected_ref")
            .and_then(Value::as_str);
        let Some((selected_sequence, digest)) =
            selected_ref.and_then(|value| selected_ref_parts(value, "main"))
        else {
            return false;
        };
        response_sequence == Some(expected_sequence)
            && selected_sequence == expected_sequence
            && digests.insert(digest)
    })
}

fn candidate_checks(samples: &[&ActivationSample]) -> CandidateChecks {
    let expected_roots = samples
        .first()
        .and_then(|sample| sample.projection.get("roots"))
        .filter(|roots| {
            roots.get("root_id").and_then(Value::as_str).is_some()
                && roots
                    .get("attribution_root_id")
                    .and_then(Value::as_str)
                    .is_some()
        });
    let expected_lower_allocations = samples
        .first()
        .and_then(|sample| sample.projection.get("lower_allocation_ids_newest_first"));
    CandidateChecks {
        selected_refs_stable: expected_roots.is_some()
            && samples
                .iter()
                .all(|sample| sample.projection.get("roots") == expected_roots),
        projections_exact_zero_build: samples
            .iter()
            .all(|sample| exact_zero_projection(&sample.projection)),
        allocations_fresh: samples
            .iter()
            .map(|sample| sample.fresh_allocation_id.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            == samples.len(),
        lower_allocations_stable: expected_lower_allocations.is_some()
            && samples.iter().all(|sample| {
                sample.projection.get("lower_allocation_ids_newest_first")
                    == expected_lower_allocations
            }),
    }
}

fn exact_zero_projection(projection: &Value) -> bool {
    projection
        .get("reconstructed_payload_bytes")
        .and_then(Value::as_u64)
        == Some(0)
        && projection
            .get("hydrated_payload_bytes")
            .and_then(Value::as_u64)
            == Some(0)
        && projection.get("base_bytes_copied").and_then(Value::as_u64) == Some(0)
        && projection
            .get("projection_built_during_activation")
            .and_then(Value::as_bool)
            == Some(false)
}

fn lifecycle_gate(
    gate: &str,
    candidate_ns: Vec<u64>,
    control_ns: Vec<u64>,
    required_ceiling_ns: u64,
    preferred_ceiling_ns: u64,
    required_preconditions: bool,
    preferred_preconditions: bool,
) -> LifecycleGate {
    let candidate_median_ns = median(&candidate_ns);
    let candidate_max_ns = candidate_ns.iter().copied().max().unwrap_or(u64::MAX);
    let control_median_ns = median(&control_ns);
    let required_ratio = ratio_at_least(control_median_ns, candidate_median_ns, 100);
    let preferred_ratio = ratio_at_least(control_median_ns, candidate_median_ns, 500);
    LifecycleGate {
        gate: gate.to_owned(),
        required: required_preconditions
            && candidate_ns
                .iter()
                .all(|elapsed| *elapsed <= required_ceiling_ns)
            && required_ratio,
        preferred: preferred_preconditions
            && candidate_ns
                .iter()
                .all(|elapsed| *elapsed <= preferred_ceiling_ns)
            && preferred_ratio,
        candidate_ns,
        control_ns,
        candidate_median_ns,
        candidate_max_ns,
        control_median_ns,
        median_ratio_numerator: control_median_ns,
        median_ratio_denominator: candidate_median_ns,
    }
}

fn ratio_at_least(numerator: u64, denominator: u64, minimum: u64) -> bool {
    denominator != 0 && (numerator as u128) >= (denominator as u128) * (minimum as u128)
}

fn receipt_ns(receipts: &[ControlOperationReceipt]) -> Vec<u64> {
    receipts
        .iter()
        .map(|receipt| receipt.span.elapsed_ns)
        .collect()
}

fn response_ns(invocations: &[CliInvocation], field: &str) -> ScorecardResult<Vec<u64>> {
    invocations
        .iter()
        .map(|invocation| required_u64(&invocation.response, field, &invocation.operation))
        .collect()
}

fn checked_non_service_overhead_ns(
    outer_elapsed_ns: u64,
    service_elapsed_ns: u64,
) -> ScorecardResult<u64> {
    outer_elapsed_ns.checked_sub(service_elapsed_ns).ok_or_else(|| {
        format!(
            "fork service elapsed time {service_elapsed_ns} ns exceeds outer elapsed time {outer_elapsed_ns} ns"
        )
        .into()
    })
}

pub(super) fn control_boundary(
    cache_state: ControlCacheMatch,
    start: &str,
    stop: &str,
) -> ControlBoundary {
    ControlBoundary {
        candidate_start: start.to_owned(),
        candidate_stop: stop.to_owned(),
        current_i2_start: start.to_owned(),
        current_i2_stop: stop.to_owned(),
        same_fixture: true,
        same_intent: true,
        same_durability: true,
        same_readiness: true,
        cache_state,
        unknown_reason: None,
    }
}

fn readiness_probe(
    relative: PathBuf,
) -> impl FnOnce(&Path) -> sandbox_runtime_mpla_poc::PocResult<ExternalReadinessReceipt> {
    move |carrier| {
        let observed = carrier.join(&relative);
        Ok(ExternalReadinessReceipt {
            probe: format!("regular_file:{}", relative.display()),
            passed: observed.is_file(),
            observed: observed.display().to_string(),
        })
    }
}

fn select_readiness_path(root: &Path) -> ScorecardResult<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut selected = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        } else if metadata.is_file() && metadata.len() >= 100 * 1024 {
            selected.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    selected.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });
    selected
        .into_iter()
        .next()
        .ok_or_else(|| "R0 has no regular readiness file of at least 100 KiB".into())
}

fn load_control_preparation(
    run_id: &str,
    phase: LifecyclePhase,
    candidate_sandbox_id: &str,
    build_commit: &str,
    catalog_binding_id: &str,
) -> ScorecardResult<LifecycleControlPreparationReceipt> {
    let preparation_root = control_preparation_root(run_id, phase);
    require_real_directory(&preparation_root, "control preparation root")?;
    let mut entries = fs::read_dir(&preparation_root)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    if entries
        != [
            std::ffi::OsString::from(CONTROL_PREPARATION_RECEIPT),
            std::ffi::OsString::from("state"),
        ]
    {
        return Err(format!(
            "control preparation has an unexpected exact inventory: {}",
            preparation_root.display()
        )
        .into());
    }
    let receipt_path = preparation_root.join(CONTROL_PREPARATION_RECEIPT);
    let metadata = fs::symlink_metadata(&receipt_path)?;
    if !metadata.file_type().is_file()
        || metadata.len() == 0
        || metadata.len() > CONTROL_PREPARATION_RECEIPT_MAX_BYTES
    {
        return Err("control preparation receipt is not a bounded regular file".into());
    }
    let receipt: LifecycleControlPreparationReceipt =
        serde_json::from_slice(&fs::read(&receipt_path)?)?;
    let expected_checksum = control_preparation_checksum(&receipt.payload)?;
    if receipt.checksum_sha256 != expected_checksum {
        return Err("control preparation receipt checksum mismatch".into());
    }
    let expected_state_root = preparation_root.join("state");
    if receipt.payload.schema_version != SCHEMA_VERSION
        || receipt.payload.kind != "mpla_booster_lifecycle_control_preparation_v1"
        || receipt.payload.run_id != run_id
        || receipt.payload.phase != phase.name()
        || receipt.payload.candidate_sandbox_id != candidate_sandbox_id
        || receipt.payload.build_commit != build_commit
        || receipt.payload.state_root != expected_state_root
        || receipt.payload.catalog_binding_id != catalog_binding_id
        || receipt.payload.fixture.source_root != Path::new(R0_ROOT)
        || receipt.payload.collection_elapsed_ns == 0
        || receipt.payload.closing_publication_elapsed_ns == 0
        || receipt.payload.candidate_preparation_elapsed_ns == 0
        || receipt.payload.preparation_elapsed_ns == 0
        || receipt.payload.collection_elapsed_ns > receipt.payload.preparation_elapsed_ns
        || receipt.payload.closing_publication_elapsed_ns > receipt.payload.preparation_elapsed_ns
        || receipt.payload.candidate_preparation_elapsed_ns > receipt.payload.preparation_elapsed_ns
        || receipt.payload.candidate_preparation_elapsed_ns != receipt.payload.candidate.elapsed_ns
    {
        return Err("control preparation receipt identity mismatch".into());
    }
    require_real_directory(&expected_state_root, "prepared control state root")?;
    if !safe_relative_path(&receipt.payload.readiness_path) {
        return Err("control preparation readiness path is not a safe relative path".into());
    }
    let readiness = Path::new(R0_ROOT).join(&receipt.payload.readiness_path);
    let readiness_metadata = fs::symlink_metadata(&readiness)?;
    if !readiness_metadata.file_type().is_file() || readiness_metadata.len() < 100 * 1024 {
        return Err("control preparation readiness target is not the required regular file".into());
    }
    if receipt.payload.closing.intent != ControlIntent::ClosingPublication
        || receipt.payload.closing.catalog_binding_id != catalog_binding_id
        || receipt.payload.closing.verdict != ControlVerdict::Matched
        || receipt.payload.closing.source.as_ref() != Some(&receipt.payload.fixture)
        || receipt.payload.closing.materialization.is_some()
        || receipt.payload.closing.readiness.is_some()
        || !receipt
            .payload
            .closing
            .publication
            .as_ref()
            .is_some_and(|publication| publication.matched)
    {
        return Err("control preparation closing-publication proof is incomplete".into());
    }
    validate_candidate_r0_preparation(&receipt.payload.candidate, &receipt.payload.fixture)?;
    require_control_cache_cold(&expected_state_root)?;
    Ok(receipt)
}

fn validate_candidate_r0_preparation(
    candidate: &CandidateR0Preparation,
    fixture: &ControlSourceProfile,
) -> ScorecardResult {
    validate_identifier(
        &candidate.workspace_session_id,
        "prepared R0 workspace session",
    )?;
    for invocation in [
        &candidate.create,
        &candidate.mount,
        &candidate.copy,
        &candidate.publication,
    ] {
        require_compact_invocation_proof(invocation)?;
    }
    if candidate.create.operation != "create_mpla_workspace_session"
        || candidate.mount.operation != "mpla_storage_admin"
        || candidate.copy.operation != "exec_command"
        || candidate.publication.operation != "publish_mpla_workspace_session"
        || required_string(
            &candidate.create.response,
            "workspace_session_id",
            "prepared R0 create",
        )? != candidate.workspace_session_id
        || approved_storage_profile(
            &required_string(
                &candidate.create.response,
                "storage_admin_profile_id",
                "prepared R0 create",
            )?,
            "prepared R0 create",
        )? != candidate.storage_admin_profile_id
        || candidate.create.response.get("storage_admin_scope")
            != Some(&candidate.storage_admin_scope)
        || candidate.elapsed_ns == 0
    {
        return Err("prepared candidate R0 operation identity mismatch".into());
    }
    let mount_response = &candidate.mount.response;
    if mount_response.get("action").and_then(Value::as_str) != Some("mount")
        || mount_response
            .get("cleanup_complete")
            .and_then(Value::as_bool)
            != Some(true)
        || !mount_response.get("failure").is_some_and(Value::is_null)
        || mount_response.get("operation_id").and_then(Value::as_str)
            != candidate.mount.request_id.as_deref()
        || mount_response.get("profile_id").and_then(Value::as_str)
            != Some(candidate.storage_admin_profile_id.as_str())
        || mount_response.get("scope") != Some(&candidate.storage_admin_scope)
        || !mount_response
            .get("mount_attestation_sha256")
            .and_then(Value::as_str)
            .is_some_and(valid_sha256)
    {
        return Err("prepared candidate R0 mount proof is incomplete".into());
    }
    require_command_exit(&candidate.copy.response, "prepared R0 copy")?;
    require_initial_r0_publication(&candidate.publication, fixture)?;
    require_oracle_match(&candidate.publication, &candidate.oracle.summary)?;
    if let Some(activation) = candidate.oracle.activation.as_ref() {
        require_compact_invocation_proof(activation)?;
        validate_identifier(
            &required_string(
                &activation.response,
                "workspace_session_id",
                "prepared candidate oracle activation",
            )?,
            "prepared candidate oracle workspace session",
        )?;
        approved_storage_profile(
            &required_string(
                &activation.response,
                "storage_admin_profile_id",
                "prepared candidate oracle activation",
            )?,
            "prepared candidate oracle activation",
        )?;
        if activation.operation != "activate_workspace_session"
            || !activation
                .response
                .get("storage_admin_scope")
                .is_some_and(Value::is_object)
        {
            return Err("prepared candidate R0 oracle activation proof is incomplete".into());
        }
    }
    for (cleanup, expected_action) in
        candidate
            .oracle
            .storage_cleanup
            .iter()
            .zip(["quiesce", "strict_unmount", "cleanup"])
    {
        require_compact_invocation_proof(cleanup)?;
        if cleanup.operation != "mpla_storage_admin"
            || cleanup.response.get("action").and_then(Value::as_str) != Some(expected_action)
            || cleanup
                .response
                .get("cleanup_complete")
                .and_then(Value::as_bool)
                != Some(true)
            || !cleanup.response.get("failure").is_some_and(Value::is_null)
            || cleanup.response.get("operation_id").and_then(Value::as_str)
                != cleanup.request_id.as_deref()
            || cleanup.response.get("profile_id").and_then(Value::as_str)
                != Some(candidate.storage_admin_profile_id.as_str())
        {
            return Err("prepared candidate R0 storage-cleanup proof is incomplete".into());
        }
    }
    if let Some(destroy) = candidate.oracle.destroy.as_ref() {
        require_compact_invocation_proof(destroy)?;
        if destroy.operation != "destroy_workspace_session" {
            return Err("prepared candidate R0 destroy proof has the wrong operation".into());
        }
    }
    if candidate.oracle.oracle_tree != "merged:main"
        || !candidate.oracle.exact_match
        || candidate.oracle.exit_code != Some(0)
        || candidate.oracle.fixture_verification.is_some()
        || candidate.oracle.activation.is_none()
        || candidate.oracle.storage_cleanup.len() != 3
        || candidate
            .oracle
            .destroy
            .as_ref()
            .and_then(|destroy| destroy.response.get("destroyed"))
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err("prepared candidate R0 oracle proof is incomplete".into());
    }
    Ok(())
}

fn require_compact_invocation_proof(invocation: &CliInvocation) -> ScorecardResult {
    if invocation
        .response
        .get("proof_kind")
        .and_then(Value::as_str)
        != Some("mpla_compact_cli_invocation_proof_v1")
        || !invocation
            .response
            .get("full_response_sha256")
            .and_then(Value::as_str)
            .is_some_and(valid_sha256)
    {
        return Err(format!(
            "prepared invocation lacks an exact compact proof: {}",
            invocation.operation
        )
        .into());
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn require_control_cache_cold(state_root: &Path) -> ScorecardResult {
    require_real_directory(state_root, "control state root")?;
    let leases = state_root.join("refs").join("leases");
    if let Some(entry) =
        first_matching_entry(&leases, |name| name.starts_with(b"materialization-"))?
    {
        return Err(format!(
            "prepared control has a materialization lease: {}",
            entry.display()
        )
        .into());
    }
    let subjects = state_root
        .join("refs")
        .join("materialization-generation-subjects");
    if let Some(entry) = first_matching_entry(&subjects, |_| true)? {
        return Err(format!(
            "prepared control has a materialization generation subject: {}",
            entry.display()
        )
        .into());
    }
    let materializations = state_root.join("materializations");
    match fs::symlink_metadata(&materializations) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            if fs::read_dir(&materializations)?
                .next()
                .transpose()?
                .is_some()
            {
                return Err("prepared control already contains a materialized carrier".into());
            }
        }
        Ok(_) => {
            return Err("prepared control materializations path is not a real directory".into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if !terminal_materialization_operations(state_root)?.is_empty() {
        return Err("prepared control already contains a materialization operation".into());
    }
    Ok(())
}

fn reclaim_control_preparation_receipt(run_id: &str, phase: LifecyclePhase) -> ScorecardResult {
    let preparation_root = control_preparation_root(run_id, phase);
    let receipt_path = preparation_root.join(CONTROL_PREPARATION_RECEIPT);
    require_regular_file(&receipt_path, "control preparation receipt")?;
    let mut entries = fs::read_dir(&preparation_root)?;
    let first = entries
        .next()
        .transpose()?
        .ok_or("control preparation root is unexpectedly empty")?;
    if first.file_name() != CONTROL_PREPARATION_RECEIPT || entries.next().transpose()?.is_some() {
        return Err(
            "control preparation root retained unexpected entries after state cleanup".into(),
        );
    }
    fs::remove_file(&receipt_path)?;
    sync_directory(&preparation_root)?;
    fs::remove_dir(&preparation_root)?;
    let parent = preparation_root
        .parent()
        .ok_or("control preparation root lacks a parent")?;
    sync_directory(parent)
}

pub(super) fn require_command_exit(response: &Value, label: &str) -> ScorecardResult {
    if response.get("status").and_then(Value::as_str) != Some("ok")
        || response.get("exit_code").and_then(Value::as_i64) != Some(0)
        || response.get("end_offset").and_then(Value::as_u64)
            != response.get("total_lines").and_then(Value::as_u64)
    {
        return Err(format!("{label} did not exit successfully: {response}").into());
    }
    Ok(())
}

pub(super) fn publication_roots_match(response: &Value) -> bool {
    let Some(root_id) = response.pointer("/roots/root_id").and_then(Value::as_str) else {
        return false;
    };
    let Some(attribution_root_id) = response
        .pointer("/roots/attribution_root_id")
        .and_then(Value::as_str)
    else {
        return false;
    };
    response
        .pointer("/semantic/roots/root_id")
        .and_then(Value::as_str)
        == Some(root_id)
        && response
            .pointer("/semantic/roots/attribution_root_id")
            .and_then(Value::as_str)
            == Some(attribution_root_id)
}

fn require_initial_r0_publication(
    publication: &CliInvocation,
    fixture: &ControlSourceProfile,
) -> ScorecardResult {
    let durable = [
        "files_fsynced",
        "object_directory_fsynced",
        "manifest_fsynced",
        "manifest_directory_fsynced",
    ]
    .into_iter()
    .all(|field| {
        publication
            .response
            .pointer(&format!("/semantic/durability/{field}"))
            .and_then(Value::as_bool)
            == Some(true)
    });
    if required_u64(
        &publication.response,
        "affected_path_count",
        "initial R0 publication",
    )? != fixture.entries
        || required_u64(
            &publication.response,
            "affected_payload_bytes_read",
            "initial R0 publication",
        )? != 0
        || publication
            .response
            .pointer("/semantic/bytes_read")
            .and_then(Value::as_u64)
            != Some(fixture.logical_bytes)
        || publication
            .response
            .pointer("/stationary/stable/after/logical_bytes")
            .and_then(Value::as_u64)
            != Some(fixture.logical_bytes)
        || !publication_roots_match(&publication.response)
        || publication
            .response
            .pointer("/stationary/no_second_payload_allocation")
            .and_then(Value::as_bool)
            != Some(true)
        || publication
            .response
            .pointer("/stationary/representative_inodes_unchanged")
            .and_then(Value::as_bool)
            != Some(true)
        || publication
            .response
            .pointer("/stationary/allocated_bytes_unchanged")
            .and_then(Value::as_bool)
            != Some(true)
        || !durable
    {
        return Err(format!(
            "initial R0 publication failed exact fixture/stationary qualification: {}",
            publication.response
        )
        .into());
    }
    Ok(())
}

pub(super) fn required_string(
    response: &Value,
    field: &str,
    label: &str,
) -> ScorecardResult<String> {
    response
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} omitted string field {field}").into())
}

pub(super) fn required_u64(response: &Value, field: &str, label: &str) -> ScorecardResult<u64> {
    response
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{label} omitted u64 field {field}").into())
}

pub(super) fn validate_identifier(value: &str, label: &str) -> ScorecardResult {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(format!("{label} is not a safe identifier").into());
    }
    Ok(())
}

pub(super) fn validate_build_commit(value: &str) -> ScorecardResult {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("build_commit must be exactly 40 lowercase hexadecimal characters".into());
    }
    Ok(())
}

pub(super) fn require_regular_file(path: &Path, label: &str) -> ScorecardResult {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(format!("{label} is not a regular file: {}", path.display()).into());
    }
    Ok(())
}

fn require_real_directory(path: &Path, label: &str) -> ScorecardResult {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{label} is not a real directory: {}", path.display()).into());
    }
    Ok(())
}

pub(super) fn median(values: &[u64]) -> u64 {
    if values.is_empty() {
        return u64::MAX;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn elapsed_ns(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
}

pub(super) fn sync_directory(path: &Path) -> ScorecardResult {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Drop only a completed current-I2 materialization cache between control
/// samples. The immutable object store, publication head, operation journal,
/// and catalog binding remain untouched, so each next sample measures a
/// genuine cache-cold materialization of the exact same durable root.
pub(super) fn reclaim_control_materializations(state_root: &Path) -> ScorecardResult {
    let root_metadata = fs::symlink_metadata(state_root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!(
            "control state root is not a real directory: {}",
            state_root.display()
        )
        .into());
    }

    let leases = state_root.join("refs").join("leases");
    if let Some(entry) =
        first_matching_entry(&leases, |name| name.starts_with(b"materialization-"))?
    {
        return Err(format!(
            "control materialization cache has an active lease: {}",
            entry.display()
        )
        .into());
    }
    let subjects = state_root
        .join("refs")
        .join("materialization-generation-subjects");
    if let Some(entry) = first_matching_entry(&subjects, |_| true)? {
        return Err(format!(
            "control materialization cache has an active generation subject: {}",
            entry.display()
        )
        .into());
    }

    let materializations = state_root.join("materializations");
    let terminal_operations = terminal_materialization_operations(state_root)?;
    match fs::symlink_metadata(&materializations) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(&materializations)?;
            sync_directory(state_root)?;
        }
        Ok(_) => {
            return Err(format!(
                "control materialization cache is not a real directory: {}",
                materializations.display()
            )
            .into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    for operation in terminal_operations {
        fs::remove_dir_all(&operation)?;
    }
    let operations = state_root.join("operations");
    if operations.is_dir() {
        sync_directory(&operations)?;
    }
    sync_directory(state_root)?;
    Ok(())
}

fn terminal_materialization_operations(state_root: &Path) -> ScorecardResult<Vec<PathBuf>> {
    let operations = state_root.join("operations");
    match fs::symlink_metadata(&operations) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "control operation registry is not a real directory: {}",
                operations.display()
            )
            .into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    }
    let mut terminal = Vec::new();
    for entry in fs::read_dir(&operations)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_dir() {
            if entry.file_name().as_bytes() == ACTIVE_COMMON_OPERATIONS_FILE {
                validate_active_common_operations(&path)?;
                continue;
            }
            return Err(format!(
                "control operation entry is not a real directory: {}",
                path.display()
            )
            .into());
        }
        let state_path = path.join("STATE");
        let state_metadata = fs::symlink_metadata(&state_path)?;
        if !state_metadata.file_type().is_file() || state_metadata.len() > 256 * 1024 {
            return Err(format!(
                "control operation STATE is not a bounded regular file: {}",
                state_path.display()
            )
            .into());
        }
        let bytes = fs::read(&state_path)?;
        if bytes.first() != Some(&b'{') {
            continue;
        }
        let state: Value = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "control JSON operation STATE is corrupt at {}: {error}",
                state_path.display()
            )
        })?;
        if state.get("schema").and_then(Value::as_str)
            != Some("layerstack-materialization-operation-v3")
        {
            continue;
        }
        let expected_id = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("materialization operation ID is not UTF-8")?;
        let exact_terminal = state.get("schema_version").and_then(Value::as_u64) == Some(3)
            && state.get("operation_id").and_then(Value::as_str) == Some(expected_id)
            && state.get("phase").and_then(Value::as_str) == Some("terminal")
            && state.get("terminal_outcome").and_then(Value::as_str) == Some("succeeded")
            && !path.join("work").exists();
        if !exact_terminal {
            return Err(format!(
                "control materialization operation is not a reaped successful terminal: {}",
                path.display()
            )
            .into());
        }
        terminal.push(path);
    }
    terminal.sort();
    Ok(terminal)
}

/// Validate layerstack's bounded derived admission index before preserving it.
///
/// Durable operation `STATE` files remain the recovery truth. This index can
/// intentionally retain a just-terminal common operation until the next
/// locked admission reconciles it, so reclamation must neither require it to
/// be empty nor delete it.
fn validate_active_common_operations(path: &Path) -> ScorecardResult {
    let path_metadata = fs::symlink_metadata(path)?;
    if !path_metadata.file_type().is_file()
        || path_metadata.len() > ACTIVE_COMMON_OPERATIONS_MAX_BYTES
    {
        return Err(format!(
            "control NONTERMINAL index is not a bounded regular file: {}",
            path.display()
        )
        .into());
    }

    let file = File::open(path)?;
    let file_metadata = file.metadata()?;
    if !file_metadata.file_type().is_file()
        || file_metadata.dev() != path_metadata.dev()
        || file_metadata.ino() != path_metadata.ino()
    {
        return Err(format!(
            "control NONTERMINAL index changed during validation: {}",
            path.display()
        )
        .into());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(file_metadata.len()).unwrap_or(0));
    file.take(ACTIVE_COMMON_OPERATIONS_MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != file_metadata.len()
        || bytes.len() as u64 > ACTIVE_COMMON_OPERATIONS_MAX_BYTES
    {
        return Err(format!(
            "control NONTERMINAL index changed or exceeded its bound: {}",
            path.display()
        )
        .into());
    }

    let minimum_len = ACTIVE_COMMON_OPERATIONS_MAGIC.len() + 2 + 32;
    if bytes.len() < minimum_len {
        return Err(format!(
            "control NONTERMINAL index has invalid framing: {}",
            path.display()
        )
        .into());
    }
    let (payload, encoded_checksum) = bytes.split_at(bytes.len() - 32);
    if !payload.starts_with(ACTIVE_COMMON_OPERATIONS_MAGIC) {
        return Err(format!(
            "control NONTERMINAL index has invalid magic: {}",
            path.display()
        )
        .into());
    }
    let count_offset = ACTIVE_COMMON_OPERATIONS_MAGIC.len();
    let count = usize::from(u16::from_be_bytes([
        payload[count_offset],
        payload[count_offset + 1],
    ]));
    if count > MAX_NONTERMINAL_COMMON_OPERATIONS {
        return Err(format!(
            "control NONTERMINAL index exceeds its operation bound: {}",
            path.display()
        )
        .into());
    }
    let expected_payload_len = count_offset + 2 + count * 32;
    if payload.len() != expected_payload_len {
        return Err(format!(
            "control NONTERMINAL index has invalid length: {}",
            path.display()
        )
        .into());
    }

    let mut checksum = Sha256::new();
    checksum.update(ACTIVE_COMMON_OPERATIONS_CHECKSUM_DOMAIN);
    checksum.update(payload);
    if checksum.finalize().as_slice() != encoded_checksum {
        return Err(format!(
            "control NONTERMINAL index checksum failed: {}",
            path.display()
        )
        .into());
    }
    if payload[count_offset + 2..]
        .chunks_exact(32)
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(format!(
            "control NONTERMINAL index is not in canonical order: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn first_matching_entry(
    directory: &Path,
    predicate: impl Fn(&[u8]) -> bool,
) -> ScorecardResult<Option<PathBuf>> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "control activity index is not a real directory: {}",
                directory.display()
            )
            .into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if predicate(entry.file_name().as_bytes()) {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// Control trees are physical current-I2 baselines, not scorecard evidence.
/// Their compact receipts are retained in memory/JSON; retaining all three
/// full trees on the shared Docker workspace can exhaust its legitimate
/// materialization-store capacity before the candidate is even provisioned.
pub(super) fn reclaim_control_state(state_root: &Path) -> ScorecardResult {
    fs::remove_dir_all(state_root)?;
    let parent = state_root
        .parent()
        .ok_or("control state root lacks a parent directory")?;
    sync_directory(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_error_is_bounded_and_utf8_safe() {
        let error = "é".repeat(PROGRESS_ERROR_SEGMENT_BYTES * 2);
        let details = bounded_progress_error(&error);
        let encoded = serde_json::to_vec(&details).expect("progress diagnostic serializes");

        assert!(encoded.len() < 128 * 1024);
        assert_eq!(details["error_bytes"].as_u64(), Some(error.len() as u64));
        assert_eq!(details["error_sha256"].as_str().map(str::len), Some(64));
        assert!(details["error_head"]
            .as_str()
            .is_some_and(|head| head.is_char_boundary(head.len())));
        assert!(details["error_tail"]
            .as_str()
            .is_some_and(|tail| tail.is_char_boundary(0)));
    }

    #[test]
    fn non_service_overhead_requires_service_time_nested_inside_outer_time() {
        assert_eq!(
            checked_non_service_overhead_ns(11, 7).expect("valid nested timing"),
            4
        );
        let error = checked_non_service_overhead_ns(7, 11)
            .expect_err("service time outside the outer boundary must fail closed");
        assert!(error
            .to_string()
            .contains("service elapsed time 11 ns exceeds outer elapsed time 7 ns"));
    }

    #[test]
    fn lifecycle_control_preparation_is_limited_to_matched_lifecycle_phases() {
        assert_eq!(
            LifecyclePhase::from_control_preparation_name("activation").ok(),
            Some(LifecyclePhase::Activation)
        );
        assert_eq!(
            LifecyclePhase::from_control_preparation_name("fork").ok(),
            Some(LifecyclePhase::Fork)
        );
        assert_eq!(
            LifecyclePhase::from_control_preparation_name("rollback").ok(),
            Some(LifecyclePhase::Rollback)
        );
        assert!(LifecyclePhase::from_control_preparation_name("squash").is_err());
        assert!(LifecyclePhase::from_control_preparation_name("publication").is_err());
    }

    #[test]
    fn logical_squash_prepares_payload_and_baseline_before_its_phase_clock() {
        assert!(
            LifecyclePhase::Squash.prepares_candidate_locally_before_phase(),
            "logical squash must time and monitor only its public metadata operations"
        );
        for phase in [
            LifecyclePhase::Activation,
            LifecyclePhase::Fork,
            LifecyclePhase::Rollback,
        ] {
            assert!(
                !phase.prepares_candidate_locally_before_phase(),
                "matched lifecycle phases use their separately receipted preparation"
            );
        }
    }

    #[test]
    fn squash_progress_receipt_preserves_completed_public_operation_evidence() -> ScorecardResult {
        let selected_ref = format!("main@7#{}", "33".repeat(32));
        let invocation = CliInvocation {
            operation: "squash_mpla_branch".to_owned(),
            request_id: Some("run-squash-02".to_owned()),
            outer_elapsed_ns: 2_000_000,
            response: json!({
                "service_elapsed_ns": 200_000,
                "roots": {
                    "root_id": "11".repeat(32),
                    "attribution_root_id": "22".repeat(32),
                },
                "ref_sequence": 7,
                "lifecycle": {"selected_ref": selected_ref},
                "durable": true,
            }),
        };

        let receipt = squash_sample_receipt(2, &invocation)?;
        let expected_response_sha256 = json_sha256(&invocation.response)?;

        assert_eq!(receipt["sample"].as_u64(), Some(2));
        assert_eq!(receipt["operation"].as_str(), Some("squash_mpla_branch"));
        assert_eq!(receipt["request_id"].as_str(), Some("run-squash-02"));
        assert_eq!(receipt["outer_elapsed_ns"].as_u64(), Some(2_000_000));
        assert_eq!(receipt["service_elapsed_ns"].as_u64(), Some(200_000));
        assert_eq!(
            receipt["selected_ref"].as_str(),
            Some(selected_ref.as_str())
        );
        assert_eq!(
            receipt.pointer("/roots/root_id").and_then(Value::as_str),
            Some("11".repeat(32).as_str())
        );
        assert_eq!(
            receipt
                .pointer("/roots/attribution_root_id")
                .and_then(Value::as_str),
            Some("22".repeat(32).as_str())
        );
        assert_eq!(receipt["ref_sequence"].as_u64(), Some(7));
        assert_eq!(
            receipt["full_response_sha256"].as_str(),
            Some(expected_response_sha256.as_str())
        );
        Ok(())
    }

    #[test]
    fn squash_continuity_requires_stable_roots_and_consecutive_committed_refs() {
        let run_id = "run-squash-continuity";
        let roots = json!({
            "root_id": "11".repeat(32),
            "attribution_root_id": "22".repeat(32),
        });
        let baseline_projection = json!({"roots": roots});
        let mut samples = (0..3)
            .map(|sample| {
                let sequence = sample + 2;
                let operation_id = format!("{run_id}-squash-{sample:02}");
                CliInvocation {
                    operation: "squash_mpla_branch".to_owned(),
                    request_id: Some(operation_id.clone()),
                    outer_elapsed_ns: 2_000_000,
                    response: json!({
                        "run_id": run_id,
                        "branch": "main",
                        "roots": roots,
                        "ref_sequence": sequence,
                        "service_elapsed_ns": 200_000,
                        "lifecycle": {
                            "operation_id": operation_id,
                            "committed": true,
                            "idempotent_replay": false,
                            "selected_ref": format!(
                                "main@{sequence}#{}",
                                format!("{:02x}", 0xbb + sample).repeat(32)
                            ),
                            "service_elapsed_ns": 200_000,
                        },
                    }),
                }
            })
            .collect::<Vec<_>>();

        assert!(squash_identity_and_attribution_stable(
            &baseline_projection,
            &samples
        ));
        assert!(squash_public_outcomes_exact(run_id, &samples));
        assert!(squash_selected_ref_progression_exact(
            &format!("main@1#{}", "aa".repeat(32)),
            &samples
        ));

        samples[1].response["roots"]["attribution_root_id"] = json!("44".repeat(32));
        assert!(!squash_identity_and_attribution_stable(
            &baseline_projection,
            &samples
        ));
        samples[1].response["roots"] = roots;
        samples[1].response["ref_sequence"] = json!(4);
        assert!(!squash_selected_ref_progression_exact(
            &format!("main@1#{}", "aa".repeat(32)),
            &samples
        ));
        samples[1].response["ref_sequence"] = json!(3);
        samples[1].response["lifecycle"]["committed"] = json!(false);
        assert!(!squash_public_outcomes_exact(run_id, &samples));
        samples[1].response["lifecycle"]["committed"] = json!(true);
        samples[1].response["lifecycle"]["operation_id"] = json!("wrong-operation");
        assert!(!squash_public_outcomes_exact(run_id, &samples));
        samples[1].response["lifecycle"]["operation_id"] = json!(format!("{run_id}-squash-01"));

        let valid_selected_ref = samples[1].response["lifecycle"]["selected_ref"].clone();
        for malformed in [
            format!("other@3#{}", "cc".repeat(32)),
            format!("main@03#{}", "cc".repeat(32)),
            format!("main@0#{}", "cc".repeat(32)),
            format!("main@3#{}", "CC".repeat(32)),
        ] {
            samples[1].response["lifecycle"]["selected_ref"] = json!(malformed);
            assert!(!squash_selected_ref_progression_exact(
                &format!("main@1#{}", "aa".repeat(32)),
                &samples
            ));
        }
        samples[1].response["lifecycle"]["selected_ref"] = valid_selected_ref;
    }

    #[test]
    fn response_hash_uses_the_cross_language_canonical_json_vector() -> ScorecardResult {
        let value = json!({
            "z": 1,
            "a": {
                "β": "值",
                "a": [true, null, 3],
            },
        });

        assert_eq!(
            json_sha256(&value)?,
            "4863f8ef3b164d0b123602b5932e180d861402f1477f1b956c1766845fe671cc"
        );
        Ok(())
    }

    #[test]
    fn control_preparation_paths_must_be_strictly_relative() {
        assert!(safe_relative_path(Path::new("dir/readiness.bin")));
        assert!(!safe_relative_path(Path::new("")));
        assert!(!safe_relative_path(Path::new("/absolute/readiness.bin")));
        assert!(!safe_relative_path(Path::new("../escape")));
        assert!(!safe_relative_path(Path::new("dir/../escape")));
        assert!(!safe_relative_path(Path::new("./readiness.bin")));
    }

    #[test]
    fn compact_invocation_proof_bounds_verbose_responses_and_rejects_bad_hashes() -> ScorecardResult
    {
        let full = CliInvocation {
            operation: "mpla_storage_admin".to_owned(),
            request_id: Some("compact-proof-test".to_owned()),
            outer_elapsed_ns: 123,
            response: json!({
                "action": "mount",
                "diagnostic_tree": "x".repeat(2 * 1024 * 1024),
            }),
        };
        let expected_sha256 = json_sha256(&full.response)?;
        let mut compact = compact_invocation(
            &full,
            json!({
                "action": "mount",
            }),
        )?;

        require_compact_invocation_proof(&compact)?;
        assert_eq!(
            compact.response["full_response_sha256"].as_str(),
            Some(expected_sha256.as_str())
        );
        assert!(serde_json::to_vec(&compact)?.len() < 1024);

        compact.response["full_response_sha256"] = Value::String("0".repeat(63));
        assert!(require_compact_invocation_proof(&compact).is_err());
        Ok(())
    }

    #[test]
    fn reclaim_control_materializations_preserves_immutable_publication_state() -> ScorecardResult {
        let root =
            std::env::temp_dir().join(format!("mpla-control-reclaim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("objects/loose/chunk/00"))?;
        fs::create_dir_all(root.join("refs/heads"))?;
        fs::create_dir_all(root.join("materializations/example/generations/1"))?;
        fs::create_dir_all(root.join("operations/materialization-op"))?;
        fs::write(root.join("objects/loose/chunk/00/sentinel"), b"immutable")?;
        fs::write(root.join("refs/heads/hidden-validation"), b"head")?;
        fs::write(
            root.join("operations/NONTERMINAL"),
            encoded_test_active_common_operations(&[[0x11; 32], [0x22; 32]]),
        )?;
        fs::write(
            root.join("operations/materialization-op/STATE"),
            serde_json::to_vec(&json!({
                "schema": "layerstack-materialization-operation-v3",
                "schema_version": 3,
                "operation_id": "materialization-op",
                "phase": "terminal",
                "terminal_outcome": "succeeded",
            }))?,
        )?;

        reclaim_control_materializations(&root)?;

        assert!(!root.join("materializations").exists());
        assert!(!root.join("operations/materialization-op").exists());
        assert!(root.join("operations/NONTERMINAL").is_file());
        assert_eq!(
            fs::read(root.join("objects/loose/chunk/00/sentinel"))?,
            b"immutable"
        );
        assert_eq!(
            fs::read(root.join("refs/heads/hidden-validation"))?,
            b"head"
        );
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn reclaim_control_materializations_refuses_corrupt_nonterminal_index() -> ScorecardResult {
        let root = std::env::temp_dir().join(format!(
            "mpla-control-reclaim-corrupt-index-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("materializations/example"))?;
        fs::create_dir_all(root.join("operations"))?;
        let mut corrupt = encoded_test_active_common_operations(&[[0x11; 32]]);
        *corrupt.last_mut().expect("checksum exists") ^= 0xff;
        fs::write(root.join("operations/NONTERMINAL"), corrupt)?;

        let error = reclaim_control_materializations(&root)
            .expect_err("corrupt active-operation index must prevent reclamation");
        assert!(error.to_string().contains("checksum failed"));
        assert!(root.join("materializations").is_dir());
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn reclaim_control_materializations_refuses_live_generation_subjects() -> ScorecardResult {
        let root =
            std::env::temp_dir().join(format!("mpla-control-reclaim-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("materializations/example"))?;
        fs::create_dir_all(root.join("refs/materialization-generation-subjects/subject/lease"))?;

        let error = reclaim_control_materializations(&root)
            .expect_err("live subject must prevent reclamation");
        assert!(error.to_string().contains("active generation subject"));
        assert!(root.join("materializations").is_dir());
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    fn encoded_test_active_common_operations(entries: &[[u8; 32]]) -> Vec<u8> {
        assert!(entries.len() <= MAX_NONTERMINAL_COMMON_OPERATIONS);
        assert!(entries.windows(2).all(|pair| pair[0] < pair[1]));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(ACTIVE_COMMON_OPERATIONS_MAGIC);
        bytes.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for entry in entries {
            bytes.extend_from_slice(entry);
        }
        let mut checksum = Sha256::new();
        checksum.update(ACTIVE_COMMON_OPERATIONS_CHECKSUM_DOMAIN);
        checksum.update(&bytes);
        bytes.extend_from_slice(&checksum.finalize());
        bytes
    }
}
