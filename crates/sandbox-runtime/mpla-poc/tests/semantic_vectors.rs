use std::fs::{File, OpenOptions};
use std::io::BufReader;
use std::os::unix::fs::{symlink, FileExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{SeekFrom, XattrFlags};
use sandbox_runtime_mpla_poc::semantic::record::{
    ExtentKind, NodeKind, RecordStreamReader, SemanticRecord,
};
use sandbox_runtime_mpla_poc::semantic::{build_with_output, SemanticBuildOutput};
use sandbox_runtime_mpla_poc::{
    AllocationId, AttributionInput, OperationId, SemanticBuildRequest, SCHEMA_VERSION,
};
use uuid::Uuid;

#[test]
fn semantic_v1_covers_normalized_node_and_overlay_facts() {
    let temporary = Temporary::new("semantic-vectors");
    let tree = temporary.path.join("tree");
    create_semantic_fixture(&tree);

    let first = build_fixture(&temporary.path, &tree, "first", "actor-a");
    let records = read_records(&first.record_stream_path);
    let regular_metadata =
        std::fs::symlink_metadata(tree.join("regular")).expect("test operation must succeed");
    let regular_node = records
        .iter()
        .find_map(|record| match record {
            SemanticRecord::Node(node) if node.path == b"regular" => Some(node),
            _ => None,
        })
        .expect("test operation must succeed");
    assert_eq!(regular_node.kind, NodeKind::Regular);
    assert_eq!(regular_node.mode, regular_metadata.mode() & 0o7777);
    assert_eq!(regular_node.uid, regular_metadata.uid());
    assert_eq!(regular_node.gid, regular_metadata.gid());
    assert_eq!(regular_node.mtime_seconds, regular_metadata.mtime());
    assert_eq!(
        regular_node.mtime_nanoseconds,
        u32::try_from(regular_metadata.mtime_nsec()).expect("test operation must succeed")
    );
    assert_eq!(regular_node.logical_size, regular_metadata.size());
    assert!(records.iter().any(|record| matches!(
        record,
        SemanticRecord::Node(node) if node.path == b"regular" && node.kind == NodeKind::Regular
    )));
    assert!(records.iter().any(|record| matches!(
        record,
        SemanticRecord::Node(node)
            if node.path == b"link"
                && node.kind == NodeKind::Symlink
                && node.symlink_target == b"regular"
    )));
    assert!(records.iter().any(|record| matches!(
        record,
        SemanticRecord::Node(node) if node.path == b"opaque" && node.kind == NodeKind::Directory
    )));
    assert!(records.iter().any(
        |record| matches!(record, SemanticRecord::Xattr { path, name, value }
            if path == b"regular" && name == b"user.mpla" && value == b"semantic")
    ));
    let mut sparse_extents = records
        .iter()
        .filter_map(|record| match record {
            SemanticRecord::Extent {
                path,
                offset,
                length,
                kind,
            } if path == b"sparse" => Some((*offset, *length, *kind)),
            _ => None,
        })
        .collect::<Vec<_>>();
    sparse_extents.sort_by_key(|extent| extent.0);
    let mut sparse_cursor = 0_u64;
    for (offset, length, _) in &sparse_extents {
        assert_eq!(*offset, sparse_cursor);
        sparse_cursor += length;
    }
    assert_eq!(sparse_cursor, 2 * 1024 * 1024);
    let sparse_file = File::open(tree.join("sparse")).expect("test operation must succeed");
    let native_data_start =
        rustix::fs::seek(&sparse_file, SeekFrom::Data(0)).expect("test operation must succeed");
    let native_data_end = rustix::fs::seek(
        &sparse_file,
        SeekFrom::Hole(i64::try_from(native_data_start).expect("test operation must succeed")),
    )
    .expect("test operation must succeed");
    let native_reports_hole = native_data_start > 0 || native_data_end < sparse_cursor;
    assert_eq!(
        sparse_extents
            .iter()
            .any(|extent| extent.2 == ExtentKind::Hole),
        native_reports_hole
    );
    assert!(records
        .iter()
        .any(|record| matches!(record, SemanticRecord::Chunk { path, .. } if path == b"sparse")));
    assert!(records
        .iter()
        .any(|record| matches!(record, SemanticRecord::Whiteout { path } if path == b"deleted")));
    assert!(records.iter().any(
        |record| matches!(record, SemanticRecord::OpaqueDirectory { path } if path == b"opaque")
    ));
    assert!(records.iter().any(|record| matches!(
        record,
        SemanticRecord::HardlinkGroup {
            member_count: 2,
            ..
        }
    )));
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, SemanticRecord::HardlinkMember { .. }))
            .count(),
        2
    );
    assert_eq!(first.receipt.entry_count, records.len() as u64);
    assert_eq!(
        first.receipt.bytes_read,
        records
            .iter()
            .filter_map(|record| match record {
                SemanticRecord::Extent {
                    length,
                    kind: ExtentKind::Data,
                    ..
                } => Some(*length),
                _ => None,
            })
            .sum::<u64>()
    );
    assert!(first.receipt.bytes_read > 0);
    assert!(first.resource_maxima.peak_managed_bytes <= 8 * 1024 * 1024);
    assert!(first.receipt.peak_open_data_fds <= 16);

    let second = build_fixture(&temporary.path, &tree, "second", "actor-a");
    assert_eq!(first.receipt.roots, second.receipt.roots);
    assert_eq!(
        first.receipt.record_stream_sha256,
        second.receipt.record_stream_sha256
    );
    assert_eq!(
        std::fs::read(&first.record_stream_path).expect("test operation must succeed"),
        std::fs::read(&second.record_stream_path).expect("test operation must succeed")
    );

    let different_attribution = build_fixture(&temporary.path, &tree, "third", "actor-b");
    assert_eq!(
        first.receipt.roots.root_id,
        different_attribution.receipt.roots.root_id
    );
    assert_ne!(
        first.receipt.roots.attribution_root_id,
        different_attribution.receipt.roots.attribution_root_id
    );
}

#[test]
fn type_and_metadata_changes_change_content_identity() {
    let temporary = Temporary::new("semantic-type-change");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    let changing = tree.join("changing");
    std::fs::write(&changing, b"file").expect("test operation must succeed");
    std::fs::set_permissions(&changing, std::fs::Permissions::from_mode(0o640))
        .expect("test operation must succeed");
    let before = build_fixture(&temporary.path, &tree, "before", "actor");

    std::fs::remove_file(&changing).expect("test operation must succeed");
    std::fs::create_dir(&changing).expect("test operation must succeed");
    std::fs::set_permissions(&changing, std::fs::Permissions::from_mode(0o750))
        .expect("test operation must succeed");
    let after = build_fixture(&temporary.path, &tree, "after", "actor");
    assert_ne!(before.receipt.roots.root_id, after.receipt.roots.root_id);
}

#[test]
fn scanner_rejects_storage_paths_inside_the_semantic_tree() {
    let temporary = Temporary::new("semantic-storage-overlap");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    std::fs::write(tree.join("payload"), b"payload").expect("test operation must succeed");
    let request = SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("overlapping-storage"),
        allocation_id: AllocationId::from_string("excluded-allocation"),
        sealed_tree: tree.clone(),
        spool_dir: temporary.path.join("spool"),
        canonical_object_dir: tree.join("physical-object-store"),
        attribution: AttributionInput {
            actor_id: "actor".to_owned(),
            semantic_operation_id: "semantic-operation".to_owned(),
        },
    };
    assert!(build_with_output(&request).is_err());
}

fn create_semantic_fixture(tree: &Path) {
    std::fs::create_dir(tree).expect("test operation must succeed");
    let regular = tree.join("regular");
    std::fs::write(&regular, b"semantic payload").expect("test operation must succeed");
    std::fs::set_permissions(&regular, std::fs::Permissions::from_mode(0o640))
        .expect("test operation must succeed");
    rustix::fs::lsetxattr(&regular, "user.mpla", b"semantic", XattrFlags::empty())
        .expect("test operation must succeed");
    std::fs::hard_link(&regular, tree.join("regular-hardlink"))
        .expect("test operation must succeed");
    symlink("regular", tree.join("link")).expect("test operation must succeed");

    let sparse = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tree.join("sparse"))
        .expect("test operation must succeed");
    sparse
        .set_len(2 * 1024 * 1024)
        .expect("test operation must succeed");
    sparse
        .write_all_at(b"delta", 1024 * 1024)
        .expect("test operation must succeed");
    sparse.sync_all().expect("test operation must succeed");

    std::fs::write(tree.join(".wh.deleted"), b"").expect("test operation must succeed");
    let opaque = tree.join("opaque");
    std::fs::create_dir(&opaque).expect("test operation must succeed");
    std::fs::write(opaque.join(".wh..wh..opq"), b"").expect("test operation must succeed");
    std::fs::write(opaque.join("visible"), b"present").expect("test operation must succeed");
}

fn build_fixture(root: &Path, tree: &Path, name: &str, actor: &str) -> SemanticBuildOutput {
    let request = SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string(format!("operation-{name}")),
        allocation_id: AllocationId::from_string(format!("allocation-{name}")),
        sealed_tree: tree.to_path_buf(),
        spool_dir: root.join(format!("spool-{name}")),
        canonical_object_dir: root.join(format!("objects-{name}")),
        attribution: AttributionInput {
            actor_id: actor.to_owned(),
            semantic_operation_id: "semantic-operation".to_owned(),
        },
    };
    build_with_output(&request).expect("test operation must succeed")
}

fn read_records(path: &Path) -> Vec<SemanticRecord> {
    let file = File::open(path).expect("test operation must succeed");
    let mut reader = RecordStreamReader::new(BufReader::new(file));
    let mut records = Vec::new();
    while let Some(record) = reader.next_record().expect("test operation must succeed") {
        records.push(record);
    }
    records
}

struct Temporary {
    path: PathBuf,
}

impl Temporary {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{label}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("test operation must succeed");
        Self { path }
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
