use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sandbox_runtime_workspace::{
    build_cgroup_monitor_sample, session_cgroup_path, CgroupCleanupState, CgroupMonitorConfig,
    CgroupMonitorRegistry, CgroupMonitorSample, CgroupMonitorTargetKind, CgroupReadErrorKind,
    CgroupSampleKind, CgroupSampleRequest, LayerStackSnapshotRef, LeaseId, RuntimeMetricsRecorder,
    RuntimeMetricsRecorderHandle, WorkspaceHandle, WorkspaceProfile, WorkspaceSessionId,
};

use crate::trace_capture::{capture_traces, with_trace_capture_lock};

#[test]
fn cgroup_monitor_session_path_uses_session_owned_tree() {
    let path = session_cgroup_path(
        Path::new("/sys/fs/cgroup"),
        &WorkspaceSessionId("wss_123".to_owned()),
    );

    assert_eq!(path, PathBuf::from("/sys/fs/cgroup/eos/sessions/wss_123"));
}

#[test]
fn cgroup_monitor_parses_complete_cgroup_files() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("complete")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    std::fs::write(upper.join("file.txt"), b"abcd")?;
    write_complete_cgroup_files(&cgroup)?;

    let sample = build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &cgroup,
        upperdir: Some(&upper),
        sample_kind: CgroupSampleKind::Periodic,
        interval_ms: 1000,
        previous: None,
        config: &CgroupMonitorConfig::default(),
    });

    assert_eq!(sample.cpu.usage_usec, Some(1200));
    assert_eq!(sample.memory.current_bytes, Some(4096));
    assert_eq!(sample.io.read_bytes, Some(10));
    assert_eq!(sample.pids.sampled, vec![123, 124]);
    assert_eq!(sample.pressure.memory.full_total_usec, Some(7));
    assert_eq!(sample.disk.upperdir_bytes, 4);
    assert_eq!(sample.state.cgroup_populated, Some(true));
    assert_eq!(sample.state.frozen, Some(false));
    assert!(sample.state.read_error.is_none());

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_missing_optional_files_produce_partial_sample(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("missing")?;
    let cgroup = root.join("cgroup");
    std::fs::create_dir_all(&cgroup)?;

    let sample = build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &cgroup,
        upperdir: None,
        sample_kind: CgroupSampleKind::Periodic,
        interval_ms: 1000,
        previous: None,
        config: &CgroupMonitorConfig::default(),
    });

    assert!(sample.state.cgroup_exists);
    assert_eq!(sample.cpu.usage_usec, None);
    assert_eq!(sample.memory.current_bytes, None);
    assert_eq!(sample.pids.sampled, Vec::<u32>::new());
    assert!(sample
        .state
        .read_error
        .as_deref()
        .is_some_and(|error| error.contains("cpu.stat")));

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_malformed_files_report_read_error() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("malformed")?;
    let cgroup = root.join("cgroup");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::write(cgroup.join("cpu.stat"), "usage_usec nope\n")?;
    std::fs::write(cgroup.join("memory.current"), "not-a-number\n")?;
    std::fs::write(cgroup.join("memory.stat"), "anon 10\n")?;
    std::fs::write(cgroup.join("memory.events"), "oom bad\n")?;
    std::fs::write(cgroup.join("io.stat"), "8:0 rbytes=bad\n")?;
    std::fs::write(cgroup.join("pids.current"), "1\n")?;
    std::fs::write(cgroup.join("pids.peak"), "2\n")?;
    std::fs::write(cgroup.join("cgroup.procs"), "123\n")?;
    std::fs::write(cgroup.join("cpu.pressure"), "some avg10=bad total=3\n")?;
    std::fs::write(cgroup.join("memory.pressure"), "some avg10=0.00 total=4\n")?;
    std::fs::write(cgroup.join("io.pressure"), "some avg10=0.00 total=5\n")?;
    std::fs::write(cgroup.join("cgroup.events"), "populated 1\nfrozen 0\n")?;

    let sample = build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &cgroup,
        upperdir: None,
        sample_kind: CgroupSampleKind::Periodic,
        interval_ms: 1000,
        previous: None,
        config: &CgroupMonitorConfig::default(),
    });

    let error = sample
        .state
        .read_error
        .as_deref()
        .expect("malformed files are reported");
    assert!(error.contains("cpu.stat malformed value"));
    assert!(error.contains("memory.current malformed integer"));
    assert!(error.contains("io.stat malformed value"));
    assert_eq!(sample.memory.anon_bytes, Some(10));

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_reports_malformed_pid_lines() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("malformed-pids")?;
    let cgroup = root.join("cgroup");
    std::fs::create_dir_all(&cgroup)?;
    write_complete_cgroup_files(&cgroup)?;
    std::fs::write(cgroup.join("cgroup.procs"), "123\nnot-a-pid\n")?;

    let sample = build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &cgroup,
        upperdir: None,
        sample_kind: CgroupSampleKind::Periodic,
        interval_ms: 1000,
        previous: None,
        config: &CgroupMonitorConfig::default(),
    });

    assert_eq!(sample.pids.sampled, vec![123]);
    assert!(sample
        .state
        .read_error
        .as_deref()
        .is_some_and(|error| error.contains("cgroup.procs malformed pid")));

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_uses_elapsed_time_for_cpu_percent() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("elapsed-cpu")?;
    let cgroup = root.join("cgroup");
    std::fs::create_dir_all(&cgroup)?;
    write_complete_cgroup_files(&cgroup)?;

    let previous = build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &cgroup,
        upperdir: None,
        sample_kind: CgroupSampleKind::Periodic,
        interval_ms: 1000,
        previous: None,
        config: &CgroupMonitorConfig::default(),
    });
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(
        cgroup.join("cpu.stat"),
        "usage_usec 2200\nuser_usec 1800\nsystem_usec 400\n",
    )?;
    let sample = build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &cgroup,
        upperdir: None,
        sample_kind: CgroupSampleKind::Periodic,
        interval_ms: 1000,
        previous: Some(&previous),
        config: &CgroupMonitorConfig::default(),
    });

    assert!(
        sample.interval_ms < 1000,
        "sample interval was {}",
        sample.interval_ms
    );
    assert!(sample.cpu.percent_over_interval.unwrap_or_default() > 0.1);

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_registry_samples_without_public_reads() -> Result<(), Box<dyn std::error::Error>>
{
    let root = temp_root("registry-loop")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    write_complete_cgroup_files(&cgroup)?;
    let registry = CgroupMonitorRegistry::new(CgroupMonitorConfig {
        sample_interval_ms: 1,
        retained_samples_per_target: 10,
        include_disk: false,
        ..CgroupMonitorConfig::default()
    });
    registry.register_command(
        WorkspaceSessionId("ws-loop".to_owned()),
        "cmd-loop",
        cgroup.clone(),
        upper,
    );
    std::fs::write(
        cgroup.join("cpu.stat"),
        "usage_usec 3200\nuser_usec 2800\nsystem_usec 400\n",
    )?;

    let mut saw_background_sample = false;
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        let samples = registry
            .read_samples(
                &WorkspaceSessionId("ws-loop".to_owned()),
                Some("cmd-loop"),
                10,
            )
            .expect("registered target has samples")
            .samples;
        if samples.len() >= 2
            && samples.last().and_then(|sample| sample.cpu.usage_usec) == Some(3200)
        {
            saw_background_sample = true;
            break;
        }
    }

    assert!(saw_background_sample);
    drop(registry);
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_healthy_periodic_samples_emit_no_trace_events(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("periodic-trace-silent")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    write_complete_cgroup_files(&cgroup)?;

    let traces = capture_traces(|| {
        let registry = CgroupMonitorRegistry::new(CgroupMonitorConfig {
            sample_interval_ms: 60_000,
            retained_samples_per_target: 10,
            include_disk: false,
            ..CgroupMonitorConfig::default()
        });
        registry.register_command(
            WorkspaceSessionId("ws-periodic-trace".to_owned()),
            "cmd-periodic-trace",
            cgroup.clone(),
            upper,
        );
        assert!(registry
            .read_samples(
                &WorkspaceSessionId("ws-periodic-trace".to_owned()),
                Some("cmd-periodic-trace"),
                10,
            )
            .expect("registered target has samples")
            .samples
            .iter()
            .all(|sample| sample.sample_kind == CgroupSampleKind::Periodic));
    });

    assert!(
        traces.trim().is_empty(),
        "healthy periodic samples must be trace-silent: {traces}"
    );
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_final_and_anomaly_events_are_bounded() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("bounded-final-anomaly-RAW_CGROUP_PATH_SECRET")?;
    let cgroup = root.join("cgroup-RAW_CGROUP_PATH_SECRET");
    let upper = root.join("upper-RAW_UPPER_PATH_SECRET");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    std::fs::write(cgroup.join("cpu.stat"), "usage_usec nope\n")?;

    let traces = capture_traces(|| {
        let registry = CgroupMonitorRegistry::new(CgroupMonitorConfig {
            sample_interval_ms: 60_000,
            retained_samples_per_target: 10,
            include_disk: false,
            include_pids: false,
            include_pressure: false,
            ..CgroupMonitorConfig::default()
        });
        let workspace_session_id = WorkspaceSessionId("ws-final-trace".to_owned());
        registry.register_command(
            workspace_session_id.clone(),
            "cmd-final-trace",
            cgroup.clone(),
            upper,
        );
        let final_sample = build_cgroup_monitor_sample(CgroupSampleRequest {
            cgroup_path: &cgroup,
            upperdir: None,
            sample_kind: CgroupSampleKind::CommandFinal,
            interval_ms: 1000,
            previous: None,
            config: registry.config(),
        });
        registry.record_command_final(
            &workspace_session_id,
            "cmd-final-trace",
            Some(final_sample),
            Some(CgroupCleanupState {
                final_sample_recorded: false,
                cgroup_exists_after_destroy: Some(true),
                last_cleanup_error: Some(
                    "RAW_CLEANUP_ERROR_SECRET /tmp/RAW_CGROUP_PATH_SECRET".to_owned(),
                ),
            }),
        );
        registry.record_cleanup(
            &workspace_session_id,
            Some("cmd-final-trace"),
            Some(false),
            Some("RAW_CLEANUP_ERROR_SECRET".to_owned()),
        );
    });

    for expected in [
        "event cgroup_monitor.anomaly",
        "anomaly_class=malformed_cgroup_file",
        "event cgroup_monitor.final_summary",
        "boundary=command_final",
        "boundary=cleanup",
        "target_kind=command",
        "sample_kind=command_final",
        "cleanup_error=true",
    ] {
        assert!(traces.contains(expected), "missing {expected} in {traces}");
    }
    for forbidden in [
        "RAW_CGROUP_PATH_SECRET",
        "RAW_UPPER_PATH_SECRET",
        "RAW_CLEANUP_ERROR_SECRET",
        "cpu.stat malformed value",
        "CgroupMonitorSample",
        "CgroupCleanupState",
    ] {
        assert!(
            !traces.contains(forbidden),
            "forbidden value {forbidden} appeared in traces: {traces}"
        );
    }

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_registry_reads_do_not_resample_finalized_targets(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("final-retained")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    write_complete_cgroup_files(&cgroup)?;
    let registry = CgroupMonitorRegistry::new(CgroupMonitorConfig {
        retained_samples_per_target: 10,
        include_disk: false,
        ..CgroupMonitorConfig::default()
    });
    let workspace_session_id = WorkspaceSessionId("ws-final".to_owned());
    registry.register_command(
        workspace_session_id.clone(),
        "cmd-final",
        cgroup.clone(),
        upper,
    );
    let final_sample = build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &cgroup,
        upperdir: None,
        sample_kind: CgroupSampleKind::CommandFinal,
        interval_ms: 1000,
        previous: None,
        config: registry.config(),
    });
    std::fs::remove_dir_all(&cgroup)?;
    with_trace_capture_lock(|| {
        registry.record_command_final(
            &workspace_session_id,
            "cmd-final",
            Some(final_sample),
            Some(CgroupCleanupState {
                final_sample_recorded: false,
                cgroup_exists_after_destroy: Some(false),
                last_cleanup_error: None,
            }),
        );
    });

    let first = registry
        .inspect(&workspace_session_id, Some("cmd-final"))
        .expect("registered target is retained");
    assert_eq!(
        first.latest.as_ref().map(|sample| sample.sample_kind),
        Some(CgroupSampleKind::CommandFinal)
    );
    assert!(first.cleanup.final_sample_recorded);
    let sample_count = first.monitor.retained_samples;

    for _ in 0..3 {
        let snapshot = registry
            .inspect(&workspace_session_id, Some("cmd-final"))
            .expect("retained target remains readable");
        assert_eq!(snapshot.monitor.retained_samples, sample_count);
        assert_eq!(
            snapshot.latest.as_ref().map(|sample| sample.sample_kind),
            Some(CgroupSampleKind::CommandFinal)
        );
    }

    drop(registry);
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_command_final_uses_retained_previous_cpu(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("command-final-cpu")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    write_complete_cgroup_files(&cgroup)?;
    let registry = CgroupMonitorRegistry::new(CgroupMonitorConfig {
        retained_samples_per_target: 10,
        include_disk: false,
        ..CgroupMonitorConfig::default()
    });
    let workspace_session_id = WorkspaceSessionId("ws-final-cpu".to_owned());
    registry.register_command(
        workspace_session_id.clone(),
        "cmd-final-cpu",
        cgroup.clone(),
        upper,
    );
    std::fs::write(
        cgroup.join("cpu.stat"),
        "usage_usec 2200\nuser_usec 1800\nsystem_usec 400\n",
    )?;
    let final_sample = build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &cgroup,
        upperdir: None,
        sample_kind: CgroupSampleKind::CommandFinal,
        interval_ms: 1000,
        previous: None,
        config: registry.config(),
    });

    with_trace_capture_lock(|| {
        registry.record_command_final(
            &workspace_session_id,
            "cmd-final-cpu",
            Some(final_sample),
            None,
        );
    });

    let samples = registry
        .read_samples(&workspace_session_id, Some("cmd-final-cpu"), 10)
        .expect("registered target has samples")
        .samples;
    let final_sample = samples.last().expect("final sample is retained");
    assert_eq!(final_sample.sample_kind, CgroupSampleKind::CommandFinal);
    assert_eq!(final_sample.cpu.usage_usec, Some(2200));
    assert_eq!(final_sample.cpu.delta_usage_usec, Some(1000));
    assert!(final_sample.cpu.percent_over_interval.is_some());

    drop(registry);
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_metrics_use_final_sample_before_cleanup() -> Result<(), Box<dyn std::error::Error>>
{
    let root = temp_root("command-final-metrics")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    write_complete_cgroup_files(&cgroup)?;
    let metrics = RecordingMetrics::default_handle();
    let recorder: RuntimeMetricsRecorderHandle = metrics.clone();
    let registry = CgroupMonitorRegistry::with_metrics_recorder(
        CgroupMonitorConfig {
            retained_samples_per_target: 1,
            include_disk: false,
            ..CgroupMonitorConfig::default()
        },
        recorder,
    );
    let workspace_session_id = WorkspaceSessionId("ws-final-metrics".to_owned());
    registry.register_command(
        workspace_session_id.clone(),
        "cmd-final-metrics",
        cgroup.clone(),
        upper,
    );
    std::fs::write(
        cgroup.join("cpu.stat"),
        "usage_usec 2200\nuser_usec 1800\nsystem_usec 400\n",
    )?;
    let final_sample = build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &cgroup,
        upperdir: None,
        sample_kind: CgroupSampleKind::CommandFinal,
        interval_ms: 1000,
        previous: None,
        config: registry.config(),
    });
    std::fs::remove_dir_all(&cgroup)?;
    with_trace_capture_lock(|| {
        registry.record_command_final(
            &workspace_session_id,
            "cmd-final-metrics",
            Some(final_sample),
            Some(CgroupCleanupState {
                final_sample_recorded: false,
                cgroup_exists_after_destroy: Some(false),
                last_cleanup_error: None,
            }),
        );
        registry.record_cleanup(
            &workspace_session_id,
            Some("cmd-final-metrics"),
            Some(false),
            None,
        );
    });

    let observed = metrics.observed_samples();
    assert_eq!(
        observed
            .iter()
            .map(|(_, sample)| sample.sample_kind)
            .collect::<Vec<_>>(),
        [CgroupSampleKind::Periodic, CgroupSampleKind::CommandFinal]
    );
    let (_, final_metric_sample) = observed.last().expect("final metric sample recorded");
    assert_eq!(final_metric_sample.cpu.usage_usec, Some(2200));
    assert_eq!(final_metric_sample.cpu.delta_usage_usec, Some(1000));
    assert!(final_metric_sample.cpu.percent_over_interval.is_some());

    let snapshot = registry
        .inspect(&workspace_session_id, Some("cmd-final-metrics"))
        .expect("retained target is readable");
    assert_eq!(snapshot.monitor.retained_samples, 1);
    assert_eq!(
        snapshot.latest.as_ref().map(|sample| sample.sample_kind),
        Some(CgroupSampleKind::CommandFinal)
    );
    let metric_count_after_cleanup = metrics.observed_samples().len();
    let _ = registry.inspect(&workspace_session_id, Some("cmd-final-metrics"));
    let _ = registry.read_samples(&workspace_session_id, Some("cmd-final-metrics"), 10);
    assert_eq!(
        metrics.observed_samples().len(),
        metric_count_after_cleanup,
        "direct monitor reads must not emit cgroup sample metrics"
    );

    drop(registry);
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_read_error_metrics_use_bounded_error_kind(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("read-error-metrics")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    std::fs::write(cgroup.join("cpu.stat"), "usage_usec nope\n")?;
    let metrics = RecordingMetrics::default_handle();
    let recorder: RuntimeMetricsRecorderHandle = metrics.clone();
    let registry = CgroupMonitorRegistry::with_metrics_recorder(
        CgroupMonitorConfig {
            sample_interval_ms: 60_000,
            include_disk: false,
            include_pids: false,
            include_pressure: false,
            ..CgroupMonitorConfig::default()
        },
        recorder,
    );

    with_trace_capture_lock(|| {
        registry.register_command(
            WorkspaceSessionId("ws-read-error-metrics".to_owned()),
            "cmd-read-error-metrics",
            cgroup,
            upper,
        );
    });

    assert_eq!(
        metrics.read_errors(),
        [(
            CgroupMonitorTargetKind::Command,
            CgroupReadErrorKind::MalformedCgroupFile
        )]
    );

    drop(registry);
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_session_final_marks_cleanup_state() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("session-final")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    let work = root.join("work");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    std::fs::create_dir_all(&work)?;
    write_complete_cgroup_files(&cgroup)?;
    let handle = WorkspaceHandle::holder_backed_for_test(
        WorkspaceSessionId("ws-session-final".to_owned()),
        PathBuf::from("/workspace"),
        WorkspaceProfile::HostCompatible,
        test_snapshot(),
        upper,
        work,
        Some(cgroup),
    );
    let registry = CgroupMonitorRegistry::default();
    registry.register_session_from_handle(&handle);
    with_trace_capture_lock(|| {
        registry.record_session_final_from_handle(&handle);
    });

    let snapshot = registry
        .inspect(&WorkspaceSessionId("ws-session-final".to_owned()), None)
        .expect("session target is retained");
    assert!(snapshot.cleanup.final_sample_recorded);
    assert_eq!(
        snapshot.latest.as_ref().map(|sample| sample.sample_kind),
        Some(CgroupSampleKind::SessionFinal)
    );

    drop(registry);
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_session_final_cleanup_state_remains_available_to_telemetry(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("session-final-cleanup-telemetry")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    let work = root.join("work");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    std::fs::create_dir_all(&work)?;
    write_complete_cgroup_files(&cgroup)?;
    let workspace_session_id = WorkspaceSessionId("ws-session-cleanup-telemetry".to_owned());
    let handle = WorkspaceHandle::holder_backed_for_test(
        workspace_session_id.clone(),
        PathBuf::from("/workspace"),
        WorkspaceProfile::HostCompatible,
        test_snapshot(),
        upper,
        work,
        Some(cgroup),
    );
    let metrics = RecordingMetrics::default_handle();
    let recorder: RuntimeMetricsRecorderHandle = metrics.clone();
    let registry =
        CgroupMonitorRegistry::with_metrics_recorder(CgroupMonitorConfig::default(), recorder);
    registry.register_session_from_handle(&handle);

    let traces = capture_traces(|| {
        registry.record_session_final_from_handle(&handle);
        registry.record_cleanup(&workspace_session_id, None, Some(false), None);
    });

    let snapshot = registry
        .inspect(&workspace_session_id, None)
        .expect("session target is retained");
    assert!(snapshot.cleanup.final_sample_recorded);
    assert_eq!(snapshot.cleanup.cgroup_exists_after_destroy, Some(false));
    assert_eq!(
        snapshot.latest.as_ref().map(|sample| sample.sample_kind),
        Some(CgroupSampleKind::SessionFinal)
    );
    assert_eq!(
        metrics
            .observed_samples()
            .iter()
            .map(|(_, sample)| sample.sample_kind)
            .collect::<Vec<_>>(),
        [CgroupSampleKind::Periodic, CgroupSampleKind::SessionFinal]
    );
    for expected in [
        "event cgroup_monitor.final_summary",
        "boundary=session_final",
        "boundary=cleanup",
        "target_kind=session",
        "sample_kind=session_final",
        "final_sample_recorded=true",
        "cgroup_exists_after_destroy=false",
        "cgroup_exists_after_destroy_present=true",
    ] {
        assert!(traces.contains(expected), "missing {expected} in {traces}");
    }

    drop(registry);
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cgroup_monitor_cleanup_state_does_not_evict_final_sample_when_retention_is_one(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("session-final-retention-one")?;
    let cgroup = root.join("cgroup");
    let upper = root.join("upper");
    let work = root.join("work");
    std::fs::create_dir_all(&cgroup)?;
    std::fs::create_dir_all(&upper)?;
    std::fs::create_dir_all(&work)?;
    write_complete_cgroup_files(&cgroup)?;
    let handle = WorkspaceHandle::holder_backed_for_test(
        WorkspaceSessionId("ws-session-retention-one".to_owned()),
        PathBuf::from("/workspace"),
        WorkspaceProfile::HostCompatible,
        test_snapshot(),
        upper,
        work,
        Some(cgroup),
    );
    let registry = CgroupMonitorRegistry::new(CgroupMonitorConfig {
        retained_samples_per_target: 1,
        ..CgroupMonitorConfig::default()
    });
    registry.register_session_from_handle(&handle);
    with_trace_capture_lock(|| {
        registry.record_session_final_from_handle(&handle);
        registry.record_cleanup(
            &WorkspaceSessionId("ws-session-retention-one".to_owned()),
            None,
            Some(false),
            None,
        );
    });

    let snapshot = registry
        .inspect(
            &WorkspaceSessionId("ws-session-retention-one".to_owned()),
            None,
        )
        .expect("session target is retained");
    assert_eq!(snapshot.monitor.retained_samples, 1);
    assert!(snapshot.cleanup.final_sample_recorded);
    assert_eq!(snapshot.cleanup.cgroup_exists_after_destroy, Some(false));
    assert_eq!(
        snapshot.latest.as_ref().map(|sample| sample.sample_kind),
        Some(CgroupSampleKind::SessionFinal)
    );

    drop(registry);
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[derive(Default)]
struct RecordingMetrics {
    samples: Mutex<Vec<(CgroupMonitorTargetKind, CgroupMonitorSample)>>,
    read_errors: Mutex<Vec<(CgroupMonitorTargetKind, CgroupReadErrorKind)>>,
}

impl RecordingMetrics {
    fn default_handle() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn observed_samples(&self) -> Vec<(CgroupMonitorTargetKind, CgroupMonitorSample)> {
        self.samples.lock().expect("metrics lock").clone()
    }

    fn read_errors(&self) -> Vec<(CgroupMonitorTargetKind, CgroupReadErrorKind)> {
        self.read_errors.lock().expect("metrics lock").clone()
    }
}

impl RuntimeMetricsRecorder for RecordingMetrics {
    fn record_cgroup_sample(
        &self,
        target_kind: CgroupMonitorTargetKind,
        sample: &CgroupMonitorSample,
    ) {
        self.samples
            .lock()
            .expect("metrics lock")
            .push((target_kind, sample.clone()));
    }

    fn record_cgroup_read_error(
        &self,
        target_kind: CgroupMonitorTargetKind,
        error_kind: CgroupReadErrorKind,
    ) {
        self.read_errors
            .lock()
            .expect("metrics lock")
            .push((target_kind, error_kind));
    }
}

fn write_complete_cgroup_files(cgroup: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(
        cgroup.join("cpu.stat"),
        "usage_usec 1200\nuser_usec 800\nsystem_usec 400\nnr_periods 10\nnr_throttled 1\nthrottled_usec 5\n",
    )?;
    std::fs::write(cgroup.join("memory.current"), "4096\n")?;
    std::fs::write(cgroup.join("memory.peak"), "8192\n")?;
    std::fs::write(
        cgroup.join("memory.stat"),
        "anon 100\nfile 200\nkernel 300\n",
    )?;
    std::fs::write(
        cgroup.join("memory.events"),
        "low 0\nhigh 1\nmax 2\noom 3\noom_kill 4\n",
    )?;
    std::fs::write(
        cgroup.join("io.stat"),
        "8:0 rbytes=10 wbytes=20 rios=1 wios=2 dbytes=3 dios=4\n",
    )?;
    std::fs::write(cgroup.join("pids.current"), "2\n")?;
    std::fs::write(cgroup.join("pids.peak"), "4\n")?;
    std::fs::write(cgroup.join("cgroup.procs"), "123\n124\n")?;
    std::fs::write(
        cgroup.join("cpu.pressure"),
        "some avg10=0.10 avg60=0.20 avg300=0.30 total=5\n",
    )?;
    std::fs::write(
        cgroup.join("memory.pressure"),
        "some avg10=0.10 avg60=0.20 avg300=0.30 total=6\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=7\n",
    )?;
    std::fs::write(
        cgroup.join("io.pressure"),
        "some avg10=0.10 avg60=0.20 avg300=0.30 total=8\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=9\n",
    )?;
    std::fs::write(cgroup.join("cgroup.events"), "populated 1\nfrozen 0\n")?;
    Ok(())
}

fn test_snapshot() -> LayerStackSnapshotRef {
    LayerStackSnapshotRef {
        lease_id: LeaseId("lease-1".to_owned()),
        manifest_version: 1,
        root_hash: "root".to_owned(),
        manifest: sandbox_runtime_layerstack::Manifest::new(
            1,
            vec![sandbox_runtime_layerstack::LayerRef {
                layer_id: "L000001-test".to_owned(),
                path: "layers/L000001-test".to_owned(),
            }],
            sandbox_runtime_layerstack::MANIFEST_SCHEMA_VERSION,
        )
        .expect("test manifest is valid"),
        layer_paths: vec![PathBuf::from("/lower/one")],
    }
}

fn temp_root(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(std::env::temp_dir().join(format!(
        "sandbox-runtime-workspace-cgroup-monitor-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    )))
}
