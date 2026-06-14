use std::ffi::OsStr;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::daemon_wire::{response_is_accepted, ProtocolClient, HEARTBEAT_OP};

const DAEMON_AUTH_TOKEN_ENV: &str = "EOS_DAEMON_AUTH_TOKEN";
const DAEMON_FORWARD_AUTH_TOKEN_ENV: &str = "EOS_DAEMON_FORWARD_AUTH_TOKEN";

#[cfg(test)]
static DOCKER_COMMAND_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
#[cfg(test)]
static DOCKER_COMMAND_OVERRIDE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) struct DockerCommandOverrideGuard {
    previous: Option<PathBuf>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for DockerCommandOverrideGuard {
    fn drop(&mut self) {
        let override_slot = DOCKER_COMMAND_OVERRIDE.get_or_init(|| Mutex::new(None));
        *override_slot.lock().expect("docker override lock") = self.previous.take();
    }
}

#[cfg(test)]
pub(crate) fn override_docker_command_for_tests(path: PathBuf) -> DockerCommandOverrideGuard {
    let lock = DOCKER_COMMAND_OVERRIDE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("docker override test lock");
    let override_slot = DOCKER_COMMAND_OVERRIDE.get_or_init(|| Mutex::new(None));
    let previous = override_slot
        .lock()
        .expect("docker override lock")
        .replace(path);
    DockerCommandOverrideGuard {
        previous,
        _lock: lock,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerLifetime {
    Keep,
    #[cfg(feature = "e2e-support")]
    SelfDestruct {
        ttl: Duration,
    },
}

#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub platform: Option<String>,
    pub privileged: bool,
    pub cap_add: Vec<String>,
    pub security_opt: Vec<String>,
    pub tmpfs: Vec<String>,
    pub labels: Vec<(String, String)>,
    pub lifetime: ContainerLifetime,
}

#[derive(Debug, Clone)]
pub struct DaemonSpec {
    pub eosd_path: PathBuf,
    pub remote_daemon_dir: PathBuf,
    pub remote_eosd_path: PathBuf,
    pub remote_config_path: PathBuf,
    pub config_yaml: String,
    pub extra_dirs: Vec<PathBuf>,
    pub tcp_port: u16,
    pub ready_timeout: Duration,
    pub request_timeout: Duration,
}

#[derive(Debug)]
pub struct DaemonContainer {
    name: String,
    client: ProtocolClient,
    daemon_log_path: String,
    token: String,
    forward_token: String,
    keep: bool,
}

impl DaemonContainer {
    #[cfg(feature = "e2e-support")]
    pub fn start(
        container: &ContainerSpec,
        daemon: &DaemonSpec,
        auth_token: String,
    ) -> Result<Self> {
        Self::start_with_forward_token(container, daemon, auth_token.clone(), auth_token)
    }

    pub fn start_with_forward_token(
        container: &ContainerSpec,
        daemon: &DaemonSpec,
        auth_token: String,
        forward_auth_token: String,
    ) -> Result<Self> {
        let keep = container.lifetime == ContainerLifetime::Keep;
        let run = docker_run_args(container, daemon.tcp_port);

        docker(run).with_context(|| format!("docker run for {}", container.name))?;

        let mut handle = Self::handle(
            container.name.clone(),
            auth_token.clone(),
            forward_auth_token.clone(),
            daemon,
            None,
            keep,
        );
        match handle.bringup(daemon) {
            Ok(client) => {
                handle.client = client;
                Ok(handle)
            }
            Err(err) => {
                let log = handle.daemon_log().unwrap_or_default();
                drop(handle);
                Err(err.context(format!("daemon bringup failed; log tail:\n{log}")))
            }
        }
    }
    pub(crate) fn for_engine(
        name: String,
        auth_token: String,
        forward_auth_token: String,
        daemon: &DaemonSpec,
        endpoint: Option<SocketAddr>,
    ) -> Self {
        Self::handle(name, auth_token, forward_auth_token, daemon, endpoint, true)
    }

    #[cfg(feature = "e2e-support")]
    pub fn adopt(id: &str, auth_token: String, daemon: &DaemonSpec) -> Result<Self> {
        Self::adopt_with_forward_token(id, auth_token.clone(), auth_token, daemon)
    }

    #[cfg(feature = "e2e-support")]
    pub fn adopt_with_forward_token(
        id: &str,
        auth_token: String,
        forward_auth_token: String,
        daemon: &DaemonSpec,
    ) -> Result<Self> {
        let mut handle = Self::handle(
            id.to_owned(),
            auth_token,
            forward_auth_token.clone(),
            daemon,
            None,
            true,
        );
        let addr = wait_for_published_addr(id, daemon.tcp_port)?;
        let client = ProtocolClient::new_forward_authorized(
            addr,
            Some(forward_auth_token),
            daemon.request_timeout,
        );
        await_ready(&client, daemon.ready_timeout)?;
        handle.client = client;
        Ok(handle)
    }

    fn handle(
        name: String,
        auth_token: String,
        forward_auth_token: String,
        daemon: &DaemonSpec,
        endpoint: Option<SocketAddr>,
        keep: bool,
    ) -> Self {
        Self {
            name,
            client: ProtocolClient::new_forward_authorized(
                endpoint.unwrap_or_else(placeholder_addr),
                Some(forward_auth_token.clone()),
                daemon.request_timeout,
            ),
            daemon_log_path: daemon
                .remote_daemon_dir
                .join("runtime.log")
                .to_string_lossy()
                .into_owned(),
            token: auth_token,
            forward_token: forward_auth_token,
            keep,
        }
    }

    fn bringup(&self, daemon: &DaemonSpec) -> Result<ProtocolClient> {
        let daemon_dir = path_str(&daemon.remote_daemon_dir)?;
        let remote_eosd_path = path_str(&daemon.remote_eosd_path)?;
        let config_dir = daemon
            .remote_config_path
            .parent()
            .context("remote config path has no parent")?;
        let config_dir = path_str(config_dir)?;
        let config_name = daemon
            .remote_config_path
            .file_name()
            .and_then(|name| name.to_str())
            .context("remote config path has no UTF-8 file name")?;

        let mut mkdir = vec!["mkdir", "-p", &daemon_dir, &config_dir];
        let extra_dirs = daemon
            .extra_dirs
            .iter()
            .map(|dir| path_str(dir))
            .collect::<Result<Vec<_>>>()?;
        mkdir.extend(extra_dirs.iter().map(String::as_str));
        self.exec(&mkdir).context("mkdir daemon dirs")?;
        self.exec(&[
            "sh",
            "-lc",
            "mount -o remount,rw /sys/fs/cgroup 2>/dev/null || true; test -w /sys/fs/cgroup",
        ])
        .context("make cgroup v2 writable for isolated workspaces")?;
        copy_file_into(&self.name, &daemon_dir, "eosd", &daemon.eosd_path).with_context(|| {
            format!(
                "copy eosd ({}) into {daemon_dir}",
                daemon.eosd_path.display()
            )
        })?;
        copy_bytes_into(
            &self.name,
            &config_dir,
            config_name,
            daemon.config_yaml.as_bytes(),
            0o644,
        )
        .with_context(|| format!("copy merged config into {config_dir}"))?;

        let remote_config_path = path_str(&daemon.remote_config_path)?;
        self.spawn_daemon(
            &daemon_dir,
            &remote_eosd_path,
            &remote_config_path,
            daemon.tcp_port,
        )
        .context("spawn eosd daemon")?;

        let addr = wait_for_published_addr(&self.name, daemon.tcp_port)?;
        let client = ProtocolClient::new_forward_authorized(
            addr,
            Some(self.forward_token.clone()),
            daemon.request_timeout,
        );
        await_ready(&client, daemon.ready_timeout)?;
        Ok(client)
    }
    pub fn client(&self) -> &ProtocolClient {
        &self.client
    }

    #[must_use]
    #[cfg(feature = "e2e-support")]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn exec(&self, argv: &[&str]) -> Result<String> {
        docker(docker_exec_args(&self.name, argv))
    }

    #[cfg(feature = "e2e-support")]
    pub fn copy_daemon_log_to(&self, host_dest: &Path) -> Result<()> {
        copy_path_from_container(&self.name, &self.daemon_log_path, host_dest)
    }

    pub fn restart_daemon(&self, daemon: &DaemonSpec) -> Result<()> {
        let daemon_dir = path_str(&daemon.remote_daemon_dir)?;
        let remote_eosd_path = path_str(&daemon.remote_eosd_path)?;
        let remote_config_path = path_str(&daemon.remote_config_path)?;
        let teardown = format!(
            "kill -9 \"$(cat {daemon_dir}/runtime.pid 2>/dev/null)\" 2>/dev/null; \
             pkill -9 -f 'eosd daemon' 2>/dev/null; sleep 1; \
             rm -f {daemon_dir}/runtime.sock {daemon_dir}/runtime.pid"
        );
        let _ = self.exec(&["sh", "-lc", &teardown]);
        self.spawn_daemon(
            &daemon_dir,
            &remote_eosd_path,
            &remote_config_path,
            daemon.tcp_port,
        )
        .context("respawn eosd daemon")?;
        await_ready(&self.client, daemon.ready_timeout).context("daemon not ready after restart")
    }

    fn spawn_daemon(
        &self,
        daemon_dir: &str,
        remote_eosd_path: &str,
        remote_config_path: &str,
        tcp_port: u16,
    ) -> Result<String> {
        let args = daemon_spawn_args(
            remote_eosd_path,
            daemon_dir,
            remote_config_path,
            tcp_port,
            &self.token,
            &self.forward_token,
        );
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        self.exec(&arg_refs)
    }

    fn daemon_log(&self) -> Option<String> {
        docker([
            "exec",
            self.name.as_str(),
            "tail",
            "-n",
            "40",
            self.daemon_log_path.as_str(),
        ])
        .ok()
    }
}

fn docker_run_args(container: &ContainerSpec, daemon_tcp_port: u16) -> Vec<String> {
    let keep = container.lifetime == ContainerLifetime::Keep;
    let mut run = vec![
        "run".to_owned(),
        "-d".to_owned(),
        "--name".to_owned(),
        container.name.clone(),
    ];
    for (key, value) in &container.labels {
        run.push("--label".to_owned());
        run.push(format!("{key}={value}"));
    }
    if !keep {
        run.push("--rm".to_owned());
    }
    if container.privileged {
        run.push("--privileged".to_owned());
    }
    if let Some(platform) = &container.platform {
        run.push("--platform".to_owned());
        run.push(platform.clone());
    }
    for cap in &container.cap_add {
        run.push("--cap-add".to_owned());
        run.push(cap.clone());
    }
    for opt in &container.security_opt {
        run.push("--security-opt".to_owned());
        run.push(opt.clone());
    }
    for tmpfs in &container.tmpfs {
        run.push("--tmpfs".to_owned());
        run.push(tmpfs.clone());
    }
    run.push("--init".to_owned());
    run.push("-p".to_owned());
    run.push(format!("127.0.0.1::{daemon_tcp_port}"));
    run.push(container.image.clone());
    match container.lifetime {
        ContainerLifetime::Keep => run.extend(["sleep".to_owned(), "infinity".to_owned()]),
        #[cfg(feature = "e2e-support")]
        ContainerLifetime::SelfDestruct { ttl } => run.extend([
            "timeout".to_owned(),
            ttl.as_secs().to_string(),
            "sleep".to_owned(),
            "infinity".to_owned(),
        ]),
    }
    run
}

fn daemon_spawn_args(
    remote_eosd_path: &str,
    daemon_dir: &str,
    remote_config_path: &str,
    tcp_port: u16,
    token: &str,
    forward_token: &str,
) -> Vec<String> {
    vec![
        "-e".to_owned(),
        format!("{DAEMON_AUTH_TOKEN_ENV}={token}"),
        "-e".to_owned(),
        format!("{DAEMON_FORWARD_AUTH_TOKEN_ENV}={forward_token}"),
        "-d".to_owned(),
        remote_eosd_path.to_owned(),
        "daemon".to_owned(),
        "--spawn".to_owned(),
        "--config-yaml".to_owned(),
        remote_config_path.to_owned(),
        "--socket".to_owned(),
        format!("{daemon_dir}/runtime.sock"),
        "--pid-file".to_owned(),
        format!("{daemon_dir}/runtime.pid"),
        "--log-file".to_owned(),
        format!("{daemon_dir}/runtime.log"),
        "--tcp-host".to_owned(),
        "0.0.0.0".to_owned(),
        "--tcp-port".to_owned(),
        tcp_port.to_string(),
    ]
}

impl Drop for DaemonContainer {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = docker(["rm", "-f", self.name.as_str()]);
    }
}

fn placeholder_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 1))
}

fn await_ready(client: &ProtocolClient, budget: Duration) -> Result<()> {
    let deadline = Instant::now() + budget;
    let mut delay = Duration::from_millis(150);
    loop {
        let observed = match client.request(HEARTBEAT_OP, "ready-probe", &json!({})) {
            Ok(resp) if response_is_accepted(&resp) => return Ok(()),
            Ok(resp) => format!("non-success heartbeat: {resp}"),
            Err(err) => err.to_string(),
        };
        if Instant::now() >= deadline {
            bail!("daemon not ready within {budget:?}: {observed}");
        }
        thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(2));
    }
}

fn docker_exec_args(container: &str, argv: &[&str]) -> Vec<String> {
    let mut rebuilt: Vec<String> = vec!["exec".to_owned()];
    let mut index = 0;
    while let Some(token) = argv.get(index) {
        if token.starts_with('-') {
            rebuilt.push((*token).to_owned());
            if docker_exec_option_takes_value(token) {
                index += 1;
                if let Some(value) = argv.get(index) {
                    rebuilt.push((*value).to_owned());
                }
            }
            index += 1;
        } else {
            rebuilt.extend(["-w".to_owned(), "/".to_owned(), container.to_owned()]);
            rebuilt.push((*token).to_owned());
            break;
        }
    }
    if index < argv.len() {
        rebuilt.extend(argv[index + 1..].iter().map(|s| (*s).to_owned()));
    }
    rebuilt
}

fn docker_exec_option_takes_value(option: &str) -> bool {
    matches!(
        option,
        "-e" | "--env" | "-u" | "--user" | "-w" | "--workdir"
    )
}

pub(crate) fn docker<I, S>(args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect::<Vec<_>>();
    let display = docker_display(&args);
    let mut command = docker_command();
    let output = command
        .args(&args)
        .output()
        .with_context(|| format!("spawn docker {display}"))?;
    if !output.status.success() {
        bail!(
            "docker {} failed ({}): {}",
            display,
            output.status,
            redact_docker_error_text(String::from_utf8_lossy(&output.stderr).trim())
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn docker_display(args: &[std::ffi::OsString]) -> String {
    args.iter()
        .map(|arg| redact_docker_display_arg(&arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_docker_display_arg(arg: &str) -> String {
    for key in [DAEMON_AUTH_TOKEN_ENV, DAEMON_FORWARD_AUTH_TOKEN_ENV] {
        if let Some((name, _)) = arg.split_once('=') {
            if name == key {
                return format!("{key}=<redacted>");
            }
        }
    }
    arg.to_owned()
}

fn redact_docker_error_text(text: &str) -> String {
    [DAEMON_AUTH_TOKEN_ENV, DAEMON_FORWARD_AUTH_TOKEN_ENV]
        .into_iter()
        .fold(text.to_owned(), |value, key| {
            redact_key_assignments(&value, key)
        })
}

fn redact_key_assignments(text: &str, key: &str) -> String {
    let marker = format!("{key}=");
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(offset) = remaining.find(&marker) {
        output.push_str(&remaining[..offset]);
        output.push_str(&marker);
        output.push_str("<redacted>");
        let value_start = offset + marker.len();
        let value_end = remaining[value_start..]
            .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '\'' | '"' | ',' | ']'))
            .map_or(remaining.len(), |end| value_start + end);
        remaining = &remaining[value_end..];
    }
    output.push_str(remaining);
    output
}

#[cfg(test)]
fn docker_command() -> Command {
    let path = DOCKER_COMMAND_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("docker override lock")
        .clone()
        .unwrap_or_else(|| PathBuf::from("docker"));
    Command::new(path)
}

#[cfg(not(test))]
fn docker_command() -> Command {
    Command::new("docker")
}
pub fn running_container_ids<S: AsRef<str>>(label_filters: &[S]) -> Vec<String> {
    let mut args = vec!["ps".to_owned(), "-q".to_owned()];
    for filter in label_filters {
        args.push("--filter".to_owned());
        args.push(format!("label={}", filter.as_ref()));
    }
    let Ok(out) = docker(args) else {
        return Vec::new();
    };
    out.split_whitespace().map(str::to_owned).collect()
}

#[cfg(feature = "e2e-support")]
pub fn container_label(id: &str, label: &str) -> Result<String> {
    let value = docker([
        "inspect",
        "-f",
        &format!("{{{{ index .Config.Labels \"{label}\" }}}}"),
        id,
    ])?;
    if value.is_empty() || value == "<no value>" {
        bail!("missing {label} label on {id}");
    }
    Ok(value)
}

pub(crate) fn container_labels(
    ids: &[String],
) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec![
        "inspect".to_owned(),
        "-f".to_owned(),
        "{{json .Config.Labels}}".to_owned(),
    ];
    args.extend(ids.iter().cloned());
    docker(args)?
        .lines()
        .map(|line| {
            serde_json::from_str(line).with_context(|| format!("parse container labels: {line}"))
        })
        .collect()
}

#[cfg(feature = "e2e-support")]
pub fn copy_path_from_container(
    container: &str,
    remote_path: &str,
    host_dest: &Path,
) -> Result<()> {
    if let Some(parent) = host_dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create host copy dir {}", parent.display()))?;
    }
    let host_dest = host_dest
        .to_str()
        .with_context(|| format!("host path is not UTF-8: {}", host_dest.display()))?;
    docker(["cp", &format!("{container}:{remote_path}"), host_dest])?;
    Ok(())
}

fn copy_file_into(container: &str, dest_dir: &str, remote_name: &str, source: &Path) -> Result<()> {
    copy_path_into(container, dest_dir, remote_name, source, 0o755)
}

fn copy_bytes_into(
    container: &str,
    dest_dir: &str,
    remote_name: &str,
    payload: &[u8],
    mode: u32,
) -> Result<()> {
    let upload = TempUploadFile::write(payload, mode)?;
    copy_path_into(container, dest_dir, remote_name, upload.path(), mode)
}

fn path_str(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("container path is not UTF-8: {}", path.display()))
}

fn copy_path_into(
    container: &str,
    dest_dir: &str,
    remote_name: &str,
    source: &Path,
    mode: u32,
) -> Result<()> {
    validate_remote_name(remote_name)?;
    let source = source
        .to_str()
        .with_context(|| format!("host path is not UTF-8: {}", source.display()))?;
    docker([
        "cp",
        source,
        &container_copy_target(container, dest_dir, remote_name),
    ])?;
    docker([
        "exec",
        container,
        "chmod",
        &format!("{mode:o}"),
        &remote_path(dest_dir, remote_name),
    ])?;
    Ok(())
}

fn validate_remote_name(remote_name: &str) -> Result<()> {
    if remote_name.is_empty() || remote_name.contains('/') || remote_name == ".." {
        bail!("invalid remote file name {remote_name:?}");
    }
    Ok(())
}

fn container_copy_target(container: &str, dest_dir: &str, remote_name: &str) -> String {
    format!("{container}:{}", remote_path(dest_dir, remote_name))
}

fn remote_path(dest_dir: &str, remote_name: &str) -> String {
    format!("{}/{remote_name}", dest_dir.trim_end_matches('/'))
}

struct TempUploadFile {
    path: PathBuf,
}

impl TempUploadFile {
    fn write(payload: &[u8], mode: u32) -> Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "sandbox-host-upload-{}-{nanos}",
            std::process::id()
        ));
        fs::write(&path, payload).with_context(|| format!("write {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .with_context(|| format!("chmod {}", path.display()))?;
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempUploadFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn resolve_published_addr(
    container: &str,
    container_port: u16,
) -> Result<Option<SocketAddr>> {
    let out = docker(["port", container, &format!("{container_port}/tcp")])?;
    Ok(parse_published_addr(&out))
}

fn wait_for_published_addr(container: &str, container_port: u16) -> Result<SocketAddr> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(Some(addr)) = resolve_published_addr(container, container_port) {
            return Ok(addr);
        }
        if Instant::now() >= deadline {
            bail!("could not resolve published port {container_port} for {container}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn parse_published_addr(output: &str) -> Option<SocketAddr> {
    for line in output.lines() {
        let mapping = line.trim();
        let port = mapping.rsplit(':').next()?.trim();
        if let Ok(port) = port.parse::<u16>() {
            if port != 0 {
                return Some(SocketAddr::from(([127, 0, 0, 1], port)));
            }
        }
    }
    None
}
#[cfg(feature = "e2e-support")]
pub fn docker_available() -> bool {
    docker(["version", "--format", "{{.Server.Version}}"]).is_ok()
}

#[cfg(feature = "e2e-support")]
pub fn remove_labeled_containers(label: &str) -> Result<usize> {
    remove_containers_by_label_filters(&[label])
}

#[cfg(feature = "e2e-support")]
pub fn remove_containers_by_label_filters<S: AsRef<str>>(label_filters: &[S]) -> Result<usize> {
    let mut args = vec!["ps".to_owned(), "-aq".to_owned()];
    for filter in label_filters {
        args.push("--filter".to_owned());
        args.push(format!("label={}", filter.as_ref()));
    }
    let out = docker(args)?;
    let ids: Vec<&str> = out.split_whitespace().collect();
    if ids.is_empty() {
        return Ok(0);
    }
    let mut argv = vec!["rm".to_owned(), "-f".to_owned()];
    argv.extend(ids.iter().map(|id| (*id).to_owned()));
    docker(argv)?;
    Ok(ids.len())
}

#[cfg(feature = "e2e-support")]
pub fn container_ids_by_ancestor(image: &str) -> Result<Vec<String>> {
    let out = docker(["ps", "-aq", "--filter", &format!("ancestor={image}")])?;
    Ok(out.split_whitespace().map(str::to_owned).collect())
}

#[cfg(test)]
#[path = "../tests/unit/runtime.rs"]
mod tests;
