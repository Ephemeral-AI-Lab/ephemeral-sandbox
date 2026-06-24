use std::collections::BTreeMap;
use std::path::PathBuf;

use sandbox_runtime_namespace_execution::NamespaceTarget;
use sandbox_runtime_workspace::model::{
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LayerStackSnapshotRef,
    LayerStackSnapshotView, LeaseId, ProtectedPathDrop, ProtectedPathDropReason,
    ReadonlySnapshotHandle, RemountWorkspaceRequest, RemountWorkspaceResult, WorkspaceEntry,
    WorkspaceEntryFds, WorkspaceHandle, WorkspaceProfile, WorkspaceSessionId,
};
use sandbox_runtime_workspace::overlay::dirs::OverlayDirs;
use sandbox_runtime_workspace::overlay::tree::TreeResourceStats;
use sandbox_runtime_workspace::profile::{
    WorkspaceModeFds, WorkspaceModeHandle, WorkspaceModeId, WorkspaceRemountState,
};

fn test_manifest() -> sandbox_runtime_layerstack::Manifest {
    sandbox_runtime_layerstack::Manifest::new(
        1,
        vec![sandbox_runtime_layerstack::LayerRef {
            layer_id: "L000001-test".to_owned(),
            path: "layers/L000001-test".to_owned(),
        }],
        sandbox_runtime_layerstack::MANIFEST_SCHEMA_VERSION,
    )
    .expect("test manifest is valid")
}

fn workspace_mode_handle() -> WorkspaceModeHandle {
    WorkspaceModeHandle {
        workspace_id: WorkspaceModeId("isolated-handle".to_owned()),
        profile: WorkspaceProfile::Isolated,
        lease_id: "lease-1".to_owned(),
        manifest_version: 42,
        manifest_root_hash: "root-hash".to_owned(),
        base_manifest: test_manifest(),
        workspace_root: "/workspace".to_owned(),
        dirs: OverlayDirs {
            run_dir: "/tmp/eos/run".into(),
            upperdir: "/tmp/eos/upper".into(),
            workdir: "/tmp/eos/work".into(),
        },
        layer_paths: vec!["/lower/one".into(), "/lower/two".into()],
        ns_fds: WorkspaceModeFds {
            user: Some(10),
            mnt: Some(11),
            pid: Some(12),
            net: Some(13),
        },
        holder_pid: 1234,
        readiness_fd: 13,
        control_fd: 14,
        veth: None,
        remount_state: WorkspaceRemountState::Pending,
        created_at: 1.0,
        last_activity: 2.0,
    }
}

fn assert_handle_projection(public: &WorkspaceHandle) {
    assert_eq!(public.id, WorkspaceSessionId("isolated-handle".to_owned()));
    assert_eq!(public.workspace_root, PathBuf::from("/workspace"));
    assert_eq!(public.profile, WorkspaceProfile::Isolated);
    assert_eq!(
        public.base_revision,
        BaseRevision {
            version: 42,
            root_hash: "root-hash".to_owned(),
            layer_count: 2,
        }
    );
    assert_eq!(
        public.snapshot,
        LayerStackSnapshotRef {
            lease_id: LeaseId("lease-1".to_owned()),
            manifest_version: 42,
            root_hash: "root-hash".to_owned(),
            manifest: test_manifest(),
            layer_paths: vec!["/lower/one".into(), "/lower/two".into()],
        }
    );
    let entry = public.entry().expect("handle produces workspace entry");
    assert_eq!(entry.workspace_root, PathBuf::from("/workspace"));
    assert_eq!(
        entry.layer_paths,
        vec![PathBuf::from("/lower/one"), PathBuf::from("/lower/two")]
    );
    assert_eq!(entry.upperdir, PathBuf::from("/tmp/eos/upper"));
    assert_eq!(entry.workdir, PathBuf::from("/tmp/eos/work"));
    assert_eq!(entry.ns_fds.user, 10);
    assert_eq!(entry.ns_fds.mnt, 11);
    assert_eq!(entry.ns_fds.pid, 12);
    assert_eq!(entry.ns_fds.net, Some(13));
}

#[test]
fn converts_workspace_mode_handle_to_public_handle() {
    let handle = workspace_mode_handle();

    assert_handle_projection(&WorkspaceHandle::from(&handle));
}

#[test]
fn public_handle_debug_does_not_expose_internal_storage_or_namespace_fields() {
    let public = WorkspaceHandle::from(&workspace_mode_handle());
    let debug = format!("{public:?}");

    assert_no_internal_fields(&debug);
    for forbidden in ["/lower/one", "/lower/two"] {
        assert!(
            !debug.contains(forbidden),
            "public handle debug output exposed snapshot path {forbidden}: {debug}"
        );
    }
}

#[test]
fn public_handle_debug_marks_launch_available_without_exposing_internals() {
    let public = WorkspaceHandle::from(&workspace_mode_handle());
    let debug = format!("{public:?}");

    assert!(debug.contains("launch"));
    assert!(debug.contains("<available>"));
    assert_no_internal_fields(&debug);
}

#[test]
fn host_compatible_entry_uses_holder_launch_without_network_fd() {
    let snapshot = LayerStackSnapshotRef {
        lease_id: LeaseId("lease-1".to_owned()),
        manifest_version: 1,
        root_hash: "root".to_owned(),
        manifest: test_manifest(),
        layer_paths: vec!["/lower/one".into()],
    };
    let handle = WorkspaceHandle::holder_backed_for_test(
        WorkspaceSessionId("shared-handle".to_owned()),
        "/workspace".into(),
        WorkspaceProfile::HostCompatible,
        snapshot,
        "/upper/shared".into(),
        "/work/shared".into(),
    );

    let entry = handle.entry().expect("host-compatible launch is valid");

    assert_eq!(entry.ns_fds.user, 10);
    assert_eq!(entry.ns_fds.mnt, 11);
    assert_eq!(entry.ns_fds.pid, 12);
    assert_eq!(entry.ns_fds.net, None);
}

#[test]
fn workspace_entry_converts_to_namespace_target() {
    let entry = WorkspaceEntry {
        workspace_root: "/workspace".into(),
        layer_paths: vec!["/lower/one".into(), "/lower/two".into()],
        upperdir: "/tmp/eos/upper".into(),
        workdir: "/tmp/eos/work".into(),
        ns_fds: WorkspaceEntryFds {
            user: 10,
            mnt: 11,
            pid: 12,
            net: Some(13),
        },
    };

    let target = NamespaceTarget::from(entry);

    assert_eq!(target.workspace_root, PathBuf::from("/workspace"));
    assert_eq!(
        target.layer_paths,
        vec![PathBuf::from("/lower/one"), PathBuf::from("/lower/two")]
    );
    assert_eq!(target.upperdir, Some(PathBuf::from("/tmp/eos/upper")));
    assert_eq!(target.workdir, Some(PathBuf::from("/tmp/eos/work")));
    assert_eq!(target.ns_fds.user.map(|fd| fd.0), Some(10));
    assert_eq!(target.ns_fds.mnt.map(|fd| fd.0), Some(11));
    assert_eq!(target.ns_fds.pid.map(|fd| fd.0), Some(12));
    assert_eq!(target.ns_fds.net.map(|fd| fd.0), Some(13));
}

#[test]
fn entry_rejects_incomplete_holder_launch() {
    let mut missing_mount = workspace_mode_handle();
    missing_mount.profile = WorkspaceProfile::HostCompatible;
    missing_mount.ns_fds.mnt = None;

    let mut missing_net = workspace_mode_handle();
    missing_net.ns_fds.net = None;

    for handle in [missing_mount, missing_net] {
        let public = WorkspaceHandle::from(&handle);
        let error = public
            .entry()
            .expect_err("incomplete holder launch is rejected");

        assert_eq!(error.to_string(), "workspace entry context is incomplete");
    }
}

#[test]
fn public_dto_debug_does_not_expose_internal_storage_or_namespace_fields() {
    let base_revision = BaseRevision {
        version: 1,
        root_hash: "root".to_owned(),
        layer_count: 1,
    };
    let dtos = [
        format!(
            "{:?}",
            CreateWorkspaceRequest {
                profile: WorkspaceProfile::HostCompatible,
            }
        ),
        format!(
            "{:?}",
            WorkspaceHandle::without_launch_for_test(
                WorkspaceSessionId("workspace".to_owned()),
                "/workspace".into(),
                WorkspaceProfile::HostCompatible,
                LayerStackSnapshotRef {
                    lease_id: LeaseId("lease".to_owned()),
                    manifest_version: 1,
                    root_hash: "root".to_owned(),
                    manifest: test_manifest(),
                    layer_paths: vec!["/lower/one".into()],
                },
            )
        ),
        format!(
            "{:?}",
            WorkspaceEntry {
                workspace_root: "/workspace".into(),
                layer_paths: vec!["/lower/one".into()],
                upperdir: "/tmp/eos/upper".into(),
                workdir: "/tmp/eos/work".into(),
                ns_fds: WorkspaceEntryFds {
                    user: 10,
                    mnt: 11,
                    pid: 12,
                    net: Some(13),
                },
            }
        ),
        format!(
            "{:?}",
            CaptureChangesRequest {
                include_stats: true,
            }
        ),
        format!(
            "{:?}",
            CapturedWorkspaceChanges {
                workspace_session_id: WorkspaceSessionId("workspace".to_owned()),
                base_revision,
                base_manifest: test_manifest(),
                changed_paths: Vec::new(),
                changed_path_kinds: BTreeMap::new(),
                protected_drops: Vec::new(),
                stats: None,
                changes: Vec::new(),
                metadata_path_count: 0,
            }
        ),
        format!("{:?}", DestroyWorkspaceRequest { grace_s: Some(1.0) }),
        format!(
            "{:?}",
            RemountWorkspaceRequest {
                layer_paths: vec!["/lower/one".into()],
            }
        ),
        format!(
            "{:?}",
            RemountWorkspaceResult {
                handle: WorkspaceHandle::without_launch_for_test(
                    WorkspaceSessionId("workspace".to_owned()),
                    "/workspace".into(),
                    WorkspaceProfile::HostCompatible,
                    LayerStackSnapshotRef {
                        lease_id: LeaseId("lease".to_owned()),
                        manifest_version: 1,
                        root_hash: "root".to_owned(),
                        manifest: test_manifest(),
                        layer_paths: vec!["/lower/one".into()],
                    },
                ),
            }
        ),
        format!(
            "{:?}",
            ReadonlySnapshotHandle {
                view_root: "/view".into(),
                generation_key: "generation".to_owned(),
                snapshot: LayerStackSnapshotView {
                    manifest_version: 1,
                    root_hash: "root".to_owned(),
                    layer_paths: vec!["/lower/one".into()],
                },
            }
        ),
        format!(
            "{:?}",
            DestroyWorkspaceResult {
                workspace_session_id: WorkspaceSessionId("workspace".to_owned()),
                evicted_upperdir_bytes: 0,
                lifetime_s: 0.0,
                lease_released: Some(true),
                lease_release_error: None,
                active_leases_after: 0,
            }
        ),
    ];

    for debug in dtos {
        assert_no_internal_fields(&debug);
    }
}

fn assert_no_internal_fields(debug: &str) {
    for forbidden in [
        "upperdir:",
        "workdir:",
        "scratch_dir:",
        "ns_fds:",
        "holder_pid:",
        "readiness_fd:",
        "control_fd:",
        "veth:",
    ] {
        assert!(
            !debug.contains(forbidden),
            "public DTO debug output exposed {forbidden}: {debug}"
        );
    }
}

#[test]
fn public_dtos_construct_clone_and_compare() {
    let base_revision = BaseRevision {
        version: 1,
        root_hash: "root".to_owned(),
        layer_count: 1,
    };
    let create = CreateWorkspaceRequest {
        profile: WorkspaceProfile::HostCompatible,
    };
    let handle = WorkspaceHandle::without_launch_for_test(
        WorkspaceSessionId("workspace".to_owned()),
        "/workspace".into(),
        WorkspaceProfile::HostCompatible,
        LayerStackSnapshotRef {
            lease_id: LeaseId("lease".to_owned()),
            manifest_version: 1,
            root_hash: "root".to_owned(),
            manifest: test_manifest(),
            layer_paths: vec!["/lower/one".into()],
        },
    );
    let entry = WorkspaceEntry {
        workspace_root: "/workspace".into(),
        layer_paths: vec!["/lower/one".into()],
        upperdir: "/tmp/eos/upper".into(),
        workdir: "/tmp/eos/work".into(),
        ns_fds: WorkspaceEntryFds {
            user: 10,
            mnt: 11,
            pid: 12,
            net: Some(13),
        },
    };
    let capture_request = CaptureChangesRequest {
        include_stats: true,
    };
    let capture = CapturedWorkspaceChanges {
        workspace_session_id: WorkspaceSessionId("workspace".to_owned()),
        base_revision: base_revision.clone(),
        base_manifest: test_manifest(),
        changed_paths: vec!["src/main.rs".to_owned()],
        changed_path_kinds: BTreeMap::from([("src/main.rs".to_owned(), ChangedPathKind::Write)]),
        protected_drops: vec![ProtectedPathDrop {
            path: "fifo".to_owned(),
            reason: ProtectedPathDropReason::UnsupportedSpecialFile,
        }],
        stats: Some(TreeResourceStats {
            files: 1,
            ..TreeResourceStats::default()
        }),
        changes: vec![sandbox_runtime_layerstack::LayerChange::Write {
            path: sandbox_runtime_layerstack::LayerPath::parse("src/main.rs")
                .expect("valid layer path"),
            content: b"fn main() {}\n".to_vec(),
        }],
        metadata_path_count: 1,
    };
    let destroy_request = DestroyWorkspaceRequest { grace_s: Some(1.0) };
    let remount_request = RemountWorkspaceRequest {
        layer_paths: vec!["/lower/one".into()],
    };
    let remount = RemountWorkspaceResult {
        handle: handle.clone(),
    };
    let readonly_snapshot = ReadonlySnapshotHandle {
        view_root: "/view".into(),
        generation_key: "generation".to_owned(),
        snapshot: LayerStackSnapshotView {
            manifest_version: handle.snapshot.manifest_version,
            root_hash: handle.snapshot.root_hash.clone(),
            layer_paths: handle.snapshot.layer_paths.clone(),
        },
    };
    let destroy = DestroyWorkspaceResult {
        workspace_session_id: WorkspaceSessionId("workspace".to_owned()),
        evicted_upperdir_bytes: 0,
        lifetime_s: 0.0,
        lease_released: Some(true),
        lease_release_error: None,
        active_leases_after: 0,
    };

    assert_eq!(create.clone(), create);
    assert_eq!(handle.clone(), handle);
    assert_eq!(entry.clone(), entry);
    assert_eq!(capture_request.clone(), capture_request);
    assert_eq!(capture.clone(), capture);
    assert_eq!(destroy_request.clone(), destroy_request);
    assert_eq!(remount_request.clone(), remount_request);
    assert_eq!(remount.clone(), remount);
    assert_eq!(readonly_snapshot.clone(), readonly_snapshot);
    assert_eq!(destroy.clone(), destroy);
}
