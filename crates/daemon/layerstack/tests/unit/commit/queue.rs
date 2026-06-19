use std::sync::mpsc;

use crate::commit::route::PublishDecision;
use crate::commit::worker::queue::{
    cas_exhaustion_result, disjoint_batches, PublishConflict, WorkItem, MAX_OCC_CAS_RETRIES,
};
use crate::commit::worker::PreparedChangeset;
use crate::commit::CommitStatus;
use crate::model::LayerPath;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn prepared(path: &str, atomic: bool) -> TestResult<PreparedChangeset> {
    let path = LayerPath::parse(path)?;
    let changes = vec![crate::model::LayerChange::Write {
        path: path.clone(),
        content: b"x".to_vec(),
    }];
    Ok(PreparedChangeset::try_new(
        &changes,
        vec![PublishDecision::gated(path, None)],
        atomic,
    )?)
}

fn item(path: &str, atomic: bool) -> TestResult<WorkItem> {
    let (reply, _) = mpsc::channel();
    Ok(WorkItem {
        prepared: prepared(path, atomic)?,
        reply,
    })
}

#[test]
fn batches_disjoint_non_atomic_changesets() -> TestResult {
    let batches = disjoint_batches(vec![item("a.txt", false)?, item("b.txt", false)?]);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 2);
    Ok(())
}

#[test]
fn atomic_changesets_are_not_batched() -> TestResult {
    let batches = disjoint_batches(vec![item("a.txt", true)?, item("b.txt", true)?]);
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 1);
    assert_eq!(batches[1].len(), 1);
    Ok(())
}

#[test]
fn overlapping_non_atomic_changesets_are_not_batched() -> TestResult {
    let batches = disjoint_batches(vec![item("a.txt", false)?, item("a.txt", false)?]);
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 1);
    assert_eq!(batches[1].len(), 1);
    Ok(())
}

#[test]
fn cas_retry_exhaustion_surfaces_aborted_version() -> TestResult {
    let prepared = prepared("a.txt", true)?;
    let result = cas_exhaustion_result(
        &prepared,
        &PublishConflict {
            observed_version: Some(42),
        },
        MAX_OCC_CAS_RETRIES,
    );
    assert!(!result.success());
    assert_eq!(result.files[0].status, CommitStatus::AbortedVersion);
    assert_eq!(result.files[0].observed_version, Some(42));
    assert_eq!(
        result.files[0].observed_state.as_deref(),
        Some("manifest_conflict")
    );
    let events = result.trace_events();
    assert_eq!(events.len(), 4);
    assert_eq!(events[2].module, "occ");
    assert_eq!(events[2].name, "commit_finished");
    assert_eq!(events[2].details["success"], false);
    assert_eq!(events[2].details["aborted_version_file_count"], 1);
    assert_eq!(events[3].module, "occ");
    assert_eq!(events[3].name, "conflict_detected");
    assert_eq!(events[3].details["path"], "a.txt");
    assert_eq!(events[3].details["reason"], "aborted_version");
    assert_eq!(events[3].details["observed_version"], 42);
    assert_eq!(events[3].details["observed_state"], "manifest_conflict");
    Ok(())
}
