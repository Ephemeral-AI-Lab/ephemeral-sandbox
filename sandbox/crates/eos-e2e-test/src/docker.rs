//! Docker CLI/API helpers for the live E2E harness.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{Read, Write as IoWrite};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::container::POOL_LABEL;
use crate::tar::tar_single_file;

/// Run `docker <args...>`, returning trimmed stdout. Errors include stderr.
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

pub(crate) fn runtime_digest(config: &Config, config_yaml: &str) -> Result<String> {
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

/// Parse `docker port` output (`0.0.0.0:54321` / `127.0.0.1:54321`, possibly
/// multiple lines) into a loopback `SocketAddr`.
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
        runtime_digest,
    };

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
