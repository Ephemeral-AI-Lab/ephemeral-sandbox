//! `sandbox-daemon ns-runner` subcommand adapter.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::RawFd;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use sandbox_config::configs::runner::RunnerConfig;

const DAEMON_CONFIG_YAML_ENV: &str = "SANDBOX_DAEMON_CONFIG_YAML";

/// Execute one command inside a holder namespace, reading the
/// resolved `NamespaceRunnerRequest` payload and emitting the `RunResult` JSON.
///
/// This is a thin call into the `sandbox-runtime-namespace-process` runner module:
/// read the request payload from `--request-fd <fd>`, load the runner
/// config, dispatch the selected [`NsRunnerOperation`], and write the compact
/// `RunResult` JSON to `--result-fd <fd>`.
pub(crate) fn run(args: std::env::Args) -> Result<()> {
    let config = RunnerCliConfig::parse(args)?;
    let request_json = read_payload_from_fd(config.request_fd)?;
    let request: sandbox_runtime_namespace_process::runner::protocol::NamespaceRunnerRequest =
        serde_json::from_str(&request_json).context("failed to decode ns-runner request JSON")?;
    let config_doc = load_runner_config_document()?;
    let runner_config = runner_config_from_document(&config_doc)?;
    let mut result_target = open_fd_for_write(config.result_fd)
        .with_context(|| format!("failed to open ns-runner result fd {}", config.result_fd))?;
    let mode = config.mode;
    let result = dispatch_runner_mode(mode, &request, &runner_config)?;
    let output = serde_json::to_vec(&result).context("failed to encode ns-runner result JSON")?;
    write_payload(&mut result_target, &output)
}

fn dispatch_runner_mode(
    operation: NsRunnerOperation,
    request: &sandbox_runtime_namespace_process::runner::protocol::NamespaceRunnerRequest,
    runner_config: &RunnerConfig,
) -> Result<sandbox_runtime_namespace_process::runner::protocol::RunResult> {
    match operation {
        NsRunnerOperation::RemountOverlay => Ok(
            sandbox_runtime_namespace_process::runner::protocol::RunResult {
                exit_code: 0,
                payload: sandbox_runtime_namespace_process::runner::setns::remount_overlay(
                    request,
                    &runner_config.mount_mask.hidden_paths,
                )
                .context("ns-runner remount overlay failed")?,
            },
        ),
        NsRunnerOperation::MountOverlay => Ok(mount_overlay_result(
            sandbox_runtime_namespace_process::runner::setns::setns_overlay_mount(
                request,
                &runner_config.mount_mask.hidden_paths,
            ),
        )),
        NsRunnerOperation::Run => {
            sandbox_runtime_namespace_process::runner::run(request).context("ns-runner failed")
        }
    }
}

pub(crate) fn mount_overlay_result(
    outcome: Result<(), impl std::fmt::Display>,
) -> sandbox_runtime_namespace_process::runner::protocol::RunResult {
    match outcome {
        Ok(()) => ok_result(),
        Err(error) => sandbox_runtime_namespace_process::runner::protocol::RunResult {
            exit_code: 1,
            payload: serde_json::json!({
                "error": format!("ns-runner setns overlay mount failed: {error}")
            }),
        },
    }
}

fn ok_result() -> sandbox_runtime_namespace_process::runner::protocol::RunResult {
    sandbox_runtime_namespace_process::runner::protocol::RunResult {
        exit_code: 0,
        payload: serde_json::json!({"success": true, "status": "ok"}),
    }
}

fn load_runner_config_document() -> Result<sandbox_config::ConfigDocument> {
    let path = std::env::var_os(DAEMON_CONFIG_YAML_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("{DAEMON_CONFIG_YAML_ENV} is required for ns-runner"))?;
    sandbox_config::load_path(&path).with_context(|| format!("load {}", path.display()))
}

fn runner_config_from_document(doc: &sandbox_config::ConfigDocument) -> Result<RunnerConfig> {
    let config = doc
        .section::<RunnerConfig>("runner")
        .context("deserialize runner config section")?;
    config.validate().context("validate runner config")?;
    Ok(config)
}

/// Which ns-runner operation the flags selected; default is command execution.
#[derive(Clone, Copy)]
enum NsRunnerOperation {
    Run,
    MountOverlay,
    RemountOverlay,
}

pub(crate) struct RunnerCliConfig {
    request_fd: RawFd,
    result_fd: RawFd,
    mode: NsRunnerOperation,
}

impl RunnerCliConfig {
    pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut request_fd = None;
        let mut result_fd = None;
        let mut mode = None;
        let mut set_mode = |selected: NsRunnerOperation| {
            if mode.is_some() {
                return Err(anyhow!(
                    "ns-runner accepts only one of --mount-overlay or --remount-overlay"
                ));
            }
            mode = Some(selected);
            Ok(())
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mount-overlay" => set_mode(NsRunnerOperation::MountOverlay)?,
                "--remount-overlay" => set_mode(NsRunnerOperation::RemountOverlay)?,
                "--request-fd" => {
                    request_fd = Some(
                        args.next()
                            .ok_or_else(|| anyhow!("--request-fd requires a file descriptor"))?
                            .parse::<RawFd>()
                            .context("--request-fd must be an integer file descriptor")?,
                    );
                }
                "--result-fd" => {
                    result_fd = Some(
                        args.next()
                            .ok_or_else(|| anyhow!("--result-fd requires a file descriptor"))?
                            .parse::<RawFd>()
                            .context("--result-fd must be an integer file descriptor")?,
                    );
                }
                "--help" | "-h" => {
                    println!(
                        "usage: sandbox-daemon ns-runner [--mount-overlay | --remount-overlay] [--request-fd FD] [--result-fd FD]"
                    );
                    std::process::exit(0);
                }
                other if other.starts_with('-') => {
                    return Err(anyhow!("unknown ns-runner flag {other:?}"));
                }
                other => {
                    return Err(anyhow!(
                        "unexpected ns-runner positional argument {other:?}; use --request-fd FD"
                    ));
                }
            }
        }
        let request_fd = request_fd.ok_or_else(|| anyhow!("ns-runner requires --request-fd FD"))?;
        let result_fd = result_fd.ok_or_else(|| anyhow!("ns-runner requires --result-fd FD"))?;
        Ok(Self {
            request_fd,
            result_fd,
            mode: mode.unwrap_or(NsRunnerOperation::Run),
        })
    }
}

fn open_fd_for_read(fd: RawFd) -> std::io::Result<File> {
    File::open(format!("/proc/self/fd/{fd}")).or_else(|_| File::open(format!("/dev/fd/{fd}")))
}

pub(crate) fn open_fd_for_write(fd: RawFd) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .open(format!("/proc/self/fd/{fd}"))
        .or_else(|_| OpenOptions::new().write(true).open(format!("/dev/fd/{fd}")))
}

fn read_payload_from_fd(fd: RawFd) -> Result<String> {
    let mut payload = String::new();
    open_fd_for_read(fd)
        .with_context(|| format!("failed to open ns-runner request fd {fd}"))?
        .read_to_string(&mut payload)
        .with_context(|| format!("failed to read ns-runner request fd {fd}"))?;
    Ok(payload)
}

fn write_payload(target: &mut File, payload: &[u8]) -> Result<()> {
    target
        .write_all(payload)
        .context("failed to write ns-runner output")
}
