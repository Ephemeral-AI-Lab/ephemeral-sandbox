use std::error::Error;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use sandbox_runtime_mpla_poc::activation::{activate_exact, ExactActivationRequest};
use sandbox_runtime_mpla_poc::allocation::{
    create_allocation, destroy_workspace_allocation, open_allocation,
};
use sandbox_runtime_mpla_poc::evidence;
use sandbox_runtime_mpla_poc::inventory::capture_stable_pair;
use sandbox_runtime_mpla_poc::lease::{issue_workspace_lease, validate_deleter, validate_writer};
use sandbox_runtime_mpla_poc::locator::{
    ForwardLocatorEntry, LocatorDelta, LocatorExtent, LocatorStore, PayloadRootId,
    ReverseLocatorEntry,
};
use sandbox_runtime_mpla_poc::occ::{
    BranchOcc, ChangedPathSet, ConflictAllocation, OccPublication, OccPublishOutcome,
    RebasedCanonical,
};
use sandbox_runtime_mpla_poc::owner::{compare_and_adopt, current_owner};
use sandbox_runtime_mpla_poc::publication::{
    stationary_adopt, stationary_adopt_receipt_hit, ReceiptHitPublicationReceipt,
    StationaryPublicationRequest,
};
use sandbox_runtime_mpla_poc::recovery::{
    capture_recovery_allocation_identity, PublicationRecovery, RecoveryOutcome, RecoveryRequest,
};
use sandbox_runtime_mpla_poc::ref_store::{PairedRefStore, RefCommitOutcome};
use sandbox_runtime_mpla_poc::semantic::record::{RecordStreamReader, SemanticRecord};
use sandbox_runtime_mpla_poc::semantic::{
    build_incremental, build_with_output, capture_affected_paths, materialize_record_stream,
    write_affected_stream_from_snapshots, AffectedPathSnapshot, IncrementalBuildOutput,
    IncrementalBuildRequest, SemanticBuildOutput,
};
use sandbox_runtime_mpla_poc::{
    durable, populate_empty_fixture_root, ActivationOperationId, AllocationHandle, AllocationId,
    ArtifactStatus, AssertionReceipt, AttributionInput, CaseOutcome, CaseReceipt, CatalogBinding,
    EvidenceClass, FaultInjector, FixtureId, FixtureReceipt, FixtureTier, LocatorDurabilityReceipt,
    LocatorRefCandidate, NamedFaultInjector, NamedFaultPoint, OperationId, OwnerSubject,
    OwnerTransitionRequest, PairedRefValue, PocError, ProjectionRecipe, PublicationId,
    QualificationReceipt, ReceiptHitSealInput, RefSequence, RunId, SemanticBuildReceipt,
    SemanticBuildRequest, SessionId, StableAllocationReceipt, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

type CampaignResult<T = ()> = Result<T, Box<dyn Error>>;

const CASE_IDS: [&str; 14] = [
    "SM-01", "SM-02", "SM-03", "SM-04", "SM-05", "SM-06", "SM-07", "SM-08", "SM-09", "SM-10",
    "SM-11", "SM-12", "SM-14", "SM-13",
];
const BRANCH: &str = "main";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PreparedSemantic {
    receipt: SemanticBuildReceipt,
    record_stream_path: PathBuf,
    root_manifest_path: PathBuf,
}

impl From<SemanticBuildOutput> for PreparedSemantic {
    fn from(output: SemanticBuildOutput) -> Self {
        Self {
            receipt: output.receipt,
            record_stream_path: output.record_stream_path,
            root_manifest_path: output.root_manifest_path,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PreparedFixture {
    fixture_id: FixtureId,
    allocation_id: AllocationId,
    fixture: FixtureReceipt,
    semantic: Option<PreparedSemantic>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PreparationReceipt {
    schema_version: u32,
    run_id: RunId,
    fixtures: Vec<PreparedFixture>,
    canonical_object_dir: PathBuf,
    prepared_unix_ms: u64,
}

#[derive(Clone)]
struct Context {
    run_id: RunId,
    payload_root: PathBuf,
    control_root: PathBuf,
    fixtures_root: PathBuf,
    evidence_root: PathBuf,
    qualification_path: PathBuf,
    oracle_path: PathBuf,
    cli_path: PathBuf,
    catalog_binding_path: PathBuf,
    cgroup_procs_path: Option<PathBuf>,
    storage_cgroup_dir: Option<PathBuf>,
    preparation: PreparationReceipt,
}

#[derive(Clone, Debug, Serialize)]
struct StorageCgroupSnapshot {
    sampled_unix_ms: u64,
    memory_current: u64,
    memory_peak: u64,
    memory_high: String,
    memory_max: String,
    memory_events: std::collections::BTreeMap<String, u64>,
    memory_stat: std::collections::BTreeMap<String, u64>,
    process_ids: Vec<u32>,
}

#[derive(Clone, Debug, Serialize)]
struct CaseExecution {
    assertions: Vec<AssertionReceipt>,
    details: Value,
}

struct Published {
    selected_ref: PairedRefValue,
    allocation: AllocationHandle,
    roots: sandbox_runtime_mpla_poc::CanonicalRootPair,
}

struct OccOwnedCandidate {
    allocation: AllocationHandle,
    operation_id: OperationId,
    publication_id: PublicationId,
    owner_epoch: u64,
    accounted_bytes: u64,
    semantic: SemanticBuildReceipt,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Sm12ChildRequest {
    Owner {
        edge: String,
        allocation_root: PathBuf,
        stable: StableAllocationReceipt,
        request: OwnerTransitionRequest,
    },
    Publication {
        edge: String,
        recovery_root: PathBuf,
        locator_root: PathBuf,
        ref_root: PathBuf,
        occ_root: PathBuf,
        operation_id: OperationId,
        fault: NamedFaultPoint,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Sm12ChildWitness {
    schema_version: u32,
    edge: String,
    pid: u32,
    observed_error: String,
    fault_fired: bool,
    written_unix_ms: u64,
}

struct Sm12RecoveryCandidate {
    allocation: AllocationHandle,
    operation_id: OperationId,
    publication_id: PublicationId,
    owner_epoch: u64,
    accounted_bytes: u64,
    fixture_logical_bytes: u64,
    semantic: SemanticBuildReceipt,
    semantic_reused: bool,
    recovery_root: PathBuf,
    locator_root: PathBuf,
    ref_root: PathBuf,
    occ_root: PathBuf,
    branch: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PublishedSemanticState {
    semantic: PreparedSemantic,
    selected_ref: PairedRefValue,
    allocation_id: AllocationId,
    owner_epoch: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CliInvocation {
    argv: Vec<String>,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    outer_elapsed_ns: u64,
    response: Value,
}

#[derive(Clone, Debug, Serialize)]
struct Sm10Timeline {
    agent: u8,
    barrier_release_ns: u64,
    hash_started_ns: u64,
    hash_finished_ns: u64,
    response_ns: u64,
}

pub fn prepare() -> CampaignResult {
    let roots = Roots::from_env()?;
    fs::create_dir_all(&roots.evidence_root)?;
    let campaign_root = roots
        .control_root
        .join("campaign")
        .join(roots.run_id.as_str());
    fs::create_dir_all(&campaign_root)?;
    let empty_lower = campaign_root.join("empty-lower");
    fs::create_dir(&empty_lower)?;
    let canonical_object_dir = campaign_root.join("canonical");
    fs::create_dir_all(&canonical_object_dir)?;

    let mut fixtures = Vec::new();
    for fixture_id in [
        FixtureId::S1Code,
        FixtureId::S2Large,
        FixtureId::S3Small,
        FixtureId::S5Semantics,
    ] {
        let operation_id =
            OperationId::from_string(format!("{}-prepare-{}", roots.run_id, fixture_id.as_str()));
        let allocation = create_allocation(&roots.payload_root.join("allocations"), &operation_id)?;
        let fixture =
            populate_empty_fixture_root(&allocation.upper_dir, fixture_id, FixtureTier::Smoke)?;
        let semantic = if matches!(fixture_id, FixtureId::S1Code | FixtureId::S2Large) {
            Some(
                full_build(
                    &campaign_root,
                    &canonical_object_dir,
                    &allocation,
                    &format!("prepare-{}", fixture_id.as_str()),
                )?
                .into(),
            )
        } else {
            None
        };
        fixtures.push(PreparedFixture {
            fixture_id,
            allocation_id: allocation.descriptor.allocation_id,
            fixture,
            semantic,
        });
    }
    let receipt = PreparationReceipt {
        schema_version: SCHEMA_VERSION,
        run_id: roots.run_id,
        fixtures,
        canonical_object_dir,
        prepared_unix_ms: sandbox_runtime_mpla_poc::unix_time_ms()?,
    };
    durable::replace_json(&campaign_root.join("PREPARED.json"), &receipt)?;
    durable::replace_json(
        &roots.evidence_root.join("environment/preparation.json"),
        &receipt,
    )?;
    Ok(())
}

pub fn prepare_hv07() -> CampaignResult {
    let roots = Roots::from_env()?;
    fs::create_dir_all(&roots.evidence_root)?;
    let campaign_root = roots
        .control_root
        .join("campaign")
        .join(roots.run_id.as_str());
    fs::create_dir_all(&campaign_root)?;
    let empty_lower = campaign_root.join("empty-lower");
    fs::create_dir(&empty_lower)?;
    let canonical_object_dir = campaign_root.join("canonical");
    fs::create_dir_all(&canonical_object_dir)?;

    let operation_id = OperationId::from_string(format!("{}-prepare-hv07-s2-large", roots.run_id));
    let allocation = create_allocation(&roots.payload_root.join("allocations"), &operation_id)?;
    let fixture = populate_empty_fixture_root(
        &allocation.upper_dir,
        FixtureId::S2Large,
        FixtureTier::Smoke,
    )?;
    let receipt = PreparationReceipt {
        schema_version: SCHEMA_VERSION,
        run_id: roots.run_id,
        fixtures: vec![PreparedFixture {
            fixture_id: FixtureId::S2Large,
            allocation_id: allocation.descriptor.allocation_id,
            fixture,
            semantic: None,
        }],
        canonical_object_dir,
        prepared_unix_ms: sandbox_runtime_mpla_poc::unix_time_ms()?,
    };
    durable::replace_json(&campaign_root.join("PREPARED.json"), &receipt)?;
    durable::replace_json(
        &roots.evidence_root.join("environment/preparation.json"),
        &receipt,
    )?;
    Ok(())
}

pub fn run() -> CampaignResult {
    let context = Context::from_env()?;
    let filter = std::env::var("MPLA_POC_CASE_FILTER").ok();
    if let Some(case) = &filter {
        if !CASE_IDS.contains(&case.as_str()) {
            return Err(format!("unsupported smoke case {case}").into());
        }
    }
    let suite_started = Instant::now();
    let mut failures = Vec::new();
    for case_id in CASE_IDS {
        if filter
            .as_deref()
            .is_some_and(|selected| selected != case_id)
        {
            continue;
        }
        let result = run_case(&context, case_id);
        if let Err(error) = result {
            failures.push(format!("{case_id}: {error}"));
        }
    }
    let suite_elapsed = suite_started.elapsed();
    if suite_elapsed >= Duration::from_secs(180) {
        failures.push(format!(
            "suite exceeded 180 second hard stop: {suite_elapsed:?}"
        ));
    }
    durable::replace_json(
        &context.evidence_root.join("suite/summary.json"),
        &json!({
            "schema_version": SCHEMA_VERSION,
            "run_id": context.run_id,
            "duration_ns": ns(suite_elapsed),
            "hard_stop_ns": 180_000_000_000_u64,
            "target_ns": 150_000_000_000_u64,
            "failures": failures,
        }),
    )?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

fn run_case(context: &Context, case_id: &str) -> CampaignResult {
    let started_unix_ms = sandbox_runtime_mpla_poc::unix_time_ms()?;
    let storage_before = context.storage_cgroup_snapshot()?;
    let started = Instant::now();
    let execution = dispatch_case(context, case_id);
    let duration_ns = ns(started.elapsed());
    let storage_after = context.storage_cgroup_snapshot()?;
    let finished_unix_ms = sandbox_runtime_mpla_poc::unix_time_ms()?;
    let case_dir = context.evidence_root.join("cases").join(case_id);
    fs::create_dir_all(&case_dir)?;
    let result_path = case_dir.join("result.json");
    let (outcome, assertions, failures_and_unknowns, mut details) = match execution {
        Ok(execution) => {
            let failed = execution
                .assertions
                .iter()
                .filter(|assertion| !assertion.passed)
                .map(|assertion| {
                    format!(
                        "{} observed {}, expected {}",
                        assertion.name, assertion.observed, assertion.expected
                    )
                })
                .collect::<Vec<_>>();
            let outcome = if failed.is_empty() {
                CaseOutcome::Passed
            } else {
                CaseOutcome::Failed
            };
            (outcome, execution.assertions, failed, execution.details)
        }
        Err(error) => (
            CaseOutcome::Failed,
            Vec::new(),
            vec![error.to_string()],
            json!({"error": error.to_string()}),
        ),
    };
    let storage_evidence = json!({
        "before": storage_before,
        "after": storage_after,
    });
    match &mut details {
        Value::Object(fields) => {
            fields.insert("storage_cgroup".to_owned(), storage_evidence);
        }
        _ => {
            details = json!({
                "case": details,
                "storage_cgroup": storage_evidence,
            });
        }
    }
    durable::replace_json(&case_dir.join("details.json"), &details)?;
    let receipt = CaseReceipt {
        schema_version: SCHEMA_VERSION,
        run_id: context.run_id.clone(),
        case_id: case_id.to_owned(),
        outcome,
        evidence_class: evidence_class(case_id),
        started_unix_ms,
        finished_unix_ms,
        duration_ns,
        assertions,
        failures_and_unknowns,
        artifact_path: result_path.clone(),
    };
    durable::replace_json(&result_path, &receipt)?;
    if receipt.passes() {
        Ok(())
    } else {
        Err(format!("{case_id} did not pass").into())
    }
}

fn dispatch_case(context: &Context, case_id: &str) -> CampaignResult<CaseExecution> {
    match case_id {
        "SM-01" => sm_01(context),
        "SM-02" => sm_02(context),
        "SM-03" => sm_03(context),
        "SM-04" => sm_04(context),
        "SM-05" => sm_05(context),
        "SM-06" => sm_06(context),
        "SM-07" => sm_07(context),
        "SM-08" => sm_08(context),
        "SM-09" => sm_09(context),
        "SM-10" => sm_10(context),
        "SM-11" => sm_11(context),
        "SM-12" => sm_12(context),
        "SM-13" => sm_13(context),
        "SM-14" => sm_14(context),
        _ => Err(format!("unknown case {case_id}").into()),
    }
}

fn sm_01(context: &Context) -> CampaignResult<CaseExecution> {
    let receipt: QualificationReceipt = evidence::read_json(&context.qualification_path)?;
    let mandatory_failures = receipt
        .probes
        .iter()
        .filter(|probe| {
            probe.mandatory && probe.status != sandbox_runtime_mpla_poc::ProbeStatus::Passed
        })
        .count();
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "qualification_status",
                receipt.status == ArtifactStatus::Passed,
                format!("{:?}", receipt.status),
                "Passed",
            ),
            assertion(
                "mandatory_probe_failures",
                mandatory_failures == 0,
                mandatory_failures,
                0,
            ),
        ],
        details: serde_json::to_value(receipt)?,
    })
}

fn sm_02(context: &Context) -> CampaignResult<CaseExecution> {
    let operation_id = OperationId::from_string(format!("{}-sm02", context.run_id));
    let allocation = create_allocation(&context.arena_root(), &operation_id)?;
    let allocation_id = allocation.descriptor.allocation_id.clone();
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &context.control_root,
        allocation,
        lease.clone(),
        context.raw_session_lower_dirs(),
        context.cgroup_procs_path.clone(),
    )?;
    let command = session.execute(
        &lease.writer,
        Path::new("/bin/sh"),
        &["-c".to_owned(), "printf smoke > lifecycle-sm02".to_owned()],
        Duration::from_secs(2),
    )?;
    drop(session);
    destroy_workspace_allocation(&context.arena_root(), &allocation_id, &lease.deleter)?;
    Ok(CaseExecution {
        assertions: vec![
            assertion("exec_success", command.success, command.success, true),
            assertion(
                "allocation_cleaned",
                open_allocation(&context.arena_root(), &allocation_id).is_err(),
                "absent",
                "absent",
            ),
        ],
        details: json!({"allocation_id": allocation_id, "command": command}),
    })
}

fn sm_03(context: &Context) -> CampaignResult<CaseExecution> {
    let fixture = context.fixture(FixtureId::S1Code)?;
    let prior = fixture
        .semantic
        .as_ref()
        .ok_or("S1 preparation has no semantic state")?;
    let allocation = open_allocation(&context.arena_root(), &fixture.allocation_id)?;
    let allocation_path = allocation.allocation_root.clone();
    let operation_id = OperationId::from_string(format!("{}-sm03-publish", context.run_id));
    let publication_id = PublicationId::from_string(format!("{}-sm03", context.run_id));
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &context.control_root,
        allocation.clone(),
        lease.clone(),
        context.raw_session_lower_dirs(),
        context.cgroup_procs_path.clone(),
    )?;
    let affected_paths = (0..10_u64)
        .map(|index| {
            PathBuf::from(format!(
                "src/d{index:04}/module-{index:08}.rs",
                index = index
            ))
        })
        .collect::<Vec<_>>();
    let workspace = session
        .workspace_root()
        .ok_or("S1 workspace is not mounted")?
        .to_path_buf();
    let receipt_work = context
        .control_root
        .join("campaign")
        .join(context.run_id.as_str())
        .join("receipt-sm03");
    fs::create_dir_all(&receipt_work)?;
    let before = capture_affected_paths(&workspace, &affected_paths, &receipt_work.join("before"))?;
    let command = session.execute(
        &lease.writer,
        Path::new("/bin/sh"),
        &s1_edit_arguments(&affected_paths),
        Duration::from_secs(5),
    )?;
    if !command.success {
        return Err("S1 edit command failed".into());
    }
    let after = capture_affected_paths(&workspace, &affected_paths, &receipt_work.join("after"))?;
    let affected_stream = receipt_work.join("affected.records");
    let affected_stream_sha256 =
        write_affected_stream_from_snapshots(&affected_stream, &before, &after)?;
    let seal_input = ReceiptHitSealInput {
        schema_version: SCHEMA_VERSION,
        affected_stream: affected_stream.clone(),
        affected_stream_sha256: affected_stream_sha256.clone(),
        affected_paths: affected_paths.clone(),
    };

    let publish_started = Instant::now();
    let (stationary, incremental, stationary_elapsed, incremental_elapsed) =
        parallel_receipt_hit_publication(
            &mut session,
            StationaryPublicationRequest {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                publication_id: publication_id.clone(),
            },
            context.control_root.join("operations"),
            seal_input,
            IncrementalBuildRequest {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                prior_manifest: prior.root_manifest_path.clone(),
                expected_prior_roots: prior.receipt.roots.clone(),
                expected_prior_record_stream_sha256: prior.receipt.record_stream_sha256.clone(),
                affected_stream,
                affected_stream_sha256,
                affected_ranges_complete: true,
                canonical_object_dir: context.preparation.canonical_object_dir.clone(),
                attribution: attribution(),
            },
        )?;
    let ref_started = Instant::now();
    let selected_ref = install_ref(
        context,
        &allocation,
        &incremental.receipt,
        stationary.stationary.adoption.new_owner.owner_epoch,
        stationary.stationary.stable.after.allocated_bytes,
        &operation_id,
        &publication_id,
    )?;
    let ref_elapsed = ref_started.elapsed();
    let publish_elapsed = publish_started.elapsed();
    let record_stream_path = materialize_record_stream(
        &incremental.root_manifest_path,
        &context.preparation.canonical_object_dir,
    )?;
    let state = PublishedSemanticState {
        semantic: PreparedSemantic {
            receipt: incremental.receipt.clone(),
            record_stream_path,
            root_manifest_path: incremental.root_manifest_path.clone(),
        },
        selected_ref: selected_ref.clone(),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        owner_epoch: stationary.stationary.adoption.new_owner.owner_epoch,
    };
    durable::replace_json(&context.campaign_root().join("SM03_STATE.json"), &state)?;

    let forced_started = Instant::now();
    let forced = full_build(
        &context
            .control_root
            .join("campaign")
            .join(context.run_id.as_str()),
        &context.preparation.canonical_object_dir,
        &allocation,
        "forced-sm03",
    )?;
    let forced_elapsed = forced_started.elapsed();
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "receipt_hit_publish_budget",
                publish_elapsed <= Duration::from_millis(100),
                format!("{publish_elapsed:?}"),
                "<=100ms",
            ),
            assertion(
                "forced_miss_budget",
                forced_elapsed <= Duration::from_millis(1_070),
                format!("{forced_elapsed:?}"),
                "<=1.070s",
            ),
            assertion(
                "forced_miss_roots_match",
                incremental.receipt.roots == forced.receipt.roots,
                format!("{:?}", incremental.receipt.roots),
                format!("{:?}", forced.receipt.roots),
            ),
            assertion(
                "payload_path_stationary",
                allocation.allocation_root == allocation_path,
                allocation.allocation_root.display(),
                allocation_path.display(),
            ),
            assertion(
                "immutable_payload_reads",
                incremental.immutable_payload_bytes_read == 0,
                incremental.immutable_payload_bytes_read,
                0,
            ),
            assertion(
                "selected_ref_matches_roots",
                selected_ref.roots == incremental.receipt.roots,
                format!("{:?}", selected_ref.roots),
                format!("{:?}", incremental.receipt.roots),
            ),
        ],
        details: json!({
            "publication": stationary,
            "semantic": incremental.receipt,
            "selected_ref": selected_ref,
            "publish_duration_ns": ns(publish_elapsed),
            "stationary_duration_ns": ns(stationary_elapsed),
            "incremental_duration_ns": ns(incremental_elapsed),
            "locator_ref_duration_ns": ns(ref_elapsed),
            "forced_miss_duration_ns": ns(forced_elapsed),
            "forced_miss_semantic": forced.receipt,
            "before_payload_bytes_read": before.payload_bytes_read,
            "after_payload_bytes_read": after.payload_bytes_read,
        }),
    })
}

fn sm_04(context: &Context) -> CampaignResult<CaseExecution> {
    let fixture = context.fixture(FixtureId::S1Code)?;
    let allocation = open_allocation(&context.arena_root(), &fixture.allocation_id)?;
    let lease_path = allocation.owner_dir.join("LEASE");
    let permissions = fs::metadata(&allocation.upper_dir)?.permissions();
    fs::set_permissions(&allocation.upper_dir, fs::Permissions::from_mode(0o000))?;
    let result = (|| {
        Ok::<_, Box<dyn Error>>((
            validate_writer(&allocation.allocation_root, &stale_writer(&lease_path)?),
            validate_deleter(&allocation.allocation_root, &stale_deleter(&lease_path)?),
        ))
    })();
    fs::set_permissions(&allocation.upper_dir, permissions.clone())?;
    let (writer, deleter) = result?;
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "stale_writer_rejected",
                writer.is_err(),
                writer.is_err(),
                true,
            ),
            assertion(
                "stale_deleter_rejected",
                deleter.is_err(),
                deleter.is_err(),
                true,
            ),
            assertion(
                "payload_permissions_unchanged",
                fs::metadata(&allocation.upper_dir)?.permissions() == permissions,
                "unchanged",
                "unchanged",
            ),
        ],
        details: json!({
            "writer_error": writer.err().map(|error| error.to_string()),
            "deleter_error": deleter.err().map(|error| error.to_string()),
        }),
    })
}

fn sm_05(context: &Context) -> CampaignResult<CaseExecution> {
    let published = published_s1(context)?;
    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: published.roots.clone(),
        base_allocation_id: published.allocation.descriptor.allocation_id.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: Vec::new(),
    };
    let mut durations = Vec::new();
    let mut empty_uppers = Vec::new();
    for sample in 0..4_u8 {
        let activated = activate_exact(ExactActivationRequest {
            activation_operation_id: ActivationOperationId::from_string(format!(
                "{}-sm05-{sample}",
                context.run_id
            )),
            allocation_operation_id: OperationId::from_string(format!(
                "{}-sm05-allocation-{sample}",
                context.run_id
            )),
            selected_ref: published.selected_ref.clone(),
            recipe: recipe.clone(),
            payload_allocations: vec![published.allocation.clone()],
            arena_root: context.arena_root(),
            control_root: context.control_root.clone(),
            cgroup_procs_path: context.cgroup_procs_path.clone(),
            readiness_path: PathBuf::from("src/d0000/module-00000000.rs"),
            readiness_contains: None,
            readiness_timeout: Duration::from_secs(2),
        })?;
        durations.push(activated.receipt.elapsed_ns);
        empty_uppers.push(activated.receipt.fresh_upper_empty_before_mount);
        let allocation_id = activated
            .session
            .allocation()
            .descriptor
            .allocation_id
            .clone();
        let deleter = activated.session.mutable_lease().deleter.clone();
        drop(activated);
        destroy_workspace_allocation(&context.arena_root(), &allocation_id, &deleter)?;
    }
    let measured = &durations[1..];
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "fresh_upper_empty",
                empty_uppers.iter().all(|empty| *empty),
                format!("{empty_uppers:?}"),
                "all true",
            ),
            assertion(
                "three_warm_activations_within_budget",
                measured.iter().all(|duration| *duration <= 100_000_000),
                format!("{measured:?}"),
                "each <=100000000ns",
            ),
        ],
        details: json!({"activation_duration_ns": durations, "warmup_samples": 1}),
    })
}

fn sm_06(context: &Context) -> CampaignResult<CaseExecution> {
    let initial: PublishedSemanticState =
        durable::read_json(&context.campaign_root().join("SM03_STATE.json"))?;
    let base = open_allocation(&context.arena_root(), &initial.allocation_id)?;
    let mut prior_receipt = initial.semantic.receipt.clone();
    let mut prior_manifest = initial.semantic.root_manifest_path.clone();
    let mut selected_ref = initial.selected_ref.clone();
    let mut recent = Vec::<AllocationHandle>::new();
    let mut carrier = None::<AllocationHandle>;
    let mut durations_ns = Vec::new();
    let mut activation_durations_ns = Vec::new();
    let mut stationary_durations_ns = Vec::new();
    let mut incremental_durations_ns = Vec::new();
    let mut locator_ref_durations_ns = Vec::new();
    let mut affected_input_bytes = Vec::new();
    let mut immutable_payload_bytes = Vec::new();
    let mut sequences = Vec::new();
    let mut final_owner_epoch = initial.owner_epoch;
    let campaign_started = Instant::now();

    for index in 0..16_u8 {
        let recipe = ProjectionRecipe {
            schema_version: SCHEMA_VERSION,
            roots: prior_receipt.roots.clone(),
            base_allocation_id: base.descriptor.allocation_id.clone(),
            net_delta_carrier_id: carrier
                .as_ref()
                .map(|allocation| allocation.descriptor.allocation_id.clone()),
            recent_delta_ids: recent
                .iter()
                .rev()
                .map(|allocation| allocation.descriptor.allocation_id.clone())
                .collect(),
        };
        let mut payload_allocations = vec![base.clone()];
        if let Some(allocation) = &carrier {
            payload_allocations.push(allocation.clone());
        }
        payload_allocations.extend(recent.iter().cloned());
        let activation_started = Instant::now();
        let activated = activate_exact(ExactActivationRequest {
            activation_operation_id: ActivationOperationId::from_string(format!(
                "{}-sm06-activate-{index:02}",
                context.run_id
            )),
            allocation_operation_id: OperationId::from_string(format!(
                "{}-sm06-allocation-{index:02}",
                context.run_id
            )),
            selected_ref: selected_ref.clone(),
            recipe,
            payload_allocations,
            arena_root: context.arena_root(),
            control_root: context.control_root.clone(),
            cgroup_procs_path: context.cgroup_procs_path.clone(),
            readiness_path: PathBuf::from("src/d0000/module-00000000.rs"),
            readiness_contains: None,
            readiness_timeout: Duration::from_secs(2),
        })?;
        activation_durations_ns.push(ns(activation_started.elapsed()));
        let mut session = activated.session;
        let allocation = session.allocation().clone();
        let lease = session.mutable_lease().clone();
        let path = s1_module_path(200 + u64::from(index));
        let affected_paths = vec![path.clone()];
        let work = context
            .campaign_root()
            .join("receipt-sm06")
            .join(format!("{index:02}"));
        fs::create_dir_all(&work)?;
        let workspace = session
            .workspace_root()
            .ok_or("SM-06 workspace disappeared")?
            .to_path_buf();
        let before = capture_affected_paths(&workspace, &affected_paths, &work.join("before"))?;
        let edit = session.execute(
            &lease.writer,
            Path::new("/bin/sh"),
            &[
                "-c".to_owned(),
                format!("printf 'delta-{index:02}' > '{}'", path.display()),
            ],
            Duration::from_secs(2),
        )?;
        if !edit.success {
            return Err(format!("SM-06 edit {index} failed").into());
        }
        let after = capture_affected_paths(&workspace, &affected_paths, &work.join("after"))?;
        let affected_stream = work.join("affected.records");
        let affected_stream_sha256 =
            write_affected_stream_from_snapshots(&affected_stream, &before, &after)?;
        let seal_input = ReceiptHitSealInput {
            schema_version: SCHEMA_VERSION,
            affected_stream: affected_stream.clone(),
            affected_stream_sha256: affected_stream_sha256.clone(),
            affected_paths,
        };
        let operation_id =
            OperationId::from_string(format!("{}-sm06-publish-{index:02}", context.run_id));
        let publication_id =
            PublicationId::from_string(format!("{}-sm06-{index:02}", context.run_id));

        let publish_started = Instant::now();
        let (stationary, incremental, stationary_elapsed, incremental_elapsed) =
            parallel_receipt_hit_publication(
                &mut session,
                StationaryPublicationRequest {
                    schema_version: SCHEMA_VERSION,
                    operation_id: operation_id.clone(),
                    publication_id: publication_id.clone(),
                },
                context.control_root.join("operations"),
                seal_input,
                IncrementalBuildRequest {
                    schema_version: SCHEMA_VERSION,
                    operation_id: operation_id.clone(),
                    prior_manifest: prior_manifest.clone(),
                    expected_prior_roots: prior_receipt.roots.clone(),
                    expected_prior_record_stream_sha256: prior_receipt.record_stream_sha256.clone(),
                    affected_stream,
                    affected_stream_sha256,
                    affected_ranges_complete: true,
                    canonical_object_dir: context.preparation.canonical_object_dir.clone(),
                    attribution: attribution(),
                },
            )?;
        stationary_durations_ns.push(ns(stationary_elapsed));
        incremental_durations_ns.push(ns(incremental_elapsed));
        let ref_started = Instant::now();
        selected_ref = install_ref(
            context,
            &allocation,
            &incremental.receipt,
            stationary.stationary.adoption.new_owner.owner_epoch,
            stationary.stationary.stable.after.allocated_bytes,
            &operation_id,
            &publication_id,
        )?;
        locator_ref_durations_ns.push(ns(ref_started.elapsed()));
        final_owner_epoch = stationary.stationary.adoption.new_owner.owner_epoch;
        let publish_elapsed = publish_started.elapsed();
        durations_ns.push(ns(publish_elapsed));
        affected_input_bytes.push(incremental.affected_input_bytes);
        immutable_payload_bytes.push(incremental.immutable_payload_bytes_read);
        sequences.push(selected_ref.sequence.get());
        prior_receipt = incremental.receipt;
        prior_manifest = incremental.root_manifest_path;
        recent.push(allocation);

        if index == 7 {
            carrier = Some(build_tiny_delta_carrier(context, &base, &initial, &recent)?);
            recent.clear();
        }
    }
    let campaign_elapsed = campaign_started.elapsed();
    let prior = PreparedSemantic {
        record_stream_path: materialize_record_stream(
            &prior_manifest,
            &context.preparation.canonical_object_dir,
        )?,
        root_manifest_path: prior_manifest,
        receipt: prior_receipt,
    };

    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: prior.receipt.roots.clone(),
        base_allocation_id: base.descriptor.allocation_id.clone(),
        net_delta_carrier_id: carrier
            .as_ref()
            .map(|allocation| allocation.descriptor.allocation_id.clone()),
        recent_delta_ids: recent
            .iter()
            .rev()
            .map(|allocation| allocation.descriptor.allocation_id.clone())
            .collect(),
    };
    let mut payload_allocations = vec![base];
    if let Some(allocation) = &carrier {
        payload_allocations.push(allocation.clone());
    }
    payload_allocations.extend(recent.iter().cloned());
    let validation = activate_exact(ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(format!(
            "{}-sm06-validation",
            context.run_id
        )),
        allocation_operation_id: OperationId::from_string(format!(
            "{}-sm06-validation-allocation",
            context.run_id
        )),
        selected_ref: selected_ref.clone(),
        recipe,
        payload_allocations,
        arena_root: context.arena_root(),
        control_root: context.control_root.clone(),
        cgroup_procs_path: context.cgroup_procs_path.clone(),
        readiness_path: s1_module_path(215),
        readiness_contains: Some(b"delta-15".to_vec()),
        readiness_timeout: Duration::from_secs(2),
    })?;
    let validation_allocation_id = validation
        .session
        .allocation()
        .descriptor
        .allocation_id
        .clone();
    let validation_deleter = validation.session.mutable_lease().deleter.clone();
    let validation_workspace = validation
        .session
        .workspace_root()
        .ok_or("SM-06 validation workspace missing")?
        .to_path_buf();
    let full = build_with_output(&SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string(format!("{}-sm06-full-check", context.run_id)),
        allocation_id: validation_allocation_id.clone(),
        sealed_tree: validation_workspace,
        spool_dir: context
            .campaign_root()
            .join("spool")
            .join(format!("sm06-full-{}", OperationId::new())),
        canonical_object_dir: context.preparation.canonical_object_dir.clone(),
        attribution: attribution(),
    })?;
    drop(validation);
    destroy_workspace_allocation(
        &context.arena_root(),
        &validation_allocation_id,
        &validation_deleter,
    )?;
    let early_median = median_u64(&durations_ns[..4]);
    let late_median = median_u64(&durations_ns[12..]);
    let state = PublishedSemanticState {
        semantic: prior.clone(),
        selected_ref: selected_ref.clone(),
        allocation_id: recent
            .last()
            .ok_or("SM-06 has no final delta")?
            .descriptor
            .allocation_id
            .clone(),
        owner_epoch: final_owner_epoch,
    };
    durable::replace_json(&context.campaign_root().join("SM06_STATE.json"), &state)?;
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "campaign_budget",
                campaign_elapsed < Duration::from_secs(2),
                format!("{campaign_elapsed:?}"),
                "<2s",
            ),
            assertion(
                "each_receipt_hit_budget",
                durations_ns.iter().all(|duration| *duration <= 100_000_000),
                format!("{durations_ns:?}"),
                "each <=100000000ns",
            ),
            assertion(
                "zero_immutable_payload_reads",
                immutable_payload_bytes.iter().all(|bytes| *bytes == 0),
                format!("{immutable_payload_bytes:?}"),
                "all zero",
            ),
            assertion(
                "full_rebuild_root_equivalence",
                prior.receipt.roots == full.receipt.roots,
                format!("{:?}", prior.receipt.roots),
                format!("{:?}", full.receipt.roots),
            ),
            assertion(
                "no_late_latency_slope",
                late_median <= early_median.saturating_mul(2).saturating_add(2_000_000),
                late_median,
                early_median.saturating_mul(2).saturating_add(2_000_000),
            ),
            assertion(
                "paired_ref_progressed_16_times",
                sequences
                    .windows(2)
                    .all(|window| window[1] == window[0] + 1),
                format!("{sequences:?}"),
                "strictly contiguous",
            ),
        ],
        details: json!({
            "duration_ns": durations_ns,
            "activation_duration_ns": activation_durations_ns,
            "stationary_duration_ns": stationary_durations_ns,
            "incremental_duration_ns": incremental_durations_ns,
            "locator_ref_duration_ns": locator_ref_durations_ns,
            "affected_input_bytes": affected_input_bytes,
            "immutable_payload_bytes_read": immutable_payload_bytes,
            "ref_sequences": sequences,
            "early_median_ns": early_median,
            "middle_median_ns": median_u64(&durations_ns[6..10]),
            "late_median_ns": late_median,
            "campaign_duration_ns": ns(campaign_elapsed),
            "carrier_allocation_id": carrier.map(|allocation| allocation.descriptor.allocation_id),
            "final_semantic": prior.receipt,
            "forced_full_semantic": full.receipt,
        }),
    })
}

fn sm_07(context: &Context) -> CampaignResult<CaseExecution> {
    let fixture = context.fixture(FixtureId::S2Large)?;
    let prior = fixture
        .semantic
        .clone()
        .ok_or("SM-07 prepared semantic state is missing")?;
    let allocation = open_allocation(&context.arena_root(), &fixture.allocation_id)?;
    let operation_id = OperationId::from_string(format!("{}-sm07-publish", context.run_id));
    let publication_id = PublicationId::from_string(format!("{}-sm07", context.run_id));
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &context.control_root,
        allocation.clone(),
        lease.clone(),
        context.raw_session_lower_dirs(),
        context.cgroup_procs_path.clone(),
    )?;
    let relative = PathBuf::from("large-0.bin");
    let before = prior_changed_window_snapshot(&prior.record_stream_path, &relative, 0)?;
    let edit = session.execute(
        &lease.writer,
        Path::new("/bin/sh"),
        &[
            "-c".to_owned(),
            "dd if=/dev/zero of=large-0.bin bs=4096 count=1 conv=notrunc status=none".to_owned(),
        ],
        Duration::from_secs(2),
    )?;
    if !edit.success {
        return Err("SM-07 4 KiB in-place edit failed".into());
    }
    let workspace = session
        .workspace_root()
        .ok_or("SM-07 workspace disappeared")?;
    let after = changed_window_snapshot(workspace, &relative, 0, &before)?;
    let receipt_root = context.campaign_root().join("receipt-sm07");
    fs::create_dir_all(&receipt_root)?;
    let affected_stream = receipt_root.join("affected.records");
    let affected_stream_sha256 =
        write_affected_stream_from_snapshots(&affected_stream, &before, &after)?;
    let seal_input = ReceiptHitSealInput {
        schema_version: SCHEMA_VERSION,
        affected_stream: affected_stream.clone(),
        affected_stream_sha256: affected_stream_sha256.clone(),
        affected_paths: vec![relative],
    };

    let publication_started = Instant::now();
    let stationary = stationary_adopt_receipt_hit(
        &mut session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
        },
        &context.control_root.join("operations"),
        &seal_input,
        &mut FaultInjector::default(),
    )?;
    let incremental = build_incremental(&IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        prior_manifest: prior.root_manifest_path,
        expected_prior_roots: prior.receipt.roots,
        expected_prior_record_stream_sha256: prior.receipt.record_stream_sha256,
        affected_stream,
        affected_stream_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: context.preparation.canonical_object_dir.clone(),
        attribution: attribution(),
    })?;
    let selected_ref = install_ref(
        context,
        &allocation,
        &incremental.receipt,
        stationary.stationary.adoption.new_owner.owner_epoch,
        stationary.stationary.stable.after.allocated_bytes,
        &operation_id,
        &publication_id,
    )?;
    let publication_elapsed = publication_started.elapsed();
    let forced = full_build(
        &context.campaign_root(),
        &context.preparation.canonical_object_dir,
        &allocation,
        "sm07-forced-check",
    )?;
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "publication_budget",
                publication_elapsed < Duration::from_secs(5),
                format!("{publication_elapsed:?}"),
                "<5s",
            ),
            assertion(
                "changed_window_payload_read",
                after.payload_bytes_read == 32 * 1024,
                after.payload_bytes_read,
                32 * 1024,
            ),
            assertion(
                "immutable_payload_read",
                incremental.immutable_payload_bytes_read == 0,
                incremental.immutable_payload_bytes_read,
                0,
            ),
            assertion(
                "bounded_affected_records",
                incremental.affected_record_count <= 2,
                incremental.affected_record_count,
                "<=2",
            ),
            assertion(
                "full_rebuild_root_equivalence",
                incremental.receipt.roots == forced.receipt.roots,
                format!("{:?}", incremental.receipt.roots),
                format!("{:?}", forced.receipt.roots),
            ),
            assertion(
                "stationary_identity",
                stationary.stationary.stable.before == stationary.stationary.stable.after,
                "physical snapshot pair",
                "exactly equal",
            ),
        ],
        details: json!({
            "publication": stationary,
            "incremental": incremental.receipt,
            "forced": forced.receipt,
            "selected_ref": selected_ref,
            "publication_duration_ns": ns(publication_elapsed),
            "before_payload_bytes_read": before.payload_bytes_read,
            "after_payload_bytes_read": after.payload_bytes_read,
            "affected_record_count": incremental.affected_record_count,
            "affected_input_bytes": incremental.affected_input_bytes,
            "immutable_payload_bytes_read": incremental.immutable_payload_bytes_read,
            "changed_range": {
                "path": "large-0.bin",
                "offset": 0,
                "length": 4096,
                "semantic_window_bytes": 32 * 1024,
            },
        }),
    })
}

fn sm_08(context: &Context) -> CampaignResult<CaseExecution> {
    let fixture = context.fixture(FixtureId::S3Small)?;
    let allocation = open_allocation(&context.arena_root(), &fixture.allocation_id)?;
    let started = Instant::now();
    let output = full_build(
        &context
            .control_root
            .join("campaign")
            .join(context.run_id.as_str()),
        &context
            .control_root
            .join("campaign")
            .join(context.run_id.as_str())
            .join("sm08-objects"),
        &allocation,
        "sm08",
    )?;
    let elapsed = started.elapsed();
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "semantic_budget",
                elapsed < Duration::from_secs(20),
                format!("{elapsed:?}"),
                "<20s",
            ),
            assertion(
                "fd_bound",
                output.receipt.peak_open_data_fds <= 16,
                output.receipt.peak_open_data_fds,
                16,
            ),
            assertion(
                "worker_bound",
                output.receipt.peak_data_workers <= 4,
                output.receipt.peak_data_workers,
                4,
            ),
        ],
        details: json!({"semantic": output.receipt, "duration_ns": ns(elapsed)}),
    })
}

fn sm_09(context: &Context) -> CampaignResult<CaseExecution> {
    let fixture = context.fixture(FixtureId::S5Semantics)?;
    let allocation = open_allocation(&context.arena_root(), &fixture.allocation_id)?;
    let operation_id = OperationId::from_string(format!("{}-sm09-publish", context.run_id));
    let publication_id = PublicationId::from_string(format!("{}-sm09", context.run_id));
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &context.control_root,
        allocation.clone(),
        lease,
        context.raw_session_lower_dirs(),
        context.cgroup_procs_path.clone(),
    )?;

    let publication_started = Instant::now();
    let stationary = stationary_adopt(
        &mut session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
        },
        &context.control_root.join("operations"),
        &mut FaultInjector::default(),
    )?;
    let candidate = full_build(
        &context.campaign_root(),
        &context.preparation.canonical_object_dir,
        &allocation,
        "sm09-candidate",
    )?;
    let selected_ref = install_ref(
        context,
        &allocation,
        &candidate.receipt,
        stationary.adoption.new_owner.owner_epoch,
        stationary.stable.after.allocated_bytes,
        &operation_id,
        &publication_id,
    )?;
    let publication_elapsed = publication_started.elapsed();

    let case_root = context.evidence_root.join("cases/SM-09");
    fs::create_dir_all(&case_root)?;
    let oracle_records = case_root.join("oracle.records");
    let oracle_started = Instant::now();
    let oracle = run_oracle(&context.oracle_path, &allocation.upper_dir, &oracle_records)?;
    let oracle_elapsed = oracle_started.elapsed();

    let substitute_operation =
        OperationId::from_string(format!("{}-sm09-substitute-allocation", context.run_id));
    let substitute = create_allocation(&context.arena_root(), &substitute_operation)?;
    let substitute_lease =
        issue_workspace_lease(&substitute, SessionId::new(), &substitute_operation)?;
    copy_tree_test_only(&allocation.upper_dir, &substitute.upper_dir)?;
    let substituted = full_build(
        &context.campaign_root(),
        &context.preparation.canonical_object_dir,
        &substitute,
        "sm09-substituted",
    )?;
    let physical_independence = different_representative_inode(
        &allocation.upper_dir,
        &substitute.upper_dir,
        Path::new("tree/d0000/node-00000000.bin"),
    )?;
    destroy_workspace_allocation(
        &context.arena_root(),
        &substitute.descriptor.allocation_id,
        &substitute_lease.deleter,
    )?;

    let streams_equal = files_equal_bounded(&candidate.record_stream_path, &oracle_records)?;
    let roots_equal = oracle["root_id"].as_str() == Some(candidate.receipt.roots.root_id.as_str())
        && oracle["attribution_root_id"].as_str()
            == Some(candidate.receipt.roots.attribution_root_id.as_str())
        && oracle["record_stream_sha256"].as_str()
            == Some(candidate.receipt.record_stream_sha256.as_str())
        && oracle["record_count"].as_u64() == Some(candidate.receipt.entry_count);
    let substitution_equal = candidate.receipt.roots == substituted.receipt.roots
        && files_equal_bounded(
            &candidate.record_stream_path,
            &substituted.record_stream_path,
        )?;
    let total_elapsed = publication_elapsed
        .saturating_add(oracle_elapsed)
        .saturating_add(Duration::from_nanos(
            substituted
                .receipt
                .phase_spans
                .iter()
                .find(|span| span.phase == "semantic-total")
                .map_or(0, |span| span.elapsed_ns),
        ));
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "publication_budget",
                publication_elapsed < Duration::from_secs(15),
                format!("{publication_elapsed:?}"),
                "<15s",
            ),
            assertion(
                "case_budget",
                total_elapsed < Duration::from_secs(15),
                format!("{total_elapsed:?}"),
                "<15s",
            ),
            assertion("independent_oracle_roots", roots_equal, roots_equal, true),
            assertion(
                "independent_oracle_record_bytes",
                streams_equal,
                streams_equal,
                true,
            ),
            assertion(
                "physical_substitution_identity",
                physical_independence,
                physical_independence,
                true,
            ),
            assertion(
                "physical_substitution_semantics",
                substitution_equal,
                substitution_equal,
                true,
            ),
            assertion(
                "oracle_memory_bound",
                oracle["peak_managed_bytes"].as_u64().unwrap_or(u64::MAX) <= 8 * 1024 * 1024,
                oracle["peak_managed_bytes"].clone(),
                8 * 1024 * 1024,
            ),
            assertion(
                "oracle_fd_bound",
                oracle["peak_open_data_fds"].as_u64().unwrap_or(u64::MAX) <= 16,
                oracle["peak_open_data_fds"].clone(),
                16,
            ),
        ],
        details: json!({
            "publication": stationary,
            "candidate": candidate.receipt,
            "oracle": oracle,
            "substituted": substituted.receipt,
            "selected_ref": selected_ref,
            "publication_duration_ns": ns(publication_elapsed),
            "oracle_duration_ns": ns(oracle_elapsed),
            "test_only_reconstruction": {
                "category": "oracle-fixture-copy",
                "excluded_from_publication_peak": true,
                "source_allocation_id": allocation.descriptor.allocation_id,
                "substituted_allocation_id": substitute.descriptor.allocation_id,
                "representative_inode_changed": physical_independence,
            },
        }),
    })
}

fn sm_10(context: &Context) -> CampaignResult<CaseExecution> {
    let case_started = Instant::now();
    let before_allocations = allocation_directory_count(&context.payload_root)?;
    let controller = sandbox_runtime_mpla_poc::AdmissionController::new();
    let mut guards = (0..5_u8)
        .map(|_| controller.submit(0))
        .collect::<Result<Vec<_>, PocError>>()?;
    let snapshot = controller.snapshot()?;
    let fifth = guards.pop().ok_or("SM-10 lacks fifth admission guard")?;
    let barrier = Arc::new(Barrier::new(4));
    let (candidates, timeline) = std::thread::scope(|scope| {
        let handles = guards
            .into_iter()
            .enumerate()
            .map(|(index, guard)| {
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    let _guard = guard;
                    prepare_sm10_candidate(
                        context,
                        u8::try_from(index).map_err(|error| error.to_string())?,
                        &barrier,
                        case_started,
                    )
                    .map_err(|error| error.to_string())
                })
            })
            .collect::<Vec<_>>();
        let mut candidates = Vec::new();
        let mut timeline = Vec::new();
        for handle in handles {
            let (candidate, sample) = handle
                .join()
                .map_err(|_| "SM-10 worker panicked".to_owned())??;
            candidates.push(candidate);
            timeline.push(sample);
        }
        Ok::<_, String>((candidates, timeline))
    })
    .map_err(|error| -> Box<dyn Error> { error.into() })?;
    let after_allocations = allocation_directory_count(&context.payload_root)?;

    let locator_store = LocatorStore::open(context.campaign_root().join("locators"))?;
    let ref_store = PairedRefStore::open(context.campaign_root().join("refs"))?;
    let occ = BranchOcc::open(context.campaign_root().join("occ"))?;
    let mut responses = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let changed = s1_module_path(100 + u64::try_from(index)?)
            .to_string_lossy()
            .into_owned();
        let publication = occ_publication(candidate, RefSequence::ZERO, [&changed])?;
        let own_roots = candidate.semantic.roots.clone();
        let own_durability = candidate.semantic.durability.clone();
        let prior_roots = index
            .checked_sub(1)
            .map(|prior| candidates[prior].semantic.roots.clone());
        let outcome = occ.publish(
            "sm10",
            &publication,
            &locator_store,
            &ref_store,
            &mut NamedFaultInjector::default(),
            |_, head, _| {
                if prior_roots.as_ref() != Some(&head.roots) {
                    return Err(PocError::Integrity(
                        "SM-10 disjoint rebase observed an unexpected prior root".to_owned(),
                    ));
                }
                Ok(RebasedCanonical {
                    roots: own_roots.clone(),
                    durability: own_durability.clone(),
                })
            },
        )?;
        let OccPublishOutcome::Committed {
            receipt,
            rebase_count,
        } = outcome
        else {
            return Err(format!("SM-10 publisher {index} did not commit").into());
        };
        if receipt.value.sequence.get() != u64::try_from(index)? + 1
            || rebase_count != u32::from(index > 0)
        {
            return Err(format!(
                "SM-10 publisher {index} returned sequence {} with {rebase_count} rebases",
                receipt.value.sequence
            )
            .into());
        }
        responses.push(json!({
            "agent": index,
            "receipt": {
                "value": receipt.value,
                "idempotent_replay": receipt.idempotent_replay,
                "parent_directory_synced": receipt.parent_directory_synced,
                "outcome_path": receipt.outcome_path,
            },
            "rebase_count": rebase_count,
        }));
    }
    let selected = ref_store
        .read("sm10")?
        .ok_or("SM-10 branch head disappeared")?;
    let all_disjoint_paths_visible = (0..4_u8).all(|index| {
        candidates[3]
            .allocation
            .upper_dir
            .join(s1_module_path(100 + u64::from(index)))
            .is_file()
    });
    let owners = candidates
        .iter()
        .map(|candidate| current_owner(&candidate.allocation.allocation_root))
        .collect::<Result<Vec<_>, _>>()?;
    let latest_hash_start = timeline
        .iter()
        .map(|sample| sample.hash_started_ns)
        .max()
        .unwrap_or(u64::MAX);
    let earliest_hash_finish = timeline
        .iter()
        .map(|sample| sample.hash_finished_ns)
        .min()
        .unwrap_or(0);
    let elapsed = case_started.elapsed();
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "active_data_workers",
                snapshot.active_data_workers == 4,
                snapshot.active_data_workers,
                4,
            ),
            assertion(
                "fifth_owns_no_allocation",
                !fifth.receipt().owns_payload_allocation,
                fifth.receipt().owns_payload_allocation,
                false,
            ),
            assertion(
                "fifth_owns_no_mount",
                !fifth.receipt().owns_workspace_mount,
                fifth.receipt().owns_workspace_mount,
                false,
            ),
            assertion(
                "exactly_four_physical_allocations",
                after_allocations == before_allocations.saturating_add(4),
                format!("before={before_allocations} after={after_allocations}"),
                "after=before+4",
            ),
            assertion(
                "hashing_overlapped",
                latest_hash_start < earliest_hash_finish,
                format!("latest_start={latest_hash_start} earliest_finish={earliest_hash_finish}"),
                "latest_start < earliest_finish",
            ),
            assertion(
                "all_disjoint_results_visible",
                all_disjoint_paths_visible
                    && selected.roots == candidates[3].semantic.roots
                    && selected.sequence.get() == 4,
                format!(
                    "paths={all_disjoint_paths_visible} sequence={} roots={:?}",
                    selected.sequence, selected.roots
                ),
                "four paths at cumulative root sequence 4",
            ),
            assertion(
                "four_exact_payload_owners",
                owners
                    .iter()
                    .zip(candidates.iter())
                    .all(|(owner, candidate)| {
                        owner.owner_epoch == candidate.owner_epoch
                            && owner.subject
                                == OwnerSubject::PayloadOwned {
                                    publication_id: candidate.publication_id.clone(),
                                }
                    }),
                format!("{owners:?}"),
                "four exact PayloadOwned generations",
            ),
            assertion(
                "case_budget",
                elapsed < Duration::from_secs(10),
                format!("{elapsed:?}"),
                "<10s",
            ),
        ],
        details: json!({
            "duration_ns": ns(elapsed),
            "resource_snapshot": snapshot,
            "fifth_admission": fifth.receipt(),
            "timeline": timeline,
            "responses": responses,
            "selected": selected,
            "owners": owners,
            "allocation_ids": candidates
                .iter()
                .map(|candidate| candidate.allocation.descriptor.allocation_id.clone())
                .collect::<Vec<_>>(),
            "cumulative_fixture": {
                "agent_0": ["src/d0036/module-00000100.rs"],
                "agent_1": ["src/d0036/module-00000100.rs", "src/d0037/module-00000101.rs"],
                "agent_2": [
                    "src/d0036/module-00000100.rs",
                    "src/d0037/module-00000101.rs",
                    "src/d0038/module-00000102.rs"
                ],
                "agent_3": [
                    "src/d0036/module-00000100.rs",
                    "src/d0037/module-00000101.rs",
                    "src/d0038/module-00000102.rs",
                    "src/d0039/module-00000103.rs"
                ],
            },
        }),
    })
}

fn sm_11(context: &Context) -> CampaignResult<CaseExecution> {
    let started = Instant::now();
    let first = prepare_occ_candidate(
        context,
        "first",
        "mkdir -p branch && printf first > branch/a.txt",
    )?;
    let second = prepare_occ_candidate(
        context,
        "second",
        "mkdir -p branch && printf first > branch/a.txt && printf second > branch/b.txt",
    )?;
    let winner = prepare_occ_candidate(
        context,
        "winner",
        "mkdir -p branch && printf winner > branch/shared.txt",
    )?;
    let loser = prepare_occ_candidate(
        context,
        "loser",
        "mkdir -p branch && printf loser > branch/shared.txt",
    )?;

    let locator_store = LocatorStore::open(context.campaign_root().join("locators"))?;
    let ref_store = PairedRefStore::open(context.campaign_root().join("refs"))?;
    let occ = BranchOcc::open(context.campaign_root().join("occ"))?;
    let first_publication = occ_publication(&first, RefSequence::ZERO, ["branch/a.txt"])?;
    let first_outcome = occ.publish(
        "sm11",
        &first_publication,
        &locator_store,
        &ref_store,
        &mut NamedFaultInjector::default(),
        |_, _, _| {
            Err(PocError::Integrity(
                "first OCC publication rebased".to_owned(),
            ))
        },
    )?;
    let OccPublishOutcome::Committed {
        receipt: first_receipt,
        rebase_count: first_rebases,
    } = first_outcome
    else {
        return Err("SM-11 first disjoint publisher conflicted".into());
    };

    let second_publication = occ_publication(&second, RefSequence::ZERO, ["branch/b.txt"])?;
    let second_roots = second.semantic.roots.clone();
    let second_durability = second.semantic.durability.clone();
    let first_roots = first.semantic.roots.clone();
    let second_outcome = occ.publish(
        "sm11",
        &second_publication,
        &locator_store,
        &ref_store,
        &mut NamedFaultInjector::default(),
        |_, head, _| {
            if head.roots != first_roots {
                return Err(PocError::Integrity(
                    "disjoint OCC rebase observed an unexpected head".to_owned(),
                ));
            }
            Ok(RebasedCanonical {
                roots: second_roots.clone(),
                durability: second_durability.clone(),
            })
        },
    )?;
    let OccPublishOutcome::Committed {
        receipt: second_receipt,
        rebase_count: second_rebases,
    } = second_outcome
    else {
        return Err("SM-11 second disjoint publisher conflicted".into());
    };

    let conflict_parent = second_receipt.value.sequence;
    let winner_publication = occ_publication(&winner, conflict_parent, ["branch/shared.txt"])?;
    let winner_outcome = occ.publish(
        "sm11",
        &winner_publication,
        &locator_store,
        &ref_store,
        &mut NamedFaultInjector::default(),
        |_, _, _| {
            Err(PocError::Integrity(
                "overlap winner unexpectedly rebased".to_owned(),
            ))
        },
    )?;
    let OccPublishOutcome::Committed {
        receipt: winner_receipt,
        rebase_count: winner_rebases,
    } = winner_outcome
    else {
        return Err("SM-11 overlap winner conflicted before its competitor".into());
    };

    let loser_publication = occ_publication(&loser, conflict_parent, ["branch/shared.txt"])?;
    let loser_outcome = occ.publish(
        "sm11",
        &loser_publication,
        &locator_store,
        &ref_store,
        &mut NamedFaultInjector::default(),
        |_, _, _| {
            Err(PocError::Integrity(
                "overlap loser unexpectedly invoked rebase".to_owned(),
            ))
        },
    )?;
    let OccPublishOutcome::Conflict(conflict) = loser_outcome else {
        return Err("SM-11 overlap loser unexpectedly committed".into());
    };
    let replay = occ.publish(
        "sm11",
        &loser_publication,
        &locator_store,
        &ref_store,
        &mut NamedFaultInjector::default(),
        |_, _, _| {
            Err(PocError::Integrity(
                "retained conflict replay invoked rebase".to_owned(),
            ))
        },
    )?;
    let current = ref_store
        .read("sm11")?
        .ok_or("SM-11 branch head disappeared")?;
    let retained_owner = current_owner(&loser.allocation.allocation_root)?;
    let elapsed = started.elapsed();
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "case_budget",
                elapsed < Duration::from_secs(5),
                format!("{elapsed:?}"),
                "<5s",
            ),
            assertion(
                "first_disjoint_committed",
                first_receipt.value.sequence.get() == 1 && first_rebases == 0,
                format!(
                    "sequence={} rebases={first_rebases}",
                    first_receipt.value.sequence
                ),
                "sequence=1 rebases=0",
            ),
            assertion(
                "second_disjoint_rebased",
                second_receipt.value.sequence.get() == 2 && second_rebases == 1,
                format!(
                    "sequence={} rebases={second_rebases}",
                    second_receipt.value.sequence
                ),
                "sequence=2 rebases=1",
            ),
            assertion(
                "overlap_winner_committed",
                winner_receipt.value.sequence.get() == 3 && winner_rebases == 0,
                format!(
                    "sequence={} rebases={winner_rebases}",
                    winner_receipt.value.sequence
                ),
                "sequence=3 rebases=0",
            ),
            assertion(
                "typed_overlap_conflict",
                conflict.overlaps.len() == 1
                    && conflict.overlaps[0].incoming == "branch/shared.txt"
                    && conflict.overlaps[0].committed == "branch/shared.txt",
                format!("{:?}", conflict.overlaps),
                "one exact overlap",
            ),
            assertion(
                "retained_conflict_replay",
                replay == OccPublishOutcome::Conflict(conflict.clone()),
                format!("{replay:?}"),
                "same retained conflict",
            ),
            assertion(
                "loser_remains_payload_owned",
                retained_owner.owner_epoch == loser.owner_epoch
                    && retained_owner.subject
                        == OwnerSubject::PayloadOwned {
                            publication_id: loser.publication_id.clone(),
                        },
                format!("{retained_owner:?}"),
                "exact loser PayloadOwned generation",
            ),
            assertion(
                "winner_remains_selected",
                current.roots == winner.semantic.roots
                    && current.sequence == winner_receipt.value.sequence,
                format!("{current:?}"),
                "winner roots at sequence 3",
            ),
        ],
        details: json!({
            "duration_ns": ns(elapsed),
            "first": {
                "value": first_receipt.value,
                "idempotent_replay": first_receipt.idempotent_replay,
                "parent_directory_synced": first_receipt.parent_directory_synced,
                "outcome_path": first_receipt.outcome_path,
            },
            "second": {
                "value": second_receipt.value,
                "idempotent_replay": second_receipt.idempotent_replay,
                "parent_directory_synced": second_receipt.parent_directory_synced,
                "outcome_path": second_receipt.outcome_path,
            },
            "winner": {
                "value": winner_receipt.value,
                "idempotent_replay": winner_receipt.idempotent_replay,
                "parent_directory_synced": winner_receipt.parent_directory_synced,
                "outcome_path": winner_receipt.outcome_path,
            },
            "conflict": conflict,
            "retained_owner": retained_owner,
            "selected_head": current,
            "publisher_allocations": [
                first.allocation.descriptor.allocation_id,
                second.allocation.descriptor.allocation_id,
                winner.allocation.descriptor.allocation_id,
                loser.allocation.descriptor.allocation_id,
            ],
            "second_rebase_fixture": {
                "candidate_contains_prior_a": true,
                "candidate_adds_b": true,
                "canonical_root": second.semantic.roots,
            },
        }),
    })
}

fn sm_12(context: &Context) -> CampaignResult<CaseExecution> {
    let started = Instant::now();
    let case_root = context.campaign_root().join("sm12");
    fs::create_dir_all(&case_root)?;

    let mut edges = Vec::new();
    edges.push(run_sm12_owner_edge(
        context,
        &case_root,
        "lease-revoke",
        ".fault-after-lease-fence",
        false,
    )?);
    edges.push(run_sm12_owner_edge(
        context,
        &case_root,
        "owner-journal-append",
        ".fault-before-owner-selector-replace",
        true,
    )?);
    edges.push(run_sm12_publication_edge(
        context,
        &case_root,
        "locator-select",
        NamedFaultPoint::LocatorAfterSelectorRename,
    )?);
    edges.push(run_sm12_publication_edge(
        context,
        &case_root,
        "ref-fsync",
        NamedFaultPoint::RefAfterParentFsync,
    )?);

    let elapsed = started.elapsed();
    let all_killed = edges
        .iter()
        .all(|edge| edge["signal"].as_i64() == Some(i64::from(libc::SIGKILL)));
    let all_replayed = edges
        .iter()
        .all(|edge| edge["same_operation_replay"].as_bool() == Some(true));
    let all_exact_owner = edges
        .iter()
        .all(|edge| edge["exact_owner"].as_bool() == Some(true));
    let all_old_or_new = edges
        .iter()
        .all(|edge| edge["old_or_complete_new"].as_bool() == Some(true));
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "four_sigkill_edges",
                all_killed && edges.len() == 4,
                format!("edges={} all_sigkill={all_killed}", edges.len()),
                "4 exact SIGKILL exits",
            ),
            assertion("same_operation_replay", all_replayed, all_replayed, true),
            assertion("exact_owner_epoch", all_exact_owner, all_exact_owner, true),
            assertion(
                "old_or_complete_new_root",
                all_old_or_new,
                all_old_or_new,
                true,
            ),
            assertion(
                "case_budget",
                elapsed < Duration::from_secs(30),
                format!("{elapsed:?}"),
                "<30s",
            ),
        ],
        details: json!({
            "duration_ns": ns(elapsed),
            "edges": edges,
        }),
    })
}

fn run_sm12_owner_edge(
    context: &Context,
    case_root: &Path,
    edge: &str,
    marker_name: &str,
    expected_post_kill_payload: bool,
) -> CampaignResult<Value> {
    let operation_id = OperationId::from_string(format!("{}-sm12-{edge}", context.run_id));
    let publication_id = PublicationId::from_string(format!("{}-sm12-{edge}", context.run_id));
    let allocation = create_allocation(&context.arena_root(), &operation_id)?;
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let payload_path = allocation.upper_dir.join("sm12.txt");
    let mut payload = File::create(&payload_path)?;
    payload.write_all(edge.as_bytes())?;
    payload.sync_all()?;
    drop(payload);
    let (before, after) = capture_stable_pair(&allocation)?;
    let stable = StableAllocationReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        allocation: allocation.descriptor.clone(),
        expected_owner_epoch: lease.owner_epoch,
        before: before.physical,
        after: after.physical,
        sync_completed: true,
    };
    let request = OwnerTransitionRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        session_id: lease.session_id.clone(),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        expected_lease_epoch: lease.lease_epoch,
        expected_owner_epoch: lease.owner_epoch,
    };
    let edge_root = case_root.join(edge);
    fs::create_dir_all(&edge_root)?;
    let child_request_path = edge_root.join("child-request.json");
    let child_witness_path = edge_root.join("child-witness.json");
    durable::replace_json(
        &child_request_path,
        &Sm12ChildRequest::Owner {
            edge: edge.to_owned(),
            allocation_root: allocation.allocation_root.clone(),
            stable: stable.clone(),
            request: request.clone(),
        },
    )?;
    let marker_path = allocation.owner_dir.join(marker_name);
    let mut marker = File::create(&marker_path)?;
    marker.write_all(edge.as_bytes())?;
    marker.sync_all()?;
    drop(marker);

    let status = spawn_sm12_child(&child_request_path, &child_witness_path)?;
    fs::remove_file(&marker_path)?;
    let witness: Sm12ChildWitness = durable::read_json(&child_witness_path)?;
    let stale_immediately_after_kill = validate_writer(&allocation.allocation_root, &lease.writer)
        .is_err()
        && validate_deleter(&allocation.allocation_root, &lease.deleter).is_err();
    let post_kill_owner = current_owner(&allocation.allocation_root)?;
    let post_kill_subject_matches = if expected_post_kill_payload {
        post_kill_owner.subject
            == OwnerSubject::PayloadOwned {
                publication_id: publication_id.clone(),
            }
    } else {
        matches!(
            post_kill_owner.subject,
            OwnerSubject::WorkspaceOwned {
                ref session_id,
                lease_epoch,
            } if session_id == &lease.session_id && lease_epoch == lease.lease_epoch
        )
    };
    let first = compare_and_adopt(&allocation.allocation_root, &stable, &request)?;
    let replay = compare_and_adopt(&allocation.allocation_root, &stable, &request)?;
    let owner = current_owner(&allocation.allocation_root)?;
    let exact_owner = owner.owner_epoch == lease.owner_epoch + 1
        && owner.operation_id == operation_id
        && owner.subject
            == OwnerSubject::PayloadOwned {
                publication_id: publication_id.clone(),
            };
    let same_operation_replay = first.new_owner == replay.new_owner
        && first.operation_id == replay.operation_id
        && replay.idempotent_replay;
    Ok(json!({
        "edge": edge,
        "signal": status.signal(),
        "witness": witness,
        "post_kill_owner": post_kill_owner,
        "post_kill_subject_matches": post_kill_subject_matches,
        "stale_immediately_after_kill": stale_immediately_after_kill,
        "first_recovery": first,
        "replay": replay,
        "final_owner": owner,
        "same_operation_replay": same_operation_replay,
        "exact_owner": exact_owner,
        "old_or_complete_new": post_kill_subject_matches,
        "allocation_path": allocation.allocation_root,
    }))
}

fn run_sm12_publication_edge(
    context: &Context,
    case_root: &Path,
    edge: &str,
    fault: NamedFaultPoint,
) -> CampaignResult<Value> {
    let candidate = prepare_sm12_recovery_candidate(context, case_root, edge, None)?;
    let edge_root = case_root.join(edge);
    let child_request_path = edge_root.join("child-request.json");
    let child_witness_path = edge_root.join("child-witness.json");
    durable::replace_json(
        &child_request_path,
        &Sm12ChildRequest::Publication {
            edge: edge.to_owned(),
            recovery_root: candidate.recovery_root.clone(),
            locator_root: candidate.locator_root.clone(),
            ref_root: candidate.ref_root.clone(),
            occ_root: candidate.occ_root.clone(),
            operation_id: candidate.operation_id.clone(),
            fault,
        },
    )?;
    let before = sandbox_runtime_mpla_poc::inventory::capture_inventory(&candidate.allocation)?;
    let status = spawn_sm12_child(&child_request_path, &child_witness_path)?;
    let witness: Sm12ChildWitness = durable::read_json(&child_witness_path)?;

    let ref_store = PairedRefStore::open(&candidate.ref_root)?;
    let post_kill_ref = ref_store.read(&candidate.branch)?;
    let old_or_complete_new = post_kill_ref.as_ref().is_none_or(|selected| {
        selected.operation_id == candidate.operation_id
            && selected.publication_id == candidate.publication_id
            && selected.roots == candidate.semantic.roots
    });
    let recovery = PublicationRecovery::open(&candidate.recovery_root)?;
    let locator_store = LocatorStore::open(&candidate.locator_root)?;
    let occ = BranchOcc::open(&candidate.occ_root)?;
    let first = recovery.replay(
        &candidate.operation_id,
        &locator_store,
        &ref_store,
        &occ,
        &mut NamedFaultInjector::default(),
        |_, _, _| {
            Err(PocError::Integrity(
                "SM-12 unique branch unexpectedly requested rebase".to_owned(),
            ))
        },
    )?;
    let first_receipt = match first {
        RecoveryOutcome::Committed(receipt) => receipt,
        other => return Err(format!("SM-12 {edge} recovery was not committed: {other:?}").into()),
    };
    let replay = recovery.replay(
        &candidate.operation_id,
        &locator_store,
        &ref_store,
        &occ,
        &mut NamedFaultInjector::default(),
        |_, _, _| {
            Err(PocError::Integrity(
                "SM-12 replay unexpectedly requested rebase".to_owned(),
            ))
        },
    )?;
    let replay_receipt = match replay {
        RecoveryOutcome::Committed(receipt) => receipt,
        other => return Err(format!("SM-12 {edge} retry was not committed: {other:?}").into()),
    };
    let selected = ref_store
        .read(&candidate.branch)?
        .ok_or_else(|| format!("SM-12 {edge} ref is absent after recovery"))?;
    let owner = current_owner(&candidate.allocation.allocation_root)?;
    let after = sandbox_runtime_mpla_poc::inventory::capture_inventory(&candidate.allocation)?;
    let exact_owner = owner.owner_epoch == candidate.owner_epoch
        && owner.operation_id == candidate.operation_id
        && owner.subject
            == OwnerSubject::PayloadOwned {
                publication_id: candidate.publication_id.clone(),
            };
    let same_operation_replay = replay_receipt.idempotent_replay
        && first_receipt.value == replay_receipt.value
        && selected == replay_receipt.value;
    let snapshot = recovery.inspect(&candidate.operation_id)?;
    Ok(json!({
        "edge": edge,
        "fault": fault,
        "signal": status.signal(),
        "witness": witness,
        "post_kill_ref": post_kill_ref,
        "first_recovery": {
            "value": first_receipt.value,
            "idempotent_replay": first_receipt.idempotent_replay,
            "parent_directory_synced": first_receipt.parent_directory_synced,
            "outcome_path": first_receipt.outcome_path,
        },
        "replay": {
            "value": replay_receipt.value,
            "idempotent_replay": replay_receipt.idempotent_replay,
            "parent_directory_synced": replay_receipt.parent_directory_synced,
            "outcome_path": replay_receipt.outcome_path,
        },
        "selected_ref": selected,
        "recovery_snapshot": snapshot,
        "final_owner": owner,
        "same_operation_replay": same_operation_replay,
        "exact_owner": exact_owner,
        "old_or_complete_new": old_or_complete_new,
        "allocation_unchanged": before == after,
        "allocation_path": candidate.allocation.allocation_root,
        "accounted_bytes": candidate.accounted_bytes,
    }))
}

fn prepare_sm12_recovery_candidate(
    context: &Context,
    case_root: &Path,
    edge: &str,
    semantic_override: Option<SemanticBuildReceipt>,
) -> CampaignResult<Sm12RecoveryCandidate> {
    let operation_id = OperationId::from_string(format!("{}-sm12-{edge}", context.run_id));
    let publication_id = PublicationId::from_string(format!("{}-sm12-{edge}", context.run_id));
    let allocation = create_allocation(&context.arena_root(), &operation_id)?;
    let fixture_logical_bytes = install_hv07_fixture(context, &allocation)?;
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let (owner_epoch, accounted_bytes) = if fixture_logical_bytes > 0 {
        prepare_hv07_unmounted_candidate(
            &allocation,
            &lease,
            &operation_id,
            &publication_id,
            case_root,
            fixture_logical_bytes,
        )?
    } else {
        let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
            &context.control_root,
            allocation.clone(),
            lease.clone(),
            context.raw_session_lower_dirs(),
            context.cgroup_procs_path.clone(),
        )?;
        let command = session.execute(
            &lease.writer,
            Path::new("/bin/sh"),
            &["-c".to_owned(), format!("printf '{edge}' > sm12.txt")],
            Duration::from_secs(2),
        )?;
        if !command.success {
            return Err(format!("SM-12 {edge} payload preparation failed").into());
        }
        let stationary = stationary_adopt(
            &mut session,
            &StationaryPublicationRequest {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                publication_id: publication_id.clone(),
            },
            &context.control_root.join("operations"),
            &mut FaultInjector::default(),
        )?;
        (
            stationary.adoption.new_owner.owner_epoch,
            stationary.stable.after.allocated_bytes.max(1),
        )
    };
    let (semantic, semantic_reused) = match semantic_override {
        Some(mut semantic) => {
            if fixture_logical_bytes != 128 * 1024 * 1024 {
                return Err(
                    "shared HV-07 semantic receipt requires the exact 128 MiB fixture".into(),
                );
            }
            semantic.operation_id = operation_id.clone();
            (semantic, true)
        }
        None => (
            full_build(
                &context.campaign_root(),
                &context.preparation.canonical_object_dir,
                &allocation,
                &format!("sm12-{edge}"),
            )?
            .receipt,
            false,
        ),
    };
    let edge_root = case_root.join(edge);
    let recovery_root = edge_root.join("recovery");
    let locator_root = edge_root.join("locators");
    let ref_root = edge_root.join("refs");
    let occ_root = edge_root.join("occ");
    let branch = format!("sm12-{edge}");
    let payload_root = PayloadRootId::parse(semantic.roots.root_id.as_str())?;
    let locator_delta = LocatorDelta {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        expected_parent: None,
        forward: vec![ForwardLocatorEntry {
            payload_root: payload_root.clone(),
            allocation_id: allocation.descriptor.allocation_id.clone(),
            owner_epoch,
            extents: vec![LocatorExtent {
                relative_path: "upper".to_owned(),
                offset: 0,
                length: accounted_bytes,
            }],
        }],
        reverse: vec![ReverseLocatorEntry {
            allocation_id: allocation.descriptor.allocation_id.clone(),
            owner_epoch,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
            payload_roots: vec![payload_root],
            accounted_bytes,
        }],
    };
    let recovery = PublicationRecovery::open(&recovery_root)?;
    recovery.prepare(&RecoveryRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        branch: branch.clone(),
        allocation_root: allocation.allocation_root.clone(),
        allocation_identity: capture_recovery_allocation_identity(
            &allocation.allocation_root,
            &allocation.descriptor.allocation_id,
        )?,
        allocation_id: allocation.descriptor.allocation_id.clone(),
        owner_epoch,
        accounted_bytes,
        locator_delta,
        candidate: LocatorRefCandidate {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
            roots: semantic.roots.clone(),
            locator_generation: sandbox_runtime_mpla_poc::LocatorGeneration::INITIAL,
            expected_sequence: RefSequence::ZERO,
        },
        canonical: semantic.durability.clone(),
        changed_paths: ChangedPathSet::new(["sm12.txt".to_owned()])?,
    })?;
    Ok(Sm12RecoveryCandidate {
        allocation,
        operation_id,
        publication_id,
        owner_epoch,
        accounted_bytes,
        fixture_logical_bytes,
        semantic,
        semantic_reused,
        recovery_root,
        locator_root,
        ref_root,
        occ_root,
        branch,
    })
}

fn prepare_hv07_unmounted_candidate(
    allocation: &AllocationHandle,
    lease: &sandbox_runtime_mpla_poc::MutableLease,
    operation_id: &OperationId,
    publication_id: &PublicationId,
    case_root: &Path,
    fixture_logical_bytes: u64,
) -> CampaignResult<(u64, u64)> {
    let payload_path = allocation.upper_dir.join("sm12.txt");
    let mut payload = File::create(&payload_path)?;
    payload.write_all(b"hv07-qualified-stable-payload-v1")?;
    payload.sync_all()?;
    File::open(&allocation.upper_dir)?.sync_all()?;
    File::open(&allocation.owner_dir)?.sync_all()?;
    let (before, after) = capture_stable_pair(allocation)?;
    let stable = StableAllocationReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        allocation: allocation.descriptor.clone(),
        expected_owner_epoch: lease.owner_epoch,
        before: before.physical,
        after: after.physical,
        sync_completed: true,
    };
    let adoption = compare_and_adopt(
        &allocation.allocation_root,
        &stable,
        &OwnerTransitionRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
            session_id: lease.session_id.clone(),
            allocation_id: allocation.descriptor.allocation_id.clone(),
            expected_lease_epoch: lease.lease_epoch,
            expected_owner_epoch: lease.owner_epoch,
        },
    )?;
    durable::replace_json(
        &case_root.join("hv07-unmounted-candidate.json"),
        &json!({
            "schema_version": SCHEMA_VERSION,
            "setup": "unmounted-stable-allocation",
            "ordinary_workload_mount_authority": false,
            "fixture_logical_bytes": fixture_logical_bytes,
            "semantic_payload_key": "hv07-qualified-stable-payload-v1",
            "stable": stable,
            "adoption": adoption,
        }),
    )?;
    Ok((
        adoption.new_owner.owner_epoch,
        stable.after.allocated_bytes.max(1),
    ))
}

fn install_hv07_fixture(context: &Context, allocation: &AllocationHandle) -> CampaignResult<u64> {
    let requested = match std::env::var("MPLA_POC_HV07_FIXTURE_BYTES") {
        Ok(value) => value.parse::<u64>()?,
        Err(std::env::VarError::NotPresent) => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    const HV07_FIXTURE_BYTES: u64 = 128 * 1024 * 1024;
    if requested != HV07_FIXTURE_BYTES {
        return Err(format!(
            "HV-07 requires exactly {HV07_FIXTURE_BYTES} fixture bytes, got {requested}"
        )
        .into());
    }
    let fixture = context
        .preparation
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture_id == FixtureId::S2Large)
        .ok_or("prepared S2-large fixture is missing")?;
    let source_allocation = open_allocation(&context.arena_root(), &fixture.allocation_id)?;
    let mut logical_bytes = 0_u64;
    for source_name in ["large-0.bin", "large-1.bin"] {
        let source = source_allocation.upper_dir.join(source_name);
        let target = allocation.upper_dir.join(source_name);
        fs::hard_link(&source, &target)?;
        logical_bytes = logical_bytes
            .checked_add(fs::metadata(&target)?.len())
            .ok_or("HV-07 fixture byte count overflow")?;
    }
    if logical_bytes != HV07_FIXTURE_BYTES {
        return Err(format!(
            "HV-07 fixture logical bytes mismatch: expected {HV07_FIXTURE_BYTES}, got {logical_bytes}"
        )
        .into());
    }
    File::open(&allocation.upper_dir)?.sync_all()?;
    Ok(logical_bytes)
}

fn spawn_sm12_child(
    request_path: &Path,
    witness_path: &Path,
) -> CampaignResult<std::process::ExitStatus> {
    let status = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("m1_sm12_child_fault_then_sigkill")
        .arg("--ignored")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("MPLA_SM12_CHILD_REQUEST", request_path)
        .env("MPLA_SM12_CHILD_WITNESS", witness_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.signal() != Some(libc::SIGKILL) {
        return Err(format!("SM-12 child did not exit through SIGKILL: status={status}").into());
    }
    if !witness_path.exists() {
        return Err(format!(
            "SM-12 child did not durably write witness {}",
            witness_path.display()
        )
        .into());
    }
    Ok(status)
}

pub fn sm12_child_fault_then_sigkill() -> CampaignResult {
    let request_path = required_path("MPLA_SM12_CHILD_REQUEST")?;
    let witness_path = required_path("MPLA_SM12_CHILD_WITNESS")?;
    let request: Sm12ChildRequest = durable::read_json(&request_path)?;
    let (edge, observed_error, fault_fired) = match request {
        Sm12ChildRequest::Owner {
            edge,
            allocation_root,
            stable,
            request,
        } => {
            let error = compare_and_adopt(&allocation_root, &stable, &request)
                .expect_err("SM-12 owner fault did not interrupt adoption");
            let fired = matches!(error, PocError::RecoveryRequired(_));
            (edge, error.to_string(), fired)
        }
        Sm12ChildRequest::Publication {
            edge,
            recovery_root,
            locator_root,
            ref_root,
            occ_root,
            operation_id,
            fault,
        } => {
            let recovery = PublicationRecovery::open(recovery_root)?;
            let locator_store = LocatorStore::open(locator_root)?;
            let ref_store = PairedRefStore::open(ref_root)?;
            let occ = BranchOcc::open(occ_root)?;
            let mut faults = NamedFaultInjector::armed([(fault, 1)]);
            let error = recovery
                .replay(
                    &operation_id,
                    &locator_store,
                    &ref_store,
                    &occ,
                    &mut faults,
                    |_, _, _| {
                        Err(PocError::Integrity(
                            "SM-12 child unexpectedly requested rebase".to_owned(),
                        ))
                    },
                )
                .expect_err("SM-12 named fault did not interrupt publication");
            let fired = faults.fired(fault, 1);
            (edge, error.to_string(), fired)
        }
    };
    if !fault_fired {
        return Err(format!("SM-12 {edge} fault did not fire").into());
    }
    durable::replace_json(
        &witness_path,
        &Sm12ChildWitness {
            schema_version: SCHEMA_VERSION,
            edge,
            pid: std::process::id(),
            observed_error,
            fault_fired,
            written_unix_ms: sandbox_runtime_mpla_poc::unix_time_ms()?,
        },
    )?;
    sigkill_self()
}

fn sigkill_self() -> ! {
    let pid = i32::try_from(std::process::id()).expect("process ID fits in pid_t");
    // SAFETY: SIGKILL is sent only to the current disposable SM-12 child process.
    let result = unsafe { libc::kill(pid, libc::SIGKILL) };
    assert_eq!(result, 0, "SIGKILL delivery failed");
    loop {
        std::hint::spin_loop();
    }
}

fn sm_13(context: &Context) -> CampaignResult<CaseExecution> {
    let started = Instant::now();
    let roots = [
        ("payload", &context.payload_root),
        ("control", &context.control_root),
        ("fixtures", &context.fixtures_root),
        ("evidence", &context.evidence_root),
    ];
    let scope = roots[0]
        .1
        .parent()
        .ok_or("payload root has no reconciliation parent")?
        .to_path_buf();
    let categories = roots
        .iter()
        .map(|(category, root)| {
            let relative = root
                .strip_prefix(&scope)
                .map_err(|_| "smoke storage root escapes reconciliation scope")?;
            let volume = relative
                .components()
                .next()
                .ok_or("smoke storage category has no volume component")?;
            Ok(sandbox_runtime_mpla_poc::StorageCategoryRoot {
                category: (*category).to_owned(),
                root: scope.join(volume.as_os_str()),
                recursive: true,
            })
        })
        .collect::<CampaignResult<Vec<_>>>()?
        .into_iter()
        .chain(std::iter::once(
            sandbox_runtime_mpla_poc::StorageCategoryRoot {
                category: "scope-root".to_owned(),
                root: scope.clone(),
                recursive: false,
            },
        ))
        .collect::<Vec<_>>();
    let leaks = sm13_leak_counts(context)?;
    let receipt = sandbox_runtime_mpla_poc::reconcile::reconcile(&scope, &categories, leaks)?;
    let elapsed = started.elapsed();
    durable::replace_json(
        &context
            .evidence_root
            .join("cases/SM-13/reconciliation.json"),
        &receipt,
    )?;
    Ok(CaseExecution {
        assertions: vec![
            assertion("balanced", receipt.balanced, receipt.balanced, true),
            assertion(
                "unexplained_bytes",
                receipt.unexplained_allocated_bytes == 0,
                receipt.unexplained_allocated_bytes,
                0,
            ),
            assertion(
                "unexplained_inodes",
                receipt.unexplained_inodes == 0,
                receipt.unexplained_inodes,
                0,
            ),
            assertion(
                "no_live_resource_leaks",
                receipt.leaks == sandbox_runtime_mpla_poc::LeakCounts::default(),
                format!("{:?}", receipt.leaks),
                "all zero",
            ),
            assertion(
                "case_budget",
                elapsed < Duration::from_secs(5),
                format!("{elapsed:?}"),
                "<5s",
            ),
        ],
        details: json!({
            "duration_ns": ns(elapsed),
            "receipt": receipt,
        }),
    })
}

fn sm13_leak_counts(context: &Context) -> CampaignResult<sandbox_runtime_mpla_poc::LeakCounts> {
    let active_leases = allocation_roots(&context.arena_root())?
        .iter()
        .filter_map(|allocation_root| {
            let lease = allocation_root.join("owner/LEASE");
            lease
                .exists()
                .then(|| durable::read_json::<Value>(&lease).ok())
                .flatten()
        })
        .filter(|lease| lease["active"].as_bool() == Some(true))
        .count();
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    let active_mounts = mountinfo
        .lines()
        .filter(|line| {
            line.split_ascii_whitespace().any(|field| {
                let path = Path::new(field);
                path.starts_with(context.control_root.join("sessions"))
                    || path.starts_with(context.control_root.join("activations"))
            })
        })
        .count();
    let writable_payload_fds = fs::read_dir("/proc/self/fd")?
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter(|target| target.starts_with(&context.payload_root))
        .count();
    Ok(sandbox_runtime_mpla_poc::LeakCounts {
        active_leases: u64::try_from(active_leases)?,
        active_mounts: u64::try_from(active_mounts)?,
        writable_payload_fds: u64::try_from(writable_payload_fds)?,
        locator_readers: 0,
        retirement_debt_objects: 0,
    })
}

fn sm_14(context: &Context) -> CampaignResult<CaseExecution> {
    let case_started = Instant::now();
    let catalog_binding: CatalogBinding = durable::read_json(&context.catalog_binding_path)?;
    let operation_id = OperationId::from_string(format!("{}-sm14-publication", context.run_id));
    let publication_id = PublicationId::from_string(format!("{}-sm14", context.run_id));
    let allocation = create_allocation(&context.arena_root(), &operation_id)?;
    let created_allocation_id = allocation.descriptor.allocation_id.clone();
    let fixture = populate_empty_fixture_root(
        &allocation.upper_dir,
        FixtureId::S5Semantics,
        FixtureTier::Smoke,
    )?;
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &context.control_root,
        allocation.clone(),
        lease.clone(),
        context.raw_session_lower_dirs(),
        context.cgroup_procs_path.clone(),
    )?;
    let command = session.execute(
        &lease.writer,
        Path::new("/bin/sh"),
        &[
            "-c".to_owned(),
            "printf lifecycle > lifecycle-sm14.txt".to_owned(),
        ],
        Duration::from_secs(2),
    )?;
    let publish_started = Instant::now();
    let stationary = stationary_adopt(
        &mut session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
        },
        &context.control_root.join("operations"),
        &mut FaultInjector::default(),
    )?;
    let semantic = full_build(
        &context.campaign_root(),
        &context.preparation.canonical_object_dir,
        &allocation,
        "sm14-semantic",
    )?;
    let selected_ref = install_ref(
        context,
        &allocation,
        &semantic.receipt,
        stationary.adoption.new_owner.owner_epoch,
        stationary.stable.after.allocated_bytes,
        &operation_id,
        &publication_id,
    )?;
    let publish_elapsed = publish_started.elapsed();
    drop(session);

    let lifecycle_root = context.campaign_root().join("sm14-lifecycle");
    let before_metadata_allocations = allocation_directory_count(&context.payload_root)?;
    let initialize = invoke_lifecycle_cli(
        context,
        lifecycle_metadata_args(
            &lifecycle_root,
            &format!("{}-sm14-initialize", context.run_id),
            "initialize",
            BRANCH,
            None,
            None,
            Some(allocation.descriptor.allocation_id.as_str()),
            Some(semantic.receipt.roots.root_id.as_str()),
            Some(semantic.receipt.roots.attribution_root_id.as_str()),
            false,
        ),
    )?;

    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: semantic.receipt.roots.clone(),
        base_allocation_id: allocation.descriptor.allocation_id.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: Vec::new(),
    };
    let mut forks = Vec::new();
    let mut fork_activation_ns = Vec::new();
    let mut fork_activation_phase_spans = Vec::new();
    let mut first_command_ns = Vec::new();
    let mut first_commands = Vec::new();
    let mut fresh_activation_ids = Vec::new();
    for sample in 0..3_u8 {
        let branch = format!("fork-{sample}");
        let fork = invoke_lifecycle_cli(
            context,
            lifecycle_metadata_args(
                &lifecycle_root,
                &format!("{}-sm14-fork-{sample}", context.run_id),
                "fork",
                &branch,
                Some(BRANCH),
                None,
                None,
                None,
                None,
                false,
            ),
        )?;
        if fork.response["selection"]["root_id"].as_str()
            != Some(semantic.receipt.roots.root_id.as_str())
            || fork.response["selection"]["attribution_root_id"].as_str()
                != Some(semantic.receipt.roots.attribution_root_id.as_str())
        {
            return Err(format!("SM-14 fork {sample} selected different canonical roots").into());
        }
        forks.push(fork);
    }
    let after_inactive_forks = allocation_directory_count(&context.payload_root)?;

    for sample in 0..3_u8 {
        let mut activated = activate_exact(ExactActivationRequest {
            activation_operation_id: ActivationOperationId::from_string(format!(
                "{}-sm14-fork-ready-{sample}",
                context.run_id
            )),
            allocation_operation_id: OperationId::from_string(format!(
                "{}-sm14-fresh-{sample}",
                context.run_id
            )),
            selected_ref: selected_ref.clone(),
            recipe: recipe.clone(),
            payload_allocations: vec![allocation.clone()],
            arena_root: context.arena_root(),
            control_root: context.control_root.clone(),
            cgroup_procs_path: context.cgroup_procs_path.clone(),
            readiness_path: PathBuf::from("tree/d0000/node-00000000.bin"),
            readiness_contains: None,
            readiness_timeout: Duration::from_secs(2),
        })?;
        fork_activation_ns.push(activated.receipt.elapsed_ns);
        fork_activation_phase_spans.push(activated.receipt.phase_spans.clone());
        let writer = activated.session.mutable_lease().writer.clone();
        let first_started = Instant::now();
        let first = activated.session.execute(
            &writer,
            Path::new("/bin/sh"),
            &["-c".to_owned(), "test -f lifecycle-sm14.txt".to_owned()],
            Duration::from_secs(2),
        )?;
        first_command_ns.push(ns(first_started.elapsed()));
        first_commands.push(first);
        let fresh_id = activated
            .session
            .allocation()
            .descriptor
            .allocation_id
            .clone();
        let deleter = activated.session.mutable_lease().deleter.clone();
        drop(activated);
        destroy_workspace_allocation(&context.arena_root(), &fresh_id, &deleter)?;
        fresh_activation_ids.push(fresh_id);
    }

    let mut rollbacks = Vec::new();
    for sample in 0..3_u8 {
        rollbacks.push(invoke_lifecycle_cli(
            context,
            lifecycle_metadata_args(
                &lifecycle_root,
                &format!("{}-sm14-rollback-{sample}", context.run_id),
                "rollback",
                BRANCH,
                None,
                Some("fork-0"),
                None,
                None,
                None,
                false,
            ),
        )?);
    }
    let mut squashes = Vec::new();
    for sample in 0..3_u8 {
        squashes.push(invoke_lifecycle_cli(
            context,
            lifecycle_metadata_args(
                &lifecycle_root,
                &format!("{}-sm14-squash-{sample}", context.run_id),
                "squash",
                BRANCH,
                None,
                None,
                None,
                None,
                None,
                false,
            ),
        )?);
    }
    let failed = invoke_lifecycle_cli(
        context,
        lifecycle_metadata_args(
            &lifecycle_root,
            &format!("{}-sm14-failed", context.run_id),
            "fork",
            "failed-branch",
            Some("missing-branch"),
            None,
            None,
            None,
            None,
            false,
        ),
    )?;
    let cancelled = invoke_lifecycle_cli(
        context,
        lifecycle_metadata_args(
            &lifecycle_root,
            &format!("{}-sm14-cancelled", context.run_id),
            "rollback",
            BRANCH,
            None,
            Some("fork-0"),
            None,
            None,
            None,
            true,
        ),
    )?;
    let after_metadata_allocations = allocation_directory_count(&context.payload_root)?;
    let active_lease: Value = durable::read_json(&allocation.owner_dir.join("LEASE"))?;
    let elapsed = case_started.elapsed();
    let fork_outer = forks
        .iter()
        .map(|invocation| invocation.outer_elapsed_ns)
        .collect::<Vec<_>>();
    let rollback_outer = rollbacks
        .iter()
        .map(|invocation| invocation.outer_elapsed_ns)
        .collect::<Vec<_>>();
    let rollback_service = rollbacks
        .iter()
        .map(|invocation| {
            invocation.response["service_elapsed_ns"]
                .as_u64()
                .unwrap_or(u64::MAX)
        })
        .collect::<Vec<_>>();
    let squash_outer = squashes
        .iter()
        .map(|invocation| invocation.outer_elapsed_ns)
        .collect::<Vec<_>>();
    let squash_service = squashes
        .iter()
        .map(|invocation| {
            invocation.response["service_elapsed_ns"]
                .as_u64()
                .unwrap_or(u64::MAX)
        })
        .collect::<Vec<_>>();
    let all_success = std::iter::once(&initialize)
        .chain(forks.iter())
        .chain(rollbacks.iter())
        .chain(squashes.iter())
        .all(|invocation| {
            invocation.exit_code == Some(0)
                && invocation.response["status"].as_str() == Some("succeeded")
                && invocation.response["outcome_path"]
                    .as_str()
                    .is_some_and(|path| Path::new(path).exists())
        });
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    let no_sm14_mount = fresh_activation_ids.iter().all(|allocation_id| {
        !mountinfo.contains(allocation_id.as_str())
            && open_allocation(&context.arena_root(), allocation_id).is_err()
    });
    Ok(CaseExecution {
        assertions: vec![
            assertion("create_exec_success", command.success, command.success, true),
            assertion(
                "stationary_publication",
                stationary.stable.after.allocation_id == created_allocation_id
                    && stationary.adoption.new_owner.subject
                        == OwnerSubject::PayloadOwned {
                            publication_id: publication_id.clone(),
                        },
                format!("{stationary:?}"),
                "same allocation selected PayloadOwned",
            ),
            assertion(
                "catalog_existing_operations",
                catalog_binding.facts.publish_workspace_session
                    && catalog_binding.facts.squash_layerstacks,
                format!("{:?}", catalog_binding.facts),
                "publish=true squash=true",
            ),
            assertion(
                "catalog_unsupported_names_not_claimed",
                !catalog_binding.facts.activate_workspace_session
                    && !catalog_binding.facts.fork_workspace_session
                    && !catalog_binding.facts.rollback_workspace_session,
                format!("{:?}", catalog_binding.facts),
                "activate=false fork=false rollback=false",
            ),
            assertion("cli_success_outcomes_durable", all_success, all_success, true),
            assertion(
                "cli_error_outcome_durable",
                failed.exit_code == Some(0)
                    && failed.response["status"].as_str() == Some("failed")
                    && failed.response["outcome_path"]
                        .as_str()
                        .is_some_and(|path| Path::new(path).exists()),
                failed.response.clone(),
                "typed failed durable outcome",
            ),
            assertion(
                "cli_cancel_outcome_durable",
                cancelled.exit_code == Some(0)
                    && cancelled.response["status"].as_str() == Some("cancelled")
                    && cancelled.response["outcome_path"]
                        .as_str()
                        .is_some_and(|path| Path::new(path).exists()),
                cancelled.response.clone(),
                "typed cancelled durable outcome",
            ),
            assertion(
                "inactive_fork_metadata_only",
                before_metadata_allocations == after_inactive_forks
                    && before_metadata_allocations == after_metadata_allocations
                    && forks.iter().all(|invocation| {
                        invocation.response["payload_objects_created"].as_u64() == Some(0)
                    }),
                format!(
                    "before={before_metadata_allocations} after_forks={after_inactive_forks} after={after_metadata_allocations}"
                ),
                "allocation count unchanged and zero payload objects",
            ),
            assertion(
                "fork_outer_budget",
                fork_outer.iter().all(|duration| *duration <= 10_000_000),
                format!("{fork_outer:?}"),
                "each <=10000000ns",
            ),
            assertion(
                "fork_to_ready_budget",
                fork_activation_ns
                    .iter()
                    .all(|duration| *duration <= 10_000_000),
                format!("{fork_activation_ns:?}"),
                "each <=10000000ns",
            ),
            assertion(
                "rollback_budget",
                rollback_outer
                    .iter()
                    .all(|duration| *duration <= 20_000_000)
                    && rollback_service
                        .iter()
                        .all(|duration| *duration <= 20_000_000),
                format!("outer={rollback_outer:?} service={rollback_service:?}"),
                "outer and service each <=20000000ns",
            ),
            assertion(
                "squash_budget",
                squash_outer
                    .iter()
                    .all(|duration| *duration <= 10_000_000)
                    && squash_service
                        .iter()
                        .all(|duration| *duration <= 1_000_000),
                format!("outer={squash_outer:?} service={squash_service:?}"),
                "outer <=10000000ns service <=1000000ns",
            ),
            assertion(
                "first_commands_ready",
                first_commands.iter().all(|receipt| receipt.success),
                format!("{first_commands:?}"),
                "all succeeded",
            ),
            assertion(
                "close_releases_resources",
                active_lease["active"].as_bool() == Some(false) && no_sm14_mount,
                format!("lease={} no_mount={no_sm14_mount}", active_lease["active"]),
                "inactive publication lease and no fresh allocation/mount",
            ),
            assertion(
                "case_budget",
                elapsed < Duration::from_secs(15),
                format!("{elapsed:?}"),
                "<15s",
            ),
        ],
        details: json!({
            "duration_ns": ns(elapsed),
            "fixture": fixture,
            "command": command,
            "publication_duration_ns": ns(publish_elapsed),
            "publication": stationary,
            "semantic": semantic.receipt,
            "selected_ref": selected_ref,
            "catalog_binding": catalog_binding,
            "transcript": {
                "initialize": initialize,
                "forks": forks,
                "rollbacks": rollbacks,
                "squashes": squashes,
                "failed": failed,
                "cancelled": cancelled,
            },
            "timings": {
                "fork_outer_ns": fork_outer,
                "fork_activation_ns": fork_activation_ns,
                "fork_activation_phase_spans": fork_activation_phase_spans,
                "first_command_ns": first_command_ns,
                "rollback_outer_ns": rollback_outer,
                "rollback_service_ns": rollback_service,
                "squash_outer_ns": squash_outer,
                "squash_service_ns": squash_service,
            },
            "storage": {
                "allocations_before_metadata": before_metadata_allocations,
                "allocations_after_inactive_forks": after_inactive_forks,
                "allocations_after_metadata": after_metadata_allocations,
                "metadata_root": lifecycle_root,
                "squash_category": "control-metadata-only",
                "squash_payload_bytes": 0,
            },
        }),
    })
}

#[allow(clippy::too_many_arguments)]
fn lifecycle_metadata_args(
    state_root: &Path,
    operation_id: &str,
    action: &str,
    branch: &str,
    source: Option<&str>,
    target: Option<&str>,
    allocation_id: Option<&str>,
    root_id: Option<&str>,
    attribution_root_id: Option<&str>,
    cancel: bool,
) -> Vec<String> {
    let mut arguments = vec![
        "lifecycle-metadata".to_owned(),
        "--state-root".to_owned(),
        state_root.display().to_string(),
        "--operation-id".to_owned(),
        operation_id.to_owned(),
        "--action".to_owned(),
        action.to_owned(),
        "--branch".to_owned(),
        branch.to_owned(),
    ];
    for (name, value) in [
        ("--source", source),
        ("--target", target),
        ("--allocation-id", allocation_id),
        ("--root-id", root_id),
        ("--attribution-root-id", attribution_root_id),
    ] {
        if let Some(value) = value {
            arguments.push(name.to_owned());
            arguments.push(value.to_owned());
        }
    }
    if cancel {
        arguments.push("--cancel".to_owned());
    }
    arguments
}

fn invoke_lifecycle_cli(
    context: &Context,
    arguments: Vec<String>,
) -> CampaignResult<CliInvocation> {
    let mut argv = vec![context.cli_path.display().to_string()];
    argv.extend(arguments.iter().cloned());
    let started = Instant::now();
    let output = Command::new(&context.cli_path)
        .args(&arguments)
        .stdin(Stdio::null())
        .output()?;
    let outer_elapsed_ns = ns(started.elapsed());
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    let response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| json!({"parse_error": "stdout was not a single JSON response"}));
    Ok(CliInvocation {
        argv,
        exit_code: output.status.code(),
        stdout,
        stderr,
        outer_elapsed_ns,
        response,
    })
}

fn allocation_directory_count(payload_root: &Path) -> CampaignResult<u64> {
    Ok(u64::try_from(
        allocation_roots(&payload_root.join("allocations"))?.len(),
    )?)
}

fn allocation_roots(arena_root: &Path) -> CampaignResult<Vec<PathBuf>> {
    if !arena_root.exists() {
        return Ok(Vec::new());
    }
    let mut roots = Vec::new();
    for prefix in fs::read_dir(arena_root)? {
        let prefix = prefix?;
        if !prefix.file_type()?.is_dir() {
            continue;
        }
        for allocation in fs::read_dir(prefix.path())? {
            let allocation = allocation?;
            let allocation_root = allocation.path();
            if allocation.file_type()?.is_dir() && allocation_root.join("ALLOCATION.json").is_file()
            {
                roots.push(allocation_root);
            }
        }
    }
    roots.sort();
    Ok(roots)
}

fn full_build(
    campaign_root: &Path,
    canonical_object_dir: &Path,
    allocation: &AllocationHandle,
    label: &str,
) -> CampaignResult<SemanticBuildOutput> {
    fs::create_dir_all(canonical_object_dir)?;
    let spool_root = campaign_root.join("spool");
    fs::create_dir_all(&spool_root)?;
    let spool_dir = spool_root.join(format!("{label}-{}", OperationId::new()));
    Ok(build_with_output(&SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string(label),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        sealed_tree: allocation.upper_dir.clone(),
        spool_dir,
        canonical_object_dir: canonical_object_dir.to_path_buf(),
        attribution: attribution(),
    })?)
}

fn install_ref(
    context: &Context,
    allocation: &AllocationHandle,
    semantic: &SemanticBuildReceipt,
    owner_epoch: u64,
    accounted_bytes: u64,
    operation_id: &OperationId,
    publication_id: &PublicationId,
) -> CampaignResult<PairedRefValue> {
    let campaign_root = context
        .control_root
        .join("campaign")
        .join(context.run_id.as_str());
    let locator_store = LocatorStore::open(campaign_root.join("locators"))?;
    let ref_store = PairedRefStore::open(campaign_root.join("refs"))?;
    let locator = install_locator(
        &locator_store,
        allocation,
        semantic,
        owner_epoch,
        accounted_bytes,
        operation_id,
        publication_id,
    )?;
    let expected_sequence = ref_store
        .read(BRANCH)?
        .map_or(RefSequence::ZERO, |value| value.sequence);
    let outcome = ref_store.commit(
        BRANCH,
        &LocatorRefCandidate {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
            roots: semantic.roots.clone(),
            locator_generation: locator.generation,
            expected_sequence,
        },
        &semantic.durability,
        &locator,
        &locator_store,
        &mut NamedFaultInjector::default(),
    )?;
    match outcome {
        RefCommitOutcome::Committed(receipt) => Ok(receipt.value),
        RefCommitOutcome::ExpectedParent { expected, observed } => Err(format!(
            "paired ref parent conflict: expected {expected}, observed {observed}"
        )
        .into()),
    }
}

fn install_locator(
    locator_store: &LocatorStore,
    allocation: &AllocationHandle,
    semantic: &SemanticBuildReceipt,
    owner_epoch: u64,
    accounted_bytes: u64,
    operation_id: &OperationId,
    publication_id: &PublicationId,
) -> CampaignResult<LocatorDurabilityReceipt> {
    let payload_root = PayloadRootId::parse(semantic.roots.root_id.as_str())?;
    for attempt in 0..64_u8 {
        let selected = locator_store.selected()?;
        let result = locator_store.install(
            &LocatorDelta {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                publication_id: publication_id.clone(),
                expected_parent: selected
                    .as_ref()
                    .map(|generation| generation.receipt.generation),
                forward: vec![ForwardLocatorEntry {
                    payload_root: payload_root.clone(),
                    allocation_id: allocation.descriptor.allocation_id.clone(),
                    owner_epoch,
                    extents: vec![LocatorExtent {
                        relative_path: "upper".to_owned(),
                        offset: 0,
                        length: accounted_bytes.max(1),
                    }],
                }],
                reverse: vec![ReverseLocatorEntry {
                    allocation_id: allocation.descriptor.allocation_id.clone(),
                    owner_epoch,
                    operation_id: operation_id.clone(),
                    publication_id: publication_id.clone(),
                    payload_roots: vec![payload_root.clone()],
                    accounted_bytes: accounted_bytes.max(1),
                }],
            },
            &mut NamedFaultInjector::default(),
        );
        match result {
            Ok(receipt) => return Ok(receipt),
            Err(PocError::OwnerConflict(message))
                if attempt < 63 && message.starts_with("locator expected parent ") =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(
        PocError::RecoveryRequired("locator compare-and-install retry bound exhausted".to_owned())
            .into(),
    )
}

fn build_tiny_delta_carrier(
    context: &Context,
    base: &AllocationHandle,
    initial: &PublishedSemanticState,
    deltas: &[AllocationHandle],
) -> CampaignResult<AllocationHandle> {
    if deltas.is_empty() || deltas.len() > sandbox_runtime_mpla_poc::projection::MAX_RECENT_DELTAS {
        return Err("SM-06 carrier delta count is outside the projection bound".into());
    }
    let delta_count = u8::try_from(deltas.len())?;
    let mut carrier_sources = Vec::with_capacity(deltas.len());
    for (index, delta) in deltas.iter().enumerate() {
        let index = u8::try_from(index)?;
        let path = s1_module_path(200 + u64::from(index));
        let source = delta.upper_dir.join(&path);
        if !source.is_file() {
            return Err(format!("SM-06 carrier source is absent: {}", source.display()).into());
        }
        carrier_sources.push((source, path));
    }
    let operation_id =
        OperationId::from_string(format!("{}-sm06-carrier-{delta_count:02}", context.run_id));
    let activated = activate_exact(ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(format!(
            "{}-sm06-carrier-build-{delta_count:02}",
            context.run_id
        )),
        allocation_operation_id: operation_id.clone(),
        selected_ref: initial.selected_ref.clone(),
        recipe: ProjectionRecipe {
            schema_version: SCHEMA_VERSION,
            roots: initial.semantic.receipt.roots.clone(),
            base_allocation_id: base.descriptor.allocation_id.clone(),
            net_delta_carrier_id: None,
            recent_delta_ids: Vec::new(),
        },
        payload_allocations: vec![base.clone()],
        arena_root: context.arena_root(),
        control_root: context.control_root.clone(),
        cgroup_procs_path: context.cgroup_procs_path.clone(),
        readiness_path: PathBuf::from("src/d0000/module-00000000.rs"),
        readiness_contains: None,
        readiness_timeout: Duration::from_secs(2),
    })?;
    let mut session = activated.session;
    let allocation = session.allocation().clone();
    let lease = session.mutable_lease().clone();
    let mut commands = Vec::with_capacity(carrier_sources.len() + 1);
    for (source, path) in carrier_sources {
        commands.push(format!(
            "cp --preserve=mode,ownership,timestamps --reflink=never --no-target-directory '{}' '{}'",
            source.display(),
            path.display()
        ));
    }
    commands.push(format!(
        "touch --no-dereference --reference='{}' .",
        base.upper_dir.display()
    ));
    let result = session.execute(
        &lease.writer,
        Path::new("/bin/sh"),
        &["-c".to_owned(), commands.join(" && ")],
        Duration::from_secs(2),
    );
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            drop(session);
            let cleanup = destroy_workspace_allocation(
                &context.arena_root(),
                &allocation.descriptor.allocation_id,
                &lease.deleter,
            );
            return match cleanup {
                Ok(()) => Err(error.into()),
                Err(cleanup_error) => Err(format!(
                    "SM-06 carrier execution failed: {error}; cleanup failed: {cleanup_error}"
                )
                .into()),
            };
        }
    };
    if !result.success {
        drop(session);
        let cleanup = destroy_workspace_allocation(
            &context.arena_root(),
            &allocation.descriptor.allocation_id,
            &lease.deleter,
        );
        let failure = format!(
            "SM-06 carrier materialization failed: exit_code={:?}, timed_out={}",
            result.exit_code, result.timed_out
        );
        return match cleanup {
            Ok(()) => Err(failure.into()),
            Err(cleanup_error) => Err(format!("{failure}; cleanup failed: {cleanup_error}").into()),
        };
    }
    let publication_id =
        PublicationId::from_string(format!("{}-sm06-carrier-{delta_count:02}", context.run_id));
    stationary_adopt(
        &mut session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
        },
        &context.control_root.join("operations"),
        &mut FaultInjector::default(),
    )?;
    Ok(allocation)
}

fn published_s1(context: &Context) -> CampaignResult<Published> {
    let fixture = context.fixture(FixtureId::S1Code)?;
    let allocation = open_allocation(&context.arena_root(), &fixture.allocation_id)?;
    let store = PairedRefStore::open(
        context
            .control_root
            .join("campaign")
            .join(context.run_id.as_str())
            .join("refs"),
    )?;
    let selected_ref = store.read(BRANCH)?.ok_or("SM-03 has not selected main")?;
    Ok(Published {
        roots: selected_ref.roots.clone(),
        selected_ref,
        allocation,
    })
}

fn prepare_sm10_candidate(
    context: &Context,
    agent: u8,
    barrier: &Barrier,
    case_started: Instant,
) -> CampaignResult<(OccOwnedCandidate, Sm10Timeline)> {
    let prior: PublishedSemanticState =
        durable::read_json(&context.campaign_root().join("SM03_STATE.json"))?;
    let base = open_allocation(&context.arena_root(), &prior.allocation_id)?;
    let operation_id = OperationId::from_string(format!("{}-sm10-agent-{agent}", context.run_id));
    let publication_id =
        PublicationId::from_string(format!("{}-sm10-agent-{agent}", context.run_id));
    let activated = activate_exact(ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(format!(
            "{}-sm10-agent-{agent}",
            context.run_id
        )),
        allocation_operation_id: operation_id.clone(),
        selected_ref: prior.selected_ref,
        recipe: ProjectionRecipe {
            schema_version: SCHEMA_VERSION,
            roots: prior.semantic.receipt.roots.clone(),
            base_allocation_id: base.descriptor.allocation_id.clone(),
            net_delta_carrier_id: None,
            recent_delta_ids: Vec::new(),
        },
        payload_allocations: vec![base],
        arena_root: context.arena_root(),
        control_root: context.control_root.clone(),
        cgroup_procs_path: context.cgroup_procs_path.clone(),
        readiness_path: PathBuf::from("src/d0000/module-00000000.rs"),
        readiness_contains: None,
        readiness_timeout: Duration::from_secs(2),
    })?;
    let mut session = activated.session;
    let allocation = session.allocation().clone();
    let lease = session.mutable_lease().clone();
    let paths = (0..=agent)
        .map(|index| s1_module_path(100 + u64::from(index)))
        .collect::<Vec<_>>();
    let work = context
        .campaign_root()
        .join("receipt-sm10")
        .join(format!("agent-{agent}"));
    fs::create_dir_all(&work)?;
    let workspace = session
        .workspace_root()
        .ok_or("SM-10 workspace disappeared")?
        .to_path_buf();
    let before = capture_affected_paths(&workspace, &paths, &work.join("before"))?;
    let edit = session.execute(
        &lease.writer,
        Path::new("/bin/sh"),
        &s1_edit_arguments(&paths),
        Duration::from_secs(2),
    )?;
    if !edit.success {
        return Err(format!("SM-10 agent {agent} edit failed").into());
    }
    let after = capture_affected_paths(&workspace, &paths, &work.join("after"))?;
    let affected_stream = work.join("affected.records");
    let affected_stream_sha256 =
        write_affected_stream_from_snapshots(&affected_stream, &before, &after)?;
    let seal_input = ReceiptHitSealInput {
        schema_version: SCHEMA_VERSION,
        affected_stream: affected_stream.clone(),
        affected_stream_sha256: affected_stream_sha256.clone(),
        affected_paths: paths,
    };

    barrier.wait();
    let barrier_release_ns = ns(case_started.elapsed());
    let stationary = stationary_adopt_receipt_hit(
        &mut session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
        },
        &context.control_root.join("operations"),
        &seal_input,
        &mut FaultInjector::default(),
    )?;
    barrier.wait();
    let hash_started_ns = ns(case_started.elapsed());
    let semantic = build_incremental(&IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        prior_manifest: prior.semantic.root_manifest_path,
        expected_prior_roots: prior.semantic.receipt.roots,
        expected_prior_record_stream_sha256: prior.semantic.receipt.record_stream_sha256,
        affected_stream,
        affected_stream_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: context.preparation.canonical_object_dir.clone(),
        attribution: attribution(),
    })?;
    let hash_finished_ns = ns(case_started.elapsed());
    let owner_epoch = stationary.stationary.adoption.new_owner.owner_epoch;
    let accounted_bytes = stationary.stationary.stable.after.allocated_bytes.max(1);
    let locator_store = LocatorStore::open(context.campaign_root().join("locators"))?;
    install_locator(
        &locator_store,
        &allocation,
        &semantic.receipt,
        owner_epoch,
        accounted_bytes,
        &operation_id,
        &publication_id,
    )?;
    let response_ns = ns(case_started.elapsed());
    Ok((
        OccOwnedCandidate {
            allocation,
            operation_id,
            publication_id,
            owner_epoch,
            accounted_bytes,
            semantic: semantic.receipt,
        },
        Sm10Timeline {
            agent,
            barrier_release_ns,
            hash_started_ns,
            hash_finished_ns,
            response_ns,
        },
    ))
}

fn prepare_occ_candidate(
    context: &Context,
    label: &str,
    command: &str,
) -> CampaignResult<OccOwnedCandidate> {
    let operation_id = OperationId::from_string(format!("{}-sm11-{label}", context.run_id));
    let publication_id = PublicationId::from_string(format!("{}-sm11-{label}", context.run_id));
    let allocation = create_allocation(&context.arena_root(), &operation_id)?;
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &context.control_root,
        allocation.clone(),
        lease.clone(),
        context.raw_session_lower_dirs(),
        context.cgroup_procs_path.clone(),
    )?;
    let result = session.execute(
        &lease.writer,
        Path::new("/bin/sh"),
        &["-c".to_owned(), command.to_owned()],
        Duration::from_secs(2),
    )?;
    if !result.success {
        return Err(format!("SM-11 publisher {label} edit failed").into());
    }
    let stationary = stationary_adopt(
        &mut session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
        },
        &context.control_root.join("operations"),
        &mut FaultInjector::default(),
    )?;
    let semantic = full_build(
        &context.campaign_root(),
        &context.preparation.canonical_object_dir,
        &allocation,
        &format!("sm11-{label}"),
    )?;
    let owner_epoch = stationary.adoption.new_owner.owner_epoch;
    let accounted_bytes = stationary.stable.after.allocated_bytes.max(1);
    let locator_store = LocatorStore::open(context.campaign_root().join("locators"))?;
    install_locator(
        &locator_store,
        &allocation,
        &semantic.receipt,
        owner_epoch,
        accounted_bytes,
        &operation_id,
        &publication_id,
    )?;
    Ok(OccOwnedCandidate {
        allocation,
        operation_id,
        publication_id,
        owner_epoch,
        accounted_bytes,
        semantic: semantic.receipt,
    })
}

fn occ_publication<const N: usize>(
    owned: &OccOwnedCandidate,
    expected_sequence: RefSequence,
    paths: [&str; N],
) -> CampaignResult<OccPublication> {
    Ok(OccPublication {
        candidate: LocatorRefCandidate {
            schema_version: SCHEMA_VERSION,
            operation_id: owned.operation_id.clone(),
            publication_id: owned.publication_id.clone(),
            roots: owned.semantic.roots.clone(),
            locator_generation: sandbox_runtime_mpla_poc::LocatorGeneration::INITIAL,
            expected_sequence,
        },
        canonical: owned.semantic.durability.clone(),
        changed_paths: ChangedPathSet::new(paths.into_iter().map(str::to_owned))?,
        conflict_allocation: ConflictAllocation {
            allocation_root: owned.allocation.allocation_root.clone(),
            allocation_id: owned.allocation.descriptor.allocation_id.clone(),
            owner_epoch: owned.owner_epoch,
            accounted_bytes: owned.accounted_bytes,
        },
    })
}

fn s1_module_path(index: u64) -> PathBuf {
    PathBuf::from(format!("src/d{:04}/module-{index:08}.rs", index % 64))
}

fn parallel_receipt_hit_publication(
    session: &mut sandbox_runtime_mpla_poc::MplaSession,
    publication_request: StationaryPublicationRequest,
    operations_root: PathBuf,
    seal_input: ReceiptHitSealInput,
    incremental_request: IncrementalBuildRequest,
) -> CampaignResult<(
    ReceiptHitPublicationReceipt,
    IncrementalBuildOutput,
    Duration,
    Duration,
)> {
    let (stationary, incremental) = std::thread::scope(|scope| {
        let stationary_task = scope.spawn(move || {
            let started = Instant::now();
            let receipt = stationary_adopt_receipt_hit(
                session,
                &publication_request,
                &operations_root,
                &seal_input,
                &mut FaultInjector::default(),
            )?;
            Ok::<_, PocError>((receipt, started.elapsed()))
        });
        let incremental_task = scope.spawn(move || {
            let started = Instant::now();
            let output = build_incremental(&incremental_request)?;
            Ok::<_, PocError>((output, started.elapsed()))
        });
        let stationary = stationary_task.join().map_err(|_| {
            PocError::Integrity("stationary receipt-hit publication thread panicked".to_owned())
        })??;
        let incremental = incremental_task.join().map_err(|_| {
            PocError::Integrity("incremental semantic publication thread panicked".to_owned())
        })??;
        Ok::<_, PocError>((stationary, incremental))
    })?;
    Ok((stationary.0, incremental.0, stationary.1, incremental.1))
}

fn prior_changed_window_snapshot(
    record_stream: &Path,
    relative: &Path,
    offset: u64,
) -> CampaignResult<AffectedPathSnapshot> {
    let path = relative.as_os_str().as_bytes();
    let mut reader = RecordStreamReader::new(BufReader::with_capacity(
        32 * 1024,
        File::open(record_stream)?,
    ));
    let mut records = Vec::with_capacity(2);
    while let Some(record) = reader.next_record()? {
        match &record {
            SemanticRecord::Node(node) if node.path == path => records.push(record),
            SemanticRecord::Chunk {
                path: record_path,
                offset: record_offset,
                ..
            } if record_path == path && *record_offset == offset => records.push(record),
            _ => {}
        }
    }
    let has_node = records
        .iter()
        .any(|record| matches!(record, SemanticRecord::Node(_)));
    let has_chunk = records
        .iter()
        .any(|record| matches!(record, SemanticRecord::Chunk { .. }));
    if !has_node || !has_chunk || records.len() != 2 {
        return Err(format!(
            "prior semantic stream lacks one node and one chunk for {}@{offset}",
            relative.display()
        )
        .into());
    }
    Ok(AffectedPathSnapshot {
        paths: vec![relative.to_path_buf()],
        records,
        payload_bytes_read: 0,
    })
}

fn changed_window_snapshot(
    tree: &Path,
    relative: &Path,
    offset: u64,
    prior: &AffectedPathSnapshot,
) -> CampaignResult<AffectedPathSnapshot> {
    let physical = tree.join(relative);
    let metadata = fs::symlink_metadata(&physical)?;
    let mut records = Vec::with_capacity(2);
    let mut payload_bytes_read = 0_u64;
    for record in &prior.records {
        match record {
            SemanticRecord::Node(node) => {
                let mut node = node.clone();
                node.mode = metadata.permissions().mode() & 0o7777;
                node.uid = metadata.uid();
                node.gid = metadata.gid();
                node.mtime_seconds = metadata.mtime();
                node.mtime_nanoseconds = u32::try_from(metadata.mtime_nsec())
                    .map_err(|_| "SM-07 observed a negative mtime nanosecond")?;
                node.logical_size = metadata.len();
                records.push(SemanticRecord::Node(node));
            }
            SemanticRecord::Chunk {
                path,
                offset: prior_offset,
                length,
                ..
            } if *prior_offset == offset => {
                let length = usize::try_from(*length)?;
                let file = File::open(&physical)?;
                let mut bytes = vec![0_u8; length];
                let mut filled = 0_usize;
                while filled < bytes.len() {
                    let position = offset
                        .checked_add(u64::try_from(filled)?)
                        .ok_or("SM-07 changed-window offset overflow")?;
                    let count = file.read_at(&mut bytes[filled..], position)?;
                    if count == 0 {
                        return Err("SM-07 changed-window read reached early EOF".into());
                    }
                    filled += count;
                }
                let mut digest = Sha256::new();
                digest.update(b"mpla-poc-semantic-v1/chunk-bytes\0");
                digest.update(&bytes);
                records.push(SemanticRecord::Chunk {
                    path: path.clone(),
                    offset,
                    length: u32::try_from(length)?,
                    sha256: digest.finalize().into(),
                });
                payload_bytes_read = payload_bytes_read.saturating_add(u64::try_from(length)?);
            }
            _ => {
                return Err("SM-07 prior range snapshot contains an unexpected record".into());
            }
        }
    }
    Ok(AffectedPathSnapshot {
        paths: prior.paths.clone(),
        records,
        payload_bytes_read,
    })
}

fn attribution() -> AttributionInput {
    AttributionInput {
        actor_id: "mpla-poc-candidate".to_owned(),
        semantic_operation_id: "m1-smoke-semantic".to_owned(),
    }
}

fn s1_edit_arguments(paths: &[PathBuf]) -> Vec<String> {
    let commands = paths
        .iter()
        .map(|path| {
            format!(
                "dd if=/dev/zero of='{}' bs=1024 count=100 conv=notrunc status=none",
                path.display()
            )
        })
        .collect::<Vec<_>>()
        .join(" && ");
    vec!["-c".to_owned(), commands]
}

fn stale_writer(path: &Path) -> CampaignResult<sandbox_runtime_mpla_poc::WriterCapability> {
    let value: Value = durable::read_json(path)?;
    Ok(serde_json::from_value(json!({
        "allocation_id": value["allocation_id"],
        "session_id": value["session_id"],
        "lease_epoch": value["lease_epoch"].as_u64().unwrap_or(2).saturating_sub(1),
        "owner_epoch": value["owner_epoch"].as_u64().unwrap_or(2).saturating_sub(1),
        "nonce": value["writer_nonce"],
    }))?)
}

fn stale_deleter(path: &Path) -> CampaignResult<sandbox_runtime_mpla_poc::DeletionCapability> {
    let value: Value = durable::read_json(path)?;
    Ok(serde_json::from_value(json!({
        "allocation_id": value["allocation_id"],
        "session_id": value["session_id"],
        "lease_epoch": value["lease_epoch"].as_u64().unwrap_or(2).saturating_sub(1),
        "owner_epoch": value["owner_epoch"].as_u64().unwrap_or(2).saturating_sub(1),
        "nonce": value["deleter_nonce"],
    }))?)
}

fn assertion(
    name: impl Into<String>,
    passed: bool,
    observed: impl ToString,
    expected: impl ToString,
) -> AssertionReceipt {
    AssertionReceipt {
        name: name.into(),
        passed,
        observed: observed.to_string(),
        expected: expected.to_string(),
    }
}

fn evidence_class(case_id: &str) -> EvidenceClass {
    match case_id {
        "SM-03" | "SM-05" | "SM-06" | "SM-07" | "SM-08" | "SM-09" | "SM-14" => {
            EvidenceClass::MatchedSpeedup
        }
        _ => EvidenceClass::AbsoluteGateOnly,
    }
}

fn ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn median_u64(values: &[u64]) -> u64 {
    let mut values = values.to_vec();
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        values[middle - 1]
            .saturating_add(values[middle])
            .saturating_div(2)
    } else {
        values[middle]
    }
}

struct Roots {
    run_id: RunId,
    payload_root: PathBuf,
    control_root: PathBuf,
    fixtures_root: PathBuf,
    evidence_root: PathBuf,
}

impl Roots {
    fn from_env() -> CampaignResult<Self> {
        Ok(Self {
            run_id: RunId::parse(required_env("MPLA_POC_RUN_ID")?)?,
            payload_root: required_path("MPLA_POC_PAYLOAD_ROOT")?,
            control_root: required_path("MPLA_POC_CONTROL_ROOT")?,
            fixtures_root: required_path("MPLA_POC_FIXTURES_ROOT")?,
            evidence_root: required_path("MPLA_POC_EVIDENCE_ROOT")?,
        })
    }
}

impl Context {
    fn from_env() -> CampaignResult<Self> {
        let roots = Roots::from_env()?;
        let preparation_path = roots
            .control_root
            .join("campaign")
            .join(roots.run_id.as_str())
            .join("PREPARED.json");
        let preparation: PreparationReceipt = durable::read_json(&preparation_path)?;
        if preparation.run_id != roots.run_id || preparation.schema_version != SCHEMA_VERSION {
            return Err("preparation receipt scope mismatch".into());
        }
        let cgroup_procs_path = std::env::var_os("MPLA_POC_CGROUP_PROCS").map(PathBuf::from);
        let storage_cgroup_dir = std::env::var_os("MPLA_POC_STORAGE_CGROUP_DIR").map(PathBuf::from);
        Ok(Self {
            run_id: roots.run_id,
            payload_root: roots.payload_root,
            control_root: roots.control_root,
            fixtures_root: roots.fixtures_root,
            evidence_root: roots.evidence_root,
            qualification_path: required_path("MPLA_POC_QUALIFICATION_PATH")?,
            oracle_path: required_path("MPLA_POC_ORACLE_BIN")?,
            cli_path: required_path("MPLA_POC_CLI_BIN")?,
            catalog_binding_path: required_path("MPLA_POC_CATALOG_BINDING_PATH")?,
            cgroup_procs_path,
            storage_cgroup_dir,
            preparation,
        })
    }

    fn fixture(&self, fixture_id: FixtureId) -> CampaignResult<&PreparedFixture> {
        self.preparation
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture_id == fixture_id)
            .ok_or_else(|| format!("fixture {} is not prepared", fixture_id.as_str()).into())
    }

    fn campaign_root(&self) -> PathBuf {
        self.control_root
            .join("campaign")
            .join(self.run_id.as_str())
    }

    fn arena_root(&self) -> PathBuf {
        self.payload_root.join("allocations")
    }

    fn raw_session_lower_dirs(&self) -> Vec<PathBuf> {
        vec![self.campaign_root().join("empty-lower")]
    }

    fn storage_cgroup_snapshot(&self) -> CampaignResult<Option<StorageCgroupSnapshot>> {
        let Some(root) = &self.storage_cgroup_dir else {
            return Ok(None);
        };
        Ok(Some(StorageCgroupSnapshot {
            sampled_unix_ms: sandbox_runtime_mpla_poc::unix_time_ms()?,
            memory_current: read_u64_file(&root.join("memory.current"))?,
            memory_peak: read_u64_file(&root.join("memory.peak"))?,
            memory_high: fs::read_to_string(root.join("memory.high"))?
                .trim()
                .to_owned(),
            memory_max: fs::read_to_string(root.join("memory.max"))?
                .trim()
                .to_owned(),
            memory_events: read_counter_file(&root.join("memory.events"))?,
            memory_stat: read_counter_file(&root.join("memory.stat"))?,
            process_ids: fs::read_to_string(root.join("cgroup.procs"))?
                .lines()
                .map(str::parse)
                .collect::<Result<Vec<_>, _>>()?,
        }))
    }
}

fn read_u64_file(path: &Path) -> CampaignResult<u64> {
    Ok(fs::read_to_string(path)?.trim().parse()?)
}

fn read_counter_file(path: &Path) -> CampaignResult<std::collections::BTreeMap<String, u64>> {
    let mut counters = std::collections::BTreeMap::new();
    for line in fs::read_to_string(path)?.lines() {
        let mut fields = line.split_whitespace();
        let name = fields.next().ok_or("cgroup counter name is missing")?;
        let value = fields
            .next()
            .ok_or("cgroup counter value is missing")?
            .parse()?;
        if fields.next().is_some() || counters.insert(name.to_owned(), value).is_some() {
            return Err(format!("invalid cgroup counter line in {}", path.display()).into());
        }
    }
    Ok(counters)
}

fn required_env(name: &str) -> CampaignResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} is required"))?)
}

fn required_path(name: &str) -> CampaignResult<PathBuf> {
    Ok(PathBuf::from(required_env(name)?))
}

fn run_oracle(binary: &Path, tree: &Path, records: &Path) -> CampaignResult<Value> {
    let output = Command::new(binary)
        .arg("--tree")
        .arg(tree)
        .arg("--records")
        .arg(records)
        .arg("--actor-id")
        .arg("mpla-poc-candidate")
        .arg("--semantic-operation-id")
        .arg("m1-smoke-semantic")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "independent oracle exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn copy_tree_test_only(source: &Path, destination: &Path) -> CampaignResult {
    let source_contents = source.join(".");
    let status = Command::new("/bin/cp")
        .arg("-a")
        .arg(source_contents)
        .arg(destination)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("test-only physical substitution copy exited {status}").into())
    }
}

fn different_representative_inode(
    source: &Path,
    destination: &Path,
    relative: &Path,
) -> CampaignResult<bool> {
    use std::os::unix::fs::MetadataExt;

    let source = fs::symlink_metadata(source.join(relative))?;
    let destination = fs::symlink_metadata(destination.join(relative))?;
    Ok((source.dev(), source.ino()) != (destination.dev(), destination.ino()))
}

fn files_equal_bounded(left: &Path, right: &Path) -> CampaignResult<bool> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = BufReader::with_capacity(32 * 1024, File::open(left)?);
    let mut right = BufReader::with_capacity(32 * 1024, File::open(right)?);
    let mut left_buffer = [0_u8; 32 * 1024];
    let mut right_buffer = [0_u8; 32 * 1024];
    loop {
        let left_count = left.read(&mut left_buffer)?;
        let right_count = right.read(&mut right_buffer)?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
}
