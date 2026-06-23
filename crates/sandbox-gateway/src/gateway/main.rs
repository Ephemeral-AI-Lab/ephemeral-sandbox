use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use sandbox_gateway::{
    GatewayConfig, SandboxGatewayServer, DEFAULT_GATEWAY_PID, DEFAULT_GATEWAY_SOCKET,
    DEFAULT_MAX_CONCURRENT_CONNECTIONS, SANDBOX_GATEWAY_SOCKET_ENV,
};
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, ManagerServices, SandboxDaemonClient,
    SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxManagerRouter, SandboxRecord,
    SandboxRuntime, SandboxStore,
};
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

#[derive(Debug, Args)]
struct ServeCommand {
    #[arg(long = "gateway-socket", value_name = "PATH")]
    gateway_socket: Option<PathBuf>,

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
    let config = GatewayConfig::new(
        gateway_socket_path(command.gateway_socket),
        command
            .pid_file
            .unwrap_or_else(|| PathBuf::from(DEFAULT_GATEWAY_PID)),
        command.max_concurrent_connections,
    );
    let manager = SandboxManagerRouter::new(default_manager_services());
    SandboxGatewayServer::with_shutdown(config, manager, shutdown)
        .serve()
        .await?;
    Ok(())
}

fn gateway_socket_path(cli_socket: Option<PathBuf>) -> PathBuf {
    cli_socket
        .or_else(|| std::env::var_os(SANDBOX_GATEWAY_SOCKET_ENV).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_GATEWAY_SOCKET))
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
        Arc::new(UnconfiguredDaemonClient),
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

    fn start_daemon(&self, _record: &SandboxRecord) -> Result<SandboxDaemonEndpoint, ManagerError> {
        Err(ManagerError::DaemonInstallFailed {
            message: "sandbox daemon installer is not configured".to_owned(),
        })
    }

    fn stop_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn check_daemon(&self, _endpoint: &SandboxDaemonEndpoint) -> Result<(), ManagerError> {
        Err(ManagerError::DaemonInstallFailed {
            message: "sandbox daemon installer is not configured".to_owned(),
        })
    }
}

struct UnconfiguredDaemonClient;

impl SandboxDaemonClient for UnconfiguredDaemonClient {
    fn invoke(
        &self,
        _endpoint: &SandboxDaemonEndpoint,
        _request: sandbox_protocol::Request,
    ) -> Result<sandbox_protocol::Response, ManagerError> {
        Err(ManagerError::ForwardingFailed {
            message: "sandbox daemon client is not configured".to_owned(),
        })
    }
}
