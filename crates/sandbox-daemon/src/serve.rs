//! Daemon serve subcommand adapter.

use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use config::configs::{
    daemon::{DaemonConfig, DaemonServerConfig},
    isolated::IsolatedNetworkConfig,
};
use config::ConfigPath;

const DAEMON_AUTH_TOKEN_ENV: &str = "EOS_DAEMON_AUTH_TOKEN";
const DAEMON_CONFIG_YAML_ENV: &str = "EOS_DAEMON_CONFIG_YAML";
const CONNECT_FAILED: i32 = 97;
const IO_FAILED: i32 = 98;

/// Start, spawn, or call the async RPC server.
///
/// Modes:
/// - `sandbox-daemon serve --socket PATH --pid-file PATH ...` runs the foreground server.
/// - `sandbox-daemon serve --spawn --socket PATH --pid-file PATH ...` starts a
///   detached foreground child and returns.
/// - `eosd daemon --client SOCKET JSON` remains the compatibility client for
///   `thin_client.py`, preserving exit codes 97/98.
pub(crate) fn run(args: std::env::Args, subcommand: ServeSubcommand) -> Result<()> {
    let args = args.collect::<Vec<_>>();
    let config_path = daemon_config_path_arg(&args)?;
    let runtime_config = load_runtime_config(config_path.as_deref())?;
    let daemon_config = &runtime_config.daemon;
    let config = DaemonCliConfig::parse(args, &daemon_config.server, config_path, subcommand)?;
    if let Some((socket_path, payload)) = config.client {
        return run_client_request(&socket_path, &payload);
    }
    if config.spawn {
        return spawn_daemon(&config);
    }
    set_runner_config_env(&config.config_yaml_path);
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
            Arc::new(build_runtime_operations(&runtime_config)),
        );
        server.serve().await
    })?;
    Ok(())
}

struct DaemonRuntimeConfig {
    daemon: DaemonConfig,
    isolated: IsolatedNetworkConfig,
}

fn build_runtime_operations(
    config: &DaemonRuntimeConfig,
) -> sandbox_runtime::SandboxDaemonOperations {
    let caps = workspace_resource_caps(&config.isolated);
    let workspace_runtime = Arc::new(workspace::WorkspaceRuntimeService::new(
        workspace::profile::WorkspaceModeManager::new(caps, config.isolated.scratch_root.clone()),
    ));
    let workspace_session = Arc::new(
        sandbox_runtime::workspace_session::WorkspaceSessionService::new(workspace_runtime),
    );
    let command = Arc::new(sandbox_runtime::CommandOperationService::new(
        workspace_session,
        command_config(&config.daemon.commands),
    ));
    sandbox_runtime::SandboxDaemonOperations::new(command)
}

fn workspace_resource_caps(config: &IsolatedNetworkConfig) -> workspace::profile::ResourceCaps {
    workspace::profile::ResourceCaps {
        ttl_s: config.ttl_s,
        total_cap: config.total_cap,
        upperdir_bytes: config.upperdir_bytes,
        memavail_fraction: config.memavail_fraction,
        setup_timeout_s: config.setup_timeout_s,
        exit_grace_s: config.exit_grace_s,
        rfc1918_egress: match config.rfc1918_egress {
            config::configs::isolated::Rfc1918Egress::Allow => {
                workspace::profile::Rfc1918Egress::Allow
            }
            config::configs::isolated::Rfc1918Egress::Deny => {
                workspace::profile::Rfc1918Egress::Deny
            }
        },
        fallback_dns: config.fallback_dns.clone(),
        eos_workspace_root: config.workspace_root.to_string_lossy().into_owned(),
    }
}

fn command_config(config: &config::configs::daemon::CommandConfig) -> command::CommandConfig {
    command::CommandConfig {
        scratch_root: config.scratch_root.clone(),
    }
}

fn load_runtime_config(path: Option<&Path>) -> Result<DaemonRuntimeConfig> {
    let doc = if let Some(path) = path {
        config::load_path(path).with_context(|| format!("load daemon config {}", path.display()))?
    } else {
        config::load_prd().context("load eos-sandbox/config/prd.yml")?
    };
    let daemon = doc
        .section::<DaemonConfig>("daemon")
        .context("deserialize daemon config section")?;
    daemon.validate().context("validate daemon config")?;
    let isolated = doc
        .section::<IsolatedNetworkConfig>("isolated")
        .context("deserialize isolated config section")?;
    isolated.validate().context("validate isolated config")?;
    Ok(DaemonRuntimeConfig { daemon, isolated })
}

fn daemon_worker_threads(max_worker_threads: usize) -> usize {
    std::thread::available_parallelism()
        .map_or(max_worker_threads, |threads| {
            threads.get().min(max_worker_threads)
        })
        .max(1)
}

pub(crate) struct DaemonCliConfig {
    subcommand: ServeSubcommand,
    pub(crate) config_yaml_path: PathBuf,
    socket_path: PathBuf,
    pid_path: PathBuf,
    tcp_host: Option<String>,
    tcp_port: Option<u16>,
    pub(crate) auth_token: Option<String>,
    spawn: bool,
    client: Option<(PathBuf, String)>,
}

impl DaemonCliConfig {
    pub(crate) fn parse(
        args: impl IntoIterator<Item = String>,
        server_defaults: &DaemonServerConfig,
        explicit_config_path: Option<PathBuf>,
        subcommand: ServeSubcommand,
    ) -> Result<Self> {
        let mut config_yaml_path = match explicit_config_path {
            Some(path) => path,
            None => ConfigPath::prd()?.as_path().to_path_buf(),
        };
        let mut socket_path = server_defaults.socket_path.clone();
        let mut pid_path = server_defaults.pid_path.clone();
        let mut tcp_host = None;
        let mut tcp_port = None;
        let mut auth_token = None;
        let mut spawn = false;
        let mut client = None;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config-yaml" => {
                    config_yaml_path = PathBuf::from(required_arg(&mut args, "--config-yaml")?);
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
                "--client" => {
                    let socket = PathBuf::from(required_arg(&mut args, "--client <socket>")?);
                    let payload = required_arg(&mut args, "--client <socket> <payload>")?;
                    client = Some((socket, payload));
                }
                "--help" | "-h" => {
                    println!(
                        "usage: {} [--spawn] [--config-yaml PATH] [--socket PATH] [--pid-file PATH] [--tcp-host HOST --tcp-port PORT --auth-token TOKEN] | {} --client SOCKET JSON",
                        subcommand.name(),
                        subcommand.name()
                    );
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown daemon flag {other:?}")),
            }
        }
        Ok(Self {
            subcommand,
            config_yaml_path,
            socket_path,
            pid_path,
            tcp_host,
            tcp_port,
            auth_token: auth_token.or_else(|| std::env::var(DAEMON_AUTH_TOKEN_ENV).ok()),
            spawn,
            client,
        })
    }

    pub(crate) fn foreground_args(&self) -> Vec<String> {
        let mut args = vec![
            self.subcommand.name().to_owned(),
            "--config-yaml".to_owned(),
            self.config_yaml_path.to_string_lossy().into_owned(),
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

pub(crate) fn daemon_config_path_arg(args: &[String]) -> Result<Option<PathBuf>> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--config-yaml" {
            let path = iter
                .next()
                .ok_or_else(|| anyhow!("--config-yaml requires a value"))?;
            return Ok(Some(PathBuf::from(path)));
        }
    }
    Ok(None)
}

fn required_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("{flag} requires a value"))
}

#[cfg(unix)]
fn run_client_request(socket_path: &PathBuf, payload: &str) -> Result<()> {
    let mut stream = match UnixStream::connect(socket_path) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("EOS_DAEMON_CONNECT_FAILED:{}", io_error_name(&err));
            std::process::exit(CONNECT_FAILED);
        }
    };
    if let Err(err) = stream
        .write_all(payload.as_bytes())
        .and_then(|()| stream.write_all(b"\n"))
    {
        eprintln!("EOS_DAEMON_IO_FAILED:{}", io_error_name(&err));
        std::process::exit(IO_FAILED);
    }
    if let Err(err) = stream.shutdown(std::net::Shutdown::Write) {
        eprintln!("EOS_DAEMON_IO_FAILED:{}", io_error_name(&err));
        std::process::exit(IO_FAILED);
    }
    let mut response = Vec::new();
    if let Err(err) = stream.read_to_end(&mut response) {
        eprintln!("EOS_DAEMON_IO_FAILED:{}", io_error_name(&err));
        std::process::exit(IO_FAILED);
    }
    std::io::stdout()
        .lock()
        .write_all(&response)
        .context("failed to write daemon client response")?;
    Ok(())
}

#[cfg(not(unix))]
fn run_client_request(_socket_path: &PathBuf, _payload: &str) -> Result<()> {
    eprintln!("EOS_DAEMON_CONNECT_FAILED:UnsupportedPlatform");
    std::process::exit(CONNECT_FAILED);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServeSubcommand {
    Serve,
    Daemon,
}

impl ServeSubcommand {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Serve => "serve",
            Self::Daemon => "daemon",
        }
    }
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

fn io_error_name(err: &std::io::Error) -> &'static str {
    match err.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
        std::io::ErrorKind::TimedOut => "TimeoutError",
        std::io::ErrorKind::BrokenPipe => "BrokenPipeError",
        _ => "OSError",
    }
}
