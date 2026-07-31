use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use sandbox_config::configs::{
    daemon::DaemonConfig, manager::ManagerConfig, observability::ObservabilityConfig,
    runtime::RuntimeConfig,
};
use sandbox_config::ConfigDocument;
use sandbox_gateway::{
    resolve_gateway_config, GatewayCliOverrides, GatewayConfig, SandboxGatewayServer,
    TcpSandboxDaemonClient, SANDBOX_GATEWAY_AUTH_TOKEN_ENV, SANDBOX_GATEWAY_SOCKET_ENV,
};
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ExportApplyCaps, ManagerError, ManagerServices,
    ObservabilitySnapshotLimits, SandboxDaemonEndpoint, SandboxDaemonInstaller,
    SandboxManagerRouter, SandboxRecord, SandboxRuntime, SandboxStore, StartedDaemon,
    WorkspaceRootPolicy,
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
    #[arg(
        long = "gateway-endpoint",
        visible_alias = "gateway-socket",
        value_name = "ENDPOINT"
    )]
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

    #[arg(long = "max-concurrent-connections", value_name = "COUNT")]
    max_concurrent_connections: Option<usize>,
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
    let document = command
        .config_yaml
        .as_deref()
        .map(sandbox_config::load_path)
        .transpose()?;
    let services = build_manager_services(command.backend, document.as_ref())?;
    let mut config = resolve_gateway_config(
        GatewayCliOverrides {
            bind_addr: command.gateway_socket,
            pid_path: command.pid_file,
            max_concurrent_connections: command.max_concurrent_connections,
        },
        std::env::var(SANDBOX_GATEWAY_SOCKET_ENV).ok(),
        load_gateway_section(document.as_ref())?,
    );
    config.auth_token = Some(auth_token);
    let manager = SandboxManagerRouter::new(services);
    SandboxGatewayServer::with_shutdown(config, manager, shutdown)
        .serve()
        .await?;
    Ok(())
}

fn load_gateway_section(
    document: Option<&ConfigDocument>,
) -> Result<GatewayConfig, Box<dyn std::error::Error>> {
    let Some(document) = document else {
        return Ok(GatewayConfig::default());
    };
    let section = match document.section::<GatewayConfig>("gateway") {
        Ok(section) => section,
        Err(sandbox_config::ConfigError::MissingSection { .. }) => GatewayConfig::default(),
        Err(error) => return Err(error.into()),
    };
    section.validate()?;
    Ok(section)
}

fn build_manager_services(
    backend: Backend,
    document: Option<&ConfigDocument>,
) -> Result<Arc<ManagerServices>, Box<dyn std::error::Error>> {
    match backend {
        Backend::None => Ok(default_manager_services()),
        Backend::Docker => build_docker_services(document),
    }
}

fn build_docker_services(
    document: Option<&ConfigDocument>,
) -> Result<Arc<ManagerServices>, Box<dyn std::error::Error>> {
    let document = document.ok_or("--config-yaml is required when --backend docker")?;
    let manager_config: ManagerConfig = document.section("manager")?;
    manager_config.validate()?;
    let daemon_config: DaemonConfig = document.section("daemon")?;
    daemon_config.validate()?;
    let runtime_config: RuntimeConfig = document.section("runtime")?;
    runtime_config.validate()?;
    let observability_config = match document.section::<ObservabilityConfig>("observability") {
        Ok(section) => section,
        Err(sandbox_config::ConfigError::MissingSection { .. }) => ObservabilityConfig::default(),
        Err(error) => return Err(error.into()),
    };
    observability_config.validate()?;
    if let Some(docker) = manager_config.docker.as_ref() {
        docker.validate_daemon_runtime_profile(&daemon_config, &runtime_config)?;
    }
    let export_caps = ExportApplyCaps {
        max_stream_bytes: manager_config.export.max_stream_bytes,
        max_decompressed_bytes: manager_config.export.max_decompressed_bytes,
        max_apply_entries: manager_config.export.max_apply_entries,
    };
    let snapshot_limits = ObservabilitySnapshotLimits {
        max_concurrent_requests: manager_config
            .observability_snapshot
            .max_concurrent_requests,
        timeout_ms: manager_config.observability_snapshot.timeout_ms,
    };
    let workspace_roots = match manager_config.workspace_roots.clone() {
        Some(roots) => WorkspaceRootPolicy::configured(roots)?,
        None => WorkspaceRootPolicy::default_picker()?,
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
    services.snapshot_limits = snapshot_limits;
    services.workspace_roots = workspace_roots;
    services.start_resource_sampler(Duration::from_millis(
        observability_config.resource_stats.sample_interval_ms,
    ));
    Ok(Arc::new(services))
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
    fn list_images(&self) -> Result<Vec<String>, ManagerError> {
        Err(ManagerError::RuntimeFailed {
            message: "sandbox runtime is not configured".to_owned(),
        })
    }

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
