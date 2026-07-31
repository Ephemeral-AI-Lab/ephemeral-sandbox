use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use sandbox_observability_telemetry::Observer;
use serde_json::json;

use sandbox_runtime_namespace_process::runner::file_op::FileRunnerOp;
use sandbox_runtime_workspace::model::{
    CaptureChangesRequest, CreateWorkspaceRequest, DestroyWorkspaceRequest, ExternalOverlayLayout,
    NetworkProfile,
};
use sandbox_runtime_workspace::session::{ResourceCaps, WorkspaceManager};
use sandbox_runtime_workspace::{holder_exit_channel, HolderExitWait, WorkspaceRuntimeService};

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

    let handle = service.create_workspace(create_request(&service)?)?;

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

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "requires real Linux namespace, mount, and network privileges"
)]
fn external_mpla_overlay_does_not_acquire_or_persist_an_ordinary_layerstack_lease(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("external-mpla-unleased")?;
    let service = fixture.service();
    let external_root = fixture.base.join("external");
    let upperdir = external_root.join("upper");
    let workdir = external_root.join("work");
    let lowerdir = external_root.join("lower");
    let workspace_root = external_root.join("workspace");
    for path in [&upperdir, &workdir, &lowerdir, &workspace_root] {
        std::fs::create_dir_all(path)?;
    }

    let handle = service.create_workspace_with_external_overlay(
        create_request(&service)?,
        ExternalOverlayLayout {
            workspace_root,
            upperdir,
            workdir,
            lower_dirs_newest_first: vec![lowerdir.clone()],
        },
    )?;

    assert_eq!(handle.snapshot.layer_paths, vec![lowerdir]);
    assert!(handle.snapshot.lease_id.0.starts_with("external-mpla:"));
    assert_eq!(
        sandbox_runtime_layerstack::LayerStack::open(fixture.layer_stack_root.clone())?
            .active_lease_count(),
        0
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.scratch_root.join("manager.json"))?)?;
    assert!(
        persisted["handles"][0]["lease_id"].is_null(),
        "an external MPLA overlay must not claim an ordinary LayerStack lease"
    );

    let destroyed = service.destroy_workspace(handle, DestroyWorkspaceRequest::default())?;
    assert_eq!(destroyed.lease_released, Some(true));
    assert_eq!(destroyed.active_leases_after, 0);
    Ok(())
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "requires real Linux namespace, mount, and network privileges"
)]
fn failed_lease_release_keeps_destroy_retryable_without_recreating_workspace(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("destroy-lease-retry")?;
    let service = fixture.service();
    let handle = service.create_workspace(create_request(&service)?)?;
    let owned = service.ownership_snapshot()?;
    assert_eq!(owned.namespace_fd_count, 0);
    assert_eq!(owned.control_fd_count, 0);
    assert_eq!(owned.active_scratch_directories, 1);
    assert_eq!(owned.persisted_workspace_handles, 1);
    assert_eq!(owned.exited_unreaped_holders, 0);
    let saved_layer_stack = fixture.base.join("saved-layer-stack");
    std::fs::rename(&fixture.layer_stack_root, &saved_layer_stack)?;
    std::fs::write(&fixture.layer_stack_root, "not a directory")?;

    let first = service
        .destroy_workspace(handle.clone(), DestroyWorkspaceRequest::default())
        .expect_err("lease release failure remains retryable");
    assert!(matches!(
        first,
        sandbox_runtime_workspace::WorkspaceError::Cleanup { ref failures, .. }
            if failures.iter().any(|failure| failure.starts_with("Leases:"))
    ));
    let retained = service.ownership_snapshot()?;
    assert_eq!(retained.namespace_fd_count, 0);
    assert_eq!(retained.control_fd_count, 0);
    assert_eq!(retained.active_scratch_directories, 0);
    assert_eq!(retained.persisted_workspace_handles, 1);
    assert_eq!(persisted_handle_count(&fixture.scratch_root)?, 1);

    std::fs::remove_file(&fixture.layer_stack_root)?;
    std::fs::rename(&saved_layer_stack, &fixture.layer_stack_root)?;
    let destroyed = service.destroy_workspace(handle, DestroyWorkspaceRequest::default())?;
    assert_eq!(destroyed.lease_released, Some(true));
    assert_eq!(destroyed.active_leases_after, 0);
    assert_eq!(
        service.ownership_snapshot()?,
        sandbox_runtime_workspace::WorkspaceOwnershipSnapshot::default()
    );
    assert_eq!(persisted_handle_count(&fixture.scratch_root)?, 0);
    Ok(())
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "requires real Linux namespace, mount, and network privileges"
)]
fn successful_peer_destroy_preserves_another_workspaces_retry_record(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("destroy-peer-persistence")?;
    let service = fixture.service();
    let first = service.create_workspace(create_request(&service)?)?;
    let second = service.create_workspace(create_request(&service)?)?;
    let saved_layer_stack = fixture.base.join("saved-layer-stack");
    std::fs::rename(&fixture.layer_stack_root, &saved_layer_stack)?;
    std::fs::write(&fixture.layer_stack_root, "not a directory")?;

    service
        .destroy_workspace(first.clone(), DestroyWorkspaceRequest::default())
        .expect_err("first workspace remains retryable");
    assert_eq!(
        persisted_handle_ids(&fixture.scratch_root)?,
        vec![first.id.0.clone(), second.id.0.clone()]
    );

    std::fs::remove_file(&fixture.layer_stack_root)?;
    std::fs::rename(&saved_layer_stack, &fixture.layer_stack_root)?;
    service.destroy_workspace(second, DestroyWorkspaceRequest::default())?;

    assert_eq!(
        persisted_handle_ids(&fixture.scratch_root)?,
        vec![first.id.0.clone()]
    );
    let retained = service.ownership_snapshot()?;
    assert_eq!(retained.persisted_workspace_handles, 1);
    assert_eq!(retained.active_scratch_directories, 0);

    service.destroy_workspace(first, DestroyWorkspaceRequest::default())?;
    assert_eq!(persisted_handle_count(&fixture.scratch_root)?, 0);
    Ok(())
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "requires real Linux namespace, mount, and network privileges"
)]
fn failed_partial_create_rollback_is_visible_and_reconcilable(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("create-rollback-retry")?;
    let service = fixture.service();
    std::fs::create_dir_all(&fixture.scratch_root)?;
    std::fs::create_dir(fixture.scratch_root.join("manager.json.tmp"))?;
    let request = create_request(&service)?;
    let workspace_session_id = request.workspace_session_id.clone();

    let error = service
        .create_workspace(request)
        .expect_err("persistence fault must make rollback failure visible");
    assert!(matches!(
        error,
        sandbox_runtime_workspace::WorkspaceError::Cleanup {
            workspace_session_id: ref failed_id,
            ref failures,
        } if failed_id == &workspace_session_id.0
            && failures.iter().any(|failure| failure.starts_with("CreateSetup:"))
            && failures.iter().any(|failure| failure.starts_with("Persistence:"))
    ));
    let retained = service.ownership_snapshot()?;
    assert_eq!(retained.namespace_fd_count, 0);
    assert_eq!(retained.control_fd_count, 0);
    assert_eq!(retained.active_scratch_directories, 0);
    assert_eq!(retained.persisted_workspace_handles, 1);
    assert_eq!(
        sandbox_runtime_layerstack::LayerStack::open(fixture.layer_stack_root.clone())?
            .active_lease_count(),
        0
    );

    std::fs::remove_dir(fixture.scratch_root.join("manager.json.tmp"))?;
    let reconciled = service.reconcile_pending_teardowns()?;
    assert_eq!(reconciled.len(), 1);
    let result = reconciled
        .into_iter()
        .next()
        .expect("one retained rollback")?;
    assert_eq!(result.workspace_session_id, workspace_session_id);
    assert_eq!(result.active_leases_after, 0);
    assert_eq!(
        service.ownership_snapshot()?,
        sandbox_runtime_workspace::WorkspaceOwnershipSnapshot::default()
    );
    assert_eq!(persisted_handle_count(&fixture.scratch_root)?, 0);
    Ok(())
}

#[test]
fn invalid_root_after_snapshot_adoption_releases_the_create_lease(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("invalid-root-create-rollback")?;
    let service = fixture.service_with_workspace_root(PathBuf::from("relative-workspace"));

    let error = service
        .create_workspace(create_request(&service)?)
        .expect_err("relative workspace root is rejected");
    assert!(matches!(
        error,
        sandbox_runtime_workspace::WorkspaceError::InvalidRequest { .. }
    ));
    assert_eq!(
        sandbox_runtime_layerstack::LayerStack::open(fixture.layer_stack_root.clone())?
            .active_lease_count(),
        0
    );
    assert_eq!(
        service.ownership_snapshot()?,
        sandbox_runtime_workspace::WorkspaceOwnershipSnapshot::default()
    );
    assert_eq!(persisted_handle_count(&fixture.scratch_root)?, 0);
    Ok(())
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "requires real Linux namespace, mount, and network privileges"
)]
fn stale_holder_generation_cannot_access_reused_workspace_id(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("stale-holder-generation")?;
    let service = fixture.service();
    let workspace_session_id = service.allocate_workspace_session_id(NetworkProfile::Shared)?;
    let request = CreateWorkspaceRequest {
        workspace_session_id: workspace_session_id.clone(),
        network: NetworkProfile::Shared,
    };
    let stale = service.create_workspace(request.clone())?;
    let destroyed = service.destroy_workspace(stale.clone(), DestroyWorkspaceRequest::default())?;
    assert_eq!(
        service.destroy_workspace(stale.clone(), DestroyWorkspaceRequest::default()),
        Ok(destroyed)
    );
    let current = service.create_workspace(request)?;

    assert_eq!(
        service.destroy_workspace(stale.clone(), DestroyWorkspaceRequest::default()),
        Err(sandbox_runtime_workspace::WorkspaceError::NotOpen)
    );
    assert_eq!(
        service.capture_changes(
            &stale,
            CaptureChangesRequest {
                include_stats: false,
            },
        ),
        Err(sandbox_runtime_workspace::WorkspaceError::NotOpen)
    );
    assert_eq!(
        service.run_file_op(
            &stale,
            None,
            FileRunnerOp::ReadFile {
                rel: "README.md".to_owned(),
                max_bytes: 1024,
            },
        ),
        Err(sandbox_runtime_workspace::WorkspaceError::NotOpen)
    );
    assert!(current.holder_is_live());
    assert_eq!(
        service
            .current_handle(&workspace_session_id)?
            .expect("current generation remains open")
            .holder_identity(),
        current.holder_identity()
    );

    service.destroy_workspace(current, DestroyWorkspaceRequest::default())?;
    assert_eq!(
        service.ownership_snapshot()?,
        sandbox_runtime_workspace::WorkspaceOwnershipSnapshot::default()
    );
    Ok(())
}

#[test]
fn holder_exit_channel_coalesces_a_bounded_wake() {
    let (notifier, subscription) = holder_exit_channel();
    let (listener, shutdown) = subscription.into_parts();

    for _ in 0..10_000 {
        notifier.notify();
    }

    assert_eq!(
        listener.wait_for_retry(std::time::Duration::ZERO),
        HolderExitWait::Wake
    );
    assert_eq!(
        listener.wait_for_retry(std::time::Duration::ZERO),
        HolderExitWait::RetryDeadline
    );
    shutdown.stop();
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "requires real Linux namespace, mount, and network privileges"
)]
fn concurrent_shutdown_is_joinable_idempotent_and_closes_admission(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("concurrent-shutdown")?;
    let service = Arc::new(fixture.service());
    for _ in 0..8 {
        service.create_workspace(create_request(&service)?)?;
    }
    let start = Arc::new(Barrier::new(3));
    let left_service = Arc::clone(&service);
    let left_start = Arc::clone(&start);
    let left = std::thread::spawn(move || {
        left_start.wait();
        left_service.shutdown()
    });
    let right_service = Arc::clone(&service);
    let right_start = Arc::clone(&start);
    let right = std::thread::spawn(move || {
        right_start.wait();
        right_service.shutdown()
    });
    start.wait();

    let left = left.join().expect("left shutdown joins");
    let right = right.join().expect("right shutdown joins");

    assert_eq!(left, right);
    assert!(left.is_complete());
    assert_eq!(service.shutdown(), left);
    assert_eq!(
        service.allocate_workspace_session_id(NetworkProfile::Shared),
        Err(sandbox_runtime_workspace::WorkspaceError::Closing)
    );
    assert_eq!(
        service.ownership_snapshot()?,
        sandbox_runtime_workspace::WorkspaceOwnershipSnapshot::default()
    );
    Ok(())
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "requires real Linux namespace, mount, and network privileges"
)]
fn failed_shutdown_stays_retryable_without_stopping_namespace_ownership(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("shutdown-retry")?;
    let service = fixture.service();
    let handle = service.create_workspace(create_request(&service)?)?;
    let saved_layer_stack = fixture.base.join("shutdown-saved-layer-stack");
    std::fs::rename(&fixture.layer_stack_root, &saved_layer_stack)?;
    std::fs::write(&fixture.layer_stack_root, "not a directory")?;

    let first = service.shutdown();

    assert!(!first.is_complete());
    assert!(!first.namespace_stopped);
    assert_eq!(first.workspaces.retryable_failures.len(), 1);
    assert_eq!(
        first.workspaces.retryable_failures[0].workspace_session_id,
        handle.id
    );
    assert_eq!(service.ownership_snapshot()?.control_fd_count, 0);
    assert_eq!(
        service.create_workspace(create_request_without_allocation()),
        Err(sandbox_runtime_workspace::WorkspaceError::Closing)
    );

    std::fs::remove_file(&fixture.layer_stack_root)?;
    std::fs::rename(&saved_layer_stack, &fixture.layer_stack_root)?;
    let retry = service.shutdown();

    assert!(retry.is_complete());
    assert!(retry.namespace_stopped);
    assert!(retry.workspaces.retryable_failures.is_empty());
    assert_eq!(service.shutdown(), retry);
    assert_eq!(
        service.ownership_snapshot()?,
        sandbox_runtime_workspace::WorkspaceOwnershipSnapshot::default()
    );
    Ok(())
}

fn create_request(
    service: &WorkspaceRuntimeService,
) -> Result<CreateWorkspaceRequest, sandbox_runtime_workspace::WorkspaceError> {
    Ok(CreateWorkspaceRequest {
        workspace_session_id: service.allocate_workspace_session_id(NetworkProfile::Shared)?,
        network: NetworkProfile::Shared,
    })
}

fn create_request_without_allocation() -> CreateWorkspaceRequest {
    CreateWorkspaceRequest {
        workspace_session_id: sandbox_runtime_workspace::WorkspaceSessionId(
            "workspace-after-closing".to_owned(),
        ),
        network: NetworkProfile::Shared,
    }
}

fn persisted_handle_count(
    scratch_root: &std::path::Path,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let payload: serde_json::Value =
        serde_json::from_slice(&std::fs::read(scratch_root.join("manager.json"))?)?;
    Ok(payload
        .get("handles")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len))
}

fn persisted_handle_ids(
    scratch_root: &std::path::Path,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let payload: serde_json::Value =
        serde_json::from_slice(&std::fs::read(scratch_root.join("manager.json"))?)?;
    let mut ids = payload
        .get("handles")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|record| record.get("workspace_handle_id"))
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ids.sort();
    Ok(ids)
}

struct Fixture {
    base: PathBuf,
    layer_stack_root: PathBuf,
    workspace_root: PathBuf,
    scratch_root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let base = std::env::current_dir()?
            .join("target")
            .join(format!("workspace-service-{label}-{}", unique_suffix()));
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
        self.service_with_workspace_root(self.workspace_root.clone())
    }

    fn service_with_workspace_root(&self, workspace_root: PathBuf) -> WorkspaceRuntimeService {
        WorkspaceRuntimeService::new(
            WorkspaceManager::new(
                workspace_root.to_string_lossy().into_owned(),
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
