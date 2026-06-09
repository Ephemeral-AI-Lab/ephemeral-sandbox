//! `DaemonContainer` — one Docker container running one `eosd`, driven entirely
//! through the Docker CLI plus the Engine archive API for binary upload.
//!
//! Container lifecycle (create / upload / spawn / teardown) is *infrastructure*,
//! not a sandbox operation, so it is allowed to use `docker` directly. The
//! operations *under test* still go exclusively through [`ProtocolClient`] over
//! the wire (D1/D4). Container-filesystem peeking is never used as a verification
//! oracle.

use std::net::SocketAddr;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_config::ConfigPath;
use eos_protocol::ops;
use serde_json::json;

use crate::client::{is_success, ProtocolClient};
use crate::config::{Config, NodeMode};
use crate::docker::{
    docker, docker_exec_args, parse_published_addr, path_str, put_archive_bytes, put_archive_file,
    runtime_digest,
};
use crate::unique_suffix;

pub use crate::docker::{docker_available, reap_e2e_containers};

pub(crate) const POOL_LABEL: &str = "eos.e2e.pool";
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
