//! `eosd` binary entry: subcommand dispatch ONLY.
//!
//! # Invariant this binary owns (`proj-lib-main-split`)
//!
//! `main.rs` holds NO domain logic. It parses argv, routes to one of three
//! library entry points, and maps their typed errors to process exit codes:
//!
//! - `eosd daemon`     -> the async RPC server in `eos-daemon`.
//! - `eosd ns-runner`  -> the single-threaded namespace runner in `eos-runner`.
//! - `eosd ns-holder`  -> the single-threaded namespace holder in `eos-ns-holder`.
//!
//! Three real processes, one static binary — this replaces the Python launcher
//! chain (`daemon/scripts/launch_daemon.sh` spawns `python -m <module>`, and the
//! isolated-workspace control plane spawns `ns_holder.py` / `setns_exec.py` as
//! separate interpreters). In Rust they collapse into `eosd <subcommand>`.
//!
//! `anyhow` is allowed here (binary crate); library crates keep `thiserror`. A
//! tiny hand-rolled arg match is used instead of `clap` — the surface is three
//! fixed subcommands plus `--version`.
//!
//! # Exit-code contract (preserved through this dispatcher)
//!
//! The library errors carry exit codes that MUST survive to the process exit
//! status; a blanket `anyhow` fallthrough would collapse them all to `1` and
//! silently drop the contract. The dispatcher therefore maps known codes via
//! [`std::process::exit`]:
//! - ns-holder: `1` (control pipe closed), `2` (unexpected token), `7` (test
//!   crash knob) — `eos_ns_holder::NsHolderError::{CONTROL_CLOSED_EXIT,
//!   UNEXPECTED_TOKEN_EXIT, TEST_CRASH_EXIT}`.
//! - thin-client / daemon connect path: `97` (`CONNECT_FAILED`), `98`
//!   (`IO_FAILED`) — `eos_protocol::{CONNECT_FAILED, IO_FAILED}`.
//!
//! PORT backend/src/sandbox/daemon/scripts/launch_daemon.sh + backend/src/sandbox/host/daemon_client.py — the launcher + thin-client this binary replaces.
#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::os::fd::RawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

const MAX_DAEMON_WORKER_THREADS: usize = 4;

fn main() -> Result<()> {
    let mut args = std::env::args();
    let _argv0 = args.next();

    match args.next().as_deref() {
        Some("--version") | Some("-V") => {
            println!("eosd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("daemon") => run_daemon(args),
        Some("ns-runner") => run_ns_runner(args),
        Some("ns-holder") => run_ns_holder(args),
        Some(other) => Err(anyhow!(
            "unknown subcommand {other:?}; expected daemon | ns-runner | ns-holder | --version"
        )),
        None => Err(anyhow!(
            "missing subcommand; expected daemon | ns-runner | ns-holder | --version"
        )),
    }
}

/// `eosd daemon` — start, spawn, or call the async RPC server.
///
/// Modes:
/// - `eosd daemon --socket PATH --pid-file PATH ...` runs the foreground server.
/// - `eosd daemon --spawn --socket PATH --pid-file PATH --log-file PATH ...`
///   starts a detached foreground child and returns.
/// - `eosd daemon --client SOCKET JSON` is the Rust replacement for
///   `thin_client.py`, preserving exit codes 97/98.
// PORT backend/src/sandbox/daemon/scripts/launch_daemon.sh:78-80 — nohup python -m <MODULE> --socket <SOCK> --pid-file <PID>; daemon serve loop entry to be added to eos-daemon
fn run_daemon(args: std::env::Args) -> Result<()> {
    let config = DaemonCliConfig::parse(args)?;
    if let Some((socket_path, payload)) = config.client {
        return run_daemon_client(&socket_path, &payload);
    }
    if config.spawn {
        return spawn_daemon(config);
    }
    let server_config = eos_daemon::ServerConfig {
        socket_path: config.socket_path,
        pid_path: config.pid_path,
        tcp_host: config.tcp_host,
        tcp_port: config.tcp_port,
        auth_token: config.auth_token,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(daemon_worker_threads())
        .enable_all()
        .build()
        .context("failed to build daemon tokio runtime")?;
    runtime.block_on(async move {
        let (server, occ_queue) = eos_daemon::DaemonServer::new(server_config);
        server.serve(occ_queue).await
    })?;
    Ok(())
}

fn daemon_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map(|threads| threads.get().min(MAX_DAEMON_WORKER_THREADS))
        .unwrap_or(MAX_DAEMON_WORKER_THREADS)
        .max(1)
}

/// `eosd ns-runner` — execute one tool call inside a namespace (fresh-ns or
/// setns), reading the resolved `RunRequest` payload and emitting the
/// `RunResult` JSON, the way `namespace_entrypoint.py` / `setns_exec.py` run as
/// child interpreters today.
///
/// This is a thin call into `eos-runner`: read the request payload from stdin
/// or `--request <path>`, construct the overlay mount adapter, call `run`, and
/// write compact JSON to stdout or `--output <path>`.
// PORT backend/src/sandbox/overlay/namespace_entrypoint.py:1 + backend/src/sandbox/isolated_workspace/scripts/setns_exec.py:1 — child-interpreter entry; call eos_runner::run once a runner CLI entry exists
fn run_ns_runner(args: std::env::Args) -> Result<()> {
    let config = RunnerCliConfig::parse(args)?;
    let request_json = read_payload(config.request_path.as_ref())?;
    let request: eos_runner::RunRequest =
        serde_json::from_str(&request_json).context("failed to decode ns-runner request JSON")?;
    if config.mount_overlay {
        eos_runner::setns::setns_overlay_mount(&request, &OverlayMountPort)
            .context("ns-runner setns overlay mount failed")?;
        let result = eos_runner::RunResult {
            exit_code: 0,
            tool_result: serde_json::json!({"success": true, "status": "ok"}),
        };
        let output =
            serde_json::to_vec(&result).context("failed to encode ns-runner result JSON")?;
        write_payload(config.output_path.as_ref(), &output)?;
        return Ok(());
    }
    let result = eos_runner::run(&request, &OverlayMountPort).context("ns-runner failed")?;
    let output = serde_json::to_vec(&result).context("failed to encode ns-runner result JSON")?;
    write_payload(config.output_path.as_ref(), &output)?;
    Ok(())
}

/// `eosd ns-holder <readiness_fd> <control_fd>` — become the single-threaded
/// child that creates and pins the isolated workspace's namespace stack and
/// runs the readiness handshake, then `pause()`s until `SIGTERM`.
///
/// Real thin call: `eos-ns-holder` already exposes `run(readiness_fd,
/// control_fd)`, and its lib doc sanctions keeping the argv -> FD parsing here.
/// We parse the two positional FD ints and dispatch; the holder's typed errors
/// carry exit codes (`1` / `2` / `7`) that we map onto the process status so the
/// daemon-side crash-recovery sees the same codes as the Python holder.
// PORT backend/src/sandbox/isolated_workspace/scripts/ns_holder.py:89-91 — readiness_fd = int(argv[1]); control_fd = int(argv[2])
fn run_ns_holder(mut args: std::env::Args) -> Result<()> {
    let readiness_fd = parse_fd(args.next(), "readiness_fd")?;
    let control_fd = parse_fd(args.next(), "control_fd")?;

    match eos_ns_holder::run(readiness_fd, control_fd) {
        Ok(()) => Ok(()),
        Err(err) => {
            let code = match &err {
                eos_ns_holder::NsHolderError::ControlPipeClosed => {
                    eos_ns_holder::NsHolderError::CONTROL_CLOSED_EXIT
                }
                eos_ns_holder::NsHolderError::UnexpectedToken => {
                    eos_ns_holder::NsHolderError::UNEXPECTED_TOKEN_EXIT
                }
                eos_ns_holder::NsHolderError::TestCrash => {
                    eos_ns_holder::NsHolderError::TEST_CRASH_EXIT
                }
                // Unshare / pipe-i/o failures have no dedicated Python exit code;
                // surface the message and fall through to the generic status.
                _ => return Err(anyhow::Error::new(err).context("ns-holder failed")),
            };
            // The holder reached a defined non-zero terminal state; reproduce the
            // exact Python exit code (1 / 2) instead of anyhow's generic 1.
            std::process::exit(code);
        }
    }
}

/// Parse a positional file-descriptor argument shared by the ns-holder arm.
// PORT backend/src/sandbox/isolated_workspace/scripts/ns_holder.py:90-91 — int(argv[n])
fn parse_fd(value: Option<String>, name: &str) -> Result<RawFd> {
    value
        .ok_or_else(|| anyhow!("missing {name} argument for ns-holder"))?
        .parse::<RawFd>()
        .with_context(|| format!("{name} must be an integer file descriptor"))
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
    fn parse(args: std::env::Args) -> Result<Self> {
        let mut socket_path = PathBuf::from("/eos/daemon/runtime.sock");
        let mut pid_path = PathBuf::from("/eos/daemon/runtime.pid");
        let mut log_path = None;
        let mut tcp_host = None;
        let mut tcp_port = None;
        let mut auth_token = None;
        let mut spawn = false;
        let mut client = None;
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--socket" => socket_path = PathBuf::from(required_arg(&mut args, "--socket")?),
                "--pid-file" => pid_path = PathBuf::from(required_arg(&mut args, "--pid-file")?),
                "--log-file" => {
                    log_path = Some(PathBuf::from(required_arg(&mut args, "--log-file")?))
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
                    println!("usage: eosd daemon [--spawn] [--socket PATH] [--pid-file PATH] [--log-file PATH] [--tcp-host HOST --tcp-port PORT --auth-token TOKEN] | eosd daemon --client SOCKET JSON");
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

fn required_arg(args: &mut std::iter::Peekable<std::env::Args>, flag: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("{flag} requires a value"))
}

#[cfg(unix)]
fn run_daemon_client(socket_path: &PathBuf, payload: &str) -> Result<()> {
    let mut stream = match UnixStream::connect(socket_path) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("EOS_DAEMON_CONNECT_FAILED:{}", io_error_name(&err));
            std::process::exit(eos_protocol::CONNECT_FAILED);
        }
    };
    if let Err(err) = stream
        .write_all(payload.as_bytes())
        .and_then(|()| stream.write_all(b"\n"))
    {
        eprintln!("EOS_DAEMON_IO_FAILED:{}", io_error_name(&err));
        std::process::exit(eos_protocol::IO_FAILED);
    }
    if let Err(err) = stream.shutdown(std::net::Shutdown::Write) {
        eprintln!("EOS_DAEMON_IO_FAILED:{}", io_error_name(&err));
        std::process::exit(eos_protocol::IO_FAILED);
    }
    let mut response = Vec::new();
    if let Err(err) = stream.read_to_end(&mut response) {
        eprintln!("EOS_DAEMON_IO_FAILED:{}", io_error_name(&err));
        std::process::exit(eos_protocol::IO_FAILED);
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
    std::process::exit(eos_protocol::CONNECT_FAILED);
}

fn spawn_daemon(config: DaemonCliConfig) -> Result<()> {
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

struct RunnerCliConfig {
    request_path: Option<PathBuf>,
    output_path: Option<PathBuf>,
    mount_overlay: bool,
}

impl RunnerCliConfig {
    fn parse(args: std::env::Args) -> Result<Self> {
        let mut request_path = None;
        let mut output_path = None;
        let mut mount_overlay = false;
        let mut positional = Vec::new();
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mount-overlay" => mount_overlay = true,
                "--request" => {
                    request_path = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| anyhow!("--request requires a path"))?,
                    ));
                }
                "--output" => {
                    output_path = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| anyhow!("--output requires a path"))?,
                    ));
                }
                "--help" | "-h" => {
                    println!(
                        "usage: eosd ns-runner [--mount-overlay] [--request PATH] [--output PATH]"
                    );
                    std::process::exit(0);
                }
                other if other.starts_with('-') => {
                    return Err(anyhow!("unknown ns-runner flag {other:?}"));
                }
                other => positional.push(PathBuf::from(other)),
            }
        }
        if request_path.is_none() && positional.len() == 1 {
            request_path = positional.pop();
        } else if !positional.is_empty() {
            return Err(anyhow!(
                "ns-runner accepts at most one positional request path"
            ));
        }
        Ok(Self {
            request_path,
            output_path,
            mount_overlay,
        })
    }
}

fn read_payload(path: Option<&PathBuf>) -> Result<String> {
    let mut payload = String::new();
    if let Some(path) = path {
        std::fs::File::open(path)
            .with_context(|| format!("failed to open request payload {}", path.display()))?
            .read_to_string(&mut payload)
            .with_context(|| format!("failed to read request payload {}", path.display()))?;
    } else {
        std::io::stdin()
            .read_to_string(&mut payload)
            .context("failed to read request payload from stdin")?;
    }
    Ok(payload)
}

fn write_payload(path: Option<&PathBuf>, payload: &[u8]) -> Result<()> {
    if let Some(path) = path {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create output dir {}", parent.display()))?;
        }
        std::fs::write(path, payload)
            .with_context(|| format!("failed to write ns-runner output {}", path.display()))?;
    } else {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(payload)
            .context("failed to write ns-runner output to stdout")?;
        stdout
            .write_all(b"\n")
            .context("failed to terminate ns-runner output line")?;
    }
    Ok(())
}

#[derive(Debug)]
struct OverlayMountPort;

impl eos_runner::KernelMountPort for OverlayMountPort {
    fn mount_overlay(
        &self,
        inputs: &eos_runner::MountInputs,
    ) -> std::result::Result<Box<dyn eos_runner::MountedOverlay>, eos_runner::RunnerError> {
        let handle = eos_overlay::OverlayHandle {
            upperdir: inputs.upperdir.clone(),
            workdir: inputs.workdir.clone(),
            layer_paths: inputs.layer_paths.clone(),
        };
        let mount = eos_overlay::mount_overlay(&inputs.workspace_root, &handle)?;
        Ok(Box::new(mount))
    }
}
