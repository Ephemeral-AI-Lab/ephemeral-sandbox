//! `SandboxRuntime` over bollard: create a stopped Linux container, remove it,
//! and recover existing containers by label after a gateway restart.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use sandbox_config::configs::manager::DockerRuntimeConfig;
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, SandboxDaemonEndpoint, SandboxId,
    SandboxRecord, SandboxRuntime, SandboxState,
};

use crate::engine::{ContainerSpec, DockerEngine, DockerError};
use crate::labels;
use crate::launch::daemon_launch_argv;

const ENDPOINT_HOST: &str = "127.0.0.1";
const TMPFS_OPTIONS: &str = "rw,nosuid,nodev,mode=755";
const RUNTIME_TMPFS_MOUNTS: &[&str] = &["/eos/layer-stack", "/eos/scratch"];

/// Docker-backed runtime. Creates stopped containers; the installer starts them.
pub struct DockerSandboxRuntime {
    engine: DockerEngine,
}

impl DockerSandboxRuntime {
    /// Build a runtime from the resolved Docker config.
    #[must_use]
    pub fn new(config: DockerRuntimeConfig) -> Self {
        Self {
            engine: DockerEngine::new(config),
        }
    }

    /// Rebuild manager records for containers owned by this gateway instance.
    ///
    /// # Errors
    /// Returns an error when the Docker Engine cannot be queried.
    pub fn recover_sandboxes(&self) -> Result<Vec<SandboxRecord>, ManagerError> {
        let config = self.engine.config();
        let recovered = self
            .engine
            .list_recoverable(config.gateway_instance_id.clone(), config.daemon_port)
            .map_err(runtime_failed)?;
        let mut records = Vec::with_capacity(recovered.len());
        for container in recovered {
            let Ok(id) = SandboxId::new(container.sandbox_id) else {
                continue;
            };
            let endpoint = SandboxDaemonEndpoint::new(
                ENDPOINT_HOST,
                container.published_port,
                container.auth_token,
            );
            records.push(SandboxRecord {
                id,
                workspace_root: PathBuf::from(container.host_workspace_root),
                state: SandboxState::Ready,
                daemon: Some(endpoint),
            });
        }
        Ok(records)
    }
}

impl SandboxRuntime for DockerSandboxRuntime {
    fn create_sandbox(
        &self,
        request: &CreateSandboxRequest,
    ) -> Result<CreateSandboxResult, ManagerError> {
        let config = self.engine.config();
        let name = format!("eos-{}", uuid::Uuid::new_v4());
        let id = SandboxId::new(name.clone()).map_err(|error| ManagerError::RuntimeFailed {
            message: format!("generated container name is invalid: {error}"),
        })?;
        let auth_token = uuid::Uuid::new_v4().to_string();
        let record = SandboxRecord::new(
            id.clone(),
            request.workspace_root.clone(),
            SandboxState::Creating,
        );
        let cmd = daemon_launch_argv(config, &record, &auth_token);
        let spec = ContainerSpec {
            name,
            image: resolve_image(config, &request.image),
            cmd,
            env: runtime_timing_env(),
            labels: build_labels(config, &id, &auth_token, &request.workspace_root),
            binds: vec![format!(
                "{}:{}",
                request.workspace_root.display(),
                config.container_workspace_root.display()
            )],
            tmpfs: runtime_tmpfs_mounts(),
            daemon_port: config.daemon_port,
            privileged: config.privileged,
            platform: config.platform.clone(),
            memory_bytes: config.memory_bytes,
            nano_cpus: config.nano_cpus,
        };
        self.engine.create_container(spec).map_err(runtime_failed)?;
        Ok(CreateSandboxResult { id })
    }

    fn destroy_sandbox(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        self.engine
            .remove_container(record.id.as_str().to_owned())
            .map_err(runtime_failed)
    }
}

fn runtime_timing_env() -> Vec<String> {
    ["EOS_RUNTIME_TIMING", "EOS_RUNTIME_TIMING_LOG"]
        .into_iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| format!("{key}={value}"))
        })
        .collect()
}

fn runtime_tmpfs_mounts() -> HashMap<String, String> {
    RUNTIME_TMPFS_MOUNTS
        .iter()
        .map(|path| ((*path).to_owned(), TMPFS_OPTIONS.to_owned()))
        .collect()
}

fn resolve_image(config: &DockerRuntimeConfig, requested: &str) -> String {
    if requested.trim().is_empty() {
        config
            .default_image
            .clone()
            .unwrap_or_else(|| requested.to_owned())
    } else {
        requested.to_owned()
    }
}

fn build_labels(
    config: &DockerRuntimeConfig,
    id: &SandboxId,
    auth_token: &str,
    host_workspace_root: &std::path::Path,
) -> HashMap<String, String> {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    HashMap::from([
        (labels::SANDBOX_ID.to_owned(), id.as_str().to_owned()),
        (
            labels::GATEWAY_INSTANCE_ID.to_owned(),
            config.gateway_instance_id.clone(),
        ),
        (labels::AUTH_TOKEN.to_owned(), auth_token.to_owned()),
        (
            labels::DAEMON_PORT.to_owned(),
            config.daemon_port.to_string(),
        ),
        (
            labels::HOST_WORKSPACE_ROOT.to_owned(),
            host_workspace_root.to_string_lossy().into_owned(),
        ),
        (
            labels::CONTAINER_WORKSPACE_ROOT.to_owned(),
            config
                .container_workspace_root
                .to_string_lossy()
                .into_owned(),
        ),
        (labels::CREATED_AT.to_owned(), created_at.to_string()),
        (
            labels::CLEANUP_POLICY.to_owned(),
            labels::CLEANUP_POLICY_REMOVE_ON_DESTROY.to_owned(),
        ),
    ])
}

fn runtime_failed(error: DockerError) -> ManagerError {
    ManagerError::RuntimeFailed {
        message: error.to_string(),
    }
}
