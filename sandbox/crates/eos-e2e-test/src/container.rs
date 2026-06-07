//! `DaemonContainer` — one Docker container running one `eosd`, driven entirely
//! through the Docker CLI plus the Engine archive API for binary upload.
//!
//! Container lifecycle (create / upload / spawn / teardown) is *infrastructure*,
//! not a sandbox operation, so it is allowed to use `docker` directly. The
//! operations *under test* still go exclusively through [`ProtocolClient`] over
//! the wire (D1/D4). Container-filesystem peeking is never used as a verification
//! oracle.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{Read, Write as IoWrite};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_config::ConfigPath;
use eos_protocol::ops;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::client::{is_success, ProtocolClient};
use crate::config::{Config, NodeMode};
use crate::unique_suffix;

const POOL_LABEL: &str = "eos.e2e.pool";
const AUTH_LABEL: &str = "eos.e2e.auth";
const CONFIG_DIGEST_LABEL: &str = "eos.e2e.config_sha256";

/// A live daemon container.
#[derive(Debug)]
pub struct DaemonContainer {
    name: String,
    client: ProtocolClient,
    daemon_log_path: String,
    token: String,
    keep: bool,
}

impl DaemonContainer {
    /// Create a container, upload `eosd`, spawn the daemon (TCP + auth), and
    /// block until it answers a heartbeat.
    ///
    /// # Errors
    /// Returns an error if any docker step fails or the daemon never becomes ready.
    pub fn start(config: &Config, config_yaml: &str) -> Result<Self> {
        let name = format!("eos-e2e-{}", unique_suffix());
        let token = format!("tok-{}", unique_suffix());
        let config_digest = runtime_digest(config, config_yaml)?;
        let keep = config.keep_container && config.mode != NodeMode::PerTest;

        let mut run = vec![
            "run".to_owned(),
            "-d".to_owned(),
            "--name".to_owned(),
            name.clone(),
            "--label".to_owned(),
            format!("{POOL_LABEL}={}", config.image),
            "--label".to_owned(),
            format!("{AUTH_LABEL}={token}"),
            "--label".to_owned(),
            format!("{CONFIG_DIGEST_LABEL}={config_digest}"),
        ];
        if !keep {
            run.push("--rm".to_owned());
        }
        // The isolated-workspace tier creates a per-workspace cgroup under
        // /sys/fs/cgroup, which Docker mounts read-only under plain --cap-add
        // (EROFS, e.g. on Docker Desktop). --privileged makes cgroup2 writable so
        // the real ns-holder/veth/cgroup path runs. The harness is test-only and
        // already requires SYS_ADMIN/NET_ADMIN + unconfined seccomp/apparmor, so
        // this is an acceptable superset; the explicit caps below remain for
        // documentation and hosts where privileged is unavailable.
        run.push("--privileged".to_owned());
        if let Some(platform) = &config.platform {
            run.push("--platform".to_owned());
            run.push(platform.clone());
        }
        for cap in &config.cap_add {
            run.push("--cap-add".to_owned());
            run.push(cap.clone());
        }
        for opt in &config.security_opt {
            run.push("--security-opt".to_owned());
            run.push(opt.clone());
        }
        for tmpfs in &config.tmpfs {
            run.push("--tmpfs".to_owned());
            run.push(tmpfs.clone());
        }
        run.push("--init".to_owned());
        run.push("-p".to_owned());
        run.push(format!("127.0.0.1::{}", config.tcp_port));
        run.push(config.image.clone());
        // Keep the container alive but self-terminating: `timeout` bounds the
        // lifetime so a leaked (`--rm`) container is reclaimed automatically.
        if keep {
            run.extend(["sleep".to_owned(), "infinity".to_owned()]);
        } else {
            run.extend([
                "timeout".to_owned(),
                config.non_kept_container_ttl.as_secs().to_string(),
                "sleep".to_owned(),
                "infinity".to_owned(),
            ]);
        }

        docker(&run).with_context(|| format!("docker run for {name}"))?;

        // From here, any failure must still tear the container down.
        let mut container = Self {
            name: name.clone(),
            // Placeholder client; replaced once the port is resolved.
            client: ProtocolClient::new(
                "127.0.0.1:1".parse().expect("valid placeholder addr"),
                Some(token.clone()),
                config.request_timeout,
            ),
            daemon_log_path: config
                .remote_daemon_dir
                .join("runtime.log")
                .to_string_lossy()
                .into_owned(),
            token: token.clone(),
            keep,
        };
        match container.bringup(config, &token, config_yaml) {
            Ok(client) => {
                container.client = client;
                Ok(container)
            }
            Err(err) => {
                let log = container.daemon_log().unwrap_or_default();
                drop(container);
                Err(err.context(format!("daemon bringup failed; log tail:\n{log}")))
            }
        }
    }

    /// Adopt already-running warm e2e containers for this image.
    ///
    /// Containers are accepted only when their auth label is present, their
    /// config digest matches, their published daemon port resolves, and the
    /// daemon answers heartbeat.
    pub fn adopt_healthy(config: &Config, config_yaml: &str) -> Vec<Self> {
        let Ok(digest) = runtime_digest(config, config_yaml) else {
            return Vec::new();
        };
        let out = Command::new("docker")
            .args([
                "ps",
                "-q",
                "--filter",
                &format!("label={POOL_LABEL}={}", config.image),
                "--filter",
                &format!("label={CONFIG_DIGEST_LABEL}={digest}"),
            ])
            .output();
        let Ok(out) = out else {
            return Vec::new();
        };
        if !out.status.success() {
            return Vec::new();
        }
        std::str::from_utf8(&out.stdout)
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|id| Self::adopt_one(id, config).ok())
            .collect()
    }

    fn adopt_one(id: &str, config: &Config) -> Result<Self> {
        let token = docker(&[
            "inspect".to_owned(),
            "-f".to_owned(),
            format!("{{{{ index .Config.Labels \"{AUTH_LABEL}\" }}}}"),
            id.to_owned(),
        ])?;
        if token.is_empty() || token == "<no value>" {
            bail!("missing {AUTH_LABEL} label on {id}");
        }
        let mut container = Self {
            name: id.to_owned(),
            client: ProtocolClient::new(
                "127.0.0.1:1".parse().expect("valid placeholder addr"),
                Some(token.clone()),
                config.request_timeout,
            ),
            daemon_log_path: config
                .remote_daemon_dir
                .join("runtime.log")
                .to_string_lossy()
                .into_owned(),
            token: token.clone(),
            keep: true,
        };
        let addr = container.resolve_addr(config.tcp_port)?;
        let client = ProtocolClient::new(addr, Some(token), config.request_timeout);
        container.await_ready(&client, config.ready_timeout)?;
        container.client = client;
        Ok(container)
    }

    fn bringup(&self, config: &Config, token: &str, config_yaml: &str) -> Result<ProtocolClient> {
        let daemon_dir = path_str(&config.remote_daemon_dir)?;
        let root_dir = path_str(&config.root_dir)?;
        let remote_eosd_path = path_str(&config.remote_eosd_path)?;
        let config_path = ConfigPath::prd()
            .context("resolve compiled daemon config path")?
            .as_path()
            .to_path_buf();
        let config_dir = config_path
            .parent()
            .context("compiled daemon config path has no parent")?;
        let config_dir = path_str(config_dir)?;
        let config_name = config_path
            .file_name()
            .and_then(|name| name.to_str())
            .context("compiled daemon config path has no UTF-8 file name")?;

        self.exec(&["mkdir", "-p", &daemon_dir, &root_dir, &config_dir])
            .context("mkdir daemon dirs")?;
        self.exec(&[
            "sh",
            "-lc",
            "mount -o remount,rw /sys/fs/cgroup 2>/dev/null || true; test -w /sys/fs/cgroup",
        ])
        .context("make cgroup v2 writable for isolated-workspace tests")?;
        put_archive_file(
            &self.name,
            &daemon_dir,
            "eosd",
            &config.eosd_path,
            config.request_timeout,
        )
        .with_context(|| {
            format!(
                "Docker put_archive eosd ({}) into {daemon_dir}",
                config.eosd_path.display()
            )
        })?;
        put_archive_bytes(
            &self.name,
            &config_dir,
            config_name,
            config_yaml.as_bytes(),
            0o644,
            config.request_timeout,
        )
        .with_context(|| format!("Docker put_archive merged config into {config_dir}"))?;

        // Spawn the daemon detached: `--spawn` re-execs a foreground child with
        // stdout/stderr redirected to `--log-file`, so bringup diagnostics land in
        // runtime.log (a plain foreground daemon parses but ignores `--log-file`).
        self.exec(&[
            "-d",
            &remote_eosd_path,
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
            &config.tcp_port.to_string(),
            "--auth-token",
            token,
        ])
        .context("spawn eosd daemon")?;

        let addr = self.resolve_addr(config.tcp_port)?;
        let client = ProtocolClient::new(addr, Some(token.to_owned()), config.request_timeout);
        self.await_ready(&client, config.ready_timeout)?;
        Ok(client)
    }

    /// Map the published TCP port to a host `SocketAddr` (retrying briefly while
    /// docker wires up the port binding).
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

    fn await_ready(&self, client: &ProtocolClient, budget: Duration) -> Result<()> {
        let deadline = Instant::now() + budget;
        let mut delay = Duration::from_millis(150);
        loop {
            let observed = match client.request(ops::API_V1_HEARTBEAT, "ready-probe", &json!({})) {
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

    /// The wire client for this container.
    #[must_use]
    pub fn client(&self) -> &ProtocolClient {
        &self.client
    }

    /// The container name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run a `docker exec <name> ...` against this container (lifecycle/provision
    /// only — never used as a verification oracle).
    ///
    /// # Errors
    /// Returns an error if the exec exits non-zero.
    pub fn exec(&self, argv: &[&str]) -> Result<String> {
        // `exec` argv may start with docker flags like `-d`; the container name
        // goes after them and before the command. Everything after the command
        // token is passed through verbatim.
        docker(&docker_exec_args(&self.name, argv))
    }

    /// Restart the in-container `eosd`: hard-kill (SIGKILL) the running daemon so
    /// graceful-shutdown cleanup does NOT run, clear the stale socket/pid, then
    /// re-spawn it with the same socket, pid, log, TCP port, and auth token, and
    /// block until it answers heartbeat. The published container port is owned by
    /// Docker, so the existing wire client stays valid across the restart. This
    /// exercises daemon startup-recovery paths (e.g. isolated-handle orphan
    /// reconciliation); the spawn mirrors [`Self::bringup`].
    ///
    /// # Errors
    /// Returns an error if the respawn exec fails or the daemon never becomes
    /// ready within the configured budget.
    pub fn restart_daemon(&self, config: &Config) -> Result<()> {
        let daemon_dir = path_str(&config.remote_daemon_dir)?;
        let remote_eosd_path = path_str(&config.remote_eosd_path)?;
        let teardown = format!(
            "kill -9 \"$(cat {daemon_dir}/runtime.pid 2>/dev/null)\" 2>/dev/null; \
             pkill -9 -f 'eosd daemon' 2>/dev/null; sleep 1; \
             rm -f {daemon_dir}/runtime.sock {daemon_dir}/runtime.pid"
        );
        let _ = self.exec(&["sh", "-lc", &teardown]);
        self.exec(&[
            "-d",
            &remote_eosd_path,
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
            &config.tcp_port.to_string(),
            "--auth-token",
            &self.token,
        ])
        .context("respawn eosd daemon")?;
        self.await_ready(&self.client, config.ready_timeout)
            .context("daemon not ready after restart")
    }

    /// Best-effort tail of the daemon log for diagnostics (not an oracle).
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

/// Run `docker <args...>`, returning trimmed stdout. Errors include stderr.
fn docker(args: &[String]) -> Result<String> {
    docker_str(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

fn docker_exec_args(container: &str, argv: &[&str]) -> Vec<String> {
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

fn put_archive_file(
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

fn put_archive_bytes(
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

fn path_str(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("container path is not UTF-8: {}", path.display()))
}

fn runtime_digest(config: &Config, config_yaml: &str) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(config_yaml.as_bytes());
    hasher.update(b"\0eosd\0");
    let eosd = fs::read(&config.eosd_path).with_context(|| {
        format!(
            "read eosd binary for digest: {}",
            config.eosd_path.display()
        )
    })?;
    hasher.update(eosd);
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
        out.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
    }
    out
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
         User-Agent: eos-e2e-test\r\n\
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

fn tar_single_file(name: &str, payload: &[u8], mode: u32) -> Result<Vec<u8>> {
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

/// Parse `docker port` output (`0.0.0.0:54321` / `127.0.0.1:54321`, possibly
/// multiple lines) into a loopback `SocketAddr`.
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

/// Whether a usable `docker` CLI is present (for the env guard).
#[must_use]
pub fn docker_available() -> bool {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Remove all `eos-e2e-*` containers left by prior harness runs.
///
/// # Errors
/// Returns an error if Docker is reachable but listing or removing containers
/// fails.
pub fn reap_e2e_containers() -> Result<usize> {
    let out = Command::new("docker")
        .args(["ps", "-aq", "--filter", &format!("label={POOL_LABEL}")])
        .output()
        .context("list eos-e2e containers")?;
    if !out.status.success() {
        bail!(
            "docker ps for eos-e2e containers failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let ids: Vec<&str> = std::str::from_utf8(&out.stdout)
        .unwrap_or("")
        .split_whitespace()
        .collect();
    if ids.is_empty() {
        return Ok(0);
    }
    let mut argv = vec!["rm", "-f"];
    argv.extend(ids.iter().copied());
    let output = Command::new("docker")
        .args(&argv)
        .output()
        .context("remove eos-e2e containers")?;
    if !output.status.success() {
        bail!(
            "docker {} failed ({}): {}",
            argv.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use anyhow::Result;

    use crate::config::{Config, NodeMode, WorkloadConfig};

    use super::{
        docker_exec_args, docker_http_status, docker_unix_socket_from_host, percent_encode,
        runtime_digest, tar_single_file,
    };

    #[test]
    fn tar_single_file_builds_executable_ustar_stream() {
        let tar = tar_single_file("eosd", b"payload", 0o755).expect("tar stream");
        assert_eq!(&tar[0..4], b"eosd");
        assert_eq!(&tar[100..108], b"0000755\0");
        assert_eq!(&tar[124..136], b"00000000007\0");
        assert_eq!(tar[156], b'0');
        assert_eq!(&tar[257..263], b"ustar\0");
        assert_eq!(tar.len() % 512, 0);
    }

    #[test]
    fn docker_helpers_parse_http_and_unix_host() {
        assert_eq!(
            docker_http_status("HTTP/1.1 200 OK\r\n\r\n").expect("status"),
            200
        );
        assert_eq!(
            percent_encode("/eos/runtime/daemon"),
            "%2Feos%2Fruntime%2Fdaemon"
        );
        assert_eq!(
            docker_unix_socket_from_host("unix:///var/run/docker.sock").expect("socket"),
            PathBuf::from("/var/run/docker.sock")
        );
    }

    #[test]
    fn docker_exec_args_runs_from_root_after_leading_flags() {
        assert_eq!(
            docker_exec_args("box", &["mkdir", "-p", "/testbed"]),
            vec!["exec", "-w", "/", "box", "mkdir", "-p", "/testbed"]
        );
        assert_eq!(
            docker_exec_args("box", &["-d", "/eos/runtime/daemon/eosd", "daemon"]),
            vec![
                "exec",
                "-d",
                "-w",
                "/",
                "box",
                "/eos/runtime/daemon/eosd",
                "daemon"
            ]
        );
    }

    fn digest_test_config(eosd_path: PathBuf) -> Config {
        Config {
            image: "image".to_owned(),
            platform: None,
            eosd_path,
            remote_daemon_dir: PathBuf::from("/eos/runtime/daemon"),
            remote_eosd_path: PathBuf::from("/eos/runtime/daemon/eosd"),
            root_dir: PathBuf::from("/eos/state/e2e"),
            cap_add: Vec::new(),
            security_opt: Vec::new(),
            tmpfs: Vec::new(),
            tcp_port: 37_657,
            sandboxes: 1,
            mode: NodeMode::Pool,
            recycle_after: 50,
            ready_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            base_build_timeout: Duration::from_secs(1),
            workspace_root: "/testbed".to_owned(),
            keep_container: true,
            non_kept_container_ttl: Duration::from_secs(60),
            audit_pull_limit: 100,
            workload: WorkloadConfig {
                concurrency_levels: vec![1, 3, 6, 12],
                write_iterations: 1,
                sample_count: 1,
                perf_artifact_dir: PathBuf::from("target/e2e-perf"),
                timeout: Duration::from_secs(1),
            },
        }
    }

    #[test]
    fn runtime_digest_tracks_config_and_eosd_bytes() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("eos-e2e-runtime-digest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root)?;
        let eosd_path = root.join("eosd");
        fs::write(&eosd_path, b"daemon-v1")?;
        let config = digest_test_config(eosd_path);
        let baseline = runtime_digest(
            &config,
            "daemon:\n  layer_stack:\n    auto_squash_max_depth: 100\n",
        )?;
        let override_digest = runtime_digest(
            &config,
            "daemon:\n  layer_stack:\n    auto_squash_max_depth: 8\n",
        )?;

        assert_eq!(
            baseline,
            runtime_digest(
                &config,
                "daemon:\n  layer_stack:\n    auto_squash_max_depth: 100\n",
            )?
        );
        assert_eq!(baseline.len(), 64);
        assert_ne!(baseline, override_digest);
        fs::write(&config.eosd_path, b"daemon-v2")?;
        let rebuilt_digest = runtime_digest(
            &config,
            "daemon:\n  layer_stack:\n    auto_squash_max_depth: 100\n",
        )?;
        assert_ne!(baseline, rebuilt_digest);

        let _ = fs::remove_dir_all(root);
        Ok(())
    }
}
