use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use sandbox_runtime_mpla_poc::{
    bind_product_catalog, durable, populate_empty_fixture_root, prepare_fixture, qualify, report,
    AllocationId, FixtureId, FixtureTier, PocConfig, QualificationRequest, RunId, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(name = "mpla-poc")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Config,
    CatalogBind {
        #[arg(long)]
        exporter: PathBuf,
        #[arg(long)]
        catalog: PathBuf,
        #[arg(long)]
        build_commit: String,
        #[arg(long, default_value_t = false)]
        refresh_catalog: bool,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    EvidenceSeal {
        #[arg(long)]
        evidence_root: PathBuf,
    },
    EvidenceVerify {
        #[arg(long)]
        evidence_root: PathBuf,
    },
    FixturePrepare {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        fixture: String,
        #[arg(long)]
        tier: String,
    },
    FixturePopulate {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        fixture: String,
        #[arg(long)]
        tier: String,
    },
    Qualification {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        allocation_id: String,
        #[arg(long)]
        payload_root: PathBuf,
        #[arg(long)]
        control_root: PathBuf,
        #[arg(long)]
        fixtures_root: PathBuf,
        #[arg(long)]
        evidence_root: PathBuf,
        #[arg(long)]
        lower_dir: PathBuf,
        #[arg(long)]
        allocation_root: PathBuf,
        #[arg(long)]
        workspace_root: PathBuf,
    },
    LifecycleMetadata {
        #[arg(long)]
        state_root: PathBuf,
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        action: LifecycleAction,
        #[arg(long)]
        branch: String,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        allocation_id: Option<String>,
        #[arg(long)]
        root_id: Option<String>,
        #[arg(long)]
        attribution_root_id: Option<String>,
        #[arg(long, default_value_t = false)]
        cancel: bool,
    },
    Suite {
        suite: String,
    },
    Test {
        case: String,
        #[arg(long, default_value_t = 1)]
        samples: u32,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LifecycleAction {
    Initialize,
    Fork,
    Rollback,
    Squash,
}

impl LifecycleAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::Fork => "fork",
            Self::Rollback => "rollback",
            Self::Squash => "squash",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LifecycleSelection {
    schema_version: u32,
    branch: String,
    sequence: u64,
    allocation_id: String,
    root_id: String,
    attribution_root_id: String,
    ancestry: Vec<u64>,
    selected_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LifecycleMetadataReceipt {
    schema_version: u32,
    kind: String,
    operation_id: String,
    action: String,
    branch: String,
    status: String,
    started_unix_ms: u64,
    finished_unix_ms: u64,
    service_elapsed_ns: u64,
    selection: Option<LifecycleSelection>,
    error: Option<String>,
    payload_objects_created: u64,
    selector_path: Option<PathBuf>,
    outcome_path: PathBuf,
    parent_directories_synced: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config = PocConfig::default();
    config.validate()?;
    match cli.command {
        Command::Config => {
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        Command::CatalogBind {
            exporter,
            catalog,
            build_commit,
            refresh_catalog,
            output,
        } => {
            if refresh_catalog {
                refresh_product_catalog(&exporter, &catalog)?;
            }
            let binding = bind_product_catalog(&exporter, &catalog, &build_commit)?;
            if let Some(output) = output {
                durable::replace_json(&output, &binding)?;
            }
            println!("{}", serde_json::to_string_pretty(&binding)?);
        }
        Command::EvidenceSeal { evidence_root } => {
            let receipt = report::seal_manifest(&evidence_root)?;
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        }
        Command::EvidenceVerify { evidence_root } => {
            let receipt = report::verify_manifest(&evidence_root)?;
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        }
        Command::FixturePrepare {
            root,
            fixture,
            tier,
        } => {
            let receipt = prepare_fixture(
                &root,
                FixtureId::parse(&fixture)?,
                FixtureTier::parse(&tier)?,
            )?;
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        }
        Command::FixturePopulate {
            root,
            fixture,
            tier,
        } => {
            let receipt = populate_empty_fixture_root(
                &root,
                FixtureId::parse(&fixture)?,
                FixtureTier::parse(&tier)?,
            )?;
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        }
        Command::Qualification {
            run_id,
            allocation_id,
            payload_root,
            control_root,
            fixtures_root,
            evidence_root,
            lower_dir,
            allocation_root,
            workspace_root,
        } => {
            let request = QualificationRequest {
                schema_version: SCHEMA_VERSION,
                run_id: RunId::parse(run_id)?,
                allocation_id: AllocationId::from_string(allocation_id),
                payload_root,
                control_root,
                fixtures_root,
                evidence_root,
                lower_dir,
                allocation_root,
                workspace_root,
            };
            let receipt = qualify::qualify(&config, &request)?;
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        }
        Command::LifecycleMetadata {
            state_root,
            operation_id,
            action,
            branch,
            source,
            target,
            allocation_id,
            root_id,
            attribution_root_id,
            cancel,
        } => {
            let request = LifecycleMetadataRequest {
                state_root,
                operation_id,
                action,
                branch,
                source,
                target,
                allocation_id,
                root_id,
                attribution_root_id,
                cancel,
            };
            let receipt = run_lifecycle_metadata(&request)?;
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        }
        Command::Suite { suite } => {
            if suite != "smoke" {
                return Err(format!("unsupported suite {suite:?}; expected \"smoke\"").into());
            }
            dispatch_campaign("suite", None, 1)?;
        }
        Command::Test { case, samples } => {
            validate_case(&case)?;
            if samples == 0 {
                return Err("--samples must be positive".into());
            }
            dispatch_campaign("test", Some(&case), samples)?;
        }
    }
    Ok(())
}

struct LifecycleMetadataRequest {
    state_root: PathBuf,
    operation_id: String,
    action: LifecycleAction,
    branch: String,
    source: Option<String>,
    target: Option<String>,
    allocation_id: Option<String>,
    root_id: Option<String>,
    attribution_root_id: Option<String>,
    cancel: bool,
}

fn run_lifecycle_metadata(
    request: &LifecycleMetadataRequest,
) -> Result<LifecycleMetadataReceipt, Box<dyn std::error::Error>> {
    validate_component(&request.operation_id, "operation ID")?;
    validate_component(&request.branch, "branch")?;
    if let Some(source) = &request.source {
        validate_component(source, "source branch")?;
    }
    if let Some(target) = &request.target {
        validate_component(target, "target branch")?;
    }
    let outcomes = request.state_root.join("outcomes");
    let outcome_path = outcomes.join(format!("{}.json", request.operation_id));
    if outcome_path.exists() {
        return Ok(durable::read_json(&outcome_path)?);
    }

    let started_unix_ms = sandbox_runtime_mpla_poc::unix_time_ms()?;
    let started = Instant::now();
    let selector_path = request
        .state_root
        .join("branches")
        .join(format!("{}.json", request.branch));
    let (status, selection, error) = if request.cancel {
        (
            "cancelled".to_owned(),
            None,
            Some("operation cancelled before durable selector mutation".to_owned()),
        )
    } else {
        match apply_lifecycle_metadata(request, &selector_path) {
            Ok(selection) => ("succeeded".to_owned(), Some(selection), None),
            Err(error) => ("failed".to_owned(), None, Some(error.to_string())),
        }
    };
    let selector_written = selection.is_some();
    let receipt = LifecycleMetadataReceipt {
        schema_version: SCHEMA_VERSION,
        kind: "mpla-lifecycle-metadata-outcome-v1".to_owned(),
        operation_id: request.operation_id.clone(),
        action: request.action.as_str().to_owned(),
        branch: request.branch.clone(),
        status,
        started_unix_ms,
        finished_unix_ms: sandbox_runtime_mpla_poc::unix_time_ms()?,
        service_elapsed_ns: u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        selection,
        error,
        payload_objects_created: 0,
        selector_path: selector_written.then_some(selector_path),
        outcome_path: outcome_path.clone(),
        parent_directories_synced: true,
    };
    durable::replace_json(&outcome_path, &receipt)?;
    Ok(receipt)
}

fn refresh_product_catalog(
    exporter: &std::path::Path,
    catalog: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = ProcessCommand::new(exporter).output()?;
    if !output.status.success() {
        return Err(format!(
            "product catalog exporter failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    durable::replace_json(catalog, &value)?;
    Ok(())
}

fn apply_lifecycle_metadata(
    request: &LifecycleMetadataRequest,
    selector_path: &std::path::Path,
) -> Result<LifecycleSelection, Box<dyn std::error::Error>> {
    let selected_unix_ms = sandbox_runtime_mpla_poc::unix_time_ms()?;
    let selection = match request.action {
        LifecycleAction::Initialize => LifecycleSelection {
            schema_version: SCHEMA_VERSION,
            branch: request.branch.clone(),
            sequence: 1,
            allocation_id: required_option(&request.allocation_id, "--allocation-id")?.clone(),
            root_id: required_option(&request.root_id, "--root-id")?.clone(),
            attribution_root_id: required_option(
                &request.attribution_root_id,
                "--attribution-root-id",
            )?
            .clone(),
            ancestry: vec![1],
            selected_unix_ms,
        },
        LifecycleAction::Fork => {
            let source = required_option(&request.source, "--source")?;
            let mut selection = read_lifecycle_selection(&request.state_root, source)?;
            selection.branch.clone_from(&request.branch);
            selection.selected_unix_ms = selected_unix_ms;
            selection
        }
        LifecycleAction::Rollback => {
            let target = required_option(&request.target, "--target")?;
            let target_selection = read_lifecycle_selection(&request.state_root, target)?;
            let current = read_lifecycle_selection(&request.state_root, &request.branch)?;
            let sequence = current
                .sequence
                .checked_add(1)
                .ok_or("lifecycle branch sequence overflow")?;
            let mut ancestry = current.ancestry;
            ancestry.push(sequence);
            LifecycleSelection {
                schema_version: SCHEMA_VERSION,
                branch: request.branch.clone(),
                sequence,
                allocation_id: target_selection.allocation_id,
                root_id: target_selection.root_id,
                attribution_root_id: target_selection.attribution_root_id,
                ancestry,
                selected_unix_ms,
            }
        }
        LifecycleAction::Squash => {
            let current = read_lifecycle_selection(&request.state_root, &request.branch)?;
            let sequence = current
                .sequence
                .checked_add(1)
                .ok_or("lifecycle branch sequence overflow")?;
            LifecycleSelection {
                schema_version: SCHEMA_VERSION,
                branch: request.branch.clone(),
                sequence,
                allocation_id: current.allocation_id,
                root_id: current.root_id,
                attribution_root_id: current.attribution_root_id,
                ancestry: vec![sequence],
                selected_unix_ms,
            }
        }
    };
    durable::replace_json(selector_path, &selection)?;
    Ok(selection)
}

fn read_lifecycle_selection(
    state_root: &std::path::Path,
    branch: &str,
) -> Result<LifecycleSelection, Box<dyn std::error::Error>> {
    Ok(durable::read_json(
        &state_root.join("branches").join(format!("{branch}.json")),
    )?)
}

fn required_option<'a>(
    value: &'a Option<String>,
    name: &str,
) -> Result<&'a String, Box<dyn std::error::Error>> {
    value
        .as_ref()
        .ok_or_else(|| format!("{name} is required").into())
}

fn validate_component(value: &str, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("{label} contains unsupported characters").into());
    }
    Ok(())
}

fn validate_case(case: &str) -> Result<(), Box<dyn std::error::Error>> {
    let Some(number) = case.strip_prefix("SM-") else {
        return Err(format!("invalid smoke case {case:?}").into());
    };
    let parsed: u8 = number.parse()?;
    if !(1..=14).contains(&parsed) || format!("SM-{parsed:02}") != case {
        return Err(format!("invalid smoke case {case:?}").into());
    }
    Ok(())
}

fn dispatch_campaign(
    mode: &str,
    case: Option<&str>,
    samples: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_binary = std::env::var_os("MPLA_POC_CAMPAIGN_TEST_BIN")
        .ok_or("MPLA_POC_CAMPAIGN_TEST_BIN is required for suite/test dispatch")?;
    let mut command = ProcessCommand::new(test_binary);
    command
        .args(["--ignored", "--exact", "m1_smoke_campaign", "--nocapture"])
        .env("MPLA_POC_CAMPAIGN_MODE", mode)
        .env("MPLA_POC_SAMPLES", samples.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(case) = case {
        command.env("MPLA_POC_CASE_FILTER", case);
    } else {
        command.env_remove("MPLA_POC_CASE_FILTER");
    }

    let mut child = command.spawn()?;
    let started = Instant::now();
    let hard_stop = Duration::from_secs(179);
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(format!("physical campaign test exited with {status}").into());
        }
        if started.elapsed() >= hard_stop {
            child.kill()?;
            let _ = child.wait();
            return Err("physical campaign exceeded the 179-second hard stop".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
