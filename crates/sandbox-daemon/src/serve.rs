//! Daemon serve subcommand adapter.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use sandbox_config::configs::{
    daemon::{DaemonConfig, DaemonServerConfig},
    manager::ManagerConfig,
    observability::ObservabilityConfig,
    runtime::RuntimeConfig,
};

const DAEMON_AUTH_TOKEN_ENV: &str = "SANDBOX_DAEMON_AUTH_TOKEN";
const DAEMON_CONFIG_YAML_ENV: &str = "SANDBOX_DAEMON_CONFIG_YAML";
const DAEMON_SANDBOX_ID_ENV: &str = "SANDBOX_DAEMON_SANDBOX_ID";

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
    let observability_config = runtime_config.observability.clone();
    let config = DaemonCliConfig::parse(args, &daemon_config.server, config_path)?;
    if config.spawn {
        return spawn_daemon(&config);
    }
    set_runner_config_env(&config.config_yaml_path);
    set_runner_sandbox_id_env(config.sandbox_id.as_deref());
    let workspace_root = config.workspace_root.clone();
    let workload_limits = selected_workload_cgroup_limits(&runtime_config);
    let (cgroup_root, workload_cgroup_unavailable_reason) = match workload_limits {
        Some(limits) => match crate::cgroup_setup::discover_and_prepare_root(limits) {
            Ok(root) => (Some(root), None),
            Err(reason) => {
                cli_log(format!("workload cgroup unavailable: {reason}"));
                (None, Some(reason))
            }
        },
        None => (None, None),
    };
    let server_config = sandbox_daemon::ServerConfig {
        socket_path: config.socket_path,
        pid_path: config.pid_path,
        tcp_host: config.tcp_host,
        tcp_port: config.tcp_port,
        http_host: config.http_host,
        http_port: config.http_port,
        auth_token: config.auth_token,
        sandbox_id: config.sandbox_id,
        cgroup_root: cgroup_root.clone(),
        observability: observability_config,
        limits: sandbox_protocol::ProtocolLimits {
            max_request_bytes: daemon_config.server.max_request_bytes,
            request_read_timeout_s: daemon_config.server.request_read_timeout_s,
        },
        max_concurrent_connections: daemon_config.server.max_concurrent_connections,
        worker_threads: daemon_config.server.worker_threads,
        max_blocking_requests: daemon_config.server.max_blocking_threads,
        blocking_thread_keep_alive_s: daemon_config.server.blocking_thread_keep_alive_s,
        forward: daemon_config.http.forward,
    };
    let runtime = build_daemon_runtime(&daemon_config.server)?;
    let serve_result = runtime.block_on(async move {
        let server = sandbox_daemon::SandboxDaemonServer::new_with_runtime_config(
            server_config,
            build_runtime_config(
                &runtime_config,
                workspace_root,
                cgroup_root,
                workload_cgroup_unavailable_reason,
            ),
        );
        server.serve().await
    });
    serve_result?;
    Ok(())
}

pub(crate) struct DaemonRuntimeConfig {
    daemon: DaemonConfig,
    runtime: RuntimeConfig,
    observability: ObservabilityConfig,
    manager: Option<ManagerConfig>,
}

pub(crate) fn build_runtime_config(
    config: &DaemonRuntimeConfig,
    workspace_root: PathBuf,
    cgroup_root: Option<PathBuf>,
    workload_cgroup_unavailable_reason: Option<String>,
) -> sandbox_runtime::SandboxRuntimeConfig {
    sandbox_runtime::SandboxRuntimeConfig {
        cgroup_root,
        workload_cgroup_limits: selected_workload_cgroup_limits(config),
        workload_cgroup_unavailable_reason,
        workspace: sandbox_runtime::WorkspaceRuntimeConfig {
            workspace_root,
            layer_stack_root: config.runtime.workspace.layer_stack_root.clone(),
            scratch_root: config.runtime.workspace.scratch_root.clone(),
            caps: sandbox_runtime::WorkspaceResourceCaps {
                setup_timeout_s: config.runtime.workspace.setup_timeout_s,
                exit_grace_s: config.runtime.workspace.exit_grace_s,
                rfc1918_egress: match config.runtime.workspace.rfc1918_egress {
                    sandbox_config::configs::runtime::Rfc1918Egress::Allow => {
                        sandbox_runtime::Rfc1918Egress::Allow
                    }
                    sandbox_config::configs::runtime::Rfc1918Egress::Deny => {
                        sandbox_runtime::Rfc1918Egress::Deny
                    }
                },
                freeze_budget_s: config.runtime.namespace_execution.freeze_budget_s,
            },
        },
        namespace_execution: sandbox_runtime::NamespaceExecutionRuntimeConfig {
            scratch_root: config.runtime.namespace_execution.scratch_root.clone(),
            caps: sandbox_runtime::NamespaceExecutionCaps {
                freeze_budget_s: config.runtime.namespace_execution.freeze_budget_s,
                stdin_write_deadline_s: config.runtime.namespace_execution.stdin_write_deadline_s,
                max_terminal_entries: config.runtime.namespace_execution.max_terminal_entries,
                max_transcript_window_bytes: config
                    .runtime
                    .namespace_execution
                    .max_transcript_window_bytes,
                max_runner_result_bytes: config.runtime.namespace_execution.max_runner_result_bytes,
            },
        },
        layerstack: sandbox_runtime::LayerstackRuntimeConfig {
            rollout_mode: match config.runtime.layerstack.rollout_mode {
                sandbox_config::configs::runtime::StorageRolloutMode::Legacy => {
                    sandbox_runtime::StorageRolloutMode::Legacy
                }
                sandbox_config::configs::runtime::StorageRolloutMode::Validation => {
                    sandbox_runtime::StorageRolloutMode::Validation
                }
                sandbox_config::configs::runtime::StorageRolloutMode::StrictCandidate => {
                    sandbox_runtime::StorageRolloutMode::StrictCandidate
                }
            },
            remount_sweep_width: config.runtime.layerstack.remount_sweep_width,
            export_chunk_bytes: config.runtime.layerstack.export_chunk_bytes,
            spool_zstd_level: config.runtime.layerstack.spool_zstd_level,
            autosquash_squash_at_n_layers: config
                .runtime
                .layerstack
                .autosquash_policies
                .squash_at_n_layers,
        },
        command: sandbox_runtime::CommandRuntimeConfig {
            max_active: config.runtime.command.max_active,
            read_lines_default: config.runtime.command.read_lines_default,
            read_lines_max: config.runtime.command.read_lines_max,
        },
        file: sandbox_runtime::FileRuntimeConfig {
            read_lines_default: config.runtime.file.read_lines_default,
            max_output_bytes: config.runtime.file.max_output_bytes,
            max_edit_bytes: config.runtime.file.max_edit_bytes,
            max_list_entries: config.runtime.file.max_list_entries,
        },
    }
}

pub(crate) fn load_runtime_config(path: &Path) -> Result<DaemonRuntimeConfig> {
    let doc = sandbox_config::load_path(path)
        .with_context(|| format!("load daemon config {}", path.display()))?;
    let manager = load_manager_config_section(&doc)?;
    let daemon = doc
        .section::<DaemonConfig>("daemon")
        .context("deserialize daemon config section")?;
    daemon.validate().context("validate daemon config")?;
    if daemon.server.used_legacy_worker_threads() {
        eprintln!(
            "warning: daemon.server.max_worker_threads is deprecated; use daemon.server.worker_threads"
        );
    }
    let runtime = doc
        .section::<RuntimeConfig>("runtime")
        .context("deserialize runtime config section")?;
    runtime.validate().context("validate runtime config")?;
    if let Some(docker) = manager.as_ref().and_then(|manager| manager.docker.as_ref()) {
        docker
            .validate_daemon_runtime_profile(&daemon, &runtime)
            .context("validate selected daemon runtime profile")?;
    }
    let observability = doc
        .section::<ObservabilityConfig>("observability")
        .unwrap_or_default();
    observability
        .validate()
        .context("validate observability config")?;
    if observability.used_legacy_max_file_bytes {
        eprintln!(
            "warning: observability.max_file_bytes is deprecated; use max_disk_bytes (effective total: {})",
            observability.max_disk_bytes
        );
    }
    Ok(DaemonRuntimeConfig {
        daemon,
        runtime,
        observability,
        manager,
    })
}

fn load_manager_config_section(
    doc: &sandbox_config::ConfigDocument,
) -> Result<Option<ManagerConfig>> {
    match doc.section::<ManagerConfig>("manager") {
        Ok(manager) => {
            manager.validate().context("validate manager config")?;
            Ok(Some(manager))
        }
        Err(sandbox_config::ConfigError::MissingSection { section }) if section == "manager" => {
            Ok(None)
        }
        Err(error) => Err(error).context("deserialize manager config section"),
    }
}

fn selected_workload_cgroup_limits(
    config: &DaemonRuntimeConfig,
) -> Option<sandbox_runtime::WorkloadCgroupLimits> {
    let docker = config.manager.as_ref()?.docker.as_ref()?;
    let profile = docker.selected_resource_profile()?;
    if !profile.separate_workload_cgroup {
        return None;
    }
    let outer_memory_max = docker.memory_bytes.unwrap_or(profile.memory_max_bytes);
    let memory_high = profile.memory_high_bytes.min(outer_memory_max);
    Some(sandbox_runtime::WorkloadCgroupLimits {
        nano_cpus: u64::try_from(docker.nano_cpus.unwrap_or(profile.nano_cpus))
            .expect("validated resource profile nano_cpus is positive"),
        memory_high_bytes: u64::try_from(memory_high)
            .expect("validated resource profile memory high is positive"),
        memory_max_bytes: u64::try_from(profile.workload_memory_max_bytes().min(outer_memory_max))
            .expect("validated resource profile workload memory max is positive"),
        pids_max: u64::try_from(profile.workload_pids_max())
            .expect("validated resource profile workload pids max is positive"),
    })
}

pub(crate) fn build_daemon_runtime(server: &DaemonServerConfig) -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(server.worker_threads)
        .max_blocking_threads(server.max_blocking_threads)
        .thread_keep_alive(Duration::from_secs_f64(server.blocking_thread_keep_alive_s))
        .enable_all()
        .build()
        .context("failed to build daemon tokio runtime")
}

pub(crate) struct DaemonCliConfig {
    pub(crate) config_yaml_path: PathBuf,
    workspace_root: PathBuf,
    socket_path: PathBuf,
    pid_path: PathBuf,
    tcp_host: Option<String>,
    tcp_port: Option<u16>,
    http_host: Option<String>,
    http_port: Option<u16>,
    pub(crate) auth_token: Option<String>,
    pub(crate) sandbox_id: Option<String>,
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
        let mut http_host = None;
        let mut http_port = None;
        let mut auth_token = None;
        let mut sandbox_id = None;
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
                "--http-host" => http_host = Some(required_arg(&mut args, "--http-host")?),
                "--http-port" => {
                    http_port = Some(
                        required_arg(&mut args, "--http-port")?
                            .parse::<u16>()
                            .context("--http-port must be an integer 1..65535")?,
                    );
                }
                "--auth-token" => auth_token = Some(required_arg(&mut args, "--auth-token")?),
                "--sandbox-id" => sandbox_id = Some(required_arg(&mut args, "--sandbox-id")?),
                "--spawn" => spawn = true,
                "--help" | "-h" => {
                    println!(
                        "usage: serve [--spawn] --config-yaml PATH --workspace-root PATH [--socket PATH] [--pid-file PATH] [--tcp-host HOST --tcp-port PORT --auth-token TOKEN] [--http-host HOST --http-port PORT] [--sandbox-id ID]"
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
            http_host,
            http_port,
            auth_token: resolved_auth_token,
            sandbox_id: non_empty_sandbox_id(sandbox_id)?,
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
        if let Some(host) = &self.http_host {
            args.push("--http-host".to_owned());
            args.push(host.clone());
        }
        if let Some(port) = self.http_port {
            args.push("--http-port".to_owned());
            args.push(port.to_string());
        }
        if let Some(sandbox_id) = &self.sandbox_id {
            args.push("--sandbox-id".to_owned());
            args.push(sandbox_id.clone());
        }
        args
    }
}

fn has_configured_token(token: Option<&str>) -> bool {
    token.is_some_and(|token| !token.is_empty())
}

fn non_empty_sandbox_id(value: Option<String>) -> Result<Option<String>> {
    match value {
        Some(value) if value.trim().is_empty() => Err(anyhow!("--sandbox-id must be non-empty")),
        value => Ok(value),
    }
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

fn set_runner_sandbox_id_env(sandbox_id: Option<&str>) {
    match sandbox_id {
        Some(sandbox_id) => std::env::set_var(DAEMON_SANDBOX_ID_ENV, sandbox_id),
        None => std::env::remove_var(DAEMON_SANDBOX_ID_ENV),
    }
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
    process_is_live(pid)
}

fn process_is_live(pid: u32) -> bool {
    let proc_root = Path::new("/proc");
    if proc_root.is_dir() {
        proc_root.join(pid.to_string()).exists()
    } else {
        pid > 0
    }
}

fn cli_log(message: impl AsRef<str>) {
    let escaped = serde_json::to_string(message.as_ref()).unwrap_or_else(|_| "\"\"".to_owned());
    eprintln!("cli_log({escaped})");
}
