//! `eosd ns-runner` subcommand adapter.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

/// Execute one tool call inside a namespace (fresh-ns or setns), reading the
/// resolved `RunRequest` payload and emitting the `RunResult` JSON.
///
/// This is a thin call into `eos-runner`: read the request payload from stdin
/// or `--request <path>`, construct the overlay mount adapter, call `run`, and
/// write compact JSON to stdout or `--output <path>`.
pub(crate) fn run(args: std::env::Args) -> Result<()> {
    let config = RunnerCliConfig::parse(args)?;
    let request_json = read_payload(config.request_path.as_ref())?;
    let request: eos_runner::RunRequest =
        serde_json::from_str(&request_json).context("failed to decode ns-runner request JSON")?;
    if config.remount_overlay {
        remount_overlay_from_request(&request).context("ns-runner remount overlay failed")?;
        let result = eos_runner::RunResult {
            exit_code: 0,
            tool_result: serde_json::json!({"success": true, "status": "ok"}),
        };
        let output =
            serde_json::to_vec(&result).context("failed to encode ns-runner result JSON")?;
        write_payload(config.output_path.as_ref(), &output)?;
        return Ok(());
    }
    if config.mount_overlay {
        eos_runner::setns::setns_overlay_mount(&request)
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
    if config.configure_dns {
        let tool_result =
            eos_runner::setns::configure_dns(&request).context("ns-runner configure dns failed")?;
        let result = eos_runner::RunResult {
            exit_code: 0,
            tool_result,
        };
        let output =
            serde_json::to_vec(&result).context("failed to encode ns-runner result JSON")?;
        write_payload(config.output_path.as_ref(), &output)?;
        return Ok(());
    }
    let result = eos_runner::run(&request).context("ns-runner failed")?;
    let output = serde_json::to_vec(&result).context("failed to encode ns-runner result JSON")?;
    write_payload(config.output_path.as_ref(), &output)?;
    Ok(())
}

struct RunnerCliConfig {
    request_path: Option<PathBuf>,
    output_path: Option<PathBuf>,
    mount_overlay: bool,
    remount_overlay: bool,
    configure_dns: bool,
}

impl RunnerCliConfig {
    fn parse(args: std::env::Args) -> Result<Self> {
        let mut request_path = None;
        let mut output_path = None;
        let mut mount_overlay = false;
        let mut remount_overlay = false;
        let mut configure_dns = false;
        let mut positional = Vec::new();
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mount-overlay" => mount_overlay = true,
                "--remount-overlay" => remount_overlay = true,
                "--configure-dns" => configure_dns = true,
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
                        "usage: eosd ns-runner [--mount-overlay | --remount-overlay | --configure-dns] [--request PATH] [--output PATH]"
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
        let special_modes =
            u8::from(mount_overlay) + u8::from(remount_overlay) + u8::from(configure_dns);
        if special_modes > 1 {
            return Err(anyhow!(
                "ns-runner accepts only one of --mount-overlay, --remount-overlay, or --configure-dns"
            ));
        }
        Ok(Self {
            request_path,
            output_path,
            mount_overlay,
            remount_overlay,
            configure_dns,
        })
    }
}

fn remount_overlay_from_request(request: &eos_runner::RunRequest) -> Result<()> {
    let upperdir = request
        .upperdir
        .clone()
        .ok_or_else(|| anyhow!("remount overlay requires upperdir"))?;
    let workdir = request
        .workdir
        .clone()
        .ok_or_else(|| anyhow!("remount overlay requires workdir"))?;
    if request.layer_paths.is_empty() {
        return Err(anyhow!("remount overlay requires layer_paths"));
    }
    let handle = eos_overlay::OverlayHandle {
        upperdir,
        workdir,
        layer_paths: request.layer_paths.clone(),
    };
    eos_overlay::unmount_overlay(&request.workspace_root.0)
        .context("failed to unmount old workspace overlay")?;
    let mount = eos_overlay::mount_overlay(&request.workspace_root.0, &handle)
        .context("failed to mount refreshed workspace overlay")?;
    std::mem::forget(mount);
    Ok(())
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
