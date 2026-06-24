use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use clap::Parser as _;
use serde_json::Value;

use sandbox_e2e_live_test::cleanup::RunGuard;
use sandbox_e2e_live_test::cli_client::{CallRecord, CliClient};
use sandbox_e2e_live_test::config::{
    self, Args, CleanupPolicy, Command as Subcommand, RunArgs, RunConfig, TestSelection,
};
use sandbox_e2e_live_test::{gateway, report};

/// The SOLE stage-aware line in the whole crate. Stage 2 flips it to the full
/// suite (drop `--test manager`) and changes nothing else here (§9).
const STAGE1_DEFAULT_TARGET: &[&str] = &["--test", "manager"];

const CLI_BIN: &str = "sandbox-cli";
const RUN_ROOT_ENV: &str = "EOS_E2E_RUN_ROOT";
const RUN_MANIFEST_FILE: &str = "run-manifest.json";

const UNCONFIGURED_GATEWAY_MESSAGE: &str = "the attached gateway has no real Docker runtime: \
create_sandbox returned \"sandbox runtime is not configured\". The shipped sandbox-gateway wires \
Unconfigured* stubs (gateway/main.rs:94-146) and fails every create_sandbox. Attach a \
sandbox-gateway built with the real Docker runtime via --gateway-socket.";

fn main() -> ExitCode {
    let args = Args::parse();
    if matches!(args.command, Some(Subcommand::Preflight)) {
        return run_preflight(&args.run);
    }
    if let Some(run_id) = args.run.clean_run.clone() {
        return run_clean(&args.run, &run_id);
    }
    run_pipeline(&args.run)
}

fn run_preflight(args: &RunArgs) -> ExitCode {
    let image = match config::resolve_image(args) {
        Ok(image) => image,
        Err(error) => return fail_usage(&format!("configuration error: {error:#}")),
    };
    if let Err(message) = preflight_environment(&image) {
        eprintln!("{message}");
        return ExitCode::from(2);
    }
    let socket = match config::resolve_gateway_socket(args) {
        Ok(socket) => socket,
        Err(error) => return fail_usage(&format!("configuration error: {error:#}")),
    };
    match preflight_probe(&image, &socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

fn run_clean(args: &RunArgs, run_id: &str) -> ExitCode {
    if let Err(error) = config::validate_run_id(run_id) {
        return fail_usage(&format!("--clean-run: {error:#}"));
    }
    let run_root = config::resolve_run_root_base(args).join(run_id);
    if !run_root.exists() {
        return ExitCode::SUCCESS;
    }
    let socket = match read_manifest_socket(&run_root.join(RUN_MANIFEST_FILE)) {
        Ok(socket) => socket,
        Err(error) => return fail_usage(&format!("--clean-run: {error:#}")),
    };
    let policy = if args.keep_artifacts {
        CleanupPolicy::Never
    } else {
        CleanupPolicy::Always
    };
    let guard = RunGuard::new(run_root, socket, policy);
    let cleanup = guard.teardown();
    println!(
        "eos-e2e --clean-run {run_id}: destroyed {} sandbox(es), removed_run_root={}",
        cleanup.destroyed_sandbox_ids.len(),
        cleanup.removed_run_root,
    );
    ExitCode::SUCCESS
}

fn run_pipeline(args: &RunArgs) -> ExitCode {
    let config = match RunConfig::resolve(args) {
        Ok(config) => config,
        Err(error) => return fail_usage(&format!("configuration error: {error:#}")),
    };

    if let Err(message) = preflight_environment(&config.image) {
        eprintln!("{message}");
        return ExitCode::from(2);
    }
    if let Err(message) = preflight_probe(&config.image, &config.gateway_socket) {
        eprintln!("{message}");
        return ExitCode::from(2);
    }

    let git_head = config::git_head().unwrap_or_default();
    if let Err(error) = report::write_run_manifest(&config.run_root, &config, &git_head) {
        return fail_usage(&format!(
            "failed to write run manifest under {}: {error}",
            config.run_root.display()
        ));
    }

    let mut guard = RunGuard::new(
        config.run_root.clone(),
        config.gateway_socket.clone(),
        config.cleanup,
    );

    let started_at = config::utc_stamp();
    let runner_wall = Instant::now();

    let build = report::BuildTiming {
        gateway_build_ms: 0,
        cli_build_ms: 0,
        cargo_profile: config.build.cargo_profile(),
        cache_hit: false,
    };

    let attach = Instant::now();
    if let Err(error) = gateway::await_ready(&config.gateway_socket) {
        eprintln!("eos-e2e: gateway not ready: {error:#}");
        return ExitCode::from(2);
    }
    let gateway_attach_ms = attach.elapsed().as_millis();

    let filters = match test_filters(&config.tests) {
        Ok(filters) => filters,
        Err(error) => return fail_usage(&format!("{error:#}")),
    };

    let test_start = Instant::now();
    let cargo_status = run_cargo_test(&config, &filters);
    let test_process_ms = test_start.elapsed().as_millis();

    let tests = report::build_tests(&config.run_root);
    let counts = report::Counts::tally(&tests);
    let cargo_ran = cargo_status.is_some();
    let cargo_ok = cargo_status == Some(0);
    let all_passed = tests.iter().all(|test| test.status == "passed");

    let status = if !cargo_ran {
        "error"
    } else if cargo_ok && all_passed {
        "passed"
    } else {
        "failed"
    };
    guard.set_succeeded(status == "passed");

    let failed_tests: Vec<String> = tests
        .iter()
        .filter(|test| test.status == "failed")
        .map(|test| test.name.clone())
        .collect();
    let per_test = tests
        .iter()
        .map(|test| report::PerTest {
            name: test.name.clone(),
            sandbox_id: test.sandbox_id.clone(),
            total_ms: test.duration_ms,
        })
        .collect();

    let mut summary = report::Summary {
        schema_version: report::SUMMARY_SCHEMA_VERSION,
        run_id: config.run_id.clone(),
        git_head,
        started_at,
        finished_at: config::utc_stamp(),
        max_parallel: config.max_parallel,
        status: status.to_owned(),
        counts,
        tests,
        failed_tests,
        artifacts_root: config.run_root.to_string_lossy().into_owned(),
        timing: report::Timing {
            build,
            runner: report::RunnerTiming {
                wall_ms: runner_wall.elapsed().as_millis(),
                gateway_attach_ms,
                test_process_ms,
                teardown_ms: 0,
                max_parallel: config.max_parallel,
            },
            per_test,
        },
        cleanup: guard.plan(),
    };
    let _ = report::write_summary(&config.run_root, &summary);

    let teardown_start = Instant::now();
    let cleanup = guard.teardown();
    let teardown_ms = teardown_start.elapsed().as_millis();

    if !cleanup.removed_run_root {
        summary.cleanup = cleanup;
        summary.timing.runner.teardown_ms = teardown_ms;
        summary.timing.runner.wall_ms = runner_wall.elapsed().as_millis();
        summary.finished_at = config::utc_stamp();
        let _ = report::write_summary(&config.run_root, &summary);
    }

    print_summary_line(&summary);

    if status == "passed" {
        ExitCode::SUCCESS
    } else {
        print_focused_rerun(&config);
        if cargo_ran {
            ExitCode::from(1)
        } else {
            ExitCode::from(2)
        }
    }
}

/// Preflight checks 1–3 (§3.2): Linux, Docker reachable, image present. None
/// need a gateway socket, so `eos-e2e preflight` surfaces these before demanding
/// one.
fn preflight_environment(image: &str) -> Result<(), String> {
    if std::env::consts::OS != "linux" {
        return Err(format!(
            "EphemeralOS E2E is Linux+Docker only; current OS={}",
            std::env::consts::OS
        ));
    }
    if !command_succeeds("docker", &["version"]) {
        return Err("Docker daemon not reachable at $DOCKER_HOST".to_owned());
    }
    if !command_succeeds("docker", &["image", "inspect", image]) {
        return Err(format!(
            "image {image} not present; run `docker pull {image}`"
        ));
    }
    Ok(())
}

/// Preflight check 4 (§3.2.1): the one black-box `create_sandbox` that trips the
/// runtime trait. A scratch temp workspace is created and removed; an
/// unconfigured gateway is detected by the carried `runtime is not configured`
/// substring; a real gateway's probe sandbox is destroyed immediately.
fn preflight_probe(image: &str, gateway_socket: &Path) -> Result<(), String> {
    let scratch = std::env::temp_dir().join("eos-e2e-preflight");
    if let Err(error) = std::fs::create_dir_all(&scratch) {
        return Err(format!(
            "failed to create preflight scratch dir {}: {error}",
            scratch.display()
        ));
    }
    let cli = CliClient::new(PathBuf::from(CLI_BIN), gateway_socket.to_path_buf());
    let scratch_arg = scratch.to_string_lossy().into_owned();
    let record = cli.manager(
        "create_sandbox",
        &["--image", image, "--workspace-root", &scratch_arg],
    );
    let result = interpret_probe(&cli, &record);
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn interpret_probe(cli: &CliClient, record: &CallRecord) -> Result<(), String> {
    if record.exit_code == 0 {
        if let Some(id) = record.response().pointer("/id").and_then(Value::as_str) {
            let _ = cli.manager("destroy_sandbox", &["--sandbox-id", id]);
            return Ok(());
        }
        return Err(format!(
            "preflight create_sandbox succeeded but returned no /id: {}",
            record.response()
        ));
    }
    if record
        .response()
        .to_string()
        .contains("runtime is not configured")
        || record.stderr.contains("runtime is not configured")
    {
        return Err(UNCONFIGURED_GATEWAY_MESSAGE.to_owned());
    }
    let message = record
        .response()
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| record.stderr.trim().to_owned());
    if message.is_empty() {
        Err("preflight create_sandbox failed with no error message".to_owned())
    } else {
        Err(message)
    }
}

fn run_cargo_test(config: &RunConfig, filters: &[String]) -> Option<i32> {
    let mut command = Command::new("cargo");
    command.args(["test", "-p", "sandbox-e2e-live-test"]);
    command.args(STAGE1_DEFAULT_TARGET);
    command.arg("--");
    for filter in filters {
        command.arg(filter);
    }
    command.arg(format!("--test-threads={}", config.max_parallel));
    command.env(RUN_ROOT_ENV, &config.run_root);
    command
        .status()
        .ok()
        .map(|status| status.code().unwrap_or(-1))
}

fn test_filters(tests: &TestSelection) -> anyhow::Result<Vec<String>> {
    match tests {
        TestSelection::All => Ok(Vec::new()),
        TestSelection::Names(names) => Ok(names.clone()),
        TestSelection::RerunFailedFrom(path) => {
            let bytes = std::fs::read(path).map_err(|error| {
                anyhow::anyhow!("reading rerun summary {}: {error}", path.display())
            })?;
            let summary: Value = serde_json::from_slice(&bytes).map_err(|error| {
                anyhow::anyhow!("parsing rerun summary {}: {error}", path.display())
            })?;
            let failed = summary
                .get("failed_tests")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(failed)
        }
    }
}

fn read_manifest_socket(path: &Path) -> anyhow::Result<PathBuf> {
    let bytes = std::fs::read(path)
        .map_err(|error| anyhow::anyhow!("reading {}: {error}", path.display()))?;
    let manifest: Value = serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("parsing {}: {error}", path.display()))?;
    let socket = manifest
        .get("gateway_socket")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{} has no gateway_socket", path.display()))?;
    Ok(PathBuf::from(socket))
}

fn command_succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn print_summary_line(summary: &report::Summary) {
    println!(
        "eos-e2e: run {} status={} (passed={} failed={} errored={}), cleanup removed_run_root={}",
        summary.run_id,
        summary.status,
        summary.counts.passed,
        summary.counts.failed,
        summary.counts.errored,
        summary.cleanup.removed_run_root,
    );
}

fn print_focused_rerun(config: &RunConfig) {
    eprintln!(
        "eos-e2e: rerun only the failures with: eos-e2e --gateway-socket {} --rerun-failed-from {}",
        config.gateway_socket.display(),
        config.run_root.join("summary.json").display(),
    );
}

fn fail_usage(message: &str) -> ExitCode {
    eprintln!("eos-e2e: {message}");
    ExitCode::from(2)
}
