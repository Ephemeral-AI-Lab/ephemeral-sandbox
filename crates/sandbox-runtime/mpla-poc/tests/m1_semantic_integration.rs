use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use sandbox_runtime_mpla_poc::semantic::record::{
    RecordMutation, RecordStreamReader, SemanticRecord,
};
use sandbox_runtime_mpla_poc::semantic::{
    build_incremental, build_with_output, materialize_record_stream, write_affected_stream,
    IncrementalBuildRequest, SemanticBuildOutput,
};
use sandbox_runtime_mpla_poc::{
    AllocationId, AttributionInput, OperationId, SemanticBuildRequest, SCHEMA_VERSION,
};
use uuid::Uuid;

#[test]
fn sixteen_tiny_deltas_match_complete_rebuilds_without_old_payload_reads() {
    let temporary = Temporary::new("semantic-incremental");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    for index in 0..32_u32 {
        std::fs::write(
            tree.join(format!("file-{index:04}")),
            format!("base-{index:04}"),
        )
        .expect("test operation must succeed");
    }
    let attribution = AttributionInput {
        actor_id: "actor".to_owned(),
        semantic_operation_id: "semantic-operation".to_owned(),
    };
    let object_dir = temporary.path.join("incremental-objects");
    let mut current = full_build(&temporary.path, &tree, "base", &object_dir, &attribution);

    for delta_index in 0..16_u32 {
        std::fs::write(
            tree.join(format!("file-{delta_index:04}")),
            format!("changed-{delta_index:04}"),
        )
        .expect("test operation must succeed");
        let expected = full_build(
            &temporary.path,
            &tree,
            &format!("expected-{delta_index:02}"),
            &temporary
                .path
                .join(format!("expected-objects-{delta_index:02}")),
            &attribution,
        );
        let mutations = diff_streams(&current.record_stream_path, &expected.record_stream_path);
        assert!(!mutations.is_empty());
        assert!(mutations.len() <= 4);
        let affected = temporary
            .path
            .join(format!("affected-{delta_index:02}.records"));
        let affected_sha256 =
            write_affected_stream(&affected, mutations).expect("test operation must succeed");
        let request = IncrementalBuildRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: OperationId::from_string(format!("incremental-{delta_index:02}")),
            prior_manifest: current.root_manifest_path.clone(),
            expected_prior_roots: current.receipt.roots.clone(),
            expected_prior_record_stream_sha256: current.receipt.record_stream_sha256.clone(),
            affected_stream: affected.clone(),
            affected_stream_sha256: affected_sha256,
            affected_ranges_complete: true,
            canonical_object_dir: object_dir.clone(),
            attribution: attribution.clone(),
        };
        let incremental = build_incremental(&request).expect("test operation must succeed");
        assert_eq!(incremental.receipt.roots, expected.receipt.roots);
        assert_eq!(
            incremental.receipt.record_stream_sha256,
            expected.receipt.record_stream_sha256
        );
        assert_eq!(
            incremental.receipt.entry_count,
            expected.receipt.entry_count
        );
        assert_eq!(incremental.immutable_payload_bytes_read, 0);
        assert!(incremental.prior_node_bytes_read > 0);
        assert_eq!(
            incremental.affected_input_bytes,
            std::fs::metadata(&affected)
                .expect("test operation must succeed")
                .len()
        );
        assert_eq!(
            incremental.receipt.bytes_read,
            incremental.affected_input_bytes
        );
        assert!(
            incremental.affected_input_bytes
                < std::fs::metadata(&expected.record_stream_path)
                    .expect("test operation must succeed")
                    .len()
        );
        assert!(incremental.resource_maxima.peak_managed_bytes <= 8 * 1024 * 1024);
        assert!(incremental.receipt.peak_open_data_fds <= 16);

        let materialized = materialize_record_stream(&incremental.root_manifest_path, &object_dir)
            .expect("test operation must succeed");
        assert_eq!(
            read_records(&materialized),
            read_records(&expected.record_stream_path)
        );
        current = SemanticBuildOutput {
            receipt: incremental.receipt,
            record_stream_path: materialized,
            root_manifest_path: incremental.root_manifest_path,
            resource_maxima: incremental.resource_maxima,
        };
    }
}

#[test]
fn incremental_input_fails_closed_when_completeness_or_prior_handle_is_invalid() {
    let temporary = Temporary::new("semantic-incremental-reject");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    std::fs::write(tree.join("file"), b"before").expect("test operation must succeed");
    let attribution = AttributionInput {
        actor_id: "actor".to_owned(),
        semantic_operation_id: "semantic-operation".to_owned(),
    };
    let object_dir = temporary.path.join("objects");
    let base = full_build(&temporary.path, &tree, "base", &object_dir, &attribution);
    let affected = temporary.path.join("empty-affected");
    let digest = write_affected_stream(&affected, Vec::<RecordMutation>::new())
        .expect("test operation must succeed");
    let request = IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("incremental-reject"),
        prior_manifest: base.root_manifest_path,
        expected_prior_roots: base.receipt.roots,
        expected_prior_record_stream_sha256: base.receipt.record_stream_sha256,
        affected_stream: affected,
        affected_stream_sha256: digest,
        affected_ranges_complete: false,
        canonical_object_dir: object_dir,
        attribution,
    };
    assert!(build_incremental(&request).is_err());

    let mut wrong_prior = request.clone();
    wrong_prior.affected_ranges_complete = true;
    wrong_prior.expected_prior_record_stream_sha256 = "00".repeat(32);
    assert!(build_incremental(&wrong_prior).is_err());

    let content_root = request.expected_prior_roots.root_id.as_str();
    let content_root_object = request
        .canonical_object_dir
        .join("objects")
        .join(&content_root[..2])
        .join(content_root);
    std::fs::remove_file(content_root_object).expect("test operation must succeed");
    let mut missing_object = request.clone();
    missing_object.affected_ranges_complete = true;
    assert!(build_incremental(&missing_object).is_err());

    let mut missing_prior = request;
    missing_prior.affected_ranges_complete = true;
    missing_prior.prior_manifest = temporary.path.join("missing-prior-manifest");
    assert!(build_incremental(&missing_prior).is_err());
}

fn full_build(
    root: &Path,
    tree: &Path,
    name: &str,
    object_dir: &Path,
    attribution: &AttributionInput,
) -> SemanticBuildOutput {
    build_with_output(&SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string(format!("operation-{name}")),
        allocation_id: AllocationId::from_string(format!("allocation-{name}")),
        sealed_tree: tree.to_path_buf(),
        spool_dir: root.join(format!("spool-{name}")),
        canonical_object_dir: object_dir.to_path_buf(),
        attribution: attribution.clone(),
    })
    .expect("test operation must succeed")
}

fn diff_streams(before: &Path, after: &Path) -> Vec<RecordMutation> {
    let before = keyed_records(before);
    let after = keyed_records(after);
    let keys = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|key| match (before.get(&key), after.get(&key)) {
            (Some(old), Some(new)) if old == new => None,
            (_, Some(new)) => Some(RecordMutation::Replace(new.clone())),
            (Some(old), None) => Some(RecordMutation::Delete {
                canonical_key: old.canonical_key().expect("test operation must succeed"),
            }),
            (None, None) => unreachable!(),
        })
        .collect()
}

fn keyed_records(path: &Path) -> BTreeMap<[u8; 32], SemanticRecord> {
    read_records(path)
        .into_iter()
        .map(|record| {
            (
                record.key_digest().expect("test operation must succeed"),
                record,
            )
        })
        .collect()
}

fn read_records(path: &Path) -> Vec<SemanticRecord> {
    let mut reader = RecordStreamReader::new(BufReader::new(
        File::open(path).expect("test operation must succeed"),
    ));
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
