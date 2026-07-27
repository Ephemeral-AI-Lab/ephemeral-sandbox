use std::path::PathBuf;

use clap::{Parser, Subcommand};
use sandbox_runtime_mpla_poc::{
    qualify, AllocationId, PocConfig, QualificationRequest, RunId, SCHEMA_VERSION,
};

#[derive(Debug, Parser)]
#[command(name = "mpla-poc")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[expect(clippy::large_enum_variant, reason = "clap subcommand arguments")]
enum Command {
    Config,
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
