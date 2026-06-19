use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use workspace::model::{
    BaseRevision, CallerId, CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LatestSnapshotRequest,
    LayerStackSnapshotRef, LayerStackSnapshotView, LeaseId, ProtectedPathDrop,
    ProtectedPathDropReason, ReadonlySnapshotHandle, RemountWorkspaceRequest,
    RemountWorkspaceResult, WorkspaceEntry, WorkspaceEntryFds, WorkspaceHandle, WorkspaceId,
    WorkspaceProfile,
};
use workspace::overlay::dirs::OverlayDirs;
use workspace::overlay::tree::TreeResourceStats;
use workspace::profile::{
    DnsConfiguration, WorkspaceModeHandle, WorkspaceModeId, WorkspaceRemountState,
};

fn workspace_mode_handle() -> WorkspaceModeHandle {
    WorkspaceModeHandle {
        workspace_id: WorkspaceModeId("isolated-handle".to_owned()),
        profile: WorkspaceProfile::Isolated,
        caller_id: "caller-1".to_owned(),
        lease_id: "lease-1".to_owned(),
        manifest_version: 42,
        manifest_root_hash: "root-hash".to_owned(),
        workspace_root: "/workspace".to_owned(),
        dirs: OverlayDirs {
            run_dir: "/tmp/eos/run".into(),
            upperdir: "/tmp/eos/upper".into(),
            workdir: "/tmp/eos/work".into(),
        },
        layer_paths: vec!["/lower/one".into(), "/lower/two".into()],
        ns_fds: HashMap::from([
            ("user".to_owned(), 10),
            ("mnt".to_owned(), 11),
            ("pid".to_owned(), 12),
            ("net".to_owned(), 13),
        ]),
        holder_pid: 1234,
        readiness_fd: 13,
        control_fd: 14,
        veth: None,
        cgroup_path: Some("/sys/fs/cgroup/eos".into()),
        dns_configuration: DnsConfiguration {
            fallback_applied: true,
            previous_first_nameserver: Some("127.0.0.53".to_owned()),
        },
        remount_state: WorkspaceRemountState::Pending,
        created_at: 1.0,
        last_activity: 2.0,
    }
}

fn assert_handle_projection(public: &WorkspaceHandle) {
    assert_eq!(public.id, WorkspaceId("isolated-handle".to_owned()));
    assert_eq!(public.owner, CallerId("caller-1".to_owned()));
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
    assert_eq!(entry.cgroup_path, Some(PathBuf::from("/sys/fs/cgroup/eos")));
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
        layer_paths: vec!["/lower/one".into()],
    };
    let handle = WorkspaceHandle::holder_backed_for_test(
        WorkspaceId("host-handle".to_owned()),
        CallerId("caller".to_owned()),
        "/workspace".into(),
        WorkspaceProfile::HostCompatible,
        snapshot,
        "/upper/host".into(),
        "/work/host".into(),
        Some("/sys/fs/cgroup/eos-host".into()),
    );

    let entry = handle.entry().expect("host-compatible launch is valid");

    assert_eq!(entry.ns_fds.user, 10);
    assert_eq!(entry.ns_fds.mnt, 11);
    assert_eq!(entry.ns_fds.pid, 12);
    assert_eq!(entry.ns_fds.net, None);
    assert_eq!(entry.cgroup_path, Some("/sys/fs/cgroup/eos-host".into()));
}

#[test]
fn entry_rejects_incomplete_holder_launch() {
    let mut missing_mount = workspace_mode_handle();
    missing_mount.profile = WorkspaceProfile::HostCompatible;
    missing_mount.ns_fds.remove("mnt");

    let mut missing_net = workspace_mode_handle();
    missing_net.ns_fds.remove("net");

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
                caller_id: CallerId("caller".to_owned()),
                workspace_root: "/workspace".into(),
                layer_stack_root: "/layers".into(),
                profile: WorkspaceProfile::HostCompatible,
            }
        ),
        format!(
            "{:?}",
            WorkspaceHandle::without_launch_for_test(
                WorkspaceId("workspace".to_owned()),
                CallerId("caller".to_owned()),
                "/workspace".into(),
                WorkspaceProfile::HostCompatible,
                LayerStackSnapshotRef {
                    lease_id: LeaseId("lease".to_owned()),
                    manifest_version: 1,
                    root_hash: "root".to_owned(),
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
                cgroup_path: Some("/sys/fs/cgroup/eos".into()),
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
                workspace_id: WorkspaceId("workspace".to_owned()),
                base_revision,
                changed_paths: Vec::new(),
                changed_path_kinds: BTreeMap::new(),
                protected_drops: Vec::new(),
                stats: None,
                changes: Vec::new(),
                route_stats: layerstack::CaptureRouteStats::default(),
                metadata_path_count: 0,
                spool_dir: None,
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
                    WorkspaceId("workspace".to_owned()),
                    CallerId("caller".to_owned()),
                    "/workspace".into(),
                    WorkspaceProfile::HostCompatible,
                    LayerStackSnapshotRef {
                        lease_id: LeaseId("lease".to_owned()),
                        manifest_version: 1,
                        root_hash: "root".to_owned(),
                        layer_paths: vec!["/lower/one".into()],
                    },
                ),
            }
        ),
        format!(
            "{:?}",
            LatestSnapshotRequest {
                workspace_root: "/workspace".into(),
                owner_request_id: "request".to_owned(),
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
                workspace_id: WorkspaceId("workspace".to_owned()),
                owner: CallerId("caller".to_owned()),
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
        "cgroup_path:",
        "veth:",
        "dns_configuration:",
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
        caller_id: CallerId("caller".to_owned()),
        workspace_root: "/workspace".into(),
        layer_stack_root: "/layers".into(),
        profile: WorkspaceProfile::HostCompatible,
    };
    let handle = WorkspaceHandle::without_launch_for_test(
        WorkspaceId("workspace".to_owned()),
        CallerId("caller".to_owned()),
        "/workspace".into(),
        WorkspaceProfile::HostCompatible,
        LayerStackSnapshotRef {
            lease_id: LeaseId("lease".to_owned()),
            manifest_version: 1,
            root_hash: "root".to_owned(),
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
        cgroup_path: Some("/sys/fs/cgroup/eos".into()),
    };
    let capture_request = CaptureChangesRequest {
        include_stats: true,
    };
    let capture = CapturedWorkspaceChanges {
        workspace_id: WorkspaceId("workspace".to_owned()),
        base_revision: base_revision.clone(),
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
        changes: vec![layerstack::LayerChange::Write {
            path: layerstack::LayerPath::parse("src/main.rs").expect("valid layer path"),
            content: b"fn main() {}\n".to_vec(),
        }],
        route_stats: layerstack::CaptureRouteStats {
            gated_path_count: 1,
            ..layerstack::CaptureRouteStats::default()
        },
        metadata_path_count: 1,
        spool_dir: Some("/tmp/eos-spool".into()),
    };
    let destroy_request = DestroyWorkspaceRequest { grace_s: Some(1.0) };
    let remount_request = RemountWorkspaceRequest {
        layer_paths: vec!["/lower/one".into()],
    };
    let remount = RemountWorkspaceResult {
        handle: handle.clone(),
    };
    let latest_request = LatestSnapshotRequest {
        workspace_root: "/workspace".into(),
        owner_request_id: "request".to_owned(),
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
        workspace_id: WorkspaceId("workspace".to_owned()),
        owner: CallerId("caller".to_owned()),
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
    assert_eq!(latest_request.clone(), latest_request);
    assert_eq!(readonly_snapshot.clone(), readonly_snapshot);
    assert_eq!(destroy.clone(), destroy);
}
