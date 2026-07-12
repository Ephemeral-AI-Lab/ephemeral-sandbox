use std::collections::BTreeSet;
use std::convert::Infallible;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use http::{Request, Response};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command as ProcessCommand;
use tokio::task::JoinSet;

use sandbox_benchmark::api::{self, ResponseBody};
use sandbox_benchmark::app::{AppState, ExecutionDependencies};
use sandbox_benchmark::config::{ResolveInput, StartupConfig};
use sandbox_benchmark::definitions;
use sandbox_benchmark::fixtures::load_workspace_profiles;
use sandbox_benchmark::gateway::fixed_git_toolchain_directory;
use sandbox_benchmark::model::ConfigurationScope;
use sandbox_benchmark::plan::{
    load_plan, load_presets, slice_default, validate_and_expand_with_profiles,
};
use sandbox_benchmark::recovery::reconcile_interrupted_runs;
use sandbox_benchmark::scheduler::{is_terminal, CampaignGateError};

type MainResult<T> = Result<T, Box<dyn Error>>;
type ConnectionResult = (SocketAddr, Result<(), hyper::Error>);
type JoinedConnection = Result<ConnectionResult, tokio::task::JoinError>;

const DEPENDENCY_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DEPENDENCY_PROBE_BYTES: usize = 64 * 1024;
const SHUTDOWN_CAMPAIGN_GRACE: Duration = Duration::from_secs(30);
const SHUTDOWN_CAMPAIGN_POLL: Duration = Duration::from_millis(100);
const PRODUCTION_CONFIG: &str = "config/prd.yml";
const ALLOWED_CONTAINER_DAEMON_PATHS: &[&str] = &[
    "dist/sandbox-daemon-linux-arm64",
    "dist/sandbox-daemon-linux-amd64",
];

#[derive(Debug, Parser)]
#[command(
    name = "sandbox-benchmark",
    about = "Local EphemeralOS Benchmark Laboratory",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the same-origin benchmark UI and versioned API.
    Serve(ServeCommand),
}

#[derive(Debug, Args)]
struct ServeCommand {
    /// Path to the EphemeralOS repository checkout.
    #[arg(long, value_name = "PATH")]
    repo: PathBuf,

    /// Dedicated test workspace root outside the repository.
    #[arg(long = "test-workspace-root", value_name = "PATH")]
    test_workspace_root: Option<PathBuf>,

    /// Loopback address for the HTTP server.
    #[arg(long, value_name = "HOST:PORT", default_value = "127.0.0.1:0")]
    bind: SocketAddr,

    /// Open the bound local URL with the platform browser launcher.
    #[arg(long)]
    open: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sandbox-benchmark error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> MainResult<()> {
    match cli.command {
        Command::Serve(command) => serve(command).await,
    }
}

async fn serve(command: ServeCommand) -> MainResult<()> {
    let (mut config, settings_source) = StartupConfig::resolve(ResolveInput {
        repo: command.repo,
        bind: command.bind,
        web_root: None,
        workspace_override: command.test_workspace_root,
    })?;
    let default_image = validate_versioned_data(&config)?;

    let listener = bind_loopback(config.bind).await?;
    let local_addr = listener.local_addr()?;
    config.bind = local_addr;

    let state = AppState::new(config, settings_source, local_addr.to_string(), false)?;
    let recovery_blocker = match reconcile_interrupted_runs(&state.config()?, &state.artifacts()?)
        .await
    {
        Ok(summary) => {
            if summary.interrupted_runs > 0 || !summary.issues.is_empty() {
                eprintln!(
                    "sandbox-benchmark recovery scanned {} run(s), terminalized {}, cleaned {} owned target(s), and quarantined {} torn tail(s)",
                    summary.scanned_runs,
                    summary.interrupted_runs,
                    summary.cleaned_owned_targets,
                    summary.quarantined_tails
                );
                for issue in &summary.issues {
                    eprintln!(
                        "sandbox-benchmark recovery {} for run {}: {}",
                        issue.code, issue.run_id, issue.message
                    );
                }
            }
            (!summary.execution_safe()).then_some(
                "restart recovery did not prove cleanup of all interrupted owned work".to_owned(),
            )
        }
        Err(error) => Some(format!("restart reconciliation failed: {error}")),
    };
    match recovery_blocker {
        Some(reason) => {
            state.mark_execution_unavailable(reason.clone())?;
            eprintln!(
                "sandbox-benchmark execution unavailable; artifact and report access remain ready: {reason}"
            );
        }
        None => match execution_preflight(&state.config()?, &default_image).await {
            Ok(dependencies) => state.install_execution_dependencies(dependencies)?,
            Err(error) => {
                let reason = error.to_string();
                state.mark_execution_unavailable(reason.clone())?;
                eprintln!(
                    "sandbox-benchmark execution unavailable; artifact and report access remain ready: {reason}"
                );
            }
        },
    }
    let local_url = format!("http://{local_addr}/");
    println!("sandbox-benchmark listening on {local_url}");
    io::stdout().flush()?;

    if command.open {
        if let Err(error) = open_browser(&local_url) {
            eprintln!("sandbox-benchmark could not open the browser: {error}");
        }
    }

    serve_listener(listener, state).await?;
    Ok(())
}

async fn bind_loopback(address: SocketAddr) -> io::Result<TcpListener> {
    if !address.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("benchmark service may bind only to loopback, received {address}"),
        ));
    }

    TcpListener::bind(address).await
}

async fn serve_listener(listener: TcpListener, state: Arc<AppState>) -> io::Result<()> {
    let mut connections = JoinSet::new();
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            signal = &mut shutdown => {
                signal?;
                break;
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let state = Arc::clone(&state);
                connections.spawn(async move {
                    (peer, serve_connection(stream, state).await)
                });
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                log_connection_result(completed);
            }
        }
    }

    cancel_active_campaign(&state).await;
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

async fn cancel_active_campaign(state: &AppState) {
    // Admission and cancellation share this lock so shutdown never observes an
    // active gate before its single authoritative manifest writer exists.
    let registration = state.lock_run_registration().await;
    let active = match state.campaigns.active() {
        Ok(active) => active,
        Err(error) => {
            eprintln!(
                "sandbox-benchmark could not inspect the active campaign at shutdown: {error}"
            );
            return;
        }
    };
    let Some((run_id, _)) = active else {
        return;
    };
    let artifacts = match state.run_artifacts(&run_id) {
        Ok(Some(artifacts)) => artifacts,
        Ok(None) => {
            eprintln!(
                "sandbox-benchmark cannot cancel active run {run_id}: its manifest authority is unavailable"
            );
            return;
        }
        Err(error) => {
            eprintln!(
                "sandbox-benchmark could not read active run {run_id} manifest authority: {error}"
            );
            return;
        }
    };
    let cancellation = match state.campaigns.cancellation_token(&run_id) {
        Ok(cancellation) => cancellation,
        Err(error) => {
            eprintln!("sandbox-benchmark could not acquire cancellation for {run_id}: {error}");
            return;
        }
    };
    let manifest = match artifacts.request_cancellation(&cancellation) {
        Ok((manifest, _)) => manifest,
        Err(error) => {
            eprintln!("sandbox-benchmark could not durably cancel active run {run_id}: {error}");
            return;
        }
    };
    if is_terminal(manifest.state) {
        return;
    }
    match state.campaigns.update_state(&run_id, manifest.state) {
        Ok(()) => eprintln!(
            "sandbox-benchmark cancelling active run {run_id} and waiting for owned teardown"
        ),
        Err(CampaignGateError::NotActive(_)) => return,
        Err(error) => {
            eprintln!("sandbox-benchmark could not mark active run {run_id} cancelling: {error}");
            return;
        }
    }
    drop(registration);

    let deadline = Instant::now() + SHUTDOWN_CAMPAIGN_GRACE;
    loop {
        match state.campaigns.active() {
            Ok(None) => return,
            Ok(Some(_)) if Instant::now() < deadline => {
                tokio::time::sleep(SHUTDOWN_CAMPAIGN_POLL).await;
            }
            Ok(Some(_)) => {
                eprintln!(
                    "sandbox-benchmark shutdown grace expired while run {run_id} was restoring owned state; restart reconciliation will fail closed"
                );
                return;
            }
            Err(error) => {
                eprintln!(
                    "sandbox-benchmark could not confirm active run {run_id} teardown: {error}"
                );
                return;
            }
        }
    }
}

async fn serve_connection(stream: TcpStream, state: Arc<AppState>) -> Result<(), hyper::Error> {
    let service = service_fn(move |request: Request<Incoming>| {
        let state = Arc::clone(&state);
        async move { Ok::<Response<ResponseBody>, Infallible>(api::handle(state, request).await) }
    });

    hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .await
}

fn log_connection_result(completed: Option<JoinedConnection>) {
    match completed {
        Some(Ok((peer, Err(error)))) => {
            eprintln!("sandbox-benchmark connection from {peer} ended with an error: {error}");
        }
        Some(Err(error)) => {
            eprintln!("sandbox-benchmark connection task failed: {error}");
        }
        Some(Ok((_, Ok(())))) | None => {}
    }
}

fn validate_versioned_data(config: &StartupConfig) -> MainResult<String> {
    let profile_directory = config.repo.join("benchmark/defaults/workspace-profiles");
    let profiles = load_workspace_profiles(&profile_directory)?;
    let _catalog = definitions::catalog_with_workspace_profiles(profiles.clone());

    let default_path = config.repo.join("benchmark/defaults/standard-local.yml");
    let default = load_plan(&default_path)?;
    if default.configuration_base.scope != ConfigurationScope::All {
        return Err(invalid_data(
            "the standard local default must have all-family scope",
        ));
    }

    let defaults = [
        ConfigurationScope::All,
        ConfigurationScope::Command,
        ConfigurationScope::Files,
        ConfigurationScope::Workspace,
        ConfigurationScope::LayerStack,
    ]
    .map(|scope| slice_default(&default, scope));
    for scoped in &defaults {
        let expanded =
            validate_and_expand_with_profiles(scoped, &config.paths, &profiles, Some(scoped))?;
        if !expanded.runnable {
            return Err(invalid_data(format!(
                "server-authored {:?} default failed canonical validation",
                scoped.configuration_base.scope
            )));
        }
    }

    let presets = load_presets(&config.repo.join("benchmark/presets"))?;
    let mut identities = BTreeSet::new();
    for preset in presets {
        if !identities.insert((preset.id.clone(), preset.version)) {
            return Err(invalid_data(format!(
                "duplicate preset identity {} version {}",
                preset.id, preset.version
            )));
        }
        let Some(declared_default) = defaults
            .iter()
            .find(|candidate| candidate.configuration_base == preset.plan.configuration_base)
        else {
            return Err(invalid_data(format!(
                "preset {} version {} does not target an exact server-authored default",
                preset.id, preset.version
            )));
        };
        let expanded = validate_and_expand_with_profiles(
            &preset.plan,
            &config.paths,
            &profiles,
            Some(declared_default),
        )?;
        if !expanded.runnable {
            return Err(invalid_data(format!(
                "preset {} version {} failed canonical validation",
                preset.id, preset.version
            )));
        }
    }
    Ok(default.environment.image.0)
}

async fn execution_preflight(
    config: &StartupConfig,
    default_image: &str,
) -> MainResult<ExecutionDependencies> {
    // The Docker runtime requires the repository-owned Git archives. Validate
    // the fixed, canonical source during admission so a browser never starts a
    // campaign that can only fail after a product request is issued.
    fixed_git_toolchain_directory(&config.repo)?;
    let current_exe = std::env::current_exe()?.canonicalize()?;
    let binary_directory = current_exe.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "benchmark executable has no parent directory",
        )
    })?;
    let gateway_binary = required_sibling_binary(binary_directory, "sandbox-gateway")?;
    let daemon_binary = required_container_daemon_binary(&config.repo)?;
    let docker_binary = find_path_executable("docker")?;
    let git_binary = find_path_executable("git")?;
    let stat_binary = required_system_binary(&["/usr/bin/stat", "/bin/stat"])?;
    let df_binary = required_system_binary(&["/bin/df", "/usr/bin/df"])?;

    let source_commit = run_fixed_probe(
        &git_binary,
        &["rev-parse", "--verify", "HEAD"],
        Some(&config.repo),
    )
    .await?;
    if source_commit.len() != 40 || !source_commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_data("Git HEAD did not resolve to a full commit id"));
    }

    let docker_engine_version = run_fixed_probe(
        &docker_binary,
        &["version", "--format", "{{.Server.Version}}"],
        Some(&config.repo),
    )
    .await?;
    if docker_engine_version.is_empty() {
        return Err(invalid_data("Docker returned an empty engine version"));
    }
    let image_identity = run_fixed_probe(
        &docker_binary,
        &["image", "inspect", "--format", "{{.Id}}", default_image],
        Some(&config.repo),
    )
    .await?;
    if !image_identity.starts_with("sha256:") {
        return Err(invalid_data(format!(
            "benchmark image {default_image} did not resolve to a content identity"
        )));
    }

    Ok(ExecutionDependencies {
        gateway_binary,
        daemon_binary,
        docker_binary,
        git_binary,
        stat_binary,
        df_binary,
        docker_engine_version,
    })
}

fn required_system_binary(candidates: &[&str]) -> io::Result<PathBuf> {
    candidates
        .iter()
        .map(std::path::Path::new)
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("required system executable was not found: {candidates:?}"),
            )
        })?
        .canonicalize()
}

fn required_sibling_binary(directory: &std::path::Path, name: &str) -> io::Result<PathBuf> {
    let candidate = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let canonical = candidate.canonicalize()?;
    if !canonical.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("required executable is not a file: {}", canonical.display()),
        ));
    }
    Ok(canonical)
}

/// The Docker backend installs a Linux daemon inside the sandbox container.
/// The host-release `sandbox-daemon` sibling is intentionally never eligible:
/// on macOS it is a Mach-O executable and fails only after a product request.
/// Resolve the one production-configured repository package from a tiny fixed
/// allowlist during startup instead.
fn required_container_daemon_binary(repo: &std::path::Path) -> io::Result<PathBuf> {
    let config_path = repo.join(PRODUCTION_CONFIG);
    let document = sandbox_config::load_path(&config_path).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "production benchmark gateway configuration is invalid",
        )
    })?;
    let manager: sandbox_config::configs::manager::ManagerConfig =
        document.section("manager").map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "production benchmark manager configuration is missing",
            )
        })?;
    let configured = manager
        .docker
        .as_ref()
        .map(|docker| &docker.daemon_binary_path)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "production benchmark Docker configuration is missing",
            )
        })?;
    if configured.is_absolute()
        || !ALLOWED_CONTAINER_DAEMON_PATHS
            .iter()
            .any(|allowed| configured == std::path::Path::new(allowed))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "production daemon package is outside the fixed benchmark allowlist",
        ));
    }
    let candidate = repo.join(configured);
    let metadata = fs::symlink_metadata(&candidate)?;
    let canonical = candidate.canonicalize()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || canonical != candidate {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "production daemon package is not a canonical regular file",
        ));
    }
    let mut magic = [0_u8; 4];
    File::open(&canonical)?.read_exact(&mut magic)?;
    if magic != [0x7f, b'E', b'L', b'F'] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "production daemon package is not a Linux ELF executable",
        ));
    }
    Ok(canonical)
}

fn find_path_executable(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os("PATH")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PATH is unavailable"))?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if candidate.is_file() {
            return candidate.canonicalize();
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("required executable {name} was not found in PATH"),
    ))
}

async fn run_fixed_probe(
    program: &std::path::Path,
    arguments: &[&str],
    current_directory: Option<&std::path::Path>,
) -> io::Result<String> {
    let mut command = ProcessCommand::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(directory) = current_directory {
        command.current_dir(directory);
    }
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("dependency probe stdout was unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("dependency probe stderr was unavailable"))?;
    let stdout_task = tokio::spawn(drain_probe_stream(stdout));
    let stderr_task = tokio::spawn(drain_probe_stream(stderr));
    let status = match tokio::time::timeout(DEPENDENCY_PROBE_TIMEOUT, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "dependency probe timed out",
            ));
        }
    };
    let (stdout, stdout_truncated) = stdout_task
        .await
        .map_err(|error| io::Error::other(error.to_string()))??;
    let (_, stderr_truncated) = stderr_task
        .await
        .map_err(|error| io::Error::other(error.to_string()))??;
    if stdout_truncated || stderr_truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "dependency probe exceeded its fixed output cap",
        ));
    }
    if !status.success() {
        return Err(io::Error::other(format!(
            "dependency probe exited with {status}"
        )));
    }
    String::from_utf8(stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "probe output was not UTF-8"))
}

async fn drain_probe_stream<R: AsyncRead + Unpin>(mut reader: R) -> io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_DEPENDENCY_PROBE_BYTES.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((retained, truncated))
}

fn invalid_data(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into()))
}

fn open_browser(url: &str) -> io::Result<()> {
    let mut command = browser_command(url);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let _browser_wait = tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn browser_command(url: &str) -> ProcessCommand {
    let mut command = ProcessCommand::new("open");
    command.arg(url);
    command
}

#[cfg(target_os = "windows")]
fn browser_command(url: &str) -> ProcessCommand {
    let mut command = ProcessCommand::new("cmd");
    command.args(["/C", "start", "", url]);
    command
}

#[cfg(all(unix, not(target_os = "macos")))]
fn browser_command(url: &str) -> ProcessCommand {
    let mut command = ProcessCommand::new("xdg-open");
    command.arg(url);
    command
}

#[cfg(not(any(unix, target_os = "windows")))]
fn browser_command(_url: &str) -> ProcessCommand {
    ProcessCommand::new("unsupported-platform-browser-launcher")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_cli_defaults_to_ephemeral_loopback_port() {
        let cli = Cli::try_parse_from(["sandbox-benchmark", "serve", "--repo", "."])
            .expect("serve arguments should parse");
        let Command::Serve(command) = cli.command;
        assert_eq!(command.bind, "127.0.0.1:0".parse().expect("valid socket"));
        assert!(command.test_workspace_root.is_none());
        assert!(!command.open);
    }

    #[test]
    fn serve_cli_accepts_documented_flags() {
        let cli = Cli::try_parse_from([
            "sandbox-benchmark",
            "serve",
            "--repo",
            "/repo",
            "--test-workspace-root",
            "/workspace",
            "--bind",
            "[::1]:8080",
            "--open",
        ])
        .expect("documented serve arguments should parse");
        let Command::Serve(command) = cli.command;
        assert_eq!(command.repo, PathBuf::from("/repo"));
        assert_eq!(
            command.test_workspace_root,
            Some(PathBuf::from("/workspace"))
        );
        assert_eq!(command.bind, "[::1]:8080".parse().expect("valid socket"));
        assert!(command.open);
    }

    #[test]
    fn production_daemon_preflight_selects_the_configured_linux_package() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("canonical repository");
        let daemon = required_container_daemon_binary(&repo)
            .expect("configured production daemon package must be usable");
        assert!(ALLOWED_CONTAINER_DAEMON_PATHS
            .iter()
            .any(|allowed| daemon == repo.join(allowed)));
        assert_eq!(
            fs::read(&daemon).expect("read daemon package")[..4],
            [0x7f, b'E', b'L', b'F']
        );
    }
}
