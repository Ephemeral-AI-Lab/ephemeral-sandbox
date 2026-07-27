use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;

use sandbox_runtime_mpla_poc::docker_protocol::DockerResponse;
use sandbox_runtime_mpla_poc::evidence::{capture_physical_snapshot, read_json, write_atomic_json};
use sandbox_runtime_mpla_poc::qualify;
use sandbox_runtime_mpla_poc::{
    AllocationId, ArtifactStatus, PocConfig, ProbeStatus, QualificationReceipt,
    QualificationRequest, RunId, SCHEMA_VERSION,
};
use serde_json::{json, Value};
use uuid::Uuid;

struct ScopedTemp {
    path: PathBuf,
}

impl ScopedTemp {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mpla-poc-{label}-{}", Uuid::new_v4()));
        fs::create_dir(&path).expect("create scoped test directory");
        Self { path }
    }
}

impl Drop for ScopedTemp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn atomic_json_replaces_and_round_trips_without_temporary_files() {
    let directory = ScopedTemp::new("atomic-evidence");
    let artifact = directory.path.join("qualification.json");

    write_atomic_json(&artifact, &json!({"schema_version": 1, "sequence": 1}))
        .expect("write first artifact");
    write_atomic_json(&artifact, &json!({"schema_version": 1, "sequence": 2}))
        .expect("replace artifact");

    let decoded: Value = read_json(&artifact).expect("read installed artifact");
    assert_eq!(decoded, json!({"schema_version": 1, "sequence": 2}));
    assert_eq!(
        fs::read_dir(&directory.path)
            .expect("read evidence directory")
            .count(),
        1,
        "the atomic writer must not leave temporary files"
    );
}

#[test]
fn docker_records_preserve_explicit_failed_and_cancelled_statuses() {
    let directory = ScopedTemp::new("docker-records");
    let failed_path = directory.path.join("failed.json");
    let cancelled_path = directory.path.join("cancelled.json");
    let failed = DockerResponse::failed("mandatory OverlayFS probe failed");
    let cancelled = DockerResponse::cancelled("qualification lease expired");

    failed
        .write_atomic(&failed_path)
        .expect("write failed Docker record");
    cancelled
        .write_atomic(&cancelled_path)
        .expect("write cancelled Docker record");

    assert_eq!(failed.status(), ArtifactStatus::Failed);
    assert_eq!(cancelled.status(), ArtifactStatus::Cancelled);
    assert_eq!(
        read_json::<DockerResponse>(&failed_path).expect("read failed record"),
        failed
    );
    assert_eq!(
        DockerResponse::decode_line(
            &cancelled
                .encode_line()
                .expect("encode cancelled Docker record")
        )
        .expect("decode cancelled Docker record"),
        cancelled
    );
}

#[test]
fn physical_snapshot_carries_device_inode_and_block_accounting() {
    let directory = ScopedTemp::new("physical-snapshot");
    let allocation_root = directory.path.join("allocation");
    let nested = allocation_root.join("upper");
    fs::create_dir_all(&nested).expect("create allocation directories");
    fs::write(allocation_root.join("sentinel"), b"stationary\n").expect("write root sentinel");
    fs::write(nested.join("payload"), vec![0x5a; 8_192]).expect("write payload");
    let allocation_id = AllocationId::from_string("allocation-snapshot-test");

    let snapshot =
        capture_physical_snapshot(&allocation_id, &allocation_root).expect("capture snapshot");

    assert_eq!(snapshot.allocation_id, allocation_id);
    assert_eq!(snapshot.allocation_path, allocation_root);
    assert_ne!(snapshot.device, 0);
    assert_eq!(snapshot.logical_bytes, 8_203);
    assert_eq!(snapshot.file_count, 2);
    assert_eq!(snapshot.directory_count, 2);
    assert_eq!(snapshot.inode_count, 4);
    assert!(snapshot.allocated_bytes >= snapshot.logical_bytes);
    assert_eq!(snapshot.representative_inodes.len(), 4);
    assert!(snapshot
        .representative_inodes
        .iter()
        .all(|witness| witness.device == snapshot.device && witness.inode != 0));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn qualification_fails_closed_and_writes_a_durable_receipt_off_linux() {
    let directory = ScopedTemp::new("fail-closed");
    let request = QualificationRequest {
        schema_version: SCHEMA_VERSION,
        run_id: RunId::parse("m0-fail-closed-test").expect("valid run ID"),
        allocation_id: AllocationId::from_string("allocation-fail-closed-test"),
        payload_root: directory.path.join("payload"),
        control_root: directory.path.join("control"),
        fixtures_root: directory.path.join("fixtures"),
        evidence_root: directory.path.join("evidence"),
        lower_dir: directory.path.join("fixtures/lower"),
        allocation_root: directory
            .path
            .join("payload/allocations/allocation-fail-closed-test"),
        workspace_root: directory
            .path
            .join("control/sessions/allocation-fail-closed-test"),
    };

    let receipt = qualify::qualify(&PocConfig::default(), &request).expect("write failed receipt");

    assert_eq!(receipt.status, ArtifactStatus::Failed);
    assert!(receipt.probes.iter().any(|probe| {
        probe.name == "linux_runtime" && probe.mandatory && probe.status == ProbeStatus::Failed
    }));
    assert_eq!(
        read_json::<QualificationReceipt>(&receipt.artifact_path).expect("read failed receipt"),
        receipt
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the lead-issued Q0/SM-01 Docker execution lease"]
fn sm_01_qualifies_real_stationary_overlay_and_durable_receipt() {
    let run_id = required_env("MPLA_POC_RUN_ID");
    let allocation_id = required_env("MPLA_POC_ALLOCATION_ID");
    let payload_root = PathBuf::from(required_env("MPLA_POC_PAYLOAD_ROOT"));
    let control_root = PathBuf::from(required_env("MPLA_POC_CONTROL_ROOT"));
    let fixtures_root = PathBuf::from(required_env("MPLA_POC_FIXTURES_ROOT"));
    let evidence_root = PathBuf::from(required_env("MPLA_POC_EVIDENCE_ROOT"));
    let lower_dir = fixtures_root.join("lower");
    prepare_sm01_fixture(&lower_dir);
    let allocation_root = payload_root.join("allocations").join(&allocation_id);
    let workspace_root = control_root.join("sessions").join(&allocation_id);
    let request = QualificationRequest {
        schema_version: SCHEMA_VERSION,
        run_id: RunId::parse(run_id).expect("valid run ID"),
        allocation_id: AllocationId::from_string(allocation_id),
        payload_root,
        control_root,
        fixtures_root,
        evidence_root: evidence_root.clone(),
        lower_dir,
        allocation_root,
        workspace_root,
    };

    let receipt = qualify::qualify(&PocConfig::default(), &request).expect("run qualification");

    assert_eq!(receipt.status, ArtifactStatus::Passed);
    assert!(receipt
        .probes
        .iter()
        .all(|probe| !probe.mandatory || probe.status == ProbeStatus::Passed));
    assert_ne!(
        receipt.environment.payload_mount_id,
        receipt.environment.control_mount_id
    );
    assert_stable_sentinel(&receipt);
    let installed: QualificationReceipt =
        read_json(&receipt.artifact_path).expect("read durable qualification receipt");
    assert_eq!(installed, receipt);
    assert!(receipt
        .artifact_path
        .starts_with(evidence_root.join("environment")));
}

#[cfg(target_os = "linux")]
fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for leased SM-01"))
}

#[cfg(target_os = "linux")]
fn prepare_sm01_fixture(lower_dir: &Path) {
    let opaque_dir = lower_dir.join("opaque-dir");
    fs::create_dir_all(&opaque_dir).expect("create lower fixture");
    fs::write(lower_dir.join("whiteout-target"), b"remove me\n").expect("write whiteout fixture");
    fs::write(opaque_dir.join("lower-entry"), b"hide me\n").expect("write opaque fixture");
}

#[cfg(target_os = "linux")]
fn assert_stable_sentinel(receipt: &QualificationReceipt) {
    let sentinel = Path::new("upper").join("mpla-stable-sentinel");
    let before = receipt
        .before
        .representative_inodes
        .iter()
        .find(|witness| witness.relative_path == sentinel)
        .expect("before sentinel witness");
    let after = receipt
        .after
        .representative_inodes
        .iter()
        .find(|witness| witness.relative_path == sentinel)
        .expect("after sentinel witness");
    assert_eq!((before.device, before.inode), (after.device, after.inode));
}
