use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sandbox_runtime_mpla_poc::semantic::record::{
    RecordMutation, RecordStreamReader, SemanticRecord,
};
use sandbox_runtime_mpla_poc::semantic::scan::scan_selected_paths;
use sandbox_runtime_mpla_poc::semantic::{
    affected_stream_paths, build_incremental, build_with_output, build_with_output_serial,
    capture_affected_paths, capture_affected_paths_with_maxima, materialize_record_stream,
    write_affected_stream, write_affected_stream_from_snapshots, AffectedPathSnapshot,
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
        assert!(
            incremental.receipt.durability.immutable_object_count <= 64,
            "incremental commit retained intermediate trie nodes"
        );

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

#[test]
fn affected_path_snapshots_read_only_selected_payload_and_emit_exact_paths() {
    let temporary = Temporary::new("semantic-affected-paths");
    let tree = temporary.path.join("tree");
    let work = temporary.path.join("work");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    std::fs::write(tree.join("a"), vec![b'a'; 65_536]).expect("test operation must succeed");
    std::fs::write(tree.join("b"), vec![b'b'; 65_536]).expect("test operation must succeed");
    let paths = vec![PathBuf::from("a")];
    let before = capture_affected_paths(&tree, &paths, &work)
        .expect("selected before snapshot must succeed");
    assert_eq!(before.payload_bytes_read, 65_536);
    std::fs::write(tree.join("a"), vec![b'c'; 65_536]).expect("test operation must succeed");
    let after =
        capture_affected_paths(&tree, &paths, &work).expect("selected after snapshot must succeed");
    assert_eq!(after.payload_bytes_read, 65_536);
    let affected = temporary.path.join("affected.records");
    write_affected_stream_from_snapshots(&affected, &before, &after)
        .expect("selected diff must succeed");
    assert_eq!(
        affected_stream_paths(&affected).expect("affected paths must decode"),
        paths
    );
}

#[test]
fn selected_parallel_dense_and_sparse_scan_matches_canonical_full_and_incremental_builds() {
    let temporary = Temporary::new("semantic-selected-parallel");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    let attribution = AttributionInput {
        actor_id: "actor".to_owned(),
        semantic_operation_id: "semantic-operation".to_owned(),
    };
    let paths = vec![
        PathBuf::new(),
        PathBuf::from("dense.bin"),
        PathBuf::from("sparse.bin"),
    ];
    std::fs::write(tree.join("dense.bin"), []).expect("test operation must succeed");
    File::create(tree.join("sparse.bin")).expect("test operation must succeed");
    let object_dir = temporary.path.join("objects");
    let prior = full_build(
        &temporary.path,
        &tree,
        "selected-prior",
        &object_dir,
        &attribution,
    );
    let before = capture_affected_paths(&tree, &paths, &temporary.path.join("selected-before"))
        .expect("selected before snapshot must succeed");

    let dense = (0..(1024 * 1024 + 17))
        .map(|index| u8::try_from(index % 251).expect("test byte must fit"))
        .collect::<Vec<_>>();
    std::fs::write(tree.join("dense.bin"), &dense).expect("test operation must succeed");
    let sparse_path = tree.join("sparse.bin");
    let mut sparse = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&sparse_path)
        .expect("test operation must succeed");
    sparse
        .set_len(1024 * 1024 + 37)
        .expect("test operation must succeed");
    sparse
        .write_all(b"sparse-prefix")
        .expect("test operation must succeed");
    sparse
        .seek(SeekFrom::Start(768 * 1024 + 11))
        .expect("test operation must succeed");
    sparse
        .write_all(b"sparse-suffix")
        .expect("test operation must succeed");
    sparse.sync_all().expect("test operation must succeed");

    let direct = scan_selected_paths(&tree, &paths, &temporary.path.join("selected-direct"))
        .expect("parallel selected scan must succeed");
    assert_eq!(direct.peak_data_workers, 4);
    assert!(direct.peak_open_data_fds <= 6);
    assert!(direct.peak_open_data_fds <= 16);
    assert!(direct.bytes_read >= u64::try_from(dense.len()).expect("test size must fit"));
    let (after, selected_maxima) =
        capture_affected_paths_with_maxima(&tree, &paths, &temporary.path.join("selected-after"))
            .expect("selected snapshot must succeed");
    assert_eq!(direct.records, after.records);
    assert_eq!(selected_maxima.peak_data_workers, 4);
    assert!(selected_maxima.peak_open_data_fds <= 16);
    assert!(selected_maxima.peak_managed_bytes <= 8 * 1024 * 1024);

    let expected = full_build(
        &temporary.path,
        &tree,
        "selected-expected",
        &temporary.path.join("selected-expected-objects"),
        &attribution,
    );
    assert_eq!(direct.records, read_records(&expected.record_stream_path));
    let affected = temporary.path.join("selected.records");
    let affected_sha256 = write_affected_stream_from_snapshots(&affected, &before, &after)
        .expect("selected delta must succeed");
    let incremental = build_incremental(&IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("selected-parallel-incremental"),
        prior_manifest: prior.root_manifest_path,
        expected_prior_roots: prior.receipt.roots,
        expected_prior_record_stream_sha256: prior.receipt.record_stream_sha256,
        affected_stream: affected,
        affected_stream_sha256: affected_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: object_dir,
        attribution,
    })
    .expect("incremental selected delta must succeed");
    let combined_maxima = incremental
        .resource_maxima
        .with_sequential_phase(selected_maxima);
    assert_eq!(combined_maxima.peak_data_workers, 4);
    assert_eq!(
        combined_maxima.application_pool_bytes,
        incremental.resource_maxima.application_pool_bytes
    );
    assert_eq!(
        combined_maxima.scan_window_bytes,
        incremental.resource_maxima.scan_window_bytes
    );
    assert_eq!(
        combined_maxima.spool_run_bytes,
        incremental.resource_maxima.spool_run_bytes
    );
    assert_eq!(
        combined_maxima.merge_fan_in,
        incremental.resource_maxima.merge_fan_in
    );
    assert_eq!(
        combined_maxima.trie_fan_out,
        incremental.resource_maxima.trie_fan_out
    );
    assert_eq!(incremental.receipt.roots, expected.receipt.roots);
    assert_eq!(
        incremental.receipt.record_stream_sha256,
        expected.receipt.record_stream_sha256
    );
}

#[test]
fn new_root_file_delta_includes_root_metadata_and_matches_complete_rebuild() {
    let temporary = Temporary::new("semantic-root-delta");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    std::fs::write(tree.join("base"), b"base").expect("test operation must succeed");
    let attribution = AttributionInput {
        actor_id: "actor".to_owned(),
        semantic_operation_id: "semantic-operation".to_owned(),
    };
    let object_dir = temporary.path.join("incremental-objects");
    let prior = full_build(
        &temporary.path,
        &tree,
        "root-delta-prior",
        &object_dir,
        &attribution,
    );

    std::fs::write(tree.join("delta"), b"delta").expect("test operation must succeed");
    let semantic_paths = vec![PathBuf::new(), PathBuf::from("delta")];
    let after = capture_affected_paths(
        &tree,
        &semantic_paths,
        &temporary.path.join("root-delta-after"),
    )
    .expect("root and file snapshot must succeed");
    let affected = temporary.path.join("root-delta.records");
    let affected_sha256 = write_affected_stream_from_snapshots(
        &affected,
        &AffectedPathSnapshot {
            paths: semantic_paths,
            records: Vec::new(),
            payload_bytes_read: 0,
        },
        &after,
    )
    .expect("root and file delta must succeed");
    let incremental = build_incremental(&IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("root-delta-incremental".to_owned()),
        prior_manifest: prior.root_manifest_path,
        expected_prior_roots: prior.receipt.roots,
        expected_prior_record_stream_sha256: prior.receipt.record_stream_sha256,
        affected_stream: affected,
        affected_stream_sha256: affected_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: object_dir.clone(),
        attribution: attribution.clone(),
    })
    .expect("root-aware incremental build must succeed");
    let expected = full_build(
        &temporary.path,
        &tree,
        "root-delta-expected",
        &temporary.path.join("root-delta-expected-objects"),
        &attribution,
    );

    assert_eq!(incremental.receipt.roots, expected.receipt.roots);
    assert_eq!(
        incremental.receipt.record_stream_sha256,
        expected.receipt.record_stream_sha256
    );
    assert_eq!(
        incremental.receipt.entry_count,
        expected.receipt.entry_count
    );
    let materialized = materialize_record_stream(&incremental.root_manifest_path, &object_dir)
        .expect("incremental record stream must materialize");
    assert_eq!(
        read_records(&materialized),
        read_records(&expected.record_stream_path)
    );
}

#[test]
fn bounded_mutation_batches_match_complete_rebuild() {
    let temporary = Temporary::new("semantic-batched-incremental");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    let attribution = AttributionInput {
        actor_id: "actor".to_owned(),
        semantic_operation_id: "semantic-operation".to_owned(),
    };
    let object_dir = temporary.path.join("incremental-objects");
    let prior = full_build(
        &temporary.path,
        &tree,
        "batched-prior",
        &object_dir,
        &attribution,
    );
    for index in 0..512_u32 {
        std::fs::write(tree.join(format!("added-{index:04}")), b"payload")
            .expect("test operation must succeed");
    }
    let expected = full_build(
        &temporary.path,
        &tree,
        "batched-expected",
        &temporary.path.join("expected-objects"),
        &attribution,
    );
    let mutations = diff_streams(&prior.record_stream_path, &expected.record_stream_path);
    assert!(mutations.len() > 512);
    let affected = temporary.path.join("batched.records");
    let affected_sha256 =
        write_affected_stream(&affected, mutations).expect("test operation must succeed");
    assert!(
        std::fs::metadata(&affected)
            .expect("test operation must succeed")
            .len()
            > 32 * 1024
    );
    let incremental = build_incremental(&IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("batched-incremental"),
        prior_manifest: prior.root_manifest_path,
        expected_prior_roots: prior.receipt.roots,
        expected_prior_record_stream_sha256: prior.receipt.record_stream_sha256,
        affected_stream: affected,
        affected_stream_sha256: affected_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: object_dir.clone(),
        attribution,
    })
    .expect("batched incremental build must succeed");

    assert_eq!(incremental.receipt.roots, expected.receipt.roots);
    assert_eq!(
        incremental.receipt.record_stream_sha256,
        expected.receipt.record_stream_sha256
    );
    assert_eq!(
        incremental.receipt.entry_count,
        expected.receipt.entry_count
    );
    let materialized = materialize_record_stream(&incremental.root_manifest_path, &object_dir)
        .expect("incremental record stream must materialize");
    assert_eq!(
        read_records(&materialized),
        read_records(&expected.record_stream_path)
    );
}

#[test]
fn one_mebibyte_ten_file_delta_matches_complete_rebuild() {
    let temporary = Temporary::new("semantic-one-mebibyte-delta");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    for index in 0..256_u32 {
        std::fs::write(tree.join(format!("base-{index:04}")), b"base")
            .expect("test operation must succeed");
    }
    let attribution = AttributionInput {
        actor_id: "actor".to_owned(),
        semantic_operation_id: "semantic-operation".to_owned(),
    };
    let object_dir = temporary.path.join("incremental-objects");
    let prior = full_build(
        &temporary.path,
        &tree,
        "one-mebibyte-prior",
        &object_dir,
        &attribution,
    );
    for index in 0..10_usize {
        let bytes = 1024 * 1024 / 10 + usize::from(index < 1024 * 1024 % 10);
        std::fs::write(
            tree.join(format!("delta-{index:02}.bin")),
            vec![0_u8; bytes],
        )
        .expect("test operation must succeed");
    }
    let expected = full_build(
        &temporary.path,
        &tree,
        "one-mebibyte-expected",
        &temporary.path.join("expected-objects"),
        &attribution,
    );
    let mutations = diff_streams(&prior.record_stream_path, &expected.record_stream_path);
    let affected = temporary.path.join("one-mebibyte.records");
    let affected_sha256 =
        write_affected_stream(&affected, mutations).expect("test operation must succeed");
    let incremental = build_incremental(&IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("one-mebibyte-incremental"),
        prior_manifest: prior.root_manifest_path,
        expected_prior_roots: prior.receipt.roots,
        expected_prior_record_stream_sha256: prior.receipt.record_stream_sha256,
        affected_stream: affected,
        affected_stream_sha256: affected_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: object_dir.clone(),
        attribution,
    })
    .expect("test operation must succeed");

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
    assert!(incremental
        .receipt
        .phase_spans
        .iter()
        .any(|span| span.phase == "incremental-affected-stream-open"));
    assert!(incremental
        .receipt
        .phase_spans
        .iter()
        .any(|span| span.phase == "incremental-store-open"));
    let pack_indexes = std::fs::read_dir(object_dir.join("objects/packs"))
        .expect("incremental segment directory must exist")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "index")
        })
        .count();
    assert_eq!(
        pack_indexes, 1,
        "the bounded incremental transaction must install one immutable segment"
    );
    let materialized = materialize_record_stream(&incremental.root_manifest_path, &object_dir)
        .expect("incremental record stream must materialize");
    assert_eq!(
        read_records(&materialized),
        read_records(&expected.record_stream_path)
    );
}

#[test]
fn serial_full_tree_build_matches_threaded_canonical_semantics() {
    let temporary = Temporary::new("semantic-serial-full-tree");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    std::fs::create_dir(tree.join("nested")).expect("test operation must succeed");
    std::fs::write(tree.join("nested/small.txt"), b"holder namespace snapshot")
        .expect("test operation must succeed");
    let sparse = tree.join("sparse.bin");
    let mut sparse_file = File::create(&sparse).expect("test operation must succeed");
    sparse_file
        .set_len(512 * 1024)
        .expect("test operation must succeed");
    sparse_file
        .seek(SeekFrom::Start(384 * 1024))
        .expect("test operation must succeed");
    sparse_file
        .write_all(b"non-zero tail")
        .expect("test operation must succeed");
    std::fs::hard_link(&sparse, tree.join("sparse-alias.bin"))
        .expect("test operation must succeed");
    let attribution = AttributionInput {
        actor_id: "sandbox-runtime-publication".to_owned(),
        semantic_operation_id: "holder-namespace-semantic-snapshot".to_owned(),
    };
    let request = SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("threaded-full-tree"),
        allocation_id: AllocationId::from_string("serial-equivalence-allocation"),
        sealed_tree: tree.clone(),
        spool_dir: temporary.path.join("threaded-spool"),
        canonical_object_dir: temporary.path.join("threaded-objects"),
        attribution: attribution.clone(),
    };
    let threaded = build_with_output(&request).expect("threaded full build must succeed");
    let serial = build_with_output_serial(&SemanticBuildRequest {
        operation_id: OperationId::from_string("serial-full-tree"),
        spool_dir: temporary.path.join("serial-spool"),
        canonical_object_dir: temporary.path.join("serial-objects"),
        ..request
    })
    .expect("serial full build must succeed");

    assert_eq!(serial.receipt.roots, threaded.receipt.roots);
    assert_eq!(
        serial.receipt.record_stream_sha256,
        threaded.receipt.record_stream_sha256
    );
    assert_eq!(serial.receipt.entry_count, threaded.receipt.entry_count);
    assert_eq!(serial.receipt.bytes_read, threaded.receipt.bytes_read);
    assert_eq!(
        read_records(&serial.record_stream_path),
        read_records(&threaded.record_stream_path)
    );
    assert_eq!(serial.receipt.peak_data_workers, 1);
    assert_eq!(serial.resource_maxima.peak_data_workers, 1);
}

#[test]
fn prepared_fixture_semantic_lineage_survives_fresh_lifecycle_runs() {
    let temporary = Temporary::new("semantic-prepared-fixture-attribution");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    std::fs::write(tree.join("base"), b"base").expect("test operation must succeed");

    let fixture_attribution = AttributionInput {
        actor_id: "sandbox-runtime-publication".to_owned(),
        semantic_operation_id: "fixture-s4-chain-v2".to_owned(),
    };
    let fresh_attribution = AttributionInput {
        actor_id: "sandbox-runtime-publication".to_owned(),
        semantic_operation_id: "fresh-scorecard-run".to_owned(),
    };
    let fixture_object_dir = temporary.path.join("prepared-fixture-objects");
    let first_run_object_dir = temporary.path.join("first-run-objects");
    let second_run_object_dir = temporary.path.join("second-run-objects");
    let prior = full_build(
        &temporary.path,
        &tree,
        "prepared-fixture-prior",
        &fixture_object_dir,
        &fixture_attribution,
    );
    std::fs::write(tree.join("first-delta"), b"first").expect("test operation must succeed");
    let first_expected = full_build(
        &temporary.path,
        &tree,
        "prepared-fixture-first-expected",
        &temporary.path.join("first-expected-objects"),
        &fixture_attribution,
    );
    let first_affected = temporary.path.join("first.records");
    let first_affected_sha256 = write_affected_stream(
        &first_affected,
        diff_streams(
            &prior.record_stream_path,
            &first_expected.record_stream_path,
        ),
    )
    .expect("test operation must succeed");
    let first_request = IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("prepared-fixture-first"),
        prior_manifest: prior.root_manifest_path,
        expected_prior_roots: prior.receipt.roots,
        expected_prior_record_stream_sha256: prior.receipt.record_stream_sha256,
        affected_stream: first_affected,
        affected_stream_sha256: first_affected_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: first_run_object_dir.clone(),
        attribution: fixture_attribution.clone(),
    };
    let mut wrong_lineage = first_request.clone();
    wrong_lineage.attribution = fresh_attribution.clone();
    assert!(build_incremental(&wrong_lineage).is_err());

    let first = build_incremental(&first_request)
        .expect("fresh lifecycle accepts the sealed fixture semantic lineage");
    assert_eq!(first.receipt.roots, first_expected.receipt.roots);
    assert_eq!(
        first.receipt.durability.semantic_attribution,
        fixture_attribution
    );

    std::fs::write(tree.join("second-delta"), b"second").expect("test operation must succeed");
    let second_expected = full_build(
        &temporary.path,
        &tree,
        "prepared-fixture-second-expected",
        &temporary.path.join("second-expected-objects"),
        &fixture_attribution,
    );
    let first_materialized =
        materialize_record_stream(&first.root_manifest_path, &first_run_object_dir)
            .expect("first incremental stream must materialize");
    let second_affected = temporary.path.join("second.records");
    let second_affected_sha256 = write_affected_stream(
        &second_affected,
        diff_streams(&first_materialized, &second_expected.record_stream_path),
    )
    .expect("test operation must succeed");
    let second = build_incremental(&IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("prepared-fixture-second"),
        prior_manifest: first.root_manifest_path,
        expected_prior_roots: first.receipt.roots,
        expected_prior_record_stream_sha256: first.receipt.record_stream_sha256,
        affected_stream: second_affected,
        affected_stream_sha256: second_affected_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: second_run_object_dir.clone(),
        attribution: first.receipt.durability.semantic_attribution.clone(),
    })
    .expect("durable semantic lineage must carry to the next incremental publication");
    assert_eq!(second.receipt.roots, second_expected.receipt.roots);
    assert_eq!(
        second.receipt.durability.semantic_attribution,
        fixture_attribution
    );
    let second_materialized =
        materialize_record_stream(&second.root_manifest_path, &second_run_object_dir)
            .expect("separate lifecycle run stream must materialize through immutable sources");
    assert_eq!(
        read_records(&second_materialized),
        read_records(&second_expected.record_stream_path)
    );
}

#[test]
fn large_incremental_batch_matches_complete_rebuild_with_bounded_staging() {
    let temporary = Temporary::new("semantic-large-incremental-stage");
    let tree = temporary.path.join("tree");
    std::fs::create_dir(&tree).expect("test operation must succeed");
    let attribution = AttributionInput {
        actor_id: "actor".to_owned(),
        semantic_operation_id: "semantic-operation".to_owned(),
    };
    let object_dir = temporary.path.join("incremental-objects");
    let prior = full_build(
        &temporary.path,
        &tree,
        "large-stage-prior",
        &object_dir,
        &attribution,
    );
    for index in 0..4_096_u32 {
        std::fs::write(
            tree.join(format!("added-{index:04}")),
            format!("payload-{index:04}"),
        )
        .expect("test operation must succeed");
    }
    let expected = full_build(
        &temporary.path,
        &tree,
        "large-stage-expected",
        &temporary.path.join("expected-objects"),
        &attribution,
    );
    let mutations = diff_streams(&prior.record_stream_path, &expected.record_stream_path);
    assert!(mutations.len() > 4_096);
    let affected = temporary.path.join("large-stage.records");
    let affected_sha256 =
        write_affected_stream(&affected, mutations).expect("test operation must succeed");
    let incremental = build_incremental(&IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("large-stage-incremental"),
        prior_manifest: prior.root_manifest_path,
        expected_prior_roots: prior.receipt.roots,
        expected_prior_record_stream_sha256: prior.receipt.record_stream_sha256,
        affected_stream: affected,
        affected_stream_sha256: affected_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: object_dir.clone(),
        attribution,
    })
    .expect("large incremental stage must remain exact with bounded staging");

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
    let materialized = materialize_record_stream(&incremental.root_manifest_path, &object_dir)
        .expect("incremental record stream must materialize");
    assert_eq!(
        read_records(&materialized),
        read_records(&expected.record_stream_path)
    );
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
