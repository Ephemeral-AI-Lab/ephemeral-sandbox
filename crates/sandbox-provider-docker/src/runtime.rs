//! `SandboxRuntime` over bollard: create a stopped Linux container, remove it,
//! and recover existing containers by label after a gateway restart.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use sandbox_config::configs::manager::DockerRuntimeConfig;
use sandbox_config::configs::runtime::RuntimeConfig;
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, SandboxDaemonEndpoint,
    SandboxHttpEndpoint, SandboxId, SandboxRecord, SandboxResourceMetrics, SandboxResourceProfile,
    SandboxRuntime, SandboxState, SharedBaseMount,
};

use crate::archive::{build_shared_base_seed_archive, build_shared_base_volume_archive};
use crate::engine::{ContainerSpec, DockerEngine, DockerError, VolumeSpec};
use crate::labels;
use crate::launch::daemon_launch_argv;

const ENDPOINT_HOST: &str = "127.0.0.1";
const SHARED_BASE_VOLUME_MOUNT_ROOT: &str = "/eos-shared-base-seed";
static SEEDED_SHARED_BASE_VOLUMES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Docker-backed runtime. Creates stopped containers; the installer starts them.
pub struct DockerSandboxRuntime {
    engine: DockerEngine,
}

/// Effective Docker and workload-cgroup limits selected for new sandboxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerResourceLimits {
    pub profile_name: String,
    pub nano_cpus: i64,
    pub memory_high_bytes: i64,
    pub memory_max_bytes: i64,
    pub pids_max: i64,
    pub workload_memory_high_bytes: i64,
    pub workload_memory_max_bytes: i64,
    pub workload_pids_max: i64,
    pub control_plane_pids_reserve: i64,
    pub daemon_runtime_profile: String,
    pub separate_workload_cgroup: bool,
}

impl DockerResourceLimits {
    fn into_manager_profile(self) -> SandboxResourceProfile {
        SandboxResourceProfile {
            name: self.profile_name,
            nano_cpus: self.nano_cpus,
            memory_high_bytes: self.memory_high_bytes,
            memory_max_bytes: self.memory_max_bytes,
            pids_max: self.pids_max,
            workload_memory_high_bytes: self.workload_memory_high_bytes,
            workload_memory_max_bytes: self.workload_memory_max_bytes,
            workload_pids_max: self.workload_pids_max,
            control_plane_pids_reserve: self.control_plane_pids_reserve,
            daemon_runtime_profile: self.daemon_runtime_profile,
            separate_workload_cgroup: self.separate_workload_cgroup,
        }
    }
}

impl DockerSandboxRuntime {
    /// Build a runtime from the resolved Docker config.
    #[must_use]
    pub fn new(config: DockerRuntimeConfig) -> Self {
        Self {
            engine: DockerEngine::new(config),
        }
    }

    /// Return the effective limits for the configured named profile.
    #[must_use]
    pub fn configured_resource_limits(&self) -> Option<DockerResourceLimits> {
        configured_resource_limits(self.engine.config())
    }

    /// Rebuild manager records for containers owned by this gateway instance.
    ///
    /// # Errors
    /// Returns an error when the Docker Engine cannot be queried.
    pub fn recover_sandboxes(&self) -> Result<Vec<SandboxRecord>, ManagerError> {
        let config = self.engine.config();
        let recovered = self
            .engine
            .list_recoverable(
                config.gateway_instance_id.clone(),
                config.daemon_port,
                config.daemon_http_port,
            )
            .map_err(runtime_failed)?;
        let fallback_resource_profile =
            configured_resource_limits(config).map(DockerResourceLimits::into_manager_profile);
        let mut records = Vec::with_capacity(recovered.len());
        for container in recovered {
            let Ok(id) = SandboxId::new(container.sandbox_id.clone()) else {
                continue;
            };
            let shared_base = recovered_shared_base(&container);
            let endpoint = SandboxDaemonEndpoint::new(
                ENDPOINT_HOST,
                container.published_port,
                container.auth_token,
            );
            records.push(SandboxRecord {
                id,
                workspace_root: PathBuf::from(container.host_workspace_root),
                state: SandboxState::Ready,
                activity_revision: 0,
                daemon: Some(endpoint),
                daemon_http: Some(SandboxHttpEndpoint::new(
                    ENDPOINT_HOST,
                    container.published_http_port,
                )),
                shared_base,
                resource_profile: container
                    .resource_profile
                    .clone()
                    .or_else(|| fallback_resource_profile.clone()),
            });
        }
        Ok(records)
    }

    fn ensure_shared_base_volume(
        &self,
        config: &DockerRuntimeConfig,
        image: &str,
        shared_base: &SharedBaseMount,
    ) -> Result<String, ManagerError> {
        let volume_name = shared_base_volume_name(&shared_base.root_hash);
        let seeded = SEEDED_SHARED_BASE_VOLUMES.get_or_init(|| Mutex::new(HashSet::new()));
        let mut seeded = seeded.lock().map_err(|_| ManagerError::RuntimeFailed {
            message: "shared base volume seed lock poisoned".to_owned(),
        })?;
        if seeded.contains(&volume_name) {
            return Ok(volume_name);
        }
        if self
            .engine
            .volume_exists(volume_name.clone())
            .map_err(runtime_failed)?
        {
            seeded.insert(volume_name.clone());
            return Ok(volume_name);
        }
        let archive = build_shared_base_volume_archive(
            Path::new(SHARED_BASE_VOLUME_MOUNT_ROOT),
            &shared_base.source,
        )
        .map_err(|error| ManagerError::RuntimeFailed {
            message: format!("failed to build shared base volume archive: {error}"),
        })?;
        self.engine
            .seed_volume_from_archive(
                image.to_owned(),
                VolumeSpec {
                    name: volume_name.clone(),
                    target: SHARED_BASE_VOLUME_MOUNT_ROOT.to_owned(),
                    labels: build_shared_base_volume_labels(config, shared_base),
                },
                archive,
            )
            .map_err(runtime_failed)?;
        seeded.insert(volume_name.clone());
        Ok(volume_name)
    }
}

impl SandboxRuntime for DockerSandboxRuntime {
    fn list_images(&self) -> Result<Vec<String>, ManagerError> {
        self.engine.list_images().map_err(runtime_failed)
    }

    fn read_sandbox_resource_metrics(
        &self,
        id: &SandboxId,
    ) -> Result<SandboxResourceMetrics, ManagerError> {
        let metrics = self
            .engine
            .container_resource_metrics(id.as_str().to_owned())
            .map_err(runtime_failed)?;
        Ok(manager_resource_metrics(metrics))
    }

    fn read_sandbox_resource_metrics_batch(
        &self,
        ids: &[SandboxId],
    ) -> Vec<(SandboxId, Result<SandboxResourceMetrics, ManagerError>)> {
        let containers = ids.iter().map(|id| id.as_str().to_owned()).collect();
        match self.engine.container_resource_metrics_batch(containers) {
            Ok(metrics) => ids
                .iter()
                .cloned()
                .zip(metrics)
                .map(|(id, metrics)| {
                    (
                        id,
                        metrics
                            .map(manager_resource_metrics)
                            .map_err(runtime_failed),
                    )
                })
                .collect(),
            Err(error) => {
                let message = error.to_string();
                ids.iter()
                    .cloned()
                    .map(|id| {
                        (
                            id,
                            Err(ManagerError::RuntimeFailed {
                                message: message.clone(),
                            }),
                        )
                    })
                    .collect()
            }
        }
    }

    fn create_sandbox(
        &self,
        request: &CreateSandboxRequest,
    ) -> Result<CreateSandboxResult, ManagerError> {
        let config = self.engine.config();
        let shared_base =
            request
                .shared_base
                .as_ref()
                .ok_or_else(|| ManagerError::RuntimeFailed {
                    message:
                        "shared base mount is required; create_sandbox must use host copy+hash"
                            .to_owned(),
                })?;
        validate_shared_base_source(shared_base)?;
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
        let limits =
            configured_resource_limits(config).ok_or_else(|| ManagerError::RuntimeFailed {
                message: format!(
                    "selected Docker resource profile `{}` is not configured",
                    config.resource_profile
                ),
            })?;
        let labels = build_labels(
            config,
            &id,
            &auth_token,
            &request.workspace_root,
            shared_base,
            &limits,
        );
        let image = resolve_image(config, &request.image);
        let shared_base_volume = self.ensure_shared_base_volume(config, &image, shared_base)?;
        let workspace_paths = runtime_workspace_paths(config)?;
        let volumes = runtime_volumes(config, &id, &workspace_paths);
        let volume_names = volumes
            .iter()
            .map(|volume| volume.name.clone())
            .collect::<Vec<_>>();
        let cmd = daemon_launch_argv(config, &record, &auth_token);
        let spec = ContainerSpec {
            name,
            image,
            cmd,
            env: container_env(config),
            labels,
            binds: vec![shared_base_volume_bind(&shared_base_volume, shared_base)],
            volumes,
            daemon_port: config.daemon_port,
            daemon_http_port: config.daemon_http_port,
            privileged: config.privileged,
            platform: config.platform.clone(),
            memory_high_bytes: limits.memory_high_bytes,
            memory_max_bytes: limits.memory_max_bytes,
            nano_cpus: limits.nano_cpus,
            pids_max: limits.pids_max,
        };
        self.engine.create_container(spec).map_err(runtime_failed)?;
        let archive = build_shared_base_seed_archive(
            &workspace_paths.layer_stack_root,
            &config.container_workspace_root,
            &shared_base.root_hash,
        )
        .map_err(|error| ManagerError::RuntimeFailed {
            message: format!("failed to build shared base seed archive: {error}"),
        });
        match archive.and_then(|archive| {
            self.engine
                .upload_archive(id.as_str().to_owned(), "/".to_owned(), archive)
                .map_err(runtime_failed)
        }) {
            Ok(()) => {}
            Err(error) => {
                let _ = self
                    .engine
                    .remove_container(id.as_str().to_owned())
                    .map_err(runtime_failed);
                for volume_name in volume_names {
                    let _ = self
                        .engine
                        .remove_volume(volume_name)
                        .map_err(runtime_failed);
                }
                return Err(error);
            }
        }
        Ok(CreateSandboxResult {
            id,
            resource_profile: Some(limits.into_manager_profile()),
        })
    }

    fn destroy_sandbox(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        self.engine
            .remove_container(record.id.as_str().to_owned())
            .map_err(runtime_failed)?;
        for volume_name in runtime_volume_names(&record.id) {
            self.engine
                .remove_volume(volume_name)
                .map_err(runtime_failed)?;
        }
        Ok(())
    }
}

fn configured_resource_limits(config: &DockerRuntimeConfig) -> Option<DockerResourceLimits> {
    let profile = config.selected_resource_profile()?;
    let memory_max_bytes = config.memory_bytes.unwrap_or(profile.memory_max_bytes);
    let memory_high_bytes = profile.memory_high_bytes.min(memory_max_bytes);
    Some(DockerResourceLimits {
        profile_name: config.resource_profile.clone(),
        nano_cpus: config.nano_cpus.unwrap_or(profile.nano_cpus),
        memory_high_bytes,
        memory_max_bytes,
        pids_max: profile.pids_max,
        workload_memory_high_bytes: memory_high_bytes,
        workload_memory_max_bytes: memory_high_bytes,
        workload_pids_max: profile.workload_pids_max(),
        control_plane_pids_reserve: profile.control_plane_pids_reserve(),
        daemon_runtime_profile: profile.daemon_runtime_profile.clone(),
        separate_workload_cgroup: profile.separate_workload_cgroup,
    })
}

struct RuntimeWorkspacePaths {
    layer_stack_root: PathBuf,
    scratch_root: PathBuf,
}

fn runtime_workspace_paths(
    config: &DockerRuntimeConfig,
) -> Result<RuntimeWorkspacePaths, ManagerError> {
    let document = sandbox_config::load_path(&config.daemon_config_yaml_path)
        .map_err(runtime_config_failed)?;
    let runtime = document
        .section::<RuntimeConfig>("runtime")
        .map_err(runtime_config_failed)?;
    runtime
        .validate()
        .map_err(|error| ManagerError::RuntimeFailed {
            message: format!("invalid daemon runtime config: {error}"),
        })?;
    Ok(RuntimeWorkspacePaths {
        layer_stack_root: runtime.workspace.layer_stack_root,
        scratch_root: runtime.workspace.scratch_root,
    })
}

fn workspace_scratch_volume_name(id: &SandboxId) -> String {
    format!("{}-workspace", id.as_str())
}

fn layer_stack_volume_name(id: &SandboxId) -> String {
    format!("{}-layer-stack", id.as_str())
}

fn shared_base_volume_name(root_hash: &str) -> String {
    format!("eos-shared-base-{root_hash}")
}

fn shared_base_volume_bind(volume_name: &str, shared_base: &SharedBaseMount) -> String {
    format!("{}:{}:ro", volume_name, shared_base.target.display())
}

fn runtime_volume_names(id: &SandboxId) -> Vec<String> {
    vec![
        layer_stack_volume_name(id),
        workspace_scratch_volume_name(id),
    ]
}

fn runtime_volumes(
    config: &DockerRuntimeConfig,
    id: &SandboxId,
    paths: &RuntimeWorkspacePaths,
) -> Vec<VolumeSpec> {
    let labels = build_volume_labels(config, id);
    vec![
        VolumeSpec {
            name: layer_stack_volume_name(id),
            target: paths.layer_stack_root.to_string_lossy().into_owned(),
            labels: labels.clone(),
        },
        VolumeSpec {
            name: workspace_scratch_volume_name(id),
            target: paths.scratch_root.to_string_lossy().into_owned(),
            labels,
        },
    ]
}

fn container_env(config: &DockerRuntimeConfig) -> Vec<String> {
    config
        .container_env
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
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

fn validate_shared_base_source(shared_base: &SharedBaseMount) -> Result<(), ManagerError> {
    if !shared_base.readonly {
        return Err(ManagerError::RuntimeFailed {
            message: "shared base mount must be read-only".to_owned(),
        });
    }
    match std::fs::metadata(&shared_base.source) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        _ => Err(ManagerError::RuntimeFailed {
            message: format!(
                "shared base source {} must be an existing host directory",
                shared_base.source.display()
            ),
        }),
    }
}

fn build_labels(
    config: &DockerRuntimeConfig,
    id: &SandboxId,
    auth_token: &str,
    host_workspace_root: &Path,
    shared_base: &SharedBaseMount,
    limits: &DockerResourceLimits,
) -> HashMap<String, String> {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let mut label_map = HashMap::from([
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
    ]);
    label_map.insert(
        labels::SHARED_BASE_SOURCE.to_owned(),
        shared_base.source.to_string_lossy().into_owned(),
    );
    label_map.insert(
        labels::SHARED_BASE_TARGET.to_owned(),
        shared_base.target.to_string_lossy().into_owned(),
    );
    label_map.insert(
        labels::SHARED_BASE_ROOT_HASH.to_owned(),
        shared_base.root_hash.clone(),
    );
    label_map.insert(
        labels::SHARED_BASE_READONLY.to_owned(),
        shared_base.readonly.to_string(),
    );
    label_map.extend([
        (
            labels::RESOURCE_PROFILE.to_owned(),
            limits.profile_name.clone(),
        ),
        (
            labels::RESOURCE_NANO_CPUS.to_owned(),
            limits.nano_cpus.to_string(),
        ),
        (
            labels::RESOURCE_MEMORY_HIGH_BYTES.to_owned(),
            limits.memory_high_bytes.to_string(),
        ),
        (
            labels::RESOURCE_MEMORY_MAX_BYTES.to_owned(),
            limits.memory_max_bytes.to_string(),
        ),
        (
            labels::RESOURCE_PIDS_MAX.to_owned(),
            limits.pids_max.to_string(),
        ),
        (
            labels::RESOURCE_WORKLOAD_MEMORY_HIGH_BYTES.to_owned(),
            limits.workload_memory_high_bytes.to_string(),
        ),
        (
            labels::RESOURCE_WORKLOAD_MEMORY_MAX_BYTES.to_owned(),
            limits.workload_memory_max_bytes.to_string(),
        ),
        (
            labels::RESOURCE_WORKLOAD_PIDS_MAX.to_owned(),
            limits.workload_pids_max.to_string(),
        ),
        (
            labels::RESOURCE_CONTROL_PLANE_PIDS_RESERVE.to_owned(),
            limits.control_plane_pids_reserve.to_string(),
        ),
        (
            labels::RESOURCE_DAEMON_RUNTIME_PROFILE.to_owned(),
            limits.daemon_runtime_profile.clone(),
        ),
        (
            labels::RESOURCE_SEPARATE_WORKLOAD_CGROUP.to_owned(),
            limits.separate_workload_cgroup.to_string(),
        ),
    ]);
    label_map
}

fn recovered_shared_base(container: &crate::engine::RecoveredContainer) -> Option<SharedBaseMount> {
    Some(SharedBaseMount {
        source: PathBuf::from(container.shared_base_source.clone()?),
        target: PathBuf::from(container.shared_base_target.clone()?),
        root_hash: container.shared_base_root_hash.clone()?,
        readonly: container.shared_base_readonly?,
    })
}

fn build_volume_labels(config: &DockerRuntimeConfig, id: &SandboxId) -> HashMap<String, String> {
    HashMap::from([
        (labels::SANDBOX_ID.to_owned(), id.as_str().to_owned()),
        (
            labels::GATEWAY_INSTANCE_ID.to_owned(),
            config.gateway_instance_id.clone(),
        ),
        (
            labels::CLEANUP_POLICY.to_owned(),
            labels::CLEANUP_POLICY_REMOVE_ON_DESTROY.to_owned(),
        ),
    ])
}

fn build_shared_base_volume_labels(
    config: &DockerRuntimeConfig,
    shared_base: &SharedBaseMount,
) -> HashMap<String, String> {
    HashMap::from([
        (
            labels::GATEWAY_INSTANCE_ID.to_owned(),
            config.gateway_instance_id.clone(),
        ),
        (
            labels::SHARED_BASE_ROOT_HASH.to_owned(),
            shared_base.root_hash.clone(),
        ),
        (
            labels::SHARED_BASE_TARGET.to_owned(),
            shared_base.target.to_string_lossy().into_owned(),
        ),
        (
            labels::SHARED_BASE_READONLY.to_owned(),
            shared_base.readonly.to_string(),
        ),
    ])
}

fn runtime_failed(error: DockerError) -> ManagerError {
    ManagerError::RuntimeFailed {
        message: error.to_string(),
    }
}

fn manager_resource_metrics(
    metrics: crate::engine::ContainerResourceMetrics,
) -> SandboxResourceMetrics {
    SandboxResourceMetrics {
        cpu_usage_usec: metrics.cpu_usage_usec,
        memory_current_bytes: metrics.memory_current_bytes,
        memory_limit_bytes: metrics.memory_limit_bytes,
        io_read_bytes: metrics.io_read_bytes,
        io_write_bytes: metrics.io_write_bytes,
    }
}

fn runtime_config_failed(error: sandbox_config::ConfigError) -> ManagerError {
    ManagerError::RuntimeFailed {
        message: format!("failed to load daemon runtime config: {error}"),
    }
}
