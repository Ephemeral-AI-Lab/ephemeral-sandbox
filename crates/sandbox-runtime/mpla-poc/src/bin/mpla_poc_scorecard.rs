use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Args, Subcommand, ValueEnum};
use sandbox_runtime_mpla_poc::{
    bind_product_catalog, CatalogBinding, ControlCatalogFacts, PocConfig, RunId, INTERFACE_VERSION,
    SCHEMA_VERSION,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

type RunnerResult<T = ()> = Result<T, Box<dyn Error>>;

const CAPSULE_KIND: &str = "mpla-booster-scorecard-capsule-v1";
const CONTRACT_KIND: &str = "mpla-booster-scorecard-runner-contract-v1";
const HISTORICAL_RUN_ID: &str = "m2r-20260728T015724p0800";
const EXACT_R0_PATH: &str = "/Users/yifanxu/Ephemeral-AI-Lab/experiment/materialization-benchmark-20260727/corpus/console-release";
const EXACT_R0_LOGICAL_BYTES: u64 = 912_350_100;
const EXACT_R0_REGULAR_FILES: u64 = 3_602;
const PINNED_IMAGE: &str =
    "ubuntu@sha256:4fbb8e6a8395de5a7550b33509421a2bafbc0aab6c06ba2cef9ebffbc7092d90";
const PINNED_PLATFORM: &str = "linux/arm64";
const MAX_INPUT_FILE_BYTES: u64 = 16 * 1024 * 1024;
const R0_MANIFEST_DOMAIN: &[u8] = b"mpla-booster-scorecard-r0-manifest-v1\0";
const COMMAND_LEDGER_KIND: &str = "mpla-booster-scorecard-command-receipt-v1";
const EXECUTION_RESULT_KIND: &str = "mpla-booster-scorecard-operation-result-v1";
const DEFAULT_RUNTIME_CLI: &str = "sandbox-runtime-cli";

#[derive(Debug, Subcommand)]
pub(crate) enum ScorecardCommand {
    /// List the formal Stage 04.6 gate selectors without running them.
    List,
    /// Validate inputs and emit the immutable capsule contract.
    Preflight {
        #[command(flatten)]
        capsule: ScorecardCapsuleArgs,
    },
    /// Select one formal gate and emit its non-executing plan.
    Gate {
        #[command(flatten)]
        capsule: ScorecardCapsuleArgs,
        /// Execute one operation sample through the public runtime CLI.
        #[arg(long, default_value_t = false)]
        execute: bool,
    },
}

#[derive(Clone, Debug, Args)]
pub(crate) struct ScorecardCapsuleArgs {
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    evidence_root: PathBuf,
    #[arg(long)]
    interface_version: String,
    #[arg(long)]
    catalog_binding: PathBuf,
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    image: String,
    #[arg(long)]
    r0: PathBuf,
    #[arg(long)]
    lease_prefix: String,
    #[arg(long)]
    branch_prefix: String,
    #[arg(long)]
    sandbox_prefix: String,
    #[arg(long, value_enum)]
    case: FormalGate,
    #[arg(long)]
    samples: u32,
    #[arg(long)]
    command_timeout_ms: u64,
    /// Assert that a separately allocated matched control arm is part of a relative gate.
    #[arg(long, default_value_t = false)]
    matched_control: bool,
    /// Optionally pin the computed deterministic R0 manifest to a previously recorded value.
    #[arg(long)]
    r0_manifest_sha256: Option<String>,
    #[command(flatten)]
    adapter: PublicCliAdapterArgs,
}

#[derive(Clone, Debug, Args, Serialize)]
struct PublicCliAdapterArgs {
    /// Public runtime CLI program or path. Resolution happens only with --execute.
    #[arg(long, default_value = DEFAULT_RUNTIME_CLI)]
    runtime_cli: PathBuf,
    /// Explicit gateway endpoint forwarded to the public runtime CLI.
    #[arg(long)]
    gateway_socket: Option<PathBuf>,
    /// Candidate sandbox already allocated for this gate.
    #[arg(long)]
    sandbox_id: Option<String>,
    /// Distinct matched-control sandbox required by BG gates.
    #[arg(long)]
    control_sandbox_id: Option<String>,
    /// Existing candidate MPLA workspace session required by publish/stream.
    #[arg(long)]
    workspace_session_id: Option<String>,
    /// Existing control MPLA workspace session required by publish control.
    #[arg(long)]
    control_workspace_session_id: Option<String>,
    /// Candidate branch for activation, fork destination, rollback, publish, or squash.
    #[arg(long)]
    branch: Option<String>,
    /// Existing source branch required by fork.
    #[arg(long)]
    source_branch: Option<String>,
    /// Existing rollback target branch required by rollback.
    #[arg(long)]
    target_branch: Option<String>,
    /// Command executed inside the MPLA workspace session by AG-STREAM.
    #[arg(long)]
    stream_command: Option<String>,
    /// Delay between AG-STREAM cursor reads.
    #[arg(long, default_value_t = 25)]
    poll_interval_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum FormalGate {
    #[value(name = "BG-ACTIVATE-EXACT")]
    BgActivateExact,
    #[value(name = "BG-ACTIVATE-SAME")]
    BgActivateSame,
    #[value(name = "BG-FORK")]
    BgFork,
    #[value(name = "BG-ROLLBACK")]
    BgRollback,
    #[value(name = "BG-PUBLISH-SMALL")]
    BgPublishSmall,
    #[value(name = "AG-SQUASH")]
    AgSquash,
    #[value(name = "AG-STREAM")]
    AgStream,
}

impl FormalGate {
    const ALL: [Self; 7] = [
        Self::BgActivateExact,
        Self::BgActivateSame,
        Self::BgFork,
        Self::BgRollback,
        Self::BgPublishSmall,
        Self::AgSquash,
        Self::AgStream,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::BgActivateExact => "BG-ACTIVATE-EXACT",
            Self::BgActivateSame => "BG-ACTIVATE-SAME",
            Self::BgFork => "BG-FORK",
            Self::BgRollback => "BG-ROLLBACK",
            Self::BgPublishSmall => "BG-PUBLISH-SMALL",
            Self::AgSquash => "AG-SQUASH",
            Self::AgStream => "AG-STREAM",
        }
    }

    const fn matched_control_required(self) -> bool {
        matches!(
            self,
            Self::BgActivateExact
                | Self::BgActivateSame
                | Self::BgFork
                | Self::BgRollback
                | Self::BgPublishSmall
        )
    }

    const fn required_catalog_operation(self) -> &'static str {
        match self {
            Self::BgActivateExact | Self::BgActivateSame => "activate_workspace_session",
            Self::BgFork => "fork_workspace_session",
            Self::BgRollback => "rollback_workspace_session",
            Self::BgPublishSmall => "publish_mpla_workspace_session",
            Self::AgSquash => "squash_mpla_branch",
            Self::AgStream => "exec_command",
        }
    }
}

#[derive(Serialize)]
struct RunnerListing {
    schema_version: u32,
    kind: &'static str,
    execution_available: bool,
    commands: [&'static str; 3],
    gates: Vec<GateListing>,
}

#[derive(Serialize)]
struct GateListing {
    case: &'static str,
    matched_control_required: bool,
    required_catalog_operation: &'static str,
}

#[derive(Serialize)]
struct RunnerContract {
    schema_version: u32,
    kind: &'static str,
    action: &'static str,
    execution_status: &'static str,
    runtime_preflight_status: &'static str,
    capsule_sha256: String,
    capsule: ScorecardCapsule,
    execution_blocker: Option<&'static str>,
}

#[derive(Clone, Serialize)]
struct ScorecardCapsule {
    schema_version: u32,
    kind: &'static str,
    run_id: String,
    evidence_root: PathBuf,
    interface_version: String,
    catalog_binding: CatalogBindingIdentity,
    config: FileIdentity,
    image: ImageIdentity,
    r0: R0Identity,
    identities: RunScopedIdentities,
    case: CaseContract,
    samples: u32,
    command_timeout_ms: u64,
    evidence_layout: EvidenceLayout,
    public_cli_adapter: PublicCliAdapterArgs,
}

#[derive(Clone, Serialize)]
struct CatalogBindingIdentity {
    path: PathBuf,
    file_sha256: String,
    binding_id: String,
    build_commit: String,
    exporter_sha256: String,
    catalog_sha256: String,
    facts: ControlCatalogFacts,
    formal_operations: FormalScorecardOperations,
}

#[derive(Clone, Serialize)]
struct FileIdentity {
    path: PathBuf,
    sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct FormalScorecardOperations {
    publish_mpla_workspace_session: bool,
    squash_mpla_branch: bool,
    exec_command: bool,
}

#[derive(Clone, Serialize)]
struct ImageIdentity {
    reference: String,
    platform: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct R0Identity {
    path: PathBuf,
    logical_bytes: u64,
    regular_files: u64,
    directories: u64,
    manifest_kind: &'static str,
    manifest_sha256: String,
}

#[derive(Clone, Serialize)]
struct RunScopedIdentities {
    lease_prefix: String,
    branch_prefix: String,
    sandbox_prefix: String,
}

#[derive(Clone, Serialize)]
struct CaseContract {
    id: &'static str,
    matched_control_required: bool,
    matched_control_declared: bool,
    required_catalog_operation: &'static str,
    execution_implementation: &'static str,
}

#[derive(Clone, Serialize)]
struct EvidenceLayout {
    case_root: PathBuf,
    command_ledger: PathBuf,
}

pub(crate) fn run(command: ScorecardCommand) -> RunnerResult {
    match command {
        ScorecardCommand::List => {
            let listing = RunnerListing {
                schema_version: SCHEMA_VERSION,
                kind: CONTRACT_KIND,
                execution_available: true,
                commands: ["list", "preflight", "gate"],
                gates: FormalGate::ALL
                    .into_iter()
                    .map(|gate| GateListing {
                        case: gate.as_str(),
                        matched_control_required: gate.matched_control_required(),
                        required_catalog_operation: gate.required_catalog_operation(),
                    })
                    .collect(),
            };
            println!("{}", serde_json::to_string_pretty(&listing)?);
        }
        ScorecardCommand::Preflight { capsule } => {
            print_contract("preflight", validate_capsule(capsule)?)?;
        }
        ScorecardCommand::Gate { capsule, execute } => {
            let capsule = validate_capsule(capsule)?;
            if execute {
                execute_gate(capsule)?;
            } else {
                print_contract("gate-plan", capsule)?;
            }
        }
    }
    Ok(())
}

fn print_contract(action: &'static str, capsule: ScorecardCapsule) -> RunnerResult {
    let capsule_sha256 = sha256_bytes(&serde_json::to_vec(&capsule)?);
    let contract = RunnerContract {
        schema_version: SCHEMA_VERSION,
        kind: CONTRACT_KIND,
        action,
        execution_status: "not_executed",
        runtime_preflight_status: "not_run",
        capsule_sha256,
        capsule,
        execution_blocker: Some(
            "execution is opt-in; --execute additionally requires one safe operation sample and complete public-CLI target inputs",
        ),
    };
    println!("{}", serde_json::to_string_pretty(&contract)?);
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
struct PlannedCliInvocation {
    arm: &'static str,
    operation: &'static str,
    request_id: String,
    argv: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CliReceipt {
    schema_version: u32,
    kind: &'static str,
    sequence: u64,
    arm: &'static str,
    operation: String,
    request_id: String,
    started_unix_ms: u128,
    elapsed_ms: u128,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    response: Option<serde_json::Value>,
    invocation_error: Option<String>,
}

#[derive(Serialize)]
struct OperationExecutionResult {
    schema_version: u32,
    kind: &'static str,
    capsule_sha256: String,
    case: &'static str,
    execution_status: &'static str,
    receipt_count: usize,
    error: Option<String>,
}

fn execute_gate(capsule: ScorecardCapsule) -> RunnerResult {
    let plan = plan_public_cli_execution(&capsule)?;
    let capsule_bytes = serde_json::to_vec_pretty(&capsule)?;
    let capsule_sha256 = sha256_bytes(&serde_json::to_vec(&capsule)?);
    create_evidence_layout(&capsule, &capsule_bytes, &plan)?;
    let mut ledger = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&capsule.evidence_layout.command_ledger)?;
    let mut sequence = 0_u64;
    let mut receipt_count = 0_usize;
    let mut execution_error = None;

    for invocation in &plan {
        match execute_invocation(&capsule, invocation, &mut ledger, &mut sequence) {
            Ok(count) => receipt_count += count,
            Err(error) => {
                execution_error = Some(error.to_string());
                break;
            }
        }
    }
    ledger.sync_all()?;

    let result = OperationExecutionResult {
        schema_version: SCHEMA_VERSION,
        kind: EXECUTION_RESULT_KIND,
        capsule_sha256,
        case: capsule.case.id,
        execution_status: if execution_error.is_none() {
            "operation_sample_completed"
        } else {
            "operation_sample_failed"
        },
        receipt_count,
        error: execution_error.clone(),
    };
    write_new_json(
        &capsule
            .evidence_layout
            .case_root
            .join("operation-result.json"),
        &result,
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if let Some(error) = execution_error {
        return Err(format!(
            "public CLI operation sample failed; evidence is retained under {}: {error}",
            capsule.evidence_root.display()
        )
        .into());
    }
    Ok(())
}

fn plan_public_cli_execution(
    capsule: &ScorecardCapsule,
) -> RunnerResult<Vec<PlannedCliInvocation>> {
    if capsule.samples != 1 {
        return Err(format!(
            "--execute currently requires --samples 1; refusing {} samples because this adapter does not allocate fresh per-sample lifecycle state",
            capsule.samples
        )
        .into());
    }
    if capsule.public_cli_adapter.poll_interval_ms == 0 {
        return Err("--poll-interval-ms must be positive".into());
    }
    if capsule
        .public_cli_adapter
        .runtime_cli
        .as_os_str()
        .is_empty()
    {
        return Err("--runtime-cli must not be empty".into());
    }

    let candidate = required_adapter_value(
        capsule.public_cli_adapter.sandbox_id.as_deref(),
        "--sandbox-id",
    )?;
    validate_cli_atom(candidate, "--sandbox-id")?;
    let mut arms = vec![("candidate", candidate, false)];
    if capsule.case.matched_control_required {
        let control = required_adapter_value(
            capsule.public_cli_adapter.control_sandbox_id.as_deref(),
            "--control-sandbox-id",
        )?;
        validate_cli_atom(control, "--control-sandbox-id")?;
        if control == candidate {
            return Err("--control-sandbox-id must differ from --sandbox-id".into());
        }
        arms.push(("control", control, true));
    }

    arms.into_iter()
        .map(|(arm, sandbox_id, control)| build_gate_invocation(capsule, arm, sandbox_id, control))
        .collect()
}

fn build_gate_invocation(
    capsule: &ScorecardCapsule,
    arm: &'static str,
    sandbox_id: &str,
    control: bool,
) -> RunnerResult<PlannedCliInvocation> {
    let request_id = deterministic_request_id(&capsule.run_id, capsule.case.id, arm, 0);
    let mut argv = runtime_cli_prefix(capsule, sandbox_id, &request_id);
    let adapter = &capsule.public_cli_adapter;
    let operation = capsule.case.required_catalog_operation;
    argv.push(operation.to_owned());
    match capsule.case.id {
        "BG-ACTIVATE-EXACT" | "BG-ACTIVATE-SAME" => {
            push_named(&mut argv, "--run-id", &capsule.run_id);
            push_named(
                &mut argv,
                "--branch",
                required_branch(adapter.branch.as_deref(), "--branch")?,
            );
        }
        "BG-FORK" => {
            push_named(&mut argv, "--run-id", &capsule.run_id);
            push_named(
                &mut argv,
                "--source-branch",
                required_branch(adapter.source_branch.as_deref(), "--source-branch")?,
            );
            push_named(
                &mut argv,
                "--branch",
                required_branch(adapter.branch.as_deref(), "--branch")?,
            );
        }
        "BG-ROLLBACK" => {
            push_named(&mut argv, "--run-id", &capsule.run_id);
            push_named(
                &mut argv,
                "--branch",
                required_branch(adapter.branch.as_deref(), "--branch")?,
            );
            push_named(
                &mut argv,
                "--target-branch",
                required_branch(adapter.target_branch.as_deref(), "--target-branch")?,
            );
        }
        "BG-PUBLISH-SMALL" => {
            let workspace_session_id = if control {
                required_adapter_value(
                    adapter.control_workspace_session_id.as_deref(),
                    "--control-workspace-session-id",
                )?
            } else {
                required_adapter_value(
                    adapter.workspace_session_id.as_deref(),
                    "--workspace-session-id",
                )?
            };
            validate_cli_atom(workspace_session_id, "workspace session ID")?;
            push_named(&mut argv, "--workspace-session-id", workspace_session_id);
            push_named(
                &mut argv,
                "--branch",
                required_branch(adapter.branch.as_deref(), "--branch")?,
            );
        }
        "AG-SQUASH" => {
            push_named(&mut argv, "--run-id", &capsule.run_id);
            push_named(
                &mut argv,
                "--branch",
                required_branch(adapter.branch.as_deref(), "--branch")?,
            );
        }
        "AG-STREAM" => {
            let workspace_session_id = required_adapter_value(
                adapter.workspace_session_id.as_deref(),
                "--workspace-session-id",
            )?;
            validate_cli_atom(workspace_session_id, "workspace session ID")?;
            let command =
                required_adapter_value(adapter.stream_command.as_deref(), "--stream-command")?;
            if command.trim().is_empty() {
                return Err("--stream-command must not be blank".into());
            }
            push_named(&mut argv, "--workspace-session-id", workspace_session_id);
            push_named(
                &mut argv,
                "--timeout-ms",
                &capsule.command_timeout_ms.to_string(),
            );
            push_named(&mut argv, "--yield-time-ms", "0");
            argv.push(command.to_owned());
        }
        other => return Err(format!("unsupported formal gate {other:?}").into()),
    }
    Ok(PlannedCliInvocation {
        arm,
        operation,
        request_id,
        argv,
    })
}

fn execute_invocation(
    capsule: &ScorecardCapsule,
    invocation: &PlannedCliInvocation,
    ledger: &mut File,
    sequence: &mut u64,
) -> RunnerResult<usize> {
    let mut receipt_count = 0_usize;
    let first = invoke_public_cli(capsule, invocation, *sequence);
    *sequence = sequence.checked_add(1).ok_or("receipt sequence overflow")?;
    append_receipt(ledger, &first)?;
    receipt_count += 1;
    require_successful_receipt(&first)?;
    if invocation.operation != "exec_command" {
        return Ok(receipt_count);
    }

    let mut response = first
        .response
        .clone()
        .ok_or("AG-STREAM exec_command did not return a JSON object")?;
    let command_session_id = response
        .get("command_session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let mut cursor = response_u64(&response, "end_offset")?;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(
            capsule.command_timeout_ms.saturating_add(5_000),
        ))
        .ok_or("AG-STREAM deadline overflow")?;
    let mut poll = 0_u64;

    loop {
        let status = response
            .get("status")
            .and_then(serde_json::Value::as_str)
            .ok_or("AG-STREAM response lacks string status")?;
        let total_lines = response_u64(&response, "total_lines")?;
        if status != "running" && cursor >= total_lines {
            if status != "ok"
                || response
                    .get("exit_code")
                    .and_then(serde_json::Value::as_i64)
                    != Some(0)
            {
                return Err(format!(
                    "AG-STREAM command completed with status {status:?} and exit code {:?}",
                    response.get("exit_code")
                )
                .into());
            }
            return Ok(receipt_count);
        }
        if Instant::now() >= deadline {
            return Err(
                "AG-STREAM cursor drain exceeded the command timeout plus 5 seconds".into(),
            );
        }
        let command_session_id = command_session_id
            .as_deref()
            .ok_or("AG-STREAM running or undrained response lacks command_session_id")?;
        thread::sleep(Duration::from_millis(
            capsule.public_cli_adapter.poll_interval_ms,
        ));
        poll = poll
            .checked_add(1)
            .ok_or("AG-STREAM poll counter overflow")?;
        let read_invocation =
            build_read_invocation(capsule, invocation.arm, command_session_id, cursor, poll);
        let read = invoke_public_cli(capsule, &read_invocation, *sequence);
        *sequence = sequence.checked_add(1).ok_or("receipt sequence overflow")?;
        append_receipt(ledger, &read)?;
        receipt_count += 1;
        require_successful_receipt(&read)?;
        response = read
            .response
            .clone()
            .ok_or("AG-STREAM read_command_lines did not return JSON")?;
        cursor = response_u64(&response, "end_offset")?;
    }
}

fn build_read_invocation(
    capsule: &ScorecardCapsule,
    arm: &'static str,
    command_session_id: &str,
    cursor: u64,
    poll: u64,
) -> PlannedCliInvocation {
    let request_id = deterministic_request_id(&capsule.run_id, capsule.case.id, arm, poll as usize);
    let sandbox_id = capsule
        .public_cli_adapter
        .sandbox_id
        .as_deref()
        .expect("execution plan validated candidate sandbox");
    let mut argv = runtime_cli_prefix(capsule, sandbox_id, &request_id);
    argv.push("read_command_lines".to_owned());
    push_named(&mut argv, "--command-session-id", command_session_id);
    push_named(&mut argv, "--start-offset", &cursor.to_string());
    push_named(&mut argv, "--limit", "200");
    PlannedCliInvocation {
        arm,
        operation: "read_command_lines",
        request_id,
        argv,
    }
}

fn invoke_public_cli(
    capsule: &ScorecardCapsule,
    invocation: &PlannedCliInvocation,
    sequence: u64,
) -> CliReceipt {
    let started_unix_ms = unix_ms();
    let started = Instant::now();
    let output = Command::new(&capsule.public_cli_adapter.runtime_cli)
        .args(&invocation.argv)
        .output();
    let elapsed_ms = started.elapsed().as_millis();
    match output {
        Ok(output) => {
            receipt_from_output(invocation, sequence, started_unix_ms, elapsed_ms, output)
        }
        Err(error) => CliReceipt {
            schema_version: SCHEMA_VERSION,
            kind: COMMAND_LEDGER_KIND,
            sequence,
            arm: invocation.arm,
            operation: invocation.operation.to_owned(),
            request_id: invocation.request_id.clone(),
            started_unix_ms,
            elapsed_ms,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            response: None,
            invocation_error: Some(error.to_string()),
        },
    }
}

fn receipt_from_output(
    invocation: &PlannedCliInvocation,
    sequence: u64,
    started_unix_ms: u128,
    elapsed_ms: u128,
    output: Output,
) -> CliReceipt {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let response = serde_json::from_str(stdout.trim()).ok();
    CliReceipt {
        schema_version: SCHEMA_VERSION,
        kind: COMMAND_LEDGER_KIND,
        sequence,
        arm: invocation.arm,
        operation: invocation.operation.to_owned(),
        request_id: invocation.request_id.clone(),
        started_unix_ms,
        elapsed_ms,
        exit_code: output.status.code(),
        stdout,
        stderr,
        response,
        invocation_error: None,
    }
}

fn require_successful_receipt(receipt: &CliReceipt) -> RunnerResult {
    if let Some(error) = &receipt.invocation_error {
        return Err(format!("failed to start public runtime CLI: {error}").into());
    }
    if receipt.exit_code != Some(0) {
        return Err(format!(
            "{} exited {:?}: {}",
            receipt.operation,
            receipt.exit_code,
            receipt.stderr.trim()
        )
        .into());
    }
    if receipt.response.is_none() {
        return Err(format!("{} did not emit one JSON response", receipt.operation).into());
    }
    Ok(())
}

fn create_evidence_layout(
    capsule: &ScorecardCapsule,
    capsule_bytes: &[u8],
    plan: &[PlannedCliInvocation],
) -> RunnerResult {
    fs::create_dir(&capsule.evidence_root)?;
    fs::create_dir(capsule.evidence_root.join("cases"))?;
    fs::create_dir(&capsule.evidence_layout.case_root)?;
    write_new_bytes(&capsule.evidence_root.join("capsule.json"), capsule_bytes)?;
    write_new_json(
        &capsule
            .evidence_layout
            .case_root
            .join("public-cli-plan.json"),
        plan,
    )?;
    Ok(())
}

fn append_receipt(ledger: &mut File, receipt: &CliReceipt) -> RunnerResult {
    serde_json::to_writer(&mut *ledger, receipt)?;
    ledger.write_all(b"\n")?;
    ledger.flush()?;
    Ok(())
}

fn write_new_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> RunnerResult {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_new_bytes(path, &bytes)
}

fn write_new_bytes(path: &Path, bytes: &[u8]) -> RunnerResult {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn runtime_cli_prefix(
    capsule: &ScorecardCapsule,
    sandbox_id: &str,
    request_id: &str,
) -> Vec<String> {
    let mut argv = Vec::new();
    if let Some(socket) = &capsule.public_cli_adapter.gateway_socket {
        push_named(&mut argv, "--gateway-socket", &socket.to_string_lossy());
    }
    push_named(&mut argv, "--sandbox-id", sandbox_id);
    push_named(&mut argv, "--request-id", request_id);
    argv
}

fn push_named(argv: &mut Vec<String>, flag: &str, value: &str) {
    argv.push(flag.to_owned());
    argv.push(value.to_owned());
}

fn required_adapter_value<'a>(value: Option<&'a str>, flag: &str) -> RunnerResult<&'a str> {
    value.ok_or_else(|| format!("{flag} is required with --execute").into())
}

fn required_branch<'a>(value: Option<&'a str>, flag: &str) -> RunnerResult<&'a str> {
    let value = required_adapter_value(value, flag)?;
    validate_cli_atom(value, flag)?;
    Ok(value)
}

fn validate_cli_atom(value: &str, label: &str) -> RunnerResult {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
    {
        return Err(format!(
            "{label} must be 1-128 ASCII letters, digits, period, underscore, colon, or dash"
        )
        .into());
    }
    Ok(())
}

fn deterministic_request_id(run_id: &str, gate: &str, arm: &str, sequence: usize) -> String {
    let digest = sha256_bytes(run_id.as_bytes());
    format!(
        "scorecard:{}:{}:{}:{sequence:04}",
        &digest[..16],
        gate.replace('-', "_"),
        arm
    )
}

fn response_u64(value: &serde_json::Value, field: &str) -> RunnerResult<u64> {
    value
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("AG-STREAM response lacks unsigned {field}").into())
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn validate_capsule(args: ScorecardCapsuleArgs) -> RunnerResult<ScorecardCapsule> {
    let run_id = validate_run_id(&args.run_id)?;
    let evidence_root = validate_fresh_evidence_root(&args.evidence_root, &run_id)?;
    validate_interface(&args.interface_version)?;
    validate_image(&args.image)?;
    validate_positive(args.samples, "--samples")?;
    validate_positive(args.command_timeout_ms, "--command-timeout-ms")?;
    validate_prefixes(
        &run_id,
        &args.lease_prefix,
        &args.branch_prefix,
        &args.sandbox_prefix,
    )?;
    if args.case.matched_control_required() && !args.matched_control {
        return Err(format!(
            "{} requires --matched-control and independently allocated candidate/control arms",
            args.case.as_str()
        )
        .into());
    }

    let catalog_binding = validate_catalog_binding(&args.catalog_binding, args.case)?;
    let config = validate_poc_config(&args.config)?;
    let r0 = validate_r0(&args.r0, args.r0_manifest_sha256.as_deref())?;
    let case_root = evidence_root.join("cases").join(args.case.as_str());
    let command_ledger = evidence_root.join("command-ledger.jsonl");

    Ok(ScorecardCapsule {
        schema_version: SCHEMA_VERSION,
        kind: CAPSULE_KIND,
        run_id: run_id.as_str().to_owned(),
        evidence_root,
        interface_version: args.interface_version,
        catalog_binding,
        config,
        image: ImageIdentity {
            reference: args.image,
            platform: PINNED_PLATFORM,
        },
        r0,
        identities: RunScopedIdentities {
            lease_prefix: args.lease_prefix,
            branch_prefix: args.branch_prefix,
            sandbox_prefix: args.sandbox_prefix,
        },
        case: CaseContract {
            id: args.case.as_str(),
            matched_control_required: args.case.matched_control_required(),
            matched_control_declared: args.matched_control,
            required_catalog_operation: args.case.required_catalog_operation(),
            execution_implementation: "public_runtime_cli_single_sample_v1",
        },
        samples: args.samples,
        command_timeout_ms: args.command_timeout_ms,
        evidence_layout: EvidenceLayout {
            case_root,
            command_ledger,
        },
        public_cli_adapter: args.adapter,
    })
}

fn validate_run_id(value: &str) -> RunnerResult<RunId> {
    if value == HISTORICAL_RUN_ID {
        return Err(
            format!("historical scorecard run ID {HISTORICAL_RUN_ID:?} is forbidden").into(),
        );
    }
    if !value.starts_with("booster-scorecard-") {
        return Err("scorecard run ID must start with \"booster-scorecard-\"".into());
    }
    Ok(RunId::parse(value)?)
}

fn validate_fresh_evidence_root(path: &Path, run_id: &RunId) -> RunnerResult<PathBuf> {
    if !path.is_absolute() {
        return Err("evidence root must be absolute".into());
    }
    if fs::symlink_metadata(path).is_ok() {
        return Err(format!("refusing pre-existing evidence root {}", path.display()).into());
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("evidence root must have a UTF-8 final component")?;
    if file_name != run_id.as_str() {
        return Err("evidence root final component must equal --run-id".into());
    }
    let parent = path.parent().ok_or("evidence root must have a parent")?;
    let canonical_parent = fs::canonicalize(parent)?;
    let canonical = canonical_parent.join(file_name);
    if canonical != path {
        return Err("evidence root must use its canonical absolute parent path".into());
    }
    Ok(canonical)
}

fn validate_interface(value: &str) -> RunnerResult {
    if value != INTERFACE_VERSION {
        return Err(format!(
            "interface version {value:?} does not match the built interface {INTERFACE_VERSION:?}"
        )
        .into());
    }
    Ok(())
}

fn validate_image(value: &str) -> RunnerResult {
    let Some((name, digest)) = value.split_once("@sha256:") else {
        return Err("image must be pinned as <name>@sha256:<64 lowercase hex characters>".into());
    };
    if name.is_empty()
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("image must be pinned as <name>@sha256:<64 lowercase hex characters>".into());
    }
    if value != PINNED_IMAGE {
        return Err(format!("Stage 04.6 requires the exact pinned image {PINNED_IMAGE}").into());
    }
    Ok(())
}

fn validate_positive<T>(value: T, name: &str) -> RunnerResult
where
    T: Copy + Default + PartialEq,
{
    if value == T::default() {
        return Err(format!("{name} must be positive").into());
    }
    Ok(())
}

fn validate_prefixes(
    run_id: &RunId,
    lease_prefix: &str,
    branch_prefix: &str,
    sandbox_prefix: &str,
) -> RunnerResult {
    for (label, value) in [
        ("lease", lease_prefix),
        ("branch", branch_prefix),
        ("sandbox", sandbox_prefix),
    ] {
        validate_prefix(value, label)?;
        if !value.starts_with(run_id.as_str()) {
            return Err(format!("{label} prefix must start with --run-id").into());
        }
    }
    if lease_prefix == branch_prefix
        || lease_prefix == sandbox_prefix
        || branch_prefix == sandbox_prefix
    {
        return Err("lease, branch, and sandbox prefixes must be distinct".into());
    }
    Ok(())
}

fn validate_prefix(value: &str, label: &str) -> RunnerResult {
    let valid = (1..=96).contains(&value.len())
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'));
    if !valid {
        return Err(format!("{label} prefix contains unsupported characters").into());
    }
    if matches!(
        value,
        "m2r-20260728T015724p0800:lead:" | "m2r-lead" | "m2-lead" | "heavy-main"
    ) {
        return Err(format!("historical {label} prefix {value:?} is forbidden").into());
    }
    Ok(())
}

fn validate_catalog_binding(path: &Path, gate: FormalGate) -> RunnerResult<CatalogBindingIdentity> {
    let identity = validate_file_identity(path, "catalog binding")?;
    let bytes = fs::read(&identity.path)?;
    let declared: CatalogBinding = serde_json::from_slice(&bytes)?;
    if declared.schema_version != SCHEMA_VERSION
        || declared.kind != "mpla-product-catalog-binding-v1"
    {
        return Err("catalog binding has an unsupported schema or kind".into());
    }
    let rebound = bind_product_catalog(
        &declared.exporter_path,
        &declared.catalog_path,
        &declared.build_commit,
    )?;
    if declared.build_commit != rebound.build_commit
        || declared.exporter_path != rebound.exporter_path
        || declared.exporter_sha256 != rebound.exporter_sha256
        || declared.catalog_path != rebound.catalog_path
        || declared.catalog_sha256 != rebound.catalog_sha256
        || declared.binding_id != rebound.binding_id
        || declared.facts != rebound.facts
    {
        return Err("catalog binding does not match the current exporter/catalog inputs".into());
    }
    let formal_operations = read_formal_catalog_operations(&declared.catalog_path)?;
    require_catalog_operation(&declared.facts, &formal_operations, gate)?;
    Ok(CatalogBindingIdentity {
        path: identity.path,
        file_sha256: identity.sha256,
        binding_id: declared.binding_id,
        build_commit: declared.build_commit,
        exporter_sha256: declared.exporter_sha256,
        catalog_sha256: declared.catalog_sha256,
        facts: declared.facts,
        formal_operations,
    })
}

fn read_formal_catalog_operations(path: &Path) -> RunnerResult<FormalScorecardOperations> {
    let catalog: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(FormalScorecardOperations {
        publish_mpla_workspace_session: catalog_operation_present(
            &catalog,
            "runtime",
            "publish_mpla_workspace_session",
        )?,
        squash_mpla_branch: catalog_operation_present(&catalog, "runtime", "squash_mpla_branch")?,
        exec_command: catalog_operation_present(&catalog, "runtime", "exec_command")?,
    })
}

fn catalog_operation_present(
    catalog: &serde_json::Value,
    domain: &str,
    operation: &str,
) -> RunnerResult<bool> {
    let operations = catalog
        .get("domains")
        .and_then(|domains| domains.get(domain))
        .and_then(|domain| domain.get("operations"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("product catalog lacks {domain} operations"))?;
    for candidate in operations {
        let name = candidate
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("product catalog {domain} operation lacks a name"))?;
        if name == operation {
            return Ok(true);
        }
    }
    Ok(false)
}

fn require_catalog_operation(
    facts: &ControlCatalogFacts,
    formal_operations: &FormalScorecardOperations,
    gate: FormalGate,
) -> RunnerResult {
    let present = match gate {
        FormalGate::BgActivateExact | FormalGate::BgActivateSame => {
            facts.activate_workspace_session
        }
        FormalGate::BgFork => facts.fork_workspace_session,
        FormalGate::BgRollback => facts.rollback_workspace_session,
        FormalGate::BgPublishSmall | FormalGate::AgStream => {
            if gate == FormalGate::BgPublishSmall {
                formal_operations.publish_mpla_workspace_session
            } else {
                formal_operations.exec_command
            }
        }
        FormalGate::AgSquash => formal_operations.squash_mpla_branch,
    };
    if !present {
        return Err(format!(
            "catalog binding does not expose required operation {:?}",
            gate.required_catalog_operation()
        )
        .into());
    }
    Ok(())
}

fn validate_poc_config(path: &Path) -> RunnerResult<FileIdentity> {
    let identity = validate_file_identity(path, "PoC config")?;
    let config: PocConfig = serde_json::from_slice(&fs::read(&identity.path)?)
        .map_err(|error| format!("PoC config must be canonical JSON: {error}"))?;
    config.validate()?;
    Ok(identity)
}

fn validate_file_identity(path: &Path, label: &str) -> RunnerResult<FileIdentity> {
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical)?;
    if !metadata.is_file() {
        return Err(format!("{label} must be a regular file").into());
    }
    if metadata.len() > MAX_INPUT_FILE_BYTES {
        return Err(format!("{label} exceeds the {MAX_INPUT_FILE_BYTES}-byte limit").into());
    }
    Ok(FileIdentity {
        sha256: sha256_file(&canonical)?,
        path: canonical,
    })
}

fn validate_r0(path: &Path, expected_manifest: Option<&str>) -> RunnerResult<R0Identity> {
    let required = Path::new(EXACT_R0_PATH);
    if path != required {
        return Err(format!("R0 must be the exact source path {}", required.display()).into());
    }
    let canonical = fs::canonicalize(path)?;
    if canonical != required || !fs::metadata(&canonical)?.is_dir() {
        return Err(format!(
            "R0 must resolve to the exact source path {}",
            required.display()
        )
        .into());
    }
    let profile = profile_r0(&canonical)?;
    if profile.regular_files != EXACT_R0_REGULAR_FILES
        || profile.logical_bytes != EXACT_R0_LOGICAL_BYTES
    {
        return Err(format!(
            "R0 profile mismatch: expected {} regular files and {} logical bytes, observed {} and {}",
            EXACT_R0_REGULAR_FILES,
            EXACT_R0_LOGICAL_BYTES,
            profile.regular_files,
            profile.logical_bytes
        )
        .into());
    }
    if let Some(expected) = expected_manifest {
        validate_sha256(expected, "--r0-manifest-sha256")?;
        if expected != profile.manifest_sha256 {
            return Err(format!(
                "R0 manifest mismatch: expected {expected}, observed {}",
                profile.manifest_sha256
            )
            .into());
        }
    }
    Ok(profile)
}

fn profile_r0(root: &Path) -> RunnerResult<R0Identity> {
    let mut hasher = Sha256::new();
    hasher.update(R0_MANIFEST_DOMAIN);
    let mut profile = R0Identity {
        path: root.to_path_buf(),
        logical_bytes: 0,
        regular_files: 0,
        directories: 1,
        manifest_kind: "mpla-booster-scorecard-r0-manifest-v1",
        manifest_sha256: String::new(),
    };
    visit_r0(root, root, &mut profile, &mut hasher)?;
    profile.manifest_sha256 = format!("{:x}", hasher.finalize());
    Ok(profile)
}

fn visit_r0(
    root: &Path,
    directory: &Path,
    profile: &mut R0Identity,
    manifest: &mut Sha256,
) -> RunnerResult {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .ok_or("R0 contains a non-UTF-8 path")?
            .replace('\\', "/");
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            profile.directories = profile
                .directories
                .checked_add(1)
                .ok_or("R0 directory count overflow")?;
            update_manifest_field(manifest, b'd', &relative, 0, None);
            visit_r0(root, &path, profile, manifest)?;
        } else if file_type.is_file() {
            let size = entry.metadata()?.len();
            profile.regular_files = profile
                .regular_files
                .checked_add(1)
                .ok_or("R0 file count overflow")?;
            profile.logical_bytes = profile
                .logical_bytes
                .checked_add(size)
                .ok_or("R0 logical byte count overflow")?;
            let content_sha256 = sha256_file_bytes(&path)?;
            update_manifest_field(manifest, b'f', &relative, size, Some(&content_sha256));
        } else {
            return Err(format!("R0 contains unsupported entry {}", path.display()).into());
        }
    }
    Ok(())
}

fn update_manifest_field(
    manifest: &mut Sha256,
    kind: u8,
    relative: &str,
    size: u64,
    content_sha256: Option<&[u8; 32]>,
) {
    manifest.update([kind]);
    manifest.update((relative.len() as u64).to_be_bytes());
    manifest.update(relative.as_bytes());
    manifest.update(size.to_be_bytes());
    if let Some(content_sha256) = content_sha256 {
        manifest.update(content_sha256);
    }
}

fn sha256_file(path: &Path) -> RunnerResult<String> {
    Ok(format!("{:x}", digest_file(path)?))
}

fn sha256_file_bytes(path: &Path) -> RunnerResult<[u8; 32]> {
    Ok(digest_file(path)?.into())
}

fn digest_file(path: &Path) -> RunnerResult<sha2::digest::Output<Sha256>> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_sha256(value: &str, label: &str) -> RunnerResult {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{label} must be 64 lowercase hexadecimal characters").into());
    }
    Ok(())
}
