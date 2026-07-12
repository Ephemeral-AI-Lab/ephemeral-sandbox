use std::path::PathBuf;

#[test]
fn config_prd_manager_docker_section_deserializes_and_validates() {
    let docker = prd_docker();
    docker.validate().expect("prd manager.docker config is valid");

    assert_eq!(docker.daemon_port, 7000);
    assert_eq!(docker.readiness_timeout_ms, 60_000);
    assert_eq!(docker.container_workspace_root, PathBuf::from("/workspace"));
    assert_eq!(docker.gateway_instance_id, "eos-gateway");
    // Phase 3: prd runs the de-privileged container boundary.
    assert!(!docker.privileged);
}

#[test]
fn config_prd_manager_docker_injects_proxy_container_env() {
    let docker = prd_docker();

    assert_eq!(
        docker.container_env.get("HTTP_PROXY").map(String::as_str),
        Some("http://http.docker.internal:3128")
    );
    assert_eq!(
        docker.container_env.get("HTTPS_PROXY").map(String::as_str),
        Some("http://http.docker.internal:3128")
    );
    assert_eq!(
        docker.container_env.get("NO_PROXY").map(String::as_str),
        Some("localhost,127.0.0.1,::1")
    );
}

#[test]
fn container_env_defaults_to_empty() {
    assert!(DockerRuntimeConfig::default().container_env.is_empty());
}

#[test]
fn validate_rejects_blank_container_env_name() {
    let mut docker = prd_docker();
    docker
        .container_env
        .insert(String::new(), "value".to_owned());
    assert_invalid(&docker, "manager.docker.container_env");
}

#[test]
fn validate_rejects_container_env_name_with_equals() {
    let mut docker = prd_docker();
    docker
        .container_env
        .insert("HTTP=PROXY".to_owned(), "value".to_owned());
    assert_invalid(&docker, "manager.docker.container_env");
}

#[test]
fn manager_section_defaults_to_no_docker_backend() {
    // The `none` backend needs no manager section, so a default ManagerConfig
    // carries no docker backend.
    let manager = ManagerConfig::default();
    assert!(manager.docker.is_none());
}

#[test]
fn manager_registry_path_defaults_to_none_and_deserializes_when_set() {
    // The prd baseline sets no registry path, so the registry stays in-memory
    // unless a deployment opts in.
    let baseline: ManagerConfig = crate::load_baseline()
        .expect("prd config loads")
        .section("manager")
        .expect("manager section deserializes");
    assert!(baseline.registry_path.is_none());

    let doc = crate::ConfigDocument::parse(
        std::path::Path::new("<test>"),
        "manager:\n  registry_path: /var/lib/eos/sandboxes.json\n",
    )
    .expect("document parses");
    let manager: ManagerConfig = doc
        .section("manager")
        .expect("manager section deserializes");
    assert_eq!(
        manager.registry_path,
        Some(PathBuf::from("/var/lib/eos/sandboxes.json"))
    );
}

#[test]
fn manager_workspace_roots_default_and_overrides_validate() {
    let default = ManagerConfig::default();
    assert!(default.workspace_roots.is_none());

    let doc = crate::ConfigDocument::parse(
        std::path::Path::new("<test>"),
        "manager:\n  workspace_roots: [/Users/me/projects, /tmp/workspaces]\n",
    )
    .expect("document parses");
    let manager: ManagerConfig = doc
        .section("manager")
        .expect("manager section deserializes");
    manager.validate().expect("workspace roots are valid");
    assert_eq!(
        manager.workspace_roots,
        Some(vec![
            PathBuf::from("/Users/me/projects"),
            PathBuf::from("/tmp/workspaces")
        ])
    );
}

#[test]
fn manager_workspace_roots_reject_empty_and_relative_values() {
    let manager = ManagerConfig {
        workspace_roots: Some(Vec::new()),
        ..ManagerConfig::default()
    };
    assert_invalid_manager(&manager, "manager.workspace_roots");

    let manager = ManagerConfig {
        workspace_roots: Some(vec![PathBuf::from("relative/workspace")]),
        ..ManagerConfig::default()
    };
    assert_invalid_manager(&manager, "manager.workspace_roots");
}

#[test]
fn validate_rejects_blank_gateway_instance_id() {
    let mut docker = prd_docker();
    docker.gateway_instance_id = String::new();
    assert_invalid(&docker, "manager.docker.gateway_instance_id");
}

#[test]
fn validate_rejects_relative_container_workspace_root() {
    let mut docker = prd_docker();
    docker.container_workspace_root = PathBuf::from("relative/workspace");
    assert_invalid(&docker, "manager.docker.container_workspace_root");
}

#[test]
fn validate_rejects_empty_daemon_binary_path() {
    let mut docker = prd_docker();
    docker.daemon_binary_path = PathBuf::new();
    assert_invalid(&docker, "manager.docker.daemon_binary_path");
}

#[test]
fn config_manager_export_defaults_preserve_shipped_policy() {
    // prd.yml carries no manager.export key, so the section must load to
    // today's exact constants.
    let manager: ManagerConfig = crate::load_baseline()
        .expect("prd config loads")
        .section("manager")
        .expect("manager section deserializes");
    assert_eq!(manager.export, ManagerExportConfig::default());
    assert_eq!(manager.export.max_stream_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(
        manager.export.max_decompressed_bytes,
        8 * 1024 * 1024 * 1024
    );
    assert_eq!(manager.export.max_apply_entries, 1_000_000);
    manager.validate().expect("prd manager config is valid");
}

#[test]
fn config_manager_export_overrides_deserialize() {
    let doc = crate::ConfigDocument::parse(
        std::path::Path::new("<test>"),
        "manager:\n  export:\n    max_stream_bytes: 4096\n    max_apply_entries: 1\n",
    )
    .expect("document parses");
    let manager: ManagerConfig = doc
        .section("manager")
        .expect("manager section deserializes");
    manager.validate().expect("export overrides are valid");
    assert_eq!(manager.export.max_stream_bytes, 4096);
    assert_eq!(
        manager.export.max_decompressed_bytes,
        8 * 1024 * 1024 * 1024
    );
    assert_eq!(manager.export.max_apply_entries, 1);
}

#[test]
fn config_manager_export_rejects_unknown_key() {
    let doc = crate::ConfigDocument::parse(
        std::path::Path::new("<test>"),
        "manager:\n  export:\n    max_stream_mb: 1\n",
    )
    .expect("document parses");
    let error = doc
        .section::<ManagerConfig>("manager")
        .expect_err("unknown export key must be rejected");
    assert!(error.to_string().contains("max_stream_mb"), "{error}");
}

#[test]
fn config_validation_rejects_zero_export_caps() {
    let mut manager = ManagerConfig::default();
    manager.export.max_stream_bytes = 0;
    assert_invalid_manager(&manager, "manager.export.max_stream_bytes");

    let mut manager = ManagerConfig::default();
    manager.export.max_decompressed_bytes = 0;
    assert_invalid_manager(&manager, "manager.export.max_decompressed_bytes");

    let mut manager = ManagerConfig::default();
    manager.export.max_apply_entries = 0;
    assert_invalid_manager(&manager, "manager.export.max_apply_entries");
}

#[test]
fn config_docker_timing_defaults_preserve_shipped_policy() {
    // prd.yml carries none of the tier-4 docker timing keys, so every knob
    // must load to today's exact constants.
    let docker = prd_docker();
    assert_eq!(docker.connect_timeout_s, 120);
    assert_eq!(docker.stop_timeout_s, 5);
    assert_eq!(docker.readiness_poll_ms, 250);
    assert_eq!(docker.port_publish_attempts, 200);
    assert_eq!(docker.port_publish_retry_delay_ms, 50);
}

#[test]
fn config_docker_timing_overrides_deserialize() {
    let doc = crate::ConfigDocument::parse(
        std::path::Path::new("<test>"),
        "manager:
  docker:
    daemon_binary_path: dist/sandbox-daemon
    daemon_config_yaml_path: config/prd.yml
    connect_timeout_s: 10
    stop_timeout_s: 1
    readiness_poll_ms: 25
    port_publish_attempts: 3
    port_publish_retry_delay_ms: 5
",
    )
    .expect("document parses");
    let manager: ManagerConfig = doc
        .section("manager")
        .expect("manager section deserializes");
    manager.validate().expect("docker timing overrides valid");
    let docker = manager.docker.expect("docker section present");
    assert_eq!(docker.connect_timeout_s, 10);
    assert_eq!(docker.stop_timeout_s, 1);
    assert_eq!(docker.readiness_poll_ms, 25);
    assert_eq!(docker.port_publish_attempts, 3);
    assert_eq!(docker.port_publish_retry_delay_ms, 5);
}

#[test]
fn config_validation_rejects_zero_docker_timing_values() {
    let mut docker = prd_docker();
    docker.connect_timeout_s = 0;
    assert_invalid(&docker, "manager.docker.connect_timeout_s");

    let mut docker = prd_docker();
    docker.stop_timeout_s = 0;
    assert_invalid(&docker, "manager.docker.stop_timeout_s");

    let mut docker = prd_docker();
    docker.readiness_poll_ms = 0;
    assert_invalid(&docker, "manager.docker.readiness_poll_ms");

    let mut docker = prd_docker();
    docker.port_publish_attempts = 0;
    assert_invalid(&docker, "manager.docker.port_publish_attempts");

    let mut docker = prd_docker();
    docker.port_publish_retry_delay_ms = 0;
    assert_invalid(&docker, "manager.docker.port_publish_retry_delay_ms");
}

#[test]
fn config_manager_snapshot_and_local_daemon_defaults_preserve_shipped_policy() {
    // prd.yml carries neither subsection, so both must load to today's
    // exact constants.
    let manager: ManagerConfig = crate::load_baseline()
        .expect("prd config loads")
        .section("manager")
        .expect("manager section deserializes");
    assert_eq!(
        manager.observability_snapshot,
        ManagerObservabilitySnapshotConfig::default()
    );
    assert_eq!(manager.observability_snapshot.max_concurrent_requests, 8);
    assert_eq!(manager.observability_snapshot.timeout_ms, 1_500);
    assert_eq!(manager.local_daemon, ManagerLocalDaemonConfig::default());
    assert!((manager.local_daemon.ready_timeout_s - 2.0).abs() < f64::EPSILON);
    assert!((manager.local_daemon.stop_timeout_s - 2.0).abs() < f64::EPSILON);
}

#[test]
fn config_manager_snapshot_and_local_daemon_overrides_deserialize() {
    let doc = crate::ConfigDocument::parse(
        std::path::Path::new("<test>"),
        "manager:
  observability_snapshot:
    max_concurrent_requests: 2
  local_daemon:
    stop_timeout_s: 0.5
",
    )
    .expect("document parses");
    let manager: ManagerConfig = doc
        .section("manager")
        .expect("manager section deserializes");
    manager.validate().expect("subsection overrides valid");
    assert_eq!(manager.observability_snapshot.max_concurrent_requests, 2);
    assert_eq!(manager.observability_snapshot.timeout_ms, 1_500);
    assert!((manager.local_daemon.stop_timeout_s - 0.5).abs() < f64::EPSILON);
    assert!((manager.local_daemon.ready_timeout_s - 2.0).abs() < f64::EPSILON);
}

#[test]
fn config_manager_snapshot_and_local_daemon_reject_unknown_keys() {
    let doc = crate::ConfigDocument::parse(
        std::path::Path::new("<test>"),
        "manager:\n  observability_snapshot:\n    parallelism: 2\n",
    )
    .expect("document parses");
    let error = doc
        .section::<ManagerConfig>("manager")
        .expect_err("unknown snapshot key must be rejected");
    assert!(error.to_string().contains("parallelism"), "{error}");

    let doc = crate::ConfigDocument::parse(
        std::path::Path::new("<test>"),
        "manager:\n  local_daemon:\n    ready_ms: 100\n",
    )
    .expect("document parses");
    let error = doc
        .section::<ManagerConfig>("manager")
        .expect_err("unknown local_daemon key must be rejected");
    assert!(error.to_string().contains("ready_ms"), "{error}");
}

#[test]
fn config_validation_rejects_snapshot_and_local_daemon_edge_values() {
    let mut manager = ManagerConfig::default();
    manager.observability_snapshot.max_concurrent_requests = 0;
    assert_invalid_manager(
        &manager,
        "manager.observability_snapshot.max_concurrent_requests",
    );

    let mut manager = ManagerConfig::default();
    manager.observability_snapshot.timeout_ms = 0;
    assert_invalid_manager(&manager, "manager.observability_snapshot.timeout_ms");

    let mut manager = ManagerConfig::default();
    manager.local_daemon.ready_timeout_s = 0.0;
    assert_invalid_manager(&manager, "manager.local_daemon.ready_timeout_s");

    let mut manager = ManagerConfig::default();
    manager.local_daemon.stop_timeout_s = -1.0;
    assert_invalid_manager(&manager, "manager.local_daemon.stop_timeout_s");
}

#[test]
fn manager_validate_delegates_to_docker_section() {
    let mut manager = ManagerConfig::default();
    let mut docker = prd_docker();
    docker.gateway_instance_id = String::new();
    manager.docker = Some(docker);
    assert_invalid_manager(&manager, "manager.docker.gateway_instance_id");
}

fn prd_docker() -> DockerRuntimeConfig {
    let manager: ManagerConfig = crate::load_baseline()
        .expect("prd config loads")
        .section("manager")
        .expect("manager section deserializes");
    manager.docker.expect("manager.docker section present")
}

fn assert_invalid_manager(config: &ManagerConfig, field: &str) {
    let err = config.validate().expect_err("config should be invalid");
    assert!(err.to_string().contains(field), "{err}");
}

fn assert_invalid(config: &DockerRuntimeConfig, field: &str) {
    let err = config.validate().expect_err("config should be invalid");
    assert!(err.to_string().contains(field), "{err}");
}
