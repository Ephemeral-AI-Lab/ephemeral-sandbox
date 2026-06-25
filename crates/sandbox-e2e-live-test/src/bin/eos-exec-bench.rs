use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

use anyhow::{Context as _, Result};
use clap::Parser as _;
use sandbox_e2e_live_test::cli_client::{CallRecord, CliClient};
use sandbox_e2e_live_test::{config, gateway};
use serde::Serialize;

const DEFAULT_COMMANDS: &[&str] = &["pwd", "ls", "python3 -c 'print(42)'"];
const DEFAULT_CONCURRENCY: &str = "1,5,10,20";
const DEFAULT_ITERATIONS_PER_WORKER: usize = 3;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_YIELD_TIME_MS: u64 = 30_000;

#[derive(clap::Parser)]
#[command(
    name = "eos-exec-bench",
    about = "Measure live sandbox exec_command latency through sandbox-cli"
)]
struct Args {
    /// Gateway socket/address, for example 127.0.0.1:7878.
    #[arg(long)]
    gateway_socket: Option<PathBuf>,
    /// Container image used for benchmark sandboxes.
    #[arg(long, default_value = "python:3.11-bookworm")]
    image: String,
    /// Comma-separated worker counts.
    #[arg(long, value_delimiter = ',', default_value = DEFAULT_CONCURRENCY)]
    concurrency: Vec<usize>,
    /// Command to benchmark. Repeat to override the default command set.
    #[arg(long = "command")]
    commands: Vec<String>,
    /// Samples per worker for each concurrency x command scenario.
    #[arg(long, default_value_t = DEFAULT_ITERATIONS_PER_WORKER)]
    iterations_per_worker: usize,
    /// Sequential warmup execs before collecting samples for each scenario.
    #[arg(long, default_value_t = 1)]
    warmups: usize,
    /// Timeout passed to each exec_command.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_MS)]
    timeout_ms: u64,
    /// Initial exec_command yield window. Defaults high so short commands return terminal timings.
    #[arg(long, default_value_t = DEFAULT_YIELD_TIME_MS)]
    yield_time_ms: u64,
    /// Workspace behavior for measured exec_command calls.
    #[arg(long, value_enum, default_value_t = WorkspaceMode::SharedSession)]
    workspace_mode: WorkspaceMode,
    /// Explicit run id. Defaults to bench-<UTC stamp>.
    #[arg(long)]
    run_id: Option<String>,
    /// Exact run root. Defaults to ${TMPDIR:-/tmp}/eos-exec-bench/<run-id>.
    #[arg(long)]
    run_root: Option<PathBuf>,
    /// Path to sandbox-cli. Defaults to ./bin/sandbox-cli when present, else sandbox-cli.
    #[arg(long)]
    cli_bin: Option<PathBuf>,
}

#[derive(Serialize)]
struct BenchSummary {
    schema_version: u32,
    run_id: String,
    image: String,
    gateway_socket: String,
    cli_bin: String,
    iterations_per_worker: usize,
    warmups: usize,
    timeout_ms: u64,
    yield_time_ms: u64,
    workspace_mode: WorkspaceMode,
    scenarios: Vec<ScenarioSummary>,
}

#[derive(Clone, Copy, clap::ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkspaceMode {
    /// Pre-create one workspace session per scenario and share it across workers.
    SharedSession,
    /// Let every exec_command use the runtime's one-shot workspace path.
    OneShot,
}

impl std::fmt::Display for WorkspaceMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SharedSession => "shared_session",
            Self::OneShot => "one_shot",
        })
    }
}

#[derive(Serialize)]
struct ScenarioSummary {
    command: String,
    command_label: String,
    concurrency: usize,
    workspace_mode: WorkspaceMode,
    workspace_session_id: Option<String>,
    sandbox_id: String,
    workspace_root: String,
    observability: Option<BenchObservabilitySnapshot>,
    total_samples: usize,
    ok_samples: usize,
    failed_samples: usize,
    cli_latency_ms: Stats,
    daemon_wall_ms: Option<Stats>,
    command_total_ms: Option<Stats>,
    transport_overhead_ms: Option<Stats>,
    samples: Vec<Sample>,
}

#[derive(Clone, Serialize)]
struct BenchObservabilitySnapshot {
    source_exit_code: i32,
    source_latency_ms: u128,
    availability: Option<String>,
    sampled_at_unix_ms: Option<i64>,
    workspace_count: usize,
    sandbox_resources: Option<BenchResourceSample>,
    workspace_resources: Option<BenchResourceSample>,
    errors: Vec<String>,
}

#[derive(Clone, Serialize)]
struct BenchResourceSample {
    sampled_at_unix_ms: Option<i64>,
    sample_delta_ms: Option<i64>,
    cgroup_available: Option<bool>,
    cpu_usage_usec: Option<i64>,
    cpu_usage_delta_usec: Option<i64>,
    memory_current_bytes: Option<i64>,
    memory_current_delta_bytes: Option<i64>,
    memory_max_bytes: Option<i64>,
    memory_max_unlimited: Option<bool>,
    disk_upperdir_bytes: Option<i64>,
    disk_upperdir_delta_bytes: Option<i64>,
    disk_file_count: Option<i64>,
    disk_dir_count: Option<i64>,
    disk_symlink_count: Option<i64>,
    disk_truncated: Option<bool>,
}

#[derive(Clone, Serialize)]
struct Sample {
    worker: usize,
    iteration: usize,
    cli_exit_code: i32,
    status: String,
    process_exit_code: Option<i64>,
    cli_latency_ms: u128,
    daemon_wall_ms: Option<f64>,
    command_total_ms: Option<f64>,
    transport_overhead_ms: Option<f64>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct Stats {
    min: f64,
    p50: f64,
    p90: f64,
    p95: f64,
    p99: f64,
    max: f64,
    avg: f64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let gateway_socket = args
        .gateway_socket
        .clone()
        .or_else(|| std::env::var_os("SANDBOX_GATEWAY_SOCKET").map(PathBuf::from))
        .context("--gateway-socket or SANDBOX_GATEWAY_SOCKET is required")?;
    let cli_bin = args.cli_bin.clone().unwrap_or_else(default_cli_bin);
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| format!("bench-{}", config::utc_stamp()));
    config::validate_run_id(&run_id)?;
    let run_root = args
        .run_root
        .clone()
        .unwrap_or_else(|| default_run_root(&run_id));
    validate_args(&args)?;
    std::fs::create_dir_all(run_root.join("work"))
        .with_context(|| format!("create run root {}", run_root.display()))?;
    gateway::await_ready(&gateway_socket, gateway::DEFAULT_READY_TIMEOUT)
        .with_context(|| format!("gateway not ready at {}", gateway_socket.display()))?;

    let commands = commands(&args);
    let cli = CliClient::new(cli_bin.clone(), gateway_socket.clone());
    let mut scenarios = Vec::new();
    for concurrency in &args.concurrency {
        for command in &commands {
            scenarios.push(run_scenario(
                &cli,
                &cli_bin,
                &gateway_socket,
                &run_root,
                &run_id,
                &args.image,
                command,
                *concurrency,
                args.iterations_per_worker,
                args.warmups,
                args.timeout_ms,
                args.yield_time_ms,
                args.workspace_mode,
            )?);
        }
    }

    let summary = BenchSummary {
        schema_version: 1,
        run_id,
        image: args.image,
        gateway_socket: gateway_socket.to_string_lossy().into_owned(),
        cli_bin: cli_bin.to_string_lossy().into_owned(),
        iterations_per_worker: args.iterations_per_worker,
        warmups: args.warmups,
        timeout_ms: args.timeout_ms,
        yield_time_ms: args.yield_time_ms,
        workspace_mode: args.workspace_mode,
        scenarios,
    };
    let summary_path = run_root.join("summary.json");
    std::fs::write(&summary_path, serde_json::to_vec_pretty(&summary)?)
        .with_context(|| format!("write {}", summary_path.display()))?;
    print_table(&summary, &summary_path);
    Ok(())
}

fn run_scenario(
    cli: &CliClient,
    cli_bin: &PathBuf,
    gateway_socket: &PathBuf,
    run_root: &std::path::Path,
    run_id: &str,
    image: &str,
    command: &str,
    concurrency: usize,
    iterations_per_worker: usize,
    warmups: usize,
    timeout_ms: u64,
    yield_time_ms: u64,
    workspace_mode: WorkspaceMode,
) -> Result<ScenarioSummary> {
    let command_label = command_label(command);
    let workspace_root = run_root
        .join("work")
        .join(format!("{run_id}-c{concurrency}-{command_label}"));
    std::fs::create_dir_all(&workspace_root)
        .with_context(|| format!("create workspace root {}", workspace_root.display()))?;
    let workspace_root = workspace_root
        .canonicalize()
        .with_context(|| format!("canonicalize workspace root {}", workspace_root.display()))?;
    let workspace_arg = workspace_root.to_string_lossy().into_owned();
    let create = cli.manager(
        "create_sandbox",
        &["--image", image, "--workspace-root", &workspace_arg],
    );
    ensure_ok(&create, "create_sandbox")?;
    let sandbox_id = create
        .response()
        .get("id")
        .and_then(serde_json::Value::as_str)
        .context("create_sandbox response missing /id")?
        .to_owned();

    let workspace_session_id = match workspace_mode {
        WorkspaceMode::SharedSession => Some(create_workspace_session(cli, &sandbox_id)?),
        WorkspaceMode::OneShot => None,
    };
    let result = run_scenario_inner(
        cli_bin,
        gateway_socket,
        command,
        &command_label,
        concurrency,
        iterations_per_worker,
        warmups,
        timeout_ms,
        yield_time_ms,
        workspace_mode,
        workspace_session_id.as_deref(),
        &sandbox_id,
        &workspace_arg,
    );
    let result = result.map(|mut scenario| {
        scenario.observability = Some(capture_observability(
            cli,
            &sandbox_id,
            workspace_session_id.as_deref(),
        ));
        scenario
    });
    if let Some(workspace_session_id) = &workspace_session_id {
        let destroy_session = cli.runtime(
            &sandbox_id,
            "destroy_workspace_session",
            &["--workspace-session-id", workspace_session_id],
        );
        if let Err(error) = ensure_ok(&destroy_session, "destroy_workspace_session") {
            eprintln!("warning: {error:#}");
        }
    }
    let destroy = cli.manager("destroy_sandbox", &["--sandbox-id", &sandbox_id]);
    if let Err(error) = ensure_ok(&destroy, "destroy_sandbox") {
        eprintln!("warning: {error:#}");
    }
    result
}

fn run_scenario_inner(
    cli_bin: &PathBuf,
    gateway_socket: &PathBuf,
    command: &str,
    command_label: &str,
    concurrency: usize,
    iterations_per_worker: usize,
    warmups: usize,
    timeout_ms: u64,
    yield_time_ms: u64,
    workspace_mode: WorkspaceMode,
    workspace_session_id: Option<&str>,
    sandbox_id: &str,
    workspace_root: &str,
) -> Result<ScenarioSummary> {
    let warmup_cli = CliClient::new(cli_bin.clone(), gateway_socket.clone());
    for _ in 0..warmups {
        let timeout = timeout_ms.to_string();
        let yield_time = yield_time_ms.to_string();
        let record = exec_command(
            &warmup_cli,
            sandbox_id,
            workspace_session_id,
            command,
            &timeout,
            &yield_time,
        );
        ensure_ok(&record, "warmup exec_command")?;
    }

    let barrier = Arc::new(Barrier::new(concurrency));
    let mut handles = Vec::with_capacity(concurrency);
    for worker in 0..concurrency {
        let barrier = Arc::clone(&barrier);
        let cli_bin = cli_bin.clone();
        let gateway_socket = gateway_socket.clone();
        let sandbox_id = sandbox_id.to_owned();
        let command = command.to_owned();
        let workspace_session_id = workspace_session_id.map(str::to_owned);
        handles.push(thread::spawn(move || {
            let cli = CliClient::new(cli_bin, gateway_socket);
            let timeout = timeout_ms.to_string();
            let yield_time = yield_time_ms.to_string();
            barrier.wait();
            (0..iterations_per_worker)
                .map(|iteration| {
                    let record = exec_command(
                        &cli,
                        &sandbox_id,
                        workspace_session_id.as_deref(),
                        &command,
                        &timeout,
                        &yield_time,
                    );
                    sample(worker, iteration, &record)
                })
                .collect::<Vec<_>>()
        }));
    }

    let mut samples = Vec::with_capacity(concurrency * iterations_per_worker);
    for handle in handles {
        samples.extend(
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("benchmark worker thread panicked"))?,
        );
    }
    samples.sort_by_key(|sample| (sample.iteration, sample.worker));

    let ok_samples = samples
        .iter()
        .filter(|sample| sample.cli_exit_code == 0 && sample.status == "ok")
        .count();
    let failed_samples = samples.len() - ok_samples;
    let cli_latency_ms = stats(
        samples
            .iter()
            .map(|sample| sample.cli_latency_ms as f64)
            .collect(),
    )
    .context("scenario has no CLI latency samples")?;
    let daemon_wall_ms = stats_optional(samples.iter().filter_map(|sample| sample.daemon_wall_ms));
    let command_total_ms =
        stats_optional(samples.iter().filter_map(|sample| sample.command_total_ms));
    let transport_overhead_ms = stats_optional(
        samples
            .iter()
            .filter_map(|sample| sample.transport_overhead_ms),
    );

    Ok(ScenarioSummary {
        command: command.to_owned(),
        command_label: command_label.to_owned(),
        concurrency,
        workspace_mode,
        workspace_session_id: workspace_session_id.map(str::to_owned),
        sandbox_id: sandbox_id.to_owned(),
        workspace_root: workspace_root.to_owned(),
        observability: None,
        total_samples: samples.len(),
        ok_samples,
        failed_samples,
        cli_latency_ms,
        daemon_wall_ms,
        command_total_ms,
        transport_overhead_ms,
        samples,
    })
}

fn capture_observability(
    cli: &CliClient,
    sandbox_id: &str,
    workspace_session_id: Option<&str>,
) -> BenchObservabilitySnapshot {
    let record = cli.manager(
        "get_observability_tree",
        &["--sandbox-id", sandbox_id, "--resource-window-ms", "60000"],
    );
    let mut snapshot = BenchObservabilitySnapshot {
        source_exit_code: record.exit_code,
        source_latency_ms: record.latency_ms,
        availability: None,
        sampled_at_unix_ms: None,
        workspace_count: 0,
        sandbox_resources: None,
        workspace_resources: None,
        errors: Vec::new(),
    };
    if record.exit_code != 0 {
        snapshot.errors.push(observability_error_message(
            &record,
            "get_observability_tree failed",
        ));
        return snapshot;
    }
    let Some(node) = record
        .response()
        .get("sandboxes")
        .and_then(serde_json::Value::as_array)
        .and_then(|nodes| {
            nodes.iter().find(|node| {
                node.get("sandbox_id").and_then(serde_json::Value::as_str) == Some(sandbox_id)
            })
        })
    else {
        snapshot
            .errors
            .push("get_observability_tree response did not include benchmark sandbox".to_owned());
        return snapshot;
    };

    snapshot.availability = node
        .get("availability")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    snapshot.sampled_at_unix_ms = node
        .get("sampled_at_unix_ms")
        .and_then(serde_json::Value::as_i64);
    snapshot.workspace_count = node
        .get("workspaces")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    snapshot.errors.extend(node_errors(node));
    snapshot.sandbox_resources = node
        .pointer("/resources/latest")
        .and_then(resource_sample_from_value);
    snapshot.workspace_resources = workspace_resource_sample(node, workspace_session_id);
    snapshot
}

fn workspace_resource_sample(
    node: &serde_json::Value,
    workspace_session_id: Option<&str>,
) -> Option<BenchResourceSample> {
    let workspaces = node.get("workspaces")?.as_array()?;
    let workspace = match workspace_session_id {
        Some(workspace_session_id) => workspaces.iter().find(|workspace| {
            workspace
                .get("workspace_id")
                .and_then(serde_json::Value::as_str)
                == Some(workspace_session_id)
        }),
        None => workspaces.first(),
    }?;
    workspace
        .pointer("/resources/latest")
        .and_then(resource_sample_from_value)
}

fn resource_sample_from_value(value: &serde_json::Value) -> Option<BenchResourceSample> {
    if value.is_null() {
        return None;
    }
    let cgroup = value.get("cgroup");
    let disk = value.get("disk");
    Some(BenchResourceSample {
        sampled_at_unix_ms: value
            .get("sampled_at_unix_ms")
            .and_then(serde_json::Value::as_i64),
        sample_delta_ms: value
            .get("sample_delta_ms")
            .and_then(serde_json::Value::as_i64),
        cgroup_available: cgroup
            .and_then(|value| value.get("available"))
            .and_then(serde_json::Value::as_bool),
        cpu_usage_usec: cgroup
            .and_then(|value| value.get("cpu_usage_usec"))
            .and_then(serde_json::Value::as_i64),
        cpu_usage_delta_usec: cgroup
            .and_then(|value| value.get("cpu_usage_delta_usec"))
            .and_then(serde_json::Value::as_i64),
        memory_current_bytes: cgroup
            .and_then(|value| value.get("memory_current_bytes"))
            .and_then(serde_json::Value::as_i64),
        memory_current_delta_bytes: cgroup
            .and_then(|value| value.get("memory_current_delta_bytes"))
            .and_then(serde_json::Value::as_i64),
        memory_max_bytes: cgroup
            .and_then(|value| value.get("memory_max_bytes"))
            .and_then(serde_json::Value::as_i64),
        memory_max_unlimited: cgroup
            .and_then(|value| value.get("memory_max_unlimited"))
            .and_then(serde_json::Value::as_bool),
        disk_upperdir_bytes: disk
            .and_then(|value| value.get("upperdir_bytes"))
            .and_then(serde_json::Value::as_i64),
        disk_upperdir_delta_bytes: disk
            .and_then(|value| value.get("upperdir_delta_bytes"))
            .and_then(serde_json::Value::as_i64),
        disk_file_count: disk
            .and_then(|value| value.get("file_count"))
            .and_then(serde_json::Value::as_i64),
        disk_dir_count: disk
            .and_then(|value| value.get("dir_count"))
            .and_then(serde_json::Value::as_i64),
        disk_symlink_count: disk
            .and_then(|value| value.get("symlink_count"))
            .and_then(serde_json::Value::as_i64),
        disk_truncated: disk
            .and_then(|value| value.get("truncated"))
            .and_then(serde_json::Value::as_bool),
    })
}

fn node_errors(node: &serde_json::Value) -> Vec<String> {
    node.get("errors")
        .and_then(serde_json::Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| {
                    error
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| Some(error.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn observability_error_message(record: &CallRecord, fallback: &str) -> String {
    record
        .response()
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let stderr = record.stderr.trim();
            if stderr.is_empty() {
                fallback.to_owned()
            } else {
                stderr.to_owned()
            }
        })
}

fn create_workspace_session(cli: &CliClient, sandbox_id: &str) -> Result<String> {
    let record = cli.runtime(sandbox_id, "create_workspace_session", &[]);
    ensure_ok(&record, "create_workspace_session")?;
    record
        .response()
        .get("workspace_session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .context("create_workspace_session response missing /workspace_session_id")
}

fn exec_command(
    cli: &CliClient,
    sandbox_id: &str,
    workspace_session_id: Option<&str>,
    command: &str,
    timeout_ms: &str,
    yield_time_ms: &str,
) -> CallRecord {
    let mut args = vec![
        "--timeout-ms".to_owned(),
        timeout_ms.to_owned(),
        "--yield-time-ms".to_owned(),
        yield_time_ms.to_owned(),
    ];
    if let Some(workspace_session_id) = workspace_session_id {
        args.push("--workspace-session-id".to_owned());
        args.push(workspace_session_id.to_owned());
    }
    args.push(command.to_owned());
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    cli.runtime(sandbox_id, "exec_command", &refs)
}

fn sample(worker: usize, iteration: usize, record: &CallRecord) -> Sample {
    let response = record.response();
    let status = response
        .get("status")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            response
                .pointer("/error/kind")
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("unknown")
        .to_owned();
    let process_exit_code = response
        .get("exit_code")
        .and_then(serde_json::Value::as_i64);
    let daemon_wall_ms = response
        .get("wall_time_seconds")
        .and_then(serde_json::Value::as_f64)
        .map(|seconds| seconds * 1000.0);
    let command_total_ms = response
        .get("command_total_time_seconds")
        .and_then(serde_json::Value::as_f64)
        .map(|seconds| seconds * 1000.0);
    let transport_overhead_ms =
        daemon_wall_ms.map(|daemon_wall_ms| (record.latency_ms as f64 - daemon_wall_ms).max(0.0));
    let error = response
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            (record.exit_code != 0 && !record.stderr.trim().is_empty())
                .then(|| record.stderr.trim().to_owned())
        });
    Sample {
        worker,
        iteration,
        cli_exit_code: record.exit_code,
        status,
        process_exit_code,
        cli_latency_ms: record.latency_ms,
        daemon_wall_ms,
        command_total_ms,
        transport_overhead_ms,
        error,
    }
}

fn ensure_ok(record: &CallRecord, operation: &str) -> Result<()> {
    if record.exit_code == 0 {
        return Ok(());
    }
    let message = record
        .response()
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| record.stderr.trim().to_owned());
    anyhow::bail!(
        "{operation} failed with exit {}: {message}",
        record.exit_code
    )
}

fn stats_optional(values: impl Iterator<Item = f64>) -> Option<Stats> {
    stats(values.collect()).ok()
}

fn stats(mut values: Vec<f64>) -> Result<Stats> {
    if values.is_empty() {
        anyhow::bail!("empty sample set");
    }
    values.sort_by(f64::total_cmp);
    let sum = values.iter().sum::<f64>();
    Ok(Stats {
        min: values[0],
        p50: percentile(&values, 0.50),
        p90: percentile(&values, 0.90),
        p95: percentile(&values, 0.95),
        p99: percentile(&values, 0.99),
        max: values[values.len() - 1],
        avg: sum / values.len() as f64,
    })
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    let index = ((values.len() as f64 * quantile).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[index]
}

fn print_table(summary: &BenchSummary, summary_path: &std::path::Path) {
    println!("summary={}", summary_path.display());
    println!("workspace_mode={}", summary.workspace_mode);
    println!(
        "{:<5} {:<28} {:>7} {:>7} {:>9} {:>9} {:>10} {:>12} {:>12}",
        "conc",
        "command",
        "samples",
        "ok",
        "cli_avg",
        "cli_p95",
        "exec_avg",
        "transport",
        "transport95"
    );
    for scenario in &summary.scenarios {
        println!(
            "{:<5} {:<28} {:>7} {:>7} {:>9.1} {:>9.1} {:>10} {:>12} {:>12}",
            scenario.concurrency,
            scenario.command_label,
            scenario.total_samples,
            scenario.ok_samples,
            scenario.cli_latency_ms.avg,
            scenario.cli_latency_ms.p95,
            scenario
                .command_total_ms
                .as_ref()
                .map(|stats| format!("{:.1}", stats.avg))
                .unwrap_or_else(|| "n/a".to_owned()),
            scenario
                .transport_overhead_ms
                .as_ref()
                .map(|stats| format!("{:.1}", stats.avg))
                .unwrap_or_else(|| "n/a".to_owned()),
            scenario
                .transport_overhead_ms
                .as_ref()
                .map(|stats| format!("{:.1}", stats.p95))
                .unwrap_or_else(|| "n/a".to_owned())
        );
    }
}

fn validate_args(args: &Args) -> Result<()> {
    if args.image.trim().is_empty() {
        anyhow::bail!("--image must not be empty");
    }
    if args.iterations_per_worker == 0 {
        anyhow::bail!("--iterations-per-worker must be greater than zero");
    }
    if args.concurrency.is_empty() || args.concurrency.contains(&0) {
        anyhow::bail!("--concurrency values must be greater than zero");
    }
    if commands(args)
        .iter()
        .any(|command| command.trim().is_empty())
    {
        anyhow::bail!("--command must not be empty");
    }
    Ok(())
}

fn commands(args: &Args) -> Vec<String> {
    if args.commands.is_empty() {
        DEFAULT_COMMANDS
            .iter()
            .map(|command| (*command).to_owned())
            .collect()
    } else {
        args.commands.clone()
    }
}

fn command_label(command: &str) -> String {
    let mut label = String::new();
    for ch in command.chars() {
        if ch.is_ascii_alphanumeric() {
            label.push(ch.to_ascii_lowercase());
        } else if !label.ends_with('-') {
            label.push('-');
        }
    }
    let label = label.trim_matches('-');
    if label.is_empty() {
        "command".to_owned()
    } else {
        label.chars().take(48).collect()
    }
}

fn default_cli_bin() -> PathBuf {
    let local = PathBuf::from("bin").join("sandbox-cli");
    if local.is_file() {
        local
    } else {
        PathBuf::from("sandbox-cli")
    }
}

fn default_run_root(run_id: &str) -> PathBuf {
    let tmp = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    tmp.join("eos-exec-bench").join(run_id)
}
