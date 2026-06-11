use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;

struct FakeBackend {
    bytes: Option<Vec<u8>>,
    recorded: RefCell<Option<Mutation>>,
}

impl FakeBackend {
    fn new(bytes: Option<Vec<u8>>) -> Self {
        Self {
            bytes,
            recorded: RefCell::new(None),
        }
    }
}

impl FileBackend for FakeBackend {
    fn workspace_kind(&self) -> &'static str {
        "ephemeral"
    }

    fn mutation_source(&self, kind: MutationKind) -> &'static str {
        match kind {
            MutationKind::Write => "api_write",
            MutationKind::Edit => "api_edit",
        }
    }

    fn resolve_path(&self, request_path: &str) -> Result<ResolvedWorkspacePath, FileOpsError> {
        Ok(ResolvedWorkspacePath::new(format!("src/{request_path}")))
    }

    fn read_bytes(&self, _path: &ResolvedWorkspacePath) -> Result<ReadBytes, FileOpsError> {
        Ok(ReadBytes {
            bytes: self.bytes.clone(),
            exists: self.bytes.is_some(),
            manifest_version: Some(7),
            timings: BTreeMap::new(),
        })
    }

    fn apply(&self, mutation: Mutation) -> Result<MutationOutcome, FileOpsError> {
        let path = mutation.path.path.clone();
        self.recorded.replace(Some(mutation));
        Ok(MutationOutcome {
            workspace_kind: "ephemeral".to_owned(),
            success: true,
            published: true,
            status: "committed".to_owned(),
            conflict: None,
            conflict_reason: None,
            changed_paths: vec![path.clone()],
            changed_path_kinds: BTreeMap::from([(path, "write".to_owned())]),
            mutation_source: "api_write".to_owned(),
            timings: BTreeMap::new(),
            ..MutationOutcome::default()
        })
    }
}

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn read_resolves_path_and_caps_size() -> TestResult {
    let backend = FakeBackend::new(Some(b"hello".to_vec()));
    let outcome = read_file(
        &backend,
        ReadFileRequest {
            path: "main.rs".to_owned(),
            max_read_bytes: 16,
        },
    )?;
    assert!(outcome.exists);
    assert_eq!(outcome.content, "hello");

    let too_small = read_file(
        &backend,
        ReadFileRequest {
            path: "main.rs".to_owned(),
            max_read_bytes: 2,
        },
    );
    assert!(too_small.is_err(), "oversized read is a typed error");
    Ok(())
}

#[test]
fn read_missing_file_reports_absent_without_error() -> TestResult {
    let backend = FakeBackend::new(None);
    let outcome = read_file(
        &backend,
        ReadFileRequest {
            path: "gone.rs".to_owned(),
            max_read_bytes: 16,
        },
    )?;
    assert!(!outcome.exists);
    assert_eq!(outcome.content, "");
    Ok(())
}

#[test]
fn write_create_only_existing_is_rejected_conflict() -> TestResult {
    let backend = FakeBackend::new(Some(b"present".to_vec()));
    let outcome = write_file(
        &backend,
        WriteFileRequest {
            path: "main.rs".to_owned(),
            content: b"new".to_vec(),
            overwrite: false,
            max_file_bytes: 64,
        },
    )?;
    assert!(!outcome.success);
    assert_eq!(outcome.status, "rejected");
    assert_eq!(
        outcome.conflict_reason.as_deref(),
        Some("create_only_existing")
    );
    assert!(
        backend.recorded.borrow().is_none(),
        "rejected create-only write never reaches apply"
    );
    Ok(())
}

#[test]
fn write_pins_base_bytes_into_the_apply() -> TestResult {
    let backend = FakeBackend::new(Some(b"old".to_vec()));
    let outcome = write_file(
        &backend,
        WriteFileRequest {
            path: "main.rs".to_owned(),
            content: b"new".to_vec(),
            overwrite: true,
            max_file_bytes: 64,
        },
    )?;
    assert!(outcome.success, "write commits: {outcome:?}");
    let recorded = backend.recorded.borrow().clone().expect("apply called");
    assert_eq!(recorded.base.bytes.as_deref(), Some(b"old".as_slice()));
    assert_eq!(recorded.content, b"new");
    assert_eq!(recorded.path.path, "src/main.rs");
    Ok(())
}

#[test]
fn edit_applies_search_replace_against_base() -> TestResult {
    let backend = FakeBackend::new(Some(b"fn main() { old(); }".to_vec()));
    let outcome = edit_file(
        &backend,
        EditFileRequest {
            path: "main.rs".to_owned(),
            edits: vec![SearchReplaceEdit {
                old_text: "old()".to_owned(),
                new_text: "new()".to_owned(),
                replace_all: false,
            }],
        },
    )?;
    assert!(outcome.success);
    assert_eq!(outcome.applied_edits, 1);
    let recorded = backend.recorded.borrow().clone().expect("apply called");
    assert_eq!(recorded.content, b"fn main() { new(); }");
    Ok(())
}

#[test]
fn edit_anchor_count_mismatch_is_aborted_overlap() -> TestResult {
    let backend = FakeBackend::new(Some(b"x x".to_vec()));
    let outcome = edit_file(
        &backend,
        EditFileRequest {
            path: "main.rs".to_owned(),
            edits: vec![SearchReplaceEdit {
                old_text: "x".to_owned(),
                new_text: "y".to_owned(),
                replace_all: false,
            }],
        },
    )?;
    assert!(!outcome.success);
    assert_eq!(outcome.status, "aborted_overlap");
    assert_eq!(
        outcome.conflict.as_ref().map(|c| c.message.as_str()),
        Some("anchor occurrence count mismatch")
    );
    Ok(())
}

#[test]
fn edit_missing_file_is_aborted_version() -> TestResult {
    let backend = FakeBackend::new(None);
    let outcome = edit_file(
        &backend,
        EditFileRequest {
            path: "gone.rs".to_owned(),
            edits: vec![SearchReplaceEdit {
                old_text: "a".to_owned(),
                new_text: "b".to_owned(),
                replace_all: false,
            }],
        },
    )?;
    assert!(!outcome.success);
    assert_eq!(outcome.status, "aborted_version");
    Ok(())
}
