//! `eosd daemon` subcommand adapter.

use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use eos_config::configs::{
    daemon::{DaemonConfig, DaemonServerConfig},
    isolated_workspace::IsolatedWorkspaceConfig,
};

/// Start, spawn, or call the async RPC server.
///
/// Modes:
/// - `eosd daemon --socket PATH --pid-file PATH ...` runs the foreground server.
/// - `eosd daemon --spawn --socket PATH --pid-file PATH --log-file PATH ...`
///   starts a detached foreground child and returns.
/// - `eosd daemon --client SOCKET JSON` is the Rust replacement for
///   `thin_client.py`, preserving exit codes 97/98.
pub(crate) fn run(args: std::env::Args) -> Result<()> {
    let runtime_config = load_runtime_config()?;
    let daemon_config = &runtime_config.daemon;
    let config = DaemonCliConfig::parse(args, &daemon_config.server)?;
    if let Some((socket_path, payload)) = config.client {
        return run_daemon_client(&socket_path, &payload);
    }
    if config.spawn {
        return spawn_daemon(&config);
    }
    emit_boot_event(
        "config_loaded",
        serde_json::json!({
            "socket_path": config.socket_path.display().to_string(),
            "pid_path": config.pid_path.display().to_string(),
            "tcp_host": config.tcp_host.clone(),
            "tcp_port": config.tcp_port,
            "auth_token_present": config.auth_token.as_ref().is_some_and(|token| !token.is_empty()),
        }),
    );
    let server_config = eos_daemon::ServerConfig {
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
        let server = eos_daemon::DaemonServer::with_daemon_config(
            server_config,
            &runtime_config.daemon,
            &runtime_config.isolated_workspace,
        );
        server.serve().await
    })?;
    Ok(())
}

struct DaemonRuntimeConfig {
    daemon: DaemonConfig,
    isolated_workspace: IsolatedWorkspaceConfig,
}

fn load_runtime_config() -> Result<DaemonRuntimeConfig> {
    let doc = eos_config::load_prd().context("load sandbox/config/prd.yml")?;
    let daemon = doc
        .section::<DaemonConfig>("daemon")
        .context("deserialize daemon config section")?;
    daemon.validate().context("validate daemon config")?;
    let isolated_workspace = doc
        .section::<IsolatedWorkspaceConfig>("isolated_workspace")
        .context("deserialize isolated_workspace config section")?;
    isolated_workspace
        .validate()
        .context("validate isolated_workspace config")?;
    Ok(DaemonRuntimeConfig {
        daemon,
        isolated_workspace,
    })
}

fn daemon_worker_threads(max_worker_threads: usize) -> usize {
    std::thread::available_parallelism()
        .map_or(max_worker_threads, |threads| {
            threads.get().min(max_worker_threads)
        })
        .max(1)
}

struct DaemonCliConfig {
    socket_path: PathBuf,
    pid_path: PathBuf,
    log_path: Option<PathBuf>,
    tcp_host: Option<String>,
    tcp_port: Option<u16>,
    auth_token: Option<String>,
    spawn: bool,
    client: Option<(PathBuf, String)>,
}

impl DaemonCliConfig {
    fn parse(args: std::env::Args, server_defaults: &DaemonServerConfig) -> Result<Self> {
        let mut socket_path = server_defaults.socket_path.clone();
        let mut pid_path = server_defaults.pid_path.clone();
        let mut log_path = None;
        let mut tcp_host = None;
        let mut tcp_port = None;
        let mut auth_token = None;
        let mut spawn = false;
        let mut client = None;
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--socket" => socket_path = PathBuf::from(required_arg(&mut args, "--socket")?),
                "--pid-file" => pid_path = PathBuf::from(required_arg(&mut args, "--pid-file")?),
                "--log-file" => {
                    log_path = Some(PathBuf::from(required_arg(&mut args, "--log-file")?));
                }
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
                        "usage: eosd daemon [--spawn] [--socket PATH] [--pid-file PATH] [--log-file PATH] [--tcp-host HOST --tcp-port PORT --auth-token TOKEN] | eosd daemon --client SOCKET JSON"
                    );
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown daemon flag {other:?}")),
            }
        }
        Ok(Self {
            socket_path,
            pid_path,
            log_path,
            tcp_host,
            tcp_port,
            auth_token,
            spawn,
            client,
        })
    }

    fn foreground_args(&self) -> Vec<String> {
        let mut args = vec![
            "daemon".to_owned(),
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
        if let Some(token) = &self.auth_token {
            args.push("--auth-token".to_owned());
            args.push(token.clone());
        }
        args
    }
}

fn required_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("{flag} requires a value"))
}

#[cfg(unix)]
fn run_daemon_client(socket_path: &PathBuf, payload: &str) -> Result<()> {
    let mut stream = match UnixStream::connect(socket_path) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("EOS_DAEMON_CONNECT_FAILED:{}", io_error_name(&err));
            std::process::exit(eos_daemon::wire::CONNECT_FAILED);
        }
    };
    if let Err(err) = stream
        .write_all(payload.as_bytes())
        .and_then(|()| stream.write_all(b"\n"))
    {
        eprintln!("EOS_DAEMON_IO_FAILED:{}", io_error_name(&err));
        std::process::exit(eos_daemon::wire::IO_FAILED);
    }
    if let Err(err) = stream.shutdown(std::net::Shutdown::Write) {
        eprintln!("EOS_DAEMON_IO_FAILED:{}", io_error_name(&err));
        std::process::exit(eos_daemon::wire::IO_FAILED);
    }
    let mut response = Vec::new();
    if let Err(err) = stream.read_to_end(&mut response) {
        eprintln!("EOS_DAEMON_IO_FAILED:{}", io_error_name(&err));
        std::process::exit(eos_daemon::wire::IO_FAILED);
    }
    std::io::stdout()
        .lock()
        .write_all(&response)
        .context("failed to write daemon client response")?;
    Ok(())
}

#[cfg(not(unix))]
fn run_daemon_client(_socket_path: &PathBuf, _payload: &str) -> Result<()> {
    eprintln!("EOS_DAEMON_CONNECT_FAILED:UnsupportedPlatform");
    std::process::exit(eos_daemon::wire::CONNECT_FAILED);
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

    let executable = std::env::current_exe().context("failed to resolve eosd executable")?;
    let mut command = Command::new(executable);
    command.args(config.foreground_args());
    command.stdin(Stdio::null());
    if let Some(path) = &config.log_path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create log dir {}", parent.display()))?;
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open daemon log {}", path.display()))?;
        command.stdout(Stdio::from(log.try_clone()?));
        command.stderr(Stdio::from(log));
    } else {
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
    }
    command.spawn().context("failed to spawn eosd daemon")?;
    Ok(())
}

fn emit_boot_event(event: &str, details: serde_json::Value) {
    eprintln!(
        "{}",
        serde_json::json!({
            "ts_ms": unix_ms(),
            "level": "info",
            "module": "daemon.boot",
            "event": event,
            "details": details,
        })
    );
}

fn unix_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
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
