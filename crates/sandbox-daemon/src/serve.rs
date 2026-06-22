//! Daemon serve subcommand adapter.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use sandbox_runtime_config::configs::{
    daemon::{DaemonConfig, DaemonServerConfig},
    runtime::RuntimeConfig,
};

const DAEMON_AUTH_TOKEN_ENV: &str = "SANDBOX_DAEMON_AUTH_TOKEN";
const DAEMON_CONFIG_YAML_ENV: &str = "SANDBOX_DAEMON_CONFIG_YAML";

/// Start, spawn, or call the async RPC server.
///
/// Modes:
/// - `sandbox-daemon serve --config-yaml PATH --workspace-root PATH --socket PATH --pid-file PATH ...`
///   runs the foreground server.
/// - `sandbox-daemon serve --spawn --config-yaml PATH --workspace-root PATH --socket PATH
///   --pid-file PATH ...` starts a detached foreground child and returns.
pub(crate) fn run(args: std::env::Args) -> Result<()> {
    let args = args.collect::<Vec<_>>();
    let config_path = daemon_config_path_arg(&args)?;
    let runtime_config = load_runtime_config(&config_path)?;
    let daemon_config = &runtime_config.daemon;
    let config = DaemonCliConfig::parse(args, &daemon_config.server, config_path)?;
    if config.spawn {
        return spawn_daemon(&config);
    }
    set_runner_config_env(&config.config_yaml_path);
    let workspace_root = config.workspace_root.clone();
    let server_config = sandbox_daemon::ServerConfig {
        socket_path: config.socket_path,
        pid_path: config.pid_path,
        tcp_host: config.tcp_host,
        tcp_port: config.tcp_port,
        auth_token: config.auth_token,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(daemon_worker_threads(
            daemon_config.server.max_worker_threads,
        ))
        .enable_all()
        .build()
        .context("failed to build daemon tokio runtime")?;
    runtime.block_on(async move {
        let server = sandbox_daemon::SandboxDaemonServer::new(
            server_config,
            Arc::new(build_runtime_operations(&runtime_config, workspace_root)),
        );
        server.serve().await
    })?;
    Ok(())
}

struct DaemonRuntimeConfig {
    daemon: DaemonConfig,
    runtime: RuntimeConfig,
}

fn build_runtime_operations(
    config: &DaemonRuntimeConfig,
    workspace_root: PathBuf,
) -> sandbox_runtime::SandboxRuntimeOperations {
    sandbox_runtime::SandboxRuntimeOperations::from_config(sandbox_runtime::SandboxRuntimeConfig {
        workspace: sandbox_runtime::WorkspaceRuntimeConfig {
            workspace_root,
            layer_stack_root: config.runtime.workspace.layer_stack_root.clone(),
            scratch_root: config.runtime.workspace.scratch_root.clone(),
            caps: sandbox_runtime::WorkspaceResourceCaps {
                ttl_s: config.runtime.workspace.ttl_s,
                total_cap: config.runtime.workspace.total_cap,
                upperdir_bytes: config.runtime.workspace.upperdir_bytes,
                memavail_fraction: config.runtime.workspace.memavail_fraction,
                setup_timeout_s: config.runtime.workspace.setup_timeout_s,
                exit_grace_s: config.runtime.workspace.exit_grace_s,
                rfc1918_egress: match config.runtime.workspace.rfc1918_egress {
                    sandbox_runtime_config::configs::runtime::Rfc1918Egress::Allow => {
                        sandbox_runtime::Rfc1918Egress::Allow
                    }
                    sandbox_runtime_config::configs::runtime::Rfc1918Egress::Deny => {
                        sandbox_runtime::Rfc1918Egress::Deny
                    }
                },
            },
        },
        command: sandbox_runtime::CommandRuntimeConfig {
            scratch_root: config.daemon.commands.scratch_root.clone(),
        },
        cgroup_monitor: sandbox_runtime::CgroupMonitorRuntimeConfig {
            enabled: config.daemon.cgroup_monitor.enabled,
            sample_interval_ms: config.daemon.cgroup_monitor.sample_interval_ms,
            retained_samples_per_target: config.daemon.cgroup_monitor.retained_samples_per_target,
            include_pids: config.daemon.cgroup_monitor.include_pids,
            include_pressure: config.daemon.cgroup_monitor.include_pressure,
            include_disk: config.daemon.cgroup_monitor.include_disk,
        },
    })
}

fn load_runtime_config(path: &Path) -> Result<DaemonRuntimeConfig> {
    let doc = sandbox_runtime_config::load_path(path)
        .with_context(|| format!("load daemon config {}", path.display()))?;
    let daemon = doc
        .section::<DaemonConfig>("daemon")
        .context("deserialize daemon config section")?;
    daemon.validate().context("validate daemon config")?;
    let runtime = doc
        .section::<RuntimeConfig>("runtime")
        .context("deserialize runtime config section")?;
    runtime.validate().context("validate runtime config")?;
    Ok(DaemonRuntimeConfig { daemon, runtime })
}

fn daemon_worker_threads(max_worker_threads: usize) -> usize {
    std::thread::available_parallelism()
        .map_or(max_worker_threads, |threads| {
            threads.get().min(max_worker_threads)
        })
        .max(1)
}

pub(crate) struct DaemonCliConfig {
    pub(crate) config_yaml_path: PathBuf,
    workspace_root: PathBuf,
    socket_path: PathBuf,
    pid_path: PathBuf,
    tcp_host: Option<String>,
    tcp_port: Option<u16>,
    pub(crate) auth_token: Option<String>,
    spawn: bool,
}

impl DaemonCliConfig {
    pub(crate) fn parse(
        args: impl IntoIterator<Item = String>,
        server_defaults: &DaemonServerConfig,
        explicit_config_path: PathBuf,
    ) -> Result<Self> {
        let mut config_yaml_path = explicit_config_path;
        let mut workspace_root = None;
        let mut socket_path = server_defaults.socket_path.clone();
        let mut pid_path = server_defaults.pid_path.clone();
        let mut tcp_host = None;
        let mut tcp_port = None;
        let mut auth_token = None;
        let mut spawn = false;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config-yaml" => {
                    config_yaml_path = PathBuf::from(required_arg(&mut args, "--config-yaml")?);
                }
                "--workspace-root" => {
                    workspace_root =
                        Some(PathBuf::from(required_arg(&mut args, "--workspace-root")?));
                }
                "--socket" => socket_path = PathBuf::from(required_arg(&mut args, "--socket")?),
                "--pid-file" => pid_path = PathBuf::from(required_arg(&mut args, "--pid-file")?),
                "--tcp-host" => tcp_host = Some(required_arg(&mut args, "--tcp-host")?),
                "--tcp-port" => {
                    tcp_port = Some(
                        required_arg(&mut args, "--tcp-port")?
                            .parse::<u16>()
                            .context("--tcp-port must be an integer 1..65535")?,
                    );
                }
                "--auth-token" => auth_token = Some(required_arg(&mut args, "--auth-token")?),
                "--spawn" => spawn = true,
                "--help" | "-h" => {
                    println!(
                        "usage: serve [--spawn] --config-yaml PATH --workspace-root PATH [--socket PATH] [--pid-file PATH] [--tcp-host HOST --tcp-port PORT --auth-token TOKEN]"
                    );
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown daemon flag {other:?}")),
            }
        }
        let workspace_root =
            workspace_root.ok_or_else(|| anyhow!("serve requires --workspace-root PATH"))?;
        if !workspace_root.is_absolute() {
            return Err(anyhow!(
                "--workspace-root must be absolute: {}",
                workspace_root.display()
            ));
        }
        let resolved_auth_token = auth_token.or_else(|| std::env::var(DAEMON_AUTH_TOKEN_ENV).ok());
        if tcp_host.is_some()
            && tcp_port.is_some()
            && !has_configured_token(resolved_auth_token.as_deref())
        {
            return Err(anyhow!(
                "serve TCP listener requires --auth-token or SANDBOX_DAEMON_AUTH_TOKEN"
            ));
        }
        Ok(Self {
            config_yaml_path,
            workspace_root,
            socket_path,
            pid_path,
            tcp_host,
            tcp_port,
            auth_token: resolved_auth_token,
            spawn,
        })
    }

    pub(crate) fn foreground_args(&self) -> Vec<String> {
        let mut args = vec![
            "serve".to_owned(),
            "--config-yaml".to_owned(),
            self.config_yaml_path.to_string_lossy().into_owned(),
            "--workspace-root".to_owned(),
            self.workspace_root.to_string_lossy().into_owned(),
            "--socket".to_owned(),
            self.socket_path.to_string_lossy().into_owned(),
            "--pid-file".to_owned(),
            self.pid_path.to_string_lossy().into_owned(),
        ];
        if let Some(host) = &self.tcp_host {
            args.push("--tcp-host".to_owned());
            args.push(host.clone());
        }
        if let Some(port) = self.tcp_port {
            args.push("--tcp-port".to_owned());
            args.push(port.to_string());
        }
        args
    }
}

fn has_configured_token(token: Option<&str>) -> bool {
    token.is_some_and(|token| !token.is_empty())
}

pub(crate) fn daemon_config_path_arg(args: &[String]) -> Result<PathBuf> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--config-yaml" {
            let path = iter
                .next()
                .ok_or_else(|| anyhow!("--config-yaml requires a value"))?;
            return Ok(PathBuf::from(path));
        }
    }
    Err(anyhow!("serve requires --config-yaml PATH"))
}

fn required_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("{flag} requires a value"))
}

fn spawn_daemon(config: &DaemonCliConfig) -> Result<()> {
    if daemon_already_running(&config.pid_path, &config.socket_path) {
        return Ok(());
    }
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create socket dir {}", parent.display()))?;
    }
    if let Some(parent) = config.pid_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create pid dir {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&config.socket_path);
    let _ = std::fs::remove_file(&config.pid_path);

    let executable = std::env::current_exe().context("failed to resolve daemon executable")?;
    let mut command = Command::new(executable);
    command.args(config.foreground_args());
    command.env(DAEMON_CONFIG_YAML_ENV, &config.config_yaml_path);
    if let Some(token) = &config.auth_token {
        command.env(DAEMON_AUTH_TOKEN_ENV, token);
    }
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command.spawn().context("failed to spawn daemon")?;
    Ok(())
}

fn set_runner_config_env(config_yaml_path: &Path) {
    std::env::set_var(DAEMON_CONFIG_YAML_ENV, config_yaml_path);
}

fn daemon_already_running(pid_path: &Path, socket_path: &Path) -> bool {
    if !socket_path.exists() {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(pid_path) else {
        return false;
    };
    let Ok(pid) = raw.trim().parse::<u32>() else {
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        PathBuf::from(format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        pid > 0
    }
}
