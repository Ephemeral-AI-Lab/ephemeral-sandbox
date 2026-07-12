mod support;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use sandbox_benchmark::artifacts::{ArtifactId, ArtifactStore};
use sandbox_benchmark::cleanup::{CleanupLedger, OwnedIdentity};
use sandbox_benchmark::config::{BenchmarkPaths, StartupConfig};
use sandbox_benchmark::definitions::catalog;
use sandbox_benchmark::events::{EventData, RunState};
use sandbox_benchmark::model::ConfigurationScope;
use sandbox_benchmark::plan::{load_plan, slice_default, validate_and_expand};
use sandbox_benchmark::recovery::reconcile_interrupted_runs;
use sandbox_benchmark::scheduler::{
    EnvironmentMetadata, HostEnvironment, RunArtifacts, RunFailure, RunManifest, SchedulerError,
    TreatmentIdentity, RUN_MANIFEST_SCHEMA_NAME, RUN_MANIFEST_SCHEMA_VERSION,
};
use tokio_util::sync::CancellationToken;

use support::TestRoot;

#[tokio::test]
async fn run_lifecycle_is_persisted_at_each_boundary_and_terminal_is_immutable() {
    let prepared = prepare("persisted-run-lifecycle").await;
    assert_eq!(
        prepared
            .artifacts
            .manifest()
            .expect("read initial manifest")
            .state,
        RunState::Planned
    );

    for state in [
        RunState::Queued,
        RunState::Preparing,
        RunState::Running,
        RunState::Verifying,
        RunState::TearingDown,
        RunState::Completed,
    ] {
        let transitioned = prepared
            .artifacts
            .transition(state, None)
            .expect("persist documented lifecycle transition");
        let persisted: RunManifest = prepared
            .store
            .read_envelope(
                &prepared.artifacts.run_id,
                ArtifactId::RunManifest,
                RUN_MANIFEST_SCHEMA_NAME,
                RUN_MANIFEST_SCHEMA_VERSION,
            )
            .expect("read persisted lifecycle transition");
        assert_eq!(transitioned, persisted);
        assert_eq!(persisted.state, state);
    }

    let before = prepared
        .store
        .content(&prepared.artifacts.run_id, ArtifactId::RunManifest.as_str())
        .expect("read terminal manifest bytes")
        .bytes;
    assert!(matches!(
        prepared.artifacts.transition(RunState::Failed, None),
        Err(SchedulerError::TerminalManifest)
    ));
    let cancellation = CancellationToken::new();
    let (terminal, newly_requested) = prepared
        .artifacts
        .request_cancellation(&cancellation)
        .expect("terminal cancellation read is idempotent");
    assert_eq!(terminal.state, RunState::Completed);
    assert!(!newly_requested);
    assert!(!cancellation.is_cancelled());
    let after = prepared
        .store
        .content(&prepared.artifacts.run_id, ArtifactId::RunManifest.as_str())
        .expect("reread terminal manifest bytes")
        .bytes;
    assert_eq!(before, after);
}

#[tokio::test]
async fn cancellation_transition_is_durable_before_terminalization() {
    let prepared = prepare("persisted-run-cancellation").await;
    let cancellation = CancellationToken::new();
    let (cancelling, newly_requested) = prepared
        .artifacts
        .request_cancellation(&cancellation)
        .expect("persist cancellation request");
    assert_eq!(cancelling.state, RunState::Cancelling);
    assert!(newly_requested);
    assert!(cancellation.is_cancelled());
    let persisted: RunManifest = prepared
        .store
        .read_envelope(
            &prepared.artifacts.run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .expect("read cancelling manifest");
    assert_eq!(persisted, cancelling);

    let cancelled = prepared
        .artifacts
        .transition(RunState::Cancelled, None)
        .expect("persist cancelled terminal state");
    assert_eq!(cancelled.state, RunState::Cancelled);
    assert!(cancelled.ended_at.is_some());
}

#[tokio::test]
async fn cancelled_terminal_persists_cleanup_failure_evidence() {
    let prepared = prepare("persisted-cancelled-cleanup-failure").await;
    let cancellation = CancellationToken::new();
    prepared
        .artifacts
        .request_cancellation(&cancellation)
        .expect("persist cancellation request");
    let failure = RunFailure {
        code: "cleanup_failure".to_owned(),
        message: "owned teardown did not restore the cleanup baseline".to_owned(),
        infrastructure: true,
    };
    let cancelled = prepared
        .artifacts
        .transition(RunState::Cancelled, Some(failure.clone()))
        .expect("persist cancelled terminal cleanup failure");
    assert_eq!(cancelled.state, RunState::Cancelled);
    assert_eq!(cancelled.failure, Some(failure));
    assert!(cancelled.ended_at.is_some());

    let persisted: RunManifest = prepared
        .store
        .read_envelope(
            &prepared.artifacts.run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .expect("read cancelled cleanup failure manifest");
    assert_eq!(persisted, cancelled);
    let duplicate = CancellationToken::new();
    let (terminal, newly_requested) = prepared
        .artifacts
        .request_cancellation(&duplicate)
        .expect("terminal cancellation remains idempotent");
    assert_eq!(terminal, cancelled);
    assert!(!newly_requested);
    assert!(!duplicate.is_cancelled());
}

#[tokio::test]
async fn interrupted_run_is_terminalized_after_tail_quarantine_and_owned_cleanup() {
    let prepared = prepare("recovery-interrupted").await;
    let run_id = prepared.artifacts.run_id.clone();
    let target = prepared
        .startup
        .paths
        .runs
        .join(&run_id)
        .join("cell-1/trial-1");
    fs::create_dir_all(&target).expect("create interrupted trial work");
    fs::write(target.join("payload"), b"owned").expect("write owned payload");
    let identity = OwnedIdentity::RunTrial {
        run_id: run_id.clone(),
        trial_id: "trial-1".to_owned(),
    };
    let mut ledger = CleanupLedger::default();
    ledger
        .register(&prepared.startup.paths, &target, identity)
        .expect("register interrupted work");
    drop(ledger);
    let outside = prepared.root.join("outside-sentinel");
    fs::write(&outside, b"keep").expect("write outside sentinel");

    prepared
        .artifacts
        .events
        .emit(
            1,
            EventData::RunState {
                state: RunState::Running,
            },
        )
        .await
        .expect("persist running event");
    prepared
        .artifacts
        .transition(RunState::Queued, None)
        .expect("persist queued manifest");
    prepared
        .artifacts
        .transition(RunState::Preparing, None)
        .expect("persist preparing manifest");
    prepared
        .artifacts
        .transition(RunState::Running, None)
        .expect("persist running manifest");
    let run_artifacts = prepared.store.run_path(&run_id).expect("resolve artifacts");
    OpenOptions::new()
        .append(true)
        .open(run_artifacts.join(ArtifactId::Events.file_name()))
        .expect("open event log")
        .write_all(br#"{"schema_name":"eos_benchmark_event""#)
        .expect("write torn event");

    let summary = reconcile_interrupted_runs(&prepared.startup, &prepared.store)
        .await
        .expect("reconcile restart");
    assert!(summary.execution_safe(), "issues: {:?}", summary.issues);
    assert_eq!(summary.interrupted_runs, 1);
    assert_eq!(summary.cleaned_owned_targets, 1);
    assert_eq!(summary.quarantined_tails, 1);
    assert!(!target.exists());
    assert_eq!(fs::read(outside).expect("read sentinel"), b"keep");

    let manifest: RunManifest = prepared
        .store
        .read_envelope(
            &run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .expect("read reconciled manifest");
    assert_eq!(manifest.state, RunState::Failed);
    assert_eq!(
        manifest
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        Some("runner_interrupted")
    );
    assert!(run_artifacts.join(".recovery-quarantine").is_dir());
    assert!(run_artifacts.join(ArtifactId::Report.file_name()).is_file());
}

#[tokio::test]
async fn mismatched_recovery_marker_fails_closed_without_deleting_work() {
    let prepared = prepare("recovery-mismatch").await;
    let run_id = prepared.artifacts.run_id.clone();
    let target = prepared.startup.paths.runs.join(&run_id).join("trial-1");
    fs::create_dir_all(&target).expect("create interrupted trial work");
    let mut ledger = CleanupLedger::default();
    ledger
        .register(
            &prepared.startup.paths,
            &target,
            OwnedIdentity::RunTrial {
                run_id: "different-run".to_owned(),
                trial_id: "trial-1".to_owned(),
            },
        )
        .expect("register deliberately mismatched marker");
    drop(ledger);
    prepared
        .artifacts
        .transition(RunState::Queued, None)
        .expect("persist queued manifest");
    prepared
        .artifacts
        .transition(RunState::Preparing, None)
        .expect("persist nonterminal manifest");

    let summary = reconcile_interrupted_runs(&prepared.startup, &prepared.store)
        .await
        .expect("reconcile restart");
    assert!(!summary.execution_safe());
    assert!(summary
        .issues
        .iter()
        .any(|issue| issue.code == "interrupted_cleanup_failed" && issue.blocks_execution));
    assert!(target.exists());
}

struct Prepared {
    root: TestRoot,
    startup: StartupConfig,
    store: ArtifactStore,
    artifacts: std::sync::Arc<RunArtifacts>,
}

async fn prepare(label: &str) -> Prepared {
    let root = TestRoot::new(label);
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical repository");
    let paths = BenchmarkPaths::initialize(&root.join("workspace"), &repo)
        .expect("initialize benchmark paths");
    let startup = StartupConfig {
        repo: repo.clone(),
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        web_root: repo.join("benchmark/web/dist"),
        settings_path: root.join("settings.json"),
        paths,
    };
    let declared = load_plan(&repo.join("benchmark/defaults/standard-local.yml"))
        .expect("load standard default");
    let plan = slice_default(&declared, ConfigurationScope::Command);
    let expanded = validate_and_expand(&plan, &startup.paths, Some(&plan)).expect("expand plan");
    let treatment = TreatmentIdentity {
        source_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        source_dirty: false,
        source_diff_hash: None,
        daemon_binary_hash: Some("sha256:daemon".to_owned()),
        gateway_binary_hash: Some("sha256:gateway".to_owned()),
    };
    let environment = EnvironmentMetadata {
        schema_version: 1,
        treatment: treatment.clone(),
        host: HostEnvironment {
            operating_system: "test".to_owned(),
            architecture: "test".to_owned(),
            kernel_release: Some("test".to_owned()),
            docker_engine_version: Some("test".to_owned()),
            filesystem: expanded.effective_environment.filesystem.clone(),
            free_space_bytes: expanded.effective_environment.free_space_bytes,
            monotonic_clock: "std::time::Instant".to_owned(),
        },
        image_reference: expanded.canonical_plan.environment.image.0.clone(),
        image_digest: expanded.effective_environment.image_digest.clone(),
        workspace_root_identity: expanded
            .effective_environment
            .workspace_root_identity
            .clone(),
        client_cohort: expanded.effective_environment.client_cohort,
        gateway_endpoint_identity: "isolated-test-gateway".to_owned(),
    };
    let store = ArtifactStore::new(&startup.paths.results).expect("create artifact store");
    let artifacts = RunArtifacts::create(
        store.clone(),
        "0190f001-0000-7000-8000-000000000001",
        &expanded,
        None,
        environment,
        catalog(),
    )
    .await
    .expect("create planned run");
    Prepared {
        root,
        startup,
        store,
        artifacts,
    }
}
