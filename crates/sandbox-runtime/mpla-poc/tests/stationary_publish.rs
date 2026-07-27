#[cfg(target_os = "linux")]
use sandbox_runtime_mpla_poc::{
    allocation::create_allocation,
    durable,
    inventory::capture_stable_pair,
    lease::{issue_workspace_lease, validate_deleter, validate_writer},
    owner::{compare_and_adopt, current_owner},
    publication::stationary_adopt,
    MplaSession, OwnerSubject, OwnerTransitionRequest, SessionId, StableAllocationReceipt,
};
use sandbox_runtime_mpla_poc::{
    FaultInjector, FaultPoint, OperationId, PocError, PublicationId, StationaryPublicationRequest,
    SCHEMA_VERSION,
};
#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::process::ExitStatusExt;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[test]
fn stationary_publication_scope_round_trips_without_physical_identity() {
    let request = StationaryPublicationRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("operation"),
        publication_id: PublicationId::from_string("publication"),
    };
    let json = serde_json::to_string(&request).expect("serialize request");
    assert!(!json.contains("allocation_path"));
    assert!(!json.contains("inode"));
    assert_eq!(
        serde_json::from_str::<StationaryPublicationRequest>(&json).expect("deserialize request"),
        request
    );
}

#[test]
fn deterministic_faults_respect_the_terminal_sealing_boundary() {
    let mut before = FaultInjector::armed([FaultPoint::BeforeSealing]);
    assert!(matches!(
        before
            .hit(FaultPoint::BeforeSealing, false)
            .expect_err("pre-Sealing fault"),
        PocError::Integrity(_)
    ));

    let mut after = FaultInjector::armed([FaultPoint::AfterSealingDurable]);
    assert!(matches!(
        after
            .hit(FaultPoint::AfterSealingDurable, true)
            .expect_err("post-Sealing fault"),
        PocError::RecoveryRequired(_)
    ));
}

/// The M0 fail-fast physical path. This test is compiled into the static Linux
/// test binary and is run only by the lead under the single Docker execution
/// lease.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the lead-issued M0 Docker/OverlayFS execution lease"]
fn m0_stationary_lifecycle_keeps_one_allocation_and_fences_the_old_session() {
    let payload_root = required_path("MPLA_POC_PAYLOAD_ROOT");
    let control_root = required_path("MPLA_POC_CONTROL_ROOT");
    let evidence_root = required_path("MPLA_POC_EVIDENCE_ROOT");
    let arena_root = payload_root.join("allocations");
    let lower_dir = control_root.join("fixtures").join("m0-lower");
    fs::create_dir_all(&lower_dir).expect("create M0 lower");
    fs::write(lower_dir.join("base-sentinel"), b"m0-lower\n").expect("write lower sentinel");

    let allocation_operation = OperationId::new();
    let before_create = allocation_descriptors(&arena_root);
    let allocation =
        create_allocation(&arena_root, &allocation_operation).expect("create allocation");
    let after_create = allocation_descriptors(&arena_root);
    assert_eq!(after_create.len(), before_create.len() + 1);
    assert!(after_create.contains(&allocation.allocation_root));

    let session_id = SessionId::new();
    let lease_operation = OperationId::new();
    let lease = issue_workspace_lease(&allocation, session_id, &lease_operation)
        .expect("issue workspace lease");
    validate_writer(&allocation.allocation_root, &lease.writer).expect("current writer");
    validate_deleter(&allocation.allocation_root, &lease.deleter).expect("current deleter");

    let cgroup_procs_path = std::env::var_os("MPLA_POC_CGROUP_PROCS").map(PathBuf::from);
    let mut session = MplaSession::open(
        &control_root,
        allocation.clone(),
        lease.clone(),
        vec![lower_dir],
        cgroup_procs_path,
    )
    .expect("mount permanent allocation");
    let test_executable = std::env::current_exe().expect("current test executable");
    let populate = session
        .execute(
            &lease.writer,
            &test_executable,
            &[
                "--exact".to_owned(),
                "m0_child_populates_s1_and_spawns_holder".to_owned(),
                "--ignored".to_owned(),
                "--nocapture".to_owned(),
            ],
            Duration::from_secs(30),
        )
        .expect("populate S1 through the mounted workspace");
    assert!(populate.success, "fixture child failed: {populate:?}");
    let holder_pid: i32 = fs::read_to_string(
        session
            .workspace_root()
            .expect("mounted workspace")
            .join(".holder-pid"),
    )
    .expect("read holder PID")
    .trim()
    .parse()
    .expect("parse holder PID");
    assert!(
        Path::new("/proc").join(holder_pid.to_string()).exists(),
        "fixture holder must be live before Sealing"
    );

    let request = StationaryPublicationRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::new(),
        publication_id: PublicationId::new(),
    };
    let started = Instant::now();
    let receipt = stationary_adopt(
        &mut session,
        &request,
        &control_root.join("operations"),
        &mut FaultInjector::default(),
    )
    .expect("stationary adoption");
    let elapsed = started.elapsed();

    assert_eq!(receipt.allocation_path_before, allocation.allocation_root);
    assert_eq!(receipt.allocation_path_after, allocation.allocation_root);
    assert!(receipt.representative_inodes_unchanged);
    assert!(receipt.allocated_bytes_unchanged);
    assert!(receipt.no_second_payload_allocation);
    assert!(receipt.stale_writer_rejected);
    assert!(receipt.stale_deleter_rejected);
    assert!(receipt.quiescence.pre_unmount_audit.is_clear());
    assert!(receipt.quiescence.post_unmount_audit.is_clear());
    assert!(
        !Path::new("/proc").join(holder_pid.to_string()).exists(),
        "holder survived terminal process-tree drain"
    );
    assert_eq!(
        allocation_descriptors(&arena_root),
        after_create,
        "stationary adoption created a second payload allocation"
    );
    assert!(matches!(
        session.execute(&lease.writer, &test_executable, &[], Duration::from_secs(1)),
        Err(PocError::StaleCapability { .. })
    ));
    assert!(matches!(
        validate_writer(&allocation.allocation_root, &lease.writer),
        Err(PocError::StaleCapability { .. })
    ));
    assert!(matches!(
        validate_deleter(&allocation.allocation_root, &lease.deleter),
        Err(PocError::StaleCapability { .. })
    ));
    let owner = current_owner(&allocation.allocation_root).expect("read adopted owner");
    assert_eq!(owner.owner_epoch, lease.owner_epoch + 1);
    assert!(matches!(
        &owner.subject,
        OwnerSubject::PayloadOwned {
            publication_id
        } if publication_id == &request.publication_id
    ));

    let artifact = evidence_root
        .join("cases")
        .join("M0")
        .join("stationary-lifecycle.json");
    durable::replace_json(
        &artifact,
        &serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "case": "M0-stationary-lifecycle",
            "status": "passed",
            "elapsed_ns": elapsed.as_nanos().to_string(),
            "allocation_id": allocation.descriptor.allocation_id,
            "allocation_root": allocation.allocation_root,
            "allocation_set_before_create": before_create,
            "allocation_set_after_create": after_create,
            "command": populate,
            "holder_pid": holder_pid,
            "post_sealing_resume_rejected": true,
            "receipt": receipt,
            "selected_owner": owner,
        }),
    )
    .expect("write M0 stationary lifecycle evidence");
}

/// Real SIGKILL boundaries around owner selector installation. The child is
/// killed only after the durable owner routine reports the injected boundary;
/// the parent then replays the same operation and requires one exact owner.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the lead-issued M0 Docker/process-crash execution lease"]
fn m0_owner_selector_sigkill_edges_recover_one_exact_owner() {
    let payload_root = required_path("MPLA_POC_PAYLOAD_ROOT");
    let control_root = required_path("MPLA_POC_CONTROL_ROOT");
    let evidence_root = required_path("MPLA_POC_EVIDENCE_ROOT");
    let arena_root = payload_root.join("allocations");
    let mut edge_artifacts = Vec::new();

    for (edge, marker) in [
        (
            "before-selector-replace",
            ".fault-before-owner-selector-replace",
        ),
        (
            "after-selector-replace",
            ".fault-after-owner-selector-replace",
        ),
    ] {
        let allocation =
            create_allocation(&arena_root, &OperationId::new()).expect("create crash allocation");
        let lease = issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new())
            .expect("issue crash lease");
        let payload_path = allocation.upper_dir.join("owner-crash-sentinel");
        let mut payload = File::create(&payload_path).expect("create crash sentinel");
        payload
            .write_all(edge.as_bytes())
            .expect("write crash sentinel");
        payload.sync_all().expect("fsync crash sentinel");
        let (first, second) = capture_stable_pair(&allocation).expect("stable crash inventory");
        let operation_id = OperationId::new();
        let request = OwnerTransitionRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            publication_id: PublicationId::new(),
            session_id: lease.session_id.clone(),
            allocation_id: allocation.descriptor.allocation_id.clone(),
            expected_lease_epoch: lease.lease_epoch,
            expected_owner_epoch: lease.owner_epoch,
        };
        let stable = StableAllocationReceipt {
            schema_version: SCHEMA_VERSION,
            operation_id,
            allocation: allocation.descriptor.clone(),
            expected_owner_epoch: lease.owner_epoch,
            before: first.physical,
            after: second.physical,
            sync_completed: true,
        };
        let edge_root = control_root.join("owner-crash").join(edge);
        let stable_path = edge_root.join("stable.json");
        let request_path = edge_root.join("request.json");
        let observed_path = edge_root.join("child-fault-observed.json");
        durable::replace_json(&stable_path, &stable).expect("persist crash stable receipt");
        durable::replace_json(&request_path, &request).expect("persist crash request");
        let marker_path = allocation.owner_dir.join(marker);
        let marker_file = File::create(&marker_path).expect("create owner fault marker");
        marker_file.sync_all().expect("fsync owner fault marker");

        let status = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                "m0_child_hits_owner_fault_then_sigkills",
                "--ignored",
                "--nocapture",
            ])
            .env("MPLA_OWNER_CRASH_ALLOCATION", &allocation.allocation_root)
            .env("MPLA_OWNER_CRASH_STABLE", &stable_path)
            .env("MPLA_OWNER_CRASH_REQUEST", &request_path)
            .env("MPLA_OWNER_CRASH_OBSERVED", &observed_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run owner crash child");
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "owner fault child was not SIGKILLed at {edge}: {status:?}"
        );
        let observed: serde_json::Value =
            durable::read_json(&observed_path).expect("read child fault witness");
        assert_eq!(observed["fault_observed"], true);
        fs::remove_file(&marker_path).expect("remove owner fault marker");

        let recovered = compare_and_adopt(&allocation.allocation_root, &stable, &request)
            .expect("replay owner adoption");
        let replay = compare_and_adopt(&allocation.allocation_root, &stable, &request)
            .expect("idempotent owner replay");
        assert!(replay.idempotent_replay);
        assert_eq!(recovered.new_owner, replay.new_owner);
        assert_eq!(
            current_owner(&allocation.allocation_root).expect("selected crash owner"),
            recovered.new_owner
        );
        assert!(matches!(
            validate_writer(&allocation.allocation_root, &lease.writer),
            Err(PocError::StaleCapability { .. })
        ));
        assert!(matches!(
            validate_deleter(&allocation.allocation_root, &lease.deleter),
            Err(PocError::StaleCapability { .. })
        ));
        assert_eq!(
            fs::read_dir(allocation.owner_dir.join("generations"))
                .expect("read owner generations")
                .filter_map(Result::ok)
                .count(),
            2,
            "owner recovery produced extra generations"
        );
        edge_artifacts.push(serde_json::json!({
            "edge": edge,
            "allocation_id": allocation.descriptor.allocation_id,
            "allocation_root": allocation.allocation_root,
            "child_signal": libc::SIGKILL,
            "fault_observed": observed,
            "recovered_receipt": recovered,
            "idempotent_replay": replay,
        }));
    }

    durable::replace_json(
        &evidence_root
            .join("cases")
            .join("M0")
            .join("owner-selector-sigkill.json"),
        &serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "case": "M0-owner-selector-sigkill",
            "status": "passed",
            "edges": edge_artifacts,
        }),
    )
    .expect("write owner SIGKILL evidence");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "M0 physical helper; invoked only as a managed child"]
fn m0_child_populates_s1_and_spawns_holder() {
    let workspace = std::env::current_dir().expect("child workspace");
    let bytes_total = 128_usize * 1024 * 1024;
    let files_total = 10_000_usize;
    let bytes_per_file = bytes_total / files_total;
    let remainder = bytes_total % files_total;
    let mut buffer = vec![0_u8; bytes_per_file + 1];
    for index in 0..files_total {
        let directory = workspace.join(format!("src/{:03}", index / 100));
        fs::create_dir_all(&directory).expect("create fixture directory");
        buffer.fill(u8::try_from(index % 251).expect("fixture byte"));
        let length = bytes_per_file + usize::from(index < remainder);
        fs::write(
            directory.join(format!("file-{index:05}.rs")),
            &buffer[..length],
        )
        .expect("write S1 fixture file");
    }
    let edit = vec![0x5a_u8; 1024 * 1024 / 10];
    for index in 0..10 {
        let path = workspace.join(format!("src/000/file-{index:05}.rs"));
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open fixture edit");
        file.write_all(&edit).expect("append fixture edit");
    }

    let holder = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "m0_child_holder_waits",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn workspace holder");
    fs::write(workspace.join(".holder-pid"), holder.id().to_string()).expect("write holder PID");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "M0 physical helper; killed by terminal process-tree drain"]
fn m0_child_holder_waits() {
    loop {
        std::thread::park_timeout(Duration::from_secs(60));
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "M0 physical helper; SIGKILLs itself after the injected owner edge"]
fn m0_child_hits_owner_fault_then_sigkills() {
    let allocation_root = required_path("MPLA_OWNER_CRASH_ALLOCATION");
    let stable: StableAllocationReceipt =
        durable::read_json(&required_path("MPLA_OWNER_CRASH_STABLE")).expect("read stable receipt");
    let request: OwnerTransitionRequest =
        durable::read_json(&required_path("MPLA_OWNER_CRASH_REQUEST")).expect("read owner request");
    let error = compare_and_adopt(&allocation_root, &stable, &request)
        .expect_err("owner fault marker must interrupt adoption");
    assert!(matches!(error, PocError::RecoveryRequired(_)));
    durable::replace_json(
        &required_path("MPLA_OWNER_CRASH_OBSERVED"),
        &serde_json::json!({
            "fault_observed": true,
            "error": error.to_string(),
        }),
    )
    .expect("persist child fault witness");
    // SAFETY: kill targets only this helper process with the platform SIGKILL
    // constant. It is intentionally the physical crash boundary under test.
    let result = unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    assert_eq!(result, 0, "self SIGKILL failed");
    unreachable!("SIGKILL returned");
}

#[cfg(target_os = "linux")]
fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} must be set for leased M0 physical execution"))
}

#[cfg(target_os = "linux")]
fn allocation_descriptors(arena_root: &Path) -> BTreeSet<PathBuf> {
    let mut output = BTreeSet::new();
    let Ok(prefixes) = fs::read_dir(arena_root) else {
        return output;
    };
    for prefix in prefixes.filter_map(Result::ok) {
        let Ok(allocations) = fs::read_dir(prefix.path()) else {
            continue;
        };
        for allocation in allocations.filter_map(Result::ok) {
            if allocation.path().join("ALLOCATION.json").is_file() {
                output.insert(allocation.path());
            }
        }
    }
    output
}
