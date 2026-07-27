use std::path::PathBuf;

use clap::{Parser, Subcommand};
use sandbox_runtime_mpla_poc::{
    bind_product_catalog, prepare_fixture, qualify, report, AllocationId, FixtureId, FixtureTier,
    PocConfig, QualificationRequest, RunId, SCHEMA_VERSION,
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
    }
    Ok(())
}
