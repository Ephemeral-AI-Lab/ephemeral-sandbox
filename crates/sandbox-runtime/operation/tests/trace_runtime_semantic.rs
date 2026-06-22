mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use sandbox_runtime_workspace::{
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind,
    DestroyWorkspaceRequest, ProtectedPathDrop, ProtectedPathDropReason, WorkspaceHandle,
    WorkspaceProfile,
};

use support::{
    build_services, create_request, trace::capture_traces, workspace_handle, FakeWorkspaceService,
};

#[test]
fn workspace_session_semantic_spans_use_live_call_paths_and_safe_fields() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let mut handle = workspace_handle(
        "workspace-secret-sentinel",
        "lease-secret-sentinel",
        PathBuf::from("/workspace/PATH_SECRET_SENTINEL"),
        WorkspaceProfile::HostCompatible,
    );
    handle.base_revision.root_hash = "ROOT_HASH_SECRET_SENTINEL".to_owned();
    handle.snapshot.root_hash = "ROOT_HASH_SECRET_SENTINEL".to_owned();
    fake.push_create_result(Ok(handle.clone()));
    let env = build_services(Arc::clone(&fake));

    let traces = capture_traces(|| {
        let handler = env
            .workspace
            .create_workspace_session(create_request())
            .expect("create workspace session succeeds");
        fake.push_capture_result(Ok(capture_result_with_sentinels(&handler.handle)));
        env.workspace
            .capture_session_changes(
                &handler,
                CaptureChangesRequest {
                    include_stats: false,
                },
            )
            .expect("capture succeeds");
        env.workspace
            .destroy_session(handler, DestroyWorkspaceRequest::default())
            .expect("destroy succeeds");
    });

    for expected in [
        "span workspace.create_session",
        "span workspace.capture_changes",
        "span workspace.destroy_session",
        "changed_path_count=1",
        "protected_drop_count=1",
        "metadata_path_count=2",
    ] {
        assert!(traces.contains(expected), "missing {expected} in {traces}");
    }
    for forbidden in [
        "workspace-secret-sentinel",
        "lease-secret-sentinel",
        "PATH_SECRET_SENTINEL",
        "ROOT_HASH_SECRET_SENTINEL",
        "LAYER_PATH_SECRET_SENTINEL",
        "CONTENT_SECRET_SENTINEL",
        "/workspace/",
        "/lower/one",
        "WorkspaceHandle",
        "WorkspaceEntry",
        "CapturedWorkspaceChanges",
    ] {
        assert!(
            !traces.contains(forbidden),
            "forbidden value {forbidden} appeared in traces: {traces}"
        );
    }
}

fn capture_result_with_sentinels(handle: &WorkspaceHandle) -> CapturedWorkspaceChanges {
    CapturedWorkspaceChanges {
        workspace_session_id: handle.id.clone(),
        base_revision: BaseRevision {
            version: 2,
            root_hash: "ROOT_HASH_SECRET_SENTINEL".to_owned(),
            layer_count: handle.snapshot.layer_paths.len(),
        },
        base_manifest: handle.snapshot.manifest.clone(),
        changed_paths: vec!["LAYER_PATH_SECRET_SENTINEL/file.txt".to_owned()],
        changed_path_kinds: BTreeMap::from([(
            "LAYER_PATH_SECRET_SENTINEL/file.txt".to_owned(),
            ChangedPathKind::Write,
        )]),
        protected_drops: vec![ProtectedPathDrop {
            path: "LAYER_PATH_SECRET_SENTINEL/protected".to_owned(),
            reason: ProtectedPathDropReason::InvalidLayerPath,
        }],
        stats: None,
        changes: vec![sandbox_runtime_layerstack::LayerChange::Write {
            path: sandbox_runtime_layerstack::LayerPath::parse(
                "LAYER_PATH_SECRET_SENTINEL/file.txt",
            )
            .expect("test path is valid"),
            content: b"CONTENT_SECRET_SENTINEL".to_vec(),
        }],
        metadata_path_count: 2,
    }
}
