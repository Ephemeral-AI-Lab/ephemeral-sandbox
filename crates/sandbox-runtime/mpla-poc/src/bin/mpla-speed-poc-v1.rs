#![cfg(unix)]

mod mpla_activation_scorecard;
mod mpla_fork_scorecard;
mod mpla_hv07_scorecard;
mod mpla_publication_scorecard;
mod mpla_qualification_scorecard;
mod mpla_rollback_scorecard;
mod mpla_speed_scorecard;
mod mpla_squash_scorecard;
mod mpla_stream_scorecard;

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clap::{Args as ClapArgs, Parser, Subcommand};
use sandbox_runtime_mpla_poc::allocation::create_allocation;
use sandbox_runtime_mpla_poc::config::{MEMORY_HIGH_BYTES, MEMORY_MAX_BYTES};
use sandbox_runtime_mpla_poc::lease::issue_workspace_lease;
use sandbox_runtime_mpla_poc::locator::{
    ForwardLocatorEntry, LocatorDelta, LocatorExtent, LocatorStore, PayloadRootId,
    ReverseLocatorEntry,
};
use sandbox_runtime_mpla_poc::publication::{
    stationary_adopt_receipt_hit, ReceiptHitPublicationReceipt, StationaryPublicationRequest,
};
use sandbox_runtime_mpla_poc::ref_store::{PairedRefStore, RefCommitOutcome};
use sandbox_runtime_mpla_poc::semantic::{
    build_incremental, build_with_output, capture_affected_paths, materialize_record_stream,
    write_affected_stream_from_snapshots, IncrementalBuildOutput, IncrementalBuildRequest,
    SemanticBuildOutput, SemanticResourceMaxima,
};
use sandbox_runtime_mpla_poc::{
    bind_product_catalog, collect_control_changes, run_current_i2_closing, AllocationHandle,
    AttributionInput, CatalogBinding, ControlBoundary, ControlCacheMatch, ControlChangeSet,
    ControlCollectionLimits, CurrentI2ClosingRequest, FaultInjector, LocatorRefCandidate,
    NamedFaultInjector, OperationId, PairedRefValue, PublicationId, ReceiptHitSealInput,
    RefSequence, RunId, SemanticBuildRequest, SessionId, PREPARED_FIXTURE_PROFILE, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;

type BenchResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const EXISTING_BYTES: u64 = GIB;
const DELTA_BYTES: u64 = MIB;
const DELTA_FILES: usize = 10;
const PAIRS: usize = 5;
const POOL_BYTES: u64 = 8 * MIB;
const ACTOR_ID: &str = "mpla-speed-poc-v1";
const BRANCH: &str = "speed-poc";
const EVENT_PREFIX: &str = "MPLA_SPEED_EVENT ";
const RESULT_PREFIX: &str = "MPLA_SPEED_RESULT ";
const ERROR_PREFIX: &str = "MPLA_SPEED_ERROR ";
const AUTHORITY_RESULT_PREFIX: &str = "MPLA_AUTHORITY_RESULT ";
const AUTHORITY_ERROR_PREFIX: &str = "MPLA_AUTHORITY_ERROR ";

#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Mode,
}

#[derive(Debug, Subcommand)]
enum Mode {
    AuthorityProbe {
        #[arg(long)]
        probe_root: PathBuf,
    },
    Measure(MeasureArgs),
    ScorecardCase(ScorecardCaseArgs),
    PrepareLifecycleControl(LifecycleControlPreparationArgs),
    PreparePublicationFixture(PublicationPreparationArgs),
    BuildPublicationFixtureCache(PublicationFixtureCacheBuildArgs),
    InspectPreparedFixtureCache,
}

#[derive(Debug, ClapArgs)]
struct MeasureArgs {
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    run_root: PathBuf,
    #[arg(long)]
    oracle: PathBuf,
    #[arg(long)]
    catalog_exporter: PathBuf,
    #[arg(long)]
    catalog: PathBuf,
    #[arg(long)]
    build_commit: String,
    #[arg(long)]
    samples_ledger: PathBuf,
}

#[derive(Debug, ClapArgs)]
struct ScorecardCaseArgs {
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    case: String,
    #[arg(long)]
    candidate_sandbox_id: String,
    #[arg(long)]
    build_commit: String,
}

#[derive(Debug, ClapArgs)]
struct LifecycleControlPreparationArgs {
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    phase: String,
    #[arg(long)]
    candidate_sandbox_id: String,
    #[arg(long)]
    build_commit: String,
}

#[derive(Debug, ClapArgs)]
struct PublicationPreparationArgs {
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    candidate_sandbox_id: String,
    #[arg(long)]
    build_commit: String,
    #[arg(long, value_parser = [PREPARED_FIXTURE_PROFILE])]
    fixture_profile: String,
}

#[derive(Debug, ClapArgs)]
struct PublicationFixtureCacheBuildArgs {
    #[arg(long)]
    candidate_sandbox_id: String,
    #[arg(long)]
    build_commit: String,
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

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct ResourceSample {
    process_rss_bytes: u64,
    process_io_rchar_bytes: u64,
    process_io_wchar_bytes: u64,
    process_io_read_bytes: u64,
    process_io_write_bytes: u64,
    cgroup_memory_current_bytes: u64,
    cgroup_memory_peak_bytes: u64,
    open_fds: u64,
    run_tree_logical_bytes: u64,
    run_tree_allocated_bytes: u64,
    run_tree_inodes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ResourceObservation {
    baseline: ResourceSample,
    maxima: ResourceSample,
    final_sample: ResourceSample,
    memory_high: Option<u64>,
    memory_max: Option<u64>,
    oom_before: u64,
    oom_after: u64,
    oom_kill_before: u64,
    oom_kill_after: u64,
}

pub(crate) struct ResourceMonitor {
    stop: Arc<AtomicBool>,
    task: Option<JoinHandle<BenchResult<ResourceObservation>>>,
}

impl ResourceMonitor {
    pub(crate) fn start(cgroup_dir: &Path, run_root: &Path) -> BenchResult<Self> {
        Self::start_with_interval(cgroup_dir, run_root, Duration::from_millis(20))
    }

    pub(crate) fn start_heavy(cgroup_dir: &Path, run_root: &Path) -> BenchResult<Self> {
        Self::start_with_interval(cgroup_dir, run_root, Duration::from_millis(50))
    }

    fn start_with_interval(
        cgroup_dir: &Path,
        run_root: &Path,
        sample_interval: Duration,
    ) -> BenchResult<Self> {
        let baseline = sample_resources(cgroup_dir, run_root)?;
        let events = read_key_values(&cgroup_dir.join("memory.events"))?;
        let memory_high = read_limit(&cgroup_dir.join("memory.high"))?;
        let memory_max = read_limit(&cgroup_dir.join("memory.max"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let cgroup_dir = cgroup_dir.to_path_buf();
        let run_root = run_root.to_path_buf();
        let task = std::thread::spawn(move || {
            let mut maxima = baseline.clone();
            while !thread_stop.load(Ordering::Acquire) {
                merge_maxima(&mut maxima, &sample_resources(&cgroup_dir, &run_root)?);
                std::thread::sleep(sample_interval);
            }
            let final_sample = sample_resources(&cgroup_dir, &run_root)?;
            merge_maxima(&mut maxima, &final_sample);
            let final_events = read_key_values(&cgroup_dir.join("memory.events"))?;
            Ok(ResourceObservation {
                baseline,
                maxima,
                final_sample,
                memory_high,
                memory_max,
                oom_before: *events.get("oom").unwrap_or(&0),
                oom_after: *final_events.get("oom").unwrap_or(&0),
                oom_kill_before: *events.get("oom_kill").unwrap_or(&0),
                oom_kill_after: *final_events.get("oom_kill").unwrap_or(&0),
            })
        });
        Ok(Self {
            stop,
            task: Some(task),
        })
    }

    pub(crate) fn finish(mut self) -> BenchResult<ResourceObservation> {
        self.stop.store(true, Ordering::Release);
        self.task
            .take()
            .ok_or("resource monitor task is absent")?
            .join()
            .map_err(|_| "resource monitor panicked")?
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

#[derive(Clone, Copy, Debug)]
enum Arm {
    Candidate,
    Control,
}

impl Arm {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Control => "control",
        }
    }
}

struct PreparedPair {
    root: PathBuf,
    candidate: AllocationHandle,
    candidate_session: sandbox_runtime_mpla_poc::MplaSession,
    prior: SemanticBuildOutput,
    canonical: PathBuf,
    affected_paths: Vec<PathBuf>,
    affected_stream: PathBuf,
    affected_stream_sha256: String,
    affected_payload_bytes: u64,
    control_changes: ControlChangeSet,
    fixture: Value,
}

struct CandidatePublished {
    stationary: ReceiptHitPublicationReceipt,
    semantic: IncrementalBuildOutput,
    selected_ref: PairedRefValue,
    elapsed_ns: u64,
    stationary_ns: u64,
    semantic_ns: u64,
    locator_and_ref_ns: u64,
    paired_ref_parent_synced: bool,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Mode::AuthorityProbe { probe_root } => run_authority_probe_command(&probe_root),
        Mode::Measure(args) => run_measurement_command(&args),
        Mode::ScorecardCase(args) => run_scorecard_case_command(&args),
        Mode::PrepareLifecycleControl(args) => run_prepare_lifecycle_control_command(&args),
        Mode::PreparePublicationFixture(args) => run_prepare_publication_fixture_command(&args),
        Mode::BuildPublicationFixtureCache(args) => {
            run_build_publication_fixture_cache_command(&args)
        }
        Mode::InspectPreparedFixtureCache => run_inspect_prepared_fixture_cache_command(),
    }
}

fn run_prepare_lifecycle_control_command(args: &LifecycleControlPreparationArgs) -> ExitCode {
    match mpla_speed_scorecard::prepare_lifecycle_control(
        &args.run_id,
        &args.phase,
        &args.candidate_sandbox_id,
        &args.build_commit,
    ) {
        Ok(result) => {
            println!("MPLA_SCORECARD_RESULT {}", compact_json(&result));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "MPLA_SCORECARD_ERROR {}",
                compact_json(&json!({"error": error.to_string()}))
            );
            ExitCode::from(2)
        }
    }
}

fn run_inspect_prepared_fixture_cache_command() -> ExitCode {
    match mpla_publication_scorecard::inspect_prepared_fixture_cache() {
        Ok(result) => {
            println!("MPLA_SCORECARD_RESULT {}", compact_json(&result));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "MPLA_SCORECARD_ERROR {}",
                compact_json(&json!({"error": error.to_string()}))
            );
            ExitCode::from(2)
        }
    }
}

fn run_build_publication_fixture_cache_command(
    args: &PublicationFixtureCacheBuildArgs,
) -> ExitCode {
    match mpla_publication_scorecard::build_prepared_fixture_cache(
        &args.candidate_sandbox_id,
        &args.build_commit,
    ) {
        Ok(result) => {
            println!("MPLA_SCORECARD_RESULT {}", compact_json(&result));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "MPLA_SCORECARD_ERROR {}",
                compact_json(&json!({"error": error.to_string()}))
            );
            ExitCode::from(2)
        }
    }
}

fn run_prepare_publication_fixture_command(args: &PublicationPreparationArgs) -> ExitCode {
    match mpla_publication_scorecard::prepare_fixture(
        &args.run_id,
        &args.candidate_sandbox_id,
        &args.build_commit,
        &args.fixture_profile,
    ) {
        Ok(result) => {
            println!("MPLA_SCORECARD_RESULT {}", compact_json(&result));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "MPLA_SCORECARD_ERROR {}",
                compact_json(&json!({"error": error.to_string()}))
            );
            ExitCode::from(2)
        }
    }
}

fn run_scorecard_case_command(args: &ScorecardCaseArgs) -> ExitCode {
    let result = match args.case.as_str() {
        "activation" => mpla_activation_scorecard::run(
            &args.run_id,
            &args.candidate_sandbox_id,
            &args.build_commit,
        ),
        "fork" => {
            mpla_fork_scorecard::run(&args.run_id, &args.candidate_sandbox_id, &args.build_commit)
        }
        "rollback" => mpla_rollback_scorecard::run(
            &args.run_id,
            &args.candidate_sandbox_id,
            &args.build_commit,
        ),
        "publication" => mpla_publication_scorecard::run(
            &args.run_id,
            &args.candidate_sandbox_id,
            &args.build_commit,
        ),
        "stream" => {
            mpla_stream_scorecard::run(&args.run_id, &args.candidate_sandbox_id, &args.build_commit)
        }
        "squash" => {
            mpla_squash_scorecard::run(&args.run_id, &args.candidate_sandbox_id, &args.build_commit)
        }
        "recovery" => {
            mpla_hv07_scorecard::run(&args.run_id, &args.candidate_sandbox_id, &args.build_commit)
        }
        "qualification" => mpla_qualification_scorecard::run(
            &args.run_id,
            &args.candidate_sandbox_id,
            &args.build_commit,
        ),
        _ => {
            println!(
                "MPLA_SCORECARD_ERROR {}",
                compact_json(&json!({"error": "unsupported scorecard case", "case": args.case}))
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(result) => {
            println!("MPLA_SCORECARD_RESULT {}", compact_json(&result));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "MPLA_SCORECARD_ERROR {}",
                compact_json(&json!({"error": error.to_string()}))
            );
            ExitCode::from(2)
        }
    }
}

fn run_authority_probe_command(probe_root: &Path) -> ExitCode {
    match run_authority_probe(probe_root) {
        Ok(result) => {
            println!("{AUTHORITY_RESULT_PREFIX}{}", compact_json(&result));
            ExitCode::SUCCESS
        }
        Err(error) => {
            let cleanup = cleanup_authority_root(probe_root)
                .map_err(|cleanup_error| format!("{cleanup_error}"));
            let payload = json!({
                "error": error.to_string(),
                "probe_root": probe_root,
                "cleanup": cleanup.as_ref().map(|_| "succeeded").unwrap_or("failed"),
                "cleanup_error": cleanup.err()
            });
            println!("{AUTHORITY_ERROR_PREFIX}{}", compact_json(&payload));
            ExitCode::from(2)
        }
    }
}

fn run_measurement_command(args: &MeasureArgs) -> ExitCode {
    let cleanup_root = args.run_root.clone();
    match run(args) {
        Ok(result) => {
            println!("{RESULT_PREFIX}{}", compact_json(&result));
            ExitCode::SUCCESS
        }
        Err(error) => {
            let cleanup = cleanup_exact_root(&cleanup_root)
                .map_err(|cleanup_error| format!("{cleanup_error}"));
            let payload = json!({
                "error": error.to_string(),
                "run_root": cleanup_root,
                "cleanup": cleanup.as_ref().map(|_| "succeeded").unwrap_or("failed"),
                "cleanup_error": cleanup.err()
            });
            println!("{ERROR_PREFIX}{}", compact_json(&payload));
            ExitCode::from(2)
        }
    }
}

fn run(args: &MeasureArgs) -> BenchResult<Value> {
    validate_args(args)?;
    let mut samples_ledger = OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&args.samples_ledger)?;
    samples_ledger.sync_all()?;
    sync_directory(
        args.samples_ledger
            .parent()
            .ok_or("samples ledger has no parent directory")?,
    )?;
    let run_root_parent = args
        .run_root
        .parent()
        .ok_or("run root has no parent directory")?;
    fs::create_dir_all(run_root_parent)?;
    fs::create_dir(&args.run_root)?;
    sync_directory(run_root_parent)?;

    let run_id = RunId::parse(args.run_id.clone())?;
    let backing = persistent_backing(&args.run_root)?;
    let authority = capability_receipt()?;
    let cgroup_dir = current_cgroup_v2_dir()?;
    let catalog_binding =
        bind_product_catalog(&args.catalog_exporter, &args.catalog, &args.build_commit)?;
    let boundary = matched_boundary();
    boundary.verdict()?;

    let mut events = Vec::new();
    let warmup_root = args.run_root.join("warmup");
    let mut frozen_fixture = None;
    let warmup = execute_pair(
        &run_id,
        &warmup_root,
        0,
        false,
        [Arm::Control, Arm::Candidate],
        args,
        &catalog_binding,
        &boundary,
        &cgroup_dir,
        &mut frozen_fixture,
        &mut events,
        &mut samples_ledger,
    )?;
    remove_exact_child(&args.run_root, &warmup_root)?;

    let measured_order = [
        [Arm::Control, Arm::Candidate],
        [Arm::Candidate, Arm::Control],
        [Arm::Control, Arm::Candidate],
        [Arm::Candidate, Arm::Control],
        [Arm::Control, Arm::Candidate],
    ];
    let mut measured = Vec::with_capacity(PAIRS);
    for (index, order) in measured_order.into_iter().enumerate() {
        let pair_number = index + 1;
        let pair_root = args.run_root.join(format!("pair-{pair_number}"));
        let pair = execute_pair(
            &run_id,
            &pair_root,
            pair_number,
            true,
            order,
            args,
            &catalog_binding,
            &boundary,
            &cgroup_dir,
            &mut frozen_fixture,
            &mut events,
            &mut samples_ledger,
        )?;
        measured.push(pair);
        remove_exact_child(&args.run_root, &pair_root)?;
    }

    let fixture = frozen_fixture.ok_or("no frozen fixture receipt was produced")?;
    let pre_cleanup = json!({
        "pair_roots_absent": (0..=PAIRS).all(|index| {
            let path = if index == 0 {
                args.run_root.join("warmup")
            } else {
                args.run_root.join(format!("pair-{index}"))
            };
            !path.exists()
        }),
        "run_root_present_before_final_cleanup": args.run_root.is_dir()
    });
    cleanup_exact_root(&args.run_root)?;
    let cleanup = json!({
        "exact_run_root": args.run_root,
        "run_root_absent": !args.run_root.exists(),
        "pair_roots_absent": pre_cleanup["pair_roots_absent"],
        "cleanup_method": "exact benchmark-owned run root removal plus parent fsync"
    });

    Ok(json!({
        "schema_version": 1,
        "kind": "mpla_speed_poc_v1_raw_result",
        "run_id": run_id,
        "contract": {
            "pairs": PAIRS,
            "warmup_pairs": 1,
            "existing_bytes": EXISTING_BYTES,
            "delta_bytes": DELTA_BYTES,
            "delta_files": DELTA_FILES,
            "concurrency": 1,
            "measured_order": ["C/K", "K/C", "C/K", "K/C", "C/K"],
            "measurement_retries": 0,
            "replacement_samples": 0,
            "omitted_samples": 0,
            "candidate_boundary": "immediately before admission close through response after paired-ref parent-directory fsync",
            "control_boundary": "immediately before the current-I2 closing-publication call through return after hidden-publication durability"
        },
        "backing": backing,
        "authority": authority,
        "cgroup": {
            "path": cgroup_dir,
            "memory_high": read_limit(&cgroup_dir.join("memory.high"))?,
            "memory_max": read_limit(&cgroup_dir.join("memory.max"))?,
            "membership_proven": cgroup_contains_self(&cgroup_dir)?
        },
        "catalog_binding": catalog_binding,
        "fixture": fixture,
        "warmup": warmup,
        "measured_pairs": measured,
        "events": events,
        "cleanup": cleanup
    }))
}

#[allow(clippy::too_many_arguments)]
fn execute_pair(
    run_id: &RunId,
    pair_root: &Path,
    pair_number: usize,
    measured: bool,
    order: [Arm; 2],
    args: &MeasureArgs,
    catalog: &CatalogBinding,
    boundary: &ControlBoundary,
    cgroup_dir: &Path,
    frozen_fixture: &mut Option<Value>,
    events: &mut Vec<Value>,
    samples_ledger: &mut File,
) -> BenchResult<Value> {
    fs::create_dir(pair_root)?;
    sync_directory(pair_root.parent().ok_or("pair root has no parent")?)?;
    let mut pair = prepare_pair(run_id, pair_root, pair_number)?;
    if let Some(frozen) = frozen_fixture.as_ref() {
        if frozen != &pair.fixture {
            return Err(format!(
                "fixture drift in pair {pair_number}: frozen={}, observed={}",
                compact_json(frozen),
                compact_json(&pair.fixture)
            )
            .into());
        }
    } else {
        *frozen_fixture = Some(pair.fixture.clone());
    }

    let mut candidate = None;
    let mut control = None;
    for (position, arm) in order.into_iter().enumerate() {
        emit_event(
            events,
            samples_ledger,
            json!({
                "event": "intent",
                "measured": measured,
                "pair": pair_number,
                "position": position + 1,
                "arm": arm.as_str(),
                "attempt": 1
            }),
        )?;
        let result = match arm {
            Arm::Candidate => {
                let value = run_candidate(run_id, pair_number, &mut pair, args, cgroup_dir)?;
                candidate = Some(value.clone());
                value
            }
            Arm::Control => {
                let value = run_control(pair_number, &pair, catalog, boundary, cgroup_dir)?;
                control = Some(value.clone());
                value
            }
        };
        emit_event(
            events,
            samples_ledger,
            json!({
                "event": "terminal_result",
                "measured": measured,
                "pair": pair_number,
                "position": position + 1,
                "arm": arm.as_str(),
                "attempt": 1,
                "status": "passed",
                "elapsed_ns": result["elapsed_ns"]
            }),
        )?;
    }
    let candidate = candidate.ok_or("pair has no candidate result")?;
    let control = control.ok_or("pair has no control result")?;
    let candidate_ns = candidate["elapsed_ns"]
        .as_u64()
        .ok_or("candidate result has no elapsed_ns")?;
    let control_ns = control["elapsed_ns"]
        .as_u64()
        .ok_or("control result has no elapsed_ns")?;
    let ratio = ratio_decimal(control_ns, candidate_ns);

    Ok(json!({
        "pair": pair_number,
        "measured": measured,
        "order": order.map(Arm::as_str),
        "candidate": candidate,
        "control": control,
        "matched_pair_ratio": ratio,
        "completed_once": true
    }))
}

fn prepare_pair(run_id: &RunId, pair_root: &Path, pair_number: usize) -> BenchResult<PreparedPair> {
    let candidate_arena = pair_root.join("candidate-arena");
    let control_arena = pair_root.join("control-arena");
    let control_root = pair_root.join("candidate-control");
    let candidate_operation = OperationId::from_string(format!(
        "{}-p{pair_number}-candidate-prepare",
        run_id.as_str()
    ));
    let control_operation = OperationId::from_string(format!(
        "{}-p{pair_number}-control-prepare",
        run_id.as_str()
    ));
    let candidate = create_allocation(&candidate_arena, &candidate_operation)?;
    let control = create_allocation(&control_arena, &control_operation)?;

    let dense_candidate = write_dense_file(
        &candidate.upper_dir.join("immutable-existing.bin"),
        EXISTING_BYTES,
        0x6d70_6c61_7370_6565,
    )?;
    let dense_control = write_dense_file(
        &control.upper_dir.join("immutable-existing.bin"),
        EXISTING_BYTES,
        0x6d70_6c61_7370_6565,
    )?;
    if dense_candidate != dense_control {
        return Err("candidate/control dense fixture hashes differ".into());
    }

    let affected_paths = (0..DELTA_FILES)
        .map(|index| PathBuf::from(format!("delta-{index:02}.bin")))
        .collect::<Vec<_>>();
    for relative in &affected_paths {
        sync_new_file(&candidate.upper_dir.join(relative), &[])?;
    }
    let delta_control = write_delta_files(&control.upper_dir, &affected_paths)?;
    sync_directory(&candidate.upper_dir)?;
    sync_directory(&control.upper_dir)?;

    let publication_operation =
        OperationId::from_string(format!("{}-p{pair_number}-candidate", run_id.as_str()));
    let canonical = pair_root.join("candidate-canonical");
    let prior = build_full_prior(
        run_id,
        pair_number,
        &candidate,
        pair_root,
        &canonical,
        &publication_operation,
    )?;
    enforce_semantic_limits(&prior.resource_maxima)?;
    let empty_lower = pair_root.join("empty-lower");
    fs::create_dir(&empty_lower)?;
    sync_directory(pair_root)?;
    let lease = issue_workspace_lease(&candidate, SessionId::new(), &publication_operation)?;
    let candidate_session = sandbox_runtime_mpla_poc::MplaSession::open(
        &control_root,
        candidate.clone(),
        lease,
        vec![empty_lower],
        None,
    )?;
    let workspace = candidate_session
        .workspace_root()
        .ok_or("candidate session has no mounted workspace")?
        .to_path_buf();
    let receipt_root = pair_root.join("receipt-input");
    fs::create_dir(&receipt_root)?;
    let before = capture_affected_paths(&workspace, &affected_paths, &receipt_root.join("before"))?;
    let delta_candidate = write_delta_files(&workspace, &affected_paths)?;
    let after = capture_affected_paths(&workspace, &affected_paths, &receipt_root.join("after"))?;
    if delta_candidate != delta_control {
        return Err("candidate/control delta fixture hashes differ".into());
    }
    if before.payload_bytes_read != 0 || after.payload_bytes_read != DELTA_BYTES {
        return Err(format!(
            "affected payload receipt mismatch: before={}, after={}",
            before.payload_bytes_read, after.payload_bytes_read
        )
        .into());
    }
    let affected_stream = receipt_root.join("affected.records");
    let affected_stream_sha256 =
        write_affected_stream_from_snapshots(&affected_stream, &before, &after)?;
    let control_changes =
        collect_control_changes(&control.upper_dir, &ControlCollectionLimits::default())?;

    let candidate_mode = fs::metadata(candidate.upper_dir.join("immutable-existing.bin"))?
        .permissions()
        .mode();
    let control_mode = fs::metadata(control.upper_dir.join("immutable-existing.bin"))?
        .permissions()
        .mode();
    if candidate_mode != control_mode {
        return Err("candidate/control fixture modes differ".into());
    }

    let fixture = json!({
        "schema_version": 1,
        "existing": {
            "path": "immutable-existing.bin",
            "bytes": EXISTING_BYTES,
            "dense": true,
            "sha256": dense_candidate,
            "mode": candidate_mode & 0o7777
        },
        "delta": {
            "bytes": DELTA_BYTES,
            "files": DELTA_FILES,
            "paths": affected_paths,
            "files_sha256": delta_candidate,
            "aggregate_sha256": aggregate_hashes(&delta_candidate)
        },
        "final_source_manifest_sha256": control_changes.profile.source_manifest_sha256,
        "final_entries": control_changes.profile.entries,
        "final_logical_bytes": control_changes.profile.logical_bytes,
        "seed": "0x6d706c6173706565"
    });

    Ok(PreparedPair {
        root: pair_root.to_path_buf(),
        candidate,
        candidate_session,
        prior,
        canonical,
        affected_paths,
        affected_stream,
        affected_stream_sha256,
        affected_payload_bytes: after.payload_bytes_read,
        control_changes,
        fixture,
    })
}

pub(crate) fn build_full_prior(
    run_id: &RunId,
    pair_number: usize,
    allocation: &AllocationHandle,
    pair_root: &Path,
    canonical: &Path,
    attribution_operation: &OperationId,
) -> BenchResult<SemanticBuildOutput> {
    fs::create_dir_all(canonical)?;
    let label = format!("{}-p{pair_number}-prior", run_id.as_str());
    let output = build_with_output(&SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string(label.clone()),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        sealed_tree: allocation.upper_dir.clone(),
        spool_dir: pair_root.join("prior-spool"),
        canonical_object_dir: canonical.to_path_buf(),
        attribution: attribution(attribution_operation.as_str()),
    })?;
    Ok(output)
}

fn run_candidate(
    run_id: &RunId,
    pair_number: usize,
    pair: &mut PreparedPair,
    args: &MeasureArgs,
    cgroup_dir: &Path,
) -> BenchResult<Value> {
    let operation_id =
        OperationId::from_string(format!("{}-p{pair_number}-candidate", run_id.as_str()));
    let publication_id = PublicationId::new();
    let monitor = ResourceMonitor::start(cgroup_dir, &pair.root)?;
    let storage_before = tree_usage(&pair.root)?.1;
    let cpu_before = process_cpu_ns()?;
    let boundary_started = Instant::now();
    let published = publish_incremental(
        pair,
        operation_id.clone(),
        publication_id.clone(),
        boundary_started,
    )?;
    let elapsed_ns = published.elapsed_ns;
    let cpu_ns = process_cpu_ns()?.saturating_sub(cpu_before);
    let storage_after = tree_usage(&pair.root)?.1;
    let resources = monitor.finish()?;
    validate_resource_observation(&resources)?;

    if published.semantic.immutable_payload_bytes_read != 0 {
        return Err("candidate read immutable existing payload bytes".into());
    }
    if pair.affected_payload_bytes != DELTA_BYTES {
        return Err("candidate affected payload is not exactly 1 MiB".into());
    }
    enforce_semantic_limits(&published.semantic.resource_maxima)?;
    if !published.stationary.stationary.no_second_payload_allocation {
        return Err("candidate stationary receipt did not prove no second payload".into());
    }
    let storage_delta = storage_after.saturating_sub(storage_before);
    if storage_delta >= EXISTING_BYTES {
        return Err(format!(
            "candidate allocated an existing-state-sized second payload: {storage_delta}"
        )
        .into());
    }
    if published.selected_ref.roots != published.semantic.receipt.roots {
        return Err("selected paired ref does not equal candidate semantic roots".into());
    }
    if !published.paired_ref_parent_synced
        || !published.semantic.receipt.durability.files_fsynced
        || !published.semantic.receipt.durability.manifest_fsynced
        || !published
            .semantic
            .receipt
            .durability
            .manifest_directory_fsynced
        || !published
            .semantic
            .receipt
            .durability
            .object_directory_fsynced
    {
        return Err("candidate durability receipt is incomplete".into());
    }

    let candidate_changes = collect_control_changes(
        &pair.candidate.upper_dir,
        &ControlCollectionLimits::default(),
    )?;
    if candidate_changes.profile.entries != pair.control_changes.profile.entries
        || candidate_changes.profile.logical_bytes != pair.control_changes.profile.logical_bytes
        || candidate_changes.profile.source_manifest_sha256
            != pair.control_changes.profile.source_manifest_sha256
    {
        return Err("candidate/control final source bytes differ".into());
    }
    let operation_label = operation_id.as_str().to_owned();
    let oracle = run_oracle(
        &args.oracle,
        &pair.candidate.upper_dir,
        &pair.root.join("oracle"),
        &operation_label,
    )?;
    let candidate_stream =
        materialize_record_stream(&published.semantic.root_manifest_path, &pair.canonical)?;
    compare_oracle(&published.semantic, &candidate_stream, &oracle)?;

    let result = json!({
        "arm": "candidate",
        "elapsed_ns": elapsed_ns,
        "cpu_ns": cpu_ns,
        "boundary": {
            "start": "immediately before admission close",
            "stop": "response after paired-ref parent-directory fsync",
            "clock": "std_instant_monotonic"
        },
        "phase_elapsed_ns": {
            "stationary": published.stationary_ns,
            "semantic": published.semantic_ns,
            "locator_and_paired_ref": published.locator_and_ref_ns
        },
        "semantic": {
            "roots": published.semantic.receipt.roots,
            "record_stream_sha256": published.semantic.receipt.record_stream_sha256,
            "entry_count": published.semantic.receipt.entry_count,
            "affected_record_count": published.semantic.affected_record_count,
            "affected_stream_bytes_read": published.semantic.affected_input_bytes,
            "affected_payload_bytes_read": pair.affected_payload_bytes,
            "prior_node_bytes_read": published.semantic.prior_node_bytes_read,
            "immutable_payload_bytes_read": published.semantic.immutable_payload_bytes_read,
            "resource_maxima": semantic_maxima_value(&published.semantic.resource_maxima),
            "durability": published.semantic.receipt.durability
        },
        "stationary": {
            "receipt_validated_before_sealing": published.stationary.receipt_validated_before_sealing,
            "affected_stream_sha256": published.stationary.affected_stream_sha256,
            "affected_paths": published.stationary.affected_paths,
            "sync_completed": published.stationary.stationary.stable.sync_completed,
            "syncfs_completed": published.stationary.stationary.quiescence.syncfs_completed,
            "representative_inodes_unchanged": published.stationary.stationary.representative_inodes_unchanged,
            "allocated_bytes_unchanged": published.stationary.stationary.allocated_bytes_unchanged,
            "no_second_payload_allocation": published.stationary.stationary.no_second_payload_allocation,
            "stale_writer_rejected": published.stationary.stationary.stale_writer_rejected,
            "stale_deleter_rejected": published.stationary.stationary.stale_deleter_rejected
        },
        "paired_ref": {
            "value": published.selected_ref,
            "parent_directory_synced": published.paired_ref_parent_synced
        },
        "source_profile": candidate_changes.profile,
        "oracle": oracle,
        "oracle_exact_match": true,
        "storage_allocated_before": storage_before,
        "storage_allocated_after": storage_after,
        "storage_allocated_delta": storage_delta,
        "no_second_copy": true,
        "resources": resources,
        "correct": true
    });
    Ok(result)
}

fn run_control(
    pair_number: usize,
    pair: &PreparedPair,
    catalog: &CatalogBinding,
    boundary: &ControlBoundary,
    cgroup_dir: &Path,
) -> BenchResult<Value> {
    let state_root = pair.root.join("current-i2-state");
    fs::create_dir(&state_root)?;
    sync_directory(&pair.root)?;
    let monitor = ResourceMonitor::start(cgroup_dir, &pair.root)?;
    let storage_before = tree_usage(&pair.root)?.1;
    let cpu_before = process_cpu_ns()?;
    let started = Instant::now();
    let receipt = run_current_i2_closing(
        &CurrentI2ClosingRequest {
            state_root,
            publication_id: *Uuid::new_v4().as_bytes(),
            public_root_hash: pair.control_changes.profile.source_manifest_sha256.clone(),
            catalog_binding: catalog.clone(),
            boundary: boundary.clone(),
        },
        &pair.control_changes,
    )?;
    let elapsed_ns = elapsed_ns(started);
    let cpu_ns = process_cpu_ns()?.saturating_sub(cpu_before);
    let storage_after = tree_usage(&pair.root)?.1;
    let resources = monitor.finish()?;
    validate_resource_observation(&resources)?;
    if receipt.boundary.verdict()? != sandbox_runtime_mpla_poc::ControlVerdict::Matched
        || receipt.boundary.unknown_reason.is_some()
        || receipt
            .publication
            .as_ref()
            .is_none_or(|publication| !publication.matched)
    {
        return Err(format!("pair {pair_number} control was not matched").into());
    }
    if receipt
        .source
        .as_ref()
        .is_none_or(|source| source != &pair.control_changes.profile)
    {
        return Err(format!("pair {pair_number} control source receipt differs").into());
    }

    Ok(json!({
        "arm": "control",
        "elapsed_ns": elapsed_ns,
        "cpu_ns": cpu_ns,
        "boundary": receipt.boundary,
        "internal_publication_span": receipt.span,
        "implementation": receipt.implementation,
        "intent": receipt.intent,
        "verdict": receipt.verdict,
        "catalog_binding_id": receipt.catalog_binding_id,
        "coverage": receipt.coverage,
        "source_profile": receipt.source,
        "publication": receipt.publication,
        "storage_allocated_before": storage_before,
        "storage_allocated_after": storage_after,
        "storage_allocated_delta": storage_after.saturating_sub(storage_before),
        "resources": resources,
        "correct": true
    }))
}

fn publish_incremental(
    pair: &mut PreparedPair,
    operation_id: OperationId,
    publication_id: PublicationId,
    boundary_started: Instant,
) -> BenchResult<CandidatePublished> {
    let request = StationaryPublicationRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
    };
    let operations = pair.root.join("candidate-control").join("operations");
    let seal_input = ReceiptHitSealInput {
        schema_version: SCHEMA_VERSION,
        affected_stream: pair.affected_stream.clone(),
        affected_stream_sha256: pair.affected_stream_sha256.clone(),
        affected_paths: pair.affected_paths.clone(),
    };
    let incremental_request = IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        prior_manifest: pair.prior.root_manifest_path.clone(),
        expected_prior_roots: pair.prior.receipt.roots.clone(),
        expected_prior_record_stream_sha256: pair.prior.receipt.record_stream_sha256.clone(),
        affected_stream: pair.affected_stream.clone(),
        affected_stream_sha256: pair.affected_stream_sha256.clone(),
        affected_ranges_complete: true,
        canonical_object_dir: pair.canonical.clone(),
        attribution: attribution(operation_id.as_str()),
    };

    let (stationary, semantic, stationary_ns, semantic_ns) = std::thread::scope(|scope| {
        let stationary_task = scope.spawn(|| {
            let started = Instant::now();
            let receipt = stationary_adopt_receipt_hit(
                &mut pair.candidate_session,
                &request,
                &operations,
                &seal_input,
                &mut FaultInjector::default(),
            )?;
            Ok::<_, sandbox_runtime_mpla_poc::PocError>((receipt, elapsed_ns(started)))
        });
        let semantic_task = scope.spawn(|| {
            let started = Instant::now();
            let output = build_incremental(&incremental_request)?;
            Ok::<_, sandbox_runtime_mpla_poc::PocError>((output, elapsed_ns(started)))
        });
        let stationary = stationary_task
            .join()
            .map_err(|_| "candidate stationary task panicked")??;
        let semantic = semantic_task
            .join()
            .map_err(|_| "candidate semantic task panicked")??;
        Ok::<_, Box<dyn Error + Send + Sync>>((stationary.0, semantic.0, stationary.1, semantic.1))
    })?;
    enforce_semantic_limits(&semantic.resource_maxima)?;
    let ref_started = Instant::now();
    let (selected_ref, paired_ref_parent_synced) = install_ref(
        &pair.root,
        &pair.candidate,
        &semantic.receipt,
        stationary.stationary.adoption.new_owner.owner_epoch,
        stationary.stationary.stable.after.allocated_bytes.max(1),
        &operation_id,
        &publication_id,
    )?;
    let locator_and_ref_ns = elapsed_ns(ref_started);

    Ok(CandidatePublished {
        stationary,
        semantic,
        selected_ref,
        elapsed_ns: elapsed_ns(boundary_started),
        stationary_ns,
        semantic_ns,
        locator_and_ref_ns,
        paired_ref_parent_synced,
    })
}

#[allow(clippy::too_many_arguments)]
fn install_ref(
    pair_root: &Path,
    allocation: &AllocationHandle,
    semantic: &sandbox_runtime_mpla_poc::SemanticBuildReceipt,
    owner_epoch: u64,
    accounted_bytes: u64,
    operation_id: &OperationId,
    publication_id: &PublicationId,
) -> BenchResult<(PairedRefValue, bool)> {
    let locator_store = LocatorStore::open(pair_root.join("locators"))?;
    let ref_store = PairedRefStore::open(pair_root.join("refs"))?;
    if locator_store.selected()?.is_some() || ref_store.read(BRANCH)?.is_some() {
        return Err("fresh pair locator/ref stores are not empty".into());
    }
    let payload_root = PayloadRootId::parse(semantic.roots.root_id.as_str())?;
    let locator = locator_store.install(
        &LocatorDelta {
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
                    length: accounted_bytes.max(1),
                }],
            }],
            reverse: vec![ReverseLocatorEntry {
                allocation_id: allocation.descriptor.allocation_id.clone(),
                owner_epoch,
                operation_id: operation_id.clone(),
                publication_id: publication_id.clone(),
                payload_roots: vec![payload_root],
                accounted_bytes: accounted_bytes.max(1),
            }],
        },
        &mut NamedFaultInjector::default(),
    )?;
    let outcome = ref_store.commit(
        BRANCH,
        &LocatorRefCandidate {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: publication_id.clone(),
            roots: semantic.roots.clone(),
            locator_generation: locator.generation,
            expected_sequence: RefSequence::ZERO,
        },
        &semantic.durability,
        &locator,
        &locator_store,
        &mut NamedFaultInjector::default(),
    )?;
    match outcome {
        RefCommitOutcome::Committed(receipt) => {
            Ok((receipt.value, receipt.parent_directory_synced))
        }
        RefCommitOutcome::ExpectedParent { expected, observed } => Err(format!(
            "unexpected paired-ref parent conflict: expected {expected}, observed {observed}"
        )
        .into()),
    }
}

fn run_oracle(
    oracle_bin: &Path,
    tree: &Path,
    records_root: &Path,
    operation: &str,
) -> BenchResult<OracleSummary> {
    fs::create_dir_all(records_root)?;
    let records = records_root.join("oracle.records");
    let output = Command::new(oracle_bin)
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
    semantic: &IncrementalBuildOutput,
    candidate_stream: &Path,
    oracle: &OracleSummary,
) -> BenchResult {
    if semantic.receipt.roots.root_id.as_str() != oracle.root_id
        || semantic.receipt.roots.attribution_root_id.as_str() != oracle.attribution_root_id
        || semantic.receipt.record_stream_sha256 != oracle.record_stream_sha256
        || semantic.receipt.entry_count != oracle.entry_count
    {
        return Err(format!(
            "candidate/oracle summary mismatch: candidate={:?}, oracle={oracle:?}",
            semantic.receipt.roots
        )
        .into());
    }
    compare_files_streaming(candidate_stream, Path::new(&oracle.record_stream_path))
}

fn compare_files_streaming(left: &Path, right: &Path) -> BenchResult {
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

fn write_dense_file(path: &Path, bytes: u64, seed: u64) -> BenchResult<String> {
    if bytes == 0 || bytes % (32 * 1024) != 0 {
        return Err("dense fixture size must be a nonzero multiple of 32 KiB".into());
    }
    let file = OpenOptions::new().create_new(true).write(true).open(path)?;
    let mut writer = BufWriter::with_capacity(32 * 1024, file);
    let mut digest = Sha256::new();
    let mut block = [0_u8; 32 * 1024];
    let mut written = 0_u64;
    while written < bytes {
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = (seed
                .wrapping_add(written / 32_768)
                .wrapping_add(u64::try_from(index)?)
                % 251) as u8;
        }
        writer.write_all(&block)?;
        digest.update(block);
        written += u64::try_from(block.len())?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    let metadata = fs::metadata(path)?;
    if metadata.len() != bytes || metadata.blocks().saturating_mul(512) < bytes {
        return Err(format!(
            "{} is not a real dense file: logical={}, allocated={}",
            path.display(),
            metadata.len(),
            metadata.blocks().saturating_mul(512)
        )
        .into());
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn write_delta_files(root: &Path, paths: &[PathBuf]) -> BenchResult<Vec<String>> {
    let mut hashes = Vec::with_capacity(paths.len());
    for (index, relative) in paths.iter().enumerate() {
        let bytes = DELTA_BYTES / u64::try_from(paths.len())?
            + u64::from(u64::try_from(index)? < DELTA_BYTES % u64::try_from(paths.len())?);
        let path = root.join(relative);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)?;
        let mut digest = Sha256::new();
        let mut remaining = bytes;
        let mut offset = 0_u64;
        let mut block = [0_u8; 32 * 1024];
        while remaining > 0 {
            let count = usize::try_from(remaining.min(u64::try_from(block.len())?))?;
            for (position, byte) in block[..count].iter_mut().enumerate() {
                *byte = (0x5a_u64
                    .wrapping_add(u64::try_from(index)? * 17)
                    .wrapping_add(offset)
                    .wrapping_add(u64::try_from(position)?)
                    % 251) as u8;
            }
            file.write_all(&block[..count])?;
            digest.update(&block[..count]);
            remaining -= u64::try_from(count)?;
            offset += u64::try_from(count)?;
        }
        file.sync_all()?;
        hashes.push(format!("{:x}", digest.finalize()));
    }
    sync_directory(root)?;
    let logical = paths
        .iter()
        .map(|path| fs::metadata(root.join(path)).map(|metadata| metadata.len()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .sum::<u64>();
    if logical != DELTA_BYTES {
        return Err(format!("delta fixture is {logical} bytes, expected {DELTA_BYTES}").into());
    }
    Ok(hashes)
}

fn sync_new_file(path: &Path, contents: &[u8]) -> BenchResult {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

fn aggregate_hashes(hashes: &[String]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"MPLA-SPEED-DELTA-HASHES-V1\0");
    for hash in hashes {
        digest.update(hash.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn validate_args(args: &MeasureArgs) -> BenchResult {
    let expected_root = Path::new("/eos/workspace/mpla-poc/speed").join(&args.run_id);
    if args.run_root != expected_root {
        return Err(format!(
            "run root must be the exact benchmark-owned path {}; observed {}",
            expected_root.display(),
            args.run_root.display()
        )
        .into());
    }
    if args.run_root.exists() {
        return Err(format!("run root already exists: {}", args.run_root.display()).into());
    }
    let expected_ledger = Path::new("/eos/workspace/samples.jsonl");
    if args.samples_ledger != expected_ledger {
        return Err(format!(
            "samples ledger must be the exact persistent campaign path {}; observed {}",
            expected_ledger.display(),
            args.samples_ledger.display()
        )
        .into());
    }
    if args.samples_ledger.exists() {
        return Err(format!(
            "samples ledger already exists: {}",
            args.samples_ledger.display()
        )
        .into());
    }
    if args.run_id.is_empty()
        || !args
            .run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("run id must contain only ASCII alphanumerics, '-' or '_'".into());
    }
    if args.build_commit.len() != 40
        || !args
            .build_commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("build commit must be a full lowercase 40-hex Git commit".into());
    }
    for (name, path) in [
        ("oracle", &args.oracle),
        ("catalog exporter", &args.catalog_exporter),
        ("catalog", &args.catalog),
    ] {
        if !path.is_absolute() || !path.is_file() {
            return Err(format!(
                "{name} must be an existing absolute file: {}",
                path.display()
            )
            .into());
        }
    }
    if fs::metadata(&args.oracle)?.permissions().mode() & 0o111 == 0
        || fs::metadata(&args.catalog_exporter)?.permissions().mode() & 0o111 == 0
    {
        return Err("oracle and catalog exporter must be executable".into());
    }
    Ok(())
}

fn matched_boundary() -> ControlBoundary {
    ControlBoundary {
        candidate_start: "immediately before admission close".to_owned(),
        candidate_stop: "response after paired-ref parent fsync".to_owned(),
        current_i2_start: "immediately before current-I2 closing publication call".to_owned(),
        current_i2_stop: "return after current-I2 hidden publication durability".to_owned(),
        same_fixture: true,
        same_intent: true,
        same_durability: true,
        same_readiness: true,
        cache_state: ControlCacheMatch::NotApplicable,
        unknown_reason: None,
    }
}

fn attribution(operation: &str) -> AttributionInput {
    AttributionInput {
        actor_id: ACTOR_ID.to_owned(),
        semantic_operation_id: operation.to_owned(),
    }
}

fn enforce_semantic_limits(maxima: &SemanticResourceMaxima) -> BenchResult {
    if maxima.application_pool_bytes != POOL_BYTES
        || maxima.peak_managed_bytes > POOL_BYTES
        || maxima.peak_open_data_fds > 16
        || maxima.peak_data_workers > 4
        || maxima.spool_run_bytes != 4 * MIB as usize
        || maxima.merge_fan_in != 8
    {
        return Err(format!("semantic resource envelope violated: {maxima:?}").into());
    }
    Ok(())
}

fn semantic_maxima_value(maxima: &SemanticResourceMaxima) -> Value {
    json!({
        "application_pool_bytes": maxima.application_pool_bytes,
        "peak_managed_bytes": maxima.peak_managed_bytes,
        "scan_window_bytes": maxima.scan_window_bytes,
        "spool_run_bytes": maxima.spool_run_bytes,
        "merge_fan_in": maxima.merge_fan_in,
        "peak_open_data_fds": maxima.peak_open_data_fds,
        "peak_data_workers": maxima.peak_data_workers,
        "trie_fan_out": maxima.trie_fan_out
    })
}

fn emit_event(events: &mut Vec<Value>, samples_ledger: &mut File, event: Value) -> BenchResult {
    let encoded = compact_json(&event);
    writeln!(samples_ledger, "{encoded}")?;
    samples_ledger.sync_all()?;
    writeln!(std::io::stdout().lock(), "{EVENT_PREFIX}{encoded}")?;
    events.push(event);
    Ok(())
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| {
        format!(
            "{{\"serialization_error\":{}}}",
            serde_json::to_string(&error.to_string()).unwrap_or_else(|_| "\"unknown\"".to_owned())
        )
    })
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn process_cpu_ns() -> BenchResult<u64> {
    let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: `value` points to writable storage for a complete `timespec`.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, value.as_mut_ptr()) };
    if status != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: a successful `clock_gettime` initialized the complete value.
    let value = unsafe { value.assume_init() };
    let seconds = u64::try_from(value.tv_sec)?;
    let nanoseconds = u64::try_from(value.tv_nsec)?;
    Ok(seconds
        .saturating_mul(1_000_000_000)
        .saturating_add(nanoseconds))
}

fn sample_resources(cgroup_dir: &Path, run_root: &Path) -> BenchResult<ResourceSample> {
    let tree = tree_usage(run_root)?;
    let process_io = read_key_values(Path::new("/proc/self/io"))?;
    Ok(ResourceSample {
        process_rss_bytes: process_rss_bytes()?,
        process_io_rchar_bytes: required_counter(&process_io, "rchar", "/proc/self/io")?,
        process_io_wchar_bytes: required_counter(&process_io, "wchar", "/proc/self/io")?,
        process_io_read_bytes: required_counter(&process_io, "read_bytes", "/proc/self/io")?,
        process_io_write_bytes: required_counter(&process_io, "write_bytes", "/proc/self/io")?,
        cgroup_memory_current_bytes: read_required_u64(&cgroup_dir.join("memory.current"))?,
        cgroup_memory_peak_bytes: read_required_u64(&cgroup_dir.join("memory.peak"))?,
        open_fds: u64::try_from(fs::read_dir("/proc/self/fd")?.count())?,
        run_tree_logical_bytes: tree.0,
        run_tree_allocated_bytes: tree.1,
        run_tree_inodes: tree.2,
    })
}

fn merge_maxima(maxima: &mut ResourceSample, sample: &ResourceSample) {
    maxima.process_rss_bytes = maxima.process_rss_bytes.max(sample.process_rss_bytes);
    maxima.process_io_rchar_bytes = maxima
        .process_io_rchar_bytes
        .max(sample.process_io_rchar_bytes);
    maxima.process_io_wchar_bytes = maxima
        .process_io_wchar_bytes
        .max(sample.process_io_wchar_bytes);
    maxima.process_io_read_bytes = maxima
        .process_io_read_bytes
        .max(sample.process_io_read_bytes);
    maxima.process_io_write_bytes = maxima
        .process_io_write_bytes
        .max(sample.process_io_write_bytes);
    maxima.cgroup_memory_current_bytes = maxima
        .cgroup_memory_current_bytes
        .max(sample.cgroup_memory_current_bytes);
    maxima.cgroup_memory_peak_bytes = maxima
        .cgroup_memory_peak_bytes
        .max(sample.cgroup_memory_peak_bytes);
    maxima.open_fds = maxima.open_fds.max(sample.open_fds);
    maxima.run_tree_logical_bytes = maxima
        .run_tree_logical_bytes
        .max(sample.run_tree_logical_bytes);
    maxima.run_tree_allocated_bytes = maxima
        .run_tree_allocated_bytes
        .max(sample.run_tree_allocated_bytes);
    maxima.run_tree_inodes = maxima.run_tree_inodes.max(sample.run_tree_inodes);
}

fn required_counter(values: &BTreeMap<String, u64>, key: &str, source: &str) -> BenchResult<u64> {
    values
        .get(key)
        .copied()
        .ok_or_else(|| format!("{source} lacks required counter {key}").into())
}

pub(crate) fn validate_resource_observation(observation: &ResourceObservation) -> BenchResult {
    let memory_max = observation
        .memory_max
        .ok_or("benchmark cgroup must have a finite memory.max")?;
    let memory_high = observation
        .memory_high
        .ok_or("benchmark cgroup must have a finite memory.high")?;
    if memory_high == 0 || memory_high > memory_max {
        return Err(format!(
            "benchmark cgroup requires 0 < memory.high <= memory.max; observed {memory_high}/{memory_max}"
        )
        .into());
    }
    if memory_high != MEMORY_HIGH_BYTES || memory_max != MEMORY_MAX_BYTES {
        return Err(format!(
            "benchmark cgroup must use the fixed Stage 04.6 envelope {MEMORY_HIGH_BYTES}/{MEMORY_MAX_BYTES}; observed {memory_high}/{memory_max}"
        )
        .into());
    }
    let maximum_rss = observation
        .baseline
        .process_rss_bytes
        .saturating_add(32 * MIB)
        .min(MEMORY_HIGH_BYTES);
    if observation.oom_after != observation.oom_before
        || observation.oom_kill_after != observation.oom_kill_before
        || observation.maxima.cgroup_memory_current_bytes > memory_max
        || observation.maxima.cgroup_memory_peak_bytes > memory_max
        || observation.maxima.process_rss_bytes > maximum_rss
    {
        return Err(format!("cgroup memory/OOM envelope violated: {observation:?}").into());
    }
    Ok(())
}

fn read_limit(path: &Path) -> BenchResult<Option<u64>> {
    let value = fs::read_to_string(path)?;
    let value = value.trim();
    if value == "max" {
        Ok(None)
    } else {
        Ok(Some(value.parse()?))
    }
}

fn read_required_u64(path: &Path) -> BenchResult<u64> {
    Ok(fs::read_to_string(path)?.trim().parse()?)
}

fn read_key_values(path: &Path) -> BenchResult<BTreeMap<String, u64>> {
    parse_key_values(&fs::read_to_string(path)?)
}

fn parse_key_values(contents: &str) -> BenchResult<BTreeMap<String, u64>> {
    let mut values = BTreeMap::new();
    for line in contents.lines() {
        let mut fields = line.split_ascii_whitespace();
        let name = fields.next().ok_or("key/value row has no key")?;
        let value = fields.next().ok_or("key/value row has no value")?;
        if fields.next().is_some() {
            return Err(format!("key/value row has trailing fields: {line}").into());
        }
        let name = name.strip_suffix(':').unwrap_or(name);
        if name.is_empty() {
            return Err(format!("key/value row has empty normalized key: {line}").into());
        }
        if values.insert(name.to_owned(), value.parse()?).is_some() {
            return Err(format!("key/value input has duplicate normalized key: {name}").into());
        }
    }
    Ok(values)
}

fn process_rss_bytes() -> BenchResult<u64> {
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

pub(crate) fn tree_usage(root: &Path) -> Result<(u64, u64, u64), Box<dyn Error + Send + Sync>> {
    match root.try_exists() {
        Ok(true) => {}
        Ok(false) => return Ok((0, 0, 0)),
        Err(error) => return Err(tree_usage_error("check tree root", root, error)),
    }
    let mut logical = 0_u64;
    let mut allocated = 0_u64;
    let mut inodes = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(tree_usage_error("inspect tree entry", &path, error)),
        };
        logical = logical.saturating_add(metadata.len());
        allocated = allocated.saturating_add(metadata.blocks().saturating_mul(512));
        inodes = inodes.saturating_add(1);
        if metadata.is_dir() {
            let entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(tree_usage_error("read tree directory", &path, error)),
            };
            for entry in entries {
                match entry {
                    Ok(entry) => pending.push(entry.path()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(tree_usage_error("read tree directory entry", &path, error));
                    }
                }
            }
        }
    }
    Ok((logical, allocated, inodes))
}

fn tree_usage_error(
    operation: &str,
    path: &Path,
    source: std::io::Error,
) -> Box<dyn Error + Send + Sync> {
    format!("{operation} at {}: {source}", path.display()).into()
}

fn run_authority_probe(probe_root: &Path) -> BenchResult<Value> {
    validate_authority_root(probe_root)?;
    let authority_parent = probe_root
        .parent()
        .ok_or("authority probe root has no parent")?;
    fs::create_dir_all(authority_parent)?;
    if probe_root.exists() {
        return Err(format!(
            "authority probe root already exists: {}",
            probe_root.display()
        )
        .into());
    }
    fs::create_dir(probe_root)?;
    sync_directory(authority_parent)?;
    let lower = probe_root.join("lower");
    let upper = probe_root.join("upper");
    let work = probe_root.join("work");
    let merged = probe_root.join("merged");
    for directory in [&lower, &upper, &work, &merged] {
        fs::create_dir(directory)?;
    }
    sync_new_file(&lower.join("sentinel"), b"persistent-lower\n")?;
    sync_directory(&lower)?;
    sync_directory(probe_root)?;

    let backing = persistent_backing(probe_root)?;
    let authority = capability_receipt()?;
    mount_overlay(&lower, &upper, &work, &merged)?;
    let verification = fs::read_to_string(merged.join("sentinel"))
        .map_err(|error| format!("read mounted sentinel: {error}"))
        .and_then(|contents| {
            if contents == "persistent-lower\n" {
                Ok(())
            } else {
                Err("mounted sentinel content differs".to_owned())
            }
        });
    unmount_path(&merged)?;
    if let Err(error) = verification {
        return Err(error.into());
    }
    if merged.join("sentinel").exists() {
        return Err("overlay sentinel remains visible after unmount".into());
    }
    cleanup_authority_root(probe_root)?;

    Ok(json!({
        "schema_version": 1,
        "kind": "mpla_cap_sys_admin_authority_probe_v1",
        "linux_architecture": std::env::consts::ARCH,
        "probe_root": probe_root,
        "authority": authority,
        "backing": backing,
        "real_overlay_mount_and_unmount": true,
        "probe_root_absent": !probe_root.exists(),
        "persistent_fixture": true,
        "tmpfs_used": false
    }))
}

fn validate_authority_root(probe_root: &Path) -> BenchResult {
    let expected_parent = Path::new("/eos/workspace/mpla-poc/authority");
    if probe_root.parent() != Some(expected_parent) || probe_root.file_name().is_none() {
        return Err(format!(
            "authority probe root must be one direct child of {}; observed {}",
            expected_parent.display(),
            probe_root.display()
        )
        .into());
    }
    let name = probe_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("authority probe root name is not UTF-8")?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("authority probe root name violates the frozen safe alphabet".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn mount_overlay(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> BenchResult {
    let source = CString::new("overlay")?;
    let filesystem = CString::new("overlay")?;
    let target = CString::new(merged.as_os_str().as_bytes())?;
    let options = CString::new(format!(
        "lowerdir={},upperdir={},workdir={},userxattr",
        lower.display(),
        upper.display(),
        work.display()
    ))?;
    // SAFETY: every pointer references a live NUL-terminated buffer for the
    // duration of the syscall, and the target paths are benchmark-owned.
    let status = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            filesystem.as_ptr(),
            0,
            options.as_ptr().cast(),
        )
    };
    if status != 0 {
        return Err(format!("overlay mount failed: {}", std::io::Error::last_os_error()).into());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn mount_overlay(_lower: &Path, _upper: &Path, _work: &Path, _merged: &Path) -> BenchResult {
    Err("authority probe requires Linux".into())
}

#[cfg(target_os = "linux")]
fn unmount_path(path: &Path) -> BenchResult {
    let target = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `target` is a live NUL-terminated benchmark-owned path.
    let status = unsafe { libc::umount2(target.as_ptr(), 0) };
    if status != 0 {
        return Err(format!(
            "overlay unmount failed: {}",
            std::io::Error::last_os_error()
        )
        .into());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn unmount_path(_path: &Path) -> BenchResult {
    Err("authority probe requires Linux".into())
}

fn cleanup_authority_root(probe_root: &Path) -> BenchResult {
    validate_authority_root(probe_root)?;
    if probe_root.exists() {
        fs::remove_dir_all(probe_root)?;
        sync_directory(
            probe_root
                .parent()
                .ok_or("authority probe root has no parent")?,
        )?;
    }
    Ok(())
}

fn capability_receipt() -> BenchResult<Value> {
    const CAP_SYS_ADMIN_BIT: u32 = 21;
    const CAP_SYS_ADMIN_MASK: u64 = 1_u64 << CAP_SYS_ADMIN_BIT;
    let status = fs::read_to_string("/proc/self/status")?;
    let effective = status_hex(&status, "CapEff")?;
    let permitted = status_hex(&status, "CapPrm")?;
    let bounding = status_hex(&status, "CapBnd")?;
    let inheritable = status_hex(&status, "CapInh")?;
    let ambient = status_hex(&status, "CapAmb")?;
    let no_new_privs = status_u64(&status, "NoNewPrivs")?;
    let seccomp = status_u64(&status, "Seccomp")?;
    let seccomp_filters = status_u64(&status, "Seccomp_filters")?;
    if effective & CAP_SYS_ADMIN_MASK == 0
        || permitted & CAP_SYS_ADMIN_MASK == 0
        || bounding & CAP_SYS_ADMIN_MASK == 0
    {
        return Err(format!(
            "CAP_SYS_ADMIN bit {CAP_SYS_ADMIN_BIT} is not effective, permitted and bounded: CapEff={effective:016x} CapPrm={permitted:016x} CapBnd={bounding:016x}"
        )
        .into());
    }
    if no_new_privs != 1 || seccomp != 2 || seccomp_filters == 0 {
        return Err(format!(
            "qualification process lost the required security envelope: NoNewPrivs={no_new_privs} Seccomp={seccomp} Seccomp_filters={seccomp_filters}"
        )
        .into());
    }
    Ok(json!({
        "command_security_profile": "mpla_benchmark_qualification",
        "cap_sys_admin": {
            "bit": CAP_SYS_ADMIN_BIT,
            "mask_hex": format!("{CAP_SYS_ADMIN_MASK:016x}"),
            "effective": effective & CAP_SYS_ADMIN_MASK != 0,
            "permitted": permitted & CAP_SYS_ADMIN_MASK != 0,
            "bounding": bounding & CAP_SYS_ADMIN_MASK != 0
        },
        "capabilities": {
            "effective_hex": format!("{effective:016x}"),
            "permitted_hex": format!("{permitted:016x}"),
            "inheritable_hex": format!("{inheritable:016x}"),
            "bounding_hex": format!("{bounding:016x}"),
            "ambient_hex": format!("{ambient:016x}")
        },
        "no_new_privs": no_new_privs,
        "seccomp_mode": seccomp,
        "seccomp_filters": seccomp_filters
    }))
}

fn status_hex(status: &str, field: &str) -> BenchResult<u64> {
    let value = status_field(status, field)?;
    Ok(u64::from_str_radix(value, 16)?)
}

fn status_u64(status: &str, field: &str) -> BenchResult<u64> {
    Ok(status_field(status, field)?.parse()?)
}

fn status_field<'a>(status: &'a str, field: &str) -> BenchResult<&'a str> {
    let prefix = format!("{field}:");
    status
        .lines()
        .find_map(|line| line.strip_prefix(&prefix).map(str::trim))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} is absent from /proc/self/status").into())
}

#[derive(Debug)]
struct MountInfoEntry {
    root: PathBuf,
    mount_point: PathBuf,
    filesystem_type: String,
    source: String,
    mount_options: Vec<String>,
    super_options: Vec<String>,
}

pub fn persistent_backing(run_root: &Path) -> BenchResult<Value> {
    let canonical = fs::canonicalize(run_root)?;
    let entries = read_mountinfo()?;
    let entry = entries
        .iter()
        .filter(|entry| canonical.starts_with(&entry.mount_point))
        .max_by_key(|entry| entry.mount_point.as_os_str().len())
        .ok_or_else(|| format!("no mountinfo entry covers {}", canonical.display()))?;
    if matches!(entry.filesystem_type.as_str(), "tmpfs" | "ramfs") {
        return Err(format!(
            "benchmark fixture is on forbidden volatile filesystem {} at {}",
            entry.filesystem_type,
            entry.mount_point.display()
        )
        .into());
    }
    Ok(json!({
        "run_root": canonical,
        "mount_root": entry.root,
        "mount_point": entry.mount_point,
        "filesystem_type": entry.filesystem_type,
        "source": entry.source,
        "mount_options": entry.mount_options,
        "super_options": entry.super_options,
        "tmpfs": false,
        "persistent_required": true
    }))
}

fn read_mountinfo() -> BenchResult<Vec<MountInfoEntry>> {
    let mut entries = Vec::new();
    for line in fs::read_to_string("/proc/self/mountinfo")?.lines() {
        if let Some(entry) = parse_mountinfo_line(line) {
            entries.push(entry);
        }
    }
    if entries.is_empty() {
        return Err("/proc/self/mountinfo has no parseable entries".into());
    }
    Ok(entries)
}

fn parse_mountinfo_line(line: &str) -> Option<MountInfoEntry> {
    let (left, right) = line.split_once(" - ")?;
    let left = left.split_ascii_whitespace().collect::<Vec<_>>();
    let right = right.split_ascii_whitespace().collect::<Vec<_>>();
    if left.len() < 6 || right.len() < 3 {
        return None;
    }
    Some(MountInfoEntry {
        root: PathBuf::from(unescape_mountinfo(left[3])),
        mount_point: PathBuf::from(unescape_mountinfo(left[4])),
        mount_options: left[5].split(',').map(str::to_owned).collect(),
        filesystem_type: right[0].to_owned(),
        source: unescape_mountinfo(right[1]),
        super_options: right[2].split(',').map(str::to_owned).collect(),
    })
}

fn unescape_mountinfo(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..=index + 3].iter().all(u8::is_ascii_digit)
        {
            let octal = &value[index + 1..=index + 3];
            if let Ok(byte) = u8::from_str_radix(octal, 8) {
                decoded.push(byte);
                index += 4;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

pub(crate) fn current_cgroup_v2_dir() -> BenchResult<PathBuf> {
    let cgroup_path = fs::read_to_string("/proc/self/cgroup")?
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(PathBuf::from)
        .ok_or("process has no unified cgroup-v2 membership")?;
    let entry = read_mountinfo()?
        .into_iter()
        .find(|entry| entry.filesystem_type == "cgroup2")
        .ok_or("no cgroup2 mount is visible")?;
    let relative = if entry.root == Path::new("/") {
        cgroup_path
            .strip_prefix("/")
            .unwrap_or(&cgroup_path)
            .to_path_buf()
    } else {
        cgroup_path
            .strip_prefix(&entry.root)
            .map(Path::to_path_buf)
            .map_err(|_| {
                format!(
                    "cgroup membership {} is outside visible cgroup mount root {}",
                    cgroup_path.display(),
                    entry.root.display()
                )
            })?
    };
    let directory = entry.mount_point.join(relative);
    if !directory.is_dir() {
        return Err(format!(
            "current cgroup directory is absent: {}",
            directory.display()
        )
        .into());
    }
    if !cgroup_contains_self(&directory)? {
        return Err(format!(
            "current pid {} is absent from {}",
            std::process::id(),
            directory.display()
        )
        .into());
    }
    Ok(directory)
}

pub(crate) fn cgroup_contains_self(cgroup_dir: &Path) -> BenchResult<bool> {
    let pid = std::process::id().to_string();
    for file in ["cgroup.procs", "cgroup.threads"] {
        let path = cgroup_dir.join(file);
        if path.is_file()
            && fs::read_to_string(path)?
                .lines()
                .any(|line| line.trim() == pid)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn remove_exact_child(run_root: &Path, child: &Path) -> BenchResult {
    if child.parent() != Some(run_root)
        || !matches!(
            child.file_name().and_then(|name| name.to_str()),
            Some("warmup")
                | Some("pair-1")
                | Some("pair-2")
                | Some("pair-3")
                | Some("pair-4")
                | Some("pair-5")
        )
    {
        return Err(format!(
            "refusing to remove non-benchmark child {} from {}",
            child.display(),
            run_root.display()
        )
        .into());
    }
    if child.exists() {
        fs::remove_dir_all(child)?;
        sync_directory(run_root)?;
    }
    Ok(())
}

fn cleanup_exact_root(run_root: &Path) -> BenchResult {
    let parent = Path::new("/eos/workspace/mpla-poc/speed");
    if run_root.parent() != Some(parent)
        || run_root.file_name().is_none()
        || run_root == parent
        || run_root == Path::new("/")
    {
        return Err(format!(
            "refusing to remove non-benchmark run root {}",
            run_root.display()
        )
        .into());
    }
    if run_root.exists() {
        fs::remove_dir_all(run_root)?;
        sync_directory(parent)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> BenchResult {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn ratio_decimal(numerator: u64, denominator: u64) -> String {
    if denominator == 0 {
        return "undefined".to_owned();
    }
    const SCALE: u128 = 1_000_000_000;
    let scaled = u128::from(numerator)
        .saturating_mul(SCALE)
        .checked_div(u128::from(denominator))
        .unwrap_or(u128::MAX);
    let whole = scaled / SCALE;
    let fraction = scaled % SCALE;
    format!("{whole}.{fraction:09}")
}
