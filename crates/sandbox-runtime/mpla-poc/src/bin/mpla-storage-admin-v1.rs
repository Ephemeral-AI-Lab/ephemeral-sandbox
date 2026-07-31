use std::io::{BufRead, BufReader, Read, Write};
use std::process::ExitCode;
use std::time::Instant;

use sandbox_runtime_mpla_poc::storage_admin::{
    decode_holder_namespace_semantic_snapshot_invocation, decode_invocation,
    decode_publication_invocation_sequence, run_platform_holder_namespace_semantic_snapshot,
    run_platform_invocation, run_platform_publication_sequence,
};

const MAX_STDIN_BYTES: u64 = 3 * 1024 * 1024 + 1;

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
    enum Mode {
        Single,
        PublicationSequence,
        HolderNamespaceSemanticSnapshot,
    }
    let mode = match std::env::args_os().skip(1).collect::<Vec<_>>().as_slice() {
        [] => Mode::Single,
        [argument] if argument == "--publication-sequence" => Mode::PublicationSequence,
        [argument] if argument == "--holder-namespace-semantic-snapshot" => {
            Mode::HolderNamespaceSemanticSnapshot
        }
        _ => {
            return Err(
                "mpla-storage-admin-v1 accepts only fixed publication-sequence or holder-namespace-semantic-snapshot modes".into(),
            )
        }
    };
    match mode {
        Mode::PublicationSequence => return run_publication_sequence(),
        Mode::HolderNamespaceSemanticSnapshot => return run_holder_namespace_semantic_snapshot(),
        Mode::Single => {}
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_STDIN_BYTES)
        .read_to_end(&mut bytes)?;
    let mut stdout = std::io::stdout().lock();
    let invocation = decode_invocation(&bytes)?;
    let receipt = run_platform_invocation(&invocation)?;
    serde_json::to_writer_pretty(&mut stdout, &receipt)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn run_holder_namespace_semantic_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_STDIN_BYTES)
        .read_to_end(&mut bytes)?;
    let invocation = decode_holder_namespace_semantic_snapshot_invocation(&bytes)?;
    let receipt = run_platform_holder_namespace_semantic_snapshot(&invocation)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &receipt)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn run_publication_sequence() -> Result<(), Box<dyn std::error::Error>> {
    let input_decode_started = Instant::now();
    let stdin = std::io::stdin();
    let mut stdin = BufReader::new(stdin.lock());
    let mut invocation_line = Vec::new();
    stdin
        .by_ref()
        .take(MAX_STDIN_BYTES)
        .read_until(b'\n', &mut invocation_line)?;
    let invocations = decode_publication_invocation_sequence(&invocation_line)?;
    let input_decode_elapsed_ns =
        u64::try_from(input_decode_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let receipts = run_platform_publication_sequence(&invocations, |unmount_result| {
        let mut unmount_result = unmount_result.clone();
        unmount_result.input_decode_elapsed_ns = input_decode_elapsed_ns;
        serde_json::to_writer(&mut stdout, &unmount_result)
            .map_err(|error| format!("encode publication unmount receipts: {error}"))?;
        stdout
            .write_all(b"\n")
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("flush publication unmount receipts: {error}"))?;
        let mut acknowledgement = String::new();
        stdin
            .read_line(&mut acknowledgement)
            .map_err(|error| format!("read publication cleanup acknowledgement: {error}"))?;
        if acknowledgement.trim_end() != "continue-cleanup" {
            return Err("publication cleanup acknowledgement is invalid".to_owned());
        }
        Ok(())
    })?;
    if receipts.len() != 3 {
        return Err("publication storage sequence stopped before cleanup".into());
    }
    serde_json::to_writer(&mut stdout, &receipts[2])?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
