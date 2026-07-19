// Daemon observability exposes live structural views and persisted operation
// events without collecting or retaining resource samples on request paths.

use std::error::Error;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use crate::observability::diagnostics::DiagnosticTracker;
use crate::observability::DaemonObservability;
use crate::rpc::{SandboxDaemonServer, ServerConfig};
use sandbox_config::configs::observability::{DiagnosticsConfig, ObservabilityConfig};
use sandbox_observability_query::ports::DaemonMetricsRequestClass;
use sandbox_observability_telemetry::collect::process_topology::{
    DaemonDiagnosticTrigger, DaemonDiagnosticWorkspaceHolder, DaemonOwnershipMetrics,
    DaemonProcessMetrics, DaemonRuntimeConfigMetrics, DaemonRuntimeUsage,
};
use sandbox_observability_telemetry::ObservabilityPaths;
use sandbox_operation_catalog::observability::{
    CGROUP_SPEC, DAEMON_SPEC, EVENTS_SPEC, LAYERSTACK_SPEC, SNAPSHOT_SPEC, TOPOLOGY_SPEC,
    TRACE_SPEC,
};
use sandbox_runtime::workspace_session::{FinalizationState, FinalizePolicy};
use sandbox_runtime::workspace_session::{
    HolderLifecycleEvent, HolderLifecycleEventKind, HolderLifecycleSnapshot,
};
use sandbox_runtime::{
    NamespaceExecutionId, NetworkProfile, RuntimeNamespaceExecutionSnapshot,
    RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot, WorkspaceSessionId,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
const TOPOLOGY_REQUEST: DaemonMetricsRequestClass = DaemonMetricsRequestClass::Topology;

#[test]
fn adapter_maps_concrete_runtime_snapshot_into_neutral_input() {
    let snapshot = crate::observability::adapter::map_snapshot(RuntimeObservabilitySnapshot {
        workspaces: vec![
            workspace_snapshot("workspace-1", None, FinalizationState::Active),
            workspace_snapshot("workspace-2", None, FinalizationState::Finalizing),
            workspace_snapshot("workspace-3", None, FinalizationState::FinalizeFailed),
        ],
        active_namespace_executions: vec![RuntimeNamespaceExecutionSnapshot {
            namespace_execution_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
            workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
            operation_name: "exec_command".to_owned(),
            command: Some("printf ok".to_owned()),
        }],
        ownership: Default::default(),
        partial_errors: vec!["partial projection".to_owned()],
    });

    assert_eq!(snapshot.partial_errors, ["partial projection"]);
    assert_eq!(snapshot.workspaces[0].workspace_id, "workspace-1");
    assert_eq!(snapshot.workspaces[0].network_profile, "shared");
    assert_eq!(snapshot.workspaces[0].finalize_policy, "no_op");
    assert_eq!(snapshot.workspaces[0].finalization_state, "active");
    assert_eq!(snapshot.workspaces[1].finalization_state, "finalizing");
    assert_eq!(snapshot.workspaces[2].finalization_state, "finalize_failed");
    assert_eq!(snapshot.workspaces[0].namespace_fd_count, Some(3));
    assert_eq!(
        snapshot.workspaces[0].base_root_hash.as_deref(),
        Some("root")
    );
    assert_eq!(snapshot.workspaces[0].layer_count, Some(1));
    assert_eq!(
        snapshot.active_namespace_executions[0].namespace_execution_id,
        "namespace_execution_1"
    );
    assert_eq!(
        snapshot.active_namespace_executions[0].workspace_session_id,
        "workspace-1"
    );
    assert_eq!(
        snapshot.active_namespace_executions[0].operation_name,
        "exec_command"
    );
    assert_eq!(
        snapshot.active_namespace_executions[0].command.as_deref(),
        Some("printf ok")
    );
}

#[test]
fn selected_allocator_is_bounded_and_reports_native_process_totals() {
    assert_eq!(
        tikv_jemalloc_ctl::config::malloc_conf::read().expect("malloc conf"),
        "narenas:1,tcache:false,dirty_decay_ms:0,muzzy_decay_ms:0,background_thread:false,retain:false,thp:never\0"
    );
    assert_eq!(tikv_jemalloc_ctl::opt::narenas::read(), Ok(1));
    assert_eq!(tikv_jemalloc_ctl::opt::tcache::read(), Ok(false));
    assert_eq!(
        tikv_jemalloc_ctl::opt::background_thread::read(),
        Ok(false)
    );

    let metrics = crate::observability::allocator::collect_current();

    assert!(metrics.supported);
    assert!(metrics.allocated_bytes.is_some());
    assert!(metrics.active_bytes.is_some());
    assert!(metrics.mapped_bytes.is_some());
    assert!(metrics.resident_bytes.is_some());
}

#[test]
fn from_config_disabled_when_sandbox_id_is_missing() {
    let root = test_root("missing-sandbox-id");
    let config = server_config(&root, None);
    let runtime = runtime_config(&root).expect("runtime config");
    assert!(DaemonObservability::from_config(&config, &runtime).is_none());
}

#[test]
fn diagnostic_threshold_window_resets_and_fires_at_exact_boundary() -> TestResult {
    let root = test_root("diagnostic-window");
    let artifact = root.join("diagnostic.json");
    let tracker = DiagnosticTracker::new(diagnostic_config(30, 100, 4096), artifact.clone());
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();

    assert!(!artifact.exists());
    assert_eq!(
        tracker
            .observe(
                TOPOLOGY_REQUEST,
                &diagnostic_process_metrics(1_000, 0, 128),
                &usage,
                &ownership,
                &[diagnostic_workspace("workspace-a", 4_242)],
            )
            .trigger_count,
        0
    );
    let first_high = tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_010, 1_000, 128),
        &usage,
        &ownership,
        &[diagnostic_workspace("workspace-a", 4_242)],
    );
    assert_eq!(
        first_high.active_window.trigger,
        Some(DaemonDiagnosticTrigger::Cpu)
    );
    assert_eq!(first_high.active_window.started_at_unix_ms, Some(1_010));

    let reset = tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_020, 1_000, 128),
        &usage,
        &ownership,
        &[diagnostic_workspace("workspace-a", 4_242)],
    );
    assert_eq!(reset.active_window.trigger, None);

    tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_030, 2_000, 128),
        &usage,
        &ownership,
        &[diagnostic_workspace("workspace-a", 4_242)],
    );
    assert_eq!(
        tracker
            .observe(
                TOPOLOGY_REQUEST,
                &diagnostic_process_metrics(1_059, 4_900, 128),
                &usage,
                &ownership,
                &[diagnostic_workspace("workspace-a", 4_242)],
            )
            .trigger_count,
        0
    );
    let fired = tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_060, 5_000, 128),
        &usage,
        &ownership,
        &[diagnostic_workspace("workspace-a", 4_242)],
    );
    assert_eq!(fired.trigger_count, 1);
    let latest = fired.latest.expect("diagnostic summary");
    assert_eq!(latest.trigger, DaemonDiagnosticTrigger::Cpu);
    assert_eq!(latest.cpu_interval.elapsed_ms, 30);
    assert_eq!(latest.cpu_interval.cpu_time_delta_us, Some(3_000));
    assert_eq!(latest.cpu_interval.percent_of_one_core, Some(10.0));
    assert_eq!(latest.runtime_config.worker_threads, Some(2));
    assert_eq!(latest.runtime_usage, usage);
    assert_eq!(
        latest.workspace_holders,
        [diagnostic_workspace("workspace-a", 4_242)]
    );
    assert!(artifact.exists());
    Ok(())
}

#[test]
fn diagnostic_capture_maps_request_classes_to_distinct_bounded_activity_classes() -> TestResult {
    let root = test_root("diagnostic-request-classes");
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspace_holders = [diagnostic_workspace("workspace-a", 4_242)];
    let cases = [
        (
            DaemonMetricsRequestClass::Topology,
            "observability.topology",
        ),
        (
            DaemonMetricsRequestClass::DaemonSelf,
            "observability.daemon",
        ),
        (
            DaemonMetricsRequestClass::LegacyCgroup,
            "observability.cgroup",
        ),
    ];

    for (request_class, expected_activity) in cases {
        let tracker = DiagnosticTracker::new(
            diagnostic_config(0, 100, 4096),
            root.join(format!("{expected_activity}.json")),
        );
        let state = tracker.observe(
            request_class,
            &diagnostic_process_metrics(1_000, 0, 2_048),
            &usage,
            &ownership,
            &workspace_holders,
        );
        let latest = state.latest.expect("request-attributed diagnostic");
        assert_eq!(
            latest.activity_classes,
            ["rpc.observability", expected_activity]
        );
        assert!(latest
            .activity_classes
            .iter()
            .all(|activity| activity.len() <= 64));
    }
    Ok(())
}

#[test]
fn diagnostic_out_of_order_mixed_route_sample_does_not_regress_window_or_attribution() -> TestResult
{
    let root = test_root("diagnostic-out-of-order");
    let tracker = DiagnosticTracker::new(
        diagnostic_config(10, 100, 4096),
        root.join("diagnostic.json"),
    );
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspace_holders = [diagnostic_workspace("workspace-a", 4_242)];

    tracker.observe(
        DaemonMetricsRequestClass::Topology,
        &diagnostic_process_metrics(1_000, 0, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    tracker.observe(
        DaemonMetricsRequestClass::Topology,
        &diagnostic_process_metrics(1_010, 1_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    let stale = tracker.observe(
        DaemonMetricsRequestClass::DaemonSelf,
        &diagnostic_process_metrics(1_005, 500, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    assert_eq!(stale.active_window.started_at_unix_ms, Some(1_010));

    let fired = tracker.observe(
        DaemonMetricsRequestClass::Topology,
        &diagnostic_process_metrics(1_020, 2_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    assert_eq!(fired.trigger_count, 1);
    assert_eq!(
        fired
            .latest
            .expect("non-regressed diagnostic")
            .activity_classes,
        ["rpc.observability", "observability.topology"]
    );
    Ok(())
}

#[test]
fn diagnostic_equal_timestamp_samples_union_route_attribution_without_resetting_state() -> TestResult
{
    let root = test_root("diagnostic-equal-timestamp");
    let tracker = DiagnosticTracker::new(
        diagnostic_config(0, 100, 4096),
        root.join("diagnostic.json"),
    );
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspace_holders = [diagnostic_workspace("workspace-a", 4_242)];

    tracker.observe(
        DaemonMetricsRequestClass::Topology,
        &diagnostic_process_metrics(1_000, 0, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    tracker.observe(
        DaemonMetricsRequestClass::DaemonSelf,
        &diagnostic_process_metrics(1_000, 0, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    let fired = tracker.observe(
        DaemonMetricsRequestClass::Topology,
        &diagnostic_process_metrics(1_010, 1_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );

    assert_eq!(fired.trigger_count, 1);
    assert_eq!(
        fired
            .latest
            .expect("equal-time attributed diagnostic")
            .activity_classes,
        [
            "rpc.observability",
            "observability.topology",
            "observability.daemon"
        ]
    );
    Ok(())
}

#[test]
fn diagnostic_cpu_window_attributes_the_routes_observed_across_the_interval() -> TestResult {
    let root = test_root("diagnostic-causal-activity");
    let tracker = DiagnosticTracker::new(
        diagnostic_config(10, 100, 4096),
        root.join("diagnostic.json"),
    );
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspace_holders = [diagnostic_workspace("workspace-a", 4_242)];

    tracker.observe(
        DaemonMetricsRequestClass::Topology,
        &diagnostic_process_metrics(1_000, 0, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    tracker.observe(
        DaemonMetricsRequestClass::DaemonSelf,
        &diagnostic_process_metrics(1_010, 1_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    let fired = tracker.observe(
        DaemonMetricsRequestClass::Topology,
        &diagnostic_process_metrics(1_020, 2_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );

    assert_eq!(fired.trigger_count, 1);
    assert_eq!(
        fired
            .latest
            .expect("causally attributed diagnostic")
            .activity_classes,
        [
            "rpc.observability",
            "observability.topology",
            "observability.daemon"
        ]
    );
    Ok(())
}

#[test]
fn diagnostic_sustained_topology_window_ages_out_the_initial_daemon_sample() -> TestResult {
    let root = test_root("diagnostic-topology-after-daemon-self");
    let tracker = DiagnosticTracker::new(
        diagnostic_config(10, 100, 4096),
        root.join("diagnostic.json"),
    );
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspace_holders = [diagnostic_workspace("workspace-a", 4_242)];

    tracker.observe(
        DaemonMetricsRequestClass::DaemonSelf,
        &diagnostic_process_metrics(1_000, 0, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    tracker.observe(
        DaemonMetricsRequestClass::Topology,
        &diagnostic_process_metrics(1_010, 1_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    let fired = tracker.observe(
        DaemonMetricsRequestClass::Topology,
        &diagnostic_process_metrics(1_020, 2_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );

    assert_eq!(fired.trigger_count, 1);
    assert_eq!(
        fired
            .latest
            .expect("topology-only sustained diagnostic")
            .activity_classes,
        ["rpc.observability", "observability.topology"]
    );
    Ok(())
}

#[test]
fn diagnostic_artifact_io_does_not_hold_the_threshold_state_mutex() -> TestResult {
    let root = test_root("diagnostic-io-lock");
    fs::create_dir_all(&root)?;
    let artifact = root.join("diagnostic.json");
    let temporary = artifact.with_extension("tmp");
    let status = std::process::Command::new("mkfifo")
        .arg(&temporary)
        .status()?;
    assert!(status.success(), "mkfifo failed with status {status}");

    let tracker = Arc::new(DiagnosticTracker::new(
        diagnostic_config(0, 100, 4096),
        artifact,
    ));
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspace_holders = [diagnostic_workspace("workspace-a", 4_242)];

    let first_tracker = Arc::clone(&tracker);
    let first_usage = usage.clone();
    let first_ownership = ownership.clone();
    let first_holders = workspace_holders.clone();
    let first = thread::spawn(move || {
        first_tracker.observe(
            DaemonMetricsRequestClass::Topology,
            &diagnostic_process_metrics(1_000, 0, 2_048),
            &first_usage,
            &first_ownership,
            &first_holders,
        )
    });
    thread::sleep(Duration::from_millis(50));

    let second_tracker = Arc::clone(&tracker);
    let (completed_tx, completed_rx) = mpsc::channel();
    let second = thread::spawn(move || {
        let state = second_tracker.observe(
            DaemonMetricsRequestClass::DaemonSelf,
            &diagnostic_process_metrics(1_001, 0, 128),
            &usage,
            &ownership,
            &workspace_holders,
        );
        let _ = completed_tx.send(state);
    });
    let state_completed_while_io_blocked = completed_rx
        .recv_timeout(Duration::from_millis(200))
        .is_ok();

    let mut fifo = fs::File::open(&temporary)?;
    let mut bytes = Vec::new();
    fifo.read_to_end(&mut bytes)?;
    let _ = first
        .join()
        .map_err(|_| "first diagnostic thread panicked")?;
    second
        .join()
        .map_err(|_| "second diagnostic thread panicked")?;
    assert!(
        state_completed_while_io_blocked,
        "artifact I/O serialized threshold state observations"
    );
    Ok(())
}

#[test]
fn diagnostic_cooldown_suppresses_repeated_capture_until_expiry() -> TestResult {
    let root = test_root("diagnostic-cooldown");
    let tracker = DiagnosticTracker::new(
        diagnostic_config(10, 100, 4096),
        root.join("diagnostic.json"),
    );
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspace_holders = [diagnostic_workspace("workspace-a", 4_242)];

    tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_000, 0, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_010, 1_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    let first = tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_020, 2_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    assert_eq!(first.trigger_count, 1);
    let first_id = first.latest.expect("first diagnostic").id;

    tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_030, 3_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    let suppressed = tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_119, 11_900, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    assert_eq!(suppressed.trigger_count, 1);
    assert!(suppressed.cooldown.active);
    assert_eq!(
        suppressed.latest.expect("first summary retained").id,
        first_id
    );

    let second = tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_120, 12_000, 128),
        &usage,
        &ownership,
        &workspace_holders,
    );
    assert_eq!(second.trigger_count, 2);
    assert!(second.cooldown.active);
    assert_eq!(second.cooldown.until_unix_ms, Some(1_220));
    assert_ne!(second.latest.expect("second diagnostic").id, first_id);
    Ok(())
}

#[test]
fn failed_diagnostic_capture_enters_cooldown_before_retrying_io() -> TestResult {
    let root = test_root("diagnostic-failed-write-cooldown");
    fs::create_dir_all(&root)?;
    let blocking_parent = root.join("not-a-directory");
    fs::write(&blocking_parent, b"block diagnostic directory creation")?;
    let artifact = blocking_parent.join("diagnostic.json");
    let tracker = DiagnosticTracker::new(diagnostic_config(1, 100, 4096), artifact.clone());
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspaces = [diagnostic_workspace("workspace-a", 4_242)];

    tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_000, 0, 128),
        &usage,
        &ownership,
        &workspaces,
    );
    tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_001, 100, 128),
        &usage,
        &ownership,
        &workspaces,
    );
    let failed = tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_002, 200, 128),
        &usage,
        &ownership,
        &workspaces,
    );
    assert_eq!(failed.trigger_count, 0);
    assert_eq!(failed.cooldown.until_unix_ms, Some(1_102));
    assert!(failed.cooldown.active);
    let failed_error = failed.last_error.expect("bounded capture error");
    assert!(failed_error.len() <= 512);

    fs::remove_file(&blocking_parent)?;
    fs::create_dir(&blocking_parent)?;
    tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_003, 300, 128),
        &usage,
        &ownership,
        &workspaces,
    );
    let suppressed = tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_101, 10_100, 128),
        &usage,
        &ownership,
        &workspaces,
    );
    assert_eq!(suppressed.trigger_count, 0);
    assert!(!artifact.exists(), "cooldown retried diagnostic I/O early");
    assert_eq!(
        suppressed.last_error.as_deref(),
        Some(failed_error.as_str())
    );

    let recovered = tracker.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_102, 10_200, 128),
        &usage,
        &ownership,
        &workspaces,
    );
    assert_eq!(recovered.trigger_count, 1);
    assert!(artifact.exists());
    assert!(recovered.last_error.is_none());
    Ok(())
}

#[test]
fn diagnostic_artifact_is_stable_bounded_and_explicitly_redacted() -> TestResult {
    let root = test_root("diagnostic-bounded");
    let artifact_a = root.join("diagnostic-a.json");
    let artifact_b = root.join("diagnostic-b.json");
    fs::create_dir_all(&root)?;
    let stale_temporary = artifact_a.with_extension("tmp");
    fs::write(&stale_temporary, b"stale diagnostic content")?;
    fs::set_permissions(&stale_temporary, fs::Permissions::from_mode(0o666))?;
    let config = diagnostic_config(1, 100, 4096);
    let first = DiagnosticTracker::new(config, artifact_a.clone());
    let second = DiagnosticTracker::new(config, artifact_b.clone());
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspace_holders = (0..300)
        .map(|index| DaemonDiagnosticWorkspaceHolder {
            workspace_id: format!("workspace-{index}-{}", "x".repeat(512)),
            holder_pid: 10_000 + index,
        })
        .collect::<Vec<_>>();

    for tracker in [&first, &second] {
        tracker.observe(
            TOPOLOGY_REQUEST,
            &diagnostic_process_metrics(1_000, 0, 128),
            &usage,
            &ownership,
            &workspace_holders,
        );
        tracker.observe(
            TOPOLOGY_REQUEST,
            &diagnostic_process_metrics(1_001, 100, 128),
            &usage,
            &ownership,
            &workspace_holders,
        );
    }
    let summary_a = first
        .observe(
            TOPOLOGY_REQUEST,
            &diagnostic_process_metrics(1_002, 200, 128),
            &usage,
            &ownership,
            &workspace_holders,
        )
        .latest
        .expect("first diagnostic");
    let summary_b = second
        .observe(
            TOPOLOGY_REQUEST,
            &diagnostic_process_metrics(1_002, 200, 128),
            &usage,
            &ownership,
            &workspace_holders,
        )
        .latest
        .expect("second diagnostic");

    assert_eq!(summary_a.id, summary_b.id);
    assert_eq!(summary_a.fingerprint, summary_b.fingerprint);
    assert!(summary_a.size_bytes <= 4096);
    assert_eq!(
        summary_a.size_bytes,
        fs::metadata(&artifact_a)?.len() as usize
    );
    assert_eq!(fs::metadata(&artifact_a)?.mode() & 0o777, 0o600);
    assert!(!stale_temporary.exists());
    assert_eq!(fs::read(&artifact_a)?, fs::read(&artifact_b)?);
    assert!(summary_a.workspace_ids_truncated);
    assert!(summary_a.workspace_holders.len() <= 128);
    assert_eq!(
        summary_a.workspace_holders.len(),
        summary_a.workspace_ids.len()
    );
    assert!(summary_a
        .workspace_holders
        .windows(2)
        .all(|pair| pair[0] < pair[1]));
    assert!(summary_a
        .workspace_holders
        .iter()
        .zip(&summary_a.workspace_ids)
        .all(|(holder, workspace_id)| &holder.workspace_id == workspace_id));
    assert_eq!(summary_a.runtime_config.worker_threads, Some(2));
    assert_eq!(summary_a.runtime_usage, usage);
    assert!(summary_a.redaction.workspace_file_content_excluded);
    assert!(summary_a.redaction.environment_variables_excluded);
    assert!(summary_a.redaction.authentication_material_excluded);
    assert!(summary_a.redaction.full_command_lines_excluded);

    let artifact_bytes = fs::read(&artifact_a)?;
    let artifact_json: Value = serde_json::from_slice(&artifact_bytes)?;
    assert_eq!(artifact_json["runtime_config"]["worker_threads"], 2);
    assert_eq!(artifact_json["runtime_usage"]["active_blocking_tasks"], 1);
    assert_eq!(artifact_json["thread_count"], 5);
    assert_eq!(artifact_json["cpu_interval"]["elapsed_ms"], 1);
    assert_eq!(artifact_json["memory"]["anonymous_memory_bytes"], 128);
    assert_eq!(
        artifact_json["workspace_holders"].as_array().map(Vec::len),
        Some(summary_a.workspace_holders.len())
    );
    assert_eq!(
        artifact_json["activity_classes"],
        json!(["rpc.observability", "observability.topology"])
    );
    let artifact = String::from_utf8(artifact_bytes)?;
    for forbidden in [
        "workspace_file_content",
        "environment_variables",
        "authentication_material",
        "full_command_lines",
        "auth_token",
        "command_line",
    ] {
        assert!(!artifact.contains(&format!("\"{forbidden}\":")));
    }
    Ok(())
}

#[test]
fn diagnostic_memory_window_and_unreaped_holder_triggers_are_distinct() -> TestResult {
    let root = test_root("diagnostic-memory-holder");
    let config = diagnostic_config(30, 100, 4096);
    let usage = diagnostic_runtime_usage();
    let ownership = diagnostic_ownership();
    let workspace_holders = [diagnostic_workspace("workspace-a", 4_242)];
    let memory = DiagnosticTracker::new(config, root.join("memory.json"));

    assert_eq!(
        memory
            .observe(
                TOPOLOGY_REQUEST,
                &diagnostic_process_metrics(1_000, 0, 2_048),
                &usage,
                &ownership,
                &workspace_holders,
            )
            .active_window
            .trigger,
        Some(DaemonDiagnosticTrigger::AnonymousMemory)
    );
    assert_eq!(
        memory
            .observe(
                TOPOLOGY_REQUEST,
                &diagnostic_process_metrics(1_029, 0, 2_048),
                &usage,
                &ownership,
                &workspace_holders,
            )
            .trigger_count,
        0
    );
    let memory_capture = memory.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(1_030, 0, 2_048),
        &usage,
        &ownership,
        &workspace_holders,
    );
    assert_eq!(memory_capture.trigger_count, 1);
    assert_eq!(
        memory_capture.latest.expect("memory diagnostic").trigger,
        DaemonDiagnosticTrigger::AnonymousMemory
    );

    let holder = DiagnosticTracker::new(config, root.join("holder.json"));
    let mut unreaped = ownership;
    unreaped.exited_unreaped_holders = Some(1);
    let holder_capture = holder.observe(
        TOPOLOGY_REQUEST,
        &diagnostic_process_metrics(2_000, 0, 128),
        &usage,
        &unreaped,
        &workspace_holders,
    );
    assert_eq!(holder_capture.trigger_count, 1);
    assert_eq!(
        holder_capture.latest.expect("holder diagnostic").trigger,
        DaemonDiagnosticTrigger::ExitedUnreapedHolder
    );
    Ok(())
}

#[test]
fn lifecycle_summary_is_bounded_and_uses_latest_events() {
    let long_reason = "x".repeat(600);
    let metrics = crate::observability::adapter::map_lifecycle(HolderLifecycleSnapshot {
        holder_exit_total: 3,
        cleanup_attempt_total: 4,
        cleanup_failure_total: 2,
        cleanup_terminal_total: 2,
        dropped_event_total: 1,
        events: vec![
            HolderLifecycleEvent {
                sequence: 1,
                workspace_session_id: WorkspaceSessionId("workspace-a".to_owned()),
                kind: HolderLifecycleEventKind::ExitObserved,
                detail: "exit-status:1".to_owned(),
                cleanup_duration_ms: None,
            },
            HolderLifecycleEvent {
                sequence: 2,
                workspace_session_id: WorkspaceSessionId("workspace-a".to_owned()),
                kind: HolderLifecycleEventKind::CleanupTerminal,
                detail: "destroyed".to_owned(),
                cleanup_duration_ms: Some(17),
            },
            HolderLifecycleEvent {
                sequence: 3,
                workspace_session_id: WorkspaceSessionId("workspace-b".to_owned()),
                kind: HolderLifecycleEventKind::CleanupFailure,
                detail: "mount cleanup failed".to_owned(),
                cleanup_duration_ms: Some(9),
            },
            HolderLifecycleEvent {
                sequence: 4,
                workspace_session_id: WorkspaceSessionId("workspace-b".to_owned()),
                kind: HolderLifecycleEventKind::ExitObserved,
                detail: long_reason,
                cleanup_duration_ms: None,
            },
        ],
    });

    assert_eq!(metrics.holder_exit_total, 3);
    assert_eq!(metrics.cleanup_attempt_total, 4);
    assert_eq!(metrics.cleanup_failure_total, 2);
    assert_eq!(metrics.cleanup_terminal_total, 2);
    assert_eq!(metrics.dropped_event_total, 1);
    assert_eq!(metrics.retained_event_count, 4);
    assert_eq!(
        metrics
            .last_holder_exit_reason
            .expect("latest exit reason")
            .len(),
        512
    );
    assert_eq!(metrics.last_cleanup_result.as_deref(), Some("destroyed"));
    assert_eq!(
        metrics.last_cleanup_failure.as_deref(),
        Some("mount cleanup failed")
    );
    assert_eq!(metrics.last_cleanup_duration_ms, Some(17));
}

#[tokio::test]
async fn runtime_request_completion_does_not_create_resource_history() -> TestResult {
    let root = test_root("request-completion-purity");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let response = server
        .dispatch_bytes(
            request_bytes("unknown_runtime_op", "req-runtime", json!({}))?,
            false,
        )
        .await;
    assert_eq!(
        response,
        sandbox_operation_contract::OperationResponse::unknown_op()
    );

    let paths = ObservabilityPaths::from_socket_path(&server.config.socket_path)?;
    let samples = sandbox_observability_telemetry::Reader::new(
        paths.log_path().to_path_buf(),
        paths.rotated_log_path().to_path_buf(),
    )
    .samples("sandbox", 600_000);
    assert!(
        samples.is_empty(),
        "request completion retained resource history"
    );
    Ok(())
}

#[tokio::test]
async fn snapshot_and_cgroup_reads_do_not_create_a_store() -> TestResult {
    let root = test_root("observability-read-purity");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let paths = ObservabilityPaths::from_socket_path(&server.config.socket_path)?;

    for (op, args) in [
        (SNAPSHOT_SPEC.name, json!({})),
        (CGROUP_SPEC.name, json!({ "scope": "sandbox" })),
        (TOPOLOGY_SPEC.name, json!({})),
    ] {
        let response = server
            .dispatch_bytes(request_bytes(op, "req-read", args)?, false)
            .await;
        assert!(response.as_json_value().get("error").is_none());
    }

    assert!(!paths.log_path().exists());
    assert!(!paths.rotated_log_path().exists());
    Ok(())
}

#[tokio::test]
async fn daemon_metrics_and_topology_do_not_read_an_oversized_corrupt_manifest() -> TestResult {
    let root = test_root("bounded-ownership-topology");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    fs::write(
        root.join("layer-stack")
            .join(sandbox_runtime_layerstack::ACTIVE_MANIFEST_FILE),
        vec![b'x'; 8 * 1024 * 1024],
    )?;

    for (operation, args) in [
        (DAEMON_SPEC.name, json!({})),
        (TOPOLOGY_SPEC.name, json!({})),
        (CGROUP_SPEC.name, json!({ "scope": "sandbox" })),
    ] {
        let response = server
            .dispatch_bytes(request_bytes(operation, "bounded-read", args)?, false)
            .await;
        let response = response.as_json_value();
        assert!(response.get("error").is_none(), "{response}");
        let daemon = response
            .get("daemon")
            .or_else(|| response.pointer("/topology/daemon"))
            .expect("bounded view includes daemon metrics");
        assert_eq!(daemon["ownership"]["active_layer_leases"], 0);
    }

    let rich_layerstack = server
        .dispatch_bytes(
            request_bytes(LAYERSTACK_SPEC.name, "rich-layerstack", json!({}))?,
            false,
        )
        .await;
    let rich_layerstack = rich_layerstack.as_json_value();
    assert_eq!(rich_layerstack["error"]["kind"], "internal_error");
    Ok(())
}

#[tokio::test]
async fn every_observability_read_is_pure_for_ten_thousand_iterations() -> TestResult {
    let root = test_root("all-observability-read-purity");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let paths = ObservabilityPaths::from_socket_path(&server.config.socket_path)?;
    fs::create_dir_all(paths.observability_dir())?;
    fs::write(
        paths.rotated_log_path(),
        concat!(
            "{\"kind\":\"span\",\"ts\":1,\"trace\":\"trace-1\",",
            "\"span\":\"d-1\",\"name\":\"command.exec\",\"dur_ms\":1.0,",
            "\"status\":\"completed\",\"attrs\":{}}\n"
        ),
    )?;
    fs::write(
        paths.log_path(),
        concat!(
            "{\"kind\":\"event\",\"ts\":2,\"trace\":\"trace-1\",",
            "\"parent\":\"d-1\",\"name\":\"command.finished\",\"attrs\":{}}\n",
            "{\"kind\":\"sample\",\"ts\":3,\"scope\":\"sandbox\",",
            "\"cpu_usec\":10,\"mem_cur\":1024}\n"
        ),
    )?;

    let requests = [
        request_bytes(SNAPSHOT_SPEC.name, "read-snapshot", json!({}))?,
        request_bytes(
            CGROUP_SPEC.name,
            "read-cgroup",
            json!({ "scope": "sandbox", "window_ms": 600_000 }),
        )?,
        request_bytes(TOPOLOGY_SPEC.name, "read-topology", json!({}))?,
        request_bytes(
            TRACE_SPEC.name,
            "read-trace",
            json!({ "trace_id": "trace-1" }),
        )?,
        request_bytes(EVENTS_SPEC.name, "read-events", json!({}))?,
        request_bytes(LAYERSTACK_SPEC.name, "read-layerstack", json!({}))?,
    ];
    let before = [
        fingerprint(paths.rotated_log_path())?,
        fingerprint(paths.log_path())?,
    ];

    for _ in 0..10_000 {
        for request in &requests {
            let response = server.dispatch_bytes(request.clone(), false).await;
            assert!(
                response.as_json_value().get("error").is_none(),
                "observability read failed: {}",
                response.as_json_value()
            );
        }
    }

    let after = [
        fingerprint(paths.rotated_log_path())?,
        fingerprint(paths.log_path())?,
    ];
    assert_eq!(after, before, "observability reads mutated the event store");
    assert_eq!(
        server
            .observability
            .as_ref()
            .expect("configured observability")
            .observer()
            .sink_stats(),
        Default::default(),
        "read paths must not attempt an event append"
    );
    Ok(())
}

#[tokio::test]
async fn concrete_observability_operations_dispatch_end_to_end() -> TestResult {
    let root = test_root("concrete-observability-operations");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let snapshot = server
        .dispatch_bytes(
            request_bytes(SNAPSHOT_SPEC.name, "req-snapshot", json!({}))?,
            false,
        )
        .await;
    let snapshot = snapshot.as_json_value();

    assert_eq!(snapshot["sandbox_id"], "sandbox-1");
    assert_eq!(snapshot["lifecycle_state"], "ready");
    assert_eq!(snapshot["availability"], "available");
    assert_eq!(snapshot["errors"], json!([]));
    assert_eq!(snapshot["resources"]["history"], json!([]));
    assert_eq!(snapshot["resources"]["latest"], Value::Null);
    assert_eq!(snapshot["workspaces"], json!([]));
    assert!(snapshot["sampled_at_unix_ms"].is_u64());
    assert!(snapshot["daemon"]["daemon_pid"].is_u64());
    assert!(snapshot["daemon"]["runtime_dir"].is_string());
    assert_eq!(
        snapshot["daemon"]["event_store"],
        json!({
            "dropped_storage": 0,
            "dropped_oversized": 0,
            "truncated_records": 0,
        })
    );
    assert!(snapshot["stack"]["layer_count"].is_u64());
    assert!(snapshot["stack"]["layers_bytes"].is_u64());
    assert_eq!(snapshot["stack"]["active_leases"], 0);

    let cgroup = server
        .dispatch_bytes(
            request_bytes(
                CGROUP_SPEC.name,
                "req-cgroup",
                json!({ "scope": "sandbox" }),
            )?,
            false,
        )
        .await;
    let cgroup = cgroup.as_json_value();
    assert_eq!(cgroup["view"], "cgroup");
    assert_eq!(cgroup["topology"]["schema_version"], 2);
    assert_eq!(cgroup["topology"]["workspaces"], json!([]));

    let topology = server
        .dispatch_bytes(
            request_bytes(TOPOLOGY_SPEC.name, "req-topology", json!({}))?,
            false,
        )
        .await;
    let topology = topology.as_json_value();
    assert_eq!(topology["view"], "topology");
    assert_eq!(topology["scope"], "sandbox");
    assert_eq!(topology["topology"]["schema_version"], 2);
    assert_eq!(topology["topology"]["workspaces"], json!([]));
    assert_eq!(
        topology["topology"]["daemon"]["runtime_config"],
        json!({
            "worker_threads": 2,
            "max_blocking_threads": 8,
            "blocking_thread_keep_alive_s": 5.0,
            "max_concurrent_connections": 256,
            "max_active_commands": 32,
            "max_blocking_queue_depth": 0,
            "max_command_queue_depth": 0,
            "infrastructure_thread_allowance": 4,
        })
    );
    assert_eq!(
        topology["topology"]["daemon"]["runtime_usage"],
        json!({
            "active_async_tasks": 0,
            "active_blocking_tasks": 1,
            "blocking_queue_depth": 0,
            "blocking_admission_in_use": 1,
            "connection_admission_in_use": 0,
            "active_commands": 0,
            "command_queue_depth": 0,
        })
    );
    assert_eq!(
        topology["topology"]["daemon"]["ownership"],
        json!({
            "open_workspaces": 0,
            "live_holders": 0,
            "exited_unreaped_holders": 0,
            "namespace_fd_count": 0,
            "control_fd_count": 0,
            "namespace_control_fd_count": 0,
            "active_scratch_directories": 0,
            "persisted_workspace_handles": 0,
            "active_layer_leases": 0,
        })
    );
    assert_eq!(
        topology["topology"]["daemon"]["lifecycle"],
        json!({
            "holder_exit_total": 0,
            "cleanup_attempt_total": 0,
            "cleanup_failure_total": 0,
            "cleanup_terminal_total": 0,
            "dropped_event_total": 0,
            "retained_event_count": 0,
            "last_holder_exit_reason": null,
            "last_cleanup_failure": null,
            "last_cleanup_result": null,
            "last_cleanup_duration_ms": null,
        })
    );
    let allocator = &topology["topology"]["daemon"]["allocator"];
    assert_eq!(allocator["supported"], true);
    for field in [
        "allocated_bytes",
        "active_bytes",
        "mapped_bytes",
        "resident_bytes",
    ] {
        assert!(allocator[field].as_u64().is_some(), "missing {field}");
    }
    assert_eq!(
        topology["topology"]["daemon"]["diagnostics"],
        json!({
            "enabled": true,
            "max_artifact_bytes": 1048576,
            "trigger_count": 0,
            "active_window": {
                "trigger": null,
                "started_at_unix_ms": null,
                "elapsed_ms": 0,
            },
            "cooldown": {
                "active": false,
                "until_unix_ms": null,
                "remaining_ms": 0,
            },
            "latest": null,
            "last_error": null,
        })
    );
    assert!(topology.get("series").is_none());

    let daemon = server
        .dispatch_bytes(
            request_bytes(DAEMON_SPEC.name, "req-daemon", json!({}))?,
            false,
        )
        .await;
    let daemon = daemon.as_json_value();
    assert_eq!(daemon["view"], "daemon");
    assert_eq!(daemon["scope"], "sandbox");
    assert_eq!(daemon["daemon"]["runtime_config"]["worker_threads"], 2);
    assert_eq!(daemon["daemon"]["runtime_usage"]["active_async_tasks"], 0);
    assert_eq!(daemon["daemon"]["ownership"]["open_workspaces"], 0);
    let allocator = &daemon["daemon"]["allocator"];
    assert_eq!(allocator["supported"], true);
    for field in [
        "allocated_bytes",
        "active_bytes",
        "mapped_bytes",
        "resident_bytes",
    ] {
        assert!(allocator[field].as_u64().is_some(), "missing {field}");
    }
    assert!(daemon.get("topology").is_none());
    assert!(daemon.get("series").is_none());

    let trace = server
        .dispatch_bytes(
            request_bytes(TRACE_SPEC.name, "req-trace", json!({ "trace_id": "last" }))?,
            false,
        )
        .await;
    let trace = trace.as_json_value();
    assert_eq!(trace["view"], "trace");
    assert_eq!(trace["trace"], "last");
    assert_eq!(trace["spans"], json!([]));

    let events = server
        .dispatch_bytes(
            request_bytes(EVENTS_SPEC.name, "req-events", json!({}))?,
            false,
        )
        .await;
    let events = events.as_json_value();
    assert_eq!(events["view"], "events");
    assert_eq!(events["events"], json!([]));

    let layerstack = server
        .dispatch_bytes(
            request_bytes(LAYERSTACK_SPEC.name, "req-layerstack", json!({}))?,
            false,
        )
        .await;
    let layerstack = layerstack.as_json_value();

    assert_eq!(layerstack["view"], "layerstack");
    assert!(layerstack["manifest_version"].is_u64());
    assert!(layerstack["root_hash"].is_string());
    assert_eq!(layerstack["active_lease_count"], 0);
    assert!(layerstack["total_bytes"].is_u64());
    assert!(layerstack["layers"].is_array());
    Ok(())
}

#[tokio::test]
async fn daemon_async_task_metric_tracks_in_flight_task_and_returns_to_idle() -> TestResult {
    let root = test_root("daemon-async-task-metric");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    assert_eq!(server.async_tasks.len(), 0);

    let (release, pending) = tokio::sync::oneshot::channel::<()>();
    let task = server.async_tasks.spawn(async move {
        let _ = pending.await;
    });
    tokio::task::yield_now().await;
    assert_eq!(server.async_tasks.len(), 1);

    let response = server
        .dispatch_bytes(
            request_bytes(DAEMON_SPEC.name, "req-daemon-active", json!({}))?,
            false,
        )
        .await;
    assert_eq!(
        response.as_json_value()["daemon"]["runtime_usage"]["active_async_tasks"],
        1
    );

    release.send(()).expect("release tracked task");
    task.await.expect("tracked task completed");
    assert_eq!(server.async_tasks.len(), 0);
    let response = server
        .dispatch_bytes(
            request_bytes(DAEMON_SPEC.name, "req-daemon-idle", json!({}))?,
            false,
        )
        .await;
    assert_eq!(
        response.as_json_value()["daemon"]["runtime_usage"]["active_async_tasks"],
        0
    );
    Ok(())
}

#[tokio::test]
async fn daemon_connection_task_tracker_increments_and_returns_to_idle() -> TestResult {
    let root = test_root("daemon-connection-task-tracker");
    let mut config = server_config(&root, Some("sandbox-1"));
    config.socket_path = std::env::temp_dir().join(format!(
        "sandbox-daemon-async-tasks-{}.sock",
        std::process::id()
    ));
    let server = daemon_server_from(&root, config)?;
    let socket_path = server.config.socket_path.clone();
    let shutdown = server.shutdown.clone();
    let async_tasks = server.async_tasks.clone();
    let serve = tokio::spawn(server.serve());

    let client = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            match tokio::net::UnixStream::connect(&socket_path).await {
                Ok(client) => break client,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("connect daemon socket: {error}"),
            }
        }
    })
    .await
    .expect("daemon socket became ready");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while async_tasks.len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("accepted connection entered task tracker");
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !async_tasks.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed connection left task tracker");

    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), serve)
        .await
        .expect("daemon stopped")
        .expect("serve task joined")?;
    assert_eq!(async_tasks.len(), 0);
    Ok(())
}

#[tokio::test]
async fn observability_emit_does_not_change_operation_responses() -> TestResult {
    let root = test_root("emit-isolated");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let response = server
        .dispatch_bytes(
            request_bytes("unknown_runtime_op", "req-1", json!({}))?,
            false,
        )
        .await;

    assert_eq!(
        response,
        sandbox_operation_contract::OperationResponse::unknown_op()
    );
    Ok(())
}

fn workspace_snapshot(
    workspace_id: &str,
    upperdir: Option<PathBuf>,
    finalization_state: FinalizationState,
) -> RuntimeWorkspaceSnapshot {
    RuntimeWorkspaceSnapshot {
        workspace_id: WorkspaceSessionId(workspace_id.to_owned()),
        holder_pid: i32::try_from(std::process::id()).expect("test pid fits i32"),
        holder_live: true,
        network: NetworkProfile::Shared,
        finalize_policy: FinalizePolicy::NoOp,
        finalization_state,
        workspace_root: PathBuf::from("/workspace").join(workspace_id),
        upperdir,
        workdir: Some(PathBuf::from("/workspace").join(workspace_id).join("work")),
        namespace_fd_count: Some(3),
        base_root_hash: Some("root".to_owned()),
        layer_count: Some(1),
        layer_ids: vec![format!("{workspace_id}-layer")],
        cgroup_path: None,
        applied_cgroup_limits: None,
        workload_cgroup_state: "unsupported".to_owned(),
        workload_cgroup_reason: Some("test host has no delegation".to_owned()),
    }
}

fn daemon_server(root: &Path, sandbox_id: Option<&str>) -> TestResult<SandboxDaemonServer> {
    daemon_server_from(root, server_config(root, sandbox_id))
}

fn daemon_server_from(root: &Path, config: ServerConfig) -> TestResult<SandboxDaemonServer> {
    Ok(SandboxDaemonServer::new_with_runtime_config(
        config,
        runtime_config(root)?,
    ))
}

fn request_bytes(op: &str, request_id: &str, args: Value) -> TestResult<Vec<u8>> {
    Ok(serde_json::to_vec(&json!({
        "op": op,
        "request_id": request_id,
        "scope": { "kind": "sandbox", "sandbox_id": "sandbox-1" },
        "args": args,
    }))?)
}

fn server_config(root: &Path, sandbox_id: Option<&str>) -> ServerConfig {
    let mut observability = ObservabilityConfig::default();
    observability.diagnostics.cpu_threshold_percent = 10_000.0;
    observability.diagnostics.anonymous_memory_threshold_bytes = u64::MAX;
    ServerConfig {
        socket_path: root.join("runtime.sock"),
        pid_path: root.join("runtime.pid"),
        tcp_host: None,
        tcp_port: None,
        http_host: None,
        http_port: None,
        auth_token: None,
        sandbox_id: sandbox_id.map(str::to_owned),
        cgroup_root: None,
        observability,
        limits: sandbox_protocol::ProtocolLimits::default(),
        max_concurrent_connections: 256,
        max_blocking_requests: 8,
        worker_threads: 2,
        blocking_thread_keep_alive_s: 5.0,
        forward: Default::default(),
    }
}

fn runtime_config(root: &Path) -> TestResult<sandbox_runtime::SandboxRuntimeConfig> {
    let layer_stack_root = root.join("layer-stack");
    let workspace_root = root.join("runtime-workspace");
    std::fs::create_dir_all(&workspace_root)?;
    sandbox_runtime_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, false)?;
    Ok(sandbox_runtime::SandboxRuntimeConfig {
        cgroup_root: None,
        workload_cgroup_limits: None,
        workload_cgroup_unavailable_reason: Some("test host has no delegation".to_owned()),
        workspace: sandbox_runtime::WorkspaceRuntimeConfig {
            workspace_root,
            layer_stack_root,
            scratch_root: root.join("workspace-scratch"),
            caps: sandbox_runtime::WorkspaceResourceCaps {
                setup_timeout_s: 30.0,
                exit_grace_s: 0.25,
                rfc1918_egress: sandbox_runtime::Rfc1918Egress::Allow,
                freeze_budget_s: 0.5,
            },
        },
        namespace_execution: sandbox_runtime::NamespaceExecutionRuntimeConfig {
            scratch_root: root.join("command-scratch"),
            caps: sandbox_runtime::NamespaceExecutionCaps::default(),
        },
        layerstack: sandbox_runtime::LayerstackRuntimeConfig::default(),
        command: sandbox_runtime::CommandRuntimeConfig::default(),
        file: sandbox_runtime::FileRuntimeConfig::default(),
    })
}

fn diagnostic_workspace(workspace_id: &str, holder_pid: i32) -> DaemonDiagnosticWorkspaceHolder {
    DaemonDiagnosticWorkspaceHolder {
        workspace_id: workspace_id.to_owned(),
        holder_pid,
    }
}

fn diagnostic_config(
    sustained_window_ms: u64,
    cooldown_ms: u64,
    max_artifact_bytes: usize,
) -> DiagnosticsConfig {
    DiagnosticsConfig {
        enabled: true,
        cpu_threshold_percent: 2.0,
        anonymous_memory_threshold_bytes: 1024,
        sustained_window_ms,
        cooldown_ms,
        max_artifact_bytes,
    }
}

fn diagnostic_runtime_usage() -> DaemonRuntimeUsage {
    DaemonRuntimeUsage {
        active_async_tasks: Some(0),
        active_blocking_tasks: Some(1),
        blocking_queue_depth: Some(0),
        blocking_admission_in_use: Some(1),
        connection_admission_in_use: Some(0),
        active_commands: Some(2),
        command_queue_depth: Some(0),
    }
}

fn diagnostic_ownership() -> DaemonOwnershipMetrics {
    DaemonOwnershipMetrics {
        open_workspaces: 1,
        live_holders: 1,
        exited_unreaped_holders: None,
        namespace_fd_count: Some(3),
        control_fd_count: Some(2),
        namespace_control_fd_count: Some(5),
        active_scratch_directories: Some(1),
        persisted_workspace_handles: Some(1),
        active_layer_leases: Some(1),
    }
}

fn diagnostic_process_metrics(
    sampled_at_unix_ms: u64,
    cpu_time_us: u64,
    anonymous_memory_bytes: u64,
) -> DaemonProcessMetrics {
    DaemonProcessMetrics {
        available: true,
        error: None,
        sampled_at_unix_ms,
        pid: 7,
        name: Some("sandbox-daemon".to_owned()),
        state: Some("S".to_owned()),
        virtual_memory_bytes: Some(4096),
        resident_memory_bytes: Some(2048),
        peak_resident_memory_bytes: Some(3072),
        proportional_set_size_bytes: Some(1536),
        unique_set_size_bytes: Some(1024),
        private_dirty_bytes: Some(768),
        anonymous_huge_pages_bytes: Some(0),
        anonymous_memory_bytes: Some(anonymous_memory_bytes),
        file_memory_bytes: Some(128),
        shared_memory_bytes: Some(0),
        data_memory_bytes: Some(512),
        swap_bytes: Some(0),
        cpu_time_us: Some(cpu_time_us),
        start_time_ticks: Some(11),
        thread_count: Some(5),
        file_descriptor_count: Some(12),
        io_read_bytes: Some(64),
        io_write_bytes: Some(32),
        read_syscalls: Some(4),
        write_syscalls: Some(2),
        voluntary_context_switches: Some(8),
        involuntary_context_switches: Some(1),
        cgroup_memberships: vec!["0::/sandbox".to_owned()],
        cgroup_path: Some("/sandbox".to_owned()),
        warnings: Vec::new(),
        runtime_config: DaemonRuntimeConfigMetrics {
            worker_threads: Some(2),
            max_blocking_threads: Some(8),
            blocking_thread_keep_alive_s: Some(5.0),
            max_concurrent_connections: Some(256),
            max_active_commands: Some(256),
            max_blocking_queue_depth: Some(0),
            max_command_queue_depth: Some(0),
            infrastructure_thread_allowance: Some(4),
        },
        runtime_usage: Default::default(),
        ownership: Default::default(),
        lifecycle: Default::default(),
        allocator: Default::default(),
        diagnostics: Default::default(),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    allocated_blocks: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    sha256: [u8; 32],
}

fn fingerprint(path: &Path) -> TestResult<FileFingerprint> {
    let metadata = fs::metadata(path)?;
    let digest = Sha256::digest(fs::read(path)?);
    Ok(FileFingerprint {
        len: metadata.len(),
        allocated_blocks: metadata.blocks(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        sha256: digest.into(),
    })
}

fn test_root(label: &str) -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "sandbox-daemon-observability-{label}-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create test root");
    root
}

#[test]
fn workspace_cgroup_paths_use_unified_hierarchy_coordinates() {
    assert_eq!(
        crate::observability::adapter::hierarchy_cgroup_path(Path::new(
            "/sys/fs/cgroup/docker/example/_workloads/workspace-1",
        )),
        "/docker/example/_workloads/workspace-1"
    );
    assert_eq!(
        crate::observability::adapter::hierarchy_cgroup_path(Path::new("/sys/fs/cgroup")),
        "/"
    );
    assert_eq!(
        crate::observability::adapter::hierarchy_cgroup_path(Path::new("/already-relative")),
        "/already-relative"
    );
}
