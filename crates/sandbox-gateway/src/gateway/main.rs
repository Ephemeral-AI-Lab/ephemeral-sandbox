use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand, ValueEnum};
use sandbox_config::configs::manager::ManagerConfig;
use sandbox_gateway::{
    GatewayConfig, SandboxGatewayServer, DEFAULT_GATEWAY_PID, DEFAULT_GATEWAY_SOCKET,
    DEFAULT_MAX_CONCURRENT_CONNECTIONS, SANDBOX_GATEWAY_AUTH_TOKEN_ENV, SANDBOX_GATEWAY_SOCKET_ENV,
};
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ExportApplyCaps, ManagerError, ManagerServices,
    SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxManagerRouter, SandboxRecord,
    SandboxRuntime, SandboxStore, StartedDaemon, TcpSandboxDaemonClient,
};
use sandbox_provider_docker::{DockerSandboxDaemonInstaller, DockerSandboxRuntime};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(name = "sandbox-gateway", disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve(ServeCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Backend {
    None,
    Docker,
}

#[derive(Debug, Args)]
struct ServeCommand {
    #[arg(long = "gateway-socket", value_name = "HOST:PORT")]
    gateway_socket: Option<String>,

    #[arg(long = "auth-token", value_name = "TOKEN")]
    auth_token: Option<String>,

    #[arg(
        long = "backend",
        value_enum,
        default_value = "none",
        env = "EOS_GATEWAY_BACKEND"
    )]
    backend: Backend,

    #[arg(long = "config-yaml", value_name = "PATH")]
    config_yaml: Option<PathBuf>,

    #[arg(long = "pid-file", value_name = "PATH")]
    pid_file: Option<PathBuf>,

    #[arg(
        long = "max-concurrent-connections",
        value_name = "COUNT",
        default_value_t = DEFAULT_MAX_CONCURRENT_CONNECTIONS
    )]
    max_concurrent_connections: usize,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(command) => serve(command).await,
    }
}

async fn serve(command: ServeCommand) -> Result<(), Box<dyn std::error::Error>> {
    let shutdown = CancellationToken::new();
    install_ctrl_c_shutdown(shutdown.clone());
    let auth_token = resolve_gateway_auth_token(command.auth_token)?;
    let services = build_manager_services(command.backend, command.config_yaml.as_deref())?;
    let config = GatewayConfig::new(
        resolve_bind_addr(command.gateway_socket),
        command
            .pid_file
            .unwrap_or_else(|| PathBuf::from(DEFAULT_GATEWAY_PID)),
        command.max_concurrent_connections,
        Some(auth_token),
    );
    let manager = SandboxManagerRouter::new(services);
    SandboxGatewayServer::with_shutdown(config, manager, shutdown)
        .serve()
        .await?;
    Ok(())
}

fn build_manager_services(
    backend: Backend,
    config_yaml: Option<&Path>,
) -> Result<Arc<ManagerServices>, Box<dyn std::error::Error>> {
    match backend {
        Backend::None => Ok(default_manager_services()),
        Backend::Docker => build_docker_services(config_yaml),
    }
}

fn build_docker_services(
    config_yaml: Option<&Path>,
) -> Result<Arc<ManagerServices>, Box<dyn std::error::Error>> {
    let path = config_yaml.ok_or("--config-yaml is required when --backend docker")?;
    let document = sandbox_config::load_path(path)?;
    let manager_config: ManagerConfig = document.section("manager")?;
    manager_config.validate()?;
    let export_caps = ExportApplyCaps {
        max_stream_bytes: manager_config.export.max_stream_bytes,
        max_decompressed_bytes: manager_config.export.max_decompressed_bytes,
        max_apply_entries: manager_config.export.max_apply_entries,
    };
    let docker_config = manager_config
        .docker
        .ok_or("config is missing the manager.docker section")?;

    let store = Arc::new(match manager_config.registry_path {
        Some(path) => SandboxStore::load(path)?,
        None => SandboxStore::new(),
    });
    let runtime = DockerSandboxRuntime::new(docker_config.clone());
    match runtime.recover_sandboxes() {
        Ok(records) => match store.reconcile(records) {
            Ok(orphaned) => {
                for id in orphaned {
                    eprintln!("sandbox {id} has no backing container; marked failed");
                }
            }
            Err(error) => eprintln!("sandbox registry reconcile failed: {error}"),
        },
        Err(error) => eprintln!("sandbox recovery failed; keeping loaded registry: {error}"),
    }

    let mut services = ManagerServices::new(
        store,
        Arc::new(runtime),
        Arc::new(DockerSandboxDaemonInstaller::new(docker_config)),
        Arc::new(TcpSandboxDaemonClient::new()),
    );
    services.export_caps = export_caps;
    Ok(Arc::new(services))
}

fn resolve_bind_addr(cli_socket: Option<String>) -> String {
    cli_socket
        .or_else(|| std::env::var(SANDBOX_GATEWAY_SOCKET_ENV).ok())
        .filter(|addr| !addr.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GATEWAY_SOCKET.to_owned())
}

fn resolve_gateway_auth_token(
    cli_token: Option<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    cli_token
        .or_else(|| std::env::var(SANDBOX_GATEWAY_AUTH_TOKEN_ENV).ok())
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "gateway auth token is required; pass --auth-token or set \
                 {SANDBOX_GATEWAY_AUTH_TOKEN_ENV}"
            )
            .into()
        })
}

fn install_ctrl_c_shutdown(shutdown: CancellationToken) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            shutdown.cancel();
        }
    });
}

fn default_manager_services() -> Arc<ManagerServices> {
    Arc::new(ManagerServices::new(
        Arc::new(SandboxStore::new()),
        Arc::new(UnconfiguredRuntime),
        Arc::new(UnconfiguredDaemonInstaller),
        Arc::new(TcpSandboxDaemonClient::new()),
    ))
}

struct UnconfiguredRuntime;

impl SandboxRuntime for UnconfiguredRuntime {
    fn create_sandbox(
        &self,
        _request: &CreateSandboxRequest,
    ) -> Result<CreateSandboxResult, ManagerError> {
        Err(ManagerError::RuntimeFailed {
            message: "sandbox runtime is not configured".to_owned(),
        })
    }

    fn destroy_sandbox(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Err(ManagerError::RuntimeFailed {
            message: "sandbox runtime is not configured".to_owned(),
        })
    }
}

struct UnconfiguredDaemonInstaller;

impl SandboxDaemonInstaller for UnconfiguredDaemonInstaller {
    fn install_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Err(ManagerError::DaemonInstallFailed {
            message: "sandbox daemon installer is not configured".to_owned(),
        })
    }

    fn start_daemon(&self, _record: &SandboxRecord) -> Result<StartedDaemon, ManagerError> {
        Err(ManagerError::DaemonInstallFailed {
            message: "sandbox daemon installer is not configured".to_owned(),
        })
    }

    fn stop_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn check_daemon(
        &self,
        _record: &SandboxRecord,
        _endpoint: &SandboxDaemonEndpoint,
    ) -> Result<(), ManagerError> {
        Err(ManagerError::DaemonInstallFailed {
            message: "sandbox daemon installer is not configured".to_owned(),
        })
    }
}
