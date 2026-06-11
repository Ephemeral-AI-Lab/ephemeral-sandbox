//! Docker-backed daemon container runtime and helper plumbing.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{Read, Write as IoWrite};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::protocol::{is_success, ProtocolClient, HEARTBEAT_OP};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerLifetime {
    Keep,
    SelfDestruct { ttl: Duration },
}

#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub platform: Option<String>,
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
    keep: bool,
}

impl DaemonContainer {
    pub fn start(
        container: &ContainerSpec,
        daemon: &DaemonSpec,
        auth_token: String,
    ) -> Result<Self> {
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
        // The isolated-workspace tier creates a per-workspace cgroup under
        // /sys/fs/cgroup, which Docker mounts read-only under plain --cap-add
        // (EROFS, e.g. on Docker Desktop). --privileged makes cgroup2 writable so
        // the real ns-holder/veth/cgroup path runs. Sandboxes already require
        // SYS_ADMIN/NET_ADMIN + unconfined seccomp/apparmor, so this is an
        // acceptable superset; the explicit caps below remain for documentation
        // and hosts where privileged is unavailable.
        run.push("--privileged".to_owned());
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
        run.push(format!("127.0.0.1::{}", daemon.tcp_port));
        run.push(container.image.clone());
        // Keep the container alive but self-terminating: `timeout` bounds the
        // lifetime so a leaked (`--rm`) container is reclaimed automatically.
        match container.lifetime {
            ContainerLifetime::Keep => run.extend(["sleep".to_owned(), "infinity".to_owned()]),
            ContainerLifetime::SelfDestruct { ttl } => run.extend([
                "timeout".to_owned(),
                ttl.as_secs().to_string(),
                "sleep".to_owned(),
                "infinity".to_owned(),
            ]),
        }

        docker(&run).with_context(|| format!("docker run for {}", container.name))?;

        // From here, any failure must still tear the container down.
        let mut handle = Self::handle(
            container.name.clone(),
            auth_token.clone(),
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

    #[must_use]
    pub(crate) fn for_engine(
        name: String,
        auth_token: String,
        daemon: &DaemonSpec,
        endpoint: Option<SocketAddr>,
    ) -> Self {
        Self::handle(name, auth_token, daemon, endpoint, true)
    }

    pub fn adopt(id: &str, auth_token: String, daemon: &DaemonSpec) -> Result<Self> {
        let mut handle = Self::handle(id.to_owned(), auth_token.clone(), daemon, None, true);
        let addr = handle.resolve_addr(daemon.tcp_port)?;
        let client = ProtocolClient::new(addr, Some(auth_token), daemon.request_timeout);
        await_ready(&client, daemon.ready_timeout)?;
        handle.client = client;
        Ok(handle)
    }

    fn handle(
        name: String,
        auth_token: String,
        daemon: &DaemonSpec,
        endpoint: Option<SocketAddr>,
        keep: bool,
    ) -> Self {
        Self {
            name,
            client: ProtocolClient::new(
                endpoint.unwrap_or_else(placeholder_addr),
                Some(auth_token.clone()),
                daemon.request_timeout,
            ),
            daemon_log_path: daemon
                .remote_daemon_dir
                .join("runtime.log")
                .to_string_lossy()
                .into_owned(),
            token: auth_token,
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
        put_archive_file(
            &self.name,
            &daemon_dir,
            "eosd",
            &daemon.eosd_path,
            daemon.request_timeout,
        )
        .with_context(|| {
            format!(
                "Docker put_archive eosd ({}) into {daemon_dir}",
                daemon.eosd_path.display()
            )
        })?;
        put_archive_bytes(
            &self.name,
            &config_dir,
            config_name,
            daemon.config_yaml.as_bytes(),
            0o644,
            daemon.request_timeout,
        )
        .with_context(|| format!("Docker put_archive merged config into {config_dir}"))?;

        self.spawn_daemon(&daemon_dir, &remote_eosd_path, daemon.tcp_port)
            .context("spawn eosd daemon")?;

        let addr = self.resolve_addr(daemon.tcp_port)?;
        let client = ProtocolClient::new(addr, Some(self.token.clone()), daemon.request_timeout);
        await_ready(&client, daemon.ready_timeout)?;
        Ok(client)
    }

    fn resolve_addr(&self, container_port: u16) -> Result<SocketAddr> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(out) = docker(&[
                "port".to_owned(),
                self.name.clone(),
                format!("{container_port}/tcp"),
            ]) {
                if let Some(addr) = parse_published_addr(&out) {
                    return Ok(addr);
                }
            }
            if Instant::now() >= deadline {
                bail!(
                    "could not resolve published port {container_port} for {}",
                    self.name
                );
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    #[must_use]
    pub fn client(&self) -> &ProtocolClient {
        &self.client
    }

    pub fn exec(&self, argv: &[&str]) -> Result<String> {
        // `exec` argv may start with docker flags like `-d`; the container name
        // goes after them and before the command. Everything after the command
        // token is passed through verbatim.
        docker(&docker_exec_args(&self.name, argv))
    }

    pub fn restart_daemon(&self, daemon: &DaemonSpec) -> Result<()> {
        let daemon_dir = path_str(&daemon.remote_daemon_dir)?;
        let remote_eosd_path = path_str(&daemon.remote_eosd_path)?;
        let teardown = format!(
            "kill -9 \"$(cat {daemon_dir}/runtime.pid 2>/dev/null)\" 2>/dev/null; \
             pkill -9 -f 'eosd daemon' 2>/dev/null; sleep 1; \
             rm -f {daemon_dir}/runtime.sock {daemon_dir}/runtime.pid"
        );
        let _ = self.exec(&["sh", "-lc", &teardown]);
        self.spawn_daemon(&daemon_dir, &remote_eosd_path, daemon.tcp_port)
            .context("respawn eosd daemon")?;
        await_ready(&self.client, daemon.ready_timeout).context("daemon not ready after restart")
    }

    fn spawn_daemon(
        &self,
        daemon_dir: &str,
        remote_eosd_path: &str,
        tcp_port: u16,
    ) -> Result<String> {
        self.exec(&[
            "-d",
            remote_eosd_path,
            "daemon",
            "--spawn",
            "--socket",
            &format!("{daemon_dir}/runtime.sock"),
            "--pid-file",
            &format!("{daemon_dir}/runtime.pid"),
            "--log-file",
            &format!("{daemon_dir}/runtime.log"),
            "--tcp-host",
            "0.0.0.0",
            "--tcp-port",
            &tcp_port.to_string(),
            "--auth-token",
            &self.token,
        ])
    }

    fn daemon_log(&self) -> Option<String> {
        docker(&[
            "exec".to_owned(),
            self.name.clone(),
            "tail".to_owned(),
            "-n".to_owned(),
            "40".to_owned(),
            self.daemon_log_path.clone(),
        ])
        .ok()
    }
}

impl Drop for DaemonContainer {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = docker(&["rm".to_owned(), "-f".to_owned(), self.name.clone()]);
    }
}

fn placeholder_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 1))
}

/// The bring-up ready gate: poll heartbeat until the daemon answers with
/// success, with exponential backoff. `sandbox.runtime.ready` cannot gate
/// provisioning — its `control_plane` probe requires a seeded workspace base
/// (see [`crate::protocol::HEARTBEAT_OP`]).
fn await_ready(client: &ProtocolClient, budget: Duration) -> Result<()> {
    let deadline = Instant::now() + budget;
    let mut delay = Duration::from_millis(150);
    loop {
        let observed = match client.request(HEARTBEAT_OP, "ready-probe", &json!({})) {
            Ok(resp) if is_success(&resp) => return Ok(()),
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

pub(crate) fn docker(args: &[String]) -> Result<String> {
    docker_str(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

pub(crate) fn docker_exec_args(container: &str, argv: &[&str]) -> Vec<String> {
    let mut rebuilt: Vec<String> = vec!["exec".to_owned()];
    let mut rest = argv.iter();
    for token in rest.by_ref() {
        if token.starts_with('-') {
            rebuilt.push((*token).to_owned());
        } else {
            rebuilt.extend(["-w".to_owned(), "/".to_owned(), container.to_owned()]);
            rebuilt.push((*token).to_owned());
            break;
        }
    }
    rebuilt.extend(rest.map(|s| (*s).to_owned()));
    rebuilt
}

fn docker_str(args: &[&str]) -> Result<String> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .with_context(|| format!("spawn docker {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "docker {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[must_use]
pub fn running_container_ids(label_filters: &[String]) -> Vec<String> {
    let mut args = vec!["ps".to_owned(), "-q".to_owned()];
    for filter in label_filters {
        args.push("--filter".to_owned());
        args.push(format!("label={filter}"));
    }
    let Ok(out) = docker(&args) else {
        return Vec::new();
    };
    out.split_whitespace().map(str::to_owned).collect()
}

pub fn container_label(id: &str, label: &str) -> Result<String> {
    let value = docker(&[
        "inspect".to_owned(),
        "-f".to_owned(),
        format!("{{{{ index .Config.Labels \"{label}\" }}}}"),
        id.to_owned(),
    ])?;
    if value.is_empty() || value == "<no value>" {
        bail!("missing {label} label on {id}");
    }
    Ok(value)
}

pub fn container_labels(ids: &[String]) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec![
        "inspect".to_owned(),
        "-f".to_owned(),
        "{{json .Config.Labels}}".to_owned(),
    ];
    args.extend(ids.iter().cloned());
    docker(&args)?
        .lines()
        .map(|line| {
            serde_json::from_str(line).with_context(|| format!("parse container labels: {line}"))
        })
        .collect()
}

pub(crate) fn put_archive_file(
    container: &str,
    dest_dir: &str,
    remote_name: &str,
    source: &Path,
    timeout: Duration,
) -> Result<()> {
    let payload = fs::read(source).with_context(|| format!("read eosd {}", source.display()))?;
    let tar_stream = tar_single_file(remote_name, &payload, 0o755)?;
    docker_put_archive(container, dest_dir, &tar_stream, timeout)
}

pub(crate) fn put_archive_bytes(
    container: &str,
    dest_dir: &str,
    remote_name: &str,
    payload: &[u8],
    mode: u32,
    timeout: Duration,
) -> Result<()> {
    let tar_stream = tar_single_file(remote_name, payload, mode)?;
    docker_put_archive(container, dest_dir, &tar_stream, timeout)
}

pub(crate) fn path_str(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("container path is not UTF-8: {}", path.display()))
}

#[cfg(unix)]
fn docker_put_archive(
    container: &str,
    dest_dir: &str,
    tar_stream: &[u8],
    timeout: Duration,
) -> Result<()> {
    use std::os::unix::net::UnixStream;

    let socket = docker_socket_path()?;
    let mut stream = UnixStream::connect(&socket)
        .with_context(|| format!("connect Docker socket {}", socket.display()))?;
    stream
        .set_read_timeout(Some(timeout))
        .context("set Docker socket read timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("set Docker socket write timeout")?;

    let api_version = docker_api_version();
    let request_path = format!(
        "/v{}/containers/{}/archive?path={}",
        api_version.trim_start_matches('v'),
        percent_encode(container),
        percent_encode(dest_dir)
    );
    let request = format!(
        "PUT {request_path} HTTP/1.1\r\n\
         Host: docker\r\n\
         User-Agent: eos-sandbox-host\r\n\
         Content-Type: application/x-tar\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        tar_stream.len()
    );
    stream
        .write_all(request.as_bytes())
        .context("write Docker put_archive request headers")?;
    stream
        .write_all(tar_stream)
        .context("write Docker put_archive tar stream")?;
    stream.flush().context("flush Docker put_archive request")?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .context("read Docker put_archive response")?;
    let response_text = String::from_utf8_lossy(&response);
    let status = docker_http_status(&response_text)?;
    if !(200..300).contains(&status) {
        bail!("Docker put_archive failed with HTTP {status}: {response_text}");
    }
    Ok(())
}

#[cfg(not(unix))]
fn docker_put_archive(
    _container: &str,
    _dest_dir: &str,
    _tar_stream: &[u8],
    _timeout: Duration,
) -> Result<()> {
    bail!("Docker put_archive over a Unix socket is only supported on Unix hosts")
}

#[cfg(unix)]
fn docker_socket_path() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        if let Some(path) = docker_unix_socket_from_host(&host) {
            candidates.push(path);
        }
    }
    if let Ok(host) = docker_str(&[
        "context",
        "inspect",
        "--format",
        "{{.Endpoints.docker.Host}}",
    ]) {
        if let Some(path) = docker_unix_socket_from_host(&host) {
            candidates.push(path);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".docker/run/docker.sock"));
    }
    candidates.push(PathBuf::from("/var/run/docker.sock"));

    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| anyhow::anyhow!("could not locate Docker Unix socket for put_archive"))
}

fn docker_unix_socket_from_host(host: &str) -> Option<PathBuf> {
    host.trim()
        .strip_prefix("unix://")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn docker_api_version() -> String {
    docker_str(&["version", "--format", "{{.Server.APIVersion}}"])
        .ok()
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| "1.41".to_owned())
}

fn docker_http_status(response: &str) -> Result<u16> {
    let status_line = response
        .lines()
        .next()
        .context("Docker put_archive response missing status line")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .context("Docker put_archive response missing status code")?;
    status
        .parse::<u16>()
        .with_context(|| format!("parse Docker HTTP status from {status_line:?}"))
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(&mut encoded, "%{byte:02X}");
        }
    }
    encoded
}

pub(crate) fn resolve_published_addr(
    container: &str,
    container_port: u16,
) -> Result<Option<SocketAddr>> {
    let out = docker(&[
        "port".to_owned(),
        container.to_owned(),
        format!("{container_port}/tcp"),
    ])?;
    Ok(parse_published_addr(&out))
}

pub(crate) fn parse_published_addr(output: &str) -> Option<SocketAddr> {
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

#[must_use]
pub fn docker_available() -> bool {
    docker_str(&["version", "--format", "{{.Server.Version}}"]).is_ok()
}

pub fn remove_labeled_containers(label: &str) -> Result<usize> {
    let out = docker(&[
        "ps".to_owned(),
        "-aq".to_owned(),
        "--filter".to_owned(),
        format!("label={label}"),
    ])?;
    let ids: Vec<&str> = out.split_whitespace().collect();
    if ids.is_empty() {
        return Ok(0);
    }
    let mut argv = vec!["rm".to_owned(), "-f".to_owned()];
    argv.extend(ids.iter().map(|id| (*id).to_owned()));
    docker(&argv)?;
    Ok(ids.len())
}

pub(crate) fn tar_single_file(name: &str, payload: &[u8], mode: u32) -> Result<Vec<u8>> {
    if name.is_empty() || name.starts_with('/') || name.split('/').any(|part| part == "..") {
        bail!("invalid tar entry name {name:?}");
    }
    let name_bytes = name.as_bytes();
    if name_bytes.len() > 100 {
        bail!("tar entry name too long: {name}");
    }

    let mut header = [0_u8; 512];
    header[..name_bytes.len()].copy_from_slice(name_bytes);
    write_octal(&mut header[100..108], u64::from(mode))?;
    write_octal(&mut header[108..116], 0)?;
    write_octal(&mut header[116..124], 0)?;
    write_octal(&mut header[124..136], payload.len() as u64)?;
    write_octal(&mut header[136..148], 0)?;
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum = header.iter().map(|byte| u32::from(*byte)).sum::<u32>();
    write_checksum(&mut header[148..156], checksum)?;

    let mut archive = Vec::with_capacity(512 + payload.len() + 1536);
    archive.extend_from_slice(&header);
    archive.extend_from_slice(payload);
    let padding = (512 - (payload.len() % 512)) % 512;
    archive.resize(archive.len() + padding, 0);
    archive.resize(archive.len() + 1024, 0);
    Ok(archive)
}

fn write_octal(field: &mut [u8], value: u64) -> Result<()> {
    let digits = field
        .len()
        .checked_sub(1)
        .context("tar octal field too short")?;
    let encoded = format!("{value:0width$o}", width = digits);
    if encoded.len() > digits {
        bail!(
            "tar octal value {value} does not fit in {} bytes",
            field.len()
        );
    }
    field[..digits].copy_from_slice(encoded.as_bytes());
    field[digits] = 0;
    Ok(())
}

fn write_checksum(field: &mut [u8], value: u32) -> Result<()> {
    if field.len() != 8 {
        bail!("tar checksum field must be 8 bytes");
    }
    let encoded = format!("{value:06o}");
    if encoded.len() > 6 {
        bail!("tar checksum {value} does not fit");
    }
    field[..6].copy_from_slice(encoded.as_bytes());
    field[6] = 0;
    field[7] = b' ';
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/runtime.rs"]
mod tests;
