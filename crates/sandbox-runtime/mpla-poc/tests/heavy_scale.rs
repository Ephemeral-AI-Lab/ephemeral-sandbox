#![cfg(unix)]

use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sandbox_runtime_mpla_poc::allocation::{create_allocation, open_allocation};
use sandbox_runtime_mpla_poc::lease::issue_workspace_lease;
use sandbox_runtime_mpla_poc::locator::{
    ForwardLocatorEntry, LocatorDelta, LocatorExtent, LocatorStore, PayloadRootId,
    ReverseLocatorEntry,
};
use sandbox_runtime_mpla_poc::publication::{
    stationary_adopt, stationary_adopt_receipt_hit, ReceiptHitPublicationReceipt,
    StationaryPublicationReceipt, StationaryPublicationRequest,
};
use sandbox_runtime_mpla_poc::ref_store::{PairedRefStore, RefCommitOutcome};
use sandbox_runtime_mpla_poc::semantic::{
    build_incremental, build_with_output, capture_affected_paths, materialize_record_stream,
    write_affected_stream_from_snapshots, IncrementalBuildOutput, IncrementalBuildRequest,
    SemanticBuildOutput, SemanticResourceMaxima,
};
use sandbox_runtime_mpla_poc::{
    collect_control_changes, durable, fixture_plan, run_current_i2_closing, AllocationHandle,
    AllocationId, AttributionInput, CatalogBinding, ControlBoundary, ControlCacheMatch,
    ControlCollectionLimits, CurrentI2ClosingRequest, FaultInjector, FixtureId, FixtureReceipt,
    FixtureTier, LocatorDurabilityReceipt, LocatorRefCandidate, NamedFaultInjector, OperationId,
    PairedRefValue, PublicationId, ReceiptHitSealInput, RefSequence, RunId, SemanticBuildReceipt,
    SemanticBuildRequest, SessionId, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const POOL_BYTES: u64 = 8 * MIB;
const CGROUP_HIGH_BYTES: u64 = 96 * MIB;
const CGROUP_MAX_BYTES: u64 = 128 * MIB;
const HV01_SIZES: [u64; 3] = [GIB, 5 * GIB, 9 * GIB];
const HV01_PREP_SIZES: [u64; 5] = [GIB, GIB, GIB, 5 * GIB, 9 * GIB];
const HV01_DELTA_BYTES: u64 = MIB;
const HV01_DELTA_FILES: u64 = 10;
const HV02_BYTES: u64 = GIB;
const HV02_SAMPLES: usize = 6;
const HV03_BYTES: u64 = GIB;
const HV04_FILES: u64 = 250_000;
const ACTOR_ID: &str = "mpla-poc-candidate";
const INTERFACE_VERSION: &str = "m2r-iface-v1";
const LEASE_PREFIX: &str = "m2r-20260728T015724p0800:lead:";
const BRANCH: &str = "m2r-lead";

#[derive(Clone, Debug)]
struct Roots {
    run_id: RunId,
    payload_root: PathBuf,
    control_root: PathBuf,
    evidence_root: PathBuf,
    cgroup_procs: PathBuf,
    cgroup_dir: PathBuf,
    oracle_bin: PathBuf,
}

impl Roots {
    fn physical(case: &str) -> TestResult<Self> {
        let lease = required_string("MPLA_POC_EXECUTION_LEASE")?;
        let expected = format!("{LEASE_PREFIX}{case}");
        if lease != expected {
            return Err(format!(
                "lead-issued execution lease mismatch: expected {expected}, observed {lease}"
            )
            .into());
        }
        let run_id = RunId::parse(required_string("MPLA_POC_RUN_ID")?)?;
        if run_id.as_str() != "m2r-20260728T015724p0800" {
            return Err("run ID differs from the lead assignment capsule".into());
        }
        let roots = Self {
            run_id,
            payload_root: required_path("MPLA_POC_PAYLOAD_ROOT")?,
            control_root: required_path("MPLA_POC_CONTROL_ROOT")?,
            evidence_root: required_path("MPLA_POC_EVIDENCE_ROOT")?,
            cgroup_procs: required_path("MPLA_POC_CGROUP_PROCS")?,
            cgroup_dir: required_path("MPLA_POC_STORAGE_CGROUP_DIR")?,
            oracle_bin: required_path("MPLA_POC_ORACLE_BIN")?,
        };
        roots.verify(case)?;
        Ok(roots)
    }

    fn verify(&self, case: &str) -> TestResult {
        for path in [
            &self.payload_root,
            &self.control_root,
            &self.evidence_root,
            &self.cgroup_dir,
        ] {
            if !path.is_absolute() {
                return Err(format!("{} must be absolute", path.display()).into());
            }
        }
        if !self.cgroup_procs.is_file() || !self.oracle_bin.is_file() {
            return Err("cgroup.procs or independent oracle binary is missing".into());
        }
        let high = read_limit(&self.cgroup_dir.join("memory.high"))?;
        let max = read_limit(&self.cgroup_dir.join("memory.max"))?;
        if high != Some(CGROUP_HIGH_BYTES) || max != Some(CGROUP_MAX_BYTES) {
            return Err(format!(
                "{case} requires memory.high={CGROUP_HIGH_BYTES} and memory.max={CGROUP_MAX_BYTES}; observed {high:?}/{max:?}"
            )
            .into());
        }
        let pid = std::process::id().to_string();
        let members = fs::read_to_string(&self.cgroup_procs)?;
        if !members.lines().any(|member| member.trim() == pid) {
            return Err(format!("test process {pid} is outside the storage cgroup").into());
        }
        fs::create_dir_all(self.case_evidence(case))?;
        Ok(())
    }

    fn campaign_root(&self) -> PathBuf {
        self.control_root
            .join("m2r-lead")
            .join(self.run_id.as_str())
    }

    fn arena_root(&self) -> PathBuf {
        self.payload_root.join("allocations")
    }

    fn case_evidence(&self, case: &str) -> PathBuf {
        self.evidence_root.join(case.to_ascii_lowercase())
    }

    fn preparation_path(&self) -> PathBuf {
        self.campaign_root().join("PREPARATION.json")
    }
}

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
struct Hv01Fixture {
    existing_bytes: u64,
    allocation_id: AllocationId,
    prior: PreparedSemantic,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Preparation {
    schema_version: u32,
    interface_version: String,
    run_id: RunId,
    hv01: Vec<Hv01Fixture>,
    hv02_allocation_ids: Vec<AllocationId>,
    hv03_lower: PathBuf,
    hv03_allocation_id: AllocationId,
    hv04_allocation_id: AllocationId,
    hv04_receipt: FixtureReceipt,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ResourceSample {
    process_rss_bytes: u64,
    cgroup_memory_current_bytes: u64,
    cgroup_memory_peak_bytes: u64,
    cgroup_kernel_bytes: u64,
    cgroup_slab_bytes: u64,
    system_slab_bytes: Option<u64>,
    open_fds: u64,
    spool_logical_bytes: u64,
    spool_allocated_bytes: u64,
    spool_inodes: u64,
    queue_logical_bytes: u64,
    queue_allocated_bytes: u64,
    queue_inodes: u64,
    storage_allocated_blocks: u64,
    storage_fragment_bytes: u64,
    storage_used_inodes: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ResourceObservation {
    baseline: ResourceSample,
    maxima: ResourceSample,
    final_sample: ResourceSample,
    oom_before: u64,
    oom_after: u64,
    oom_kill_before: u64,
    oom_kill_after: u64,
}

struct ResourceMonitor {
    stop: Arc<AtomicBool>,
    task: Option<JoinHandle<TestResult<ResourceObservation>>>,
}

impl ResourceMonitor {
    fn start(roots: &Roots, spool_root: PathBuf) -> TestResult<Self> {
        let baseline = sample_resources(roots, &spool_root)?;
        let events = read_events(&roots.cgroup_dir.join("memory.events"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let cgroup_dir = roots.cgroup_dir.clone();
        let payload_root = roots.payload_root.clone();
        let task = std::thread::spawn(move || {
            let mut maxima = baseline.clone();
            while !thread_stop.load(Ordering::Acquire) {
                let roots = SamplingRoots {
                    cgroup_dir: &cgroup_dir,
                    payload_root: &payload_root,
                };
                let sample = sample_resources_from(&roots, &spool_root)?;
                merge_maxima(&mut maxima, &sample);
                std::thread::sleep(Duration::from_millis(50));
            }
            let roots = SamplingRoots {
                cgroup_dir: &cgroup_dir,
                payload_root: &payload_root,
            };
            let final_sample = sample_resources_from(&roots, &spool_root)?;
            merge_maxima(&mut maxima, &final_sample);
            let after = read_events(&cgroup_dir.join("memory.events"))?;
            Ok(ResourceObservation {
                baseline,
                maxima,
                final_sample,
                oom_before: *events.get("oom").unwrap_or(&0),
                oom_after: *after.get("oom").unwrap_or(&0),
                oom_kill_before: *events.get("oom_kill").unwrap_or(&0),
                oom_kill_after: *after.get("oom_kill").unwrap_or(&0),
            })
        });
        Ok(Self {
            stop,
            task: Some(task),
        })
    }

    fn finish(mut self) -> TestResult<ResourceObservation> {
        self.stop.store(true, Ordering::Release);
        self.task
            .take()
            .ok_or("resource monitor was already finished")?
            .join()
            .map_err(|_| Box::<dyn Error + Send + Sync>::from("resource monitor thread panicked"))?
    }
}

impl Drop for ResourceMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

struct SamplingRoots<'a> {
    cgroup_dir: &'a Path,
    payload_root: &'a Path,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OracleSummary {
    root_id: String,
    attribution_root_id: String,
    record_stream_sha256: String,
    record_stream_path: String,
    entry_count: u64,
    bytes_read: u64,
    spool_runs: u64,
    spool_bytes: u64,
    peak_open_data_fds: u16,
    peak_managed_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
struct PublicationTiming {
    boundary: String,
    elapsed_ns: u64,
    stationary_ns: u64,
    semantic_ns: u64,
    locator_and_ref_ns: u64,
}

#[derive(Clone, Debug)]
struct Published {
    stationary: StationaryPublicationReceipt,
    semantic: SemanticBuildOutput,
    selected_ref: PairedRefValue,
    timing: PublicationTiming,
}

#[derive(Clone, Debug)]
struct IncrementallyPublished {
    stationary: ReceiptHitPublicationReceipt,
    semantic: IncrementalBuildOutput,
    selected_ref: PairedRefValue,
    timing: PublicationTiming,
}

struct IncrementalPublicationInput {
    operation_id: OperationId,
    publication_id: PublicationId,
    seal_input: ReceiptHitSealInput,
    semantic_request: IncrementalBuildRequest,
    boundary_started: Instant,
}

#[test]
fn heavy_scale_contract_is_frozen() {
    assert_eq!(
        sandbox_runtime_mpla_poc::INTERFACE_VERSION,
        INTERFACE_VERSION
    );
    assert_eq!(HV01_SIZES, [GIB, 5 * GIB, 9 * GIB]);
    assert_eq!(HV01_PREP_SIZES, [GIB, GIB, GIB, 5 * GIB, 9 * GIB]);
    assert_eq!(HV01_DELTA_BYTES, MIB);
    assert_eq!(HV01_DELTA_FILES, 10);
    assert_eq!(HV02_BYTES, GIB);
    assert_eq!(HV02_SAMPLES, 6);
    assert_eq!(HV03_BYTES, GIB);
    assert_eq!(HV04_FILES, 250_000);
    assert_eq!(POOL_BYTES, 8 * MIB);
    assert_eq!(CGROUP_HIGH_BYTES, 96 * MIB);
    assert_eq!(CGROUP_MAX_BYTES, 128 * MIB);
    let plan = fixture_plan(FixtureId::S3Small, FixtureTier::Heavy);
    assert_eq!(plan.declared_paths, HV04_FILES);
    assert_eq!(plan.maximum_chain_bytes, GIB);
    if let Some(root) = std::env::var_os("MPLA_POC_HOST_EVIDENCE_ROOT") {
        let root = PathBuf::from(root);
        fs::create_dir_all(&root).expect("create host evidence root");
        durable::replace_json(
            &root.join("heavy-scale-contract.json"),
            &json!({
                "schema_version": SCHEMA_VERSION,
                "interface_version": INTERFACE_VERSION,
                "physical_execution_lease": "required",
                "hv01_existing_bytes": HV01_SIZES,
                "hv01_delta_bytes": HV01_DELTA_BYTES,
                "hv01_delta_files": HV01_DELTA_FILES,
                "hv02_stream_bytes": HV02_BYTES,
                "hv02_interleaved_samples": HV02_SAMPLES,
                "hv03_lower_bytes": HV03_BYTES,
                "hv03_write_bytes": 4096,
                "hv04_exact_paths": plan.declared_paths,
                "candidate_pool_bytes": POOL_BYTES,
                "memory_high_bytes": CGROUP_HIGH_BYTES,
                "memory_max_bytes": CGROUP_MAX_BYTES,
                "ignored_physical_entrypoints": [
                    "prepare_hv_01_through_hv_04",
                    "hv_01_existing_size_independence",
                    "hv_02_one_gib_upper_stream",
                    "hv_03_honest_overlay_copy_up",
                    "hv_04_exact_250k_files"
                ],
                "required_common_environment": [
                    "MPLA_POC_RUN_ID",
                    "MPLA_POC_PAYLOAD_ROOT",
                    "MPLA_POC_CONTROL_ROOT",
                    "MPLA_POC_EVIDENCE_ROOT",
                    "MPLA_POC_CGROUP_PROCS",
                    "MPLA_POC_STORAGE_CGROUP_DIR",
                    "MPLA_POC_ORACLE_BIN",
                    "MPLA_POC_EXECUTION_LEASE"
                ],
                "hv01_matched_control_environment": [
                    "MPLA_POC_CATALOG_BINDING_PATH",
                    "MPLA_POC_I2_CONTROL_INTERFACE"
                ],
                "hv01_required_control_interface_value": "m2-iface-v1-current-i2-closing-tpub",
                "single_case_filter_environment_supported": false,
                "required_lease_tokens": {
                    "prepare": "m2r-20260728T015724p0800:lead:PREPARE",
                    "HV-01": "m2r-20260728T015724p0800:lead:HV-01",
                    "HV-02": "m2r-20260728T015724p0800:lead:HV-02",
                    "HV-03": "m2r-20260728T015724p0800:lead:HV-03",
                    "HV-04": "m2r-20260728T015724p0800:lead:HV-04"
                },
                "hard_stop_seconds": {
                    "prepare": 7200,
                    "HV-01": 35,
                    "HV-02": 20,
                    "HV-03": 25,
                    "HV-04": 60
                }
            }),
        )
        .expect("write host contract evidence");
    }
}

#[test]
#[ignore = "requires lead-issued M2 physical execution lease"]
fn prepare_hv_01_through_hv_04() {
    prepare_all().expect("prepare exact heavy fixtures");
}

#[test]
#[ignore = "requires lead-issued M2 physical execution lease"]
fn hv_01_existing_size_independence() {
    run_hv01().expect("HV-01 physical case");
}

#[test]
#[ignore = "requires lead-issued M2 physical execution lease"]
fn hv_02_one_gib_upper_stream() {
    run_hv02().expect("HV-02 physical case");
}

#[test]
#[ignore = "requires lead-issued M2 physical execution lease"]
fn hv_03_honest_overlay_copy_up() {
    run_hv03().expect("HV-03 physical case");
}

#[test]
#[ignore = "requires lead-issued M2 physical execution lease"]
fn hv_04_exact_250k_files() {
    run_hv04().expect("HV-04 physical case");
}

fn prepare_all() -> TestResult {
    let roots = Roots::physical("PREPARE")?;
    let campaign = roots.campaign_root();
    if roots.preparation_path().exists() {
        return Err(format!(
            "immutable preparation receipt already exists at {}",
            roots.preparation_path().display()
        )
        .into());
    }
    fs::create_dir_all(&campaign)?;
    fs::create_dir_all(roots.arena_root())?;
    let mut hv01 = Vec::with_capacity(HV01_PREP_SIZES.len());
    for (index, existing_bytes) in HV01_PREP_SIZES.into_iter().enumerate() {
        let operation = OperationId::from_string(format!("hv01-prepare-{index}"));
        let allocation = create_allocation(&roots.arena_root(), &operation)?;
        write_dense_file(
            &allocation.upper_dir.join("immutable-existing.bin"),
            existing_bytes,
            u64::try_from(index)?,
        )?;
        for delta in 0..HV01_DELTA_FILES {
            sync_new_file(
                &allocation.upper_dir.join(format!("delta-{delta:02}.bin")),
                &[],
            )?;
        }
        sync_directory(&allocation.upper_dir)?;
        let prior = full_build(
            &roots,
            &allocation,
            &format!("hv01-prior-{index}"),
            &format!("hv01-candidate-{index}"),
            &campaign.join(format!("hv01-canonical-{index}")),
        )?;
        hv01.push(Hv01Fixture {
            existing_bytes,
            allocation_id: allocation.descriptor.allocation_id,
            prior: prior.into(),
        });
    }
    let mut hv02_allocation_ids = Vec::with_capacity(HV02_SAMPLES);
    for sample in 0..HV02_SAMPLES {
        let operation = OperationId::from_string(format!("hv02-prepare-{sample}"));
        let allocation = create_allocation(&roots.arena_root(), &operation)?;
        write_dense_file(
            &allocation.upper_dir.join("changed-upper.bin"),
            HV02_BYTES,
            u64::try_from(sample)? + 10,
        )?;
        sync_directory(&allocation.upper_dir)?;
        hv02_allocation_ids.push(allocation.descriptor.allocation_id);
    }
    let hv03_lower = campaign.join("hv03-lower");
    fs::create_dir(&hv03_lower)?;
    write_dense_file(&hv03_lower.join("lower-only.bin"), HV03_BYTES, 30)?;
    sync_directory(&hv03_lower)?;
    let hv03_allocation = create_allocation(
        &roots.arena_root(),
        &OperationId::from_string("hv03-prepare"),
    )?;
    let hv04_allocation = create_allocation(
        &roots.arena_root(),
        &OperationId::from_string("hv04-prepare"),
    )?;
    let hv04_receipt = sandbox_runtime_mpla_poc::populate_empty_fixture_root(
        &hv04_allocation.upper_dir,
        FixtureId::S3Small,
        FixtureTier::Heavy,
    )?;
    if hv04_receipt.observed_paths != HV04_FILES {
        return Err(format!(
            "S3-heavy produced {} paths instead of {HV04_FILES}",
            hv04_receipt.observed_paths
        )
        .into());
    }
    let preparation = Preparation {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        run_id: roots.run_id.clone(),
        hv01,
        hv02_allocation_ids,
        hv03_lower,
        hv03_allocation_id: hv03_allocation.descriptor.allocation_id,
        hv04_allocation_id: hv04_allocation.descriptor.allocation_id,
        hv04_receipt,
    };
    durable::replace_json(&roots.preparation_path(), &preparation)?;
    durable::replace_json(
        &roots
            .case_evidence("PREPARE")
            .join("preparation-summary.json"),
        &json!({
            "run_id": roots.run_id,
            "hv01_existing_bytes": HV01_SIZES,
            "hv01_prepared_existing_bytes": HV01_PREP_SIZES,
            "hv01_matched_one_gib_pairs": 3,
            "hv01_delta_bytes": HV01_DELTA_BYTES,
            "hv02_allocations": HV02_SAMPLES,
            "hv02_each_bytes": HV02_BYTES,
            "hv03_lower_bytes": HV03_BYTES,
            "hv04_exact_paths": preparation.hv04_receipt.observed_paths
        }),
    )?;
    Ok(())
}

fn load_preparation(roots: &Roots) -> TestResult<Preparation> {
    let preparation: Preparation = durable::read_json(&roots.preparation_path())?;
    if preparation.schema_version != SCHEMA_VERSION
        || preparation.interface_version != INTERFACE_VERSION
        || preparation.run_id != roots.run_id
        || preparation.hv01.len() != HV01_PREP_SIZES.len()
        || preparation.hv02_allocation_ids.len() != HV02_SAMPLES
        || preparation.hv04_receipt.observed_paths != HV04_FILES
    {
        return Err("heavy preparation receipt violates the frozen M2 contract".into());
    }
    for (fixture, expected) in preparation.hv01.iter().zip(HV01_PREP_SIZES) {
        if fixture.existing_bytes != expected {
            return Err("HV-01 preparation sizes are not exactly 1/5/9 GiB".into());
        }
    }
    Ok(preparation)
}

fn write_dense_file(path: &Path, bytes: u64, seed: u64) -> TestResult {
    if bytes == 0 || bytes % (32 * 1024) != 0 {
        return Err("dense fixture size must be a nonzero multiple of 32 KiB".into());
    }
    let mut file = BufWriter::with_capacity(
        32 * 1024,
        OpenOptions::new().create_new(true).write(true).open(path)?,
    );
    let mut block = [0_u8; 32 * 1024];
    let mut written = 0_u64;
    while written < bytes {
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = (seed
                .wrapping_add(written / 32_768)
                .wrapping_add(u64::try_from(index)?)
                % 251) as u8;
        }
        file.write_all(&block)?;
        written += u64::try_from(block.len())?;
    }
    file.flush()?;
    file.get_ref().sync_all()?;
    drop(file);
    let metadata = fs::metadata(path)?;
    if metadata.len() != bytes || metadata.blocks().saturating_mul(512) < bytes {
        return Err(format!(
            "fixture {} is not a real dense {bytes}-byte file: len={}, allocated={}",
            path.display(),
            metadata.len(),
            metadata.blocks().saturating_mul(512)
        )
        .into());
    }
    Ok(())
}

fn sync_new_file(path: &Path, contents: &[u8]) -> TestResult {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> TestResult {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn full_build(
    roots: &Roots,
    allocation: &AllocationHandle,
    label: &str,
    attribution_operation: &str,
    canonical: &Path,
) -> TestResult<SemanticBuildOutput> {
    fs::create_dir_all(canonical)?;
    let output = build_with_output(&SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string(label),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        sealed_tree: allocation.upper_dir.clone(),
        spool_dir: roots.campaign_root().join("spool").join(label),
        canonical_object_dir: canonical.to_path_buf(),
        attribution: attribution(attribution_operation),
    })?;
    enforce_semantic_limits(&output.resource_maxima)?;
    Ok(output)
}

fn attribution(operation: &str) -> AttributionInput {
    AttributionInput {
        actor_id: ACTOR_ID.to_owned(),
        semantic_operation_id: operation.to_owned(),
    }
}

fn enforce_semantic_limits(maxima: &SemanticResourceMaxima) -> TestResult {
    if maxima.application_pool_bytes != POOL_BYTES
        || maxima.peak_managed_bytes > POOL_BYTES
        || maxima.peak_open_data_fds > 16
        || maxima.peak_data_workers > 4
        || maxima.spool_run_bytes != 4 * 1024 * 1024
        || maxima.merge_fan_in != 8
    {
        return Err(format!("semantic resource envelope violated: {maxima:?}").into());
    }
    Ok(())
}

fn sample_resources(roots: &Roots, spool_root: &Path) -> TestResult<ResourceSample> {
    sample_resources_from(
        &SamplingRoots {
            cgroup_dir: &roots.cgroup_dir,
            payload_root: &roots.payload_root,
        },
        spool_root,
    )
}

fn sample_resources_from(
    roots: &SamplingRoots<'_>,
    spool_root: &Path,
) -> TestResult<ResourceSample> {
    let memory_stat = read_events(&roots.cgroup_dir.join("memory.stat"))?;
    let spool = tree_usage(spool_root)?;
    let queue = named_usage(spool_root, "directory.queue")?;
    let storage = filesystem_usage(roots.payload_root)?;
    Ok(ResourceSample {
        process_rss_bytes: process_rss_bytes()?,
        cgroup_memory_current_bytes: read_required_u64(&roots.cgroup_dir.join("memory.current"))?,
        cgroup_memory_peak_bytes: read_required_u64(&roots.cgroup_dir.join("memory.peak"))?,
        cgroup_kernel_bytes: *memory_stat.get("kernel").unwrap_or(&0),
        cgroup_slab_bytes: *memory_stat.get("slab").unwrap_or(&0),
        system_slab_bytes: system_slab_bytes().ok(),
        open_fds: fs::read_dir("/proc/self/fd")?.count() as u64,
        spool_logical_bytes: spool.0,
        spool_allocated_bytes: spool.1,
        spool_inodes: spool.2,
        queue_logical_bytes: queue.0,
        queue_allocated_bytes: queue.1,
        queue_inodes: queue.2,
        storage_allocated_blocks: storage.0,
        storage_fragment_bytes: storage.1,
        storage_used_inodes: storage.2,
    })
}

fn merge_maxima(maxima: &mut ResourceSample, sample: &ResourceSample) {
    maxima.process_rss_bytes = maxima.process_rss_bytes.max(sample.process_rss_bytes);
    maxima.cgroup_memory_current_bytes = maxima
        .cgroup_memory_current_bytes
        .max(sample.cgroup_memory_current_bytes);
    maxima.cgroup_memory_peak_bytes = maxima
        .cgroup_memory_peak_bytes
        .max(sample.cgroup_memory_peak_bytes);
    maxima.cgroup_kernel_bytes = maxima.cgroup_kernel_bytes.max(sample.cgroup_kernel_bytes);
    maxima.cgroup_slab_bytes = maxima.cgroup_slab_bytes.max(sample.cgroup_slab_bytes);
    maxima.system_slab_bytes = match (maxima.system_slab_bytes, sample.system_slab_bytes) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    };
    maxima.open_fds = maxima.open_fds.max(sample.open_fds);
    maxima.spool_logical_bytes = maxima.spool_logical_bytes.max(sample.spool_logical_bytes);
    maxima.spool_allocated_bytes = maxima
        .spool_allocated_bytes
        .max(sample.spool_allocated_bytes);
    maxima.spool_inodes = maxima.spool_inodes.max(sample.spool_inodes);
    maxima.queue_logical_bytes = maxima.queue_logical_bytes.max(sample.queue_logical_bytes);
    maxima.queue_allocated_bytes = maxima
        .queue_allocated_bytes
        .max(sample.queue_allocated_bytes);
    maxima.queue_inodes = maxima.queue_inodes.max(sample.queue_inodes);
    maxima.storage_allocated_blocks = maxima
        .storage_allocated_blocks
        .max(sample.storage_allocated_blocks);
    maxima.storage_fragment_bytes = maxima
        .storage_fragment_bytes
        .max(sample.storage_fragment_bytes);
    maxima.storage_used_inodes = maxima.storage_used_inodes.max(sample.storage_used_inodes);
}

fn run_hv01() -> TestResult {
    let roots = Roots::physical("HV-01")?;
    let preparation = load_preparation(&roots)?;
    let boundary = hv01_control_boundary();
    let catalog: Option<CatalogBinding> = if boundary.unknown_reason.is_none() {
        Some(durable::read_json(&required_path(
            "MPLA_POC_CATALOG_BINDING_PATH",
        )?)?)
    } else {
        None
    };
    let mut samples = Vec::with_capacity(preparation.hv01.len());
    for (index, fixture) in preparation.hv01.iter().enumerate() {
        let allocation = open_allocation(&roots.arena_root(), &fixture.allocation_id)?;
        let operation_id = OperationId::from_string(format!("hv01-candidate-{index}"));
        let publication_id = PublicationId::new();
        let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
        let empty_lower = ensure_empty_lower(&roots)?;
        let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
            &roots.control_root,
            allocation.clone(),
            lease.clone(),
            vec![empty_lower],
            Some(roots.cgroup_procs.clone()),
        )?;
        let workspace = session
            .workspace_root()
            .ok_or("HV-01 session has no mounted workspace")?
            .to_path_buf();
        let affected_paths = (0..HV01_DELTA_FILES)
            .map(|delta| PathBuf::from(format!("delta-{delta:02}.bin")))
            .collect::<Vec<_>>();
        let receipt_work = roots
            .campaign_root()
            .join("hv01-receipts")
            .join(index.to_string());
        fs::create_dir_all(&receipt_work)?;
        let before =
            capture_affected_paths(&workspace, &affected_paths, &receipt_work.join("before"))?;
        let edit = session.execute(
            &lease.writer,
            Path::new("/bin/sh"),
            &["-ceu".to_owned(), hv01_edit_script()],
            Duration::from_secs(10),
        )?;
        if !edit.success {
            return Err(format!("HV-01 edit failed: {edit:?}").into());
        }
        let after =
            capture_affected_paths(&workspace, &affected_paths, &receipt_work.join("after"))?;
        if after.payload_bytes_read != HV01_DELTA_BYTES {
            return Err(format!(
                "HV-01 selected snapshot read {} bytes instead of {HV01_DELTA_BYTES}",
                after.payload_bytes_read
            )
            .into());
        }
        let affected_stream = receipt_work.join("affected.records");
        let affected_stream_sha256 =
            write_affected_stream_from_snapshots(&affected_stream, &before, &after)?;
        let delta_source = receipt_work.join("control-delta");
        fs::create_dir(&delta_source)?;
        for path in &affected_paths {
            fs::copy(allocation.upper_dir.join(path), delta_source.join(path))?;
        }
        sync_tree_files(&delta_source)?;
        let control_changes =
            if fixture.existing_bytes == GIB && index < 3 && boundary.unknown_reason.is_none() {
                Some(collect_control_changes(
                    &delta_source,
                    &ControlCollectionLimits::default(),
                )?)
            } else {
                None
            };
        let storage_before = combined_storage_usage(&roots)?.1;
        let monitor = ResourceMonitor::start(
            &roots,
            roots
                .campaign_root()
                .join("spool")
                .join(format!("hv01-candidate-{index}")),
        )?;
        let started = Instant::now();
        let published = publish_incremental(
            &roots,
            &mut session,
            &allocation,
            IncrementalPublicationInput {
                operation_id: operation_id.clone(),
                publication_id: publication_id.clone(),
                seal_input: ReceiptHitSealInput {
                    schema_version: SCHEMA_VERSION,
                    affected_stream: affected_stream.clone(),
                    affected_stream_sha256: affected_stream_sha256.clone(),
                    affected_paths: affected_paths.clone(),
                },
                semantic_request: IncrementalBuildRequest {
                    schema_version: SCHEMA_VERSION,
                    operation_id: operation_id.clone(),
                    prior_manifest: fixture.prior.root_manifest_path.clone(),
                    expected_prior_roots: fixture.prior.receipt.roots.clone(),
                    expected_prior_record_stream_sha256: fixture
                        .prior
                        .receipt
                        .record_stream_sha256
                        .clone(),
                    affected_stream,
                    affected_stream_sha256,
                    affected_ranges_complete: true,
                    canonical_object_dir: roots
                        .campaign_root()
                        .join(format!("hv01-canonical-{index}")),
                    attribution: attribution(&format!("hv01-candidate-{index}")),
                },
                boundary_started: started,
            },
        )?;
        let resources = monitor.finish()?;
        validate_resource_observation(&resources, 128 * MIB)?;
        if published.semantic.immutable_payload_bytes_read != 0 {
            return Err("HV-01 read accumulated immutable payload".into());
        }
        let control = if let Some(changes) = control_changes.as_ref() {
            let state_root = roots
                .campaign_root()
                .join("hv01-control")
                .join(index.to_string());
            fs::create_dir_all(&state_root)?;
            Some(run_current_i2_closing(
                &CurrentI2ClosingRequest {
                    state_root,
                    publication_id: *Uuid::new_v4().as_bytes(),
                    public_root_hash: published.semantic.receipt.roots.root_id.as_str().to_owned(),
                    catalog_binding: catalog
                        .as_ref()
                        .ok_or("compatible HV-01 control has no catalog binding")?
                        .clone(),
                    boundary: boundary.clone(),
                },
                changes,
            )?)
        } else {
            None
        };
        let oracle = run_oracle(
            &roots,
            &allocation.upper_dir,
            &format!("hv01-candidate-{index}"),
        )?;
        let materialized_record_stream = materialize_record_stream(
            &published.semantic.root_manifest_path,
            &roots
                .campaign_root()
                .join(format!("hv01-canonical-{index}")),
        )?;
        compare_oracle(
            &published.semantic.receipt,
            &materialized_record_stream,
            &oracle,
        )?;
        let storage_after = combined_storage_usage(&roots)?.1;
        if !published.stationary.stationary.no_second_payload_allocation
            || storage_after.saturating_sub(storage_before) >= fixture.existing_bytes
        {
            return Err("HV-01 created an existing-state-sized second payload".into());
        }
        samples.push(json!({
            "existing_bytes": fixture.existing_bytes,
            "delta_bytes": HV01_DELTA_BYTES,
            "delta_files": HV01_DELTA_FILES,
            "candidate": published.timing,
            "semantic_phases": published.semantic.receipt.phase_spans,
            "immutable_payload_bytes_read": published.semantic.immutable_payload_bytes_read,
            "affected_input_bytes": published.semantic.affected_input_bytes,
            "resource_maxima": semantic_maxima_value(&published.semantic.resource_maxima),
            "observed_resources": resources,
            "oracle": oracle,
            "control": control,
            "no_second_copy": true,
            "storage_allocated_before": storage_before,
            "storage_allocated_after": storage_after,
            "storage_allocated_delta": storage_after.saturating_sub(storage_before),
            "selected_ref": published.selected_ref
        }));
        preserve_raw_samples(&roots, "HV-01", &samples)?;
    }
    let candidate_ns = samples
        .iter()
        .map(|sample| {
            sample["candidate"]["elapsed_ns"]
                .as_u64()
                .unwrap_or(u64::MAX)
        })
        .collect::<Vec<_>>();
    if candidate_ns.iter().any(|sample| *sample > 100_000_000) {
        return preserve_failure(
            &roots,
            "HV-01",
            &json!({"status":"FAIL","reason":"candidate exceeded 100 ms","samples":samples}),
        );
    }
    let controls = samples
        .iter()
        .filter_map(|sample| sample["control"]["span"]["elapsed_ns"].as_u64())
        .collect::<Vec<_>>();
    let one_gib_candidates = candidate_ns[..3].to_vec();
    let verdict = if boundary.unknown_reason.is_some() {
        "UNKNOWN"
    } else {
        if controls.len() != 3
            || median(&controls)? < median(&one_gib_candidates)?.saturating_mul(100)
        {
            return preserve_failure(
                &roots,
                "HV-01",
                &json!({"status":"FAIL","reason":"matched control median below required 100x","samples":samples}),
            );
        }
        "PASS"
    };
    let elapsed_max = *candidate_ns.iter().max().ok_or("no HV-01 samples")?;
    let one_gib_median = median(&one_gib_candidates)?;
    let candidate_size_slope_ns_per_gib = signed_slope_per_unit(candidate_ns[4], one_gib_median, 8);
    let stationary_ns = samples
        .iter()
        .map(|sample| {
            sample["candidate"]["stationary_ns"]
                .as_u64()
                .unwrap_or(u64::MAX)
        })
        .collect::<Vec<_>>();
    let stationary_size_slope_ns_per_gib =
        signed_slope_per_unit(stationary_ns[4], median(&stationary_ns[..3])?, 8);
    durable::replace_json(
        &roots.case_evidence("HV-01").join("result.json"),
        &json!({
            "id":"HV-01",
            "status": verdict,
            "timed_boundary":"immediately before admission close through response after paired-ref parent fsync",
            "samples": samples,
            "candidate_median_ns": median(&candidate_ns)?,
            "candidate_max_ns": elapsed_max,
            "candidate_size_slope_ns_per_gib": candidate_size_slope_ns_per_gib,
            "stationary_size_slope_ns_per_gib": stationary_size_slope_ns_per_gib,
            "size_slope_points_existing_bytes": HV01_SIZES,
            "zero_existing_payload_work": true,
            "control_median_ns": if controls.len() == 3 { Some(median(&controls)?) } else { None },
            "preferred_20ms": candidate_ns.iter().all(|value| *value <= 20_000_000),
            "preferred_500x": controls.len() == 3 && median(&controls)? >= median(&one_gib_candidates)?.saturating_mul(500),
            "control_boundary": boundary,
            "lead_integration_dependency":"582f82b63"
        }),
    )?;
    Ok(())
}

fn run_hv02() -> TestResult {
    let roots = Roots::physical("HV-02")?;
    let preparation = load_preparation(&roots)?;
    let order = [
        "candidate",
        "comparator",
        "comparator",
        "candidate",
        "candidate",
        "comparator",
    ];
    let mut samples = Vec::with_capacity(HV02_SAMPLES);
    for (index, allocation_id) in preparation.hv02_allocation_ids.iter().enumerate() {
        let allocation = open_allocation(&roots.arena_root(), allocation_id)?;
        assert_dense_file(&allocation.upper_dir.join("changed-upper.bin"), HV02_BYTES)?;
        let baseline_usage = combined_storage_usage(&roots)?;
        let operation_id = OperationId::from_string(format!("hv02-{index}"));
        let publication_id = PublicationId::new();
        let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
        let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
            &roots.control_root,
            allocation.clone(),
            lease,
            vec![ensure_empty_lower(&roots)?],
            Some(roots.cgroup_procs.clone()),
        )?;
        let spool = roots
            .campaign_root()
            .join("spool")
            .join(format!("hv02-{index}"));
        let monitor = ResourceMonitor::start(&roots, spool)?;
        let started = Instant::now();
        let published = publish_full(
            &roots,
            &mut session,
            &allocation,
            operation_id,
            publication_id,
            &format!("hv02-{index}"),
            started,
        )?;
        let resources = monitor.finish()?;
        validate_resource_observation(&resources, 128 * MIB)?;
        if published.semantic.receipt.bytes_read != HV02_BYTES {
            return Err(format!(
                "HV-02 streamed {} bytes instead of {HV02_BYTES}",
                published.semantic.receipt.bytes_read
            )
            .into());
        }
        let oracle = run_oracle(&roots, &allocation.upper_dir, &format!("hv02-{index}"))?;
        compare_oracle(
            &published.semantic.receipt,
            &published.semantic.record_stream_path,
            &oracle,
        )?;
        let final_usage = combined_storage_usage(&roots)?;
        if !published.stationary.no_second_payload_allocation
            || final_usage.1.saturating_sub(baseline_usage.1) >= HV02_BYTES
        {
            return Err("HV-02 created a second 1-GiB payload".into());
        }
        samples.push(json!({
            "role": order[index],
            "stream_bytes": HV02_BYTES,
            "timing": published.timing,
            "throughput_bytes_per_second": throughput(HV02_BYTES, published.timing.elapsed_ns),
            "semantic_phases": published.semantic.receipt.phase_spans,
            "semantic_resources": semantic_maxima_value(&published.semantic.resource_maxima),
            "observed_resources": resources,
            "oracle": oracle,
            "no_second_copy": true,
            "storage_allocated_before": baseline_usage.1,
            "storage_allocated_after": final_usage.1,
            "storage_allocated_delta": final_usage.1.saturating_sub(baseline_usage.1),
            "selected_ref": published.selected_ref
        }));
        preserve_raw_samples(&roots, "HV-02", &samples)?;
    }
    let candidate = role_samples(&samples, "candidate")?;
    let comparator = role_samples(&samples, "comparator")?;
    let candidate_median = median(&candidate)?;
    let comparator_median = median(&comparator)?;
    let candidate_throughput = throughput(HV02_BYTES, candidate_median);
    let comparator_throughput = throughput(HV02_BYTES, comparator_median);
    let throughput_ratio_basis_points =
        candidate_throughput.saturating_mul(10_000) / comparator_throughput.max(1);
    if throughput_ratio_basis_points < 9_500 {
        return preserve_failure(
            &roots,
            "HV-02",
            &json!({"status":"FAIL","reason":"candidate median throughput regressed more than 5%","samples":samples}),
        );
    }
    if candidate_throughput < GIB {
        return preserve_failure(
            &roots,
            "HV-02",
            &json!({"status":"FAIL","reason":"candidate median throughput is below 1 GiB/s","samples":samples}),
        );
    }
    let stream_campaign_ns = candidate
        .iter()
        .chain(comparator.iter())
        .copied()
        .sum::<u64>();
    if stream_campaign_ns > 15_000_000_000 {
        return preserve_failure(
            &roots,
            "HV-02",
            &json!({"status":"FAIL","reason":"six measured streams exceeded the reserved 15 seconds","samples":samples}),
        );
    }
    durable::replace_json(
        &roots.case_evidence("HV-02").join("result.json"),
        &json!({
            "id":"HV-02",
            "status":"PASS",
            "timed_boundary":"immediately before admission close through response after paired-ref parent fsync",
            "samples":samples,
            "candidate_median_ns":candidate_median,
            "comparator_median_ns":comparator_median,
            "candidate_maximum_ns":candidate.iter().copied().max(),
            "comparator_maximum_ns":comparator.iter().copied().max(),
            "overall_maximum_ns":candidate.iter().chain(comparator.iter()).copied().max(),
            "stream_campaign_ns":stream_campaign_ns,
            "candidate_median_throughput_bytes_per_second":candidate_throughput,
            "comparator_median_throughput_bytes_per_second":comparator_throughput,
            "throughput_ratio_basis_points":throughput_ratio_basis_points,
            "throughput_regression_basis_points":10_000_u64.saturating_sub(throughput_ratio_basis_points),
            "required_throughput_met":candidate_throughput >= GIB,
            "preferred_throughput_met":candidate_throughput >= 5 * GIB,
            "lead_integration_dependency":"582f82b63"
        }),
    )?;
    Ok(())
}

fn run_hv03() -> TestResult {
    let roots = Roots::physical("HV-03")?;
    let preparation = load_preparation(&roots)?;
    assert_dense_file(&preparation.hv03_lower.join("lower-only.bin"), HV03_BYTES)?;
    let allocation = open_allocation(&roots.arena_root(), &preparation.hv03_allocation_id)?;
    let operation_id = OperationId::from_string("hv03-candidate");
    let publication_id = PublicationId::new();
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &roots.control_root,
        allocation.clone(),
        lease.clone(),
        vec![preparation.hv03_lower.clone()],
        Some(roots.cgroup_procs.clone()),
    )?;
    let monitor = ResourceMonitor::start(
        &roots,
        roots.campaign_root().join("spool").join("hv03-candidate"),
    )?;
    let storage_before_copyup = combined_storage_usage(&roots)?;
    let boundary_started = Instant::now();
    let write = session.execute(
        &lease.writer,
        Path::new("/bin/sh"),
        &[
            "-ceu".to_owned(),
            "dd if=/dev/zero of=lower-only.bin bs=4096 count=1 conv=notrunc status=none; sync -f lower-only.bin"
                .to_owned(),
        ],
        Duration::from_secs(20),
    )?;
    if !write.success {
        return Err(format!("HV-03 first 4-KiB write failed: {write:?}").into());
    }
    assert_dense_file(&allocation.upper_dir.join("lower-only.bin"), HV03_BYTES)?;
    let upper_after_copyup = tree_usage(&allocation.upper_dir)?;
    let storage_after_copyup = combined_storage_usage(&roots)?;
    let published = publish_full(
        &roots,
        &mut session,
        &allocation,
        operation_id,
        publication_id,
        "hv03-candidate",
        boundary_started,
    )?;
    let resources = monitor.finish()?;
    validate_resource_observation(&resources, HV03_BYTES + 128 * MIB)?;
    if published.timing.elapsed_ns > 25_000_000_000 {
        return preserve_failure(
            &roots,
            "HV-03",
            &json!({"status":"FAIL","reason":"Tcopyup+pub exceeded 25 seconds","timing":published.timing}),
        );
    }
    if published.semantic.receipt.bytes_read != HV03_BYTES {
        return Err("HV-03 semantic scan did not stream the complete copied-up file".into());
    }
    let final_upper = tree_usage(&allocation.upper_dir)?;
    let storage_after_publication = combined_storage_usage(&roots)?;
    if upper_after_copyup.1 < HV03_BYTES
        || final_upper.1.saturating_sub(upper_after_copyup.1) >= HV03_BYTES
        || storage_after_publication
            .1
            .saturating_sub(storage_after_copyup.1)
            >= HV03_BYTES
        || !published.stationary.no_second_payload_allocation
    {
        return Err("HV-03 copy-up accounting indicates an extra payload-sized twin".into());
    }
    let oracle = run_oracle(&roots, &allocation.upper_dir, "hv03-candidate")?;
    compare_oracle(
        &published.semantic.receipt,
        &published.semantic.record_stream_path,
        &oracle,
    )?;
    durable::replace_json(
        &roots.case_evidence("HV-03").join("result.json"),
        &json!({
            "id":"HV-03",
            "status":"PASS",
            "fixture":{"lower_only_dense_bytes":HV03_BYTES,"first_write_bytes":4096},
            "timed_boundary":"immediately before the first 4-KiB write syscall through response after paired-ref parent fsync",
            "timing":published.timing,
            "semantic_phases":published.semantic.receipt.phase_spans,
            "semantic_resources":semantic_maxima_value(&published.semantic.resource_maxima),
            "observed_resources":resources,
            "upper_allocated_after_copyup":upper_after_copyup.1,
            "upper_allocated_after_publication":final_upper.1,
            "storage_allocated_before_copyup":storage_before_copyup.1,
            "storage_allocated_after_copyup":storage_after_copyup.1,
            "storage_allocated_after_publication":storage_after_publication.1,
            "honest_copyup_allocated_delta":storage_after_copyup.1.saturating_sub(storage_before_copyup.1),
            "publication_allocated_delta":storage_after_publication.1.saturating_sub(storage_after_copyup.1),
            "oracle":oracle,
            "no_second_copy":true,
            "selected_ref":published.selected_ref,
            "lead_integration_dependency":"582f82b63"
        }),
    )?;
    Ok(())
}

fn run_hv04() -> TestResult {
    let roots = Roots::physical("HV-04")?;
    let preparation = load_preparation(&roots)?;
    if preparation.hv04_receipt.observed_paths != HV04_FILES {
        return Err("HV-04 fixture is not exactly 250,000 paths".into());
    }
    let allocation = open_allocation(&roots.arena_root(), &preparation.hv04_allocation_id)?;
    let operation_id = OperationId::from_string("hv04-candidate");
    let publication_id = PublicationId::new();
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &roots.control_root,
        allocation.clone(),
        lease,
        vec![ensure_empty_lower(&roots)?],
        Some(roots.cgroup_procs.clone()),
    )?;
    let spool = roots.campaign_root().join("spool").join("hv04-candidate");
    let monitor = ResourceMonitor::start(&roots, spool)?;
    let started = Instant::now();
    let published = publish_full(
        &roots,
        &mut session,
        &allocation,
        operation_id,
        publication_id,
        "hv04-candidate",
        started,
    )?;
    let resources = monitor.finish()?;
    validate_resource_observation(&resources, 128 * MIB)?;
    let rss_limit = CGROUP_HIGH_BYTES.min(
        resources
            .baseline
            .process_rss_bytes
            .saturating_add(32 * MIB),
    );
    if resources.maxima.process_rss_bytes > rss_limit
        || published.timing.elapsed_ns > 60_000_000_000
    {
        return preserve_failure(
            &roots,
            "HV-04",
            &json!({
                "status":"FAIL",
                "reason":"250k case exceeded RSS or 60-second gate",
                "rss_limit":rss_limit,
                "timing":published.timing,
                "resources":resources
            }),
        );
    }
    let oracle = run_oracle(&roots, &allocation.upper_dir, "hv04-candidate")?;
    compare_oracle(
        &published.semantic.receipt,
        &published.semantic.record_stream_path,
        &oracle,
    )?;
    if !published.stationary.no_second_payload_allocation {
        return Err("HV-04 stationary publication created a second payload allocation".into());
    }
    durable::replace_json(
        &roots.case_evidence("HV-04").join("result.json"),
        &json!({
            "id":"HV-04",
            "status":"PASS",
            "fixture":{
                "exact_paths":preparation.hv04_receipt.observed_paths,
                "regular_files":preparation.hv04_receipt.regular_files,
                "directories":preparation.hv04_receipt.directories,
                "logical_bytes":preparation.hv04_receipt.logical_bytes,
                "allocated_bytes":preparation.hv04_receipt.allocated_bytes,
                "unique_inodes":preparation.hv04_receipt.unique_inodes
            },
            "timed_boundary":"immediately before admission close through response after paired-ref parent fsync",
            "timing":published.timing,
            "semantic_phases":published.semantic.receipt.phase_spans,
            "semantic_resources":semantic_maxima_value(&published.semantic.resource_maxima),
            "observed_resources":resources,
            "rss_limit_bytes":rss_limit,
            "oracle":oracle,
            "all_semantic_entries_compared":true,
            "no_second_copy":true,
            "selected_ref":published.selected_ref,
            "lead_integration_dependency":"582f82b63"
        }),
    )?;
    Ok(())
}

fn publish_full(
    roots: &Roots,
    session: &mut sandbox_runtime_mpla_poc::MplaSession,
    allocation: &AllocationHandle,
    operation_id: OperationId,
    publication_id: PublicationId,
    label: &str,
    boundary_started: Instant,
) -> TestResult<Published> {
    let request = StationaryPublicationRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
    };
    let operations = roots.control_root.join("operations");
    let semantic_request = SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        sealed_tree: allocation.upper_dir.clone(),
        spool_dir: roots.campaign_root().join("spool").join(label),
        canonical_object_dir: roots.campaign_root().join("canonical").join(label),
        attribution: attribution(label),
    };
    let (stationary, semantic, stationary_ns, semantic_ns) = std::thread::scope(|scope| {
        let stationary_task = scope.spawn(move || {
            let started = Instant::now();
            let receipt = stationary_adopt(
                session,
                &request,
                &operations,
                &mut FaultInjector::default(),
            )?;
            Ok::<_, sandbox_runtime_mpla_poc::PocError>((receipt, elapsed_ns(started)))
        });
        let semantic_task = scope.spawn(move || {
            let started = Instant::now();
            let output = build_with_output(&semantic_request)?;
            Ok::<_, sandbox_runtime_mpla_poc::PocError>((output, elapsed_ns(started)))
        });
        let stationary = stationary_task
            .join()
            .map_err(|_| "stationary publication thread panicked")??;
        let semantic = semantic_task
            .join()
            .map_err(|_| "semantic build thread panicked")??;
        Ok::<_, Box<dyn Error + Send + Sync>>((stationary.0, semantic.0, stationary.1, semantic.1))
    })?;
    enforce_semantic_limits(&semantic.resource_maxima)?;
    let ref_started = Instant::now();
    let selected_ref = install_ref(
        roots,
        allocation,
        &semantic.receipt,
        stationary.adoption.new_owner.owner_epoch,
        stationary.stable.after.allocated_bytes.max(1),
        &operation_id,
        &publication_id,
    )?;
    let locator_and_ref_ns = elapsed_ns(ref_started);
    Ok(Published {
        stationary,
        semantic,
        selected_ref,
        timing: PublicationTiming {
            boundary: "Tpub".to_owned(),
            elapsed_ns: elapsed_ns(boundary_started),
            stationary_ns,
            semantic_ns,
            locator_and_ref_ns,
        },
    })
}

fn publish_incremental(
    roots: &Roots,
    session: &mut sandbox_runtime_mpla_poc::MplaSession,
    allocation: &AllocationHandle,
    input: IncrementalPublicationInput,
) -> TestResult<IncrementallyPublished> {
    let request = StationaryPublicationRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: input.operation_id.clone(),
        publication_id: input.publication_id.clone(),
    };
    let operations = roots.control_root.join("operations");
    let seal_input = input.seal_input;
    let incremental_request = input.semantic_request;
    let (stationary, semantic, stationary_ns, semantic_ns) = std::thread::scope(|scope| {
        let stationary_task = scope.spawn(move || {
            let started = Instant::now();
            let receipt = stationary_adopt_receipt_hit(
                session,
                &request,
                &operations,
                &seal_input,
                &mut FaultInjector::default(),
            )?;
            Ok::<_, sandbox_runtime_mpla_poc::PocError>((receipt, elapsed_ns(started)))
        });
        let semantic_task = scope.spawn(move || {
            let started = Instant::now();
            let output = build_incremental(&incremental_request)?;
            Ok::<_, sandbox_runtime_mpla_poc::PocError>((output, elapsed_ns(started)))
        });
        let stationary = stationary_task
            .join()
            .map_err(|_| "receipt-hit publication thread panicked")??;
        let semantic = semantic_task
            .join()
            .map_err(|_| "incremental semantic thread panicked")??;
        Ok::<_, Box<dyn Error + Send + Sync>>((stationary.0, semantic.0, stationary.1, semantic.1))
    })?;
    enforce_semantic_limits(&semantic.resource_maxima)?;
    let ref_started = Instant::now();
    let selected_ref = install_ref(
        roots,
        allocation,
        &semantic.receipt,
        stationary.stationary.adoption.new_owner.owner_epoch,
        stationary.stationary.stable.after.allocated_bytes.max(1),
        &input.operation_id,
        &input.publication_id,
    )?;
    let locator_and_ref_ns = elapsed_ns(ref_started);
    Ok(IncrementallyPublished {
        stationary,
        semantic,
        selected_ref,
        timing: PublicationTiming {
            boundary: "Tpub".to_owned(),
            elapsed_ns: elapsed_ns(input.boundary_started),
            stationary_ns,
            semantic_ns,
            locator_and_ref_ns,
        },
    })
}

fn install_ref(
    roots: &Roots,
    allocation: &AllocationHandle,
    semantic: &SemanticBuildReceipt,
    owner_epoch: u64,
    accounted_bytes: u64,
    operation_id: &OperationId,
    publication_id: &PublicationId,
) -> TestResult<PairedRefValue> {
    let locator_store = LocatorStore::open(roots.campaign_root().join("locators"))?;
    let ref_store = PairedRefStore::open(roots.campaign_root().join("refs"))?;
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
    match ref_store.commit(
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
    )? {
        RefCommitOutcome::Committed(receipt) if receipt.parent_directory_synced => {
            Ok(receipt.value)
        }
        RefCommitOutcome::Committed(_) => {
            Err("paired-ref response did not prove parent-directory fsync".into())
        }
        RefCommitOutcome::ExpectedParent { expected, observed } => Err(format!(
            "paired-ref parent conflict: expected {expected}, observed {observed}"
        )
        .into()),
    }
}

fn install_locator(
    store: &LocatorStore,
    allocation: &AllocationHandle,
    semantic: &SemanticBuildReceipt,
    owner_epoch: u64,
    accounted_bytes: u64,
    operation_id: &OperationId,
    publication_id: &PublicationId,
) -> TestResult<LocatorDurabilityReceipt> {
    let payload_root = PayloadRootId::parse(semantic.roots.root_id.as_str())?;
    for attempt in 0..64_u8 {
        let selected = store.selected()?;
        let delta = LocatorDelta {
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
        };
        match store.install(&delta, &mut NamedFaultInjector::default()) {
            Ok(receipt) => return Ok(receipt),
            Err(sandbox_runtime_mpla_poc::PocError::OwnerConflict(message))
                if attempt < 63 && message.starts_with("locator expected parent ") => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("locator compare-and-install retry bound exhausted".into())
}

fn run_oracle(roots: &Roots, tree: &Path, operation: &str) -> TestResult<OracleSummary> {
    let records_root = roots.evidence_root.join("oracle-records");
    fs::create_dir_all(&records_root)?;
    let records = records_root.join(format!("{operation}.records"));
    let output = Command::new(&roots.oracle_bin)
        .args([
            "--tree",
            &tree.to_string_lossy(),
            "--records",
            &records.to_string_lossy(),
            "--actor-id",
            ACTOR_ID,
            "--semantic-operation-id",
            operation,
        ])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "independent oracle failed: status={:?}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn compare_oracle(
    semantic: &SemanticBuildReceipt,
    candidate_stream: &Path,
    oracle: &OracleSummary,
) -> TestResult {
    if semantic.roots.root_id.as_str() != oracle.root_id
        || semantic.roots.attribution_root_id.as_str() != oracle.attribution_root_id
        || semantic.record_stream_sha256 != oracle.record_stream_sha256
        || semantic.entry_count != oracle.entry_count
    {
        return Err(format!(
            "candidate/oracle mismatch: candidate={:?}, oracle={oracle:?}",
            semantic.roots
        )
        .into());
    }
    compare_files_streaming(candidate_stream, Path::new(&oracle.record_stream_path))
}

fn compare_files_streaming(left: &Path, right: &Path) -> TestResult {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Err("candidate/oracle record stream lengths differ".into());
    }
    let mut left = BufReader::with_capacity(32 * 1024, File::open(left)?);
    let mut right = BufReader::with_capacity(32 * 1024, File::open(right)?);
    let mut left_block = [0_u8; 32 * 1024];
    let mut right_block = [0_u8; 32 * 1024];
    loop {
        let left_read = left.read(&mut left_block)?;
        let right_read = right.read(&mut right_block)?;
        if left_read != right_read || left_block[..left_read] != right_block[..right_read] {
            return Err("candidate/oracle record streams differ".into());
        }
        if left_read == 0 {
            return Ok(());
        }
    }
}

fn hv01_control_boundary() -> ControlBoundary {
    let observed = std::env::var("MPLA_POC_I2_CONTROL_INTERFACE").ok();
    let compatible = observed.as_deref() == Some("m2-iface-v1-current-i2-closing-tpub");
    ControlBoundary {
        candidate_start: "immediately before admission close".to_owned(),
        candidate_stop: "response after paired-ref parent fsync".to_owned(),
        current_i2_start: "immediately before current-I2 closing publication call".to_owned(),
        current_i2_stop: "return after current-I2 hidden publication durability".to_owned(),
        same_fixture: true,
        same_intent: true,
        same_durability: compatible,
        same_readiness: true,
        cache_state: ControlCacheMatch::NotApplicable,
        unknown_reason: (!compatible).then(|| {
            format!(
                "lead-provided current I2 control interface is absent or incompatible: {observed:?}"
            )
        }),
    }
}

fn validate_resource_observation(
    observation: &ResourceObservation,
    storage_delta_limit: u64,
) -> TestResult {
    let rss_limit = CGROUP_HIGH_BYTES.min(
        observation
            .baseline
            .process_rss_bytes
            .saturating_add(32 * MIB),
    );
    if observation.oom_after != observation.oom_before
        || observation.oom_kill_after != observation.oom_kill_before
        || observation.maxima.process_rss_bytes > rss_limit
        || observation.maxima.cgroup_memory_current_bytes > CGROUP_HIGH_BYTES
        || observation.maxima.cgroup_memory_peak_bytes >= CGROUP_MAX_BYTES
    {
        return Err(format!("cgroup memory/OOM envelope violated: {observation:?}").into());
    }
    let block_delta = observation
        .maxima
        .storage_allocated_blocks
        .saturating_sub(observation.baseline.storage_allocated_blocks);
    let block_bytes = block_delta.saturating_mul(
        observation
            .maxima
            .storage_fragment_bytes
            .max(observation.baseline.storage_fragment_bytes),
    );
    if block_bytes > storage_delta_limit {
        return Err(format!(
            "publication storage-domain delta {block_bytes} exceeds {storage_delta_limit}"
        )
        .into());
    }
    Ok(())
}

fn semantic_maxima_value(maxima: &SemanticResourceMaxima) -> Value {
    json!({
        "application_pool_bytes":maxima.application_pool_bytes,
        "peak_managed_bytes":maxima.peak_managed_bytes,
        "scan_window_bytes":maxima.scan_window_bytes,
        "spool_run_bytes":maxima.spool_run_bytes,
        "merge_fan_in":maxima.merge_fan_in,
        "peak_open_data_fds":maxima.peak_open_data_fds,
        "peak_data_workers":maxima.peak_data_workers,
        "trie_fan_out":maxima.trie_fan_out
    })
}

fn preserve_failure(roots: &Roots, case: &str, evidence: &Value) -> TestResult {
    durable::replace_json(&roots.case_evidence(case).join("result.json"), evidence)?;
    Err(format!("{case} failed; raw result preserved").into())
}

fn preserve_raw_samples(roots: &Roots, case: &str, samples: &[Value]) -> TestResult {
    durable::replace_json(
        &roots.case_evidence(case).join("raw-samples.json"),
        &json!({"id":case,"samples":samples}),
    )?;
    Ok(())
}

fn required_path(name: &str) -> TestResult<PathBuf> {
    let value = required_string(name)?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!("{name} must be an absolute path").into());
    }
    Ok(path)
}

fn required_string(name: &str) -> TestResult<String> {
    let value = std::env::var(name).map_err(|_| format!("{name} is required"))?;
    if value.trim().is_empty() {
        return Err(format!("{name} cannot be empty").into());
    }
    Ok(value)
}

fn read_limit(path: &Path) -> TestResult<Option<u64>> {
    let value = fs::read_to_string(path)?;
    let value = value.trim();
    if value == "max" {
        Ok(None)
    } else {
        Ok(Some(value.parse()?))
    }
}

fn read_required_u64(path: &Path) -> TestResult<u64> {
    let value = fs::read_to_string(path)?;
    Ok(value.trim().parse()?)
}

fn read_events(path: &Path) -> TestResult<std::collections::BTreeMap<String, u64>> {
    let mut values = std::collections::BTreeMap::new();
    for line in fs::read_to_string(path)?.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(value) = fields.next() else {
            continue;
        };
        values.insert(name.to_owned(), value.parse()?);
    }
    Ok(values)
}

fn process_rss_bytes() -> TestResult<u64> {
    for line in fs::read_to_string("/proc/self/status")?.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            let kib: u64 = value
                .split_ascii_whitespace()
                .next()
                .ok_or("VmRSS has no value")?
                .parse()?;
            return Ok(kib.saturating_mul(1024));
        }
    }
    Err("VmRSS is absent from /proc/self/status".into())
}

fn system_slab_bytes() -> TestResult<u64> {
    for line in fs::read_to_string("/proc/meminfo")?.lines() {
        if let Some(value) = line.strip_prefix("Slab:") {
            let kib: u64 = value
                .split_ascii_whitespace()
                .next()
                .ok_or("Slab has no value")?
                .parse()?;
            return Ok(kib.saturating_mul(1024));
        }
    }
    Err("Slab is absent from /proc/meminfo".into())
}

fn filesystem_usage(path: &Path) -> TestResult<(u64, u64, u64)> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a live NUL-terminated CString and `stats` points to writable storage.
    let status = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
    if status != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: a zero return from `statvfs` initialized the complete output structure.
    let stats = unsafe { stats.assume_init() };
    let used_blocks = stats.f_blocks.saturating_sub(stats.f_bfree);
    let used_inodes = stats.f_files.saturating_sub(stats.f_ffree);
    Ok((
        widen_u64(used_blocks),
        widen_u64(stats.f_frsize),
        widen_u64(used_inodes),
    ))
}

fn widen_u64<T: Into<u64>>(value: T) -> u64 {
    value.into()
}

fn tree_usage(root: &Path) -> TestResult<(u64, u64, u64)> {
    if !root.exists() {
        return Ok((0, 0, 0));
    }
    let mut logical = 0_u64;
    let mut allocated = 0_u64;
    let mut inodes = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        logical = logical.saturating_add(metadata.len());
        allocated = allocated.saturating_add(metadata.blocks().saturating_mul(512));
        inodes = inodes.saturating_add(1);
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok((logical, allocated, inodes))
}

fn combined_storage_usage(roots: &Roots) -> TestResult<(u64, u64, u64)> {
    let payload = tree_usage(&roots.payload_root)?;
    let control = tree_usage(&roots.campaign_root())?;
    Ok((
        payload.0.saturating_add(control.0),
        payload.1.saturating_add(control.1),
        payload.2.saturating_add(control.2),
    ))
}

fn named_usage(root: &Path, file_name: &str) -> TestResult<(u64, u64, u64)> {
    if !root.exists() {
        return Ok((0, 0, 0));
    }
    let mut logical = 0_u64;
    let mut allocated = 0_u64;
    let mut inodes = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else if path.file_name().and_then(|name| name.to_str()) == Some(file_name) {
            logical = logical.saturating_add(metadata.len());
            allocated = allocated.saturating_add(metadata.blocks().saturating_mul(512));
            inodes = inodes.saturating_add(1);
        }
    }
    Ok((logical, allocated, inodes))
}

fn ensure_empty_lower(roots: &Roots) -> TestResult<PathBuf> {
    let lower = roots.campaign_root().join("empty-lower");
    match fs::create_dir(&lower) {
        Ok(()) => sync_directory(&roots.campaign_root())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read_dir(&lower)?.next().is_some() {
                return Err("shared empty lower is not empty".into());
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(lower)
}

fn sync_tree_files(root: &Path) -> TestResult {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            File::open(entry.path())?.sync_all()?;
        }
    }
    sync_directory(root)
}

fn assert_dense_file(path: &Path, expected: u64) -> TestResult {
    let metadata = fs::metadata(path)?;
    let allocated = metadata.blocks().saturating_mul(512);
    if metadata.len() != expected || allocated < expected {
        return Err(format!(
            "{} is not a dense {expected}-byte file: len={}, allocated={allocated}",
            path.display(),
            metadata.len()
        )
        .into());
    }
    Ok(())
}

fn hv01_edit_script() -> String {
    let mut script = String::new();
    for index in 0..HV01_DELTA_FILES {
        let bytes = HV01_DELTA_BYTES / HV01_DELTA_FILES
            + u64::from(index < HV01_DELTA_BYTES % HV01_DELTA_FILES);
        script.push_str(&format!(
            "head -c {bytes} /dev/zero > delta-{index:02}.bin; sync -f delta-{index:02}.bin; "
        ));
    }
    script
}

fn median(values: &[u64]) -> TestResult<u64> {
    if values.is_empty() {
        return Err("median requires at least one sample".into());
    }
    let mut values = values.to_vec();
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        Ok(values[middle - 1].saturating_add(values[middle]) / 2)
    } else {
        Ok(values[middle])
    }
}

fn role_samples(samples: &[Value], role: &str) -> TestResult<Vec<u64>> {
    let values = samples
        .iter()
        .filter(|sample| sample["role"].as_str() == Some(role))
        .map(|sample| {
            sample["timing"]["elapsed_ns"]
                .as_u64()
                .ok_or_else(|| format!("{role} sample has no elapsed_ns"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() != 3 {
        return Err(format!("{role} requires exactly three interleaved samples").into());
    }
    Ok(values)
}

fn throughput(bytes: u64, elapsed_ns: u64) -> u64 {
    bytes.saturating_mul(1_000_000_000) / elapsed_ns.max(1)
}

fn signed_slope_per_unit(end: u64, start: u64, units: i64) -> i64 {
    let end = i64::try_from(end).unwrap_or(i64::MAX);
    let start = i64::try_from(start).unwrap_or(i64::MAX);
    end.saturating_sub(start) / units
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
