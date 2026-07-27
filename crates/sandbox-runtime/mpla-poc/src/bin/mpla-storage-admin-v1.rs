use std::io::{Read, Write};
use std::process::ExitCode;

use sandbox_runtime_mpla_poc::storage_admin::{decode_invocation, run_platform_invocation};

const MAX_STDIN_BYTES: u64 = 1024 * 1024 + 1;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mpla-storage-admin-v1: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().len() != 1 {
        return Err("mpla-storage-admin-v1 accepts no command-line arguments".into());
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_STDIN_BYTES)
        .read_to_end(&mut bytes)?;
    let invocation = decode_invocation(&bytes)?;
    let receipt = run_platform_invocation(&invocation)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &receipt)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
