use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use workspace::model::{
    CallerId, CreateWorkspaceRequest, DestroyWorkspaceRequest, LatestSnapshotRequest,
    WorkspaceProfile,
};
use workspace::profile::{ResourceCaps, WorkspaceModeManager};
use workspace::WorkspaceRuntimeService;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn latest_snapshot_returns_readonly_handle_without_lease() -> TestResult {
    let fixture = Fixture::new("latest-snapshot")?;
    let service = fixture.service();

    let readonly = service.latest_snapshot(LatestSnapshotRequest {
        workspace_root: fixture.layer_stack_root.clone(),
        owner_request_id: "reader".to_owned(),
    })?;

    assert_eq!(readonly.view_root, fixture.layer_stack_root);
    assert_eq!(readonly.snapshot.manifest_version, 1);
    assert_eq!(readonly.snapshot.layer_paths.len(), 1);
    assert!(readonly.generation_key.starts_with("1:"));
    assert_eq!(
        layerstack::LayerStack::open(readonly.view_root.clone())?.active_lease_count(),
        0
    );
    Ok(())
}

#[test]
fn runtime_service_create_and_destroy_are_backed_by_impl_files() -> TestResult {
    let fixture = Fixture::new("create-destroy")?;
    let service = fixture.service();

    let handle = service.create_workspace(CreateWorkspaceRequest {
        caller_id: CallerId("caller-1".to_owned()),
        workspace_root: fixture.workspace_root.clone(),
        layer_stack_root: fixture.layer_stack_root.clone(),
        profile: WorkspaceProfile::HostCompatible,
    })?;

    assert_eq!(handle.owner, CallerId("caller-1".to_owned()));
    assert_eq!(handle.workspace_root, fixture.workspace_root);
    assert_eq!(handle.profile, WorkspaceProfile::HostCompatible);
    assert_eq!(handle.snapshot.manifest_version, 1);
    assert_eq!(
        layerstack::LayerStack::open(fixture.layer_stack_root.clone())?.active_lease_count(),
        1
    );

    let destroyed = service.destroy_workspace(handle, DestroyWorkspaceRequest::default())?;

    assert_eq!(destroyed.owner, CallerId("caller-1".to_owned()));
    assert_eq!(destroyed.lease_released, Some(true));
    assert_eq!(destroyed.lease_release_error, None);
    assert_eq!(destroyed.active_leases_after, 0);
    Ok(())
}

struct Fixture {
    base: PathBuf,
    layer_stack_root: PathBuf,
    workspace_root: PathBuf,
    scratch_root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let base =
            std::env::temp_dir().join(format!("workspace-service-{label}-{}", unique_suffix()));
        let _ = std::fs::remove_dir_all(&base);
        let layer_stack_root = base.join("layer-stack");
        let workspace_root = base.join("workspace");
        let scratch_root = base.join("scratch");
        let layer = layer_stack_root.join("layers").join("B000001-base");
        std::fs::create_dir_all(&layer)?;
        std::fs::create_dir_all(layer_stack_root.join("staging"))?;
        std::fs::create_dir_all(&workspace_root)?;
        std::fs::write(layer.join("README.md"), "# README\n")?;
        std::fs::write(
            layer_stack_root.join("manifest.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "version": 1,
                "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
            }))?,
        )?;
        Ok(Self {
            base,
            layer_stack_root,
            workspace_root,
            scratch_root,
        })
    }

    fn service(&self) -> WorkspaceRuntimeService {
        let caps = ResourceCaps {
            enabled: true,
            eos_workspace_root: self.workspace_root.to_string_lossy().into_owned(),
            ..ResourceCaps::default()
        };
        WorkspaceRuntimeService::new(WorkspaceModeManager::stubbed(
            caps,
            self.scratch_root.clone(),
        ))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}
