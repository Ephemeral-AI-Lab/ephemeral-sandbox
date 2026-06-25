//! Bollard client wrapper plus the async→sync bridge (§4.9): every call spawns a
//! fresh thread, builds a current-thread tokio runtime inside it, constructs the
//! bollard client inside that runtime, blocks on the operation, and joins. A
//! bollard `Docker` handle is never reused across ephemeral runtimes.

use std::collections::HashMap;
use std::future::Future;

use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions, UploadToContainerOptions,
};
use bollard::models::{
    ContainerInspectResponse, ContainerSummary, HostConfig, HostConfigCgroupnsModeEnum, PortBinding,
};
use bollard::Docker;
use bytes::Bytes;
use futures_util::StreamExt as _;

use sandbox_config::configs::manager::DockerRuntimeConfig;

use crate::labels;

const HTTP_NOT_FOUND: u16 = 404;
const HTTP_NOT_MODIFIED: u16 = 304;
const CONNECT_TIMEOUT_SECS: u64 = 120;
const LOG_CAPTURE_TAIL: &str = "200";
const LOG_CAPTURE_CAP_BYTES: usize = 8192;

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
    pub(crate) tmpfs: HashMap<String, String>,
    pub(crate) daemon_port: u16,
    pub(crate) privileged: bool,
    pub(crate) platform: Option<String>,
    pub(crate) memory_bytes: Option<i64>,
    pub(crate) nano_cpus: Option<i64>,
}

/// Result of starting a container and resolving its published daemon port.
pub(crate) struct StartedContainer {
    pub(crate) port: u16,
    pub(crate) auth_token: String,
}

/// A container reconstructed from labels + published port during recovery.
pub(crate) struct RecoveredContainer {
    pub(crate) sandbox_id: String,
    pub(crate) host_workspace_root: String,
    pub(crate) auth_token: String,
    pub(crate) published_port: u16,
}

pub(crate) struct DockerEngine {
    config: DockerRuntimeConfig,
}

impl DockerEngine {
    pub(crate) fn new(config: DockerRuntimeConfig) -> Self {
        Self { config }
    }

    pub(crate) fn config(&self) -> &DockerRuntimeConfig {
        &self.config
    }

    fn run_blocking<T, F, Fut>(&self, op: F) -> Result<T, DockerError>
    where
        T: Send + 'static,
        F: FnOnce(Docker) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, DockerError>>,
    {
        let endpoint = self.config.docker_endpoint.clone();
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
                    let docker = connect(endpoint.as_deref())?;
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
            let port_key = format!("{}/tcp", spec.daemon_port);
            let mut exposed_ports = HashMap::new();
            exposed_ports.insert(port_key.clone(), HashMap::new());
            let mut port_bindings = HashMap::new();
            port_bindings.insert(
                port_key,
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_owned()),
                    host_port: Some("0".to_owned()),
                }]),
            );
            let host_config = HostConfig {
                binds: Some(spec.binds),
                tmpfs: if spec.tmpfs.is_empty() {
                    None
                } else {
                    Some(spec.tmpfs)
                },
                port_bindings: Some(port_bindings),
                privileged: Some(spec.privileged),
                cgroupns_mode: Some(HostConfigCgroupnsModeEnum::PRIVATE),
                init: Some(true),
                memory: spec.memory_bytes,
                nano_cpus: spec.nano_cpus,
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
            docker
                .create_container(Some(options), config)
                .await
                .map(|_| ())
                .map_err(|error| DockerError::Api(format!("create_container: {error}")))
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

    pub(crate) fn start_and_resolve(
        &self,
        container: String,
        daemon_port: u16,
    ) -> Result<StartedContainer, DockerError> {
        self.run_blocking(move |docker| async move {
            docker
                .start_container(&container, None::<StartContainerOptions<String>>)
                .await
                .map_err(|error| DockerError::Api(format!("start_container: {error}")))?;
            let inspected = docker
                .inspect_container(&container, None)
                .await
                .map_err(|error| DockerError::Api(format!("inspect_container: {error}")))?;
            let port = published_port(&inspected, daemon_port)?;
            let auth_token = label_value(&inspected, labels::AUTH_TOKEN)?;
            Ok(StartedContainer { port, auth_token })
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
                ..Default::default()
            };
            match docker.remove_container(&container, Some(options)).await {
                Ok(()) => Ok(()),
                Err(error) if server_status(&error) == Some(HTTP_NOT_FOUND) => Ok(()),
                Err(error) => Err(DockerError::Api(format!("remove_container: {error}"))),
            }
        })
    }

    pub(crate) fn list_recoverable(
        &self,
        gateway_instance_id: String,
        daemon_port: u16,
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
                .filter_map(|summary| recovered_from_summary(summary, daemon_port))
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
            if !logs.is_empty() {
                context.push_str(&format!("; logs: {logs}"));
            }
            Ok(context)
        })
        .unwrap_or_default()
    }
}

fn connect(endpoint: Option<&str>) -> Result<Docker, DockerError> {
    let docker = match endpoint {
        Some(value) if value.starts_with("http://") || value.starts_with("tcp://") => {
            Docker::connect_with_http(value, CONNECT_TIMEOUT_SECS, bollard::API_DEFAULT_VERSION)
        }
        Some(value) => {
            Docker::connect_with_unix(value, CONNECT_TIMEOUT_SECS, bollard::API_DEFAULT_VERSION)
        }
        None => Docker::connect_with_local_defaults(),
    };
    docker.map_err(|error| DockerError::Connect(error.to_string()))
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
) -> Option<RecoveredContainer> {
    let labels = summary.labels.as_ref()?;
    let sandbox_id = labels.get(labels::SANDBOX_ID)?.clone();
    let host_workspace_root = labels.get(labels::HOST_WORKSPACE_ROOT)?.clone();
    let auth_token = labels.get(labels::AUTH_TOKEN)?.clone();
    let published_port = summary
        .ports
        .as_ref()?
        .iter()
        .find(|port| port.private_port == daemon_port)
        .and_then(|port| port.public_port)?;
    Some(RecoveredContainer {
        sandbox_id,
        host_workspace_root,
        auth_token,
        published_port,
    })
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

fn server_status(error: &bollard::errors::Error) -> Option<u16> {
    match error {
        bollard::errors::Error::DockerResponseServerError { status_code, .. } => Some(*status_code),
        _ => None,
    }
}
