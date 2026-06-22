use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use sandbox_runtime_workspace::model::{
    CreateWorkspaceRequest, DestroyWorkspaceRequest, WorkspaceProfile,
};
use sandbox_runtime_workspace::profile::{ResourceCaps, WorkspaceModeManager};
use sandbox_runtime_workspace::WorkspaceRuntimeService;

use crate::trace_capture::capture_traces;

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
    ignore = "requires real Linux namespace, cgroup, mount, and network privileges"
)]
fn runtime_service_create_and_destroy_are_backed_by_impl_files(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("create-destroy")?;
    let service = fixture.service();

    let handle = service.create_workspace(CreateWorkspaceRequest {
        profile: WorkspaceProfile::HostCompatible,
    })?;

    assert_eq!(handle.workspace_root, fixture.workspace_root);
    assert_eq!(handle.profile, WorkspaceProfile::HostCompatible);
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

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "requires real Linux namespace, cgroup, mount, and network privileges"
)]
fn runtime_service_create_emits_existing_phase_timing_events(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("trace-create-phases")?;
    let service = fixture.service();

    let traces = capture_traces(|| {
        let handle = service
            .create_workspace(CreateWorkspaceRequest {
                profile: WorkspaceProfile::HostCompatible,
            })
            .expect("create workspace succeeds");
        service
            .destroy_workspace(handle, DestroyWorkspaceRequest::default())
            .expect("destroy workspace succeeds");
    });

    for phase in [
        "spawn_ns_holder",
        "open_ns_fds",
        "mount_overlay",
        "create_cgroup",
        "join_holder_cgroup",
    ] {
        assert!(
            traces.contains("event workspace_create_phase_finished")
                && traces.contains(&format!("phase={phase}"))
                && traces.contains("duration_ms="),
            "missing create phase {phase} in {traces}"
        );
    }
    for forbidden in [
        "WorkspaceHandle",
        "WorkspaceEntry",
        "/workspace",
        "/layer-stack",
        "manifest.json",
    ] {
        assert!(
            !traces.contains(forbidden),
            "forbidden value {forbidden} appeared in traces: {traces}"
        );
    }
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
            WorkspaceModeManager::new(
                self.workspace_root.to_string_lossy().into_owned(),
                ResourceCaps::default(),
                self.scratch_root.clone(),
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
