use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_observability::Observer;
use serde_json::json;

use sandbox_runtime_workspace::model::{
    CreateWorkspaceRequest, DestroyWorkspaceRequest, NetworkProfile,
};
use sandbox_runtime_workspace::session::{ResourceCaps, WorkspaceManager};
use sandbox_runtime_workspace::WorkspaceRuntimeService;

#[test]
fn latest_snapshot_returns_readonly_handle_without_lease(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("latest-snapshot")?;
    let service = fixture.service();

    let readonly = service.latest_snapshot()?;

    assert_eq!(readonly.view_root, fixture.layer_stack_root);
    assert_eq!(readonly.snapshot.manifest_version, 1);
    assert_eq!(readonly.snapshot.layer_paths.len(), 1);
    assert!(readonly.generation_key.starts_with("1:"));
    assert_eq!(
        sandbox_runtime_layerstack::LayerStack::open(readonly.view_root.clone())?
            .active_lease_count(),
        0
    );
    Ok(())
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "requires real Linux namespace, mount, and network privileges"
)]
fn runtime_service_create_and_destroy_are_backed_by_impl_files(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("create-destroy")?;
    let service = fixture.service();

    let handle = service.create_workspace(CreateWorkspaceRequest {
        network: NetworkProfile::Shared,
    })?;

    assert_eq!(handle.workspace_root, fixture.workspace_root);
    assert_eq!(handle.network, NetworkProfile::Shared);
    assert_eq!(handle.snapshot.manifest_version, 1);
    assert_eq!(
        sandbox_runtime_layerstack::LayerStack::open(fixture.layer_stack_root.clone())?
            .active_lease_count(),
        1
    );

    let destroyed = service.destroy_workspace(handle, DestroyWorkspaceRequest::default())?;

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
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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
        WorkspaceRuntimeService::new(
            WorkspaceManager::new(
                self.workspace_root.to_string_lossy().into_owned(),
                ResourceCaps::default(),
                self.scratch_root.clone(),
                Observer::disabled(),
            ),
            self.layer_stack_root.clone(),
        )
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
