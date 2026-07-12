use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::artifacts::{ArtifactError, ArtifactId, ArtifactStore, QuarantinedTail};
use crate::cleanup::{
    owned_identity, CleanupError, CleanupLedger, OwnedIdentity, OWNERSHIP_MARKER,
};
use crate::config::{BenchmarkPaths, StartupConfig};
use crate::events::{EventData, EventJournal, RunState};
use crate::report;
use crate::scheduler::{
    is_terminal, wall_timestamp, RunFailure, RunManifest, RUN_MANIFEST_SCHEMA_NAME,
    RUN_MANIFEST_SCHEMA_VERSION,
};

const MAX_RECOVERY_DIRECTORIES: usize = 100_000;
const MAX_RECOVERY_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryIssue {
    pub run_id: String,
    pub code: String,
    pub message: String,
    pub blocks_execution: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySummary {
    pub scanned_runs: usize,
    pub interrupted_runs: usize,
    pub cleaned_owned_targets: usize,
    pub quarantined_tails: usize,
    pub issues: Vec<RecoveryIssue>,
}

impl RecoverySummary {
    #[must_use]
    pub fn execution_safe(&self) -> bool {
        self.issues.iter().all(|issue| !issue.blocks_execution)
    }
}

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
}

/// Reconciles durable runs before the service permits a new campaign. A
/// previous process cannot still be represented by the new in-memory campaign
/// gate, so every non-terminal manifest is terminalized as interrupted after
/// marker-owned work is recovered. Raw complete records are preserved.
pub async fn reconcile_interrupted_runs(
    startup: &StartupConfig,
    store: &ArtifactStore,
) -> Result<RecoverySummary, RecoveryError> {
    let mut summary = RecoverySummary::default();
    for run_id in store.list_run_ids()? {
        summary.scanned_runs = summary.scanned_runs.saturating_add(1);
        let mut manifest = match store.read_envelope::<RunManifest>(
            &run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        ) {
            Ok(manifest) => manifest,
            Err(error) => {
                summary.issues.push(issue(
                    &run_id,
                    "manifest_unreadable",
                    format!("Run manifest could not be reconciled: {error}"),
                    true,
                ));
                continue;
            }
        };
        if is_terminal(manifest.state) {
            continue;
        }
        summary.interrupted_runs = summary.interrupted_runs.saturating_add(1);

        let mut quarantined = Vec::new();
        for artifact in [ArtifactId::Events, ArtifactId::Observations] {
            match store.quarantine_partial_tail(&run_id, artifact) {
                Ok(Some(tail)) => {
                    summary.quarantined_tails = summary.quarantined_tails.saturating_add(1);
                    quarantined.push(tail);
                }
                Ok(None) => {}
                Err(error) => summary.issues.push(issue(
                    &run_id,
                    "partial_tail_quarantine_failed",
                    format!("Could not quarantine {:?} tail: {error}", artifact),
                    true,
                )),
            }
        }

        let cleanup = cleanup_interrupted_run(&startup.paths, &run_id);
        let (cleaned, cleanup_error) = match cleanup {
            Ok(cleaned) => {
                summary.cleaned_owned_targets =
                    summary.cleaned_owned_targets.saturating_add(cleaned);
                (cleaned, None)
            }
            Err(error) => {
                let message = error.to_string();
                summary.issues.push(issue(
                    &run_id,
                    "interrupted_cleanup_failed",
                    message.clone(),
                    true,
                ));
                (0, Some(message))
            }
        };

        manifest.state = RunState::Failed;
        manifest.ended_at = wall_timestamp().ok();
        manifest.failure = Some(RunFailure {
            code: "runner_interrupted".to_owned(),
            message: recovery_failure_message(cleaned, &quarantined, cleanup_error.as_deref()),
            infrastructure: true,
        });
        if let Err(error) = store.replace_snapshot(
            &run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
            &manifest,
        ) {
            summary.issues.push(issue(
                &run_id,
                "manifest_terminalization_failed",
                format!("Interrupted manifest could not be made terminal: {error}"),
                true,
            ));
            continue;
        }

        if let Err(error) =
            append_recovery_events(store, &run_id, &quarantined, cleanup_error).await
        {
            summary
                .issues
                .push(issue(&run_id, "recovery_event_failed", error, false));
        }
        if let Err(error) = report::regenerate(store, &run_id, false) {
            summary.issues.push(issue(
                &run_id,
                "recovery_report_failed",
                format!("Interrupted report could not be regenerated: {error}"),
                false,
            ));
        }
    }
    Ok(summary)
}

async fn append_recovery_events(
    store: &ArtifactStore,
    run_id: &str,
    quarantined: &[QuarantinedTail],
    cleanup_error: Option<String>,
) -> Result<(), String> {
    let journal = EventJournal::open(store.clone(), run_id)
        .await
        .map_err(|error| error.to_string())?;
    for tail in quarantined {
        journal
            .emit(
                0,
                EventData::Warning {
                    code: "partial_ndjson_tail_quarantined".to_owned(),
                    message: format!(
                        "Quarantined incomplete {} record at line {} ({} bytes, {}).",
                        tail.artifact.as_str(),
                        tail.line,
                        tail.bytes,
                        tail.sha256
                    ),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    if let Some(error) = cleanup_error {
        journal
            .emit(
                0,
                EventData::Warning {
                    code: "interrupted_cleanup_failed".to_owned(),
                    message: bounded_message(&error),
                },
            )
            .await
            .map_err(|event_error| event_error.to_string())?;
    }
    journal
        .emit(
            0,
            EventData::RunState {
                state: RunState::Failed,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn cleanup_interrupted_run(paths: &BenchmarkPaths, run_id: &str) -> Result<usize, CleanupError> {
    let targets = discover_owned_targets(paths, run_id)?;
    let mut ledger = CleanupLedger::default();
    let mut cleaned = 0_usize;
    for (target, identity) in targets {
        let target = ledger.adopt_existing(paths, &target, &identity)?;
        ledger.remove_owned(paths, &target, &identity)?;
        cleaned = cleaned.saturating_add(1);
    }
    Ok(cleaned)
}

fn discover_owned_targets(
    paths: &BenchmarkPaths,
    run_id: &str,
) -> Result<Vec<(PathBuf, OwnedIdentity)>, CleanupError> {
    let run_root = paths.runs.join(run_id);
    let metadata = match fs::symlink_metadata(&run_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(cleanup_io(&run_root, source)),
    };
    if metadata.file_type().is_symlink() {
        return Err(CleanupError::Symlink(run_root));
    }
    if !metadata.is_dir() {
        return Err(CleanupError::OutsideRoot(run_root));
    }

    let mut pending = vec![(run_root, 0_usize)];
    let mut visited = 0_usize;
    let mut targets = Vec::new();
    while let Some((directory, depth)) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > MAX_RECOVERY_DIRECTORIES || depth > MAX_RECOVERY_DEPTH {
            return Err(CleanupError::OutsideRoot(directory));
        }
        let marker = directory.join(OWNERSHIP_MARKER);
        if marker.exists() {
            let identity = owned_identity(&directory)?;
            match &identity {
                OwnedIdentity::RunTrial {
                    run_id: marker_run_id,
                    ..
                } if marker_run_id == run_id => {
                    targets.push((directory, identity));
                    continue;
                }
                _ => return Err(CleanupError::IdentityMismatch(directory)),
            }
        }
        for entry in fs::read_dir(&directory).map_err(|source| cleanup_io(&directory, source))? {
            let entry = entry.map_err(|source| cleanup_io(&directory, source))?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| cleanup_io(&path, source))?;
            if metadata.file_type().is_symlink() {
                return Err(CleanupError::Symlink(path));
            }
            if metadata.is_dir() {
                pending.push((path, depth.saturating_add(1)));
            } else {
                // Files are valid only below a marker-owned directory, which
                // is treated atomically above. Unowned residue is never
                // recursively deleted during recovery.
                return Err(CleanupError::NotInLedger(path));
            }
        }
    }
    Ok(targets)
}

fn cleanup_io(path: &Path, source: io::Error) -> CleanupError {
    CleanupError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn recovery_failure_message(
    cleaned: usize,
    quarantined: &[QuarantinedTail],
    cleanup_error: Option<&str>,
) -> String {
    let mut message = format!(
        "The previous benchmark runner stopped before reaching a terminal state. Recovered {cleaned} marker-owned cleanup target(s) and quarantined {} incomplete log tail(s).",
        quarantined.len()
    );
    if let Some(error) = cleanup_error {
        message.push_str(" Cleanup did not restore the owned baseline: ");
        message.push_str(&bounded_message(error));
    }
    bounded_message(&message)
}

fn bounded_message(message: &str) -> String {
    const LIMIT: usize = 3_500;
    if message.len() <= LIMIT {
        return message.to_owned();
    }
    let mut end = LIMIT;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &message[..end])
}

fn issue(run_id: &str, code: &str, message: String, blocks_execution: bool) -> RecoveryIssue {
    RecoveryIssue {
        run_id: run_id.to_owned(),
        code: code.to_owned(),
        message: bounded_message(&message),
        blocks_execution,
    }
}
