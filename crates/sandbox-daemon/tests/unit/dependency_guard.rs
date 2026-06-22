#[test]
fn daemon_manifest_excludes_host_store_and_sqlite_dependencies() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("read daemon manifest");
    for forbidden in ["rusqlite", "host"] {
        assert!(
            !manifest.contains(forbidden),
            "daemon hot path must not depend on {forbidden}"
        );
    }
}

#[test]
fn forbidden_runtime_telemetry_infrastructure_is_absent() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root");

    for forbidden in [
        "crates/sandbox-runtime-trace",
        "crates/sandbox-runtime/operation/src/internal/telemetry.rs",
    ] {
        assert!(
            !workspace_root.join(forbidden).exists(),
            "forbidden telemetry infrastructure exists: {forbidden}"
        );
    }

    for relative in telemetry_boundary_source_and_manifest_files(workspace_root) {
        let text = std::fs::read_to_string(&relative).expect("read source or manifest");
        for forbidden in [
            "TelemetryConfig",
            "TelemetrySink",
            "TelemetryOutputStream",
            "DaemonServeMode",
            "tracing_subscriber",
            "SubscriberInitExt",
            "set_global_default",
            "opentelemetry",
            "otlp",
        ] {
            assert!(
                !text.contains(forbidden),
                "runtime/config boundary must not define telemetry DTOs or subscriber/exporter setup: {} contains {forbidden}",
                relative.display()
            );
        }
    }
}

fn telemetry_boundary_source_and_manifest_files(
    workspace_root: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for root in ["crates/sandbox-runtime", "crates/sandbox-config"] {
        collect_telemetry_boundary_files(&workspace_root.join(root), &mut files);
    }
    files
}

fn collect_telemetry_boundary_files(path: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(path).expect("read telemetry boundary crate directory");
    for entry in entries {
        let entry = entry.expect("read telemetry boundary crate entry");
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(name.as_ref(), "target" | "tests") {
                continue;
            }
            collect_telemetry_boundary_files(&path, files);
            continue;
        }
        if name == "Cargo.toml" || path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn daemon_metric_labels_are_allowlisted_in_source() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root");
    let metrics = std::fs::read_to_string(
        workspace_root.join("crates/sandbox-daemon/src/telemetry/metrics.rs"),
    )
    .expect("read daemon metrics source");

    for expected in [
        "KeyValue::new(\"operation\"",
        "KeyValue::new(\"workspace_phase\"",
        "KeyValue::new(\"cgroup_target_kind\"",
        "KeyValue::new(\"status\"",
        "KeyValue::new(\"bounded_reason\"",
        "KeyValue::new(\"bounded_error_kind\"",
        "KeyValue::new(\"resource_kind\"",
    ] {
        assert!(
            metrics.contains(expected),
            "metrics source is missing allowlisted label {expected}"
        );
    }

    for forbidden in [
        "request_id",
        "workspace_session_id",
        "command_session_id",
        "pid_list",
        "raw_path",
        "root_hash",
        "command_text",
        "stdin",
        "auth_token",
        "env_value",
        "cgroup_path",
        "layer_path",
    ] {
        assert!(
            !metrics.contains(&format!("KeyValue::new(\"{forbidden}\"")),
            "metrics source uses forbidden label {forbidden}"
        );
    }
}

#[test]
fn phase4_observability_stack_configures_metrics_without_loki() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root");
    for relative in [
        "observability/docker-compose.yml",
        "observability/otel-collector.yaml",
        "observability/prometheus.yml",
        "observability/grafana/provisioning/datasources/metrics.yaml",
        "observability/grafana/provisioning/datasources/tempo.yaml",
        "observability/grafana/provisioning/dashboards/dashboards.yaml",
    ] {
        let text = std::fs::read_to_string(workspace_root.join(relative))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        assert!(
            !text.to_ascii_lowercase().contains("loki"),
            "{relative} must not configure loki"
        );
        assert!(
            !text.contains("tracesToLogs") && !text.contains("derivedFields"),
            "{relative} must not configure trace-to-logs"
        );
    }

    let collector =
        std::fs::read_to_string(workspace_root.join("observability/otel-collector.yaml"))
            .expect("read collector config");
    assert!(collector.contains("metrics:"));
    assert!(collector.contains("exporters: [prometheus]"));
    assert!(
        !collector.contains("resource_to_telemetry_conversion"),
        "collector must not promote resource attributes such as sandbox ids into metric labels"
    );
    let metrics_datasource = std::fs::read_to_string(
        workspace_root.join("observability/grafana/provisioning/datasources/metrics.yaml"),
    )
    .expect("read metrics datasource");
    assert!(metrics_datasource.contains("uid: prometheus"));
    assert!(metrics_datasource.contains("type: prometheus"));
}

#[test]
fn phase4_dashboards_load_with_metrics_datasource_without_public_cgroup_reads_or_logs() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root");
    for file in [
        "command-latency.json",
        "publish-conflicts.json",
        "remount-health.json",
        "cgroup-resources.json",
    ] {
        let path = workspace_root
            .join("observability/trace/dashboards")
            .join(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for forbidden in [
            "inspect_cgroup_monitor",
            "read_cgroup_monitor_samples",
            "loki",
            "logs",
            "traceToLogs",
            "derivedFields",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} contains forbidden dashboard reference {forbidden}",
                path.display()
            );
        }
        let dashboard: serde_json::Value =
            serde_json::from_str(&text).expect("dashboard json parses");
        let panels = dashboard["panels"]
            .as_array()
            .expect("dashboard has panel array");
        assert!(
            !panels.is_empty(),
            "dashboard has panels: {}",
            path.display()
        );
        for panel in panels {
            assert_ne!(panel["type"].as_str(), Some("logs"));
            assert_eq!(panel["datasource"]["uid"].as_str(), Some("prometheus"));
            assert_eq!(panel["datasource"]["type"].as_str(), Some("prometheus"));
        }
    }
}
