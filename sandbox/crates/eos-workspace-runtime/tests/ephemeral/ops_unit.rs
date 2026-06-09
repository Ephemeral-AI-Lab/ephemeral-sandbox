use std::cell::RefCell;
use std::collections::BTreeMap;

use eos_workspace_contract::{
    ReadFileRequest, ResolvedWorkspacePath, WorkspaceFileOps, WorkspaceMode, WorkspaceMutationKind,
    WorkspaceMutationOutcome, WorkspaceMutationRequest, WorkspaceMutationSink, WorkspaceReadBytes,
    WorkspaceReadView, WriteFileRequest,
};

use super::*;

struct FakePorts {
    bytes: Option<Vec<u8>>,
    recorded: RefCell<Option<WorkspaceMutationRequest>>,
}

impl FakePorts {
    fn new(bytes: Option<Vec<u8>>) -> Self {
        Self {
            bytes,
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
        Ok(WorkspaceReadBytes {
            bytes: self.bytes.clone(),
            exists: self.bytes.is_some(),
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
            mode: WorkspaceMode::Ephemeral,
            success: true,
            published: true,
            status: "committed".to_owned(),
            conflict: None,
            conflict_reason: None,
            changed_paths: vec![path.clone()],
            changed_path_kinds: BTreeMap::from([(path, "write".to_owned())]),
            mutation_source: "api_write".to_owned(),
            error: None,
            timings: BTreeMap::new(),
        })
    }
}

#[test]
fn read_file_uses_shared_read_view_and_preserves_ephemeral_mode() {
    let ops = EphemeralWorkspaceOps::new(FakePorts::new(Some(b"hello".to_vec())));

    let outcome = match ops.read_file(ReadFileRequest {
        path: "file.txt".to_owned(),
        max_read_bytes: 1024,
    }) {
        Ok(outcome) => outcome,
        Err(error) => panic!("read_file failed: {error}"),
    };

    assert_eq!(outcome.mode, WorkspaceMode::Ephemeral);
    assert!(outcome.success);
    assert_eq!(outcome.content, "hello");
}

#[test]
fn write_file_returns_publish_capable_mutation_outcome() {
    let ops = EphemeralWorkspaceOps::new(FakePorts::new(None));

    let outcome = match ops.write_file(WriteFileRequest {
        path: "file.txt".to_owned(),
        content: b"new".to_vec(),
        overwrite: true,
        max_file_bytes: 1024,
    }) {
        Ok(outcome) => outcome,
        Err(error) => panic!("write_file failed: {error}"),
    };

    assert_eq!(outcome.mode, WorkspaceMode::Ephemeral);
    assert!(outcome.success);
    assert!(outcome.published);
    assert_eq!(outcome.changed_paths, vec!["src/file.txt"]);

    let recorded = ops.ports().recorded.borrow();
    match recorded.as_ref() {
        Some(request) => assert_eq!(request.kind, WorkspaceMutationKind::Write),
        None => panic!("mutation sink was not called"),
    }
}
