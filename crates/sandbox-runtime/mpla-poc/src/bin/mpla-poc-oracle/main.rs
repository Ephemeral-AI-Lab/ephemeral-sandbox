mod oracle_record;
mod oracle_scan;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mpla-poc-oracle: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let mut tree = None;
    let mut records = None;
    let mut actor_id = None;
    let mut operation_id = None;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {}", argument.to_string_lossy()))?;
        match argument.to_str() {
            Some("--tree") => tree = Some(PathBuf::from(value)),
            Some("--records") => records = Some(PathBuf::from(value)),
            Some("--actor-id") => {
                actor_id = Some(
                    value
                        .into_string()
                        .map_err(|_| "actor id is not UTF-8".to_owned())?,
                );
            }
            Some("--semantic-operation-id") => {
                operation_id = Some(
                    value
                        .into_string()
                        .map_err(|_| "semantic operation id is not UTF-8".to_owned())?,
                );
            }
            _ => {
                return Err(format!(
                    "unknown argument {}; expected --tree, --records, --actor-id, or --semantic-operation-id",
                    argument.to_string_lossy()
                ));
            }
        }
    }
    let tree = tree.ok_or_else(|| "--tree is required".to_owned())?;
    let records = records.ok_or_else(|| "--records is required".to_owned())?;
    let actor_id = actor_id.ok_or_else(|| "--actor-id is required".to_owned())?;
    let operation_id =
        operation_id.ok_or_else(|| "--semantic-operation-id is required".to_owned())?;
    if actor_id.is_empty() || operation_id.is_empty() {
        return Err("attribution identifiers cannot be empty".to_owned());
    }
    let summary = oracle_scan::scan(&tree, &records, &actor_id, &operation_id)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&summary)
            .map_err(|error| format!("encode oracle summary: {error}"))?
    );
    Ok(())
}
