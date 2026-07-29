#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use sandbox_runtime_mpla_poc::allocation::create_allocation;
use sandbox_runtime_mpla_poc::semantic::{
    build_incremental, capture_affected_paths, write_affected_stream_from_snapshots,
    IncrementalBuildRequest,
};
use sandbox_runtime_mpla_poc::{AttributionInput, OperationId, RunId, SCHEMA_VERSION};

#[allow(dead_code)]
#[path = "../src/bin/mpla-speed-poc-v1.rs"]
mod speed_poc;

static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(name: &str) -> Self {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("workspace root");
        let path = workspace
            .join("target")
            .join("mpla-poc-tree-usage-tests")
            .join(format!(
                "{name}-{}-{}",
                std::process::id(),
                NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).expect("create exact test root");
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn tree_usage_survives_concurrent_file_disappearance() {
    let root = TestRoot::new("concurrent-disappearance");
    let churn = root.path.join("churn");
    fs::create_dir(&churn).expect("create churn directory");
    let paths = (0..1_024)
        .map(|index| churn.join(format!("entry-{index:04}")))
        .collect::<Vec<_>>();
    for path in &paths {
        fs::write(path, b"x").expect("seed churn entry");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(2));
    let worker_stop = Arc::clone(&stop);
    let worker_start = Arc::clone(&start);
    let worker_paths = paths.clone();
    let worker = thread::spawn(move || {
        worker_start.wait();
        while !worker_stop.load(Ordering::Acquire) {
            for (index, path) in worker_paths.iter().enumerate() {
                let _ = fs::remove_file(path);
                if index % 32 == 0 {
                    thread::yield_now();
                }
            }
            for (index, path) in worker_paths.iter().enumerate() {
                let _ = fs::write(path, b"x");
                if index % 32 == 0 {
                    thread::yield_now();
                }
            }
        }
    });

    start.wait();
    let mut failure = None;
    for _ in 0..128 {
        if let Err(error) = speed_poc::tree_usage(&root.path) {
            failure = Some(error.to_string());
            break;
        }
    }
    stop.store(true, Ordering::Release);
    worker.join().expect("join churn worker");

    assert!(
        failure.is_none(),
        "tree walk rejected expected concurrent disappearance: {failure:?}"
    );
}

#[test]
fn tree_usage_retains_non_not_found_error_context() {
    let root = TestRoot::new("error-context");
    let blocking_file = root.path.join("not-a-directory");
    fs::write(&blocking_file, b"x").expect("write blocking path component");
    let inaccessible = blocking_file.join("child");

    let error = speed_poc::tree_usage(&inaccessible).expect_err("non-NotFound error must fail");
    let message = error.to_string();

    assert!(message.contains("check tree root"));
    assert!(message.contains(&inaccessible.display().to_string()));
}

#[test]
fn benchmark_prior_accepts_candidate_attribution_during_incremental_build() {
    let root = TestRoot::new("candidate-attribution");
    let pair_root = root.path.join("pair");
    fs::create_dir(&pair_root).expect("create pair root");
    let prepare_operation = OperationId::from_string("candidate-prepare");
    let allocation =
        create_allocation(&pair_root.join("arena"), &prepare_operation).expect("create allocation");
    let changed_path = PathBuf::from("delta.bin");
    let source_path = allocation.upper_dir.join(&changed_path);
    fs::write(&source_path, []).expect("create empty affected file");
    fs::File::open(&source_path)
        .expect("open empty affected file")
        .sync_all()
        .expect("sync empty affected file");
    fs::File::open(&allocation.upper_dir)
        .expect("open allocation upper directory")
        .sync_all()
        .expect("sync allocation upper directory");

    let run_id = RunId::parse("candidate-attribution-run").expect("parse run ID");
    let candidate_operation = OperationId::from_string(format!("{}-p1-candidate", run_id.as_str()));
    let canonical = pair_root.join("canonical");
    let prior = speed_poc::build_full_prior(
        &run_id,
        1,
        &allocation,
        &pair_root,
        &canonical,
        &candidate_operation,
    )
    .expect("build prior with candidate attribution");

    let affected_paths = vec![changed_path];
    let before = capture_affected_paths(
        &allocation.upper_dir,
        &affected_paths,
        &pair_root.join("before"),
    )
    .expect("capture before state");
    fs::write(&source_path, b"x").expect("write affected file");
    fs::File::open(&source_path)
        .expect("open affected file")
        .sync_all()
        .expect("sync affected file");
    let after = capture_affected_paths(
        &allocation.upper_dir,
        &affected_paths,
        &pair_root.join("after"),
    )
    .expect("capture after state");
    let affected_stream = pair_root.join("affected.records");
    let affected_stream_sha256 =
        write_affected_stream_from_snapshots(&affected_stream, &before, &after)
            .expect("write affected stream");

    let incremental = build_incremental(&IncrementalBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: candidate_operation.clone(),
        prior_manifest: prior.root_manifest_path,
        expected_prior_roots: prior.receipt.roots,
        expected_prior_record_stream_sha256: prior.receipt.record_stream_sha256,
        affected_stream,
        affected_stream_sha256,
        affected_ranges_complete: true,
        canonical_object_dir: canonical,
        attribution: AttributionInput {
            actor_id: "mpla-speed-poc-v1".to_owned(),
            semantic_operation_id: candidate_operation.as_str().to_owned(),
        },
    })
    .expect("incremental build accepts prior candidate attribution");

    assert!(incremental.affected_record_count >= 1);
    assert_eq!(incremental.immutable_payload_bytes_read, 0);
}
