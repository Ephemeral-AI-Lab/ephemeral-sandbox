use std::ffi::OsString;
use std::path::{Path, PathBuf};

use sandbox_runtime_layerstack::{build_shared_workspace_base, SHARED_BASE_DIR};

use crate::operation::ManagerServices;
use crate::{
    CreateSandboxRequest, ManagerError, ProgressSink, SandboxRecord, SandboxState, SharedBaseMount,
    StartedDaemon,
};

const DEFAULT_SHARED_BASE_CACHE_DIR: &str = "eos-shared-workspace-base-cache";
const SHARED_BASE_CACHE_ENV: &str = "EOS_SHARED_BASE_CACHE";
const CONTAINER_LAYER_STACK_ROOT: &str = "/eos/layer-stack";

pub(crate) struct CreateSandboxInput {
    pub(crate) image: String,
    pub(crate) workspace_root: PathBuf,
    pub(crate) count: usize,
}

pub(crate) fn create_sandbox(
    services: &ManagerServices,
    input: CreateSandboxInput,
    progress: &ProgressSink,
) -> Result<Vec<SandboxRecord>, ManagerError> {
    let CreateSandboxInput {
        image,
        workspace_root,
        count,
    } = input;
    let shared_base = shared_base_mount(&workspace_root, progress)?;
    let mut records = Vec::with_capacity(count);
    for index in 0..count {
        progress.emit(format!(
            "creating shared-base sandbox {}/{}",
            index + 1,
            count
        ));
        match create_one(
            services,
            image.clone(),
            workspace_root.clone(),
            shared_base.clone(),
            progress,
        ) {
            Ok(record) => records.push(record),
            Err(error) => {
                progress.emit("destroying failed shared-base batch");
                for record in records.iter().rev() {
                    rollback(services, record);
                }
                progress.emit("failed shared-base batch destroyed");
                return Err(error);
            }
        }
    }
    Ok(records)
}

fn create_one(
    services: &ManagerServices,
    image: String,
    workspace_root: PathBuf,
    shared_base: SharedBaseMount,
    progress: &ProgressSink,
) -> Result<SandboxRecord, ManagerError> {
    progress.emit(format!(
        "creating runtime sandbox for {}",
        workspace_root.display()
    ));
    let create_request = CreateSandboxRequest {
        image,
        workspace_root: workspace_root.clone(),
        shared_base: Some(shared_base.clone()),
    };
    let created = match services.runtime.create_sandbox(&create_request) {
        Ok(created) => {
            progress.emit("runtime sandbox created");
            progress.emit(format!(
                "shared base mounted source={} target={} root_hash={} readonly={}",
                shared_base.source.display(),
                shared_base.target.display(),
                shared_base.root_hash,
                shared_base.readonly
            ));
            created
        }
        Err(error) => {
            progress.emit(error.to_string());
            return Err(error);
        }
    };
    let id = created.id;
    progress.emit("recording sandbox");
    let record = match services.store.create_with_shared_base(
        id.clone(),
        workspace_root.clone(),
        Some(shared_base.clone()),
    ) {
        Ok(record) => record,
        Err(error) => {
            progress.emit(error.to_string());
            let mut untracked = SandboxRecord::new(id, workspace_root, SandboxState::Creating);
            untracked.shared_base = Some(shared_base);
            let _ = services.runtime.destroy_sandbox(&untracked);
            return Err(error);
        }
    };
    progress.emit("sandbox recorded");
    let started = match provision_daemon(services, &record, progress) {
        Ok(started) => started,
        Err(error) => {
            progress.emit("destroying failed sandbox");
            rollback(services, &record);
            progress.emit("failed sandbox destroyed");
            return Err(error);
        }
    };
    if let Err(error) =
        services
            .store
            .update_endpoints(&id, Some(started.daemon), started.daemon_http)
    {
        progress.emit(error.to_string());
        rollback(services, &record);
        return Err(error);
    }
    progress.emit("marking sandbox ready");
    match services
        .store
        .transition_state(&id, SandboxState::Creating, SandboxState::Ready)
    {
        Ok(ready) => {
            progress.emit("sandbox is ready");
            Ok(ready)
        }
        Err(error) => {
            progress.emit(error.to_string());
            rollback(services, &record);
            Err(error)
        }
    }
}

fn shared_base_mount(
    workspace_root: &Path,
    progress: &ProgressSink,
) -> Result<SharedBaseMount, ManagerError> {
    let cache_root = shared_base_cache_root(workspace_root);
    progress.emit(format!(
        "building shared workspace base cache={} workspace={}",
        cache_root.display(),
        workspace_root.display()
    ));
    let shared = build_shared_workspace_base(&cache_root, workspace_root).map_err(|error| {
        ManagerError::WorkspaceSetupFailed {
            message: error.to_string(),
        }
    })?;
    progress.emit(format!(
        "shared workspace base {} source={} root_hash={} bytes={}",
        if shared.built { "built" } else { "reused" },
        shared.base_mount_source.display(),
        shared.root_hash,
        shared.bytes
    ));
    Ok(SharedBaseMount {
        source: shared.base_mount_source,
        target: PathBuf::from(CONTAINER_LAYER_STACK_ROOT).join(SHARED_BASE_DIR),
        root_hash: shared.root_hash,
        readonly: true,
    })
}

fn shared_base_cache_root(workspace_root: &Path) -> PathBuf {
    shared_base_cache_root_from_env(workspace_root, std::env::var_os(SHARED_BASE_CACHE_ENV))
}

fn shared_base_cache_root_from_env(workspace_root: &Path, configured: Option<OsString>) -> PathBuf {
    configured.map(PathBuf::from).unwrap_or_else(|| {
        workspace_root
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .join(DEFAULT_SHARED_BASE_CACHE_DIR)
    })
}

fn provision_daemon(
    services: &ManagerServices,
    record: &SandboxRecord,
    progress: &ProgressSink,
) -> Result<StartedDaemon, ManagerError> {
    progress.emit("installing daemon assets");
    if let Err(error) = services.daemon_installer.install_daemon(record) {
        progress.emit(error.to_string());
        return Err(error);
    }
    progress.emit("daemon assets installed");

    progress.emit("starting daemon");
    let started = match services.daemon_installer.start_daemon(record) {
        Ok(started) => {
            progress.emit(format!(
                "daemon published on {}:{}",
                started.daemon.host, started.daemon.port
            ));
            started
        }
        Err(error) => {
            progress.emit(error.to_string());
            return Err(error);
        }
    };

    progress.emit("waiting for daemon readiness");
    if let Err(error) =
        services
            .daemon_installer
            .check_daemon_with_progress(record, &started.daemon, progress)
    {
        progress.emit(error.to_string());
        return Err(error);
    }
    progress.emit("daemon is ready");
    Ok(started)
}

fn rollback(services: &ManagerServices, record: &SandboxRecord) {
    let _ = services.daemon_installer.stop_daemon(record);
    let _ = services.runtime.destroy_sandbox(record);
    let _ = services.store.remove(&record.id);
}
