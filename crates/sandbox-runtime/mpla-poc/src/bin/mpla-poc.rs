use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use sandbox_runtime_mpla_poc::{
    bind_product_catalog, populate_empty_fixture_root, prepare_fixture, qualify, report,
    AllocationId, FixtureId, FixtureTier, PocConfig, QualificationRequest, RunId, SCHEMA_VERSION,
};

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
    Suite {
        suite: String,
    },
    Test {
        case: String,
        #[arg(long, default_value_t = 1)]
        samples: u32,
    },
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
        } => {
            let binding = bind_product_catalog(&exporter, &catalog, &build_commit)?;
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
