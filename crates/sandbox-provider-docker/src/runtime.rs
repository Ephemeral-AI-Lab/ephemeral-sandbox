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
    SandboxHttpEndpoint, SandboxId, SandboxRecord, SandboxRuntime, SandboxState, SharedBaseMount,
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
            .list_recoverable(
                config.gateway_instance_id.clone(),
                config.daemon_port,
                config.daemon_http_port,
            )
            .map_err(runtime_failed)?;
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
                daemon: Some(endpoint),
                daemon_http: Some(SandboxHttpEndpoint::new(
                    ENDPOINT_HOST,
                    container.published_http_port,
                )),
                shared_base,
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
        let labels = build_labels(
            config,
            &id,
            &auth_token,
            &request.workspace_root,
            shared_base,
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
            memory_bytes: config.memory_bytes,
            nano_cpus: config.nano_cpus,
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
        Ok(CreateSandboxResult { id })
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

fn runtime_config_failed(error: sandbox_config::ConfigError) -> ManagerError {
    ManagerError::RuntimeFailed {
        message: format!("failed to load daemon runtime config: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn workspace_scratch_volume_name_is_derived_from_sandbox_id() {
        let id = SandboxId::new("eos-abc").expect("valid id");

        assert_eq!(workspace_scratch_volume_name(&id), "eos-abc-workspace");
    }

    #[test]
    fn layer_stack_volume_name_is_derived_from_sandbox_id() {
        let id = SandboxId::new("eos-abc").expect("valid id");

        assert_eq!(layer_stack_volume_name(&id), "eos-abc-layer-stack");
    }

    #[test]
    fn shared_base_volume_name_is_derived_from_root_hash() {
        assert_eq!(shared_base_volume_name("abc123"), "eos-shared-base-abc123");
    }

    #[test]
    fn shared_base_volume_bind_is_read_only_at_target() {
        let shared_base = SharedBaseMount {
            source: PathBuf::from("/host/base"),
            target: PathBuf::from("/eos/layer-stack/base"),
            root_hash: "abc123".to_owned(),
            readonly: true,
        };

        assert_eq!(
            shared_base_volume_bind("eos-shared-base-abc123", &shared_base),
            "eos-shared-base-abc123:/eos/layer-stack/base:ro"
        );
    }

    #[test]
    fn runtime_volumes_mount_layer_stack_and_workspace_scratch_roots() {
        let config = DockerRuntimeConfig {
            gateway_instance_id: "gateway-1".to_owned(),
            ..DockerRuntimeConfig::default()
        };
        let id = SandboxId::new("eos-abc").expect("valid id");
        let paths = RuntimeWorkspacePaths {
            layer_stack_root: PathBuf::from("/eos/layer-stack"),
            scratch_root: PathBuf::from("/eos/workspace"),
        };

        let volumes = runtime_volumes(&config, &id, &paths);

        assert_eq!(volumes.len(), 2);
        assert_eq!(volumes[0].name, "eos-abc-layer-stack");
        assert_eq!(volumes[0].target, "/eos/layer-stack");
        assert_eq!(volumes[1].name, "eos-abc-workspace");
        assert_eq!(volumes[1].target, "/eos/workspace");
    }

    #[test]
    fn workspace_scratch_volume_labels_do_not_include_auth_token() {
        let config = DockerRuntimeConfig {
            gateway_instance_id: "gateway-1".to_owned(),
            ..DockerRuntimeConfig::default()
        };
        let id = SandboxId::new("eos-abc").expect("valid id");

        let labels = build_volume_labels(&config, &id);

        assert_eq!(
            labels.get(crate::labels::SANDBOX_ID),
            Some(&"eos-abc".to_owned())
        );
        assert_eq!(
            labels.get(crate::labels::GATEWAY_INSTANCE_ID),
            Some(&"gateway-1".to_owned())
        );
        assert!(!labels.contains_key(crate::labels::AUTH_TOKEN));
    }

    #[test]
    fn shared_base_volume_labels_are_not_per_sandbox_cleanup_labels() {
        let config = DockerRuntimeConfig {
            gateway_instance_id: "gateway-1".to_owned(),
            ..DockerRuntimeConfig::default()
        };
        let shared_base = SharedBaseMount {
            source: PathBuf::from("/host/base"),
            target: PathBuf::from("/eos/layer-stack/base"),
            root_hash: "abc123".to_owned(),
            readonly: true,
        };

        let labels = build_shared_base_volume_labels(&config, &shared_base);

        assert_eq!(
            labels.get(crate::labels::GATEWAY_INSTANCE_ID),
            Some(&"gateway-1".to_owned())
        );
        assert_eq!(
            labels.get(crate::labels::SHARED_BASE_ROOT_HASH),
            Some(&"abc123".to_owned())
        );
        assert!(!labels.contains_key(crate::labels::SANDBOX_ID));
        assert!(!labels.contains_key(crate::labels::CLEANUP_POLICY));
    }

    #[test]
    fn runtime_workspace_paths_read_daemon_runtime_config() {
        let path = temp_config_path();
        fs::write(
            &path,
            r#"
runtime:
  workspace:
    layer_stack_root: /eos/layer-stack
    scratch_root: /custom/workspace
    setup_timeout_s: 30
    exit_grace_s: 0.25
    rfc1918_egress: allow
  namespace_execution:
    scratch_root: /eos/namespace_execution
"#,
        )
        .expect("write config");
        let config = DockerRuntimeConfig {
            daemon_config_yaml_path: path.clone(),
            ..DockerRuntimeConfig::default()
        };

        let paths = runtime_workspace_paths(&config).expect("runtime paths load");

        assert_eq!(paths.layer_stack_root, PathBuf::from("/eos/layer-stack"));
        assert_eq!(paths.scratch_root, PathBuf::from("/custom/workspace"));
        let _ = fs::remove_file(path);
    }

    fn temp_config_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "eos-docker-runtime-config-{}.yml",
            unique_test_suffix()
        ))
    }

    fn unique_test_suffix() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        format!("{nanos}-{}", std::process::id())
    }
}
