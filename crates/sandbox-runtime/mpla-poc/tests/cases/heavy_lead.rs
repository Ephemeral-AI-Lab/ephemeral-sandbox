#![allow(dead_code)]

// The lead heavy rows deliberately reuse the already-falsified smoke mechanisms
// instead of introducing a second publication/activation implementation.
include!("smoke.rs");

use std::io::{Seek, SeekFrom};

use sandbox_runtime_mpla_poc::controls::{
    collect_control_changes, run_current_i2_closing, run_current_i2_materialization,
    ControlBoundary, ControlCacheExpectation, ControlCacheMatch, ControlCollectionLimits,
    ControlIntent, ControlOperationReceipt, CurrentI2ClosingRequest,
    CurrentI2MaterializationRequest, ExternalReadinessReceipt,
};
use sandbox_runtime_mpla_poc::evacuation::{
    EvacuationPhase, EvacuationRequest, EvacuationStore, StageFiveRetirementAuthorization,
};
use sandbox_runtime_mpla_poc::locator::LocatorReplacement;
use sandbox_runtime_mpla_poc::recovery::{
    hv07_fault_expectations, CrashExecutionMode, CrashRecoveryObservation, CrashSweepLedger,
    DurableCrashWitness, PhysicalKillWitness, RealOperationWitness, RecoveryReplayWitness,
    SelectedVisibility,
};
use sandbox_runtime_mpla_poc::{AdmissionTier, PhysicalFaultMarker};

const HEAVY_GIB: u64 = 1024 * 1024 * 1024;
const HEAVY_MIB: u64 = 1024 * 1024;
const HV05_BASE_BYTES: u64 = 5 * HEAVY_GIB;
const HV05_DELTAS: usize = 64;
const HV05_FILES_PER_DELTA: usize = 10;
const HV05_FILE_BYTES: u64 = 100 * 1024;
const HV05_TPUB_MAX_NS: u64 = 1_070_000_000;
const HV05_MEDIAN_OBJECTIVE_NS: u64 = 100_000_000;
const HV07_CHILD_STDERR_LIMIT_BYTES: u64 = 32 * 1024;
const HEAVY_LEASE_PREFIX: &str = "m2r-20260728T015724p0800:lead:";
const HEAVY_BRANCH: &str = "heavy-main";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct HeavyPreparation {
    schema_version: u32,
    interface_version: String,
    run_id: RunId,
    hv05_base_allocation_id: AllocationId,
    hv05_base_semantic: PreparedSemantic,
    hv05_base_ref: PairedRefValue,
    hv05_base_owner_epoch: u64,
    r0_allocation_id: AllocationId,
    r0_source_root: PathBuf,
    r0_transfer_elapsed_ns: u64,
    r0_profile: TreeProfile,
    hv09_source_allocation_id: AllocationId,
    hv09_source_owner_epoch: u64,
    hv09_payload_sha256: String,
    hv09_source_logical_bytes: u64,
    hv09_source_allocated_bytes: u64,
    canonical_object_dir: PathBuf,
    prepared_unix_ms: u64,
}

#[derive(Clone, Debug)]
struct HeavyContext {
    run_id: RunId,
    payload_root: PathBuf,
    control_root: PathBuf,
    fixtures_root: PathBuf,
    evidence_root: PathBuf,
    oracle_path: PathBuf,
    cli_path: PathBuf,
    catalog_binding_path: PathBuf,
    cgroup_procs_path: PathBuf,
    storage_cgroup_dir: PathBuf,
    preparation: HeavyPreparation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TreeProfile {
    directories: u64,
    regular_files: u64,
    symlinks: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    files_at_least_100_kib: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Hv07PayloadSnapshot {
    profile: TreeProfile,
    metadata_inventory_sha256: String,
    content_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Hv05State {
    semantic: PreparedSemantic,
    selected_ref: PairedRefValue,
    base_allocation_id: AllocationId,
    final_delta_allocation_id: AllocationId,
    carrier_allocation_id: AllocationId,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Hv08State {
    semantic: PreparedSemantic,
    selected_ref: PairedRefValue,
    base_allocation_id: AllocationId,
    delta_allocation_id: AllocationId,
    readiness_path: PathBuf,
    current_control_state_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
struct FixedWindowSample {
    kind: String,
    side: String,
    operations: u64,
    elapsed_ns: u64,
    bytes: u64,
    checksum: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Hv07ChildRequest {
    schema_version: u32,
    fault_point: NamedFaultPoint,
    operation_id: OperationId,
    recovery_root: PathBuf,
    locator_root: PathBuf,
    ref_root: PathBuf,
    occ_root: PathBuf,
    durable_state_paths: Vec<PathBuf>,
    operation_root: PathBuf,
    allocation_root: PathBuf,
    allocation_id: AllocationId,
    owner_epoch: u64,
    publication_id: PublicationId,
    semantic_roots: sandbox_runtime_mpla_poc::CanonicalRootPair,
    canonical: sandbox_runtime_mpla_poc::CanonicalDurabilityReceipt,
    accounted_bytes: u64,
    branch: String,
    operation_cgroup_procs_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Hv07OperationFamily {
    Session,
    Owner,
    Canonical,
    Publication,
    Rollback,
    Activation,
}

impl Hv07OperationFamily {
    const fn for_point(point: NamedFaultPoint) -> Self {
        match point {
            NamedFaultPoint::FenceBeforeClose
            | NamedFaultPoint::FenceAfterClose
            | NamedFaultPoint::FenceAfterDrain
            | NamedFaultPoint::SealingBeforeWrite
            | NamedFaultPoint::SealingAfterFileFsync
            | NamedFaultPoint::SealingAfterDirFsync
            | NamedFaultPoint::QuiesceBeforeStop
            | NamedFaultPoint::QuiesceAfterReap
            | NamedFaultPoint::QuiesceAfterFdAudit
            | NamedFaultPoint::UnmountBeforeStrict
            | NamedFaultPoint::UnmountAfterStrict
            | NamedFaultPoint::FlushBeforeSyncfs
            | NamedFaultPoint::FlushAfterSyncfs
            | NamedFaultPoint::InventoryAfterFirst
            | NamedFaultPoint::InventoryAfterStableSecond => Self::Session,
            NamedFaultPoint::OwnerBeforeIntent
            | NamedFaultPoint::OwnerAfterIntentFsync
            | NamedFaultPoint::OwnerBeforeCompare
            | NamedFaultPoint::OwnerAfterGenerationFsync
            | NamedFaultPoint::OwnerAfterJournalCommit
            | NamedFaultPoint::OwnerAfterSelectorRename
            | NamedFaultPoint::OwnerAfterSelectorDirFsync
            | NamedFaultPoint::OwnerBeforeReceipt
            | NamedFaultPoint::OwnerAfterReceiptDirFsync => Self::Owner,
            NamedFaultPoint::CanonicalBeforeInstall
            | NamedFaultPoint::CanonicalAfterObjectFsync
            | NamedFaultPoint::CanonicalAfterObjectDirFsync
            | NamedFaultPoint::CanonicalAfterRootManifestFsync => Self::Canonical,
            NamedFaultPoint::LocatorAfterForward
            | NamedFaultPoint::LocatorAfterReverse
            | NamedFaultPoint::LocatorAfterManifestFsync
            | NamedFaultPoint::LocatorAfterSelectorRename
            | NamedFaultPoint::LocatorAfterSelectorDirFsync
            | NamedFaultPoint::RefBeforeTemp
            | NamedFaultPoint::RefAfterTempFsync
            | NamedFaultPoint::RefAfterReplace
            | NamedFaultPoint::RefAfterParentFsync
            | NamedFaultPoint::ResponseLossPublish => Self::Publication,
            NamedFaultPoint::ResponseLossRollback => Self::Rollback,
            NamedFaultPoint::ResponseLossActivate
            | NamedFaultPoint::ActivateAfterRefSelect
            | NamedFaultPoint::ActivateAfterLocatorPin
            | NamedFaultPoint::ActivateAfterFreshOwner
            | NamedFaultPoint::ActivateAfterMount
            | NamedFaultPoint::ActivateAfterReady
            | NamedFaultPoint::ActivateAfterBindingFsync => Self::Activation,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Hv07ChildIntent {
    schema_version: u32,
    family: Hv07OperationFamily,
    fault_point: NamedFaultPoint,
    operation_id: OperationId,
    operation_root: PathBuf,
}

struct Hv07PhysicalChildGuard {
    child: Option<std::process::Child>,
}

impl Hv07PhysicalChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self { child: Some(child) }
    }

    fn child_mut(&mut self) -> &mut std::process::Child {
        self.child
            .as_mut()
            .expect("HV-07 physical child guard is armed")
    }

    fn id(&self) -> u32 {
        self.child
            .as_ref()
            .expect("HV-07 physical child guard is armed")
            .id()
    }

    fn disarm_reaped(&mut self) {
        let _ = self.child.take();
    }

    fn kill_and_reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let child = self
            .child
            .as_mut()
            .expect("HV-07 physical child guard is armed");
        child.kill()?;
        let status = child.wait()?;
        let _ = self.child.take();
        Ok(status)
    }
}

impl Drop for Hv07PhysicalChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn hv07_decode_wait_status(status: libc::c_int) -> String {
    if libc::WIFEXITED(status) {
        format!("exited(code={})", libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        format!("signaled(signal={})", libc::WTERMSIG(status))
    } else if libc::WIFSTOPPED(status) {
        format!("stopped(signal={})", libc::WSTOPSIG(status))
    } else if libc::WIFCONTINUED(status) {
        "continued".to_owned()
    } else {
        "unknown".to_owned()
    }
}

fn hv07_child_stderr_window(length: u64) -> (u64, u64) {
    (
        length.saturating_sub(HV07_CHILD_STDERR_LIMIT_BYTES),
        length.min(HV07_CHILD_STDERR_LIMIT_BYTES),
    )
}

fn hv07_read_child_stderr(path: &Path) -> String {
    let mut stderr = match File::open(path) {
        Ok(stderr) => stderr,
        Err(error) => return format!("<unable to open child stderr: {error}>"),
    };
    let length = match stderr.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => return format!("<unable to inspect child stderr: {error}>"),
    };
    let (omitted, retained) = hv07_child_stderr_window(length);
    if let Err(error) = stderr.seek(SeekFrom::Start(omitted)) {
        return format!("<unable to seek child stderr: {error}>");
    }
    let mut bytes = Vec::with_capacity(usize::try_from(retained).unwrap_or(0));
    if let Err(error) = stderr
        .take(HV07_CHILD_STDERR_LIMIT_BYTES)
        .read_to_end(&mut bytes)
    {
        return format!("<unable to read child stderr: {error}>");
    }
    let diagnostic = String::from_utf8_lossy(&bytes);
    if omitted == 0 {
        diagnostic.into_owned()
    } else {
        format!("<omitted {omitted} leading bytes>\n{diagnostic}")
    }
}

#[test]
fn hv07_wait_status_diagnostic_is_decoded_and_bounded() {
    assert_eq!(hv07_decode_wait_status(101 << 8), "exited(code=101)");
    assert_eq!(
        hv07_decode_wait_status(libc::SIGKILL),
        format!("signaled(signal={})", libc::SIGKILL)
    );
    assert_eq!(
        hv07_child_stderr_window(HV07_CHILD_STDERR_LIMIT_BYTES + 7),
        (7, HV07_CHILD_STDERR_LIMIT_BYTES)
    );
}

struct Hv07CgroupGuard {
    directory: PathBuf,
    cgroup_procs_path: PathBuf,
    armed: bool,
}

impl Hv07CgroupGuard {
    fn create(parent: &Path) -> CampaignResult<Self> {
        if !parent.is_absolute() || !parent.starts_with("/sys/fs/cgroup") {
            return Err(format!(
                "HV-07 exclusive cgroup parent is outside /sys/fs/cgroup: {}",
                parent.display()
            )
            .into());
        }
        let directory = parent.join(format!("mpla-hv07-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let cgroup_procs_path = directory.join("cgroup.procs");
        if !cgroup_procs_path.is_file()
            || !fs::read_to_string(&cgroup_procs_path)?.trim().is_empty()
        {
            let _ = fs::remove_dir(&directory);
            return Err("HV-07 exclusive cgroup was not created empty".into());
        }
        Ok(Self {
            directory,
            cgroup_procs_path,
            armed: true,
        })
    }

    fn cgroup_procs_path(&self) -> &Path {
        &self.cgroup_procs_path
    }

    fn remove_terminal(&mut self) -> CampaignResult {
        let members = fs::read_to_string(&self.cgroup_procs_path)?;
        if !members.trim().is_empty() {
            return Err(
                format!("HV-07 exclusive cgroup retained terminal members: {members:?}").into(),
            );
        }
        let events = fs::read_to_string(self.directory.join("cgroup.events"))?;
        let populated = events
            .lines()
            .find_map(|line| line.strip_prefix("populated "))
            .ok_or("HV-07 exclusive cgroup has no populated event")?;
        if populated != "0" {
            return Err(format!(
                "HV-07 exclusive cgroup remained populated: {}",
                self.directory.display()
            )
            .into());
        }
        fs::remove_dir(&self.directory)?;
        self.armed = false;
        Ok(())
    }

    fn force_cleanup(&mut self) {
        if !self.armed {
            return;
        }
        let kill_path = self.directory.join("cgroup.kill");
        if kill_path.is_file() {
            let _ = fs::write(&kill_path, b"1\n");
        }
        for _ in 0..200 {
            let empty = fs::read_to_string(&self.cgroup_procs_path)
                .map(|members| members.trim().is_empty())
                .unwrap_or(true);
            if empty {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = fs::remove_dir(&self.directory);
        self.armed = false;
    }
}

impl Drop for Hv07CgroupGuard {
    fn drop(&mut self) {
        self.force_cleanup();
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Hv07SessionRetry {
    allocation: AllocationHandle,
    recovery: sandbox_runtime_mpla_poc::SessionSealRecoveryRequest,
    control_root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Hv07OwnerRetry {
    allocation_root: PathBuf,
    stable: StableAllocationReceipt,
    transition: OwnerTransitionRequest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Hv07ActivationRetry {
    payload: AllocationHandle,
    selected_ref: PairedRefValue,
    recipe: ProjectionRecipe,
    arena_root: PathBuf,
    control_root: PathBuf,
    cgroup_procs_path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
struct Hv07DebtInventory {
    profile: TreeProfile,
    mount_targets: Vec<PathBuf>,
    writable_fds: Vec<PathBuf>,
    durable_manifest_bytes: u64,
    temporary_manifest_bytes: u64,
    retirement_manifest_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
struct Hv07DebtProof {
    before: Hv07DebtInventory,
    after: Hv07DebtInventory,
    allocated_bytes_added: u64,
    durable_operation_bytes: u64,
    temporary_debt_bytes: u64,
    retirement_debt_bytes: u64,
    unexplained_bytes: u64,
}

#[derive(Debug)]
struct Hv07RecoveryProof {
    family: Hv07OperationFamily,
    recovery_invoked: bool,
    recovery_completed: bool,
    pre_recovery_phase: Option<sandbox_runtime_mpla_poc::recovery::DurableRecoveryPhase>,
    final_recovery_phase: Option<sandbox_runtime_mpla_poc::recovery::DurableRecoveryPhase>,
    before_owner: sandbox_runtime_mpla_poc::OwnerGeneration,
    after_owner: sandbox_runtime_mpla_poc::OwnerGeneration,
    locator_generation: Option<sandbox_runtime_mpla_poc::LocatorGeneration>,
    ref_sequence: Option<RefSequence>,
    selected_visibility: SelectedVisibility,
    idempotent_retry_same_result: bool,
    terminal_invariant_verified: bool,
    exact_owner_verified: bool,
    exact_locator_verified: bool,
    exact_ref_verified: bool,
    stationary_payload_verified: bool,
    stationary_payload_inventory: Value,
    session_terminal_before: bool,
    session_terminal_after: bool,
    post_sealing_session_resumed: bool,
    state_parent_synced: bool,
    details: Value,
}

impl HeavyContext {
    fn from_env(case: &str) -> CampaignResult<Self> {
        let lease = required_env("MPLA_POC_EXECUTION_LEASE")?;
        let expected = format!("{HEAVY_LEASE_PREFIX}{case}");
        if lease != expected {
            return Err(format!(
                "lead-issued execution lease mismatch: expected {expected}, observed {lease}"
            )
            .into());
        }
        let run_id = RunId::parse(required_env("MPLA_POC_RUN_ID")?)?;
        if run_id.as_str() != "m2r-20260728T015724p0800" {
            return Err("M2R lead heavy run ID differs from the frozen capsule".into());
        }
        let payload_root = required_path("MPLA_POC_PAYLOAD_ROOT")?;
        let control_root = required_path("MPLA_POC_CONTROL_ROOT")?;
        let fixtures_root = required_path("MPLA_POC_FIXTURES_ROOT")?;
        let evidence_root = required_path("MPLA_POC_EVIDENCE_ROOT")?;
        let cgroup_procs_path = required_path("MPLA_POC_CGROUP_PROCS")?;
        let storage_cgroup_dir = required_path("MPLA_POC_STORAGE_CGROUP_DIR")?;
        verify_storage_cgroup(&cgroup_procs_path, &storage_cgroup_dir)?;
        let preparation: HeavyPreparation =
            durable::read_json(&heavy_preparation_path(&control_root, &run_id))?;
        if preparation.schema_version != SCHEMA_VERSION
            || preparation.interface_version != sandbox_runtime_mpla_poc::INTERFACE_VERSION
            || preparation.run_id != run_id
        {
            return Err("M2 lead heavy preparation scope mismatch".into());
        }
        Ok(Self {
            run_id,
            payload_root,
            control_root,
            fixtures_root,
            evidence_root,
            oracle_path: required_path("MPLA_POC_ORACLE_BIN")?,
            cli_path: required_path("MPLA_POC_CLI_BIN")?,
            catalog_binding_path: required_path("MPLA_POC_CATALOG_BINDING_PATH")?,
            cgroup_procs_path,
            storage_cgroup_dir,
            preparation,
        })
    }

    fn root(&self) -> PathBuf {
        heavy_root(&self.control_root, &self.run_id)
    }

    fn arena_root(&self) -> PathBuf {
        self.payload_root.join("allocations")
    }

    fn case_dir(&self, case: &str) -> PathBuf {
        self.evidence_root.join("cases").join(case)
    }

    fn cgroup_snapshot(&self) -> CampaignResult<StorageCgroupSnapshot> {
        Ok(StorageCgroupSnapshot {
            sampled_unix_ms: sandbox_runtime_mpla_poc::unix_time_ms()?,
            memory_current: read_u64_file(&self.storage_cgroup_dir.join("memory.current"))?,
            memory_peak: read_u64_file(&self.storage_cgroup_dir.join("memory.peak"))?,
            memory_high: fs::read_to_string(self.storage_cgroup_dir.join("memory.high"))?
                .trim()
                .to_owned(),
            memory_max: fs::read_to_string(self.storage_cgroup_dir.join("memory.max"))?
                .trim()
                .to_owned(),
            memory_events: read_counter_file(&self.storage_cgroup_dir.join("memory.events"))?,
            memory_stat: read_counter_file(&self.storage_cgroup_dir.join("memory.stat"))?,
            process_ids: fs::read_to_string(&self.cgroup_procs_path)?
                .lines()
                .map(str::parse)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

pub fn prepare_heavy() -> CampaignResult {
    let lease = required_env("MPLA_POC_EXECUTION_LEASE")?;
    let expected = format!("{HEAVY_LEASE_PREFIX}PREPARE");
    if lease != expected {
        return Err(format!(
            "lead-issued execution lease mismatch: expected {expected}, observed {lease}"
        )
        .into());
    }
    let run_id = RunId::parse(required_env("MPLA_POC_RUN_ID")?)?;
    if run_id.as_str() != "m2r-20260728T015724p0800" {
        return Err("M2R lead heavy run ID differs from the frozen capsule".into());
    }
    let payload_root = required_path("MPLA_POC_PAYLOAD_ROOT")?;
    let control_root = required_path("MPLA_POC_CONTROL_ROOT")?;
    let evidence_root = required_path("MPLA_POC_EVIDENCE_ROOT")?;
    let cgroup_procs = required_path("MPLA_POC_CGROUP_PROCS")?;
    let cgroup_dir = required_path("MPLA_POC_STORAGE_CGROUP_DIR")?;
    verify_storage_cgroup(&cgroup_procs, &cgroup_dir)?;
    let source_root = required_path("MPLA_POC_R0_CORPUS_ROOT")?;
    let source_profile = profile_tree(&source_root)?;
    require_r0_profile(&source_profile)?;

    let root = heavy_root(&control_root, &run_id);
    let canonical_object_dir = root.join("canonical");
    let empty_lower = root.join("empty-lower");
    fs::create_dir_all(&canonical_object_dir)?;
    fs::create_dir_all(&empty_lower)?;
    fs::create_dir_all(&evidence_root)?;
    let arena_root = payload_root.join("allocations");

    let smoke_preparation = control_root
        .join("campaign")
        .join(run_id.as_str())
        .join("PREPARED.json");
    if !smoke_preparation.exists() {
        prepare()?;
    }

    let hv05_operation = OperationId::from_string(format!("{run_id}-hv05-base-prepare"));
    let hv05_base = create_allocation(&arena_root, &hv05_operation)?;
    populate_hv05_base(&hv05_base.upper_dir)?;
    let hv05_base_semantic = full_build(
        &root,
        &canonical_object_dir,
        &hv05_base,
        "hv05-base-prepare",
    )?;
    let lease = issue_workspace_lease(&hv05_base, SessionId::new(), &hv05_operation)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &control_root,
        hv05_base.clone(),
        lease,
        vec![empty_lower.clone()],
        Some(cgroup_procs.clone()),
    )?;
    let hv05_publication = PublicationId::from_string(format!("{run_id}-hv05-base-publication"));
    let stationary = stationary_adopt(
        &mut session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: hv05_operation.clone(),
            publication_id: hv05_publication.clone(),
        },
        &root.join("operations"),
        &mut FaultInjector::default(),
    )?;
    let hv05_base_ref = heavy_install_ref(
        &root,
        &hv05_base,
        &hv05_base_semantic.receipt,
        stationary.adoption.new_owner.owner_epoch,
        stationary.stable.after.allocated_bytes,
        &hv05_operation,
        &hv05_publication,
    )?;

    let r0_operation = OperationId::from_string(format!("{run_id}-r0-prepare"));
    let r0 = create_allocation(&arena_root, &r0_operation)?;
    let transfer_started = Instant::now();
    copy_tree_test_only(&source_root, &r0.upper_dir)?;
    let r0_transfer_elapsed_ns = ns(transfer_started.elapsed());
    let copied_profile = profile_tree(&r0.upper_dir)?;
    if copied_profile != source_profile {
        return Err(format!(
            "R0 transfer profile changed: source={source_profile:?} copied={copied_profile:?}"
        )
        .into());
    }

    let hv09_operation = OperationId::from_string(format!("{run_id}-hv09-source-prepare"));
    let hv09_publication = PublicationId::from_string(format!("{run_id}-hv09-source"));
    let hv09_source = create_allocation(&arena_root, &hv09_operation)?;
    let hv09_source_path = hv09_source.upper_dir.join("s2-large-1gib.bin");
    let hv09_payload_sha256 = write_pattern_file_digest(&hv09_source_path, HEAVY_GIB, 0x92)?;
    let hv09_metadata = hv09_source_path.metadata()?;
    let hv09_lease = issue_workspace_lease(&hv09_source, SessionId::new(), &hv09_operation)?;
    let mut hv09_session = sandbox_runtime_mpla_poc::MplaSession::open(
        &control_root,
        hv09_source.clone(),
        hv09_lease,
        vec![empty_lower],
        Some(cgroup_procs),
    )?;
    let hv09_stationary = stationary_adopt(
        &mut hv09_session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: hv09_operation,
            publication_id: hv09_publication,
        },
        &root.join("operations"),
        &mut FaultInjector::default(),
    )?;
    drop(hv09_session);

    let preparation = HeavyPreparation {
        schema_version: SCHEMA_VERSION,
        interface_version: sandbox_runtime_mpla_poc::INTERFACE_VERSION.to_owned(),
        run_id: run_id.clone(),
        hv05_base_allocation_id: hv05_base.descriptor.allocation_id,
        hv05_base_semantic: hv05_base_semantic.into(),
        hv05_base_ref,
        hv05_base_owner_epoch: stationary.adoption.new_owner.owner_epoch,
        r0_allocation_id: r0.descriptor.allocation_id,
        r0_source_root: r0.upper_dir,
        r0_transfer_elapsed_ns,
        r0_profile: copied_profile,
        hv09_source_allocation_id: hv09_source.descriptor.allocation_id,
        hv09_source_owner_epoch: hv09_stationary.adoption.new_owner.owner_epoch,
        hv09_payload_sha256,
        hv09_source_logical_bytes: hv09_metadata.len(),
        hv09_source_allocated_bytes: hv09_metadata.blocks() * 512,
        canonical_object_dir,
        prepared_unix_ms: sandbox_runtime_mpla_poc::unix_time_ms()?,
    };
    durable::replace_json(
        &heavy_preparation_path(&control_root, &run_id),
        &preparation,
    )?;
    Ok(())
}

pub fn run_hv05() -> CampaignResult {
    let context = HeavyContext::from_env("HV-05")?;
    run_heavy_case(&context, "HV-05", EvidenceClass::AbsoluteGateOnly, hv05)
}

pub fn run_hv06() -> CampaignResult {
    let context = HeavyContext::from_env("HV-06")?;
    run_heavy_case(&context, "HV-06", EvidenceClass::AbsoluteGateOnly, hv06)
}

pub fn run_hv07() -> CampaignResult {
    let context = HeavyContext::from_env("HV-07")?;
    run_heavy_case(&context, "HV-07", EvidenceClass::AbsoluteGateOnly, hv07)
}

pub fn run_hv08() -> CampaignResult {
    let context = HeavyContext::from_env("HV-08")?;
    run_heavy_case(&context, "HV-08", EvidenceClass::MatchedSpeedup, hv08)
}

pub fn run_hv10() -> CampaignResult {
    let context = HeavyContext::from_env("HV-10")?;
    run_heavy_case(&context, "HV-10", EvidenceClass::MatchedSpeedup, hv10)
}

pub fn run_hv09() -> CampaignResult {
    let context = HeavyContext::from_env("HV-09")?;
    run_heavy_case(&context, "HV-09", EvidenceClass::AbsoluteGateOnly, hv09)
}

fn run_heavy_case(
    context: &HeavyContext,
    case_id: &str,
    evidence_class: EvidenceClass,
    execute: fn(&HeavyContext) -> CampaignResult<CaseExecution>,
) -> CampaignResult {
    let started_unix_ms = sandbox_runtime_mpla_poc::unix_time_ms()?;
    let storage_before = context.cgroup_snapshot()?;
    let started = Instant::now();
    let execution = execute(context);
    let duration_ns = ns(started.elapsed());
    let storage_after = context.cgroup_snapshot()?;
    let finished_unix_ms = sandbox_runtime_mpla_poc::unix_time_ms()?;
    let case_dir = context.case_dir(case_id);
    fs::create_dir_all(&case_dir)?;
    let result_path = case_dir.join("result.json");
    let (outcome, assertions, failures, mut details) = match execution {
        Ok(execution) => {
            let failures = execution
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
            (
                if failures.is_empty() {
                    CaseOutcome::Passed
                } else {
                    CaseOutcome::Failed
                },
                execution.assertions,
                failures,
                execution.details,
            )
        }
        Err(error) => (
            CaseOutcome::Failed,
            Vec::new(),
            vec![error.to_string()],
            json!({"error": error.to_string()}),
        ),
    };
    let storage = json!({"before": storage_before, "after": storage_after});
    if let Value::Object(fields) = &mut details {
        fields.insert("storage_cgroup".to_owned(), storage);
    } else {
        details = json!({"case": details, "storage_cgroup": storage});
    }
    durable::replace_json(&case_dir.join("details.json"), &details)?;
    let receipt = CaseReceipt {
        schema_version: SCHEMA_VERSION,
        run_id: context.run_id.clone(),
        case_id: case_id.to_owned(),
        outcome,
        evidence_class,
        started_unix_ms,
        finished_unix_ms,
        duration_ns,
        assertions,
        failures_and_unknowns: failures,
        artifact_path: result_path.clone(),
    };
    durable::replace_json(&result_path, &receipt)?;
    if receipt.passes() {
        Ok(())
    } else {
        Err(format!("{case_id} did not pass").into())
    }
}

fn hv06(_context: &HeavyContext) -> CampaignResult<CaseExecution> {
    let started = Instant::now();
    let smoke_context = Context::from_env()?;
    let mut disjoint = sm_10(&smoke_context)?;
    let mut overlap = sm_11(&smoke_context)?;
    for receipt in &mut disjoint.assertions {
        receipt.name = format!("physical_disjoint_{}", receipt.name);
    }
    for receipt in &mut overlap.assertions {
        receipt.name = format!("physical_overlap_{}", receipt.name);
    }

    let cancellation_controller = sandbox_runtime_mpla_poc::AdmissionController::new();
    let mut cancellation_guards = (0..32)
        .map(|_| cancellation_controller.submit(4_096))
        .collect::<Result<Vec<_>, _>>()?;
    let cancellation_snapshot = cancellation_controller.snapshot()?;
    let cancelled = cancellation_guards.remove(5);
    let cancelled_receipt = cancelled.receipt().clone();
    drop(cancelled);
    let after_cancellation = cancellation_controller.snapshot()?;
    drop(cancellation_guards);

    let controller = sandbox_runtime_mpla_poc::AdmissionController::new();
    let mut guards = (0..32)
        .map(|_| controller.submit(4_096).map(Some))
        .collect::<Result<Vec<_>, _>>()?;
    let saturated = controller.snapshot()?;
    let queued_have_no_physical_ownership = guards.iter().flatten().all(|guard| {
        guard.receipt().tier == AdmissionTier::ActiveData
            || (!guard.receipt().owns_payload_allocation
                && !guard.receipt().owns_workspace_mount
                && !guard.receipt().owns_staging_allocation)
    });
    let job_33 = controller.submit(1).expect_err("job 33 must be rejected");
    let mut completed = Vec::with_capacity(32);
    for slot in guards.iter_mut().take(4) {
        completed.push(
            slot.as_ref()
                .ok_or("active admission disappeared")?
                .receipt()
                .job_ordinal,
        );
        drop(slot.take());
    }
    for slot in guards.iter_mut().skip(4) {
        let expected = slot
            .as_ref()
            .ok_or("queued admission disappeared")?
            .receipt()
            .job_ordinal;
        loop {
            let promoted = slot
                .as_mut()
                .ok_or("queued admission disappeared during promotion")?
                .try_promote()?
                .ok_or("FIFO head did not make progress")?;
            if promoted.tier == AdmissionTier::ActiveData {
                if promoted.job_ordinal != expected {
                    return Err("promotion changed stable job identity".into());
                }
                completed.push(promoted.job_ordinal);
                break;
            }
        }
        drop(slot.take());
    }
    let drained = controller.snapshot()?;
    let elapsed_ns = ns(started.elapsed());
    let mut assertions = disjoint.assertions;
    assertions.extend(overlap.assertions);
    assertions.extend([
        assertion(
            "exact_admission_shape",
            saturated.active_data_workers == 4
                && saturated.coordinators == 16
                && saturated.pending_descriptors == 16
                && saturated.pending_descriptor_bytes == 65_536,
            format!("{saturated:?}"),
            "4 active, 12 queued coordinators, 16 pending, 65536 descriptor bytes",
        ),
        assertion(
            "queued_jobs_own_no_physical_resources",
            queued_have_no_physical_ownership,
            queued_have_no_physical_ownership,
            true,
        ),
        assertion(
            "job_33_typed_preallocation_rejection",
            matches!(job_33, PocError::Overloaded(ref detail)
                if detail.contains("job 33 rejected before resource ownership")),
            job_33.to_string(),
            "typed overload before resource ownership",
        ),
        assertion(
            "queued_cancellation_releases_only_coordinator",
            cancelled_receipt.tier == AdmissionTier::Coordinator
                && after_cancellation.coordinators + 1 == cancellation_snapshot.coordinators
                && after_cancellation.active_data_workers
                    == cancellation_snapshot.active_data_workers
                && after_cancellation.private_allocations
                    == cancellation_snapshot.private_allocations,
            format!(
                "cancelled={cancelled_receipt:?} before={cancellation_snapshot:?} after={after_cancellation:?}"
            ),
            "one coordinator released and no physical ownership changed",
        ),
        assertion(
            "fifo_eventual_progress",
            completed == (1_u32..=32).collect::<Vec<_>>(),
            format!("{completed:?}"),
            "publication ordinals 1 through 32",
        ),
        assertion(
            "terminal_resources_zero",
            drained.active_data_workers == 0
                && drained.coordinators == 0
                && drained.pending_descriptors == 0
                && drained.pending_descriptor_bytes == 0
                && drained.private_allocations == 0
                && drained.active_mounts == 0
                && drained.staging_allocations == 0,
            format!("{drained:?}"),
            "all live resource counters zero",
        ),
        assertion(
            "case_hard_stop",
            elapsed_ns <= 45_000_000_000,
            elapsed_ns,
            "<=45000000000ns",
        ),
    ]);
    Ok(CaseExecution {
        assertions,
        details: json!({
            "physical_disjoint": disjoint.details,
            "physical_overlap": overlap.details,
            "admission": {
                "saturated": saturated,
                "job_33": job_33.to_string(),
                "cancelled": cancelled_receipt,
                "after_cancellation": after_cancellation,
                "completion_sequence": completed,
                "drained": drained,
            },
            "elapsed_ns": elapsed_ns,
        }),
    })
}

fn hv07(context: &HeavyContext) -> CampaignResult<CaseExecution> {
    let fault_point = NamedFaultPoint::parse(&required_env("MPLA_POC_FAULT_POINT")?)?;
    let smoke_context = Context::from_env()?;
    hv07_point(
        &smoke_context,
        &context.case_dir("HV-07"),
        fault_point,
        None,
    )
    .map(|(execution, _)| execution)
}

fn hv07_storage_cgroup_binding(context: &Context) -> CampaignResult<(&Path, &Path)> {
    Ok(
        crate::hv07_cgroup_binding::validate_hv07_storage_cgroup_binding(
            context.cgroup_procs_path.as_deref(),
            context.storage_cgroup_dir.as_deref(),
        )?,
    )
}

fn hv07_point(
    smoke_context: &Context,
    case_dir: &Path,
    fault_point: NamedFaultPoint,
    semantic_override: Option<SemanticBuildReceipt>,
) -> CampaignResult<(CaseExecution, SemanticBuildReceipt)> {
    let started = Instant::now();
    let expectation = hv07_fault_expectations()
        .into_iter()
        .find(|expectation| expectation.fault_point == fault_point)
        .ok_or("HV-07 point is absent from the frozen registry")?;
    let point_component = fault_point.as_str().replace(['.', '-'], "_");
    let point_dir = case_dir.join("faults").join(&point_component);
    fs::create_dir_all(&point_dir)?;

    let candidate = prepare_sm12_recovery_candidate(
        smoke_context,
        &point_dir,
        &point_component,
        semantic_override,
    )?;
    let state_path = candidate
        .recovery_root
        .join("operations")
        .join(candidate.operation_id.as_str())
        .join("STATE.json");
    let durable_state_paths = vec![
        state_path,
        candidate.allocation.owner_dir.join("CURRENT"),
        candidate.locator_root.join("CURRENT"),
        candidate.ref_root.join("refs"),
    ];
    let request_path = point_dir.join("child-request.json");
    let marker_path = point_dir.join("armed.json");
    let failed_span_path = point_dir.join("failed-span.json");
    let cancelled_span_path = point_dir.join("cancelled-span.json");
    let family = Hv07OperationFamily::for_point(fault_point);
    let operation_root = point_dir.join("real-operations").join(format!(
        "{}-{}",
        candidate.operation_id,
        uuid::Uuid::new_v4()
    ));
    let mut operation_cgroup = if matches!(
        family,
        Hv07OperationFamily::Session | Hv07OperationFamily::Activation
    ) {
        let (_, parent) = hv07_storage_cgroup_binding(smoke_context)?;
        Some(Hv07CgroupGuard::create(parent)?)
    } else {
        None
    };
    let child_request = Hv07ChildRequest {
        schema_version: SCHEMA_VERSION,
        fault_point,
        operation_id: candidate.operation_id.clone(),
        recovery_root: candidate.recovery_root.clone(),
        locator_root: candidate.locator_root.clone(),
        ref_root: candidate.ref_root.clone(),
        occ_root: candidate.occ_root.clone(),
        durable_state_paths,
        operation_root: operation_root.clone(),
        allocation_root: candidate.allocation.allocation_root.clone(),
        allocation_id: candidate.allocation.descriptor.allocation_id.clone(),
        owner_epoch: candidate.owner_epoch,
        publication_id: candidate.publication_id.clone(),
        semantic_roots: candidate.semantic.roots.clone(),
        canonical: candidate.semantic.durability.clone(),
        accounted_bytes: candidate.accounted_bytes,
        branch: candidate.branch.clone(),
        operation_cgroup_procs_path: operation_cgroup
            .as_ref()
            .map(|cgroup| cgroup.cgroup_procs_path().to_path_buf()),
    };
    durable::replace_json(&request_path, &child_request)?;
    fs::create_dir_all(&operation_root)?;
    let child_intent = Hv07ChildIntent {
        schema_version: SCHEMA_VERSION,
        family,
        fault_point,
        operation_id: candidate.operation_id.clone(),
        operation_root: operation_root.clone(),
    };
    write_hv07_create_new_json(&operation_root.join("INTENT.json"), &child_intent)?;
    let stale_session_probe = if family == Hv07OperationFamily::Session {
        Some(prepare_hv07_session_operation(&child_request)?)
    } else {
        None
    };
    let pre_activation_payload = if family == Hv07OperationFamily::Activation {
        Some(hv07_payload_snapshot(&candidate.allocation.upper_dir)?)
    } else {
        None
    };
    let debt_before = capture_hv07_debt_inventory(&operation_root)?;
    if marker_path.exists() {
        fs::remove_file(&marker_path)?;
        File::open(&point_dir)?.sync_all()?;
    }

    let child_stderr_path = point_dir.join(format!(
        "child-stderr-{}.log",
        candidate.operation_id.as_str()
    ));
    let child_stderr = File::options()
        .write(true)
        .create_new(true)
        .open(&child_stderr_path)?;
    let mut child_command = Command::new(std::env::current_exe()?);
    child_command
        .args([
            "--ignored",
            "--exact",
            "m2_hv07_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MPLA_POC_HV07_CHILD_REQUEST", &request_path)
        .env("MPLA_POC_PHYSICAL_FAULT_POINT", fault_point.as_str())
        .env("MPLA_POC_PHYSICAL_FAULT_ORDINAL", "1")
        .env("MPLA_POC_PHYSICAL_FAULT_ARMED_PATH", &marker_path)
        .stdout(Stdio::null())
        .stderr(Stdio::from(child_stderr));
    child_command.stdin(if stale_session_probe.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = Hv07PhysicalChildGuard::new(child_command.spawn()?);
    if let Some(lease) = stale_session_probe.as_ref() {
        let mut child_stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or("HV-07 session child has no private capability pipe")?;
        serde_json::to_writer(&mut child_stdin, lease)?;
        child_stdin.flush()?;
        drop(child_stdin);
    }
    let child_pid = child.id();
    let mut stopped_status = 0;
    let stopped_pid = unsafe {
        libc::waitpid(
            i32::try_from(child_pid)?,
            &mut stopped_status,
            libc::WUNTRACED,
        )
    };
    if stopped_pid != i32::try_from(child_pid)?
        || !libc::WIFSTOPPED(stopped_status)
        || libc::WSTOPSIG(stopped_status) != libc::SIGSTOP
    {
        if stopped_pid == i32::try_from(child_pid)?
            && (libc::WIFEXITED(stopped_status) || libc::WIFSIGNALED(stopped_status))
        {
            child.disarm_reaped();
        }
        let decoded_status = if stopped_pid == i32::try_from(child_pid)? {
            hv07_decode_wait_status(stopped_status)
        } else {
            "not-reaped".to_owned()
        };
        let child_stderr = hv07_read_child_stderr(&child_stderr_path);
        return Err(format!(
            "HV-07 child did not stop at the durable marker: pid={stopped_pid} raw_status={stopped_status} decoded_status={decoded_status} stderr_path={} stderr_tail=\n{child_stderr}",
            child_stderr_path.display()
        )
        .into());
    }
    let marker: PhysicalFaultMarker = durable::read_json(&marker_path)?;
    let real_operation_witness: RealOperationWitness =
        durable::read_json(&point_dir.join("real-operation.json"))?;
    let child_qualification_capabilities: Value =
        durable::read_json(&operation_root.join("qualification-capabilities.json"))?;
    let observed_intent: Hv07ChildIntent = durable::read_json(&operation_root.join("INTENT.json"))?;
    if observed_intent != child_intent {
        return Err("HV-07 child changed its immutable exact-operation intent".into());
    }
    if marker.fault_point != fault_point
        || marker.ordinal != 1
        || marker.process_id != child_pid
        || marker.operation_id.as_deref() != Some(candidate.operation_id.as_str())
        || !marker.marker_parent_synced
    {
        return Err(format!("HV-07 durable marker mismatch: {marker:?}").into());
    }
    use std::os::unix::process::ExitStatusExt as _;
    let killed_status = child.kill_and_reap()?;
    if killed_status.signal() != Some(libc::SIGKILL) {
        return Err(format!(
            "HV-07 child did not terminate through SIGKILL: pid={child_pid} status={killed_status}"
        )
        .into());
    }

    let recovery_proof = recover_hv07_exact_operation(
        &candidate,
        &child_intent,
        &marker,
        &real_operation_witness,
        stale_session_probe.as_ref(),
        pre_activation_payload.as_ref(),
    )?;
    if let Some(cgroup) = operation_cgroup.as_mut() {
        cgroup.remove_terminal()?;
    }
    let debt_after = capture_hv07_debt_inventory(&operation_root)?;
    let debt_proof = hv07_debt_proof(debt_before, debt_after)?;

    durable::replace_json(
        &failed_span_path,
        &json!({
            "schema_version": SCHEMA_VERSION,
            "status": "failed",
            "fault_point": fault_point,
            "operation_id": candidate.operation_id,
            "signal": libc::SIGKILL,
            "marker": marker_path,
        }),
    )?;
    durable::replace_json(
        &cancelled_span_path,
        &json!({
            "schema_version": SCHEMA_VERSION,
            "status": "cancelled",
            "fault_point": fault_point,
            "operation_id": candidate.operation_id,
            "reason": "physical worker killed after durable stop marker",
        }),
    )?;

    let before = DurableCrashWitness {
        schema_version: SCHEMA_VERSION,
        protocol_phase: expectation.protocol_phase,
        recovery_phase: recovery_proof.pre_recovery_phase,
        owner_count: 1,
        owner_allocation_id: Some(recovery_proof.before_owner.allocation_id.clone()),
        owner_epoch: Some(recovery_proof.before_owner.owner_epoch),
        locator_generation: recovery_proof.locator_generation,
        ref_sequence: recovery_proof.ref_sequence,
        session_terminal: recovery_proof.session_terminal_before,
        state_parent_synced: marker.marker_parent_synced,
    };
    let after = DurableCrashWitness {
        schema_version: SCHEMA_VERSION,
        protocol_phase: expectation.protocol_phase,
        recovery_phase: recovery_proof.final_recovery_phase,
        owner_count: 1,
        owner_allocation_id: Some(recovery_proof.after_owner.allocation_id.clone()),
        owner_epoch: Some(recovery_proof.after_owner.owner_epoch),
        locator_generation: recovery_proof.locator_generation,
        ref_sequence: recovery_proof.ref_sequence,
        session_terminal: recovery_proof.session_terminal_after,
        state_parent_synced: recovery_proof.state_parent_synced,
    };
    let ledger = CrashSweepLedger::open(case_dir.join("crash-ledger"))?;
    let attempt = next_hv07_attempt(&case_dir, fault_point)?;
    let record = ledger.record(CrashRecoveryObservation {
        schema_version: SCHEMA_VERSION,
        fault_point,
        attempt,
        execution_mode: CrashExecutionMode::ProcessSigkill,
        operation_id: candidate.operation_id.clone(),
        retry_operation_id: candidate.operation_id.clone(),
        before,
        after,
        real_operation_witness: Some(real_operation_witness),
        physical_kill_witness: Some(PhysicalKillWitness {
            schema_version: SCHEMA_VERSION,
            fault_point,
            operation_id: candidate.operation_id.clone(),
            process_id: child_pid,
            signal: libc::SIGKILL,
            durable_marker_observed: true,
            marker_parent_synced: marker.marker_parent_synced,
            terminated_by_expected_signal: true,
        }),
        recovery_replay_witness: Some(RecoveryReplayWitness {
            schema_version: SCHEMA_VERSION,
            fault_point,
            operation_id: candidate.operation_id.clone(),
            retry_operation_id: candidate.operation_id.clone(),
            recovery_invoked: recovery_proof.recovery_invoked,
            recovery_completed: recovery_proof.recovery_completed,
            terminal_invariant_verified: recovery_proof.terminal_invariant_verified,
            selected_visibility: recovery_proof.selected_visibility,
            exact_owner_verified: recovery_proof.exact_owner_verified,
            exact_locator_verified: recovery_proof.exact_locator_verified,
            exact_ref_verified: recovery_proof.exact_ref_verified,
            stationary_payload_verified: recovery_proof.stationary_payload_verified,
            failed_attempt_bundle_durable: failed_span_path.is_file(),
            cancelled_attempt_bundle_durable: cancelled_span_path.is_file(),
            idempotent_retry_verified: recovery_proof.idempotent_retry_same_result,
        }),
        selected_visibility: recovery_proof.selected_visibility,
        idempotent_retry_same_result: recovery_proof.idempotent_retry_same_result,
        post_sealing_session_resumed: recovery_proof.post_sealing_session_resumed,
        failed_span_retained: failed_span_path.is_file(),
        cancelled_span_retained: cancelled_span_path.is_file(),
        observed_debt_bytes: debt_proof
            .temporary_debt_bytes
            .checked_add(debt_proof.retirement_debt_bytes)
            .ok_or("HV-07 observed debt overflow")?,
        temporary_debt_bytes: debt_proof.temporary_debt_bytes,
        retirement_debt_bytes: debt_proof.retirement_debt_bytes,
        unclassified_debt_bytes: debt_proof.unexplained_bytes,
    })?;
    let summary = ledger.summary(true)?;
    let point_receipt = point_dir.join(format!("attempt-{attempt:08}.json"));
    durable::replace_json(
        &point_receipt,
        &json!({
            "marker": marker,
            "record": record,
            "summary": summary,
            "operation_family": recovery_proof.family,
            "exact_operation_recovery": recovery_proof.details,
            "stationary_payload_inventory": recovery_proof.stationary_payload_inventory,
            "final_owner": recovery_proof.after_owner,
            "debt_proof": debt_proof,
            "child_qualification_capabilities": child_qualification_capabilities,
            "semantic_receipt_reused": candidate.semantic_reused,
        }),
    )?;
    let elapsed_ns = ns(started.elapsed());
    let exact_fixture_requested = std::env::var_os("MPLA_POC_HV07_FIXTURE_BYTES").is_some();
    let semantic = candidate.semantic.clone();
    Ok((
        CaseExecution {
            assertions: vec![
                assertion(
                    "exact_fixture_bytes",
                    !exact_fixture_requested || candidate.fixture_logical_bytes == 128 * HEAVY_MIB,
                    candidate.fixture_logical_bytes,
                    "134217728 logical bytes when the HV-07 fixture is requested",
                ),
                assertion(
                    "physical_attempt_passed",
                    record.passed && record.observation.execution_mode.is_physical(),
                    format!("{record:?}"),
                    "passing process-SIGKILL attempt",
                ),
                assertion(
                    "durable_stop_then_sigkill",
                    marker.process_id == child_pid
                        && marker.fault_point == fault_point
                        && marker.marker_parent_synced,
                    format!("{marker:?}"),
                    "durable exact marker followed by SIGKILL",
                ),
                assertion(
                    "same_operation_exact_replay",
                    record.observation.idempotent_retry_same_result
                        && record.observation.operation_id == record.observation.retry_operation_id,
                    format!("{:?}", record.observation),
                    "same operation ID and exact selected result",
                ),
                assertion(
                    "old_or_complete_new_visibility",
                    recovery_proof.selected_visibility != SelectedVisibility::PartialNew,
                    format!("{:?}", recovery_proof.selected_visibility),
                    "Old or CompleteNew",
                ),
                assertion(
                    "no_failed_attempts_or_unclassified_debt",
                    summary.failed_attempts == 0 && record.observation.unclassified_debt_bytes == 0,
                    format!("{summary:?}"),
                    "zero failed attempts and zero unclassified debt",
                ),
                assertion(
                    "case_hard_stop",
                    elapsed_ns <= 60_000_000_000,
                    elapsed_ns,
                    "<=60000000000ns",
                ),
            ],
            details: json!({
                "fault_point": fault_point,
                "attempt": attempt,
                "point_receipt": point_receipt,
                "record": record,
                "summary": summary,
                "fixture_logical_bytes": candidate.fixture_logical_bytes,
                "child_qualification_capabilities": child_qualification_capabilities,
                "semantic_receipt_reused": candidate.semantic_reused,
                "elapsed_ns": elapsed_ns,
            }),
        },
        semantic,
    ))
}

fn recover_hv07_exact_operation(
    candidate: &Sm12RecoveryCandidate,
    intent: &Hv07ChildIntent,
    marker: &PhysicalFaultMarker,
    real_operation: &RealOperationWitness,
    stale_session_probe: Option<&sandbox_runtime_mpla_poc::MutableLease>,
    pre_activation_payload: Option<&Hv07PayloadSnapshot>,
) -> CampaignResult<Hv07RecoveryProof> {
    if marker.operation_id.as_deref() != Some(intent.operation_id.as_str())
        || real_operation.operation_id != intent.operation_id
        || real_operation.fault_point != intent.fault_point
    {
        return Err("HV-07 recovery was not bound to the killed exact operation".into());
    }
    let expected_stationary_payload_path = match intent.family {
        Hv07OperationFamily::Session => {
            let retry: Hv07SessionRetry =
                durable::read_json(&intent.operation_root.join("session-retry.json"))?;
            retry.allocation.upper_dir
        }
        Hv07OperationFamily::Owner => {
            let retry: Hv07OwnerRetry =
                durable::read_json(&intent.operation_root.join("owner-retry.json"))?;
            retry.allocation_root.join("upper")
        }
        Hv07OperationFamily::Canonical => {
            let request: SemanticBuildRequest =
                durable::read_json(&intent.operation_root.join("canonical-retry.json"))?;
            request.sealed_tree
        }
        Hv07OperationFamily::Publication
        | Hv07OperationFamily::Rollback
        | Hv07OperationFamily::Activation => candidate.allocation.upper_dir.clone(),
    };
    let stationary_payload_path_exact = matches!(
        (
            real_operation.stationary_payload_path_before.as_ref(),
            real_operation.stationary_payload_path_after.as_ref(),
        ),
        (Some(before), Some(after))
            if before == &expected_stationary_payload_path
                && after == &expected_stationary_payload_path
    );
    if !stationary_payload_path_exact
        || real_operation.payload_bytes_moved != 0
        || real_operation.payload_bytes_copied != 0
    {
        return Err("HV-07 killed operation moved or copied the stationary payload".into());
    }
    match intent.family {
        Hv07OperationFamily::Session => recover_hv07_session(
            candidate,
            intent,
            marker,
            stale_session_probe.ok_or("HV-07 session recovery has no in-memory stale probe")?,
        ),
        Hv07OperationFamily::Owner => recover_hv07_owner(candidate, intent, marker),
        Hv07OperationFamily::Canonical => recover_hv07_canonical(candidate, intent, marker),
        Hv07OperationFamily::Publication => recover_hv07_publication(candidate, intent, marker),
        Hv07OperationFamily::Rollback => recover_hv07_rollback(candidate, intent, marker),
        Hv07OperationFamily::Activation => recover_hv07_activation(
            candidate,
            intent,
            marker,
            pre_activation_payload
                .ok_or("HV-07 activation recovery has no pre-operation payload snapshot")?,
        ),
    }
}

fn recover_hv07_publication(
    candidate: &Sm12RecoveryCandidate,
    intent: &Hv07ChildIntent,
    marker: &PhysicalFaultMarker,
) -> CampaignResult<Hv07RecoveryProof> {
    hv07_cleanup_operation_mounts(&intent.operation_root)?;
    let recovery = PublicationRecovery::open(&candidate.recovery_root)?;
    let locator_store = LocatorStore::open(&candidate.locator_root)?;
    let ref_store = PairedRefStore::open(&candidate.ref_root)?;
    let occ = BranchOcc::open(&candidate.occ_root)?;
    let pre = recovery.inspect(&intent.operation_id)?;
    let before_owner = current_owner(&candidate.allocation.allocation_root)?;
    let first = recovery.replay(
        &intent.operation_id,
        &locator_store,
        &ref_store,
        &occ,
        &mut NamedFaultInjector::default(),
        |_, _, _| {
            Err(PocError::Integrity(
                "HV-07 recovery unexpectedly requested rebase".to_owned(),
            ))
        },
    )?;
    let first_value = hv07_recovery_value(&first).cloned();
    let second = recovery.replay(
        &intent.operation_id,
        &locator_store,
        &ref_store,
        &occ,
        &mut NamedFaultInjector::default(),
        |_, _, _| {
            Err(PocError::Integrity(
                "HV-07 idempotent recovery unexpectedly requested rebase".to_owned(),
            ))
        },
    )?;
    let second_value = hv07_recovery_value(&second).cloned();
    let post = recovery.inspect(&intent.operation_id)?;
    let after_owner = current_owner(&candidate.allocation.allocation_root)?;
    let selected_ref = ref_store.read(&candidate.branch)?;
    let selected_locator = locator_store.selected()?;
    let exact_owner = hv07_payload_owner_matches(
        &after_owner,
        &candidate.allocation.descriptor.allocation_id,
        &intent.operation_id,
        &candidate.publication_id,
    );
    let exact_ref = first_value.is_some()
        && first_value == second_value
        && selected_ref == first_value
        && selected_ref.as_ref().is_some_and(|value| {
            value.operation_id == intent.operation_id
                && value.publication_id == candidate.publication_id
        });
    let exact_locator = selected_locator.as_ref().is_some_and(|selected| {
        selected.operation_id == intent.operation_id
            && selected.publication_id == candidate.publication_id
            && selected_ref
                .as_ref()
                .is_some_and(|value| value.locator_generation == selected.receipt.generation)
    });
    let (stationary_payload_verified, stationary_payload_inventory) =
        hv07_stationary_payload_inventory(&candidate.allocation.upper_dir)?;
    let live = capture_hv07_live_state(&intent.operation_root)?;
    let selected_visibility = match &first {
        RecoveryOutcome::Committed(_) => SelectedVisibility::CompleteNew,
        RecoveryOutcome::Conflict(_) => SelectedVisibility::Old,
        RecoveryOutcome::AwaitingOwnerTransition { .. } => SelectedVisibility::PartialNew,
    };
    let state_parent_synced = hv07_observe_durable_parent_contract(&[
        candidate
            .recovery_root
            .join("operations")
            .join(intent.operation_id.as_str())
            .join("STATE.json"),
        candidate.allocation.owner_dir.join("CURRENT"),
        candidate.locator_root.join("CURRENT"),
        candidate.ref_root.join("refs"),
    ])?;
    let idempotent_retry_same_result = first_value.is_some() && first_value == second_value;
    Ok(Hv07RecoveryProof {
        family: intent.family,
        recovery_invoked: true,
        recovery_completed: true,
        pre_recovery_phase: Some(pre.phase),
        final_recovery_phase: Some(post.phase),
        before_owner,
        after_owner,
        locator_generation: selected_locator
            .as_ref()
            .map(|selected| selected.receipt.generation),
        ref_sequence: selected_ref.as_ref().map(|value| value.sequence),
        selected_visibility,
        idempotent_retry_same_result,
        terminal_invariant_verified: exact_owner
            && exact_locator
            && exact_ref
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty(),
        exact_owner_verified: exact_owner,
        exact_locator_verified: exact_locator,
        exact_ref_verified: exact_ref,
        stationary_payload_verified,
        stationary_payload_inventory,
        session_terminal_before: marker.post_sealing && marker.mount_ids.is_empty(),
        session_terminal_after: marker.post_sealing
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty(),
        post_sealing_session_resumed: marker.post_sealing
            && (!live.mount_targets.is_empty() || !live.writable_fds.is_empty()),
        state_parent_synced,
        details: json!({
            "recovery_path": "publication_recovery_replay",
            "first_outcome": format!("{first:?}"),
            "second_outcome": format!("{second:?}"),
            "pre_snapshot": pre,
            "post_snapshot": post,
            "live_state": live,
        }),
    })
}

fn recover_hv07_owner(
    _candidate: &Sm12RecoveryCandidate,
    intent: &Hv07ChildIntent,
    marker: &PhysicalFaultMarker,
) -> CampaignResult<Hv07RecoveryProof> {
    hv07_cleanup_operation_mounts(&intent.operation_root)?;
    let retry: Hv07OwnerRetry =
        durable::read_json(&intent.operation_root.join("owner-retry.json"))?;
    if retry.transition.operation_id != intent.operation_id
        || retry.stable.operation_id != intent.operation_id
    {
        return Err("HV-07 owner retry state does not name the killed operation".into());
    }
    let before_owner = current_owner(&retry.allocation_root)?;
    let first = compare_and_adopt(&retry.allocation_root, &retry.stable, &retry.transition)?;
    let second = compare_and_adopt(&retry.allocation_root, &retry.stable, &retry.transition)?;
    let after_owner = current_owner(&retry.allocation_root)?;
    let exact_owner = first.operation_id == intent.operation_id
        && second.operation_id == intent.operation_id
        && first.new_owner == second.new_owner
        && after_owner == second.new_owner
        && second.idempotent_replay;
    let no_locator = !intent.operation_root.join("owner/locators").exists();
    let no_ref = !intent.operation_root.join("owner/refs").exists();
    let allocation = open_allocation(
        retry
            .allocation_root
            .parent()
            .and_then(Path::parent)
            .ok_or("HV-07 owner retry allocation has no arena root")?,
        &retry.transition.allocation_id,
    )?;
    let (stationary_payload_verified, stationary_payload_inventory) =
        hv07_stationary_payload_inventory(&allocation.upper_dir)?;
    let live = capture_hv07_live_state(&intent.operation_root)?;
    let state_parent_synced = hv07_observe_durable_parent_contract(&[
        retry.allocation_root.join("owner/CURRENT"),
        retry
            .allocation_root
            .join("owner/receipts")
            .join(format!("{}.json", intent.operation_id)),
    ])?;
    Ok(Hv07RecoveryProof {
        family: intent.family,
        recovery_invoked: true,
        recovery_completed: true,
        pre_recovery_phase: None,
        final_recovery_phase: None,
        before_owner,
        after_owner,
        locator_generation: None,
        ref_sequence: None,
        selected_visibility: if exact_owner {
            SelectedVisibility::CompleteNew
        } else {
            SelectedVisibility::PartialNew
        },
        idempotent_retry_same_result: exact_owner,
        terminal_invariant_verified: exact_owner
            && no_locator
            && no_ref
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty(),
        exact_owner_verified: exact_owner,
        exact_locator_verified: no_locator,
        exact_ref_verified: no_ref,
        stationary_payload_verified,
        stationary_payload_inventory,
        session_terminal_before: marker.post_sealing && marker.mount_ids.is_empty(),
        session_terminal_after: marker.post_sealing
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty(),
        post_sealing_session_resumed: marker.post_sealing
            && (!live.mount_targets.is_empty() || !live.writable_fds.is_empty()),
        state_parent_synced,
        details: json!({
            "recovery_path": "compare_and_adopt_same_operation",
            "first_receipt": first,
            "second_receipt": second,
            "live_state": live,
        }),
    })
}

fn recover_hv07_canonical(
    candidate: &Sm12RecoveryCandidate,
    intent: &Hv07ChildIntent,
    marker: &PhysicalFaultMarker,
) -> CampaignResult<Hv07RecoveryProof> {
    hv07_cleanup_operation_mounts(&intent.operation_root)?;
    let request: SemanticBuildRequest =
        durable::read_json(&intent.operation_root.join("canonical-retry.json"))?;
    if request.operation_id != intent.operation_id {
        return Err("HV-07 canonical retry state does not name the killed operation".into());
    }
    let before_owner = current_owner(&candidate.allocation.allocation_root)?;
    hv07_reset_canonical_spool(&request, intent)?;
    let first = build_with_output(&request)?;
    hv07_reset_canonical_spool(&request, intent)?;
    let second = build_with_output(&request)?;
    let after_owner = current_owner(&candidate.allocation.allocation_root)?;
    let idempotent = first.receipt.operation_id == intent.operation_id
        && second.receipt.operation_id == intent.operation_id
        && first.receipt.roots == second.receipt.roots
        && first.receipt.record_stream_sha256 == second.receipt.record_stream_sha256
        && first.root_manifest_path == second.root_manifest_path;
    let exact_owner = before_owner == after_owner
        && after_owner.allocation_id == request.allocation_id
        && after_owner.operation_id == intent.operation_id;
    let no_locator = !intent.operation_root.join("canonical/locators").exists();
    let no_ref = !intent.operation_root.join("canonical/refs").exists();
    let (stationary_payload_verified, stationary_payload_inventory) =
        hv07_stationary_payload_inventory(&request.sealed_tree)?;
    let live = capture_hv07_live_state(&intent.operation_root)?;
    let state_parent_synced = hv07_observe_durable_parent_contract(&[
        first.root_manifest_path.clone(),
        second.root_manifest_path.clone(),
    ])?;
    Ok(Hv07RecoveryProof {
        family: intent.family,
        recovery_invoked: true,
        recovery_completed: true,
        pre_recovery_phase: None,
        final_recovery_phase: None,
        before_owner,
        after_owner,
        locator_generation: None,
        ref_sequence: None,
        selected_visibility: if idempotent {
            SelectedVisibility::CompleteNew
        } else {
            SelectedVisibility::PartialNew
        },
        idempotent_retry_same_result: idempotent,
        terminal_invariant_verified: idempotent
            && exact_owner
            && no_locator
            && no_ref
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty(),
        exact_owner_verified: exact_owner,
        exact_locator_verified: no_locator,
        exact_ref_verified: no_ref,
        stationary_payload_verified,
        stationary_payload_inventory,
        session_terminal_before: marker.post_sealing && marker.mount_ids.is_empty(),
        session_terminal_after: marker.post_sealing
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty(),
        post_sealing_session_resumed: marker.post_sealing
            && (!live.mount_targets.is_empty() || !live.writable_fds.is_empty()),
        state_parent_synced,
        details: json!({
            "recovery_path": "semantic_build_same_request",
            "first_receipt": first.receipt,
            "second_receipt": second.receipt,
            "live_state": live,
        }),
    })
}

fn hv07_reset_canonical_spool(
    request: &SemanticBuildRequest,
    intent: &Hv07ChildIntent,
) -> CampaignResult {
    if request.operation_id != intent.operation_id
        || !request.spool_dir.starts_with(&intent.operation_root)
        || request.spool_dir == intent.operation_root
    {
        return Err("HV-07 refused to clear a spool outside the killed operation".into());
    }
    if request.spool_dir.exists() {
        fs::remove_dir_all(&request.spool_dir)?;
        File::open(
            request
                .spool_dir
                .parent()
                .ok_or("HV-07 canonical spool has no parent")?,
        )?
        .sync_all()?;
    }
    Ok(())
}

fn recover_hv07_rollback(
    candidate: &Sm12RecoveryCandidate,
    intent: &Hv07ChildIntent,
    marker: &PhysicalFaultMarker,
) -> CampaignResult<Hv07RecoveryProof> {
    hv07_cleanup_operation_mounts(&intent.operation_root)?;
    let source_root = intent.operation_root.join("rollback");
    let locator_store = LocatorStore::open(source_root.join("locators"))?;
    let ref_store = PairedRefStore::open(source_root.join("refs"))?;
    let before_owner = current_owner(&candidate.allocation.allocation_root)?;
    let first = ref_store.recover_committed(
        "hv07-rollback",
        intent.operation_id.as_str(),
        &locator_store,
    )?;
    let second = ref_store.recover_committed(
        "hv07-rollback",
        intent.operation_id.as_str(),
        &locator_store,
    )?;
    let after_owner = current_owner(&candidate.allocation.allocation_root)?;
    let selected_ref = ref_store.read("hv07-rollback")?;
    let selected_locator = locator_store.selected()?;
    let exact_ref = first.as_ref().map(|receipt| &receipt.value)
        == second.as_ref().map(|receipt| &receipt.value)
        && first.as_ref().map(|receipt| &receipt.value) == selected_ref.as_ref()
        && selected_ref.as_ref().is_some_and(|value| {
            value.operation_id == intent.operation_id
                && value.publication_id == candidate.publication_id
        });
    let exact_locator = selected_locator.as_ref().is_some_and(|selected| {
        selected.operation_id == intent.operation_id
            && selected.publication_id == candidate.publication_id
            && selected_ref
                .as_ref()
                .is_some_and(|value| value.locator_generation == selected.receipt.generation)
    });
    let exact_owner = hv07_payload_owner_matches(
        &after_owner,
        &candidate.allocation.descriptor.allocation_id,
        &intent.operation_id,
        &candidate.publication_id,
    );
    let (stationary_payload_verified, stationary_payload_inventory) =
        hv07_stationary_payload_inventory(&candidate.allocation.upper_dir)?;
    let live = capture_hv07_live_state(&intent.operation_root)?;
    let state_parent_synced = hv07_observe_durable_parent_contract(&[
        source_root.join("locators/CURRENT"),
        source_root.join("refs/refs"),
    ])?;
    let idempotent = first.is_some() && exact_ref;
    Ok(Hv07RecoveryProof {
        family: intent.family,
        recovery_invoked: true,
        recovery_completed: true,
        pre_recovery_phase: None,
        final_recovery_phase: None,
        before_owner,
        after_owner,
        locator_generation: selected_locator
            .as_ref()
            .map(|selected| selected.receipt.generation),
        ref_sequence: selected_ref.as_ref().map(|value| value.sequence),
        selected_visibility: if exact_ref && exact_locator {
            SelectedVisibility::CompleteNew
        } else {
            SelectedVisibility::PartialNew
        },
        idempotent_retry_same_result: idempotent,
        terminal_invariant_verified: idempotent
            && exact_owner
            && exact_locator
            && exact_ref
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty(),
        exact_owner_verified: exact_owner,
        exact_locator_verified: exact_locator,
        exact_ref_verified: exact_ref,
        stationary_payload_verified,
        stationary_payload_inventory,
        session_terminal_before: marker.post_sealing && marker.mount_ids.is_empty(),
        session_terminal_after: marker.post_sealing
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty(),
        post_sealing_session_resumed: marker.post_sealing
            && (!live.mount_targets.is_empty() || !live.writable_fds.is_empty()),
        state_parent_synced,
        details: json!({
            "recovery_path": "paired_ref_recover_committed_same_operation",
            "first_receipt": first.map(|receipt| receipt.value),
            "second_receipt": second.map(|receipt| receipt.value),
            "live_state": live,
        }),
    })
}

fn recover_hv07_session(
    _candidate: &Sm12RecoveryCandidate,
    intent: &Hv07ChildIntent,
    marker: &PhysicalFaultMarker,
    stale_capability_probe: &sandbox_runtime_mpla_poc::MutableLease,
) -> CampaignResult<Hv07RecoveryProof> {
    let retry: Hv07SessionRetry =
        durable::read_json(&intent.operation_root.join("session-retry.json"))?;
    if retry.recovery.operation_id != intent.operation_id
        || retry.recovery.prior_operation_id != intent.operation_id
        || retry.recovery.allocation_id != retry.allocation.descriptor.allocation_id
        || stale_capability_probe.session_id != retry.recovery.session_id
        || stale_capability_probe.allocation_id != retry.recovery.allocation_id
        || stale_capability_probe.lease_epoch != retry.recovery.lease_epoch
        || stale_capability_probe.owner_epoch != retry.recovery.owner_epoch
    {
        return Err("HV-07 session retry state does not name the killed exact operation".into());
    }
    let session_dir = retry
        .control_root
        .join("sessions")
        .join(retry.recovery.session_id.as_str());
    let session_path = session_dir.join("SESSION.json");
    let sealing_path = session_dir.join("SEALING.json");
    let recovery_path = session_dir.join("SEAL-RECOVERY.json");
    let before_record: sandbox_runtime_mpla_poc::SessionRecord = durable::read_json(&session_path)?;
    let before_owner = current_owner(&retry.allocation.allocation_root)?;
    let payload_before = hv07_payload_snapshot(&retry.allocation.upper_dir)?;
    let first = sandbox_runtime_mpla_poc::recover_session_seal(
        &retry.control_root,
        &retry.allocation,
        &retry.recovery,
    )?;
    let payload_after_first = hv07_payload_snapshot(&retry.allocation.upper_dir)?;
    let first_recovery_bytes = fs::read(&recovery_path)?;
    let second = sandbox_runtime_mpla_poc::recover_session_seal(
        &retry.control_root,
        &retry.allocation,
        &retry.recovery,
    )?;
    let second_recovery_bytes = fs::read(&recovery_path)?;
    let payload_after_second = hv07_payload_snapshot(&retry.allocation.upper_dir)?;
    let after_record: sandbox_runtime_mpla_poc::SessionRecord = durable::read_json(&session_path)?;
    let after_owner = current_owner(&retry.allocation.allocation_root)?;
    let live = capture_hv07_live_state(&intent.operation_root)?;
    let expected_old = matches!(
        intent.fault_point,
        NamedFaultPoint::FenceBeforeClose
            | NamedFaultPoint::FenceAfterClose
            | NamedFaultPoint::FenceAfterDrain
            | NamedFaultPoint::SealingBeforeWrite
            | NamedFaultPoint::SealingAfterFileFsync
    );
    let expected_disposition = if expected_old {
        sandbox_runtime_mpla_poc::SessionSealRecoveryDisposition::Old
    } else {
        sandbox_runtime_mpla_poc::SessionSealRecoveryDisposition::CompleteNew
    };
    let disposition_exact =
        first.disposition == expected_disposition && second.disposition == expected_disposition;
    let immutable_replay =
        first == second && first_recovery_bytes == second_recovery_bytes && disposition_exact;
    let stale_writer_error = sandbox_runtime_mpla_poc::lease::validate_writer(
        &retry.allocation.allocation_root,
        &stale_capability_probe.writer,
    )
    .expect_err("terminal recovery unexpectedly preserved the stale writer capability");
    let stale_deleter_error = sandbox_runtime_mpla_poc::lease::validate_deleter(
        &retry.allocation.allocation_root,
        &stale_capability_probe.deleter,
    )
    .expect_err("terminal recovery unexpectedly preserved the stale deleter capability");
    let same_session_lease_reissue_error = issue_workspace_lease(
        &retry.allocation,
        retry.recovery.session_id.clone(),
        &retry.recovery.prior_operation_id,
    )
    .expect_err("terminal recovery unexpectedly reissued executable session authority");
    let expected_fenced_lease_epoch = retry
        .recovery
        .lease_epoch
        .checked_add(1)
        .ok_or("HV-07 session lease epoch overflow")?;
    let expected_fenced_owner_epoch = retry
        .recovery
        .owner_epoch
        .checked_add(1)
        .ok_or("HV-07 session owner epoch overflow")?;
    let stale_writer_rejected = matches!(
        &stale_writer_error,
        PocError::StaleCapability {
            capability,
            allocation_id,
            expected_epoch,
            observed_epoch,
        } if *capability == "writer"
            && allocation_id == retry.recovery.allocation_id.as_str()
            && *expected_epoch == expected_fenced_lease_epoch
            && *observed_epoch == retry.recovery.lease_epoch
    );
    let stale_deleter_rejected = matches!(
        &stale_deleter_error,
        PocError::StaleCapability {
            capability,
            allocation_id,
            expected_epoch,
            observed_epoch,
        } if *capability == "deleter"
            && allocation_id == retry.recovery.allocation_id.as_str()
            && *expected_epoch == expected_fenced_lease_epoch
            && *observed_epoch == retry.recovery.lease_epoch
    );
    let same_session_lease_reissue_rejected = matches!(
        &same_session_lease_reissue_error,
        PocError::OwnerConflict(detail)
            if detail == "workspace lease already belongs to another session or operation"
    );
    let before_terminal = matches!(
        before_record.phase,
        sandbox_runtime_mpla_poc::SessionPhase::PublicationCommitted
            | sandbox_runtime_mpla_poc::SessionPhase::RecoveryRequired
    );
    let after_terminal = matches!(
        after_record.phase,
        sandbox_runtime_mpla_poc::SessionPhase::PublicationCommitted
            | sandbox_runtime_mpla_poc::SessionPhase::RecoveryRequired
    ) && after_record.phase == first.terminal_phase
        && live.mount_targets.is_empty()
        && live.writable_fds.is_empty();
    let exact_owner = before_owner == after_owner
        && after_owner.allocation_id == retry.recovery.allocation_id
        && after_owner.operation_id == retry.recovery.prior_operation_id
        && after_owner.owner_epoch == retry.recovery.owner_epoch;
    let exact_fence = first.lease_fence.schema_version == SCHEMA_VERSION
        && first.lease_fence.operation_id == retry.recovery.operation_id
        && first.lease_fence.prior_operation_id == retry.recovery.prior_operation_id
        && first.lease_fence.allocation_id == retry.recovery.allocation_id
        && first.lease_fence.session_id == retry.recovery.session_id
        && first.lease_fence.prior_lease_epoch == retry.recovery.lease_epoch
        && first.lease_fence.prior_owner_epoch == retry.recovery.owner_epoch
        && first.lease_fence.fenced_lease_epoch == expected_fenced_lease_epoch
        && first.lease_fence.fenced_owner_epoch == expected_fenced_owner_epoch
        && first.lease_fence.writer_revoked
        && first.lease_fence.deleter_revoked
        && stale_writer_rejected
        && stale_deleter_rejected
        && same_session_lease_reissue_rejected;
    let exact_terminal_projection = match first.disposition {
        sandbox_runtime_mpla_poc::SessionSealRecoveryDisposition::Old => {
            first.sealing.is_none()
                && first.stable.is_none()
                && first.quiescence.is_none()
                && !sealing_path.exists()
                && !session_dir.join("STABLE.json").exists()
                && !session_dir.join("QUIESCENCE.json").exists()
        }
        sandbox_runtime_mpla_poc::SessionSealRecoveryDisposition::CompleteNew => {
            first.sealing.is_some()
                && first.stable.is_some()
                && first.quiescence.is_some()
                && sealing_path.is_file()
                && session_dir.join("STABLE.json").is_file()
                && session_dir.join("QUIESCENCE.json").is_file()
        }
    };
    let no_locator = !intent.operation_root.join("session/locators").exists();
    let no_ref = !intent.operation_root.join("session/refs").exists();
    let stationary_payload_verified =
        payload_before == payload_after_first && payload_after_first == payload_after_second;
    let stationary_payload_inventory = json!({
        "path": retry.allocation.upper_dir,
        "verification_bytes_read": payload_before.profile.logical_bytes
            .checked_mul(3)
            .ok_or("HV-07 session verification byte count overflow")?,
        "payload_bytes_moved": 0,
        "payload_bytes_copied": 0,
        "before": payload_before,
        "after_first_recovery": payload_after_first,
        "after_same_operation_replay": payload_after_second,
    });
    let state_parent_synced =
        hv07_observe_durable_parent_contract(&[session_path.clone(), sealing_path, recovery_path])?;
    let post_sealing_session_resumed =
        marker.post_sealing && (!after_terminal || !live.mount_targets.is_empty());
    let recovery_must_kill_holder = matches!(
        intent.fault_point,
        NamedFaultPoint::FenceBeforeClose
            | NamedFaultPoint::FenceAfterClose
            | NamedFaultPoint::FenceAfterDrain
            | NamedFaultPoint::SealingBeforeWrite
            | NamedFaultPoint::SealingAfterFileFsync
            | NamedFaultPoint::SealingAfterDirFsync
            | NamedFaultPoint::QuiesceBeforeStop
    );
    let physical_process_boundary_exact = if recovery_must_kill_holder {
        !first.cleanup.killed_or_signaled_pids.is_empty()
    } else {
        marker.post_sealing
            && marker.marker_parent_synced
            && marker.operation_id.as_deref() == Some(intent.operation_id.as_str())
            && first.cleanup.post_unmount_audit.is_clear()
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty()
    };
    let terminal_invariant_verified = immutable_replay
        && exact_owner
        && exact_fence
        && exact_terminal_projection
        && no_locator
        && no_ref
        && stationary_payload_verified
        && after_terminal
        && physical_process_boundary_exact
        && first.cleanup.post_unmount_audit.is_clear()
        && !post_sealing_session_resumed;
    Ok(Hv07RecoveryProof {
        family: intent.family,
        recovery_invoked: true,
        recovery_completed: true,
        pre_recovery_phase: None,
        final_recovery_phase: None,
        before_owner,
        after_owner,
        locator_generation: None,
        ref_sequence: None,
        selected_visibility: match first.disposition {
            sandbox_runtime_mpla_poc::SessionSealRecoveryDisposition::Old => {
                SelectedVisibility::Old
            }
            sandbox_runtime_mpla_poc::SessionSealRecoveryDisposition::CompleteNew => {
                SelectedVisibility::CompleteNew
            }
        },
        idempotent_retry_same_result: immutable_replay,
        terminal_invariant_verified,
        exact_owner_verified: exact_owner,
        exact_locator_verified: no_locator,
        exact_ref_verified: no_ref,
        stationary_payload_verified,
        stationary_payload_inventory,
        session_terminal_before: before_terminal,
        session_terminal_after: after_terminal,
        post_sealing_session_resumed,
        state_parent_synced,
        details: json!({
            "recovery_path": "secret_free_restart_recover_session_seal",
            "expected_disposition": expected_disposition,
            "disposition_exact": disposition_exact,
            "immutable_same_operation_replay": immutable_replay,
            "exact_terminal_projection": exact_terminal_projection,
            "exact_terminal_lease_fence": exact_fence,
            "stale_writer_rejected": stale_writer_rejected,
            "stale_deleter_rejected": stale_deleter_rejected,
            "same_session_lease_reissue_rejected": same_session_lease_reissue_rejected,
            "recovery_must_kill_holder": recovery_must_kill_holder,
            "recovery_killed_or_signaled_pids": first.cleanup.killed_or_signaled_pids,
            "physical_process_boundary_exact": physical_process_boundary_exact,
            "stale_writer_error": stale_writer_error.to_string(),
            "stale_deleter_error": stale_deleter_error.to_string(),
            "same_session_lease_reissue_error": same_session_lease_reissue_error.to_string(),
            "before_record": before_record,
            "after_record": after_record,
            "first_receipt": first,
            "second_receipt": second,
            "live_state": live,
        }),
    })
}

fn recover_hv07_activation(
    candidate: &Sm12RecoveryCandidate,
    intent: &Hv07ChildIntent,
    marker: &PhysicalFaultMarker,
    pre_operation_payload: &Hv07PayloadSnapshot,
) -> CampaignResult<Hv07RecoveryProof> {
    let retry: Hv07ActivationRetry =
        durable::read_json(&intent.operation_root.join("activation-retry.json"))?;
    if retry.selected_ref.operation_id != intent.operation_id {
        return Err("HV-07 activation retry state does not name the killed operation".into());
    }
    let activation_dir = retry
        .control_root
        .join("activations")
        .join(intent.operation_id.as_str());
    let locator_pin_path = activation_dir.join("LOCATOR_PIN.json");
    let binding_path = activation_dir.join("SESSION_BOUND.json");
    let outcome_path = activation_dir.join("OUTCOME.json");
    let recovery_path = activation_dir.join("RECOVERY.json");
    let before_owner = current_owner(&retry.payload.allocation_root)?;
    let payload_before = pre_operation_payload.clone();
    let recovery_request = ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(intent.operation_id.as_str()),
        allocation_operation_id: OperationId::from_string(format!(
            "{}-activation-allocation",
            intent.operation_id
        )),
        selected_ref: retry.selected_ref.clone(),
        recipe: retry.recipe.clone(),
        payload_allocations: vec![retry.payload.clone()],
        arena_root: retry.arena_root.clone(),
        control_root: retry.control_root.clone(),
        cgroup_procs_path: Some(retry.cgroup_procs_path.clone()),
        readiness_path: PathBuf::from("sm12.txt"),
        readiness_contains: None,
        readiness_timeout: Duration::from_secs(2),
    };
    let first = sandbox_runtime_mpla_poc::recover_exact_activation(&recovery_request)?;
    let payload_after_first = hv07_payload_snapshot(&retry.payload.upper_dir)?;
    let first_recovery_bytes = fs::read(&recovery_path)?;
    let second = sandbox_runtime_mpla_poc::recover_exact_activation(&recovery_request)?;
    let second_recovery_bytes = fs::read(&recovery_path)?;
    let payload_after_second = hv07_payload_snapshot(&retry.payload.upper_dir)?;
    let immutable_replay = first == second && first_recovery_bytes == second_recovery_bytes;
    let expected_old = matches!(
        intent.fault_point,
        NamedFaultPoint::ActivateAfterRefSelect
            | NamedFaultPoint::ActivateAfterLocatorPin
            | NamedFaultPoint::ActivateAfterFreshOwner
            | NamedFaultPoint::ActivateAfterMount
            | NamedFaultPoint::ActivateAfterReady
    );
    let expected_disposition = if expected_old {
        sandbox_runtime_mpla_poc::ActivationRecoveryDisposition::Old
    } else {
        sandbox_runtime_mpla_poc::ActivationRecoveryDisposition::CompleteNew
    };
    let disposition_exact =
        first.disposition == expected_disposition && second.disposition == expected_disposition;
    let after_owner = current_owner(&retry.payload.allocation_root)?;
    let exact_owner = before_owner == after_owner
        && after_owner.allocation_id == retry.payload.descriptor.allocation_id
        && after_owner.operation_id == intent.operation_id
        && after_owner.owner_epoch == candidate.owner_epoch
        && hv07_payload_owner_matches(
            &after_owner,
            &candidate.allocation.descriptor.allocation_id,
            &candidate.operation_id,
            &candidate.publication_id,
        );
    let local_ref_store = PairedRefStore::open(intent.operation_root.join("activation/refs"))?;
    let observed_ref = local_ref_store.read("hv07-activation")?;
    let exact_ref = observed_ref.as_ref() == Some(&retry.selected_ref);
    let locator_pin: Option<Value> = if locator_pin_path.exists() {
        Some(durable::read_json(&locator_pin_path)?)
    } else {
        None
    };
    let locator_pin_expected =
        !matches!(intent.fault_point, NamedFaultPoint::ActivateAfterRefSelect);
    let exact_locator = first.locator_pin_durable == locator_pin_expected
        && second.locator_pin_durable == locator_pin_expected
        && locator_pin.as_ref().map_or(!locator_pin_expected, |pin| {
            pin.get("activation_operation_id").and_then(Value::as_str)
                == Some(intent.operation_id.as_str())
                && pin
                    .get("locator_generation")
                    .and_then(Value::as_u64)
                    .is_some_and(|generation| {
                        generation == retry.selected_ref.locator_generation.get()
                    })
        });
    let binding_exact = match first.disposition {
        sandbox_runtime_mpla_poc::ActivationRecoveryDisposition::Old => first.binding.is_none(),
        sandbox_runtime_mpla_poc::ActivationRecoveryDisposition::CompleteNew => {
            first.binding.as_ref().is_some_and(|binding| {
                binding.activation_operation_id.as_str() == intent.operation_id.as_str()
                    && binding.selected_ref == retry.selected_ref
                    && binding.session_id == first.session_id
            })
        }
    };
    let outcome_expected = matches!(intent.fault_point, NamedFaultPoint::ResponseLossActivate);
    let outcome_exact = first.original_outcome.is_some() == outcome_expected
        && first.original_outcome.as_ref().is_none_or(|outcome| {
            outcome.activation_operation_id.as_str() == intent.operation_id.as_str()
                && outcome.session_id == first.session_id
                && first
                    .binding
                    .as_ref()
                    .is_some_and(|binding| binding.session_id == outcome.session_id)
        });
    let selected_payload_exact = first.selected_payloads_preserved
        && first.selected_payload_allocations_before == vec![retry.payload.clone()]
        && first.selected_payload_allocations_after == vec![retry.payload.clone()]
        && first.selected_payload_owners_before == vec![before_owner.clone()]
        && first.selected_payload_owners_after == vec![after_owner.clone()];
    let terminal_disposition_exact = match first.disposition {
        sandbox_runtime_mpla_poc::ActivationRecoveryDisposition::Old => {
            first.binding.is_none()
                && first.original_outcome.is_none()
                && first.terminal_session_record.is_none()
                && !first.allocation_retained
                && first.allocation_removed == first.fresh_allocation.is_some()
                && first.authority_fenced == first.terminal_lease_fence.is_some()
                && (first.fresh_owner.is_none() || first.terminal_lease_fence.is_some())
        }
        sandbox_runtime_mpla_poc::ActivationRecoveryDisposition::CompleteNew => {
            first.binding.is_some()
                && first
                    .terminal_session_record
                    .as_ref()
                    .is_some_and(|record| {
                        record.session_id == first.session_id
                            && record.phase
                                == sandbox_runtime_mpla_poc::SessionPhase::RecoveryRequired
                    })
                && !first.allocation_removed
                && first.allocation_retained
                && first.terminal_lease_fence.is_some()
                && first.authority_fenced
        }
    };
    let stationary_payload_verified = pre_operation_payload == &payload_before
        && payload_before == payload_after_first
        && payload_after_first == payload_after_second;
    let stationary_payload_inventory = json!({
        "path": retry.payload.upper_dir,
        "verification_bytes_read": payload_before.profile.logical_bytes
            .checked_mul(3)
            .ok_or("HV-07 activation verification byte count overflow")?,
        "payload_bytes_moved": 0,
        "payload_bytes_copied": 0,
        "before_child": pre_operation_payload,
        "after_sigkill_before_recovery": payload_before,
        "after_first_recovery": payload_after_first,
        "after_same_operation_replay": payload_after_second,
    });
    let live = capture_hv07_live_state(&intent.operation_root)?;
    let post_sealing_session_resumed =
        marker.post_sealing && (!live.mount_targets.is_empty() || !live.writable_fds.is_empty());
    let state_parent_synced = hv07_observe_durable_parent_contract(&[
        intent.operation_root.join("activation-retry.json"),
        locator_pin_path.clone(),
        binding_path.clone(),
        outcome_path.clone(),
        recovery_path,
    ])?;
    let terminal_invariant_verified = immutable_replay
        && disposition_exact
        && terminal_disposition_exact
        && exact_owner
        && exact_locator
        && exact_ref
        && binding_exact
        && outcome_exact
        && selected_payload_exact
        && stationary_payload_verified
        && first.mount_removed
        && first.process_audit_clear
        && !first.executable_authority_returned
        && live.mount_targets.is_empty()
        && live.writable_fds.is_empty()
        && !post_sealing_session_resumed;
    Ok(Hv07RecoveryProof {
        family: intent.family,
        recovery_invoked: true,
        recovery_completed: true,
        pre_recovery_phase: None,
        final_recovery_phase: None,
        before_owner,
        after_owner,
        locator_generation: observed_ref.as_ref().map(|value| value.locator_generation),
        ref_sequence: observed_ref.as_ref().map(|value| value.sequence),
        selected_visibility: match first.disposition {
            sandbox_runtime_mpla_poc::ActivationRecoveryDisposition::Old => SelectedVisibility::Old,
            sandbox_runtime_mpla_poc::ActivationRecoveryDisposition::CompleteNew => {
                SelectedVisibility::CompleteNew
            }
        },
        idempotent_retry_same_result: immutable_replay,
        terminal_invariant_verified,
        exact_owner_verified: exact_owner,
        exact_locator_verified: exact_locator,
        exact_ref_verified: exact_ref,
        stationary_payload_verified,
        stationary_payload_inventory,
        session_terminal_before: marker.post_sealing && marker.mount_ids.is_empty(),
        session_terminal_after: terminal_disposition_exact
            && live.mount_targets.is_empty()
            && live.writable_fds.is_empty(),
        post_sealing_session_resumed,
        state_parent_synced,
        details: json!({
            "recovery_path": "secret_free_restart_recover_exact_activation",
            "expected_disposition": expected_disposition,
            "disposition_exact": disposition_exact,
            "immutable_same_operation_replay": immutable_replay,
            "terminal_disposition_exact": terminal_disposition_exact,
            "selected_payload_exact": selected_payload_exact,
            "binding_exact": binding_exact,
            "outcome_exact": outcome_exact,
            "locator_pin": locator_pin,
            "first_receipt": first,
            "second_receipt": second,
            "live_state": live,
        }),
    })
}

fn hv07_recovery_value(outcome: &RecoveryOutcome) -> Option<&PairedRefValue> {
    match outcome {
        RecoveryOutcome::Committed(receipt) => Some(&receipt.value),
        RecoveryOutcome::AwaitingOwnerTransition { .. } | RecoveryOutcome::Conflict(_) => None,
    }
}

fn hv07_payload_owner_matches(
    owner: &sandbox_runtime_mpla_poc::OwnerGeneration,
    allocation_id: &AllocationId,
    operation_id: &OperationId,
    publication_id: &PublicationId,
) -> bool {
    owner.allocation_id == *allocation_id
        && owner.operation_id == *operation_id
        && matches!(
            &owner.subject,
            OwnerSubject::PayloadOwned {
                publication_id: observed
            } if observed == publication_id
        )
}

#[derive(Debug, Serialize)]
struct Hv07LiveState {
    mount_targets: Vec<PathBuf>,
    writable_fds: Vec<PathBuf>,
}

fn capture_hv07_live_state(operation_root: &Path) -> CampaignResult<Hv07LiveState> {
    Ok(Hv07LiveState {
        mount_targets: hv07_operation_mount_targets(operation_root)?,
        writable_fds: hv07_operation_writable_fds(operation_root)?,
    })
}

fn hv07_cleanup_operation_mounts(operation_root: &Path) -> CampaignResult {
    let mut targets = hv07_operation_mount_targets(operation_root)?;
    targets.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for target in targets {
        let encoded = std::ffi::CString::new(target.as_os_str().as_bytes())?;
        // SAFETY: `encoded` is a live, NUL-terminated path and every target was
        // read from mountinfo and proven to be inside this operation root.
        if unsafe { libc::umount2(encoded.as_ptr(), 0) } != 0 {
            return Err(format!(
                "HV-07 strict operation-scoped unmount failed for {}: {}",
                target.display(),
                std::io::Error::last_os_error()
            )
            .into());
        }
    }
    if !hv07_operation_mount_targets(operation_root)?.is_empty() {
        return Err("HV-07 operation-scoped mounts remained after strict unmount".into());
    }
    Ok(())
}

fn hv07_operation_mount_targets(operation_root: &Path) -> CampaignResult<Vec<PathBuf>> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    let mut targets = Vec::new();
    for line in mountinfo.lines() {
        let raw = line
            .split_whitespace()
            .nth(4)
            .ok_or("malformed /proc/self/mountinfo row")?;
        let target = PathBuf::from(hv07_decode_mount_path(raw)?);
        if target.starts_with(operation_root) {
            targets.push(target);
        }
    }
    targets.sort();
    targets.dedup();
    Ok(targets)
}

fn hv07_decode_mount_path(raw: &str) -> CampaignResult<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &raw[index + 1..index + 4];
            if octal.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
                decoded.push(u8::from_str_radix(octal, 8)?);
                index += 4;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    Ok(String::from_utf8(decoded)?)
}

fn hv07_operation_writable_fds(operation_root: &Path) -> CampaignResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        if !target.starts_with(operation_root) {
            continue;
        }
        let fd = entry.file_name().to_string_lossy().parse::<i32>()?;
        // SAFETY: `fd` names an entry observed in this process's `/proc/self/fd`;
        // F_GETFL only reads its status flags and does not retain a pointer.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            continue;
        }
        let access = flags & libc::O_ACCMODE;
        if access == libc::O_WRONLY || access == libc::O_RDWR {
            paths.push(target);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn write_hv07_create_new_json<T: Serialize>(path: &Path, value: &T) -> CampaignResult {
    let parent = path
        .parent()
        .ok_or("HV-07 immutable JSON path has no parent")?;
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn capture_hv07_debt_inventory(operation_root: &Path) -> CampaignResult<Hv07DebtInventory> {
    let profile = profile_tree(operation_root)?;
    let (durable_manifest_bytes, temporary_manifest_bytes, retirement_manifest_bytes) =
        hv07_classified_allocated_bytes(operation_root)?;
    Ok(Hv07DebtInventory {
        profile,
        mount_targets: hv07_operation_mount_targets(operation_root)?,
        writable_fds: hv07_operation_writable_fds(operation_root)?,
        durable_manifest_bytes,
        temporary_manifest_bytes,
        retirement_manifest_bytes,
    })
}

fn hv07_classified_allocated_bytes(root: &Path) -> CampaignResult<(u64, u64, u64)> {
    let mut durable = 0_u64;
    let mut temporary = 0_u64;
    let mut retirement = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let bytes = metadata
            .blocks()
            .checked_mul(512)
            .ok_or("HV-07 debt overflow")?;
        let relative = path.strip_prefix(root)?;
        let is_retirement = relative.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("retir")
        });
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let is_temporary = file_name.starts_with('.')
            || file_name.contains(".tmp")
            || file_name.ends_with(".partial");
        if is_retirement {
            retirement = retirement.checked_add(bytes).ok_or("HV-07 debt overflow")?;
        } else if is_temporary {
            temporary = temporary.checked_add(bytes).ok_or("HV-07 debt overflow")?;
        } else {
            durable = durable.checked_add(bytes).ok_or("HV-07 debt overflow")?;
        }
    }
    Ok((durable, temporary, retirement))
}

fn hv07_debt_proof(
    before: Hv07DebtInventory,
    after: Hv07DebtInventory,
) -> CampaignResult<Hv07DebtProof> {
    let allocated_bytes_added = after
        .profile
        .allocated_bytes
        .checked_sub(before.profile.allocated_bytes)
        .ok_or("HV-07 operation allocation shrank during recovery")?;
    let durable_operation_bytes = after
        .durable_manifest_bytes
        .checked_sub(before.durable_manifest_bytes)
        .ok_or("HV-07 durable operation bytes shrank during recovery")?;
    let temporary_debt_bytes = after
        .temporary_manifest_bytes
        .checked_sub(before.temporary_manifest_bytes)
        .ok_or("HV-07 temporary operation bytes shrank during recovery")?;
    let retirement_debt_bytes = after
        .retirement_manifest_bytes
        .checked_sub(before.retirement_manifest_bytes)
        .ok_or("HV-07 retirement operation bytes shrank during recovery")?;
    let classified = durable_operation_bytes
        .checked_add(temporary_debt_bytes)
        .and_then(|bytes| bytes.checked_add(retirement_debt_bytes))
        .ok_or("HV-07 classified debt overflow")?;
    let unexplained_bytes = allocated_bytes_added
        .checked_sub(classified)
        .ok_or("HV-07 classified bytes exceed measured allocation growth")?;
    Ok(Hv07DebtProof {
        temporary_debt_bytes,
        retirement_debt_bytes,
        before,
        after,
        allocated_bytes_added,
        durable_operation_bytes,
        unexplained_bytes,
    })
}

fn hv07_stationary_payload_inventory(path: &Path) -> CampaignResult<(bool, Value)> {
    let first_profile = profile_tree(path)?;
    let first_sha256 = hv07_tree_inventory_sha256(path)?;
    File::open(path)?.sync_all()?;
    let second_profile = profile_tree(path)?;
    let second_sha256 = hv07_tree_inventory_sha256(path)?;
    Ok((
        first_profile.regular_files > 0
            && first_profile == second_profile
            && first_sha256 == second_sha256,
        json!({
            "path": path,
            "first_profile": first_profile,
            "second_profile": second_profile,
            "first_inventory_sha256": first_sha256,
            "second_inventory_sha256": second_sha256,
        }),
    ))
}

fn hv07_tree_inventory_sha256(root: &Path) -> CampaignResult<String> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        let kind = if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
            "directory"
        } else if metadata.is_file() {
            "file"
        } else if metadata.file_type().is_symlink() {
            "symlink"
        } else {
            "other"
        };
        entries.push((
            path.strip_prefix(root)?.as_os_str().as_bytes().to_vec(),
            kind,
            metadata.len(),
            metadata.blocks(),
        ));
    }
    entries.sort();
    Ok(hex_digest(Sha256::digest(serde_json::to_vec(&entries)?)))
}

fn hv07_payload_snapshot(root: &Path) -> CampaignResult<Hv07PayloadSnapshot> {
    let profile = profile_tree(root)?;
    let metadata_inventory_sha256 = hv07_tree_inventory_sha256(root)?;
    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        }
        paths.push(path);
    }
    paths.sort_by(|left, right| {
        left.strip_prefix(root)
            .expect("HV-07 payload path is rooted")
            .as_os_str()
            .as_bytes()
            .cmp(
                right
                    .strip_prefix(root)
                    .expect("HV-07 payload path is rooted")
                    .as_os_str()
                    .as_bytes(),
            )
    });

    let mut content = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    for path in paths {
        let relative = path.strip_prefix(root)?.as_os_str().as_bytes();
        content.update(
            u64::try_from(relative.len())
                .map_err(|_| "HV-07 relative path length overflow")?
                .to_le_bytes(),
        );
        content.update(relative);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            content.update([b'd']);
        } else if metadata.is_file() {
            content.update([b'f']);
            content.update(metadata.len().to_le_bytes());
            let mut reader = BufReader::new(File::open(&path)?);
            let mut observed = 0_u64;
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                observed = observed
                    .checked_add(u64::try_from(read)?)
                    .ok_or("HV-07 payload content byte count overflow")?;
                content.update(&buffer[..read]);
            }
            if observed != metadata.len() {
                return Err(format!(
                    "HV-07 payload changed while hashing {}: metadata={} observed={observed}",
                    path.display(),
                    metadata.len()
                )
                .into());
            }
        } else if metadata.file_type().is_symlink() {
            content.update([b'l']);
            let target = fs::read_link(&path)?;
            let target = target.as_os_str().as_bytes();
            content.update(
                u64::try_from(target.len())
                    .map_err(|_| "HV-07 symlink target length overflow")?
                    .to_le_bytes(),
            );
            content.update(target);
        } else {
            return Err(format!(
                "HV-07 payload contains unsupported entry {}",
                path.display()
            )
            .into());
        }
    }
    Ok(Hv07PayloadSnapshot {
        profile,
        metadata_inventory_sha256,
        content_sha256: hex_digest(content.finalize()),
    })
}

fn hv07_observe_durable_parent_contract(paths: &[PathBuf]) -> CampaignResult<bool> {
    let mut observed = false;
    for path in paths {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            return Err(format!(
                "HV-07 durable state entry is not a regular file or directory: {}",
                path.display()
            )
            .into());
        }
        let parent = path
            .parent()
            .ok_or("HV-07 durable state path has no parent")?;
        let parent_metadata = fs::symlink_metadata(parent)?;
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(format!(
                "HV-07 durable state parent is not an exact directory: {}",
                parent.display()
            )
            .into());
        }
        observed = true;
    }
    Ok(observed)
}

pub fn run_hv07_fresh_sweep() -> CampaignResult {
    const HV07_HARD_STOP_NS: u64 = 60_000_000_000;
    let started = Instant::now();
    let context = Context::from_env()?;
    let (cgroup_procs_path, storage_cgroup_dir) = hv07_storage_cgroup_binding(&context)?;
    verify_storage_cgroup(cgroup_procs_path, storage_cgroup_dir)?;
    let qualification_capabilities = qualification_capability_receipt()?;
    let requested_fixture_bytes = required_env("MPLA_POC_HV07_FIXTURE_BYTES")?.parse::<u64>()?;
    if requested_fixture_bytes != 128 * HEAVY_MIB {
        return Err(format!(
            "HV-07 fresh sweep requires exactly {} fixture bytes, got {requested_fixture_bytes}",
            128 * HEAVY_MIB
        )
        .into());
    }
    let case_dir = context.evidence_root.join("cases").join("HV-07");
    fs::create_dir_all(&case_dir)?;
    let mut points = Vec::with_capacity(NamedFaultPoint::ALL.len());
    let mut failures = Vec::new();
    let mut shared_semantic = None;
    let mut canonical_semantic_builds = 0_u64;
    for fault_point in NamedFaultPoint::ALL {
        let semantic_receipt_reused = shared_semantic.is_some();
        match hv07_point(&context, &case_dir, *fault_point, shared_semantic.clone()) {
            Ok((execution, semantic)) => {
                if !semantic_receipt_reused {
                    canonical_semantic_builds += 1;
                    shared_semantic = Some(semantic);
                }
                let passed = execution
                    .assertions
                    .iter()
                    .all(|assertion| assertion.passed);
                if !passed {
                    failures.push(format!("{} assertion failure", fault_point.as_str()));
                }
                points.push(json!({
                    "fault_point": fault_point,
                    "passed": passed,
                    "semantic_receipt_reused": semantic_receipt_reused,
                    "assertions": execution.assertions,
                    "details": execution.details,
                }));
            }
            Err(error) => {
                failures.push(format!("{}: {error}", fault_point.as_str()));
                points.push(json!({
                    "fault_point": fault_point,
                    "passed": false,
                    "error": error.to_string(),
                }));
            }
        }
    }
    let ledger = CrashSweepLedger::open(case_dir.join("crash-ledger"))?;
    let summary = ledger.summary(true)?;
    if !summary.complete_for_requested_mode {
        failures.push(format!(
            "physical sweep incomplete: {} missing points",
            summary.physical_missing_fault_points.len()
        ));
    }
    let elapsed_ns = ns(started.elapsed());
    if elapsed_ns > HV07_HARD_STOP_NS {
        failures.push(format!(
            "HV-07 fresh sweep exceeded hard stop: {elapsed_ns}ns > {HV07_HARD_STOP_NS}ns"
        ));
    }
    let passed = failures.is_empty();
    durable::replace_json(
        &case_dir.join("fresh-sweep-result.json"),
        &json!({
            "schema_version": SCHEMA_VERSION,
            "case_id": "HV-07",
            "fixture_logical_bytes": requested_fixture_bytes,
            "qualification_capabilities": qualification_capabilities,
            "required_fault_points": NamedFaultPoint::ALL.len(),
            "canonical_semantic_builds": canonical_semantic_builds,
            "semantic_receipt_reuses": points.iter().filter(|point| {
                point
                    .get("semantic_receipt_reused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            }).count(),
            "elapsed_ns": elapsed_ns,
            "hard_stop_ns": HV07_HARD_STOP_NS,
            "summary": summary,
            "points": points,
            "failures": failures,
            "passed": passed,
        }),
    )?;
    if passed {
        Ok(())
    } else {
        Err("HV-07 fresh crash sweep failed".into())
    }
}

fn qualification_capability_receipt() -> CampaignResult<Value> {
    const CAP_SYS_ADMIN_BIT: u64 = 1_u64 << 21;
    let status = fs::read_to_string("/proc/self/status")?;
    let status_field = |key: &str| -> CampaignResult<String> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(|value| value.trim().to_owned())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("/proc/self/status omitted {key}"),
                )
                .into()
            })
    };
    let effective = status_field("CapEff:")?;
    let permitted = status_field("CapPrm:")?;
    let bounding = status_field("CapBnd:")?;
    let effective_bits = u64::from_str_radix(&effective, 16)?;
    let permitted_bits = u64::from_str_radix(&permitted, 16)?;
    let bounding_bits = u64::from_str_radix(&bounding, 16)?;
    let cap_sys_admin = json!({
        "bit": 21,
        "effective": effective_bits & CAP_SYS_ADMIN_BIT != 0,
        "permitted": permitted_bits & CAP_SYS_ADMIN_BIT != 0,
        "bounding": bounding_bits & CAP_SYS_ADMIN_BIT != 0,
    });
    if !cap_sys_admin
        .get("effective")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || !cap_sys_admin
            .get("permitted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || !cap_sys_admin
            .get("bounding")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Err(
            "qualified coordinator lacks CAP_SYS_ADMIN in an effective, permitted, or bounding set"
                .into(),
        );
    }
    Ok(json!({
        "process_id": std::process::id(),
        "cap_eff": effective,
        "cap_prm": permitted,
        "cap_bnd": bounding,
        "cap_sys_admin": cap_sys_admin,
        "no_new_privs": status_field("NoNewPrivs:")?,
        "seccomp": status_field("Seccomp:")?,
    }))
}

pub fn run_hv07_child() -> CampaignResult {
    let request_path = required_path("MPLA_POC_HV07_CHILD_REQUEST")?;
    let request: Hv07ChildRequest = durable::read_json(&request_path)?;
    if request.schema_version != SCHEMA_VERSION {
        return Err("HV-07 child request schema mismatch".into());
    }
    let configured = NamedFaultPoint::parse(&required_env("MPLA_POC_PHYSICAL_FAULT_POINT")?)?;
    if configured != request.fault_point {
        return Err("HV-07 child request and configured faultpoint disagree".into());
    }
    fs::create_dir_all(&request.operation_root)?;
    let intent: Hv07ChildIntent = durable::read_json(&request.operation_root.join("INTENT.json"))?;
    let expected_intent = Hv07ChildIntent {
        schema_version: SCHEMA_VERSION,
        family: Hv07OperationFamily::for_point(request.fault_point),
        fault_point: request.fault_point,
        operation_id: request.operation_id.clone(),
        operation_root: request.operation_root.clone(),
    };
    if intent != expected_intent {
        return Err("HV-07 child immutable exact-operation intent mismatch".into());
    }
    durable::replace_json(
        &request
            .operation_root
            .join("qualification-capabilities.json"),
        &qualification_capability_receipt()?,
    )?;
    match request.fault_point {
        NamedFaultPoint::LocatorAfterForward
        | NamedFaultPoint::LocatorAfterReverse
        | NamedFaultPoint::LocatorAfterManifestFsync
        | NamedFaultPoint::LocatorAfterSelectorRename
        | NamedFaultPoint::LocatorAfterSelectorDirFsync
        | NamedFaultPoint::RefBeforeTemp
        | NamedFaultPoint::RefAfterTempFsync
        | NamedFaultPoint::RefAfterReplace
        | NamedFaultPoint::RefAfterParentFsync
        | NamedFaultPoint::ResponseLossPublish => run_hv07_publication_operation(&request),
        NamedFaultPoint::FenceBeforeClose
        | NamedFaultPoint::FenceAfterClose
        | NamedFaultPoint::FenceAfterDrain
        | NamedFaultPoint::SealingBeforeWrite
        | NamedFaultPoint::SealingAfterFileFsync
        | NamedFaultPoint::SealingAfterDirFsync
        | NamedFaultPoint::QuiesceBeforeStop
        | NamedFaultPoint::QuiesceAfterReap
        | NamedFaultPoint::QuiesceAfterFdAudit
        | NamedFaultPoint::UnmountBeforeStrict
        | NamedFaultPoint::UnmountAfterStrict
        | NamedFaultPoint::FlushBeforeSyncfs
        | NamedFaultPoint::FlushAfterSyncfs
        | NamedFaultPoint::InventoryAfterFirst
        | NamedFaultPoint::InventoryAfterStableSecond => run_hv07_session_operation(&request),
        NamedFaultPoint::OwnerBeforeIntent
        | NamedFaultPoint::OwnerAfterIntentFsync
        | NamedFaultPoint::OwnerBeforeCompare
        | NamedFaultPoint::OwnerAfterGenerationFsync
        | NamedFaultPoint::OwnerAfterJournalCommit
        | NamedFaultPoint::OwnerAfterSelectorRename
        | NamedFaultPoint::OwnerAfterSelectorDirFsync
        | NamedFaultPoint::OwnerBeforeReceipt
        | NamedFaultPoint::OwnerAfterReceiptDirFsync => run_hv07_owner_operation(&request),
        NamedFaultPoint::CanonicalBeforeInstall
        | NamedFaultPoint::CanonicalAfterObjectFsync
        | NamedFaultPoint::CanonicalAfterObjectDirFsync
        | NamedFaultPoint::CanonicalAfterRootManifestFsync => {
            run_hv07_canonical_operation(&request)
        }
        NamedFaultPoint::ResponseLossRollback => run_hv07_rollback_operation(&request),
        NamedFaultPoint::ResponseLossActivate
        | NamedFaultPoint::ActivateAfterRefSelect
        | NamedFaultPoint::ActivateAfterLocatorPin
        | NamedFaultPoint::ActivateAfterFreshOwner
        | NamedFaultPoint::ActivateAfterMount
        | NamedFaultPoint::ActivateAfterReady
        | NamedFaultPoint::ActivateAfterBindingFsync => run_hv07_activation_operation(&request),
    }
}

fn run_hv07_publication_operation(request: &Hv07ChildRequest) -> CampaignResult {
    let recovery = PublicationRecovery::open(&request.recovery_root)?;
    let locator_store = LocatorStore::open(&request.locator_root)?;
    let ref_store = PairedRefStore::open(&request.ref_root)?;
    let occ = BranchOcc::open(&request.occ_root)?;
    let mut faults = NamedFaultInjector::default()
        .with_physical_context(
            request.operation_id.as_str(),
            request.durable_state_paths.clone(),
        )
        .with_physical_stationary_payload_path(request.allocation_root.join("upper"));
    let outcome = recovery.replay(
        &request.operation_id,
        &locator_store,
        &ref_store,
        &occ,
        &mut faults,
        |_, _, _| {
            Err(PocError::Integrity(
                "HV-07 child unexpectedly requested rebase".to_owned(),
            ))
        },
    )?;
    Err(format!(
        "HV-07 publication operation passed {} without stopping: {outcome:?}",
        request.fault_point.as_str()
    )
    .into())
}

fn prepare_hv07_session_operation(
    request: &Hv07ChildRequest,
) -> CampaignResult<sandbox_runtime_mpla_poc::MutableLease> {
    let source_root = request.operation_root.join("session");
    let lower = source_root.join("lower");
    fs::create_dir_all(&lower)?;
    durable::replace_json(&lower.join("LOWER.json"), &json!({"kind": "hv07-lower"}))?;
    let allocation = create_allocation(
        &source_root.join("payload/allocations"),
        &request.operation_id,
    )?;
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &request.operation_id)?;
    let recovery = sandbox_runtime_mpla_poc::SessionSealRecoveryRequest::from_lease(
        &lease,
        request.operation_id.clone(),
        request.operation_id.clone(),
    );
    write_hv07_create_new_json(
        &request.operation_root.join("session-retry.json"),
        &Hv07SessionRetry {
            allocation,
            recovery,
            control_root: source_root.join("control"),
        },
    )?;
    Ok(lease)
}

fn run_hv07_session_operation(request: &Hv07ChildRequest) -> CampaignResult {
    let source_root = request.operation_root.join("session");
    let lower = source_root.join("lower");
    let retry: Hv07SessionRetry =
        durable::read_json(&request.operation_root.join("session-retry.json"))?;
    let lease: sandbox_runtime_mpla_poc::MutableLease =
        serde_json::from_reader(std::io::stdin().lock())?;
    if retry.recovery.operation_id != request.operation_id
        || retry.recovery.prior_operation_id != request.operation_id
        || retry.recovery.allocation_id != retry.allocation.descriptor.allocation_id
        || retry.recovery.session_id != lease.session_id
        || retry.recovery.allocation_id != lease.allocation_id
        || retry.recovery.lease_epoch != lease.lease_epoch
        || retry.recovery.owner_epoch != lease.owner_epoch
    {
        return Err("HV-07 private session capability does not match durable retry tuple".into());
    }
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &retry.control_root,
        retry.allocation,
        lease.clone(),
        vec![lower],
        Some(
            request
                .operation_cgroup_procs_path
                .clone()
                .ok_or("HV-07 session operation has no exclusive cgroup")?,
        ),
    )?;
    let command = session.execute(
        &lease.writer,
        Path::new("/bin/sh"),
        &[
            "-c".to_owned(),
            "printf hv07-session > hv07-session.txt; \
             (exec 9>>hv07-session.txt; sleep 30) >/dev/null 2>&1 &"
                .to_owned(),
        ],
        Duration::from_secs(2),
    )?;
    if !command.success {
        return Err("HV-07 session source command failed".into());
    }
    let result = session.seal(&request.operation_id, &mut FaultInjector::default());
    Err(format!(
        "HV-07 session operation passed {} without stopping: {result:?}",
        request.fault_point.as_str()
    )
    .into())
}

fn run_hv07_owner_operation(request: &Hv07ChildRequest) -> CampaignResult {
    let source_root = request.operation_root.join("owner");
    let allocation = create_allocation(
        &source_root.join("payload/allocations"),
        &request.operation_id,
    )?;
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &request.operation_id)?;
    let payload_path = allocation.upper_dir.join("hv07-owner.txt");
    let mut payload = File::create(&payload_path)?;
    payload.write_all(b"hv07-owner")?;
    payload.sync_all()?;
    drop(payload);
    let (before, after) = capture_stable_pair(&allocation)?;
    let stable = StableAllocationReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        allocation: allocation.descriptor.clone(),
        expected_owner_epoch: lease.owner_epoch,
        before: before.physical,
        after: after.physical,
        sync_completed: true,
    };
    let transition = OwnerTransitionRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        publication_id: request.publication_id.clone(),
        session_id: lease.session_id,
        allocation_id: allocation.descriptor.allocation_id.clone(),
        expected_lease_epoch: lease.lease_epoch,
        expected_owner_epoch: lease.owner_epoch,
    };
    write_hv07_create_new_json(
        &request.operation_root.join("owner-retry.json"),
        &Hv07OwnerRetry {
            allocation_root: allocation.allocation_root.clone(),
            stable: stable.clone(),
            transition: transition.clone(),
        },
    )?;
    let result = compare_and_adopt(&allocation.allocation_root, &stable, &transition);
    Err(format!(
        "HV-07 owner operation passed {} without stopping: {result:?}",
        request.fault_point.as_str()
    )
    .into())
}

fn run_hv07_canonical_operation(request: &Hv07ChildRequest) -> CampaignResult {
    let source_root = request.operation_root.join("canonical");
    let sealed_tree = source_root.join("sealed-tree");
    fs::create_dir_all(&sealed_tree)?;
    let source_path = sealed_tree.join("hv07-canonical.txt");
    let mut source = File::create(&source_path)?;
    source.write_all(b"hv07-canonical")?;
    source.sync_all()?;
    drop(source);
    File::open(&sealed_tree)?.sync_all()?;
    let semantic_request = SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        allocation_id: request.allocation_id.clone(),
        sealed_tree,
        spool_dir: source_root.join("spool"),
        canonical_object_dir: source_root.join("objects"),
        attribution: attribution(),
    };
    write_hv07_create_new_json(
        &request.operation_root.join("canonical-retry.json"),
        &semantic_request,
    )?;
    let output = build_with_output(&semantic_request);
    Err(format!(
        "HV-07 canonical operation passed {} without stopping: {output:?}",
        request.fault_point.as_str()
    )
    .into())
}

fn run_hv07_rollback_operation(request: &Hv07ChildRequest) -> CampaignResult {
    let source_root = request.operation_root.join("rollback");
    let locator_store = LocatorStore::open(source_root.join("locators"))?;
    let ref_store = PairedRefStore::open(source_root.join("refs"))?;
    let delta = hv07_locator_delta(request)?;
    let locator = locator_store.install(&delta, &mut NamedFaultInjector::default())?;
    let candidate = LocatorRefCandidate {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        publication_id: request.publication_id.clone(),
        roots: request.semantic_roots.clone(),
        locator_generation: locator.generation,
        expected_sequence: RefSequence::ZERO,
    };
    let mut faults = NamedFaultInjector::default()
        .with_physical_context(request.operation_id.as_str(), [source_root.join("refs")])
        .with_physical_stationary_payload_path(request.allocation_root.join("upper"));
    let outcome = ref_store.commit_rollback(
        "hv07-rollback",
        &candidate,
        &request.canonical,
        &locator,
        &locator_store,
        &mut faults,
    )?;
    Err(format!(
        "HV-07 rollback operation passed {} without stopping: {outcome:?}",
        request.fault_point.as_str()
    )
    .into())
}

fn run_hv07_activation_operation(request: &Hv07ChildRequest) -> CampaignResult {
    let source_root = request.operation_root.join("activation");
    let payload = open_allocation(
        request
            .allocation_root
            .parent()
            .and_then(Path::parent)
            .ok_or("HV-07 activation allocation has no allocations root")?,
        &request.allocation_id,
    )?;
    let locator_store = LocatorStore::open(source_root.join("locators"))?;
    let ref_store = PairedRefStore::open(source_root.join("refs"))?;
    let locator = locator_store.install(
        &hv07_locator_delta(request)?,
        &mut NamedFaultInjector::default(),
    )?;
    let candidate = LocatorRefCandidate {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        publication_id: request.publication_id.clone(),
        roots: request.semantic_roots.clone(),
        locator_generation: locator.generation,
        expected_sequence: RefSequence::ZERO,
    };
    let selected_ref = match ref_store.commit(
        "hv07-activation",
        &candidate,
        &request.canonical,
        &locator,
        &locator_store,
        &mut NamedFaultInjector::default(),
    )? {
        RefCommitOutcome::Committed(receipt) => receipt.value,
        RefCommitOutcome::ExpectedParent { .. } => {
            return Err("HV-07 activation seed ref had an unexpected parent".into())
        }
    };
    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: request.semantic_roots.clone(),
        base_allocation_id: request.allocation_id.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: Vec::new(),
    };
    let arena_root = source_root.join("payload/allocations");
    let control_root = source_root.join("control");
    let cgroup_procs_path = request
        .operation_cgroup_procs_path
        .clone()
        .ok_or("HV-07 activation operation has no exclusive cgroup")?;
    fs::create_dir(&control_root)?;
    File::open(&source_root)?.sync_all()?;
    write_hv07_create_new_json(
        &request.operation_root.join("activation-retry.json"),
        &Hv07ActivationRetry {
            payload: payload.clone(),
            selected_ref: selected_ref.clone(),
            recipe: recipe.clone(),
            arena_root: arena_root.clone(),
            control_root: control_root.clone(),
            cgroup_procs_path: cgroup_procs_path.clone(),
        },
    )?;
    let activation = activate_exact(ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(request.operation_id.as_str()),
        allocation_operation_id: OperationId::from_string(format!(
            "{}-activation-allocation",
            request.operation_id
        )),
        selected_ref,
        recipe,
        payload_allocations: vec![payload],
        arena_root,
        control_root,
        cgroup_procs_path: Some(cgroup_procs_path),
        readiness_path: PathBuf::from("sm12.txt"),
        readiness_contains: None,
        readiness_timeout: Duration::from_secs(2),
    });
    Err(format!(
        "HV-07 activation operation passed {} without stopping: {activation:?}",
        request.fault_point.as_str()
    )
    .into())
}

fn hv07_locator_delta(request: &Hv07ChildRequest) -> CampaignResult<LocatorDelta> {
    let payload_root = PayloadRootId::parse(request.semantic_roots.root_id.as_str())?;
    Ok(LocatorDelta {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        publication_id: request.publication_id.clone(),
        expected_parent: None,
        forward: vec![ForwardLocatorEntry {
            payload_root: payload_root.clone(),
            allocation_id: request.allocation_id.clone(),
            owner_epoch: request.owner_epoch,
            extents: vec![LocatorExtent {
                relative_path: "upper".to_owned(),
                offset: 0,
                length: request.accounted_bytes,
            }],
        }],
        reverse: vec![ReverseLocatorEntry {
            allocation_id: request.allocation_id.clone(),
            owner_epoch: request.owner_epoch,
            operation_id: request.operation_id.clone(),
            publication_id: request.publication_id.clone(),
            payload_roots: vec![payload_root],
            accounted_bytes: request.accounted_bytes,
        }],
    })
}

fn hv09(context: &HeavyContext) -> CampaignResult<CaseExecution> {
    let started = Instant::now();
    let case_root = context.root().join("hv09");
    fs::create_dir_all(&case_root)?;
    let source = open_allocation(
        &context.arena_root(),
        &context.preparation.hv09_source_allocation_id,
    )?;
    let source_owner = current_owner(&source.allocation_root)?;
    let source_path = source.upper_dir.join("s2-large-1gib.bin");
    let source_metadata = source_path.metadata()?;
    let source_allocation_id = source.descriptor.allocation_id.clone();
    let source_owner_epoch = context.preparation.hv09_source_owner_epoch;
    let target_allocation_id =
        AllocationId::from_string(format!("{}-hv09-packed-target", context.run_id));
    let target_owner_epoch = source_owner_epoch
        .checked_add(1)
        .ok_or("HV-09 target owner epoch overflow")?;
    let payload_root = PayloadRootId::parse(context.preparation.hv09_payload_sha256.clone())?;
    let locator_store = LocatorStore::open(case_root.join("locators"))?;
    let source_operation_id =
        OperationId::from_string(format!("{}-hv09-source-locator", context.run_id));
    let source_publication_id =
        PublicationId::from_string(format!("{}-hv09-source-locator", context.run_id));
    let source_locator = locator_store.install(
        &LocatorDelta {
            schema_version: SCHEMA_VERSION,
            operation_id: source_operation_id.clone(),
            publication_id: source_publication_id.clone(),
            expected_parent: None,
            forward: vec![ForwardLocatorEntry {
                payload_root: payload_root.clone(),
                allocation_id: source_allocation_id.clone(),
                owner_epoch: source_owner_epoch,
                extents: vec![LocatorExtent {
                    relative_path: "upper/s2-large-1gib.bin".to_owned(),
                    offset: 0,
                    length: source_metadata.len(),
                }],
            }],
            reverse: vec![ReverseLocatorEntry {
                allocation_id: source_allocation_id.clone(),
                owner_epoch: source_owner_epoch,
                operation_id: source_operation_id,
                publication_id: source_publication_id,
                payload_roots: vec![payload_root.clone()],
                accounted_bytes: source_metadata.blocks() * 512,
            }],
        },
        &mut NamedFaultInjector::default(),
    )?;

    let store = EvacuationStore::open(case_root.join("evacuation"))?;
    let operation_id = OperationId::from_string(format!("{}-hv09-evacuate", context.run_id));
    let publication_id = PublicationId::from_string(format!("{}-hv09-evacuate", context.run_id));
    let request = EvacuationRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        payload_root: payload_root.clone(),
        source_allocation_id: source_allocation_id.clone(),
        source_owner_epoch,
        source_generation: source_locator.generation,
        source_payload_path: source_path.clone(),
        source_logical_bytes: source_metadata.len(),
        source_allocated_bytes: source_metadata.blocks() * 512,
        target_allocation_id: target_allocation_id.clone(),
        target_owner_epoch,
        target_payload_path: store.pack_path(&operation_id),
    };
    let prepared = store.prepare(&request)?;
    let mut pinned_source = store.pin_selected(&operation_id)?;
    let ready = store.build_pack(&operation_id)?;
    let replacement = LocatorReplacement {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        payload_root: payload_root.clone(),
        expected_parent: source_locator.generation,
        expected_source_allocation_id: source_allocation_id.clone(),
        expected_source_owner_epoch: source_owner_epoch,
        target: ForwardLocatorEntry {
            payload_root: payload_root.clone(),
            allocation_id: target_allocation_id.clone(),
            owner_epoch: target_owner_epoch,
            extents: vec![LocatorExtent {
                relative_path: format!("packs/{}/payload.pack", operation_id.as_str()),
                offset: 0,
                length: ready.target_logical_bytes,
            }],
        },
        target_reverse: ReverseLocatorEntry {
            allocation_id: target_allocation_id.clone(),
            owner_epoch: target_owner_epoch,
            operation_id: operation_id.clone(),
            publication_id,
            payload_roots: vec![payload_root.clone()],
            accounted_bytes: ready.target_allocated_bytes,
        },
    };
    let selected = store.replace_locator(
        &operation_id,
        &locator_store,
        &replacement,
        &mut NamedFaultInjector::default(),
    )?;
    pinned_source.seek(SeekFrom::Start(0))?;
    let old_reader_digest = sha256_reader(&mut pinned_source)?;
    let mut target_reader = store.pin_selected(&operation_id)?;
    let target_reader_digest = sha256_reader(&mut target_reader)?;
    drop(target_reader);
    let authorization = StageFiveRetirementAuthorization {
        schema_version: SCHEMA_VERSION,
        authorization_id: OperationId::from_string(format!("{}-hv09-stage-five", context.run_id)),
        evacuation_operation_id: operation_id.clone(),
        payload_root: payload_root.clone(),
        source_allocation_id: source_allocation_id.clone(),
        source_owner_epoch,
        selected_generation: selected.selected_generation,
        deletion_authorized: true,
    };
    let blocked_retirement = store.retire_source(&operation_id, &authorization);
    let source_present_while_pinned = source_path.is_file();
    drop(pinned_source);
    let terminal = store.retire_source(&operation_id, &authorization)?;
    let replay = store.retire_source(&operation_id, &authorization)?;
    let resolved = locator_store
        .resolve(&payload_root)?
        .ok_or("HV-09 selected locator disappeared")?;
    let reconciliation = sandbox_runtime_mpla_poc::reconcile::reconcile(
        &case_root,
        &[
            sandbox_runtime_mpla_poc::StorageCategoryRoot {
                category: "scope".to_owned(),
                root: case_root.clone(),
                recursive: false,
            },
            sandbox_runtime_mpla_poc::StorageCategoryRoot {
                category: "evacuation".to_owned(),
                root: case_root.join("evacuation"),
                recursive: true,
            },
            sandbox_runtime_mpla_poc::StorageCategoryRoot {
                category: "locators".to_owned(),
                root: case_root.join("locators"),
                recursive: true,
            },
        ],
        sandbox_runtime_mpla_poc::LeakCounts::default(),
    )?;
    let elapsed_ns = ns(started.elapsed());
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "stationary_adopted_exact_1gib_source",
                source_owner.owner_epoch == source_owner_epoch
                    && matches!(source_owner.subject, OwnerSubject::PayloadOwned { .. })
                    && source_metadata.len() == HEAVY_GIB
                    && context.preparation.hv09_source_logical_bytes == HEAVY_GIB
                    && context.preparation.hv09_source_allocated_bytes
                        == source_metadata.blocks() * 512,
                format!(
                    "owner={source_owner:?} logical={} allocated={}",
                    source_metadata.len(),
                    source_metadata.blocks() * 512
                ),
                "PayloadOwned exact 1GiB dense source",
            ),
            assertion(
                "explicit_post_publication_pack",
                prepared.phase == EvacuationPhase::Building
                    && ready.phase == EvacuationPhase::Ready
                    && ready.target_logical_bytes == HEAVY_GIB
                    && ready.payload_sha256.as_deref()
                        == Some(context.preparation.hv09_payload_sha256.as_str()),
                format!("prepared={prepared:?} ready={ready:?}"),
                "Building -> Ready exact digest and 1GiB",
            ),
            assertion(
                "honest_old_plus_new_peak",
                ready.honest_old_plus_new_peak_bytes
                    == ready.source_allocated_bytes + ready.target_allocated_bytes,
                ready.honest_old_plus_new_peak_bytes,
                ready.source_allocated_bytes + ready.target_allocated_bytes,
            ),
            assertion(
                "held_reader_survives_locator_replacement",
                old_reader_digest == context.preparation.hv09_payload_sha256
                    && target_reader_digest == context.preparation.hv09_payload_sha256
                    && selected.active_reader_pins == 1,
                format!(
                    "old={old_reader_digest} target={target_reader_digest} pins={}",
                    selected.active_reader_pins
                ),
                "old and new readers match with one old-generation pin",
            ),
            assertion(
                "pin_blocks_source_retirement",
                matches!(blocked_retirement, Err(PocError::RecoveryRequired(ref detail))
                    if detail.contains("active reader pins"))
                    && source_present_while_pinned,
                format!("{blocked_retirement:?}"),
                "typed RecoveryRequired and source retained",
            ),
            assertion(
                "exact_locator_owner_replacement",
                resolved == replacement.target
                    && selected.selected_generation == source_locator.generation.checked_next()?,
                format!("resolved={resolved:?} selected={selected:?}"),
                format!(
                    "target allocation/epoch at generation {}",
                    selected.selected_generation
                ),
            ),
            assertion(
                "authorized_idempotent_terminal_retirement",
                terminal.phase == EvacuationPhase::Terminal
                    && terminal == replay
                    && !terminal.source_present
                    && terminal.target_present
                    && terminal.active_reader_pins == 0
                    && terminal.retirement_debt_objects == 0
                    && terminal.retirement_debt_bytes == 0,
                format!("terminal={terminal:?} replay={replay:?}"),
                "idempotent terminal with zero pins and debt",
            ),
            assertion(
                "balanced_storage_x_unexplained_zero",
                reconciliation.balanced
                    && reconciliation.unexplained_allocated_bytes == 0
                    && reconciliation.unexplained_inodes == 0
                    && reconciliation.leaks == sandbox_runtime_mpla_poc::LeakCounts::default(),
                format!("{reconciliation:?}"),
                "balanced, zero unexplained, zero leaks",
            ),
            assertion(
                "case_hard_stop",
                elapsed_ns <= 60_000_000_000,
                elapsed_ns,
                "<=60000000000ns",
            ),
        ],
        details: json!({
            "source_owner": source_owner,
            "source_locator": source_locator,
            "prepared": prepared,
            "ready": ready,
            "selected": selected,
            "blocked_retirement": blocked_retirement.err().map(|error| error.to_string()),
            "terminal": terminal,
            "replay": replay,
            "resolved": resolved,
            "old_reader_sha256": old_reader_digest,
            "target_reader_sha256": target_reader_digest,
            "reconciliation": reconciliation,
            "elapsed_ns": elapsed_ns,
        }),
    })
}

fn hv05(context: &HeavyContext) -> CampaignResult<CaseExecution> {
    let base = open_allocation(
        &context.arena_root(),
        &context.preparation.hv05_base_allocation_id,
    )?;
    let mut prior = context.preparation.hv05_base_semantic.clone();
    let mut selected_ref = context.preparation.hv05_base_ref.clone();
    let mut recent = Vec::<AllocationHandle>::new();
    let mut carrier = None::<AllocationHandle>;
    let mut all_carriers = Vec::<AllocationHandle>::new();
    let mut durations_ns = Vec::with_capacity(HV05_DELTAS);
    let mut activation_ns = Vec::with_capacity(HV05_DELTAS);
    let mut stationary_ns = Vec::with_capacity(HV05_DELTAS);
    let mut incremental_ns = Vec::with_capacity(HV05_DELTAS);
    let mut locator_ref_ns = Vec::with_capacity(HV05_DELTAS);
    let mut affected_input_bytes = Vec::with_capacity(HV05_DELTAS);
    let mut immutable_payload_bytes = Vec::with_capacity(HV05_DELTAS);
    let mut projection_lower_counts = Vec::with_capacity(HV05_DELTAS);
    let mut carrier_build_ns = Vec::new();
    let campaign_started = Instant::now();

    for index in 0..HV05_DELTAS {
        let (recipe, payload_allocations) =
            hv05_projection(&base, carrier.as_ref(), &recent, &prior.receipt.roots);
        projection_lower_counts.push(
            1_u64
                + u64::from(carrier.is_some())
                + u64::try_from(recent.len()).expect("recent length fits u64"),
        );
        let activated_started = Instant::now();
        let activated = activate_exact(ExactActivationRequest {
            activation_operation_id: ActivationOperationId::from_string(format!(
                "{}-hv05-activate-{index:02}",
                context.run_id
            )),
            allocation_operation_id: OperationId::from_string(format!(
                "{}-hv05-allocation-{index:02}",
                context.run_id
            )),
            selected_ref: selected_ref.clone(),
            recipe,
            payload_allocations,
            arena_root: context.arena_root(),
            control_root: context.control_root.clone(),
            cgroup_procs_path: Some(context.cgroup_procs_path.clone()),
            readiness_path: hv05_path(index * HV05_FILES_PER_DELTA),
            readiness_contains: None,
            readiness_timeout: Duration::from_secs(2),
        })?;
        activation_ns.push(ns(activated_started.elapsed()));
        let mut session = activated.session;
        let allocation = session.allocation().clone();
        let writer = session.mutable_lease().writer.clone();
        let paths = (0..HV05_FILES_PER_DELTA)
            .map(|within| hv05_path(index * HV05_FILES_PER_DELTA + within))
            .collect::<Vec<_>>();
        let work = context.root().join("hv05").join(format!("{index:02}"));
        fs::create_dir_all(&work)?;
        let workspace = session
            .workspace_root()
            .ok_or("HV-05 workspace disappeared")?
            .to_path_buf();
        let before = capture_affected_paths(&workspace, &paths, &work.join("before"))?;
        for path in &paths {
            let command = session.execute(
                &writer,
                Path::new("/usr/bin/dd"),
                &[
                    format!("if=/dev/zero"),
                    format!("of={}", path.display()),
                    "bs=102400".to_owned(),
                    "count=1".to_owned(),
                    "conv=notrunc".to_owned(),
                ],
                Duration::from_secs(2),
            )?;
            if !command.success {
                return Err(
                    format!("HV-05 delta {index} edit failed for {}", path.display()).into(),
                );
            }
        }
        let after = capture_affected_paths(&workspace, &paths, &work.join("after"))?;
        let affected_stream = work.join("affected.records");
        let affected_stream_sha256 =
            write_affected_stream_from_snapshots(&affected_stream, &before, &after)?;
        let operation_id =
            OperationId::from_string(format!("{}-hv05-publish-{index:02}", context.run_id));
        let publication_id =
            PublicationId::from_string(format!("{}-hv05-{index:02}", context.run_id));
        let publish_started = Instant::now();
        let (stationary, incremental, stationary_elapsed, incremental_elapsed) =
            parallel_receipt_hit_publication(
                &mut session,
                StationaryPublicationRequest {
                    schema_version: SCHEMA_VERSION,
                    operation_id: operation_id.clone(),
                    publication_id: publication_id.clone(),
                },
                context.root().join("operations"),
                ReceiptHitSealInput {
                    schema_version: SCHEMA_VERSION,
                    affected_stream: affected_stream.clone(),
                    affected_stream_sha256: affected_stream_sha256.clone(),
                    affected_paths: paths,
                },
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
        stationary_ns.push(ns(stationary_elapsed));
        incremental_ns.push(ns(incremental_elapsed));
        affected_input_bytes.push(incremental.affected_input_bytes);
        immutable_payload_bytes.push(incremental.immutable_payload_bytes_read);
        let ref_started = Instant::now();
        selected_ref = heavy_install_ref(
            &context.root(),
            &allocation,
            &incremental.receipt,
            stationary.stationary.adoption.new_owner.owner_epoch,
            stationary.stationary.stable.after.allocated_bytes,
            &operation_id,
            &publication_id,
        )?;
        locator_ref_ns.push(ns(ref_started.elapsed()));
        durations_ns.push(ns(publish_started.elapsed()));
        prior = PreparedSemantic {
            receipt: incremental.receipt,
            record_stream_path: materialize_record_stream(
                &incremental.root_manifest_path,
                &context.preparation.canonical_object_dir,
            )?,
            root_manifest_path: incremental.root_manifest_path,
        };
        recent.push(allocation);

        if (index + 1) % sandbox_runtime_mpla_poc::projection::MAX_RECENT_DELTAS == 0 {
            let started = Instant::now();
            let next = build_hv05_carrier(
                context,
                &base,
                carrier.as_ref(),
                &recent,
                &selected_ref,
                &prior.receipt.roots,
                (index + 1) * HV05_FILES_PER_DELTA,
            )?;
            carrier_build_ns.push(ns(started.elapsed()));
            if let Some(old) = carrier.replace(next.clone()) {
                all_carriers.push(old);
            }
            recent.clear();
        }
    }
    let carrier = carrier.ok_or("HV-05 did not produce the final delta carrier")?;
    let final_delta = all_carriers
        .last()
        .or(Some(&carrier))
        .ok_or("HV-05 lacks a final allocation")?;
    let validation_started = Instant::now();
    let validation = activate_exact(ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(format!(
            "{}-hv05-validation",
            context.run_id
        )),
        allocation_operation_id: OperationId::from_string(format!(
            "{}-hv05-validation-allocation",
            context.run_id
        )),
        selected_ref: selected_ref.clone(),
        recipe: ProjectionRecipe {
            schema_version: SCHEMA_VERSION,
            roots: prior.receipt.roots.clone(),
            base_allocation_id: base.descriptor.allocation_id.clone(),
            net_delta_carrier_id: Some(carrier.descriptor.allocation_id.clone()),
            recent_delta_ids: Vec::new(),
        },
        payload_allocations: vec![base.clone(), carrier.clone()],
        arena_root: context.arena_root(),
        control_root: context.control_root.clone(),
        cgroup_procs_path: Some(context.cgroup_procs_path.clone()),
        readiness_path: hv05_path(HV05_DELTAS * HV05_FILES_PER_DELTA - 1),
        readiness_contains: Some(vec![0; 32]),
        readiness_timeout: Duration::from_secs(2),
    })?;
    let validation_activation_ns = ns(validation_started.elapsed());
    let validation_allocation_id = validation
        .session
        .allocation()
        .descriptor
        .allocation_id
        .clone();
    let validation_deleter = validation.session.mutable_lease().deleter.clone();
    let full = build_with_output(&SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string(format!("{}-hv05-full-check", context.run_id)),
        allocation_id: validation_allocation_id.clone(),
        sealed_tree: validation
            .session
            .workspace_root()
            .ok_or("HV-05 validation workspace missing")?
            .to_path_buf(),
        spool_dir: context.root().join("spool").join("hv05-final-full"),
        canonical_object_dir: context.preparation.canonical_object_dir.clone(),
        attribution: attribution(),
    })?;
    drop(validation);
    destroy_workspace_allocation(
        &context.arena_root(),
        &validation_allocation_id,
        &validation_deleter,
    )?;
    let campaign_ns = ns(campaign_started.elapsed());
    let first_eight_median = median_u64(&durations_ns[..8]);
    let middle_eight_median = median_u64(&durations_ns[28..36]);
    let last_eight_median = median_u64(&durations_ns[56..]);
    let state = Hv05State {
        semantic: prior.clone(),
        selected_ref: selected_ref.clone(),
        base_allocation_id: base.descriptor.allocation_id.clone(),
        final_delta_allocation_id: final_delta.descriptor.allocation_id.clone(),
        carrier_allocation_id: carrier.descriptor.allocation_id.clone(),
    };
    durable::replace_json(&context.root().join("HV05_STATE.json"), &state)?;
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "case_budget",
                campaign_ns < 50_000_000_000,
                campaign_ns,
                "<50000000000ns",
            ),
            assertion(
                "every_candidate_within_gate",
                durations_ns.iter().all(|value| *value <= HV05_TPUB_MAX_NS),
                format!("{durations_ns:?}"),
                format!("each <={HV05_TPUB_MAX_NS}ns"),
            ),
            assertion(
                "candidate_median_objective",
                median_u64(&durations_ns) <= HV05_MEDIAN_OBJECTIVE_NS,
                median_u64(&durations_ns),
                HV05_MEDIAN_OBJECTIVE_NS,
            ),
            assertion(
                "zero_prior_immutable_payload_reads",
                immutable_payload_bytes.iter().all(|value| *value == 0),
                format!("{immutable_payload_bytes:?}"),
                "all zero",
            ),
            assertion(
                "bounded_projection_depth",
                projection_lower_counts.iter().all(|count| *count <= 9),
                format!("{projection_lower_counts:?}"),
                "all <=9",
            ),
            assertion(
                "full_rebuild_root_equivalence",
                prior.receipt.roots == full.receipt.roots,
                format!("{:?}", prior.receipt.roots),
                format!("{:?}", full.receipt.roots),
            ),
            assertion(
                "no_late_publication_decay",
                last_eight_median
                    <= first_eight_median
                        .saturating_mul(2)
                        .saturating_add(5_000_000),
                last_eight_median,
                first_eight_median
                    .saturating_mul(2)
                    .saturating_add(5_000_000),
            ),
        ],
        details: json!({
            "fixture": {
                "base_logical_bytes": HV05_BASE_BYTES,
                "deltas": HV05_DELTAS,
                "files_per_delta": HV05_FILES_PER_DELTA,
                "bytes_per_file": HV05_FILE_BYTES,
            },
            "duration_ns": durations_ns,
            "activation_duration_ns": activation_ns,
            "stationary_duration_ns": stationary_ns,
            "incremental_duration_ns": incremental_ns,
            "locator_ref_duration_ns": locator_ref_ns,
            "carrier_build_duration_ns": carrier_build_ns,
            "final_validation_activation_ns": validation_activation_ns,
            "affected_input_bytes": affected_input_bytes,
            "immutable_payload_bytes_read": immutable_payload_bytes,
            "projection_lower_counts": projection_lower_counts,
            "first_eight_median_ns": first_eight_median,
            "middle_eight_median_ns": middle_eight_median,
            "last_eight_median_ns": last_eight_median,
            "all_median_ns": median_u64(&durations_ns),
            "case_duration_ns": campaign_ns,
            "final_incremental": prior.receipt,
            "forced_full": full.receipt,
            "selected_ref": selected_ref,
            "final_carrier": carrier.descriptor.allocation_id,
            "retained_superseded_carriers": all_carriers
                .iter()
                .map(|allocation| allocation.descriptor.allocation_id.clone())
                .collect::<Vec<_>>(),
        }),
    })
}

fn hv08(context: &HeavyContext) -> CampaignResult<CaseExecution> {
    let case_started = Instant::now();
    let catalog_binding: CatalogBinding = durable::read_json(&context.catalog_binding_path)?;
    let r0 = open_allocation(&context.arena_root(), &context.preparation.r0_allocation_id)?;
    let readiness_paths = r0_large_regular_paths(&r0.upper_dir)?;
    let readiness_path = readiness_paths
        .first()
        .ok_or("R0 has no regular file at least 100 KiB")?
        .clone();
    let changes = collect_control_changes(
        &r0.upper_dir,
        &ControlCollectionLimits {
            max_entries: 8 * 1024,
            max_logical_bytes: 2 * HEAVY_GIB,
            max_path_bytes: 4 * 1024,
        },
    )?;

    let mut control_closing = Vec::<ControlOperationReceipt>::new();
    let mut control_cold = Vec::<ControlOperationReceipt>::new();
    let mut control_same = Vec::<ControlOperationReceipt>::new();
    let mut current_control_state_roots = Vec::new();
    for sample in 0..3_u8 {
        sandbox_runtime_layerstack::reset_process_state_for_tests();
        let state_root = context
            .root()
            .join("hv08-current-i2")
            .join(format!("pair-{sample}"));
        fs::create_dir_all(&state_root)?;
        let closing = run_current_i2_closing(
            &CurrentI2ClosingRequest {
                state_root: state_root.clone(),
                publication_id: [sample.saturating_add(1); 16],
                public_root_hash: changes.profile.source_manifest_sha256.clone(),
                catalog_binding: catalog_binding.clone(),
                boundary: heavy_control_boundary(
                    ControlCacheMatch::NotApplicable,
                    "closed R0 corpus",
                    "durable hidden publication",
                ),
            },
            &changes,
        )?;
        let cold = run_current_i2_materialization(
            &CurrentI2MaterializationRequest {
                state_root: state_root.clone(),
                intent: ControlIntent::ColdActivation,
                timeout: Duration::from_secs(120),
                cache_expectation: ControlCacheExpectation::ColdBuilt,
                expected_selection: None,
                catalog_binding: catalog_binding.clone(),
                boundary: heavy_control_boundary(
                    ControlCacheMatch::Matched,
                    "durable hidden publication",
                    "externally usable R0 carrier",
                ),
            },
            external_directory_readiness,
        )?;
        let selection = cold
            .materialization
            .as_ref()
            .ok_or("HV-08 cold control omitted its materialization")?
            .selection_key();
        let same = run_current_i2_materialization(
            &CurrentI2MaterializationRequest {
                state_root: state_root.clone(),
                intent: ControlIntent::SameKeyActivation,
                timeout: Duration::from_secs(120),
                cache_expectation: ControlCacheExpectation::SameKeyReused,
                expected_selection: Some(selection),
                catalog_binding: catalog_binding.clone(),
                boundary: heavy_control_boundary(
                    ControlCacheMatch::Matched,
                    "selected R0 key",
                    "externally usable R0 carrier",
                ),
            },
            external_directory_readiness,
        )?;
        control_closing.push(closing);
        control_cold.push(cold);
        control_same.push(same);
        current_control_state_roots.push(state_root);
    }
    sandbox_runtime_layerstack::reset_process_state_for_tests();

    let initial_semantic = full_build(
        &context.root(),
        &context.preparation.canonical_object_dir,
        &r0,
        "hv08-r0-initial",
    )?;
    let operation_id = OperationId::from_string(format!("{}-hv08-base", context.run_id));
    let publication_id = PublicationId::from_string(format!("{}-hv08-base", context.run_id));
    let lease = issue_workspace_lease(&r0, SessionId::new(), &operation_id)?;
    let empty_lower = context.root().join("empty-lower");
    fs::create_dir_all(&empty_lower)?;
    let mut base_session = sandbox_runtime_mpla_poc::MplaSession::open(
        &context.control_root,
        r0.clone(),
        lease,
        vec![empty_lower],
        Some(context.cgroup_procs_path.clone()),
    )?;
    let base_stationary = stationary_adopt(
        &mut base_session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
        },
        &context.root().join("operations"),
        &mut FaultInjector::default(),
    )?;
    let mut selected_ref = heavy_install_ref(
        &context.root(),
        &r0,
        &initial_semantic.receipt,
        base_stationary.adoption.new_owner.owner_epoch,
        base_stationary.stable.after.allocated_bytes,
        &operation_id,
        &publication_id,
    )?;
    drop(base_session);

    let mut empty_deltas = Vec::new();
    for index in 0..7_u8 {
        empty_deltas.push(create_empty_payload_layer(context, index)?);
    }
    let activation_depths = [1_usize, 4, 8, 8, 8];
    let mut activation_receipts = Vec::new();
    let mut activation_ns = Vec::new();
    for (sample, depth) in activation_depths.into_iter().enumerate() {
        let recent_count = depth.saturating_sub(1);
        let recent = &empty_deltas[..recent_count];
        let activated = activate_exact(ExactActivationRequest {
            activation_operation_id: ActivationOperationId::from_string(format!(
                "{}-hv08-activate-{sample}",
                context.run_id
            )),
            allocation_operation_id: OperationId::from_string(format!(
                "{}-hv08-fresh-{sample}",
                context.run_id
            )),
            selected_ref: selected_ref.clone(),
            recipe: ProjectionRecipe {
                schema_version: SCHEMA_VERSION,
                roots: initial_semantic.receipt.roots.clone(),
                base_allocation_id: r0.descriptor.allocation_id.clone(),
                net_delta_carrier_id: None,
                recent_delta_ids: recent
                    .iter()
                    .map(|allocation| allocation.descriptor.allocation_id.clone())
                    .collect(),
            },
            payload_allocations: std::iter::once(r0.clone())
                .chain(recent.iter().cloned())
                .collect(),
            arena_root: context.arena_root(),
            control_root: context.control_root.clone(),
            cgroup_procs_path: Some(context.cgroup_procs_path.clone()),
            readiness_path: readiness_path.clone(),
            readiness_contains: None,
            readiness_timeout: Duration::from_secs(5),
        })?;
        activation_ns.push(activated.receipt.elapsed_ns);
        activation_receipts.push(activated.receipt.clone());
        let fresh_id = activated
            .session
            .allocation()
            .descriptor
            .allocation_id
            .clone();
        let deleter = activated.session.mutable_lease().deleter.clone();
        drop(activated);
        destroy_workspace_allocation(&context.arena_root(), &fresh_id, &deleter)?;
    }

    let mutation = activate_exact(ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(format!(
            "{}-hv08-mutation",
            context.run_id
        )),
        allocation_operation_id: OperationId::from_string(format!("{}-hv08-delta", context.run_id)),
        selected_ref: selected_ref.clone(),
        recipe: ProjectionRecipe {
            schema_version: SCHEMA_VERSION,
            roots: initial_semantic.receipt.roots.clone(),
            base_allocation_id: r0.descriptor.allocation_id.clone(),
            net_delta_carrier_id: None,
            recent_delta_ids: Vec::new(),
        },
        payload_allocations: vec![r0.clone()],
        arena_root: context.arena_root(),
        control_root: context.control_root.clone(),
        cgroup_procs_path: Some(context.cgroup_procs_path.clone()),
        readiness_path: readiness_path.clone(),
        readiness_contains: None,
        readiness_timeout: Duration::from_secs(5),
    })?;
    let mut mutation_session = mutation.session;
    let delta = mutation_session.allocation().clone();
    let writer = mutation_session.mutable_lease().writer.clone();
    let workspace = mutation_session
        .workspace_root()
        .ok_or("HV-08 mutation workspace is absent")?
        .to_path_buf();
    let affected_paths = readiness_paths.into_iter().take(10).collect::<Vec<_>>();
    let work = context.root().join("hv08-affected");
    fs::create_dir_all(&work)?;
    let before = capture_affected_paths(&workspace, &affected_paths, &work.join("before"))?;
    for path in &affected_paths {
        let command = mutation_session.execute(
            &writer,
            Path::new("/usr/bin/dd"),
            &[
                "if=/dev/zero".to_owned(),
                format!("of={}", path.display()),
                "bs=102400".to_owned(),
                "count=1".to_owned(),
                "conv=notrunc".to_owned(),
                "status=none".to_owned(),
            ],
            Duration::from_secs(5),
        )?;
        if !command.success {
            return Err(format!("HV-08 mutation failed for {}", path.display()).into());
        }
    }
    let after = capture_affected_paths(&workspace, &affected_paths, &work.join("after"))?;
    let affected_stream = work.join("affected.records");
    let affected_stream_sha256 =
        write_affected_stream_from_snapshots(&affected_stream, &before, &after)?;
    let delta_operation = OperationId::from_string(format!("{}-hv08-publish", context.run_id));
    let delta_publication = PublicationId::from_string(format!("{}-hv08", context.run_id));
    let publish_started = Instant::now();
    let (stationary, incremental, stationary_elapsed, incremental_elapsed) =
        parallel_receipt_hit_publication(
            &mut mutation_session,
            StationaryPublicationRequest {
                schema_version: SCHEMA_VERSION,
                operation_id: delta_operation.clone(),
                publication_id: delta_publication.clone(),
            },
            context.root().join("operations"),
            ReceiptHitSealInput {
                schema_version: SCHEMA_VERSION,
                affected_stream: affected_stream.clone(),
                affected_stream_sha256: affected_stream_sha256.clone(),
                affected_paths: affected_paths.clone(),
            },
            IncrementalBuildRequest {
                schema_version: SCHEMA_VERSION,
                operation_id: delta_operation.clone(),
                prior_manifest: initial_semantic.root_manifest_path.clone(),
                expected_prior_roots: initial_semantic.receipt.roots.clone(),
                expected_prior_record_stream_sha256: initial_semantic
                    .receipt
                    .record_stream_sha256
                    .clone(),
                affected_stream,
                affected_stream_sha256,
                affected_ranges_complete: true,
                canonical_object_dir: context.preparation.canonical_object_dir.clone(),
                attribution: attribution(),
            },
        )?;
    let publication_ns = ns(publish_started.elapsed());
    selected_ref = heavy_install_ref(
        &context.root(),
        &delta,
        &incremental.receipt,
        stationary.stationary.adoption.new_owner.owner_epoch,
        stationary.stationary.stable.after.allocated_bytes,
        &delta_operation,
        &delta_publication,
    )?;
    let prepared = PreparedSemantic {
        record_stream_path: materialize_record_stream(
            &incremental.root_manifest_path,
            &context.preparation.canonical_object_dir,
        )?,
        root_manifest_path: incremental.root_manifest_path.clone(),
        receipt: incremental.receipt.clone(),
    };
    drop(mutation_session);

    let published_profile = profile_tree(&delta.upper_dir)?;
    let validation = activate_exact(ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(format!(
            "{}-hv08-validation",
            context.run_id
        )),
        allocation_operation_id: OperationId::from_string(format!(
            "{}-hv08-validation-fresh",
            context.run_id
        )),
        selected_ref: selected_ref.clone(),
        recipe: ProjectionRecipe {
            schema_version: SCHEMA_VERSION,
            roots: prepared.receipt.roots.clone(),
            base_allocation_id: r0.descriptor.allocation_id.clone(),
            net_delta_carrier_id: None,
            recent_delta_ids: vec![delta.descriptor.allocation_id.clone()],
        },
        payload_allocations: vec![r0.clone(), delta.clone()],
        arena_root: context.arena_root(),
        control_root: context.control_root.clone(),
        cgroup_procs_path: Some(context.cgroup_procs_path.clone()),
        readiness_path: readiness_path.clone(),
        readiness_contains: Some(vec![0; 32]),
        readiness_timeout: Duration::from_secs(5),
    })?;
    let validation_root = validation
        .session
        .workspace_root()
        .ok_or("HV-08 validation workspace is absent")?
        .to_path_buf();
    let validation_id = validation
        .session
        .allocation()
        .descriptor
        .allocation_id
        .clone();
    let validation_deleter = validation.session.mutable_lease().deleter.clone();
    let forced_full = build_with_output(&SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string(format!("{}-hv08-full", context.run_id)),
        allocation_id: validation_id.clone(),
        sealed_tree: validation_root.clone(),
        spool_dir: context.root().join("spool/hv08-full"),
        canonical_object_dir: context.preparation.canonical_object_dir.clone(),
        attribution: attribution(),
    })?;
    let oracle_records = context.case_dir("HV-08").join("oracle.records");
    fs::create_dir_all(context.case_dir("HV-08"))?;
    let oracle = run_oracle(&context.oracle_path, &validation_root, &oracle_records)?;
    let substitute_operation =
        OperationId::from_string(format!("{}-hv08-substitute", context.run_id));
    let substitute = create_allocation(&context.arena_root(), &substitute_operation)?;
    let substitute_lease =
        issue_workspace_lease(&substitute, SessionId::new(), &substitute_operation)?;
    copy_tree_test_only(&validation_root, &substitute.upper_dir)?;
    let substituted = full_build(
        &context.root(),
        &context.preparation.canonical_object_dir,
        &substitute,
        "hv08-substituted",
    )?;
    let physical_independence =
        different_representative_inode(&validation_root, &substitute.upper_dir, &readiness_path)?;
    destroy_workspace_allocation(
        &context.arena_root(),
        &substitute.descriptor.allocation_id,
        &substitute_lease.deleter,
    )?;
    drop(validation);
    destroy_workspace_allocation(&context.arena_root(), &validation_id, &validation_deleter)?;

    let control_cold_ns = control_cold
        .iter()
        .map(|receipt| receipt.span.elapsed_ns)
        .collect::<Vec<_>>();
    let control_same_ns = control_same
        .iter()
        .map(|receipt| receipt.span.elapsed_ns)
        .collect::<Vec<_>>();
    let candidate_cold_ns = activation_ns[..3].to_vec();
    let candidate_same_ns = activation_ns[2..].to_vec();
    let cold_gate = 99_876_753_u64.min(median_u64(&control_cold_ns) / 100);
    let same_gate = 50_000_000_u64.min(median_u64(&control_same_ns) / 100);
    let all_zero_work = activation_receipts.iter().all(|receipt| {
        receipt.projection.reconstructed_payload_bytes == 0
            && receipt.projection.hydrated_payload_bytes == 0
            && receipt.projection.base_bytes_copied == 0
            && !receipt.projection.projection_built_during_activation
    });
    let oracle_equal = oracle["root_id"].as_str() == Some(prepared.receipt.roots.root_id.as_str())
        && oracle["attribution_root_id"].as_str()
            == Some(prepared.receipt.roots.attribution_root_id.as_str())
        && oracle["record_stream_sha256"].as_str()
            == Some(prepared.receipt.record_stream_sha256.as_str())
        && files_equal_bounded(&prepared.record_stream_path, &oracle_records)?;
    let substitution_equal = prepared.receipt.roots == substituted.receipt.roots
        && files_equal_bounded(
            &prepared.record_stream_path,
            &substituted.record_stream_path,
        )?;
    let state = Hv08State {
        semantic: prepared.clone(),
        selected_ref: selected_ref.clone(),
        base_allocation_id: r0.descriptor.allocation_id.clone(),
        delta_allocation_id: delta.descriptor.allocation_id.clone(),
        readiness_path: readiness_path.clone(),
        current_control_state_roots: current_control_state_roots.clone(),
    };
    durable::replace_json(&context.root().join("HV08_STATE.json"), &state)?;
    let case_ns = ns(case_started.elapsed());
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "exact_r0_profile",
                context.preparation.r0_profile.regular_files == 3_602
                    && context.preparation.r0_profile.directories == 694
                    && context.preparation.r0_profile.logical_bytes == 912_350_100,
                format!("{:?}", context.preparation.r0_profile),
                "3602 files, 694 directories, 912350100 logical bytes",
            ),
            assertion(
                "five_exact_activation_repeats",
                activation_receipts.len() == 5,
                activation_receipts.len(),
                5,
            ),
            assertion(
                "exact_projection_depths",
                activation_receipts
                    .iter()
                    .map(|receipt| usize::from(receipt.projection.kernel_lower_count))
                    .collect::<Vec<_>>()
                    == vec![1, 4, 8, 8, 8],
                format!("{activation_depths:?}"),
                "[1, 4, 8, 8, 8]",
            ),
            assertion(
                "cold_activation_absolute_and_matched_gate",
                candidate_cold_ns.iter().all(|value| *value <= cold_gate),
                format!("candidate={candidate_cold_ns:?} control={control_cold_ns:?}"),
                format!("each <= {cold_gate}ns"),
            ),
            assertion(
                "same_key_absolute_and_matched_gate",
                candidate_same_ns.iter().all(|value| *value <= same_gate),
                format!("candidate={candidate_same_ns:?} control={control_same_ns:?}"),
                format!("each <= {same_gate}ns"),
            ),
            assertion(
                "activation_zero_reconstruction_hydration",
                all_zero_work,
                all_zero_work,
                true,
            ),
            assertion(
                "stationary_r0_identity",
                base_stationary.stable.before.allocation_id
                    == base_stationary.stable.after.allocation_id
                    && base_stationary.stable.after.allocation_id == r0.descriptor.allocation_id,
                format!("{:?}", base_stationary.stable),
                "same R0 allocation before and after publication",
            ),
            assertion(
                "receipt_hit_affected_only",
                incremental.immutable_payload_bytes_read == 0
                    && incremental.affected_input_bytes <= 2 * HEAVY_MIB,
                format!(
                    "affected={} immutable={}",
                    incremental.affected_input_bytes, incremental.immutable_payload_bytes_read
                ),
                "affected <=2MiB and immutable=0",
            ),
            assertion(
                "no_second_corpus_sized_publication_copy",
                published_profile.allocated_bytes < 32 * HEAVY_MIB,
                published_profile.allocated_bytes,
                "<33554432",
            ),
            assertion(
                "full_rebuild_root_equivalence",
                prepared.receipt.roots == forced_full.receipt.roots,
                format!("{:?}", prepared.receipt.roots),
                format!("{:?}", forced_full.receipt.roots),
            ),
            assertion(
                "independent_oracle_equivalence",
                oracle_equal,
                oracle_equal,
                true,
            ),
            assertion(
                "physical_substitution_semantics",
                physical_independence && substitution_equal,
                format!(
                    "independent_inode={physical_independence} equivalent={substitution_equal}"
                ),
                "independent inode and equal canonical bytes/roots",
            ),
            assertion(
                "case_target",
                case_ns <= 60_000_000_000,
                case_ns,
                "<=60000000000ns",
            ),
            assertion(
                "case_diagnostic_ceiling",
                case_ns <= 120_000_000_000,
                case_ns,
                "<=120000000000ns",
            ),
        ],
        details: json!({
            "fixture": context.preparation.r0_profile,
            "control": {
                "closing": control_closing,
                "cold": control_cold,
                "same_key": control_same,
                "cold_ns": control_cold_ns,
                "same_key_ns": control_same_ns,
            },
            "candidate": {
                "activation_depths": activation_depths,
                "activation_ns": activation_ns,
                "activation_receipts": activation_receipts,
                "cold_ns": candidate_cold_ns,
                "same_key_ns": candidate_same_ns,
                "cold_gate_ns": cold_gate,
                "same_gate_ns": same_gate,
            },
            "publication": {
                "duration_ns": publication_ns,
                "stationary_duration_ns": ns(stationary_elapsed),
                "incremental_duration_ns": ns(incremental_elapsed),
                "stationary": stationary,
                "incremental": {
                    "receipt": incremental.receipt,
                    "root_manifest_path": incremental.root_manifest_path,
                    "affected_record_count": incremental.affected_record_count,
                    "affected_input_bytes": incremental.affected_input_bytes,
                    "prior_node_bytes_read": incremental.prior_node_bytes_read,
                    "immutable_payload_bytes_read": incremental.immutable_payload_bytes_read,
                    "resource_maxima": {
                        "application_pool_bytes": incremental.resource_maxima.application_pool_bytes,
                        "peak_managed_bytes": incremental.resource_maxima.peak_managed_bytes,
                        "scan_window_bytes": incremental.resource_maxima.scan_window_bytes,
                        "spool_run_bytes": incremental.resource_maxima.spool_run_bytes,
                        "merge_fan_in": incremental.resource_maxima.merge_fan_in,
                        "peak_open_data_fds": incremental.resource_maxima.peak_open_data_fds,
                        "peak_data_workers": incremental.resource_maxima.peak_data_workers,
                        "trie_fan_out": incremental.resource_maxima.trie_fan_out,
                    },
                },
                "published_delta_profile": published_profile,
            },
            "semantic": {
                "incremental": prepared.receipt,
                "forced_full": forced_full.receipt,
                "oracle": oracle,
                "substituted": substituted.receipt,
                "physical_independence": physical_independence,
            },
            "selected_ref": selected_ref,
            "case_duration_ns": case_ns,
        }),
    })
}

fn hv10(context: &HeavyContext) -> CampaignResult<CaseExecution> {
    let case_started = Instant::now();
    let state: Hv08State = durable::read_json(&context.root().join("HV08_STATE.json"))?;
    let catalog_binding: CatalogBinding = durable::read_json(&context.catalog_binding_path)?;
    let base = open_allocation(&context.arena_root(), &state.base_allocation_id)?;
    let delta = open_allocation(&context.arena_root(), &state.delta_allocation_id)?;
    let lifecycle_root = context.root().join("hv10-lifecycle");
    let initialize = invoke_heavy_lifecycle(
        context,
        lifecycle_metadata_args(
            &lifecycle_root,
            &format!("{}-hv10-initialize", context.run_id),
            "initialize",
            HEAVY_BRANCH,
            None,
            None,
            Some(delta.descriptor.allocation_id.as_str()),
            Some(state.semantic.receipt.roots.root_id.as_str()),
            Some(state.semantic.receipt.roots.attribution_root_id.as_str()),
            false,
        ),
    )?;
    let allocations_before = allocation_directory_count(&context.payload_root)?;
    let mut forks = Vec::<CliInvocation>::with_capacity(1_000);
    let mut allocation_samples = Vec::new();
    for index in 0..996_usize {
        let invocation = invoke_heavy_lifecycle(
            context,
            lifecycle_metadata_args(
                &lifecycle_root,
                &format!("{}-hv10-fork-{index:04}", context.run_id),
                "fork",
                &format!("inactive-{index:04}"),
                Some(HEAVY_BRANCH),
                None,
                None,
                None,
                None,
                false,
            ),
        )?;
        forks.push(invocation);
        if index + 1 == 1 || index + 1 == 64 {
            allocation_samples.push((
                index + 1,
                allocation_directory_count(&context.payload_root)?,
            ));
        }
    }
    let concurrent = std::thread::scope(|scope| {
        let tasks = (996_usize..1_000)
            .map(|index| {
                let lifecycle_root = lifecycle_root.clone();
                scope.spawn(move || {
                    invoke_heavy_lifecycle(
                        context,
                        lifecycle_metadata_args(
                            &lifecycle_root,
                            &format!("{}-hv10-fork-{index:04}", context.run_id),
                            "fork",
                            &format!("inactive-{index:04}"),
                            Some(HEAVY_BRANCH),
                            None,
                            None,
                            None,
                            None,
                            false,
                        ),
                    )
                    .map_err(|error| error.to_string())
                })
            })
            .collect::<Vec<_>>();
        tasks
            .into_iter()
            .map(|task| {
                task.join()
                    .map_err(|_| "HV-10 concurrent fork task panicked".to_owned())?
            })
            .collect::<Result<Vec<_>, String>>()
    })
    .map_err(|error| -> Box<dyn Error> { error.into() })?;
    forks.extend(concurrent);
    let allocations_after_forks = allocation_directory_count(&context.payload_root)?;
    allocation_samples.push((1_000, allocations_after_forks));

    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: state.semantic.receipt.roots.clone(),
        base_allocation_id: base.descriptor.allocation_id.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: vec![delta.descriptor.allocation_id.clone()],
    };
    let payload_allocations = vec![base.clone(), delta.clone()];
    let mut fork_activation_ns = Vec::new();
    let mut activation_receipts = Vec::new();
    for selected_count in [1_usize, 64, 1_000] {
        let activated = activate_exact(ExactActivationRequest {
            activation_operation_id: ActivationOperationId::from_string(format!(
                "{}-hv10-fork-activate-{selected_count}",
                context.run_id
            )),
            allocation_operation_id: OperationId::from_string(format!(
                "{}-hv10-fork-fresh-{selected_count}",
                context.run_id
            )),
            selected_ref: state.selected_ref.clone(),
            recipe: recipe.clone(),
            payload_allocations: payload_allocations.clone(),
            arena_root: context.arena_root(),
            control_root: context.control_root.clone(),
            cgroup_procs_path: Some(context.cgroup_procs_path.clone()),
            readiness_path: state.readiness_path.clone(),
            readiness_contains: Some(vec![0; 32]),
            readiness_timeout: Duration::from_secs(5),
        })?;
        fork_activation_ns.push(activated.receipt.elapsed_ns);
        activation_receipts.push(activated.receipt.clone());
        let fresh_id = activated
            .session
            .allocation()
            .descriptor
            .allocation_id
            .clone();
        let deleter = activated.session.mutable_lease().deleter.clone();
        drop(activated);
        destroy_workspace_allocation(&context.arena_root(), &fresh_id, &deleter)?;
    }

    let mut rollbacks = Vec::new();
    let mut squashes = Vec::new();
    for sample in 0..3_u8 {
        rollbacks.push(invoke_heavy_lifecycle(
            context,
            lifecycle_metadata_args(
                &lifecycle_root,
                &format!("{}-hv10-rollback-{sample}", context.run_id),
                "rollback",
                HEAVY_BRANCH,
                None,
                Some("inactive-0000"),
                None,
                None,
                None,
                false,
            ),
        )?);
        squashes.push(invoke_heavy_lifecycle(
            context,
            lifecycle_metadata_args(
                &lifecycle_root,
                &format!("{}-hv10-squash-{sample}", context.run_id),
                "squash",
                HEAVY_BRANCH,
                None,
                None,
                None,
                None,
                None,
                false,
            ),
        )?);
    }
    let response_loss_args = lifecycle_metadata_args(
        &lifecycle_root,
        &format!("{}-hv10-response-loss", context.run_id),
        "fork",
        "response-loss",
        Some(HEAVY_BRANCH),
        None,
        None,
        None,
        None,
        false,
    );
    let response_lost = invoke_heavy_lifecycle(context, response_loss_args.clone())?;
    let response_replayed = invoke_heavy_lifecycle(context, response_loss_args)?;
    let cancelled = invoke_heavy_lifecycle(
        context,
        lifecycle_metadata_args(
            &lifecycle_root,
            &format!("{}-hv10-cancelled", context.run_id),
            "rollback",
            HEAVY_BRANCH,
            None,
            Some("inactive-0000"),
            None,
            None,
            None,
            true,
        ),
    )?;

    let mut current_fork = Vec::new();
    let mut current_rollback = Vec::new();
    for state_root in &state.current_control_state_roots {
        current_fork.push(run_current_i2_materialization(
            &CurrentI2MaterializationRequest {
                state_root: state_root.clone(),
                intent: ControlIntent::Fork,
                timeout: Duration::from_secs(120),
                cache_expectation: ControlCacheExpectation::NaturallyProduced,
                expected_selection: None,
                catalog_binding: catalog_binding.clone(),
                boundary: heavy_control_boundary(
                    ControlCacheMatch::Matched,
                    "selected R0 branch",
                    "externally usable fork carrier",
                ),
            },
            external_directory_readiness,
        )?);
        current_rollback.push(run_current_i2_materialization(
            &CurrentI2MaterializationRequest {
                state_root: state_root.clone(),
                intent: ControlIntent::Rollback,
                timeout: Duration::from_secs(120),
                cache_expectation: ControlCacheExpectation::NaturallyProduced,
                expected_selection: None,
                catalog_binding: catalog_binding.clone(),
                boundary: heavy_control_boundary(
                    ControlCacheMatch::Matched,
                    "selected prior R0 branch",
                    "externally usable rollback carrier",
                ),
            },
            external_directory_readiness,
        )?);
    }
    let current_fork_ns = current_fork
        .iter()
        .map(|receipt| receipt.span.elapsed_ns)
        .collect::<Vec<_>>();
    let current_rollback_ns = current_rollback
        .iter()
        .map(|receipt| receipt.span.elapsed_ns)
        .collect::<Vec<_>>();
    let candidate_fork_ns = forks
        .iter()
        .rev()
        .take(3)
        .map(|receipt| receipt.outer_elapsed_ns)
        .collect::<Vec<_>>();
    let rollback_outer_ns = rollbacks
        .iter()
        .map(|receipt| receipt.outer_elapsed_ns)
        .collect::<Vec<_>>();
    let rollback_service_ns = rollbacks
        .iter()
        .map(|receipt| {
            receipt.response["service_elapsed_ns"]
                .as_u64()
                .unwrap_or(u64::MAX)
        })
        .collect::<Vec<_>>();
    let squash_outer_ns = squashes
        .iter()
        .map(|receipt| receipt.outer_elapsed_ns)
        .collect::<Vec<_>>();
    let squash_service_ns = squashes
        .iter()
        .map(|receipt| {
            receipt.response["service_elapsed_ns"]
                .as_u64()
                .unwrap_or(u64::MAX)
        })
        .collect::<Vec<_>>();

    let benchmark_activation = activate_exact(ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(format!(
            "{}-hv10-benchmark",
            context.run_id
        )),
        allocation_operation_id: OperationId::from_string(format!(
            "{}-hv10-benchmark-fresh",
            context.run_id
        )),
        selected_ref: state.selected_ref.clone(),
        recipe,
        payload_allocations,
        arena_root: context.arena_root(),
        control_root: context.control_root.clone(),
        cgroup_procs_path: Some(context.cgroup_procs_path.clone()),
        readiness_path: state.readiness_path.clone(),
        readiness_contains: Some(vec![0; 32]),
        readiness_timeout: Duration::from_secs(5),
    })?;
    let candidate_root = benchmark_activation
        .session
        .workspace_root()
        .ok_or("HV-10 benchmark workspace is absent")?
        .to_path_buf();
    let benchmark_id = benchmark_activation
        .session
        .allocation()
        .descriptor
        .allocation_id
        .clone();
    let benchmark_deleter = benchmark_activation.session.mutable_lease().deleter.clone();
    let current_root = current_fork[0]
        .materialization
        .as_ref()
        .ok_or("HV-10 current control omitted its carrier")?
        .carrier_path
        .clone();
    let throughput = benchmark_fixed_windows(
        &current_root,
        &candidate_root,
        &state.readiness_path,
        Duration::from_secs(1),
    )?;
    drop(benchmark_activation);
    destroy_workspace_allocation(&context.arena_root(), &benchmark_id, &benchmark_deleter)?;

    let allocations_after = allocation_directory_count(&context.payload_root)?;
    let throughput_ok = ["command_cpu", "sequential_read", "random_read"]
        .into_iter()
        .all(|kind| fixed_window_regression_ok(&throughput, kind));
    let leaks = heavy_leak_counts(context)?;
    let reconciliation = heavy_reconciliation(context, leaks.clone())?;
    durable::replace_json(
        &context.case_dir("HV-10").join("reconciliation.json"),
        &reconciliation,
    )?;
    let fork_ratio_gate = median_u64(&current_fork_ns) / 100;
    let rollback_ratio_gate = median_u64(&current_rollback_ns) / 100;
    let case_ns = ns(case_started.elapsed());
    let all_metadata_success = std::iter::once(&initialize)
        .chain(forks.iter())
        .chain(rollbacks.iter())
        .chain(squashes.iter())
        .all(|receipt| {
            receipt.exit_code == Some(0)
                && receipt.response["status"].as_str() == Some("succeeded")
                && receipt.response["payload_objects_created"].as_u64() == Some(0)
        });
    Ok(CaseExecution {
        assertions: vec![
            assertion(
                "exact_inactive_fork_counts",
                forks.len() == 1_000
                    && allocation_samples.iter().map(|sample| sample.0).collect::<Vec<_>>()
                        == vec![1, 64, 1_000],
                format!("forks={} samples={allocation_samples:?}", forks.len()),
                "1000 forks sampled at 1, 64, 1000",
            ),
            assertion(
                "inactive_forks_metadata_only",
                allocations_before == allocations_after_forks
                    && allocations_before == allocations_after,
                format!(
                    "before={allocations_before} after_forks={allocations_after_forks} after={allocations_after}"
                ),
                "allocation count unchanged",
            ),
            assertion(
                "durable_metadata_outcomes",
                all_metadata_success,
                all_metadata_success,
                true,
            ),
            assertion(
                "selected_fork_activation_gate",
                fork_activation_ns.iter().all(|value| *value <= 10_000_000)
                    && candidate_fork_ns
                        .iter()
                        .all(|value| *value <= fork_ratio_gate),
                format!(
                    "activation={fork_activation_ns:?} candidate={candidate_fork_ns:?} current={current_fork_ns:?}"
                ),
                format!("activation <=10ms and candidate fork <= {fork_ratio_gate}ns"),
            ),
            assertion(
                "fork_activation_zero_reconstruction",
                activation_receipts.iter().all(|receipt| {
                    receipt.projection.reconstructed_payload_bytes == 0
                        && receipt.projection.hydrated_payload_bytes == 0
                        && receipt.projection.base_bytes_copied == 0
                }),
                format!("{activation_receipts:?}"),
                "zero reconstructed/hydrated/copied bytes",
            ),
            assertion(
                "rollback_absolute_and_matched_gate",
                rollback_outer_ns.iter().all(|value| *value <= 20_000_000)
                    && rollback_service_ns.iter().all(|value| *value <= 1_000_000)
                    && rollback_outer_ns
                        .iter()
                        .all(|value| *value <= rollback_ratio_gate),
                format!(
                    "outer={rollback_outer_ns:?} service={rollback_service_ns:?} current={current_rollback_ns:?}"
                ),
                format!("outer <=20ms/{rollback_ratio_gate}ns and service <=1ms"),
            ),
            assertion(
                "squash_absolute_gate",
                squash_outer_ns.iter().all(|value| *value <= 10_000_000)
                    && squash_service_ns.iter().all(|value| *value <= 1_000_000),
                format!("outer={squash_outer_ns:?} service={squash_service_ns:?}"),
                "outer <=10ms and service <=1ms",
            ),
            assertion(
                "response_loss_exact_replay",
                response_lost.response == response_replayed.response,
                format!(
                    "first={} replay={}",
                    response_lost.response, response_replayed.response
                ),
                "identical durable response",
            ),
            assertion(
                "cancellation_durable",
                cancelled.exit_code == Some(0)
                    && cancelled.response["status"].as_str() == Some("cancelled")
                    && cancelled.response["outcome_path"]
                        .as_str()
                        .is_some_and(|path| Path::new(path).exists()),
                cancelled.response.clone(),
                "typed durable cancelled outcome",
            ),
            assertion(
                "normal_operation_no_regression",
                throughput_ok,
                format!("{throughput:?}"),
                "candidate median operations >=95% of current for all workloads",
            ),
            assertion(
                "balanced_storage",
                reconciliation.balanced
                    && reconciliation.unexplained_allocated_bytes == 0
                    && reconciliation.unexplained_inodes == 0,
                format!("{reconciliation:?}"),
                "balanced with X_unexplained=0",
            ),
            assertion(
                "zero_live_resource_leaks",
                reconciliation.leaks == sandbox_runtime_mpla_poc::LeakCounts::default(),
                format!("{:?}", reconciliation.leaks),
                "all zero",
            ),
            assertion(
                "case_budget",
                case_ns <= 30_000_000_000,
                case_ns,
                "<=30000000000ns",
            ),
        ],
        details: json!({
            "fixture": context.preparation.r0_profile,
            "metadata": {
                "initialize": initialize,
                "forks": forks,
                "rollbacks": rollbacks,
                "squashes": squashes,
                "response_lost": response_lost,
                "response_replayed": response_replayed,
                "cancelled": cancelled,
                "allocation_samples": allocation_samples,
            },
            "timings": {
                "fork_activation_ns": fork_activation_ns,
                "candidate_fork_outer_ns": candidate_fork_ns,
                "current_fork_ns": current_fork_ns,
                "rollback_outer_ns": rollback_outer_ns,
                "rollback_service_ns": rollback_service_ns,
                "current_rollback_ns": current_rollback_ns,
                "squash_outer_ns": squash_outer_ns,
                "squash_service_ns": squash_service_ns,
            },
            "throughput": throughput,
            "reconciliation": reconciliation,
            "case_duration_ns": case_ns,
        }),
    })
}

fn heavy_control_boundary(
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

fn external_directory_readiness(
    carrier: &Path,
) -> sandbox_runtime_mpla_poc::PocResult<ExternalReadinessReceipt> {
    Ok(ExternalReadinessReceipt {
        probe: "external_carrier_directory".to_owned(),
        passed: carrier.is_dir(),
        observed: carrier.display().to_string(),
    })
}

fn r0_large_regular_paths(root: &Path) -> CampaignResult<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        } else if metadata.is_file() && metadata.len() >= 100 * 1024 {
            paths.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    paths.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });
    if paths.len() < 10 {
        return Err(format!(
            "R0 needs at least 10 regular files >=100 KiB, observed {}",
            paths.len()
        )
        .into());
    }
    Ok(paths)
}

fn create_empty_payload_layer(
    context: &HeavyContext,
    index: u8,
) -> CampaignResult<AllocationHandle> {
    let operation_id = OperationId::from_string(format!("{}-hv08-empty-{index}", context.run_id));
    let publication_id =
        PublicationId::from_string(format!("{}-hv08-empty-{index}", context.run_id));
    let allocation = create_allocation(&context.arena_root(), &operation_id)?;
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &operation_id)?;
    let empty_lower = context.root().join("empty-lower");
    fs::create_dir_all(&empty_lower)?;
    let mut session = sandbox_runtime_mpla_poc::MplaSession::open(
        &context.control_root,
        allocation.clone(),
        lease,
        vec![empty_lower],
        Some(context.cgroup_procs_path.clone()),
    )?;
    stationary_adopt(
        &mut session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id,
            publication_id,
        },
        &context.root().join("operations"),
        &mut FaultInjector::default(),
    )?;
    drop(session);
    Ok(allocation)
}

fn invoke_heavy_lifecycle(
    context: &HeavyContext,
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

fn benchmark_fixed_windows(
    current_root: &Path,
    candidate_root: &Path,
    readiness_path: &Path,
    window: Duration,
) -> CampaignResult<Vec<FixedWindowSample>> {
    let mut samples = Vec::with_capacity(18);
    for kind in ["command_cpu", "sequential_read", "random_read"] {
        for pair in 0..3_u8 {
            let ordered = if pair % 2 == 0 {
                [("current", current_root), ("candidate", candidate_root)]
            } else {
                [("candidate", candidate_root), ("current", current_root)]
            };
            for (side, root) in ordered {
                samples.push(run_fixed_probe(root, readiness_path, kind, side, window)?);
            }
        }
    }
    Ok(samples)
}

fn run_fixed_probe(
    root: &Path,
    readiness_path: &Path,
    kind: &str,
    side: &str,
    window: Duration,
) -> CampaignResult<FixedWindowSample> {
    let path = root.join(readiness_path);
    let started = Instant::now();
    let mut operations = 0_u64;
    let mut bytes = 0_u64;
    let mut checksum = 0_u64;
    match kind {
        "command_cpu" => {
            while started.elapsed() < window {
                let metadata = fs::symlink_metadata(&path)?;
                checksum ^= metadata.len().rotate_left((operations % 63) as u32);
                operations = operations.saturating_add(1);
            }
        }
        "sequential_read" => {
            let mut buffer = vec![0_u8; 64 * 1024];
            while started.elapsed() < window {
                let mut file = File::open(&path)?;
                loop {
                    let count = file.read(&mut buffer)?;
                    if count == 0 || started.elapsed() >= window {
                        break;
                    }
                    bytes = bytes.saturating_add(u64::try_from(count)?);
                    operations = operations.saturating_add(1);
                    checksum ^= u64::from(buffer[count - 1]);
                }
            }
        }
        "random_read" => {
            let file = File::open(&path)?;
            let length = file.metadata()?.len();
            if length < 4 * 1024 {
                return Err(format!("random-read fixture is too small: {}", path.display()).into());
            }
            let mut buffer = vec![0_u8; 4 * 1024];
            let mut state = 0x9e37_79b9_7f4a_7c15_u64;
            while started.elapsed() < window {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let maximum = length.saturating_sub(u64::try_from(buffer.len())?);
                let offset = if maximum == 0 { 0 } else { state % maximum };
                let count = file.read_at(&mut buffer, offset)?;
                if count == 0 {
                    continue;
                }
                bytes = bytes.saturating_add(u64::try_from(count)?);
                operations = operations.saturating_add(1);
                checksum ^= u64::from(buffer[count - 1]);
            }
        }
        other => return Err(format!("unknown fixed-window workload: {other}").into()),
    }
    Ok(FixedWindowSample {
        kind: kind.to_owned(),
        side: side.to_owned(),
        operations,
        elapsed_ns: ns(started.elapsed()),
        bytes,
        checksum,
    })
}

fn fixed_window_regression_ok(samples: &[FixedWindowSample], kind: &str) -> bool {
    let current = samples
        .iter()
        .filter(|sample| sample.kind == kind && sample.side == "current")
        .map(|sample| sample.operations)
        .collect::<Vec<_>>();
    let candidate = samples
        .iter()
        .filter(|sample| sample.kind == kind && sample.side == "candidate")
        .map(|sample| sample.operations)
        .collect::<Vec<_>>();
    current.len() == 3
        && candidate.len() == 3
        && median_u64(&candidate).saturating_mul(100) >= median_u64(&current).saturating_mul(95)
}

fn heavy_leak_counts(
    context: &HeavyContext,
) -> CampaignResult<sandbox_runtime_mpla_poc::LeakCounts> {
    let active_leases = allocation_roots(&context.arena_root())?
        .iter()
        .filter_map(|allocation_root| {
            let path = allocation_root.join("owner/LEASE");
            path.exists()
                .then(|| durable::read_json::<Value>(&path).ok())
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

fn heavy_reconciliation(
    context: &HeavyContext,
    leaks: sandbox_runtime_mpla_poc::LeakCounts,
) -> CampaignResult<sandbox_runtime_mpla_poc::reconcile::ReconciliationReceipt> {
    let roots = [
        ("payload", &context.payload_root),
        ("control", &context.control_root),
        ("fixtures", &context.fixtures_root),
        ("evidence", &context.evidence_root),
    ];
    let scope = roots[0]
        .1
        .parent()
        .ok_or("heavy payload root has no reconciliation parent")?
        .to_path_buf();
    let categories = roots
        .iter()
        .map(|(category, root)| {
            let relative = root
                .strip_prefix(&scope)
                .map_err(|_| "heavy storage root escapes reconciliation scope")?;
            let volume = relative
                .components()
                .next()
                .ok_or("heavy storage category has no volume component")?;
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
    Ok(sandbox_runtime_mpla_poc::reconcile::reconcile(
        &scope,
        &categories,
        leaks,
    )?)
}

fn heavy_root(control_root: &Path, run_id: &RunId) -> PathBuf {
    control_root.join("m2-lead").join(run_id.as_str())
}

fn heavy_preparation_path(control_root: &Path, run_id: &RunId) -> PathBuf {
    heavy_root(control_root, run_id).join("PREPARATION.json")
}

fn verify_storage_cgroup(cgroup_procs: &Path, cgroup_dir: &Path) -> CampaignResult {
    let high = fs::read_to_string(cgroup_dir.join("memory.high"))?
        .trim()
        .parse::<u64>()?;
    let max = fs::read_to_string(cgroup_dir.join("memory.max"))?
        .trim()
        .parse::<u64>()?;
    if high != 96 * HEAVY_MIB || max != 128 * HEAVY_MIB {
        return Err(format!(
            "heavy storage cgroup limits differ: memory.high={high} memory.max={max}"
        )
        .into());
    }
    let pid = std::process::id().to_string();
    if !fs::read_to_string(cgroup_procs)?
        .lines()
        .any(|line| line.trim() == pid)
    {
        return Err(format!("heavy process {pid} is outside the storage cgroup").into());
    }
    Ok(())
}

fn populate_hv05_base(root: &Path) -> CampaignResult {
    let delta_dir = root.join("chain");
    fs::create_dir_all(&delta_dir)?;
    let delta_total = u64::try_from(HV05_DELTAS * HV05_FILES_PER_DELTA)?
        .checked_mul(HV05_FILE_BYTES)
        .ok_or("HV-05 delta target bytes overflow")?;
    let base_bytes = HV05_BASE_BYTES
        .checked_sub(delta_total)
        .ok_or("HV-05 base is smaller than its target files")?;
    write_pattern_file(&root.join("immutable-base.bin"), base_bytes, 0x5a)?;
    for index in 0..(HV05_DELTAS * HV05_FILES_PER_DELTA) {
        write_pattern_file(&hv05_path_at(root, index), HV05_FILE_BYTES, 0xa5)?;
    }
    Ok(())
}

fn write_pattern_file(path: &Path, bytes: u64, value: u8) -> CampaignResult {
    let mut file = File::create(path)?;
    let buffer = vec![value; 64 * 1024];
    let mut remaining = bytes;
    while remaining != 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64))?;
        file.write_all(&buffer[..count])?;
        remaining -= u64::try_from(count)?;
    }
    file.sync_all()?;
    Ok(())
}

fn write_pattern_file_digest(path: &Path, bytes: u64, value: u8) -> CampaignResult<String> {
    let mut file = File::create(path)?;
    let buffer = vec![value; 64 * 1024];
    let mut hasher = Sha256::new();
    let mut remaining = bytes;
    while remaining != 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64))?;
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        remaining -= u64::try_from(count)?;
    }
    file.sync_all()?;
    Ok(hex_digest(hasher.finalize()))
}

fn sha256_reader(reader: &mut impl Read) -> CampaignResult<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex_digest(hasher.finalize()))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn next_hv07_attempt(case_dir: &Path, point: NamedFaultPoint) -> CampaignResult<u32> {
    let attempts_dir = case_dir
        .join("crash-ledger")
        .join("attempts")
        .join(point.as_str());
    let mut maximum = 0_u32;
    match fs::read_dir(&attempts_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                if let Ok(value) = stem.parse::<u32>() {
                    maximum = maximum.max(value);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    maximum
        .checked_add(1)
        .ok_or_else(|| "HV-07 crash attempt ordinal overflow".into())
}

fn hv05_path(index: usize) -> PathBuf {
    PathBuf::from(format!("chain/file-{index:04}.bin"))
}

fn hv05_path_at(root: &Path, index: usize) -> PathBuf {
    root.join(hv05_path(index))
}

fn hv05_projection(
    base: &AllocationHandle,
    carrier: Option<&AllocationHandle>,
    recent: &[AllocationHandle],
    roots: &sandbox_runtime_mpla_poc::CanonicalRootPair,
) -> (ProjectionRecipe, Vec<AllocationHandle>) {
    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots.clone(),
        base_allocation_id: base.descriptor.allocation_id.clone(),
        net_delta_carrier_id: carrier.map(|allocation| allocation.descriptor.allocation_id.clone()),
        recent_delta_ids: recent
            .iter()
            .rev()
            .map(|allocation| allocation.descriptor.allocation_id.clone())
            .collect(),
    };
    let mut allocations = vec![base.clone()];
    if let Some(carrier) = carrier {
        allocations.push(carrier.clone());
    }
    allocations.extend(recent.iter().cloned());
    (recipe, allocations)
}

fn build_hv05_carrier(
    context: &HeavyContext,
    base: &AllocationHandle,
    carrier: Option<&AllocationHandle>,
    recent: &[AllocationHandle],
    selected_ref: &PairedRefValue,
    roots: &sandbox_runtime_mpla_poc::CanonicalRootPair,
    changed_files: usize,
) -> CampaignResult<AllocationHandle> {
    if recent.len() != sandbox_runtime_mpla_poc::projection::MAX_RECENT_DELTAS {
        return Err("HV-05 carrier requires exactly eight recent deltas".into());
    }
    let (recipe, payload_allocations) = hv05_projection(base, carrier, recent, roots);
    let operation_id = OperationId::from_string(format!(
        "{}-hv05-carrier-{changed_files:04}",
        context.run_id
    ));
    let activated = activate_exact(ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(format!(
            "{}-hv05-carrier-build-{changed_files:04}",
            context.run_id
        )),
        allocation_operation_id: operation_id.clone(),
        selected_ref: selected_ref.clone(),
        recipe,
        payload_allocations,
        arena_root: context.arena_root(),
        control_root: context.control_root.clone(),
        cgroup_procs_path: Some(context.cgroup_procs_path.clone()),
        readiness_path: hv05_path(changed_files - 1),
        readiness_contains: Some(vec![0; 32]),
        readiness_timeout: Duration::from_secs(2),
    })?;
    let mut session = activated.session;
    let allocation = session.allocation().clone();
    let writer = session.mutable_lease().writer.clone();
    for index in 0..changed_files {
        let path = hv05_path(index);
        let temporary = PathBuf::from(format!("{}.carrier", path.display()));
        let copy = session.execute(
            &writer,
            Path::new("/bin/cp"),
            &[
                "--reflink=never".to_owned(),
                "--no-target-directory".to_owned(),
                path.display().to_string(),
                temporary.display().to_string(),
            ],
            Duration::from_secs(2),
        )?;
        if !copy.success {
            return Err(format!("HV-05 carrier copy failed for {}", path.display()).into());
        }
        let rename = session.execute(
            &writer,
            Path::new("/bin/mv"),
            &[
                "--no-target-directory".to_owned(),
                temporary.display().to_string(),
                path.display().to_string(),
            ],
            Duration::from_secs(2),
        )?;
        if !rename.success {
            return Err(format!("HV-05 carrier rename failed for {}", path.display()).into());
        }
    }
    let publication_id = PublicationId::from_string(format!(
        "{}-hv05-carrier-{changed_files:04}",
        context.run_id
    ));
    stationary_adopt(
        &mut session,
        &StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id,
            publication_id,
        },
        &context.root().join("operations"),
        &mut FaultInjector::default(),
    )?;
    Ok(allocation)
}

fn heavy_install_ref(
    root: &Path,
    allocation: &AllocationHandle,
    semantic: &SemanticBuildReceipt,
    owner_epoch: u64,
    accounted_bytes: u64,
    operation_id: &OperationId,
    publication_id: &PublicationId,
) -> CampaignResult<PairedRefValue> {
    let locator_store = LocatorStore::open(root.join("locators"))?;
    let ref_store = PairedRefStore::open(root.join("refs"))?;
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
        .read(HEAVY_BRANCH)?
        .map_or(RefSequence::ZERO, |value| value.sequence);
    match ref_store.commit(
        HEAVY_BRANCH,
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
        RefCommitOutcome::Committed(receipt) => Ok(receipt.value),
        RefCommitOutcome::ExpectedParent { expected, observed } => Err(format!(
            "heavy paired-ref parent conflict: expected {expected}, observed {observed}"
        )
        .into()),
    }
}

fn profile_tree(root: &Path) -> CampaignResult<TreeProfile> {
    let mut profile = TreeProfile {
        directories: 0,
        regular_files: 0,
        symlinks: 0,
        logical_bytes: 0,
        allocated_bytes: 0,
        files_at_least_100_kib: 0,
    };
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            profile.directories += 1;
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        } else if metadata.is_file() {
            profile.regular_files += 1;
            profile.logical_bytes = profile.logical_bytes.saturating_add(metadata.len());
            profile.allocated_bytes = profile
                .allocated_bytes
                .saturating_add(metadata.blocks().saturating_mul(512));
            profile.files_at_least_100_kib += u64::from(metadata.len() >= 100 * 1024);
        } else if metadata.file_type().is_symlink() {
            profile.symlinks += 1;
        } else {
            return Err(format!("unsupported fixture entry: {}", path.display()).into());
        }
    }
    Ok(profile)
}

fn require_r0_profile(profile: &TreeProfile) -> CampaignResult {
    if profile.regular_files != 3_602
        || profile.directories != 694
        || profile.files_at_least_100_kib < 10
        || profile.logical_bytes != 912_350_100
    {
        return Err(
            format!("R0 corpus profile differs from the frozen corpus: {profile:?}").into(),
        );
    }
    Ok(())
}
