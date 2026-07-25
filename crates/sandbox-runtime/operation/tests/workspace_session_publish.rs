mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use sandbox_observability_telemetry::record::{names, proc};
use sandbox_observability_telemetry::{
    Observer, ObserverConfig, RawFilter, Reader, Record, Sink, TraceContext,
};
use sandbox_operation_contract::{OperationRequest, OperationScope};
use sandbox_runtime::command::{CommandStatus, ExecCommandInput};
use sandbox_runtime::file::FileService;
use sandbox_runtime::workspace_session::{
    FinalizationState, WorkspaceSessionError, WorkspaceSessionService,
};
use sandbox_runtime::{LayerStackService, LayerstackRuntimeConfig, SandboxRuntimeOperations};
use sandbox_runtime_workspace::{
    CapturedWorkspaceChanges, FileRunnerOp, LayerStackSnapshotRef, LeaseId, NetworkProfile,
    ProtectedPathDrop, ProtectedPathDropReason, WorkspaceError, WorkspaceHandle,
    WorkspaceSessionId,
};
use serde_json::{json, Value};

use support::{
    build_services_with_launch_driver_and_layerstack, create_request, FakeLaunchDriver,
    FakeWorkspaceService, ScriptedCommandYield,
};

static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

struct PublishFixture {
    root_dir: PathBuf,
    layer_stack_root: PathBuf,
    workspace_root: PathBuf,
}

impl PublishFixture {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let root_dir = std::env::temp_dir().join(format!(
            "workspace-session-publish-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root_dir);
        let layer_stack_root = root_dir.join("layer-stack");
        let workspace_root = root_dir.join("workspace");
        std::fs::create_dir_all(&workspace_root)?;
        Ok(Self {
            root_dir,
            layer_stack_root,
            workspace_root,
        })
    }

    fn build_base(
        &self,
    ) -> Result<sandbox_runtime_layerstack::Manifest, Box<dyn std::error::Error + Send + Sync>>
    {
        sandbox_runtime_layerstack::build_workspace_base(
            &self.layer_stack_root,
            &self.workspace_root,
            false,
        )?;
        self.active_manifest()
    }

    fn active_manifest(
        &self,
    ) -> Result<sandbox_runtime_layerstack::Manifest, Box<dyn std::error::Error + Send + Sync>>
    {
        Ok(
            sandbox_runtime_layerstack::LayerStack::open(self.layer_stack_root.clone())?
                .read_active_manifest()?,
        )
    }

    fn layerstack(
        &self,
        file: Arc<FileService>,
    ) -> Result<Arc<LayerStackService>, Box<dyn std::error::Error + Send + Sync>> {
        self.layerstack_with(
            file,
            Observer::disabled(),
            LayerstackRuntimeConfig::default(),
        )
    }

    fn layerstack_with(
        &self,
        file: Arc<FileService>,
        observer: Observer,
        config: LayerstackRuntimeConfig,
    ) -> Result<Arc<LayerStackService>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Arc::new(LayerStackService::new(
            self.layer_stack_root.clone(),
            self.root_dir.join("scratch"),
            config,
            observer,
            file,
        )?))
    }

    fn read_text(
        &self,
        path: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
        let manifest = self.active_manifest()?;
        let view = sandbox_runtime_layerstack::MergedView::new(self.layer_stack_root.clone());
        let (bytes, exists) = view.read_bytes(path, &manifest)?;
        Ok(if exists {
            Some(String::from_utf8(bytes.expect("existing path has bytes"))?)
        } else {
            None
        })
    }
}

impl Drop for PublishFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root_dir);
    }
}

struct TraceLog {
    root: PathBuf,
    path: PathBuf,
}

impl TraceLog {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "workspace-session-publish-trace-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create trace directory");
        Self {
            path: root.join("observability.ndjson"),
            root,
        }
    }

    fn observer(&self) -> Observer {
        Observer::new(
            ObserverConfig {
                proc: proc::DAEMON,
                enabled: true,
            },
            Sink::new(
                self.path.clone(),
                sandbox_observability_telemetry::MAX_LINE_BYTES,
            ),
        )
    }

    fn records(&self) -> Vec<Record> {
        Reader::new(self.path.clone(), self.path.with_extension("absent"))
            .raw(RawFilter::default())
            .into_iter()
            .map(|line| serde_json::from_str(&line).expect("valid observability record"))
            .collect()
    }
}

impl Drop for TraceLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn layer_path(path: &str) -> sandbox_runtime_layerstack::LayerPath {
    sandbox_runtime_layerstack::LayerPath::parse(path).expect("valid layer path")
}

fn write_change(path: &str, content: &str) -> sandbox_runtime_layerstack::LayerChange {
    sandbox_runtime_layerstack::LayerChange::Write {
        path: layer_path(path),
        content: content.as_bytes().to_vec(),
    }
}

fn direct_publish(
    fixture: &PublishFixture,
    service: &LayerStackService,
    publication_id: [u8; 16],
    path: &str,
    content: &str,
) -> Result<
    sandbox_runtime::layerstack::PublishChangesResult,
    Box<dyn std::error::Error + Send + Sync>,
> {
    let base = fixture.active_manifest()?;
    Ok(
        service.publish_changes(sandbox_runtime::layerstack::PublishChangesRequest {
            publication_id,
            expected_base: sandbox_runtime::layerstack::LayerStackRevision {
                manifest_version: base.version,
                root_hash: sandbox_runtime_layerstack::manifest_root_hash(&base),
                layer_count: base.layers.len(),
            },
            base_manifest: base,
            protected_drops: Vec::new(),
            changes: vec![write_change(path, content)],
            owner: format!("operation:hidden-validation-{path}"),
        })?,
    )
}

fn session_handle(
    id: &str,
    manifest: sandbox_runtime_layerstack::Manifest,
    layer_stack_root: &Path,
) -> WorkspaceHandle {
    let snapshot = LayerStackSnapshotRef {
        lease_id: LeaseId(format!("lease-{id}")),
        manifest_version: manifest.version,
        root_hash: sandbox_runtime_layerstack::manifest_root_hash(&manifest),
        layer_paths: manifest
            .layers
            .iter()
            .map(|layer| layer_stack_root.join(&layer.path))
            .collect(),
        manifest,
    };
    let mount_root = std::env::temp_dir().join(format!(
        "workspace-session-publish-mount-{id}-{}",
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ));
    WorkspaceHandle::holder_backed_for_test(
        WorkspaceSessionId(id.to_owned()),
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
        snapshot,
        mount_root.join("upper"),
        mount_root.join("work"),
    )
}

fn captured(
    handle: &WorkspaceHandle,
    base_manifest: sandbox_runtime_layerstack::Manifest,
    changes: Vec<sandbox_runtime_layerstack::LayerChange>,
) -> CapturedWorkspaceChanges {
    let changed_paths = changes
        .iter()
        .map(|change| change.path().to_string())
        .collect::<Vec<_>>();
    CapturedWorkspaceChanges {
        workspace_session_id: handle.id.clone(),
        base_revision: handle.base_revision(),
        base_manifest,
        changed_path_kinds: Default::default(),
        protected_drops: Vec::new(),
        stats: None,
        metadata_path_count: changed_paths.len(),
        changed_paths,
        changes,
    }
}

fn operations(
    fake: Arc<FakeWorkspaceService>,
    launch_driver: Arc<FakeLaunchDriver>,
    layerstack: Arc<LayerStackService>,
    file: Arc<FileService>,
) -> SandboxRuntimeOperations {
    let services = build_services_with_launch_driver_and_layerstack(
        fake,
        launch_driver,
        Arc::clone(&layerstack),
    );
    SandboxRuntimeOperations::new(services.command, services.workspace, layerstack, file)
}

fn observed_operations(
    fake: Arc<FakeWorkspaceService>,
    layerstack: Arc<LayerStackService>,
    file: Arc<FileService>,
    observer: Observer,
) -> SandboxRuntimeOperations {
    let workspace = Arc::new(WorkspaceSessionService::new(
        support::fake_workspace_runtime(fake),
        Arc::clone(&layerstack),
        observer,
    ));
    let command = Arc::new(support::build_command_service(
        &workspace,
        &FakeLaunchDriver::new(),
    ));
    SandboxRuntimeOperations::new(command, workspace, layerstack, file)
}

fn request(args: Value) -> OperationRequest {
    OperationRequest::new(
        "publish_workspace_session",
        "req-publish",
        OperationScope::sandbox("sandbox-publish"),
        args,
    )
}

fn publish_json(operations: &SandboxRuntimeOperations, args: Value) -> Value {
    sandbox_runtime::dispatch_operation(operations, &request(args)).into_json_value()
}

fn dispatch_json(operations: &SandboxRuntimeOperations, op: &str, args: Value) -> Value {
    sandbox_runtime::dispatch_operation(
        operations,
        &OperationRequest::new(
            op,
            format!("req-{op}"),
            OperationScope::sandbox("sandbox-publish"),
            args,
        ),
    )
    .into_json_value()
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "condition timed out after {timeout:?}"
        );
        std::thread::yield_now();
    }
}

fn exec_input(workspace_session_id: &WorkspaceSessionId, yield_time_ms: u64) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: Some(workspace_session_id.clone()),
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(yield_time_ms),
    }
}

#[test]
fn test_s03_s02_validation_uses_normal_protocol_without_changing_v1_authority(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("s03-s02-hidden-validation")?;
    std::fs::write(fixture.workspace_root.join("README.md"), "base\n")?;
    fixture.build_base()?;
    let file = support::test_file_service();
    let validation_config = LayerstackRuntimeConfig {
        rollout_mode: sandbox_runtime::StorageRolloutMode::Validation,
        ..LayerstackRuntimeConfig::default()
    };
    let validation =
        fixture.layerstack_with(Arc::clone(&file), Observer::disabled(), validation_config)?;

    let initial = validation.observe()?;
    assert_eq!(
        initial.route.configured_mode,
        sandbox_runtime::StorageRolloutMode::Validation
    );
    assert_eq!(
        initial.route.write_authority,
        sandbox_runtime::StorageAuthority::LegacyV1
    );
    assert_eq!(
        initial.route.read_authority,
        sandbox_runtime::StorageAuthority::LegacyV1
    );

    let first = direct_publish(
        &fixture,
        &validation,
        [0x21; 16],
        "matched.txt",
        "matched\n",
    )?;
    assert!(!first.no_op);
    assert_eq!(
        fixture.read_text("matched.txt")?.as_deref(),
        Some("matched\n")
    );
    wait_until(Duration::from_secs(10), || {
        validation
            .observe()
            .is_ok_and(|snapshot| snapshot.route.shadow_completed_count >= 1)
    });
    let first_observation = validation.observe()?;
    assert_eq!(first_observation.route.shadow_comparison_count, 1);
    assert_eq!(first_observation.route.mismatch_count, 0);
    assert_eq!(
        validation.hidden_validation_last_correlation().as_deref(),
        Some("21212121212121212121212121212121")
    );

    validation.force_next_hidden_validation_mismatch_for_tests();
    direct_publish(
        &fixture,
        &validation,
        [0x22; 16],
        "mismatch.txt",
        "public remains authoritative\n",
    )?;
    wait_until(Duration::from_secs(10), || {
        validation.observe().is_ok_and(|snapshot| {
            snapshot.route.shadow_completed_count >= 2 && snapshot.route.mismatch_count == 1
        })
    });
    assert_eq!(
        fixture.read_text("mismatch.txt")?.as_deref(),
        Some("public remains authoritative\n")
    );

    validation.pause_hidden_validation_for_tests(true);
    direct_publish(&fixture, &validation, [0x30; 16], "paused.txt", "paused\n")?;
    wait_until(Duration::from_secs(10), || {
        validation
            .observe()
            .is_ok_and(|snapshot| snapshot.resources.active_tasks == 1)
    });
    direct_publish(&fixture, &validation, [0x31; 16], "queued-a.txt", "a\n")?;
    direct_publish(&fixture, &validation, [0x32; 16], "queued-b.txt", "b\n")?;
    let fallback_before = validation.observe()?.route.fallback_count;
    let started = Instant::now();
    direct_publish(
        &fixture,
        &validation,
        [0x33; 16],
        "backpressured.txt",
        "v1 still commits\n",
    )?;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "hidden queue saturation must not block the public v1 publish"
    );
    let saturated = validation.observe()?;
    assert!(saturated.route.fallback_count > fallback_before);
    assert_eq!(saturated.resources.active_tasks, 1);
    assert_eq!(saturated.resources.queued_items, 2);
    assert!(saturated.resources.queued_bytes > 0);
    assert_eq!(
        fixture.read_text("backpressured.txt")?.as_deref(),
        Some("v1 still commits\n")
    );
    validation.pause_hidden_validation_for_tests(false);
    wait_until(Duration::from_secs(10), || {
        validation.observe().is_ok_and(|snapshot| {
            snapshot.route.shadow_completed_count >= 5
                && snapshot.resources.active_tasks == 0
                && snapshot.resources.queued_items == 0
                && snapshot.resources.queued_bytes == 0
                && snapshot.resources.byte_permits_in_use == 0
        })
    });

    let diagnostic =
        sandbox_runtime_layerstack::LayerStack::open(fixture.layer_stack_root.clone())?;
    let generation_before_off = diagnostic
        .hidden_validation_generation()?
        .expect("validation head exists");
    let comparisons_before_off = validation.observe()?.route.shadow_comparison_count;
    drop(validation);
    let stopped = diagnostic.observe()?;
    assert_eq!(stopped.resources.active_workers, 0);
    assert!(stopped.resources.logical_cleanup_complete);

    let legacy = fixture.layerstack(Arc::clone(&file))?;
    direct_publish(
        &fixture,
        &legacy,
        [0x40; 16],
        "validation-off.txt",
        "legacy only\n",
    )?;
    let off = legacy.observe()?;
    assert_eq!(
        off.route.configured_mode,
        sandbox_runtime::StorageRolloutMode::Legacy
    );
    assert_eq!(off.route.shadow_comparison_count, comparisons_before_off);
    assert_eq!(
        diagnostic.hidden_validation_generation()?,
        Some(generation_before_off)
    );
    drop(legacy);

    let restarted =
        fixture.layerstack_with(Arc::clone(&file), Observer::disabled(), validation_config)?;
    direct_publish(
        &fixture,
        &restarted,
        [0x41; 16],
        "validation-restarted.txt",
        "restart\n",
    )?;
    wait_until(Duration::from_secs(10), || {
        restarted
            .observe()
            .is_ok_and(|snapshot| snapshot.route.shadow_completed_count > comparisons_before_off)
    });
    assert!(diagnostic
        .hidden_validation_generation()?
        .is_some_and(|generation| generation > generation_before_off));
    assert_eq!(
        restarted.observe()?.route.write_authority,
        sandbox_runtime::StorageAuthority::LegacyV1
    );
    drop(restarted);
    assert!(diagnostic.observe()?.resources.logical_cleanup_complete);
    Ok(())
}

#[test]
fn hidden_validation_accepts_directory_node_attribution(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("hidden-validation-directory-attribution")?;
    std::fs::write(fixture.workspace_root.join("README.md"), "base\n")?;
    fixture.build_base()?;
    let validation = fixture.layerstack_with(
        support::test_file_service(),
        Observer::disabled(),
        LayerstackRuntimeConfig {
            rollout_mode: sandbox_runtime::StorageRolloutMode::Validation,
            ..LayerstackRuntimeConfig::default()
        },
    )?;
    let base = fixture.active_manifest()?;

    validation.publish_changes(sandbox_runtime::layerstack::PublishChangesRequest {
        publication_id: [0x51; 16],
        expected_base: sandbox_runtime::layerstack::LayerStackRevision {
            manifest_version: base.version,
            root_hash: sandbox_runtime_layerstack::manifest_root_hash(&base),
            layer_count: base.layers.len(),
        },
        base_manifest: base,
        protected_drops: Vec::new(),
        changes: vec![
            sandbox_runtime_layerstack::LayerChange::Directory {
                path: layer_path("import"),
            },
            write_change("import/file.bin", "payload\n"),
        ],
        owner: "operation:hidden-validation-directory-attribution".to_string(),
    })?;
    wait_until(Duration::from_secs(10), || {
        validation.observe().is_ok_and(|snapshot| {
            snapshot.route.shadow_completed_count >= 1 || snapshot.route.fallback_count >= 1
        })
    });

    let observation = validation.observe()?;
    assert_eq!(observation.route.shadow_completed_count, 1);
    assert_eq!(observation.route.shadow_comparison_count, 1);
    assert_eq!(observation.route.fallback_count, 0);
    assert_eq!(observation.route.mismatch_count, 0);
    assert_eq!(observation.resources.active_tasks, 0);
    assert_eq!(observation.resources.queued_items, 0);
    assert_eq!(observation.resources.byte_permits_in_use, 0);
    assert_eq!(observation.resources.active_leases, 0);
    Ok(())
}

#[test]
fn hidden_validation_idle_worker_is_quiescent(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("hidden-validation-idle-worker")?;
    std::fs::write(fixture.workspace_root.join("README.md"), "base\n")?;
    fixture.build_base()?;
    let validation = fixture.layerstack_with(
        support::test_file_service(),
        Observer::disabled(),
        LayerstackRuntimeConfig {
            rollout_mode: sandbox_runtime::StorageRolloutMode::Validation,
            ..LayerstackRuntimeConfig::default()
        },
    )?;

    let observation = validation.observe()?;
    assert_eq!(observation.resources.active_workers, 0);
    assert_eq!(observation.resources.high_water_active_workers, 0);
    assert!(observation.resources.logical_cleanup_complete);
    Ok(())
}

#[test]
fn explicit_publish_commits_one_layer_audits_owner_closes_and_projects_only_public_fields(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("success")?;
    std::fs::write(fixture.workspace_root.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let base_layer_count = base.layers.len();
    let handle = session_handle("ws-success", base.clone(), &fixture.layer_stack_root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(captured(
        &handle,
        base,
        vec![write_change("README.md", "published\n")],
    )));
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(
        Arc::clone(&fake),
        Arc::new(FakeLaunchDriver::new()),
        layerstack,
        Arc::clone(&file),
    );
    operations
        .workspace_session
        .create_workspace_session(create_request())?;

    let response = publish_json(
        &operations,
        json!({"workspace_session_id": "ws-success", "grace_s": 1.25}),
    );
    let active = fixture.active_manifest()?;

    assert_eq!(
        response,
        json!({
            "workspace_session_id": "ws-success",
            "publish": {
                "no_op": false,
                "revision": {
                    "manifest_version": active.version,
                    "root_hash": sandbox_runtime_layerstack::manifest_root_hash(&active),
                    "layer_count": active.layers.len(),
                },
                "route_summary": {"source_count": 1, "ignored_count": 0},
            },
            "destroyed": true,
            "evicted_upperdir_bytes": 4096,
        })
    );
    assert_eq!(active.layers.len(), base_layer_count + 1);
    assert_eq!(
        fixture.read_text("README.md")?,
        Some("published\n".to_owned())
    );
    assert!(matches!(
        operations
            .workspace_session
            .resolve_session(WorkspaceSessionId("ws-success".to_owned())),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert_eq!(fake.capture_calls().len(), 1);
    assert_eq!(fake.destroy_calls().len(), 1);
    let blame = file.blame("README.md")?;
    assert!(blame
        .iter()
        .all(|range| range.owner == "workspace_session:ws-success"));
    let serialized = serde_json::to_string(&response)?;
    for forbidden in [
        "\"manifest\":",
        "\"layer_paths\":",
        "\"workspace_root\":",
        "\"upperdir\":",
        "\"workdir\":",
        fixture.root_dir.to_string_lossy().as_ref(),
    ] {
        assert!(
            !serialized.contains(forbidden),
            "leaked field or path: {forbidden}"
        );
    }
    Ok(())
}

#[test]
fn explicit_publish_rejects_active_commands_before_capture(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("active-command")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-active", base, &fixture.layer_stack_root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let launcher = launch_driver.launcher();
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(Arc::clone(&fake), launch_driver, layerstack, file);
    let workspace_session_id = operations
        .workspace_session
        .create_workspace_session(create_request())?
        .workspace_session_id;
    let running = operations
        .command
        .exec_command(exec_input(&workspace_session_id, 0))?;
    assert_eq!(running.status, CommandStatus::Running);
    let command_session_id = running
        .command_session_id
        .expect("running command has an id");

    let response = publish_json(
        &operations,
        json!({"workspace_session_id": workspace_session_id.0}),
    );

    assert_eq!(response["error"]["kind"], "operation_failed");
    assert_eq!(
        response["error"]["message"],
        "workspace session has active command sessions"
    );
    assert_eq!(
        response["error"]["details"]["active_command_session_ids"],
        json!([command_session_id.0.clone()])
    );
    assert!(fake.capture_calls().is_empty());
    assert!(fake.destroy_calls().is_empty());
    assert_eq!(
        operations.observability_snapshot().workspaces[0].finalization_state,
        FinalizationState::Active
    );

    launcher.complete_request(
        &command_session_id.0,
        sandbox_runtime_namespace_process::runner::protocol::RunResult {
            exit_code: 0,
            payload: json!({"status": "ok"}),
        },
    );
    let cleanup_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match operations
            .workspace_session
            .guarded_destroy(workspace_session_id.clone(), None)
        {
            Ok(_) => break,
            Err(WorkspaceSessionError::ActiveCommands { .. }) => {
                assert!(
                    Instant::now() < cleanup_deadline,
                    "workspace command ledger did not drain before cleanup"
                );
                std::thread::yield_now();
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[test]
fn capture_failure_restores_active_retains_session_and_allows_command_then_retry(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("capture-retry")?;
    std::fs::write(fixture.workspace_root.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-capture", base.clone(), &fixture.layer_stack_root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Err(WorkspaceError::Capture {
        message: format!("could not scan {}", fixture.root_dir.display()),
    }));
    fake.push_capture_result(Ok(captured(
        &handle,
        base.clone(),
        vec![write_change("README.md", "retry\n")],
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "still active\n",
    )));
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(Arc::clone(&fake), launch_driver, layerstack, file);
    let workspace_session_id = operations
        .workspace_session
        .create_workspace_session(create_request())?
        .workspace_session_id;

    let rejected = publish_json(
        &operations,
        json!({"workspace_session_id": workspace_session_id.0.clone()}),
    );

    assert_eq!(
        rejected,
        json!({
            "error": {
                "kind": "operation_failed",
                "message": "workspace session publish was rejected",
                "details": {
                    "workspace_session_id": "ws-capture",
                    "stage": "capture",
                    "session_retained": true,
                },
            },
        })
    );
    assert!(
        !serde_json::to_string(&rejected)?.contains(fixture.root_dir.to_string_lossy().as_ref())
    );
    assert_eq!(fixture.active_manifest()?, base);
    assert_eq!(
        operations.observability_snapshot().workspaces[0].finalization_state,
        FinalizationState::Active
    );
    operations
        .workspace_session
        .with_gated_session(&workspace_session_id, |_| ())?;
    let command = operations
        .command
        .exec_command(exec_input(&workspace_session_id, 250))?;
    assert_eq!(command.status, CommandStatus::Ok);

    let retried = publish_json(
        &operations,
        json!({"workspace_session_id": workspace_session_id.0}),
    );
    assert_eq!(retried["publish"]["no_op"], false);
    assert_eq!(retried["destroyed"], true);
    assert_eq!(fixture.read_text("README.md")?, Some("retry\n".to_owned()));
    assert_eq!(fake.capture_calls().len(), 2);
    assert_eq!(fake.destroy_calls().len(), 1);
    Ok(())
}

#[test]
fn command_queued_behind_failed_publish_runs_only_after_session_returns_active(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("failed-publish-command-gate")?;
    let base = fixture.build_base()?;
    let handle = session_handle(
        "ws-failed-publish-command",
        base.clone(),
        &fixture.layer_stack_root,
    );
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle));
    fake.push_capture_result(Err(WorkspaceError::Capture {
        message: "injected capture failure".to_owned(),
    }));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "ran after restore\n",
    )));
    let launch_probe = Arc::clone(&launch_driver);
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = Arc::new(operations(
        Arc::clone(&fake),
        launch_driver,
        layerstack,
        file,
    ));
    let workspace_session_id = operations
        .workspace_session
        .create_workspace_session(create_request())?
        .workspace_session_id;
    let (capture_entered, release_capture) = fake.park_next_capture();

    let publisher = {
        let operations = Arc::clone(&operations);
        let workspace_session_id = workspace_session_id.clone();
        std::thread::spawn(move || {
            operations
                .workspace_session
                .publish_workspace_session(workspace_session_id, None)
        })
    };
    capture_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("publish holds the admission gate inside capture");

    let (command_started_tx, command_started_rx) = mpsc::channel();
    let commander = {
        let operations = Arc::clone(&operations);
        let workspace_session_id = workspace_session_id.clone();
        std::thread::spawn(move || {
            command_started_tx.send(()).expect("signal command attempt");
            operations
                .command
                .exec_command(exec_input(&workspace_session_id, 250))
        })
    };
    command_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("command thread reached admission");
    assert!(launch_probe.recorded_requests().is_empty());
    assert_eq!(
        operations.observability_snapshot().workspaces[0].finalization_state,
        FinalizationState::Finalizing,
    );

    release_capture.send(())?;
    assert!(matches!(
        publisher.join().expect("publish thread does not panic"),
        Err(WorkspaceSessionError::PublishRetained { .. })
    ));
    let command = commander.join().expect("command thread does not panic")?;
    assert_eq!(command.status, CommandStatus::Ok);
    assert_eq!(launch_probe.recorded_requests().len(), 1);
    assert_eq!(
        operations.observability_snapshot().workspaces[0].finalization_state,
        FinalizationState::Active,
    );
    assert_eq!(fake.capture_calls().len(), 1);
    assert!(fake.destroy_calls().is_empty());
    assert_eq!(fixture.active_manifest()?, base);
    Ok(())
}

#[test]
fn every_capture_drop_blocks_the_whole_explicit_publish_and_retry_preserves_safe_changes(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("capture-drop")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-drop", base.clone(), &fixture.layer_stack_root);
    let mut first_capture = captured(
        &handle,
        base.clone(),
        vec![write_change("safe.txt", "retained\n")],
    );
    first_capture.protected_drops.push(ProtectedPathDrop {
        path: "device-node".to_owned(),
        reason: ProtectedPathDropReason::UnsupportedSpecialFile,
    });
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(first_capture));
    fake.push_capture_result(Ok(captured(
        &handle,
        base.clone(),
        vec![write_change("safe.txt", "retained\n")],
    )));
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(
        Arc::clone(&fake),
        Arc::new(FakeLaunchDriver::new()),
        layerstack,
        file,
    );
    operations
        .workspace_session
        .create_workspace_session(create_request())?;

    let rejected = publish_json(&operations, json!({"workspace_session_id": "ws-drop"}));

    assert_eq!(rejected["error"]["details"]["stage"], "publish");
    assert_eq!(rejected["error"]["details"]["session_retained"], true);
    assert_eq!(
        rejected["error"]["details"]["publish_rejection"],
        json!({
            "path": null,
            "reason": "protected_path",
            "source_conflict": null,
            "protected_drop": {
                "path": "device-node",
                "reason": "unsupported_special_file",
            },
            "message": null,
        })
    );
    assert_eq!(fixture.active_manifest()?, base);
    assert_eq!(fixture.read_text("safe.txt")?, None);
    assert!(fake.destroy_calls().is_empty());
    assert_eq!(
        operations.observability_snapshot().workspaces[0].finalization_state,
        FinalizationState::Active
    );

    let retried = publish_json(&operations, json!({"workspace_session_id": "ws-drop"}));
    assert_eq!(retried["destroyed"], true);
    assert_eq!(
        fixture.read_text("safe.txt")?,
        Some("retained\n".to_owned())
    );
    Ok(())
}

#[test]
fn storage_failure_keeps_revision_and_session_then_retry_commits_once(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("storage-retry")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-storage", base.clone(), &fixture.layer_stack_root);
    let change = vec![write_change("retry.txt", "one commit\n")];
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(captured(&handle, base.clone(), change.clone())));
    fake.push_capture_result(Ok(captured(&handle, base.clone(), change)));
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(
        Arc::clone(&fake),
        Arc::new(FakeLaunchDriver::new()),
        layerstack,
        file,
    );
    operations
        .workspace_session
        .create_workspace_session(create_request())?;
    let marker_dir = fixture.layer_stack_root.join(".layer-metadata");
    std::fs::create_dir_all(&marker_dir)?;
    std::fs::write(marker_dir.join("fail-next-publish"), b"")?;
    std::env::set_var("SANDBOX_LAYERSTACK_ENABLE_TEST_FAILPOINTS", "1");
    let rejected = publish_json(&operations, json!({"workspace_session_id": "ws-storage"}));
    std::env::remove_var("SANDBOX_LAYERSTACK_ENABLE_TEST_FAILPOINTS");

    assert_eq!(rejected["error"]["details"]["stage"], "publish");
    assert_eq!(rejected["error"]["details"]["session_retained"], true);
    assert!(rejected["error"]["details"]
        .get("publish_rejection")
        .is_none());
    assert!(
        !serde_json::to_string(&rejected)?.contains(fixture.root_dir.to_string_lossy().as_ref())
    );
    assert_eq!(fixture.active_manifest()?, base);
    assert_eq!(fixture.read_text("retry.txt")?, None);
    assert!(fake.destroy_calls().is_empty());
    assert_eq!(
        operations.observability_snapshot().workspaces[0].finalization_state,
        FinalizationState::Active
    );

    let retried = publish_json(&operations, json!({"workspace_session_id": "ws-storage"}));
    assert_eq!(retried["publish"]["no_op"], false);
    assert_eq!(
        fixture.active_manifest()?.layers.len(),
        base.layers.len() + 1
    );
    assert_eq!(
        fixture.read_text("retry.txt")?,
        Some("one commit\n".to_owned())
    );
    assert_eq!(fake.destroy_calls().len(), 1);
    Ok(())
}

#[test]
fn merge_conflict_is_structured_atomic_and_retryable(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("merge-conflict")?;
    std::fs::write(fixture.workspace_root.join("notes.txt"), "base\n")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-conflict", base.clone(), &fixture.layer_stack_root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(captured(
        &handle,
        base.clone(),
        vec![write_change("notes.txt", "session\n")],
    )));
    fake.push_capture_result(Ok(captured(
        &handle,
        base.clone(),
        vec![write_change("retry.txt", "safe retry\n")],
    )));
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(
        Arc::clone(&fake),
        Arc::new(FakeLaunchDriver::new()),
        Arc::clone(&layerstack),
        file,
    );
    operations
        .workspace_session
        .create_workspace_session(create_request())?;
    layerstack.publish_changes(sandbox_runtime::layerstack::PublishChangesRequest {
        publication_id: [5; 16],
        expected_base: sandbox_runtime::layerstack::LayerStackRevision {
            manifest_version: base.version,
            root_hash: sandbox_runtime_layerstack::manifest_root_hash(&base),
            layer_count: base.layers.len(),
        },
        base_manifest: base.clone(),
        protected_drops: Vec::new(),
        changes: vec![write_change("notes.txt", "external\n")],
        owner: "operation:external".to_owned(),
    })?;
    let external_head = fixture.active_manifest()?;

    let rejected = publish_json(&operations, json!({"workspace_session_id": "ws-conflict"}));

    assert_eq!(
        rejected["error"]["details"]["publish_rejection"]["reason"],
        "source_conflict"
    );
    assert_eq!(
        rejected["error"]["details"]["publish_rejection"]["path"],
        "notes.txt"
    );
    assert_eq!(
        rejected["error"]["details"]["publish_rejection"]["source_conflict"]["path"],
        "notes.txt"
    );
    assert_eq!(
        rejected["error"]["details"]["publish_rejection"]["source_conflict"]["expected"]
            ["executable"],
        false
    );
    assert_eq!(
        rejected["error"]["details"]["publish_rejection"]["source_conflict"]["actual"]
            ["executable"],
        false
    );
    assert_eq!(fixture.active_manifest()?, external_head);
    assert_eq!(
        fixture.read_text("notes.txt")?,
        Some("external\n".to_owned())
    );
    assert!(fake.destroy_calls().is_empty());

    let retried = publish_json(&operations, json!({"workspace_session_id": "ws-conflict"}));
    assert_eq!(retried["publish"]["no_op"], false);
    assert_eq!(
        fixture.active_manifest()?.layers.len(),
        external_head.layers.len() + 1
    );
    assert_eq!(
        fixture.read_text("retry.txt")?,
        Some("safe retry\n".to_owned())
    );
    Ok(())
}

#[test]
fn committed_destroy_failure_is_partial_success_cleanup_only_and_never_duplicates_layer(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("partial-success")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-partial", base.clone(), &fixture.layer_stack_root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(captured(
        &handle,
        base.clone(),
        vec![write_change("durable.txt", "committed\n")],
    )));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: format!("teardown failed under {}", fixture.root_dir.display()),
    }));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let launch_probe = Arc::clone(&launch_driver);
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(Arc::clone(&fake), launch_driver, layerstack, file);
    let workspace_session_id = operations
        .workspace_session
        .create_workspace_session(create_request())?
        .workspace_session_id;

    let partial = publish_json(
        &operations,
        json!({"workspace_session_id": workspace_session_id.0.clone()}),
    );
    let committed_head = fixture.active_manifest()?;

    assert_eq!(partial["error"]["kind"], "operation_failed");
    assert_eq!(
        partial["error"]["message"],
        "workspace session published but could not be closed"
    );
    assert_eq!(
        partial["error"]["details"],
        json!({
            "workspace_session_id": "ws-partial",
            "stage": "destroy",
            "publish_completed": true,
            "layer_committed": true,
            "publish": {
                "no_op": false,
                "revision": {
                    "manifest_version": committed_head.version,
                    "root_hash": sandbox_runtime_layerstack::manifest_root_hash(&committed_head),
                    "layer_count": committed_head.layers.len(),
                },
                "route_summary": {"source_count": 1, "ignored_count": 0},
            },
            "destroyed": false,
            "session_state": "finalize_failed",
            "recovery_operation": "destroy_workspace_session",
        })
    );
    assert!(!serde_json::to_string(&partial)?.contains(fixture.root_dir.to_string_lossy().as_ref()));
    assert_eq!(committed_head.layers.len(), base.layers.len() + 1);
    assert_eq!(
        fixture.read_text("durable.txt")?,
        Some("committed\n".to_owned())
    );
    let snapshot = operations.observability_snapshot();
    assert_eq!(snapshot.workspaces.len(), 1);
    assert_eq!(
        snapshot.workspaces[0].finalization_state,
        FinalizationState::FinalizeFailed
    );
    assert!(matches!(
        operations
            .workspace_session
            .with_gated_session(&workspace_session_id, |_| ()),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert!(matches!(
        operations.workspace_session.run_file_op(
            &workspace_session_id,
            FileRunnerOp::ReadFile {
                rel: "durable.txt".to_owned(),
                max_bytes: 32,
            },
        ),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert!(operations
        .command
        .exec_command(exec_input(&workspace_session_id, 250))
        .is_err());
    assert!(launch_probe.recorded_requests().is_empty());

    let republish = publish_json(
        &operations,
        json!({"workspace_session_id": workspace_session_id.0.clone()}),
    );
    assert_eq!(republish["error"]["kind"], "operation_failed");
    assert_eq!(
        republish["error"]["details"]["workspace_session_id"],
        "ws-partial"
    );
    assert_eq!(fixture.active_manifest()?, committed_head);
    assert_eq!(fake.capture_calls().len(), 1);

    let cleanup = dispatch_json(
        &operations,
        "destroy_workspace_session",
        json!({"workspace_session_id": workspace_session_id.0}),
    );
    assert_eq!(cleanup["destroyed"], true);
    assert_eq!(fake.destroy_calls().len(), 2);
    assert!(operations.observability_snapshot().workspaces.is_empty());
    assert_eq!(fixture.active_manifest()?, committed_head);
    Ok(())
}

#[test]
fn holder_reconciliation_cannot_retry_post_publish_cleanup_failure(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("partial-success-dead-holder")?;
    let base = fixture.build_base()?;
    let handle = session_handle(
        "ws-partial-dead-holder",
        base.clone(),
        &fixture.layer_stack_root,
    );
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(captured(
        &handle,
        base,
        vec![write_change("durable.txt", "committed\n")],
    )));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "injected post-publish cleanup failure".to_owned(),
    }));
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(
        Arc::clone(&fake),
        Arc::new(FakeLaunchDriver::new()),
        layerstack,
        file,
    );
    operations
        .workspace_session
        .create_workspace_session(create_request())?;

    let partial = publish_json(
        &operations,
        json!({"workspace_session_id": "ws-partial-dead-holder"}),
    );
    assert_eq!(
        partial["error"]["details"]["session_state"],
        "finalize_failed"
    );
    assert_eq!(fake.destroy_calls().len(), 1);

    // Teardown may have already killed the holder before a later cleanup step
    // failed. Public snapshots and session enumeration must not turn that fact
    // into an implicit second destroy; only the guarded recovery operation may
    // retry a post-publish cleanup failure.
    fake.mark_holder_exited(&handle, "exit-status:0");
    for _ in 0..2 {
        let snapshot = operations.observability_snapshot();
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(
            snapshot.workspaces[0].finalization_state,
            FinalizationState::FinalizeFailed
        );
        assert_eq!(
            operations.workspace_session.session_ids(),
            vec![WorkspaceSessionId("ws-partial-dead-holder".to_owned())]
        );
        assert_eq!(fake.destroy_calls().len(), 1);
    }

    let cleanup = dispatch_json(
        &operations,
        "destroy_workspace_session",
        json!({"workspace_session_id": "ws-partial-dead-holder"}),
    );
    assert_eq!(cleanup["destroyed"], true);
    assert_eq!(fake.destroy_calls().len(), 2);
    assert!(operations.observability_snapshot().workspaces.is_empty());
    Ok(())
}

#[test]
fn no_op_destroy_failure_reports_no_layer_commit_and_allows_guarded_cleanup(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("no-op-partial")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-no-op-partial", base.clone(), &fixture.layer_stack_root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(captured(&handle, base.clone(), Vec::new())));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "injected teardown failure".to_owned(),
    }));
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(
        Arc::clone(&fake),
        Arc::new(FakeLaunchDriver::new()),
        layerstack,
        file,
    );
    operations
        .workspace_session
        .create_workspace_session(create_request())?;

    let partial = publish_json(
        &operations,
        json!({"workspace_session_id": "ws-no-op-partial"}),
    );

    assert_eq!(partial["error"]["details"]["publish_completed"], true);
    assert_eq!(partial["error"]["details"]["layer_committed"], false);
    assert_eq!(partial["error"]["details"]["publish"]["no_op"], true);
    assert_eq!(
        partial["error"]["details"]["session_state"],
        "finalize_failed"
    );
    assert_eq!(fixture.active_manifest()?, base);
    let cleanup = dispatch_json(
        &operations,
        "destroy_workspace_session",
        json!({"workspace_session_id": "ws-no-op-partial"}),
    );
    assert_eq!(cleanup["destroyed"], true);
    assert_eq!(fixture.active_manifest()?, base);
    assert_eq!(fake.destroy_calls().len(), 2);
    Ok(())
}

#[test]
fn publish_serializes_with_destroy_and_command_admission_under_one_gate(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("gate-races")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-race", base.clone(), &fixture.layer_stack_root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(captured(
        &handle,
        base.clone(),
        vec![write_change("winner.txt", "publish wins\n")],
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "must not launch\n",
    )));
    let launch_probe = Arc::clone(&launch_driver);
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = Arc::new(operations(
        Arc::clone(&fake),
        launch_driver,
        layerstack,
        file,
    ));
    let workspace_session_id = operations
        .workspace_session
        .create_workspace_session(create_request())?
        .workspace_session_id;
    let (capture_entered, release_capture) = fake.park_next_capture();

    let publisher = {
        let operations = Arc::clone(&operations);
        let workspace_session_id = workspace_session_id.clone();
        std::thread::spawn(move || {
            operations
                .workspace_session
                .publish_workspace_session(workspace_session_id, None)
        })
    };
    capture_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("publish reached capture while holding the session gate");

    let (destroy_started_tx, destroy_started_rx) = mpsc::channel();
    let destroyer = {
        let operations = Arc::clone(&operations);
        let workspace_session_id = workspace_session_id.clone();
        std::thread::spawn(move || {
            destroy_started_tx.send(()).expect("signal destroy attempt");
            operations
                .workspace_session
                .guarded_destroy(workspace_session_id, None)
        })
    };
    destroy_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("destroy thread started");

    let (command_started_tx, command_started_rx) = mpsc::channel();
    let commander = {
        let operations = Arc::clone(&operations);
        let workspace_session_id = workspace_session_id.clone();
        std::thread::spawn(move || {
            command_started_tx.send(()).expect("signal command attempt");
            operations
                .command
                .exec_command(exec_input(&workspace_session_id, 250))
        })
    };
    command_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("command thread started");
    assert!(fake.destroy_calls().is_empty());
    assert!(launch_probe.recorded_requests().is_empty());

    release_capture.send(())?;
    let published = publisher.join().expect("publish thread does not panic")?;
    assert!(!published.publish.no_op);
    assert!(matches!(
        destroyer.join().expect("destroy thread does not panic"),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert!(commander
        .join()
        .expect("command thread does not panic")
        .is_err());
    assert!(launch_probe.recorded_requests().is_empty());
    assert_eq!(fake.capture_calls().len(), 1);
    assert_eq!(fake.destroy_calls().len(), 1);
    assert_eq!(
        fixture.active_manifest()?.layers.len(),
        base.layers.len() + 1
    );
    assert_eq!(
        fixture.read_text("winner.txt")?,
        Some("publish wins\n".to_owned())
    );
    Ok(())
}

#[test]
fn publish_input_validation_and_unknown_session_have_no_side_effects(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("input-validation")?;
    let base = fixture.build_base()?;
    let fake = Arc::new(FakeWorkspaceService::new());
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(
        Arc::clone(&fake),
        Arc::new(FakeLaunchDriver::new()),
        layerstack,
        file,
    );

    for args in [
        json!({}),
        json!({"workspace_session_id": ""}),
        json!({"workspace_session_id": 7}),
        json!({"workspace_session_id": "missing", "grace_s": -0.1}),
        json!({"workspace_session_id": "missing", "grace_s": "soon"}),
    ] {
        let response = publish_json(&operations, args);
        assert_eq!(response["error"]["kind"], "invalid_request");
    }
    let unknown = publish_json(&operations, json!({"workspace_session_id": "missing"}));
    assert_eq!(unknown["error"]["kind"], "operation_failed");
    assert_eq!(
        unknown["error"]["details"]["workspace_session_id"],
        "missing"
    );
    assert!(fake.capture_calls().is_empty());
    assert!(fake.destroy_calls().is_empty());
    assert_eq!(fixture.active_manifest()?, base);
    Ok(())
}

#[test]
fn publish_observability_has_nested_stages_and_partial_errors_do_not_leak_host_paths(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("observability-security")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-observed", base.clone(), &fixture.layer_stack_root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(captured(
        &handle,
        base,
        vec![write_change("observed.txt", "not logged\n")],
    )));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: format!("private mount under {}", fixture.root_dir.display()),
    }));
    let trace = TraceLog::new("stages");
    let observer = trace.observer();
    let file = support::test_file_service();
    let layerstack = fixture.layerstack_with(
        Arc::clone(&file),
        observer.clone(),
        LayerstackRuntimeConfig::default(),
    )?;
    let operations = observed_operations(Arc::clone(&fake), layerstack, file, observer.clone());
    operations
        .workspace_session
        .create_workspace_session(create_request())?;

    let partial = observer.with_context(
        TraceContext {
            trace: Arc::from("req-publish-observed"),
            parent: None,
        },
        || publish_json(&operations, json!({"workspace_session_id": "ws-observed"})),
    );
    assert_eq!(
        partial["error"]["details"]["session_state"],
        "finalize_failed"
    );

    let records = trace.records();
    for expected in [
        names::WORKSPACE_SESSION_PUBLISH,
        names::WORKSPACE_SESSION_CAPTURE_CHANGES,
        names::LAYERSTACK_PUBLISH,
        names::WORKSPACE_SESSION_DESTROY,
    ] {
        assert!(
            records
                .iter()
                .any(|record| { matches!(record, Record::Span(span) if span.name == expected) }),
            "missing span {expected}"
        );
    }
    let publish_span = records
        .iter()
        .find_map(|record| match record {
            Record::Span(span) if span.name == names::WORKSPACE_SESSION_PUBLISH => Some(span),
            _ => None,
        })
        .expect("publish span exists");
    assert_eq!(
        publish_span.attrs.get("workspace_session_id"),
        Some(&json!("ws-observed"))
    );
    assert_eq!(publish_span.attrs.get("committed"), Some(&json!(true)));
    assert_eq!(publish_span.attrs.get("no_op"), Some(&json!(false)));
    assert_eq!(publish_span.attrs.get("destroyed"), Some(&json!(false)));
    assert_eq!(
        publish_span.attrs.get("cleanup_outcome"),
        Some(&json!("finalize_failed"))
    );
    let serialized_records = records
        .iter()
        .map(|record| serde_json::to_string(record).expect("serialize record"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!serialized_records.contains(fixture.root_dir.to_string_lossy().as_ref()));
    assert!(!serialized_records.contains("not logged"));
    Ok(())
}

#[test]
fn autosquash_notifies_once_after_failed_commit_cleanup_and_never_for_no_op_or_rejection(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("autosquash-order")?;
    let base = fixture.build_base()?;
    let no_op_handle = session_handle(
        "ws-autosquash-no-op",
        base.clone(),
        &fixture.layer_stack_root,
    );
    let rejected_handle = session_handle(
        "ws-autosquash-rejected",
        base.clone(),
        &fixture.layer_stack_root,
    );
    let committed_handle = session_handle(
        "ws-autosquash-committed",
        base.clone(),
        &fixture.layer_stack_root,
    );
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(no_op_handle.clone()));
    fake.push_create_result(Ok(rejected_handle.clone()));
    fake.push_create_result(Ok(committed_handle.clone()));
    fake.push_capture_result(Ok(captured(&no_op_handle, base.clone(), Vec::new())));
    let mut rejected_capture = captured(
        &rejected_handle,
        base.clone(),
        vec![write_change("retained.txt", "must remain private\n")],
    );
    rejected_capture.protected_drops.push(ProtectedPathDrop {
        path: "private.sock".to_owned(),
        reason: ProtectedPathDropReason::UnsupportedSpecialFile,
    });
    fake.push_capture_result(Ok(rejected_capture));
    fake.push_capture_result(Ok(captured(
        &committed_handle,
        base,
        vec![write_change("layer.txt", "committed\n")],
    )));
    fake.push_destroy_result(Ok(support::destroy_result(&no_op_handle)));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "injected destroy failure".to_owned(),
    }));
    let trace = TraceLog::new("autosquash");
    let observer = trace.observer();
    let file = support::test_file_service();
    let layerstack = fixture.layerstack_with(
        Arc::clone(&file),
        observer.clone(),
        LayerstackRuntimeConfig {
            autosquash_squash_at_n_layers: Some(usize::MAX),
            ..LayerstackRuntimeConfig::default()
        },
    )?;
    let operations = Arc::new(observed_operations(
        Arc::clone(&fake),
        layerstack,
        file,
        observer,
    ));
    let no_op_session = operations
        .workspace_session
        .create_workspace_session(create_request())?
        .workspace_session_id;
    wait_until(Duration::from_secs(5), || {
        trace.records().iter().any(|record| {
            matches!(record, Record::Span(span)
                if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                    && span.attrs.get("trigger_reason") == Some(&json!("startup")))
        })
    });
    let no_op = operations
        .workspace_session
        .publish_workspace_session(no_op_session, None)?;
    assert!(no_op.publish.no_op);

    let rejected_session = operations
        .workspace_session
        .create_workspace_session(create_request())?
        .workspace_session_id;
    assert!(matches!(
        operations
            .workspace_session
            .publish_workspace_session(rejected_session, None),
        Err(WorkspaceSessionError::PublishRetained { .. })
    ));
    assert!(trace.records().iter().all(|record| {
        !matches!(record, Record::Span(span)
            if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                && span.attrs.get("trigger_reason") == Some(&json!("layer_committed")))
    }));

    let committed_session = operations
        .workspace_session
        .create_workspace_session(create_request())?
        .workspace_session_id;
    let (destroy_entered, release_destroy) = fake.park_next_destroy();
    let publisher = {
        let operations = Arc::clone(&operations);
        std::thread::spawn(move || {
            operations
                .workspace_session
                .publish_workspace_session(committed_session, None)
        })
    };
    destroy_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("publish reached destroy attempt");
    assert!(trace.records().iter().all(|record| {
        !matches!(record, Record::Span(span)
            if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                && span.attrs.get("trigger_reason") == Some(&json!("layer_committed")))
    }));

    release_destroy.send(())?;
    assert!(matches!(
        publisher.join().expect("publish thread does not panic"),
        Err(WorkspaceSessionError::PublishedButNotClosed { .. })
    ));
    wait_until(Duration::from_secs(5), || {
        trace.records().iter().any(|record| {
            matches!(record, Record::Span(span)
                if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                    && span.attrs.get("trigger_reason") == Some(&json!("layer_committed")))
        })
    });
    let committed_evaluations = trace
        .records()
        .into_iter()
        .filter_map(|record| match record {
            Record::Span(span)
                if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                    && span.attrs.get("trigger_reason") == Some(&json!("layer_committed")) =>
            {
                Some(
                    1 + span
                        .attrs
                        .get("coalesced_notifications")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                )
            }
            _ => None,
        })
        .sum::<u64>();
    assert_eq!(committed_evaluations, 1);
    Ok(())
}

#[test]
fn explicit_empty_publish_returns_current_revision_and_closes_without_a_layer(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("no-op")?;
    std::fs::write(fixture.workspace_root.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let handle = session_handle("ws-no-op", base.clone(), &fixture.layer_stack_root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(captured(&handle, base.clone(), Vec::new())));
    let file = support::test_file_service();
    let layerstack = fixture.layerstack(Arc::clone(&file))?;
    let operations = operations(
        Arc::clone(&fake),
        Arc::new(FakeLaunchDriver::new()),
        layerstack,
        file,
    );
    operations
        .workspace_session
        .create_workspace_session(create_request())?;

    let response = publish_json(&operations, json!({"workspace_session_id": "ws-no-op"}));

    assert_eq!(response["publish"]["no_op"], true);
    assert_eq!(
        response["publish"]["revision"]["manifest_version"],
        base.version
    );
    assert_eq!(
        response["publish"]["revision"]["root_hash"],
        sandbox_runtime_layerstack::manifest_root_hash(&base)
    );
    assert_eq!(
        response["publish"]["revision"]["layer_count"],
        base.layers.len()
    );
    assert_eq!(
        response["publish"]["route_summary"],
        json!({"source_count": 0, "ignored_count": 0})
    );
    assert_eq!(response["destroyed"], true);
    assert_eq!(fixture.active_manifest()?, base);
    assert_eq!(fake.capture_calls().len(), 1);
    assert_eq!(fake.destroy_calls().len(), 1);
    Ok(())
}
