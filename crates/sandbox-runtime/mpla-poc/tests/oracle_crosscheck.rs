use std::fs::{File, OpenOptions};
use std::io::BufReader;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, FileExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use rustix::fs::{AtFlags, Timespec, Timestamps, XattrFlags, CWD};
#[cfg(target_os = "linux")]
use sandbox_runtime_mpla_poc::semantic::allocation::is_fully_allocated;
use sandbox_runtime_mpla_poc::semantic::record::{RecordStreamReader, SemanticRecord};
use sandbox_runtime_mpla_poc::semantic::{build_with_output, SemanticBuildOutput};
use sandbox_runtime_mpla_poc::{
    AllocationId, AttributionInput, OperationId, SemanticBuildRequest, SCHEMA_VERSION,
};
use serde_json::Value;
use uuid::Uuid;

#[test]
fn separately_compiled_oracle_matches_decoded_records_roots_and_physical_substitution() {
    let temporary = Temporary::new("semantic-oracle");
    let source_tree = temporary.path.join("source-tree");
    create_fixture(&source_tree);
    let attribution = AttributionInput {
        actor_id: "oracle-actor".to_owned(),
        semantic_operation_id: "oracle-semantic-operation".to_owned(),
    };

    let candidate = candidate_build(
        &temporary.path,
        &source_tree,
        "source-allocation",
        "source-operation",
        "source",
        &attribution,
    );
    let oracle = run_oracle(
        &source_tree,
        &temporary.path.join("source-oracle.records"),
        &attribution,
    );
    compare_candidate_and_oracle(&candidate, &oracle);
    assert!(!decoded_records(&candidate.record_stream_path)
        .iter()
        .any(|record| matches!(record, SemanticRecord::Xattr { name, .. }
            if name == b"user.overlay.uuid")));

    let substitute_tree = temporary.path.join("substituted-tree");
    reconstruct_fixture(&source_tree, &substitute_tree);
    let source_metadata = std::fs::symlink_metadata(source_tree.join("regular"))
        .expect("test operation must succeed");
    let substitute_metadata = std::fs::symlink_metadata(substitute_tree.join("regular"))
        .expect("test operation must succeed");
    assert_ne!(
        (source_metadata.dev(), source_metadata.ino()),
        (substitute_metadata.dev(), substitute_metadata.ino())
    );

    let substituted = candidate_build(
        &temporary.path,
        &substitute_tree,
        "substituted-allocation",
        "substituted-operation",
        "substituted",
        &attribution,
    );
    assert_eq!(candidate.receipt.roots, substituted.receipt.roots);
    assert_eq!(
        decoded_records(&candidate.record_stream_path),
        decoded_records(&substituted.record_stream_path)
    );
    let substituted_oracle = run_oracle(
        &substitute_tree,
        &temporary.path.join("substituted-oracle.records"),
        &attribution,
    );
    compare_candidate_and_oracle(&substituted, &substituted_oracle);
}

fn compare_candidate_and_oracle(candidate: &SemanticBuildOutput, oracle: &OracleRun) {
    assert_eq!(
        candidate.receipt.roots.root_id.as_str(),
        oracle.summary["root_id"]
            .as_str()
            .expect("test operation must succeed")
    );
    assert_eq!(
        candidate.receipt.roots.attribution_root_id.as_str(),
        oracle.summary["attribution_root_id"]
            .as_str()
            .expect("test operation must succeed")
    );
    assert_eq!(
        candidate.receipt.record_stream_sha256,
        oracle.summary["record_stream_sha256"]
            .as_str()
            .expect("test operation must succeed")
    );
    assert_eq!(
        candidate.receipt.entry_count,
        oracle.summary["record_count"]
            .as_u64()
            .expect("test operation must succeed")
    );
    assert_eq!(
        decoded_records(&candidate.record_stream_path),
        decoded_records(&oracle.records)
    );
    assert_eq!(
        std::fs::read(&candidate.record_stream_path).expect("test operation must succeed"),
        std::fs::read(&oracle.records).expect("test operation must succeed")
    );
    assert!(
        oracle.summary["peak_managed_bytes"]
            .as_u64()
            .expect("test operation must succeed")
            <= 8 * 1024 * 1024
    );
    assert!(
        oracle.summary["peak_open_data_fds"]
            .as_u64()
            .expect("test operation must succeed")
            <= 16
    );
}

fn candidate_build(
    root: &Path,
    tree: &Path,
    allocation: &str,
    operation: &str,
    label: &str,
    attribution: &AttributionInput,
) -> SemanticBuildOutput {
    build_with_output(&SemanticBuildRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string(operation),
        allocation_id: AllocationId::from_string(allocation),
        sealed_tree: tree.to_path_buf(),
        spool_dir: root.join(format!("{label}-spool")),
        canonical_object_dir: root.join(format!("{label}-objects")),
        attribution: attribution.clone(),
    })
    .expect("test operation must succeed")
}

fn run_oracle(tree: &Path, records: &Path, attribution: &AttributionInput) -> OracleRun {
    let output = Command::new(env!("CARGO_BIN_EXE_mpla-poc-oracle"))
        .arg("--tree")
        .arg(tree)
        .arg("--records")
        .arg(records)
        .arg("--actor-id")
        .arg(&attribution.actor_id)
        .arg("--semantic-operation-id")
        .arg(&attribution.semantic_operation_id)
        .output()
        .expect("test operation must succeed");
    assert!(
        output.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    OracleRun {
        records: records.to_path_buf(),
        summary: serde_json::from_slice(&output.stdout).expect("test operation must succeed"),
    }
}

fn create_fixture(tree: &Path) {
    std::fs::create_dir(tree).expect("test operation must succeed");
    rustix::fs::lsetxattr(
        tree,
        "user.overlay.uuid",
        b"physical-overlay-identity",
        XattrFlags::empty(),
    )
    .expect("test operation must succeed");
    let regular = tree.join("regular");
    std::fs::write(&regular, b"oracle payload").expect("test operation must succeed");
    std::fs::set_permissions(&regular, std::fs::Permissions::from_mode(0o640))
        .expect("test operation must succeed");
    rustix::fs::lsetxattr(&regular, "user.mpla", b"oracle", XattrFlags::empty())
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
        .write_all_at(b"oracle-delta", 1024 * 1024)
        .expect("test operation must succeed");
    sparse.sync_all().expect("test operation must succeed");

    #[cfg(target_os = "linux")]
    create_fully_allocated_zero_file(&tree.join("dense-zero"));

    std::fs::write(tree.join(".wh.deleted"), b"").expect("test operation must succeed");
    let opaque = tree.join("opaque");
    std::fs::create_dir(&opaque).expect("test operation must succeed");
    std::fs::write(opaque.join(".wh..wh..opq"), b"").expect("test operation must succeed");
    std::fs::write(opaque.join("visible"), b"visible").expect("test operation must succeed");
}

#[cfg(target_os = "linux")]
fn create_fully_allocated_zero_file(path: &Path) {
    const DENSE_ZERO_BYTES: i64 = 1024 * 1024;
    let file = File::create(path).expect("test operation must succeed");
    // SAFETY: the open regular file descriptor and fixed positive range are
    // valid for Linux `fallocate`.
    let result = unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, DENSE_ZERO_BYTES) };
    assert_eq!(
        result, 0,
        "test filesystem must support fully allocated zero files"
    );
    file.sync_all().expect("test operation must succeed");
    assert!(
        is_fully_allocated(
            &file,
            path,
            u64::try_from(DENSE_ZERO_BYTES).expect("test byte count fits"),
        )
        .expect("test allocation probe must succeed"),
        "the shared allocation probe must recognize a fully allocated zero file"
    );
}

fn reconstruct_fixture(source: &Path, destination: &Path) {
    let status = Command::new("/bin/cp")
        .arg("-a")
        .arg(source)
        .arg(destination)
        .status()
        .expect("test operation must succeed");
    assert!(status.success());

    std::fs::remove_file(destination.join("regular-hardlink"))
        .expect("test operation must succeed");
    std::fs::hard_link(
        destination.join("regular"),
        destination.join("regular-hardlink"),
    )
    .expect("test operation must succeed");
    restore_times(source, destination);
}

fn restore_times(source: &Path, destination: &Path) {
    let metadata = std::fs::symlink_metadata(source).expect("test operation must succeed");
    let timestamps = Timestamps {
        last_access: Timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        last_modification: Timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    };
    rustix::fs::utimensat(CWD, destination, &timestamps, AtFlags::empty())
        .expect("test operation must succeed");
}

fn decoded_records(path: &Path) -> Vec<SemanticRecord> {
    let mut reader = RecordStreamReader::new(BufReader::new(
        File::open(path).expect("test operation must succeed"),
    ));
    let mut records = Vec::new();
    while let Some(record) = reader.next_record().expect("test operation must succeed") {
        records.push(record);
    }
    records
}

struct OracleRun {
    records: PathBuf,
    summary: Value,
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
