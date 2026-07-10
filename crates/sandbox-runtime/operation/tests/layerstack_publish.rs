mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sandbox_runtime::command::{CommandStatus, ExecCommandInput};
use sandbox_runtime_workspace::{
    CapturedWorkspaceChanges, LayerStackSnapshotRef, LeaseId, NetworkProfile, WorkspaceHandle,
    WorkspaceSessionId,
};

use support::{
    build_services_with_launch_driver, build_services_with_launch_driver_and_layerstack,
    create_request, success_exit, FakeLaunchDriver, FakeWorkspaceService, ScriptedCommandYield,
};

struct PublishFixture {
    base: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
}

impl PublishFixture {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let base = std::env::temp_dir().join(format!(
            "operation-layerstack-publish-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace)?;
        Ok(Self {
            base,
            root,
            workspace,
        })
    }

    fn build_base(
        &self,
    ) -> Result<sandbox_runtime_layerstack::Manifest, Box<dyn std::error::Error + Send + Sync>>
    {
        sandbox_runtime_layerstack::build_workspace_base(&self.root, &self.workspace, false)?;
        let stack = sandbox_runtime_layerstack::LayerStack::open(self.root.clone())?;
        Ok(stack.read_active_manifest()?)
    }

    fn service(
        &self,
    ) -> Result<
        sandbox_runtime::layerstack::LayerStackService,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        Ok(sandbox_runtime::layerstack::LayerStackService::new(
            self.root.clone(),
            self.base.join("scratch"),
            sandbox_runtime::LayerstackRuntimeConfig::default(),
            sandbox_observability::Observer::disabled(),
            support::test_file_service(),
        )?)
    }
}

impl Drop for PublishFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

fn exec_input(workspace_session_id: WorkspaceSessionId) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: Some(workspace_session_id),
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(250),
    }
}

fn implicit_exec_input() -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: None,
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(5_000),
    }
}

fn workspace_handle(
    manifest: sandbox_runtime_layerstack::Manifest,
    layer_stack_root: &std::path::Path,
) -> WorkspaceHandle {
    let root_hash = sandbox_runtime_layerstack::manifest_root_hash(&manifest);
    let layer_paths = manifest
        .layers
        .iter()
        .map(|layer| layer_stack_root.join(&layer.path))
        .collect::<Vec<_>>();
    let snapshot = LayerStackSnapshotRef {
        lease_id: LeaseId("lease-1".to_owned()),
        manifest_version: manifest.version,
        root_hash,
        manifest,
        layer_paths,
    };
    WorkspaceHandle::holder_backed_for_test(
        WorkspaceSessionId("workspace-session".to_owned()),
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
        snapshot,
        std::env::temp_dir().join("operation-layerstack-publish-upper"),
        std::env::temp_dir().join("operation-layerstack-publish-work"),
    )
}

fn read_text(
    fixture: &PublishFixture,
    path: &str,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let stack = sandbox_runtime_layerstack::LayerStack::open(fixture.root.clone())?;
    let manifest = stack.read_active_manifest()?;
    let view = sandbox_runtime_layerstack::MergedView::new(fixture.root.clone());
    let (bytes, exists) = view.read_bytes(path, &manifest)?;
    if !exists {
        return Ok(None);
    }
    let bytes = bytes.expect("merged view returned bytes for existing path");
    Ok(Some(String::from_utf8(bytes).expect("test file is utf8")))
}

fn lp(path: &str) -> sandbox_runtime_layerstack::LayerPath {
    sandbox_runtime_layerstack::LayerPath::parse(path).expect("test path is valid")
}

#[test]
fn existing_session_command_completion_does_not_publish(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("existing-session-no-publish")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let base_revision = sandbox_runtime::layerstack::LayerStackRevision {
        manifest_version: base.version,
        root_hash: sandbox_runtime_layerstack::manifest_root_hash(&base),
        layer_count: base.layers.len(),
    };
    let handle = workspace_handle(base.clone(), &fixture.root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())?
        .workspace_session_id;

    let output = env.command.exec_command(exec_input(workspace_session_id))?;

    assert_eq!(output.status, CommandStatus::Ok);
    assert!(fake.capture_calls().is_empty());
    assert!(fake.destroy_calls().is_empty());
    assert_eq!(read_text(&fixture, "README.md")?, Some("base\n".to_owned()));
    let resolved = env
        .workspace
        .resolve_session(WorkspaceSessionId("workspace-session".to_owned()))?;
    assert_eq!(
        resolved.handle.snapshot.manifest_version,
        base_revision.manifest_version
    );
    assert_eq!(resolved.handle.snapshot.root_hash, base_revision.root_hash);
    assert_eq!(
        sandbox_runtime_layerstack::manifest_root_hash(&resolved.handle.snapshot.manifest),
        resolved.handle.snapshot.root_hash
    );
    Ok(())
}

#[test]
fn implicit_session_completion_publishes_captured_changes_before_destroy(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("implicit-publish")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let handle = workspace_handle(base.clone(), &fixture.root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(CapturedWorkspaceChanges {
        workspace_session_id: handle.id.clone(),
        base_revision: handle.base_revision(),
        base_manifest: base,
        changed_paths: vec!["README.md".to_owned()],
        changed_path_kinds: Default::default(),
        protected_drops: Vec::new(),
        stats: None,
        changes: vec![sandbox_runtime_layerstack::LayerChange::Write {
            path: lp("README.md"),
            content: b"implicit\n".to_vec(),
        }],
        metadata_path_count: 1,
    }));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        launch_driver,
        Arc::new(fixture.service()?),
    );

    let _ = env.command.exec_command(implicit_exec_input())?;
    wait_for_destroy(&fake);

    assert_eq!(
        fake.capture_calls(),
        vec![WorkspaceSessionId("workspace-session".to_owned())]
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-session".to_owned())]
    );
    assert_eq!(
        read_text(&fixture, "README.md")?,
        Some("implicit\n".to_owned())
    );
    Ok(())
}

fn wait_for_destroy(fake: &FakeWorkspaceService) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while fake.destroy_calls().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn empty_capture_skips_publish_and_still_destroys(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("empty-capture-skip")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let base_version = base.version;
    let handle = workspace_handle(base.clone(), &fixture.root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(CapturedWorkspaceChanges {
        workspace_session_id: handle.id.clone(),
        base_revision: handle.base_revision(),
        base_manifest: base,
        changed_paths: Vec::new(),
        changed_path_kinds: Default::default(),
        protected_drops: Vec::new(),
        stats: None,
        changes: Vec::new(),
        metadata_path_count: 0,
    }));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        launch_driver,
        Arc::new(fixture.service()?),
    );

    let output = env.command.exec_command(implicit_exec_input())?;
    wait_for_destroy(&fake);

    assert_eq!(output.publish_rejected, None);
    assert_eq!(
        fake.capture_calls(),
        vec![WorkspaceSessionId("workspace-session".to_owned())]
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-session".to_owned())]
    );
    let stack = sandbox_runtime_layerstack::LayerStack::open(fixture.root.clone())?;
    let manifest = stack.read_active_manifest()?;
    assert_eq!(
        manifest.version, base_version,
        "an empty capture publishes nothing"
    );
    Ok(())
}

#[test]
fn rejected_finalize_publish_surfaces_on_terminal_response_and_destroy_proceeds(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("finalize-publish-reject")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let handle = workspace_handle(base.clone(), &fixture.root);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(CapturedWorkspaceChanges {
        workspace_session_id: handle.id.clone(),
        base_revision: handle.base_revision(),
        base_manifest: base,
        changed_paths: vec!["manifest.json".to_owned()],
        changed_path_kinds: Default::default(),
        protected_drops: Vec::new(),
        stats: None,
        changes: vec![sandbox_runtime_layerstack::LayerChange::Write {
            path: lp("manifest.json"),
            content: b"forbidden\n".to_vec(),
        }],
        metadata_path_count: 1,
    }));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        launch_driver,
        Arc::new(fixture.service()?),
    );

    let output = env.command.exec_command(implicit_exec_input())?;
    wait_for_destroy(&fake);

    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(
        output.publish_rejected,
        Some("protected_path"),
        "the completing command's terminal response carries the reject class"
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-session".to_owned())],
        "destroy proceeds even when the publish is rejected"
    );
    assert_eq!(
        read_text(&fixture, "README.md")?,
        Some("base\n".to_owned()),
        "the rejected changeset is discarded whole"
    );
    Ok(())
}

#[test]
fn layerstack_service_rejects_invalid_base_revision(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("invalid-base")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let service = fixture.service()?;

    let error = service
        .publish_changes(sandbox_runtime::layerstack::PublishChangesRequest {
            expected_base: sandbox_runtime::layerstack::LayerStackRevision {
                manifest_version: base.version,
                root_hash: "not-the-base-root".to_owned(),
                layer_count: base.layers.len(),
            },
            base_manifest: base,
            protected_drops: Vec::new(),
            changes: Vec::new(),
            owner: "operation:test".to_owned(),
        })
        .expect_err("invalid base metadata rejects before publish");

    assert!(matches!(
        error,
        sandbox_runtime::layerstack::LayerStackServiceError::InvalidBaseRevision { .. }
    ));
    Ok(())
}

#[test]
fn layerstack_service_preserves_structured_publish_rejection(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("structured-reject")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let service = fixture.service()?;
    let revision = sandbox_runtime::layerstack::LayerStackRevision {
        manifest_version: base.version,
        root_hash: sandbox_runtime_layerstack::manifest_root_hash(&base),
        layer_count: base.layers.len(),
    };

    let error = service
        .publish_changes(sandbox_runtime::layerstack::PublishChangesRequest {
            expected_base: revision,
            base_manifest: base,
            protected_drops: Vec::new(),
            changes: vec![sandbox_runtime_layerstack::LayerChange::Write {
                path: lp("manifest.json"),
                content: b"bad\n".to_vec(),
            }],
            owner: "operation:test".to_owned(),
        })
        .expect_err("protected path rejects publish");

    match error {
        sandbox_runtime::layerstack::LayerStackServiceError::PublishRejected { rejection } => {
            assert_eq!(
                rejection.reason,
                sandbox_runtime_layerstack::PublishRejectReason::ProtectedPath
            );
            assert_eq!(
                rejection.path.as_ref().map(ToString::to_string).as_deref(),
                Some("manifest.json")
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
    Ok(())
}

#[test]
fn layerstack_service_empty_changes_return_no_op_revision(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("service-empty-no-op")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let service = fixture.service()?;
    let revision = sandbox_runtime::layerstack::LayerStackRevision {
        manifest_version: base.version,
        root_hash: sandbox_runtime_layerstack::manifest_root_hash(&base),
        layer_count: base.layers.len(),
    };

    let result = service.publish_changes(sandbox_runtime::layerstack::PublishChangesRequest {
        expected_base: revision.clone(),
        base_manifest: base.clone(),
        protected_drops: Vec::new(),
        changes: Vec::new(),
        owner: "operation:test".to_owned(),
    })?;

    assert!(result.no_op);
    assert_eq!(result.revision, revision);
    assert_eq!(result.manifest, base);
    assert_eq!(result.route_summary.source_count, 0);
    assert_eq!(result.route_summary.ignored_count, 0);
    Ok(())
}

#[test]
fn layerstack_service_ignored_only_publish_preserves_route_summary(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("service-ignored-only")?;
    std::fs::write(fixture.workspace.join(".gitignore"), "out.log\n")?;
    let base = fixture.build_base()?;
    let service = fixture.service()?;
    let revision = sandbox_runtime::layerstack::LayerStackRevision {
        manifest_version: base.version,
        root_hash: sandbox_runtime_layerstack::manifest_root_hash(&base),
        layer_count: base.layers.len(),
    };

    let result = service.publish_changes(sandbox_runtime::layerstack::PublishChangesRequest {
        expected_base: revision,
        base_manifest: base,
        protected_drops: Vec::new(),
        changes: vec![sandbox_runtime_layerstack::LayerChange::Write {
            path: lp("out.log"),
            content: b"ignored\n".to_vec(),
        }],
        owner: "operation:test".to_owned(),
    })?;

    assert!(!result.no_op);
    assert_eq!(result.route_summary.source_count, 0);
    assert_eq!(result.route_summary.ignored_count, 1);
    assert_eq!(
        read_text(&fixture, "out.log")?,
        Some("ignored\n".to_owned())
    );
    Ok(())
}
