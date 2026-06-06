//! The Docker [`ProviderAdapter`] over `bollard` (the only Rust production
//! provider). The container/exec calls require a live
//! Docker daemon, so their behavior is exercised at integration time (the
//! `docker` cargo feature); unit tests cover the pure serialize/host-config/env
//! helpers, and the `#[cfg(test)]` mock adapter (see the `support` test module) substitutes for
//! daemon/lifecycle unit tests.

use std::collections::HashMap;

use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogOutput, RemoveContainerOptions,
    UploadToContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::{CreateImageOptions, ListImagesOptions};
use bollard::models::{
    ContainerInspectResponse, ContainerState, ContainerSummary, HostConfig, ImageSummary,
    PortBinding,
};
use bollard::Docker;
use eos_types::SandboxId;
use futures::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::error::SandboxHostError;
use crate::provider::{
    sealed, ContextPreparer, CreateSandboxSpec, DaemonTcpEndpoint, DockerContextPreparer, ExecOpts,
    Labels, ProviderAdapter, ProviderHealth, ProviderKind, RawExecResult, SandboxInfo,
    SnapshotInfo,
};

// --- constants ----------------------------------------------------------------

const APP_MANAGED_BY: &str = "eos";
const APP_CREATED_VIA: &str = "ephemeral_os";
const DAEMON_TCP_INTERNAL_PORT: u16 = 37657;
const DAEMON_TCP_ENABLED_LABEL: &str = "eos.daemon.tcp.enabled";
const DAEMON_TCP_PORT_LABEL: &str = "eos.daemon.tcp.port";
const DAEMON_TCP_ENV_HOST: &str = "EOS_DAEMON_TCP_HOST";
const DAEMON_TCP_ENV_PORT: &str = "EOS_DAEMON_TCP_PORT";
const DAEMON_AUTH_ENV: &str = "EOS_DAEMON_AUTH_TOKEN";
const DOCKER_INIT_ENABLED_LABEL: &str = "eos.docker.init.enabled";
const EOS_RUNTIME_TMPFS_TARGET: &str = "/eos";
const DEFAULT_OVERLAY_WRITABLE_TMPFS_OPTIONS: &str = "rw,exec,size=2g,mode=1777";

fn is_eos_tmpfs_destination(dest_dir: &str) -> bool {
    dest_dir == EOS_RUNTIME_TMPFS_TARGET
        || dest_dir
            .strip_prefix(EOS_RUNTIME_TMPFS_TARGET)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

// Host-side Docker launch knob. The host adapter only *launches* the container
// the sandbox daemon runs in; sandbox configuration (including the E2E launch
// config in `sandbox/config/prd.yml`) is owned by the ephemeral-os sandbox
// module — so this is a fixed compiled default, not agent-core config. Sandbox
// containers
// always run with SYS_ADMIN + NET_ADMIN (see `host_config_kwargs`); the old
// `privileged` / `no_privilege` knobs were dead — dropping those caps would
// break the namespace/overlay/netlink machinery the sandbox requires.
const DAEMON_TCP: bool = true;

/// The Docker-backed provider adapter. Holds a cheap-to-clone, pooled
/// `bollard::Docker` (no lazy `Option`/`to_thread` artifacts — bollard is async)
/// plus the fixed daemon-TCP launch knob (no env / config — sandbox provisioning
/// policy is owned by the sandbox module).
#[derive(Debug, Clone)]
pub struct DockerProviderAdapter {
    docker: Docker,
    daemon_tcp: bool,
}

impl DockerProviderAdapter {
    /// Connect to the local Docker daemon (env / default socket) with the fixed
    /// host-adapter launch knob.
    pub fn connect() -> Result<Self, SandboxHostError> {
        let docker = Docker::connect_with_local_defaults().map_err(SandboxHostError::Docker)?;
        Ok(Self::from_client(docker))
    }

    /// Wrap an existing `bollard::Docker` handle with the fixed launch knob.
    #[must_use]
    pub fn from_client(docker: Docker) -> Self {
        Self {
            docker,
            daemon_tcp: DAEMON_TCP,
        }
    }
}

impl sealed::Sealed for DockerProviderAdapter {}

#[async_trait]
impl ProviderAdapter for DockerProviderAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Docker
    }

    async fn health(&self) -> Result<ProviderHealth, SandboxHostError> {
        // fail-open: a docker error becomes `healthy: false` with the detail.
        match self.docker.info().await {
            Ok(info) => Ok(ProviderHealth {
                provider: ProviderKind::Docker.as_str().to_owned(),
                healthy: true,
                server_version: info.server_version,
                containers_running: info.containers_running.and_then(|c| u64::try_from(c).ok()),
                kernel_version: info.kernel_version,
                operating_system: info.operating_system,
                error: None,
            }),
            Err(err) => Ok(ProviderHealth {
                provider: ProviderKind::Docker.as_str().to_owned(),
                healthy: false,
                server_version: None,
                containers_running: None,
                kernel_version: None,
                operating_system: None,
                error: Some(err.to_string()),
            }),
        }
    }

    async fn list_snapshots(&self) -> Result<Vec<SnapshotInfo>, SandboxHostError> {
        // fail-open: an error logs + returns an empty list.
        match self
            .docker
            .list_images(None::<ListImagesOptions<String>>)
            .await
        {
            Ok(images) => Ok(images.iter().map(serialize_image).collect()),
            Err(err) => {
                tracing::warn!(?err, "docker list_images failed; returning empty");
                Ok(Vec::new())
            }
        }
    }

    async fn create(&self, spec: &CreateSandboxSpec) -> Result<SandboxInfo, SandboxHostError> {
        let image_ref = spec
            .image
            .as_deref()
            .or(spec.snapshot.as_deref())
            .unwrap_or("")
            .trim()
            .to_owned();
        if image_ref.is_empty() {
            return Err(SandboxHostError::InvalidRequest(
                "docker create requires image or snapshot".to_owned(),
            ));
        }

        // base labels, then caller labels, THEN the init label (caller cannot
        // override the init/TCP labels — order is load-bearing).
        let mut labels: Labels = Labels::new();
        labels.insert("managed_by".to_owned(), APP_MANAGED_BY.to_owned());
        labels.insert("created_via".to_owned(), APP_CREATED_VIA.to_owned());
        labels.insert("language".to_owned(), spec.language.clone());
        if let Some(snapshot) = spec.snapshot.as_deref() {
            if !snapshot.is_empty() {
                labels.insert("snapshot".to_owned(), snapshot.to_owned());
            }
        }
        for (k, v) in normalize_string_map(&spec.labels) {
            labels.insert(k, v);
        }
        labels.insert(DOCKER_INIT_ENABLED_LABEL.to_owned(), "1".to_owned());

        let mut environment = normalize_string_map(&spec.env_vars);
        let mut host_config = host_config_kwargs();
        if self.daemon_tcp {
            environment.insert(DAEMON_TCP_ENV_HOST.to_owned(), "0.0.0.0".to_owned());
            environment.insert(
                DAEMON_TCP_ENV_PORT.to_owned(),
                DAEMON_TCP_INTERNAL_PORT.to_string(),
            );
            environment.insert(DAEMON_AUTH_ENV.to_owned(), generate_auth_token());
            labels.insert(DAEMON_TCP_ENABLED_LABEL.to_owned(), "1".to_owned());
            labels.insert(
                DAEMON_TCP_PORT_LABEL.to_owned(),
                DAEMON_TCP_INTERNAL_PORT.to_string(),
            );
            let mut ports: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
            ports.insert(
                format!("{DAEMON_TCP_INTERNAL_PORT}/tcp"),
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_owned()),
                    host_port: None, // None → Docker assigns a random ephemeral host port
                }]),
            );
            host_config.port_bindings = Some(ports);
        }

        let env_vec: Vec<String> = environment
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let labels_map: HashMap<String, String> = labels.into_iter().collect();
        let config = Config {
            image: Some(image_ref.clone()),
            cmd: Some(vec!["sleep".to_owned(), "infinity".to_owned()]),
            tty: Some(false),
            env: Some(env_vec),
            labels: Some(labels_map),
            host_config: Some(host_config),
            ..Default::default()
        };
        let options = CreateContainerOptions {
            name: spec.name.clone(),
            platform: spec.platform.clone(),
        };

        let created = match self
            .docker
            .create_container(Some(options.clone()), config.clone())
            .await
        {
            Ok(created) => created,
            Err(err) if is_image_not_found(&err) => {
                self.pull_image(&image_ref, spec.platform.as_deref())
                    .await?;
                self.docker
                    .create_container(Some(options), config)
                    .await
                    .map_err(SandboxHostError::Docker)?
            }
            Err(err) => return Err(SandboxHostError::Docker(err)),
        };

        self.docker
            .start_container::<String>(&created.id, None)
            .await
            .map_err(SandboxHostError::Docker)?;
        let inspect = self.inspect(&created.id).await?;
        serialize_container(&inspect)
    }

    async fn get(&self, id: &SandboxId) -> Result<SandboxInfo, SandboxHostError> {
        serialize_container(&self.inspect(id.as_str()).await?)
    }

    async fn list(&self) -> Result<Vec<SandboxInfo>, SandboxHostError> {
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        filters.insert(
            "label".to_owned(),
            vec![format!("managed_by={APP_MANAGED_BY}")],
        );
        let options = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };
        match self.docker.list_containers(Some(options)).await {
            Ok(summaries) => summaries.iter().map(serialize_container_summary).collect(),
            Err(err) => {
                tracing::warn!(?err, "docker list_containers failed; returning empty");
                Ok(Vec::new())
            }
        }
    }

    async fn start(&self, id: &SandboxId) -> Result<SandboxInfo, SandboxHostError> {
        self.docker
            .start_container::<String>(id.as_str(), None)
            .await
            .map_err(SandboxHostError::Docker)?;
        serialize_container(&self.inspect(id.as_str()).await?)
    }

    async fn stop(&self, id: &SandboxId) -> Result<SandboxInfo, SandboxHostError> {
        self.docker
            .stop_container(id.as_str(), None)
            .await
            .map_err(SandboxHostError::Docker)?;
        serialize_container(&self.inspect(id.as_str()).await?)
    }

    async fn delete(&self, id: &SandboxId) -> Result<(), SandboxHostError> {
        let options = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        match self
            .docker
            .remove_container(id.as_str(), Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(err) if is_docker_not_found(&err) => Ok(()),
            Err(err) => Err(SandboxHostError::Docker(err)),
        }
    }

    async fn set_labels(
        &self,
        id: &SandboxId,
        labels: &Labels,
    ) -> Result<SandboxInfo, SandboxHostError> {
        // Docker cannot mutate labels on a live container; read the current state,
        // warn if a change was requested, and return the unchanged container.
        let inspect = self.inspect(id.as_str()).await?;
        let current = inspect
            .config
            .as_ref()
            .and_then(|c| c.labels.clone())
            .unwrap_or_default();
        let requested = normalize_string_map(labels);
        let changed = requested
            .iter()
            .any(|(k, v)| current.get(k).map(String::as_str) != Some(v.as_str()));
        if changed {
            let mut keys: Vec<&String> = requested.keys().collect();
            keys.sort();
            tracing::warn!(
                sandbox = id.as_str(),
                ?keys,
                "docker cannot mutate labels on a live container; ignoring"
            );
        }
        serialize_container(&inspect)
    }

    async fn daemon_tcp_endpoint(
        &self,
        id: &SandboxId,
    ) -> Result<Option<DaemonTcpEndpoint>, SandboxHostError> {
        if !self.daemon_tcp {
            return Ok(None);
        }
        let inspect = self.inspect(id.as_str()).await?;
        Ok(daemon_tcp_endpoint_from_inspect(&inspect))
    }

    async fn exec(
        &self,
        id: &SandboxId,
        command: &str,
        opts: &ExecOpts,
    ) -> Result<RawExecResult, SandboxHostError> {
        let fut = self.exec_inner(id, command, opts.cwd.as_deref());
        match opts.timeout {
            Some(timeout) => match tokio::time::timeout(timeout, fut).await {
                Ok(result) => result,
                Err(_) => Err(SandboxHostError::ExecFailed {
                    exit_code: -1,
                    message: format!("exec timed out after {}s", timeout.as_secs()),
                }),
            },
            None => fut.await,
        }
    }

    async fn put_archive(
        &self,
        id: &SandboxId,
        tar_stream: &[u8],
        dest_dir: &str,
    ) -> Result<(), SandboxHostError> {
        if is_eos_tmpfs_destination(dest_dir) {
            return self
                .put_archive_into_eos_tmpfs(id, tar_stream, dest_dir)
                .await;
        }
        let options = UploadToContainerOptions {
            path: dest_dir.to_owned(),
            ..Default::default()
        };
        self.docker
            .upload_to_container(
                id.as_str(),
                Some(options),
                bytes::Bytes::copy_from_slice(tar_stream),
            )
            .await
            .map_err(SandboxHostError::Docker)
    }

    fn context_preparer(&self, id: &SandboxId) -> ContextPreparer {
        ContextPreparer::Docker(DockerContextPreparer::new(id.clone()))
    }
}

impl DockerProviderAdapter {
    async fn put_archive_into_eos_tmpfs(
        &self,
        id: &SandboxId,
        tar_stream: &[u8],
        dest_dir: &str,
    ) -> Result<(), SandboxHostError> {
        let exec = self
            .docker
            .create_exec(
                id.as_str(),
                CreateExecOptions {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(false),
                    cmd: Some(vec!["tar", "-xf", "-", "-C", dest_dir]),
                    ..Default::default()
                },
            )
            .await
            .map_err(SandboxHostError::Docker)?
            .id;

        let StartExecResults::Attached {
            mut output,
            mut input,
        } = self
            .docker
            .start_exec(&exec, None)
            .await
            .map_err(SandboxHostError::Docker)?
        else {
            return Err(SandboxHostError::ExecFailed {
                exit_code: -1,
                message: "docker put_archive tar exec detached unexpectedly".to_owned(),
            });
        };

        input.write_all(tar_stream).await?;
        input.shutdown().await?;

        let mut captured = Vec::new();
        while let Some(chunk) = output.next().await {
            let chunk = chunk.map_err(SandboxHostError::Docker)?;
            captured.extend_from_slice(chunk.into_bytes().as_ref());
        }

        let inspected = self
            .docker
            .inspect_exec(&exec)
            .await
            .map_err(SandboxHostError::Docker)?;
        let exit_code = inspected
            .exit_code
            .and_then(|code| i32::try_from(code).ok())
            .unwrap_or(-1);
        if exit_code != 0 {
            return Err(SandboxHostError::ExecFailed {
                exit_code,
                message: format!(
                    "docker put_archive tar extraction failed for {dest_dir}: {}",
                    String::from_utf8_lossy(&captured)
                ),
            });
        }

        Ok(())
    }
}

impl DockerProviderAdapter {
    async fn inspect(&self, id: &str) -> Result<ContainerInspectResponse, SandboxHostError> {
        self.docker
            .inspect_container(id, None)
            .await
            .map_err(SandboxHostError::Docker)
    }

    async fn pull_image(
        &self,
        image_ref: &str,
        platform: Option<&str>,
    ) -> Result<(), SandboxHostError> {
        let options = CreateImageOptions {
            from_image: image_ref.to_owned(),
            platform: platform.unwrap_or("").to_owned(),
            ..Default::default()
        };
        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(item) = stream.next().await {
            item.map_err(SandboxHostError::Docker)?;
        }
        Ok(())
    }

    async fn exec_inner(
        &self,
        id: &SandboxId,
        command: &str,
        cwd: Option<&str>,
    ) -> Result<RawExecResult, SandboxHostError> {
        // cwd wrap: the literal newlines around the subshell parens are deliberate
        // (they keep heredoc terminators on their own line).
        let wrapped = match cwd {
            Some(dir) if !dir.is_empty() => {
                format!(
                    "cd {} && (\n{command}\n)",
                    crate::daemon_client::posix_quote(dir)
                )
            }
            _ => command.to_owned(),
        };
        let create = self
            .docker
            .create_exec(
                id.as_str(),
                CreateExecOptions {
                    cmd: Some(vec!["/bin/bash".to_owned(), "-lc".to_owned(), wrapped]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(false),
                    ..Default::default()
                },
            )
            .await
            .map_err(SandboxHostError::Docker)?;

        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();
        if let StartExecResults::Attached { mut output, .. } = self
            .docker
            .start_exec(&create.id, None)
            .await
            .map_err(SandboxHostError::Docker)?
        {
            while let Some(chunk) = output.next().await {
                match chunk.map_err(SandboxHostError::Docker)? {
                    LogOutput::StdOut { message } => stdout.extend_from_slice(&message),
                    LogOutput::StdErr { message } => stderr.extend_from_slice(&message),
                    _ => {}
                }
            }
        }
        let inspect = self
            .docker
            .inspect_exec(&create.id)
            .await
            .map_err(SandboxHostError::Docker)?;
        let exit_code = i32::try_from(inspect.exit_code.unwrap_or(0)).unwrap_or(-1);
        Ok(RawExecResult {
            success: exit_code == 0,
            exit_code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

// --- pure helpers (pub(crate) for unit tests) ---------------------------------

/// Normalize a string map: trim keys+values, drop empty-key entries.
fn normalize_string_map(map: &Labels) -> Labels {
    map.iter()
        .filter_map(|(k, v)| {
            let key = k.trim();
            if key.is_empty() {
                None
            } else {
                Some((key.to_owned(), v.trim().to_owned()))
            }
        })
        .collect()
}

fn generate_auth_token() -> String {
    // The exact token format is not load-bearing (the daemon reads whatever is
    // in EOS_DAEMON_AUTH_TOKEN). Two
    // v4 UUIDs give 244 bits of CSPRNG entropy without a new dep.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn host_config_kwargs() -> HostConfig {
    // Sandbox containers always run with SYS_ADMIN + NET_ADMIN and unconfined
    // seccomp/apparmor: these caps are load-bearing for the namespace, overlay,
    // and netlink machinery the sandbox daemon runs. There is no host-side
    // privilege toggle (the old privileged / no_privilege knobs were dead).
    let mut host_config = HostConfig {
        init: Some(true),
        cap_add: Some(vec!["SYS_ADMIN".to_owned(), "NET_ADMIN".to_owned()]),
        security_opt: Some(vec![
            "seccomp=unconfined".to_owned(),
            "apparmor=unconfined".to_owned(),
        ]),
        ..Default::default()
    };
    // Always mount the writable /eos overlay tmpfs with the default options.
    // The size/disable toggles were env config; sandbox provisioning policy is
    // owned by the sandbox module, not agent-core, so this is a fixed default.
    let mut tmpfs = HashMap::new();
    tmpfs.insert(
        EOS_RUNTIME_TMPFS_TARGET.to_owned(),
        DEFAULT_OVERLAY_WRITABLE_TMPFS_OPTIONS.to_owned(),
    );
    host_config.tmpfs = Some(tmpfs);
    host_config
}

fn is_image_not_found(err: &bollard::errors::Error) -> bool {
    if let bollard::errors::Error::DockerResponseServerError {
        status_code,
        message,
    } = err
    {
        let lower = message.to_ascii_lowercase();
        return *status_code == 404
            || lower.contains("no such image")
            || lower.contains("image not found");
    }
    false
}

fn is_docker_not_found(err: &bollard::errors::Error) -> bool {
    matches!(
        err,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

fn state_status_string(state: Option<&ContainerState>) -> Option<String> {
    let status = state?.status.as_ref()?;
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
}

pub(crate) fn serialize_container(
    inspect: &ContainerInspectResponse,
) -> Result<SandboxInfo, SandboxHostError> {
    let config = inspect.config.as_ref();
    let labels: Labels = config
        .and_then(|c| c.labels.clone())
        .map(|m| m.into_iter().collect())
        .unwrap_or_default();
    let id = inspect.id.clone().unwrap_or_default();
    let name = inspect
        .name
        .as_deref()
        .unwrap_or("")
        .trim_start_matches('/')
        .to_owned();
    let state = state_status_string(inspect.state.as_ref())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let project_dir = labels
        .get("project_dir")
        .cloned()
        .or_else(|| config.and_then(|c| c.working_dir.clone()))
        .filter(|s| !s.is_empty());
    let managed_by_app = labels.get("managed_by").map(String::as_str) == Some(APP_MANAGED_BY);
    Ok(SandboxInfo {
        id: parse_sandbox_id(&id)?,
        name,
        image: config.and_then(|c| c.image.clone()),
        snapshot: labels.get("snapshot").cloned(),
        state,
        labels,
        project_dir,
        managed_by_app,
    })
}

fn serialize_container_summary(
    summary: &ContainerSummary,
) -> Result<SandboxInfo, SandboxHostError> {
    let labels: Labels = summary
        .labels
        .clone()
        .map(|m| m.into_iter().collect())
        .unwrap_or_default();
    let id = summary.id.clone().unwrap_or_default();
    let name = summary
        .names
        .as_ref()
        .and_then(|names| names.first())
        .map(|n| n.trim_start_matches('/').to_owned())
        .unwrap_or_default();
    let state = summary
        .state
        .clone()
        .or_else(|| summary.status.clone())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let project_dir = labels.get("project_dir").cloned().filter(|s| !s.is_empty());
    let managed_by_app = labels.get("managed_by").map(String::as_str) == Some(APP_MANAGED_BY);
    Ok(SandboxInfo {
        id: parse_sandbox_id(&id)?,
        name,
        image: summary.image.clone(),
        snapshot: labels.get("snapshot").cloned(),
        state,
        labels,
        project_dir,
        managed_by_app,
    })
}

fn serialize_image(image: &ImageSummary) -> SnapshotInfo {
    let primary = image.repo_tags.first().cloned();
    SnapshotInfo {
        name: primary.clone(),
        image: primary,
        id: image.id.clone(),
        tags: image.repo_tags.clone(),
    }
}

fn container_env(inspect: &ContainerInspectResponse) -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Some(items) = inspect.config.as_ref().and_then(|c| c.env.as_ref()) {
        for item in items {
            if let Some((key, value)) = item.split_once('=') {
                env.insert(key.to_owned(), value.to_owned());
            }
        }
    }
    env
}

fn daemon_tcp_endpoint_from_inspect(
    inspect: &ContainerInspectResponse,
) -> Option<DaemonTcpEndpoint> {
    let labels = inspect.config.as_ref().and_then(|c| c.labels.as_ref());
    if labels
        .and_then(|l| l.get(DAEMON_TCP_ENABLED_LABEL))
        .map(String::as_str)
        != Some("1")
    {
        return None;
    }
    let internal_port: u16 = match labels
        .and_then(|l| l.get(DAEMON_TCP_PORT_LABEL))
        .map(String::as_str)
        .filter(|s| !s.is_empty())
    {
        Some(port) => port.parse().ok()?,
        None => DAEMON_TCP_INTERNAL_PORT,
    };
    let bindings = inspect
        .network_settings
        .as_ref()
        .and_then(|ns| ns.ports.as_ref())
        .and_then(|ports| ports.get(&format!("{internal_port}/tcp")))
        .and_then(Option::as_ref)?;
    let binding = bindings
        .iter()
        .find(|b| b.host_port.as_deref().map(str::is_empty) == Some(false))?;
    let mut host = binding.host_ip.clone().unwrap_or_default();
    if host.is_empty() || host == "0.0.0.0" || host == "::" {
        host = "127.0.0.1".to_owned();
    }
    let host_port: u16 = binding.host_port.as_deref()?.parse().ok()?;
    let auth_token = container_env(inspect)
        .get(DAEMON_AUTH_ENV)
        .cloned()
        .unwrap_or_default();
    Some(DaemonTcpEndpoint {
        host,
        port: host_port,
        internal_port: Some(internal_port),
        auth_token,
    })
}

/// Parse a Docker id into a `SandboxId`. A real daemon response always carries a
/// non-empty id (the only value `SandboxId` rejects), so an empty/unparseable id
/// is surfaced as an error rather than fabricating a placeholder identity.
fn parse_sandbox_id(id: &str) -> Result<SandboxId, SandboxHostError> {
    id.parse().map_err(|_| {
        SandboxHostError::InvalidRequest(
            "provider returned a container with an empty or unparseable id".to_owned(),
        )
    })
}

#[cfg(test)]
#[path = "../tests/docker/mod.rs"]
mod tests;
