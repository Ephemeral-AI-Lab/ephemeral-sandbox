use std::cell::RefCell;
use std::collections::BTreeMap;

use eos_workspace_contract::{
    ReadFileRequest, ResolvedWorkspacePath, WorkspaceFileOps, WorkspaceMode, WorkspaceMutationKind,
    WorkspaceMutationOutcome, WorkspaceMutationRequest, WorkspaceMutationSink, WorkspaceReadBytes,
    WorkspaceReadView, WriteFileRequest,
};

use super::*;

struct FakePorts {
    upper_bytes: Option<Vec<u8>>,
    snapshot_bytes: Option<Vec<u8>>,
    recorded: RefCell<Option<WorkspaceMutationRequest>>,
}

impl FakePorts {
    fn new(upper_bytes: Option<Vec<u8>>, snapshot_bytes: Option<Vec<u8>>) -> Self {
        Self {
            upper_bytes,
            snapshot_bytes,
            recorded: RefCell::new(None),
        }
    }
}

impl WorkspaceReadView for FakePorts {
    fn resolve_path(
        &self,
        request_path: &str,
    ) -> Result<ResolvedWorkspacePath, eos_workspace_contract::WorkspaceApiError> {
        Ok(ResolvedWorkspacePath::new(format!("src/{request_path}")))
    }

    fn read_bytes(
        &self,
        _path: &ResolvedWorkspacePath,
    ) -> Result<WorkspaceReadBytes, eos_workspace_contract::WorkspaceApiError> {
        let bytes = self
            .upper_bytes
            .clone()
            .or_else(|| self.snapshot_bytes.clone());
        Ok(WorkspaceReadBytes {
            exists: bytes.is_some(),
            bytes,
            manifest_version: Some(7),
            timings: BTreeMap::new(),
        })
    }
}

impl WorkspaceMutationSink for FakePorts {
    fn commit_or_record(
        &self,
        request: WorkspaceMutationRequest,
    ) -> Result<WorkspaceMutationOutcome, eos_workspace_contract::WorkspaceApiError> {
        let path = request.path.path.clone();
        self.recorded.replace(Some(request));
        Ok(WorkspaceMutationOutcome {
            mode: WorkspaceMode::Isolated,
            success: true,
            published: false,
            status: "committed".to_owned(),
            conflict: None,
            conflict_reason: None,
            changed_paths: vec![path.clone()],
            changed_path_kinds: BTreeMap::from([(path, "write".to_owned())]),
            mutation_source: "isolated_workspace".to_owned(),
            error: None,
            timings: BTreeMap::new(),
        })
    }
}

#[test]
fn read_file_uses_upperdir_first_view_and_preserves_isolated_mode() {
    let ops = IsolatedWorkspaceOps::new(FakePorts::new(
        Some(b"upper".to_vec()),
        Some(b"base".to_vec()),
    ));

    let outcome = match ops.read_file(ReadFileRequest {
        path: "file.txt".to_owned(),
        max_read_bytes: 1024,
    }) {
        Ok(outcome) => outcome,
        Err(error) => panic!("read_file failed: {error}"),
    };

    assert_eq!(outcome.mode, WorkspaceMode::Isolated);
    assert!(outcome.success);
    assert_eq!(outcome.content, "upper");
}

#[test]
fn write_file_records_audit_only_mutation_outcome() {
    let ops = IsolatedWorkspaceOps::new(FakePorts::new(None, None));

    let outcome = match ops.write_file(WriteFileRequest {
        path: "file.txt".to_owned(),
        content: b"new".to_vec(),
        overwrite: true,
        max_file_bytes: 1024,
    }) {
        Ok(outcome) => outcome,
        Err(error) => panic!("write_file failed: {error}"),
    };

    assert_eq!(outcome.mode, WorkspaceMode::Isolated);
    assert!(outcome.success);
    assert!(!outcome.published);
    assert_eq!(outcome.changed_paths, vec!["src/file.txt"]);

    let recorded = ops.ports().recorded.borrow();
    match recorded.as_ref() {
        Some(request) => assert_eq!(request.kind, WorkspaceMutationKind::Write),
        None => panic!("mutation sink was not called"),
    }
}
