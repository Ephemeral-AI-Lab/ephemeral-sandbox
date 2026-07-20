//! Bollard client wrapper plus the async→sync bridge (§4.9). Ordinary operations
//! use a fresh thread, current-thread Tokio runtime, and Bollard client. The hot
//! resource-metrics batch path lazily retains one executor so periodic sampling
//! does not rebuild that stack or cross a worker-thread channel every cycle.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions, StatsOptions, StopContainerOptions, UploadToContainerOptions,
};
use bollard::image::ListImagesOptions;
use bollard::models::{
    ContainerInspectResponse, ContainerState, ContainerStateStatusEnum, ContainerSummary,
    HostConfig, HostConfigCgroupnsModeEnum, PortBinding,
};
use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
use bollard::Docker;
use bytes::Bytes;
use futures_util::{future::join_all, StreamExt as _};

use sandbox_config::configs::manager::DockerRuntimeConfig;
use sandbox_manager::SandboxResourceProfile;

use crate::labels;

const HTTP_NOT_FOUND: u16 = 404;
const HTTP_NOT_MODIFIED: u16 = 304;
const LOG_CAPTURE_TAIL: &str = "200";
const LOG_CAPTURE_CAP_BYTES: usize = 8192;

/// Capabilities the de-privileged sandbox container grants the daemon's setup
/// paths: `SYS_ADMIN` for namespace/overlay/mount setup and `NET_ADMIN` for
/// bridge/veth provisioning. Docker's default seccomp profile stays active and
/// gates the corresponding syscalls on these capabilities.
const DEPRIVILEGED_CAPABILITIES: &[&str] = &["SYS_ADMIN", "NET_ADMIN"];
const NO_NEW_PRIVILEGES_SECURITY_OPT: &str = "no-new-privileges";

#[derive(Debug, thiserror::Error)]
pub(crate) enum DockerError {
    #[error("docker connection failed: {0}")]
    Connect(String),
    #[error("docker api error: {0}")]
    Api(String),
}

/// Owned, `Send + 'static` request to create a stopped container.
pub(crate) struct ContainerSpec {
    pub(crate) name: String,
    pub(crate) image: String,
    pub(crate) cmd: Vec<String>,
    pub(crate) env: Vec<String>,
    pub(crate) labels: HashMap<String, String>,
    pub(crate) binds: Vec<String>,
    pub(crate) volumes: Vec<VolumeSpec>,
    pub(crate) daemon_port: u16,
    pub(crate) daemon_http_port: u16,
    pub(crate) privileged: bool,
    pub(crate) platform: Option<String>,
    pub(crate) memory_high_bytes: i64,
    pub(crate) memory_max_bytes: i64,
    pub(crate) nano_cpus: i64,
    pub(crate) pids_max: i64,
}

pub(crate) struct VolumeSpec {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) labels: HashMap<String, String>,
}

/// Cumulative counters obtained from Docker's read-only container stats API.
/// Optional counters preserve Docker's distinction between unavailable data and
/// an observed zero.
pub(crate) struct ContainerResourceMetrics {
    pub(crate) cpu_usage_usec: Option<u64>,
    pub(crate) memory_current_bytes: Option<u64>,
    pub(crate) memory_limit_bytes: Option<u64>,
    pub(crate) io_read_bytes: Option<u64>,
    pub(crate) io_write_bytes: Option<u64>,
}

type ResourceMetricsBatch = Vec<Result<ContainerResourceMetrics, DockerError>>;

struct ResourceMetricsExecutor {
    runtime: tokio::runtime::Runtime,
    docker: Docker,
}

impl ResourceMetricsExecutor {
    fn start(endpoint: Option<String>, connect_timeout_s: u64) -> Result<Self, DockerError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                DockerError::Connect(format!(
                    "failed to build docker resource metrics runtime: {error}"
                ))
            })?;
        let docker = connect(endpoint.as_deref(), connect_timeout_s)?;
        Ok(Self { runtime, docker })
    }

    fn read(&self, containers: Vec<String>) -> Result<ResourceMetricsBatch, DockerError> {
        Ok(self.runtime.block_on(read_container_resource_metrics_batch(
            &self.docker,
            containers,
        )))
    }

    #[cfg(test)]
    fn identity(&self) -> usize {
        std::ptr::from_ref(self).addr()
    }
}

/// Result of starting a container and resolving its published daemon ports (the
/// JSON-line RPC port and the HTTP-surface port).
pub(crate) struct StartedContainer {
    pub(crate) port: u16,
    pub(crate) http_port: u16,
    pub(crate) auth_token: String,
}

/// A container reconstructed from labels + published ports during recovery.
pub(crate) struct RecoveredContainer {
    pub(crate) sandbox_id: String,
    pub(crate) host_workspace_root: String,
    pub(crate) shared_base_source: Option<String>,
    pub(crate) shared_base_target: Option<String>,
    pub(crate) shared_base_root_hash: Option<String>,
    pub(crate) shared_base_readonly: Option<bool>,
    pub(crate) auth_token: String,
    pub(crate) published_port: u16,
    pub(crate) published_http_port: u16,
    pub(crate) resource_profile: Option<SandboxResourceProfile>,
}

pub(crate) struct DockerEngine {
    config: DockerRuntimeConfig,
    resource_metrics_executor: Mutex<Option<ResourceMetricsExecutor>>,
}

impl DockerEngine {
    pub(crate) fn new(config: DockerRuntimeConfig) -> Self {
        Self {
            config,
            resource_metrics_executor: Mutex::new(None),
        }
    }

    pub(crate) fn config(&self) -> &DockerRuntimeConfig {
        &self.config
    }

    pub(crate) fn list_images(&self) -> Result<Vec<String>, DockerError> {
        self.run_blocking(move |docker| async move {
            let images = docker
                .list_images(Some(ListImagesOptions::<String> {
                    all: true,
                    ..Default::default()
                }))
                .await
                .map_err(|error| DockerError::Api(format!("list_images: {error}")))?;
            let mut references = Vec::new();
            for image in images {
                let tags = image
                    .repo_tags
                    .into_iter()
                    .filter(|tag| tag != "<none>:<none>")
                    .collect::<Vec<_>>();
                if tags.is_empty() {
                    if !image.id.is_empty() {
                        references.push(image.id);
                    }
                } else {
                    references.extend(tags);
                }
            }
            references.sort();
            references.dedup();
            Ok(references)
        })
    }

    fn run_blocking<T, F, Fut>(&self, op: F) -> Result<T, DockerError>
    where
        T: Send + 'static,
        F: FnOnce(Docker) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, DockerError>>,
    {
        let endpoint = self.config.docker_endpoint.clone();
        let connect_timeout_s = self.config.connect_timeout_s;
        let worker = std::thread::Builder::new()
            .name("docker-engine".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| {
                        DockerError::Connect(format!("failed to build docker runtime: {error}"))
                    })?;
                runtime.block_on(async move {
                    let docker = connect(endpoint.as_deref(), connect_timeout_s)?;
                    op(docker).await
                })
            })
            .map_err(|error| {
                DockerError::Connect(format!("failed to spawn docker worker: {error}"))
            })?;
        worker
            .join()
            .map_err(|_| DockerError::Api("docker worker thread panicked".to_owned()))?
    }

    pub(crate) fn create_container(&self, spec: ContainerSpec) -> Result<(), DockerError> {
        self.run_blocking(move |docker| async move {
            let (exposed_ports, port_bindings) =
                publish_loopback_ports(&[spec.daemon_port, spec.daemon_http_port]);
            for volume in &spec.volumes {
                create_volume(&docker, volume).await?;
            }
            let mut binds = spec
                .volumes
                .iter()
                .map(|volume| format!("{}:{}", volume.name, volume.target))
                .collect::<Vec<_>>();
            binds.extend(spec.binds);
            let host_config = HostConfig {
                binds: Some(binds),
                port_bindings: Some(port_bindings),
                privileged: Some(spec.privileged),
                cap_add: deprivileged_cap_add(spec.privileged),
                security_opt: deprivileged_security_opt(spec.privileged),
                cgroupns_mode: Some(HostConfigCgroupnsModeEnum::PRIVATE),
                init: Some(true),
                memory_reservation: Some(spec.memory_high_bytes),
                memory: Some(spec.memory_max_bytes),
                nano_cpus: Some(spec.nano_cpus),
                pids_limit: Some(spec.pids_max),
                ..Default::default()
            };
            let config = Config {
                image: Some(spec.image),
                cmd: Some(spec.cmd),
                env: if spec.env.is_empty() {
                    None
                } else {
                    Some(spec.env)
                },
                labels: Some(spec.labels),
                exposed_ports: Some(exposed_ports),
                host_config: Some(host_config),
                ..Default::default()
            };
            let options = CreateContainerOptions {
                name: spec.name,
                platform: spec.platform,
            };
            match docker.create_container(Some(options), config).await {
                Ok(_) => Ok(()),
                Err(error) => {
                    for volume in &spec.volumes {
                        let _ = remove_volume(&docker, &volume.name).await;
                    }
                    Err(DockerError::Api(format!("create_container: {error}")))
                }
            }
        })
    }

    pub(crate) fn upload_archive(
        &self,
        container: String,
        dest_path: String,
        archive: Bytes,
    ) -> Result<(), DockerError> {
        self.run_blocking(move |docker| async move {
            let options = UploadToContainerOptions {
                path: dest_path,
                ..Default::default()
            };
            docker
                .upload_to_container(&container, Some(options), archive)
                .await
                .map_err(|error| DockerError::Api(format!("upload_to_container: {error}")))
        })
    }

    pub(crate) fn container_resource_metrics(
        &self,
        container: String,
    ) -> Result<ContainerResourceMetrics, DockerError> {
        self.run_blocking(move |docker| async move {
            read_container_resource_metrics(&docker, &container).await
        })
    }

    pub(crate) fn container_resource_metrics_batch(
        &self,
        containers: Vec<String>,
    ) -> Result<Vec<Result<ContainerResourceMetrics, DockerError>>, DockerError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return self.run_blocking(move |docker| async move {
                Ok(read_container_resource_metrics_batch(&docker, containers).await)
            });
        }

        let mut executor = self.resource_metrics_executor.lock().map_err(|_| {
            DockerError::Api("docker resource metrics executor lock poisoned".to_owned())
        })?;
        if executor.is_none() {
            *executor = Some(ResourceMetricsExecutor::start(
                self.config.docker_endpoint.clone(),
                self.config.connect_timeout_s,
            )?);
        }
        executor
            .as_ref()
            .expect("resource metrics executor initialized")
            .read(containers)
    }

    pub(crate) fn seed_volume_from_archive(
        &self,
        image: String,
        volume: VolumeSpec,
        archive: Bytes,
    ) -> Result<(), DockerError> {
        self.run_blocking(move |docker| async move {
            create_volume(&docker, &volume).await?;
            let name = format!("{}-seed-{}", volume.name, uuid::Uuid::new_v4());
            let host_config = HostConfig {
                binds: Some(vec![format!("{}:{}", volume.name, volume.target)]),
                ..Default::default()
            };
            let config = Config {
                image: Some(image),
                cmd: Some(vec!["true".to_owned()]),
                host_config: Some(host_config),
                ..Default::default()
            };
            let options = CreateContainerOptions {
                name: name.clone(),
                platform: None,
            };
            let seeded = async {
                docker
                    .create_container(Some(options), config)
                    .await
                    .map_err(|error| DockerError::Api(format!("create seed container: {error}")))?;
                let uploaded = docker
                    .upload_to_container(
                        &name,
                        Some(UploadToContainerOptions {
                            path: "/".to_owned(),
                            ..Default::default()
                        }),
                        archive,
                    )
                    .await
                    .map_err(|error| DockerError::Api(format!("seed volume archive: {error}")));
                let removed = docker
                    .remove_container(
                        &name,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: false,
                            ..Default::default()
                        }),
                    )
                    .await
                    .map_err(|error| DockerError::Api(format!("remove seed container: {error}")));
                uploaded?;
                removed
            }
            .await;
            if seeded.is_err() {
                let _ = remove_volume(&docker, &volume.name).await;
            }
            seeded
        })
    }

    pub(crate) fn start_and_resolve(
        &self,
        container: String,
        daemon_port: u16,
        daemon_http_port: u16,
    ) -> Result<StartedContainer, DockerError> {
        let publish_attempts = self.config.port_publish_attempts;
        let publish_retry_delay =
            std::time::Duration::from_millis(self.config.port_publish_retry_delay_ms);
        self.run_blocking(move |docker| async move {
            docker
                .start_container(&container, None::<StartContainerOptions<String>>)
                .await
                .map_err(|error| DockerError::Api(format!("start_container: {error}")))?;
            let inspected = inspect_until_ports_published(
                &docker,
                &container,
                &[daemon_port, daemon_http_port],
                publish_attempts,
                publish_retry_delay,
            )
            .await?;
            let port = published_port(&inspected, daemon_port)?;
            let http_port = published_port(&inspected, daemon_http_port)?;
            let auth_token = label_value(&inspected, labels::AUTH_TOKEN)?;
            Ok(StartedContainer {
                port,
                http_port,
                auth_token,
            })
        })
    }

    pub(crate) fn stop_container(
        &self,
        container: String,
        timeout_secs: i64,
    ) -> Result<(), DockerError> {
        self.run_blocking(move |docker| async move {
            match docker
                .stop_container(&container, Some(StopContainerOptions { t: timeout_secs }))
                .await
            {
                Ok(()) => Ok(()),
                Err(error) if server_status(&error) == Some(HTTP_NOT_FOUND) => Ok(()),
                Err(error) if server_status(&error) == Some(HTTP_NOT_MODIFIED) => Ok(()),
                Err(error) => Err(DockerError::Api(format!("stop_container: {error}"))),
            }
        })
    }

    pub(crate) fn remove_container(&self, container: String) -> Result<(), DockerError> {
        self.run_blocking(move |docker| async move {
            let options = RemoveContainerOptions {
                force: true,
                v: true,
                ..Default::default()
            };
            match docker.remove_container(&container, Some(options)).await {
                Ok(()) => Ok(()),
                Err(error) if server_status(&error) == Some(HTTP_NOT_FOUND) => Ok(()),
                Err(error) => Err(DockerError::Api(format!("remove_container: {error}"))),
            }
        })
    }

    pub(crate) fn remove_volume(&self, volume: String) -> Result<(), DockerError> {
        self.run_blocking(move |docker| async move { remove_volume(&docker, &volume).await })
    }

    pub(crate) fn volume_exists(&self, volume: String) -> Result<bool, DockerError> {
        self.run_blocking(move |docker| async move {
            match docker.inspect_volume(&volume).await {
                Ok(_) => Ok(true),
                Err(error) if server_status(&error) == Some(HTTP_NOT_FOUND) => Ok(false),
                Err(error) => Err(DockerError::Api(format!("inspect_volume: {error}"))),
            }
        })
    }

    pub(crate) fn list_recoverable(
        &self,
        gateway_instance_id: String,
        daemon_port: u16,
        daemon_http_port: u16,
    ) -> Result<Vec<RecoveredContainer>, DockerError> {
        self.run_blocking(move |docker| async move {
            let mut filters = HashMap::new();
            filters.insert(
                "label".to_owned(),
                vec![format!(
                    "{}={}",
                    labels::GATEWAY_INSTANCE_ID,
                    gateway_instance_id
                )],
            );
            let options = ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            };
            let summaries = docker
                .list_containers(Some(options))
                .await
                .map_err(|error| DockerError::Api(format!("list_containers: {error}")))?;
            Ok(summaries
                .iter()
                .filter_map(|summary| {
                    recovered_from_summary(summary, daemon_port, daemon_http_port)
                })
                .collect())
        })
    }

    /// Best-effort capture of container `State` + log tail for failure
    /// diagnostics. Returns an empty string rather than erroring.
    pub(crate) fn capture_failure_context(&self, container: String) -> String {
        self.run_blocking(move |docker| async move {
            let mut context = String::new();
            if let Ok(inspected) = docker.inspect_container(&container, None).await {
                if let Some(state) = inspected.state {
                    context.push_str(&format!(
                        "state={:?} running={:?} exit_code={:?} error={:?}",
                        state.status, state.running, state.exit_code, state.error
                    ));
                }
            }
            let logs = collect_logs(&docker, &container).await;
            let logs = logs_for_context(&logs);
            if !logs.is_empty() {
                context.push_str(&format!("; logs: {logs}"));
            }
            Ok(context)
        })
        .unwrap_or_default()
    }

    pub(crate) fn capture_logs(&self, container: String) -> String {
        self.run_blocking(move |docker| async move { Ok(collect_logs(&docker, &container).await) })
            .unwrap_or_default()
    }

    pub(crate) fn container_exit_reason(
        &self,
        container: String,
    ) -> Result<Option<String>, DockerError> {
        self.run_blocking(move |docker| async move {
            let inspected = docker
                .inspect_container(&container, None)
                .await
                .map_err(|error| DockerError::Api(format!("inspect_container: {error}")))?;
            Ok(inspected.state.as_ref().and_then(container_exit_reason))
        })
    }
}

async fn read_container_resource_metrics(
    docker: &Docker,
    container: &str,
) -> Result<ContainerResourceMetrics, DockerError> {
    let mut stats = docker.stats(
        container,
        Some(StatsOptions {
            stream: false,
            one_shot: true,
        }),
    );
    let stats = match stats.next().await {
        Some(Ok(stats)) => stats,
        Some(Err(error)) => {
            return Err(DockerError::Api(format!("container stats: {error}")));
        }
        None => {
            return Err(DockerError::Api(
                "container stats returned no response".to_owned(),
            ));
        }
    };
    Ok(container_resource_metrics(&stats))
}

async fn read_container_resource_metrics_batch(
    docker: &Docker,
    containers: Vec<String>,
) -> ResourceMetricsBatch {
    let reads = containers.into_iter().map(|container| {
        let docker = docker.clone();
        async move { read_container_resource_metrics(&docker, &container).await }
    });
    join_all(reads).await
}

fn container_resource_metrics(stats: &bollard::container::Stats) -> ContainerResourceMetrics {
    let (io_read_bytes, io_write_bytes) = block_io_totals(stats);
    ContainerResourceMetrics {
        // Docker's total CPU usage counter is nanoseconds; the observability
        // contract uses microseconds to match cgroup v2's cpu.stat usage_usec.
        cpu_usage_usec: Some(stats.cpu_stats.cpu_usage.total_usage / 1_000),
        memory_current_bytes: stats.memory_stats.usage,
        memory_limit_bytes: stats.memory_stats.limit,
        io_read_bytes,
        io_write_bytes,
    }
}

fn block_io_totals(stats: &bollard::container::Stats) -> (Option<u64>, Option<u64>) {
    let mut reads = 0_u64;
    let mut writes = 0_u64;
    let primary_available =
        if let Some(entries) = stats.blkio_stats.io_service_bytes_recursive.as_deref() {
            for entry in entries {
                match entry.op.to_ascii_lowercase().as_str() {
                    "read" => reads = reads.saturating_add(entry.value),
                    "write" => writes = writes.saturating_add(entry.value),
                    _ => {}
                }
            }
            true
        } else {
            false
        };
    if reads != 0 || writes != 0 {
        (Some(reads), Some(writes))
    } else if stats.storage_stats.read_size_bytes.is_some()
        || stats.storage_stats.write_size_bytes.is_some()
    {
        (
            stats.storage_stats.read_size_bytes,
            stats.storage_stats.write_size_bytes,
        )
    } else if primary_available {
        (Some(0), Some(0))
    } else {
        (None, None)
    }
}

fn deprivileged_cap_add(privileged: bool) -> Option<Vec<String>> {
    (!privileged).then(|| {
        DEPRIVILEGED_CAPABILITIES
            .iter()
            .map(ToString::to_string)
            .collect()
    })
}

fn deprivileged_security_opt(privileged: bool) -> Option<Vec<String>> {
    (!privileged).then(|| vec![NO_NEW_PRIVILEGES_SECURITY_OPT.to_owned()])
}

async fn create_volume(docker: &Docker, volume: &VolumeSpec) -> Result<(), DockerError> {
    let options = CreateVolumeOptions {
        name: volume.name.clone(),
        labels: volume.labels.clone(),
        ..Default::default()
    };
    docker
        .create_volume(options)
        .await
        .map(|_| ())
        .map_err(|error| DockerError::Api(format!("create_volume: {error}")))
}

async fn remove_volume(docker: &Docker, volume: &str) -> Result<(), DockerError> {
    let options = RemoveVolumeOptions { force: true };
    match docker.remove_volume(volume, Some(options)).await {
        Ok(()) => Ok(()),
        Err(error) if server_status(&error) == Some(HTTP_NOT_FOUND) => Ok(()),
        Err(error) => Err(DockerError::Api(format!("remove_volume: {error}"))),
    }
}

fn connect(endpoint: Option<&str>, connect_timeout_s: u64) -> Result<Docker, DockerError> {
    let docker = match endpoint {
        Some(value) if value.starts_with("http://") || value.starts_with("tcp://") => {
            Docker::connect_with_http(value, connect_timeout_s, bollard::API_DEFAULT_VERSION)
        }
        Some(value) => {
            Docker::connect_with_socket(value, connect_timeout_s, bollard::API_DEFAULT_VERSION)
        }
        None => Docker::connect_with_local_defaults(),
    };
    docker.map_err(|error| DockerError::Connect(error.to_string()))
}

/// Inspect the container until every requested port has a published host
/// binding. Docker can report an empty port map for a brief window right
/// after `start_container`; retry within a small bounded budget and return
/// the last inspection so the caller surfaces the ordinary
/// `no published host port` error if the binding never appears.
async fn inspect_until_ports_published(
    docker: &Docker,
    container: &str,
    ports: &[u16],
    attempts: u32,
    retry_delay: std::time::Duration,
) -> Result<ContainerInspectResponse, DockerError> {
    let mut inspected = inspect_container(docker, container).await?;
    for _ in 0..attempts {
        if ports
            .iter()
            .all(|port| published_port(&inspected, *port).is_ok())
        {
            break;
        }
        tokio::time::sleep(retry_delay).await;
        inspected = inspect_container(docker, container).await?;
    }
    Ok(inspected)
}

async fn inspect_container(
    docker: &Docker,
    container: &str,
) -> Result<ContainerInspectResponse, DockerError> {
    docker
        .inspect_container(container, None)
        .await
        .map_err(|error| DockerError::Api(format!("inspect_container: {error}")))
}

fn published_port(
    inspected: &ContainerInspectResponse,
    daemon_port: u16,
) -> Result<u16, DockerError> {
    let key = format!("{daemon_port}/tcp");
    let host_port = inspected
        .network_settings
        .as_ref()
        .and_then(|settings| settings.ports.as_ref())
        .and_then(|ports| ports.get(&key))
        .and_then(|bindings| bindings.as_ref())
        .and_then(|bindings| bindings.first())
        .and_then(|binding| binding.host_port.as_ref())
        .ok_or_else(|| DockerError::Api(format!("no published host port for {key}")))?;
    host_port.parse::<u16>().map_err(|error| {
        DockerError::Api(format!("invalid published host port {host_port}: {error}"))
    })
}

fn label_value(inspected: &ContainerInspectResponse, key: &str) -> Result<String, DockerError> {
    inspected
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| labels.get(key))
        .cloned()
        .ok_or_else(|| DockerError::Api(format!("container is missing label {key}")))
}

fn recovered_from_summary(
    summary: &ContainerSummary,
    daemon_port: u16,
    daemon_http_port: u16,
) -> Option<RecoveredContainer> {
    let labels = summary.labels.as_ref()?;
    let sandbox_id = labels.get(labels::SANDBOX_ID)?.clone();
    let host_workspace_root = labels.get(labels::HOST_WORKSPACE_ROOT)?.clone();
    let shared_base_source = labels.get(labels::SHARED_BASE_SOURCE).cloned();
    let shared_base_target = labels.get(labels::SHARED_BASE_TARGET).cloned();
    let shared_base_root_hash = labels.get(labels::SHARED_BASE_ROOT_HASH).cloned();
    let shared_base_readonly = labels
        .get(labels::SHARED_BASE_READONLY)
        .and_then(|value| value.parse::<bool>().ok());
    let auth_token = labels.get(labels::AUTH_TOKEN)?.clone();
    let resource_profile = recovered_resource_profile(labels);
    let ports = summary.ports.as_ref()?;
    let published_port = published_summary_port(ports, daemon_port)?;
    let published_http_port = published_summary_port(ports, daemon_http_port)?;
    Some(RecoveredContainer {
        sandbox_id,
        host_workspace_root,
        shared_base_source,
        shared_base_target,
        shared_base_root_hash,
        shared_base_readonly,
        auth_token,
        published_port,
        published_http_port,
        resource_profile,
    })
}

fn recovered_resource_profile(labels: &HashMap<String, String>) -> Option<SandboxResourceProfile> {
    Some(SandboxResourceProfile {
        name: labels.get(labels::RESOURCE_PROFILE)?.clone(),
        nano_cpus: labels.get(labels::RESOURCE_NANO_CPUS)?.parse().ok()?,
        memory_high_bytes: labels
            .get(labels::RESOURCE_MEMORY_HIGH_BYTES)?
            .parse()
            .ok()?,
        memory_max_bytes: labels
            .get(labels::RESOURCE_MEMORY_MAX_BYTES)?
            .parse()
            .ok()?,
        pids_max: labels.get(labels::RESOURCE_PIDS_MAX)?.parse().ok()?,
        workload_memory_high_bytes: labels
            .get(labels::RESOURCE_WORKLOAD_MEMORY_HIGH_BYTES)?
            .parse()
            .ok()?,
        workload_memory_max_bytes: labels
            .get(labels::RESOURCE_WORKLOAD_MEMORY_MAX_BYTES)?
            .parse()
            .ok()?,
        workload_pids_max: labels
            .get(labels::RESOURCE_WORKLOAD_PIDS_MAX)?
            .parse()
            .ok()?,
        control_plane_pids_reserve: labels
            .get(labels::RESOURCE_CONTROL_PLANE_PIDS_RESERVE)?
            .parse()
            .ok()?,
        daemon_runtime_profile: labels.get(labels::RESOURCE_DAEMON_RUNTIME_PROFILE)?.clone(),
        separate_workload_cgroup: labels
            .get(labels::RESOURCE_SEPARATE_WORKLOAD_CGROUP)?
            .parse()
            .ok()?,
    })
}

fn published_summary_port(ports: &[bollard::models::Port], private_port: u16) -> Option<u16> {
    ports
        .iter()
        .find(|port| port.private_port == private_port)
        .and_then(|port| port.public_port)
}

type ExposedPorts = HashMap<String, HashMap<(), ()>>;
type PortBindings = HashMap<String, Option<Vec<PortBinding>>>;

/// Expose each container port and bind it to a random `127.0.0.1` host port.
/// One helper builds the Docker `exposed_ports` + `port_bindings` maps for every
/// published daemon port, so the RPC and HTTP ports share one publish path.
fn publish_loopback_ports(ports: &[u16]) -> (ExposedPorts, PortBindings) {
    let mut exposed_ports = HashMap::new();
    let mut port_bindings = HashMap::new();
    for port in ports {
        let key = format!("{port}/tcp");
        exposed_ports.insert(key.clone(), HashMap::new());
        port_bindings.insert(
            key,
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_owned()),
                host_port: Some("0".to_owned()),
            }]),
        );
    }
    (exposed_ports, port_bindings)
}

async fn collect_logs(docker: &Docker, container: &str) -> String {
    let options = LogsOptions::<String> {
        stdout: true,
        stderr: true,
        tail: LOG_CAPTURE_TAIL.to_owned(),
        ..Default::default()
    };
    let mut stream = docker.logs(container, Some(options));
    let mut buffer = Vec::new();
    while let Some(item) = stream.next().await {
        let Ok(output) = item else { break };
        buffer.extend_from_slice(output.into_bytes().as_ref());
        if buffer.len() >= LOG_CAPTURE_CAP_BYTES {
            break;
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

fn container_exit_reason(state: &ContainerState) -> Option<String> {
    let status = state.status;
    let stopped = matches!(
        status,
        Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD)
    ) || state.dead == Some(true)
        || (state.running == Some(false)
            && !matches!(
                status,
                Some(ContainerStateStatusEnum::CREATED | ContainerStateStatusEnum::RESTARTING)
            ));
    if !stopped {
        return None;
    }
    Some(format!(
        "container exited before daemon became ready: state={status:?} running={:?} exit_code={:?} error={:?}",
        state.running,
        state.exit_code,
        state.error.as_deref()
    ))
}

fn logs_for_context(logs: &str) -> String {
    logs.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| parse_cli_log(line.trim()).unwrap_or_else(|| line.to_owned()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_cli_log(line: &str) -> Option<String> {
    let encoded = line.strip_prefix("cli_log(")?.strip_suffix(')')?;
    serde_json::from_str(encoded).ok()
}

fn server_status(error: &bollard::errors::Error) -> Option<u16> {
    match error {
        bollard::errors::Error::DockerResponseServerError { status_code, .. } => Some(*status_code),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_metrics_batches_reuse_one_lazy_executor() {
        let engine = DockerEngine::new(DockerRuntimeConfig::default());
        assert!(engine
            .resource_metrics_executor
            .lock()
            .expect("resource metrics executor lock")
            .is_none());

        assert!(engine
            .container_resource_metrics_batch(Vec::new())
            .expect("first empty metrics batch")
            .is_empty());
        let first_executor = engine
            .resource_metrics_executor
            .lock()
            .expect("resource metrics executor lock")
            .as_ref()
            .map(ResourceMetricsExecutor::identity)
            .expect("resource metrics executor");

        assert!(engine
            .container_resource_metrics_batch(Vec::new())
            .expect("second empty metrics batch")
            .is_empty());
        let second_executor = engine
            .resource_metrics_executor
            .lock()
            .expect("resource metrics executor lock")
            .as_ref()
            .map(ResourceMetricsExecutor::identity)
            .expect("resource metrics executor");

        assert_eq!(first_executor, second_executor);
    }

    #[test]
    fn resource_metrics_batch_uses_safe_fallback_inside_tokio_runtime() {
        let engine = DockerEngine::new(DockerRuntimeConfig::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        runtime.block_on(async {
            assert!(engine
                .container_resource_metrics_batch(Vec::new())
                .expect("empty metrics batch inside Tokio runtime")
                .is_empty());
        });
        assert!(engine
            .resource_metrics_executor
            .lock()
            .expect("resource metrics executor lock")
            .is_none());
    }
}
