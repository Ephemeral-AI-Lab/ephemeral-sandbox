use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use sandbox_runtime_mpla_poc::inventory::{
    capture_inventory, capture_physical_witness, capture_stable_metadata_pair, capture_stable_pair,
};
use sandbox_runtime_mpla_poc::quiesce::validate_receipt_hit_input;
use sandbox_runtime_mpla_poc::semantic::record::{
    NodeKind, NodeRecord, RecordMutation, SemanticRecord,
};
use sandbox_runtime_mpla_poc::semantic::write_affected_stream;
use sandbox_runtime_mpla_poc::{
    AllocationDescriptor, AllocationHandle, ManagedProcessTree, OperationId, PocError,
    ReceiptHitSealInput, SCHEMA_VERSION,
};

#[test]
fn external_session_preparation_persists_open_state_without_mounting() {
    let root = TestDirectory::new("external-session-preparation");
    let allocation_operation = OperationId::from_string("allocate-external-session");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &allocation_operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &allocation_operation,
    )
    .expect("issue workspace lease");

    let prepared = sandbox_runtime_mpla_poc::prepare_external_session(
        &root.0.join("control"),
        &allocation,
        &lease,
    )
    .expect("prepare external session without mounting");

    assert!(prepared.session_dir().join("SESSION.json").is_file());
    assert!(prepared.workspace_root().is_dir());
    let record: sandbox_runtime_mpla_poc::SessionRecord = serde_json::from_slice(
        &fs::read(prepared.session_dir().join("SESSION.json")).expect("read session record"),
    )
    .expect("parse session record");
    assert_eq!(record.session_id, lease.session_id);
    assert_eq!(record.allocation_id, allocation.descriptor.allocation_id);
    assert_eq!(record.phase, sandbox_runtime_mpla_poc::SessionPhase::Open);
    assert_eq!(record.workspace_root, prepared.workspace_root());
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mpla-poc-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn double_inventory_is_stable_and_detects_later_mutation() {
    let root = TestDirectory::new("inventory");
    let allocation = allocation_handle(&root.0);
    fs::create_dir_all(allocation.upper_dir.join("nested")).expect("create nested directory");
    fs::write(allocation.upper_dir.join("nested/file"), b"first").expect("write fixture");

    let (before, after) = capture_stable_pair(&allocation).expect("stable inventory");
    assert_eq!(before, after);
    assert_eq!(before.physical.file_count, 1);
    assert_eq!(before.physical.logical_bytes, 5);

    fs::write(allocation.upper_dir.join("nested/file"), b"second").expect("mutate fixture");
    let changed = capture_inventory(&allocation).expect("capture changed inventory");
    assert_ne!(before.inventory_sha256, changed.inventory_sha256);
}

#[test]
fn metadata_stability_inventory_omits_regular_file_content_digests() {
    let root = TestDirectory::new("metadata-inventory");
    let allocation = allocation_handle(&root.0);
    fs::create_dir_all(allocation.upper_dir.join("nested")).expect("create nested directory");
    fs::write(allocation.upper_dir.join("nested/file"), b"fixture").expect("write fixture");

    let (before, after) =
        capture_stable_metadata_pair(&allocation).expect("stable metadata inventory");
    let full = capture_inventory(&allocation).expect("capture full inventory");

    assert_eq!(before, after);
    assert!(before
        .entries
        .iter()
        .all(|entry| entry.content_sha256.is_none()));
    assert!(full
        .entries
        .iter()
        .any(|entry| entry.content_sha256.is_some()));
    assert_ne!(before.inventory_sha256, full.inventory_sha256);
}

#[test]
fn receipt_hit_witness_is_bounded_to_authenticated_affected_paths() {
    let root = TestDirectory::new("receipt-witness");
    let allocation = allocation_handle(&root.0);
    let affected = PathBuf::from("nested/affected");
    fs::create_dir_all(allocation.upper_dir.join("nested")).expect("create nested directory");
    fs::write(allocation.upper_dir.join(&affected), b"first").expect("write affected fixture");
    fs::write(
        allocation.upper_dir.join("unrelated"),
        vec![7_u8; 1024 * 1024],
    )
    .expect("write unrelated fixture");

    let before = capture_physical_witness(&allocation, std::slice::from_ref(&affected))
        .expect("capture bounded witness");
    fs::write(allocation.upper_dir.join(&affected), b"later").expect("replace affected bytes");
    let after = capture_physical_witness(&allocation, std::slice::from_ref(&affected))
        .expect("recapture bounded witness");

    assert_eq!(before, after);
    assert_eq!(before.file_count, 1);
    assert_eq!(before.logical_bytes, 5);
    assert_eq!(before.representative_inodes.len(), 2);
    assert!(!before
        .representative_inodes
        .iter()
        .any(|entry| entry.relative_path == std::path::Path::new("unrelated")));
    assert!(capture_physical_witness(&allocation, &[PathBuf::from("../escape")]).is_err());
}

#[test]
fn receipt_hit_input_binds_stream_bytes_and_normalized_path_set() {
    let root = TestDirectory::new("receipt-input");
    let stream = root.0.join("affected.stream");
    let digest = write_affected_stream(
        &stream,
        [RecordMutation::Replace(SemanticRecord::Node(node_record(
            b"nested/affected",
        )))],
    )
    .expect("write affected stream");
    let input = ReceiptHitSealInput {
        schema_version: SCHEMA_VERSION,
        affected_stream: stream.clone(),
        affected_stream_sha256: digest,
        affected_paths: vec![PathBuf::from("nested/affected")],
    };
    validate_receipt_hit_input(&input).expect("validate exact receipt input");

    fs::write(&stream, b"changed").expect("replace affected stream");
    assert!(matches!(
        validate_receipt_hit_input(&input),
        Err(PocError::Integrity(_))
    ));

    let second_stream = root.0.join("second.stream");
    let second_digest = write_affected_stream(
        &second_stream,
        [RecordMutation::Replace(SemanticRecord::Node(node_record(
            b"nested/affected",
        )))],
    )
    .expect("write second affected stream");
    let mut invalid = input;
    invalid.affected_stream = second_stream;
    invalid.affected_stream_sha256 = second_digest;
    invalid.affected_paths = vec![PathBuf::from("other")];
    assert!(matches!(
        validate_receipt_hit_input(&invalid),
        Err(PocError::Integrity(_))
    ));
    invalid.affected_paths = vec![PathBuf::from("../escape")];
    assert!(matches!(
        validate_receipt_hit_input(&invalid),
        Err(PocError::Integrity(_))
    ));
}

fn node_record(path: &[u8]) -> NodeRecord {
    NodeRecord {
        path: path.to_vec(),
        kind: NodeKind::Regular,
        mode: 0o644,
        uid: 1,
        gid: 1,
        mtime_seconds: 1,
        mtime_nanoseconds: 0,
        logical_size: 1,
        symlink_target: Vec::new(),
        device_major: 0,
        device_minor: 0,
    }
}

#[cfg(unix)]
#[test]
fn managed_process_tree_executes_then_fences_admission() {
    let root = TestDirectory::new("process-tree");
    let mut tree = ManagedProcessTree::new(root.0.clone(), None);
    let args = vec!["-c".to_owned(), "printf ok > sentinel".to_owned()];
    let receipt = tree
        .run(
            std::path::Path::new("/bin/sh"),
            &args,
            Duration::from_secs(2),
        )
        .expect("run managed command");
    assert!(receipt.success);
    assert_eq!(
        fs::read_to_string(root.0.join("sentinel")).expect("read sentinel"),
        "ok"
    );

    tree.fence();
    let error = tree
        .run(
            std::path::Path::new("/bin/sh"),
            &args,
            Duration::from_secs(2),
        )
        .expect_err("fenced admission must fail");
    assert!(matches!(error, PocError::Integrity(_)));
    tree.stop_kill_reap().expect("clean process groups");
}

#[cfg(unix)]
#[test]
fn managed_process_tree_probes_from_external_adapter_child() {
    let root = TestDirectory::new("readiness-probe");
    let mut content = vec![b'x'; 4094];
    content.extend_from_slice(b"boundary-sentinel");
    fs::write(root.0.join("sentinel"), content).expect("write readiness sentinel");
    let mut tree = ManagedProcessTree::new(root.0.clone(), None);

    let receipt = tree
        .probe_file(
            std::path::Path::new("sentinel"),
            Some(b"boundary-sentinel"),
            Duration::from_secs(2),
        )
        .expect("probe readiness from adapter child");
    assert!(receipt.success);
    assert_eq!(
        receipt.program,
        std::path::Path::new("adapter-direct-open-read-metadata")
    );
    assert!(
        !tree
            .probe_file(
                std::path::Path::new("sentinel"),
                Some(b"missing"),
                Duration::from_secs(2),
            )
            .expect("report content mismatch")
            .success
    );
    assert!(tree
        .probe_file(
            std::path::Path::new("../escape"),
            None,
            Duration::from_secs(2),
        )
        .is_err());
    assert!(tree
        .probe_file(
            std::path::Path::new("sentinel"),
            Some(b""),
            Duration::from_secs(2),
        )
        .is_err());
    assert!(tree.audit(false).expect("audit readiness child").is_clear());
    tree.stop_kill_reap().expect("clean readiness child");
}

fn allocation_handle(root: &std::path::Path) -> AllocationHandle {
    let allocation_root = root.join("allocations").join("aa").join("fixture");
    let upper_dir = allocation_root.join("upper");
    let work_dir = allocation_root.join("work");
    let owner_dir = allocation_root.join("owner");
    for path in [&upper_dir, &work_dir, &owner_dir] {
        fs::create_dir_all(path).expect("create allocation path");
    }
    AllocationHandle {
        descriptor: AllocationDescriptor {
            schema_version: SCHEMA_VERSION,
            allocation_id: sandbox_runtime_mpla_poc::AllocationId::from_string("fixture"),
            created_by_operation: OperationId::from_string("create-fixture"),
            created_unix_ms: 1,
        },
        allocation_root,
        upper_dir,
        work_dir,
        owner_dir,
    }
}
