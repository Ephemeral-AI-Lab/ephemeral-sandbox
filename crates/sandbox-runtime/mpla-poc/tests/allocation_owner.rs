use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::Duration;

use sandbox_runtime_mpla_poc::allocation::{
    create_allocation, destroy_workspace_allocation, open_allocation,
};
use sandbox_runtime_mpla_poc::durable::{read_json, replace_json};
use sandbox_runtime_mpla_poc::lease::{issue_workspace_lease, validate_deleter, validate_writer};
use sandbox_runtime_mpla_poc::owner::{compare_and_adopt, current_owner};
use sandbox_runtime_mpla_poc::{
    AllocationHandle, InodeWitness, MutableLease, OperationId, OwnerSubject,
    OwnerTransitionRequest, PhysicalSnapshot, PocError, PublicationId, SessionId,
    StableAllocationReceipt, SCHEMA_VERSION,
};
use serde_json::{json, Value};
use uuid::Uuid;

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mpla-allocation-owner-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create test root");
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct Fixture {
    _root: TestRoot,
    allocation: AllocationHandle,
    lease: MutableLease,
}

impl Fixture {
    fn new() -> Self {
        let root = TestRoot::new();
        let allocation =
            create_allocation(&root.path.join("arena"), &OperationId::new()).expect("allocate");
        let lease = issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new())
            .expect("issue workspace lease");
        Self {
            _root: root,
            allocation,
            lease,
        }
    }

    fn transition(&self) -> (StableAllocationReceipt, OwnerTransitionRequest) {
        let operation_id = OperationId::new();
        let snapshot = physical_snapshot(&self.allocation);
        (
            StableAllocationReceipt {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                allocation: self.allocation.descriptor.clone(),
                expected_owner_epoch: self.lease.owner_epoch,
                before: snapshot.clone(),
                after: snapshot,
                sync_completed: true,
            },
            OwnerTransitionRequest {
                schema_version: SCHEMA_VERSION,
                operation_id,
                publication_id: PublicationId::new(),
                session_id: self.lease.session_id.clone(),
                allocation_id: self.lease.allocation_id.clone(),
                expected_lease_epoch: self.lease.lease_epoch,
                expected_owner_epoch: self.lease.owner_epoch,
            },
        )
    }
}

#[test]
fn allocation_is_created_once_at_its_stable_random_final_path() {
    let root = TestRoot::new();
    let arena = root.path.join("arena");
    let operation = OperationId::new();
    let allocation = create_allocation(&arena, &operation).expect("allocate");
    let id = allocation.descriptor.allocation_id.as_str();

    assert_eq!(
        Uuid::parse_str(id).expect("UUID").hyphenated().to_string(),
        id
    );
    assert_eq!(allocation.allocation_root, arena.join(&id[..2]).join(id));
    assert_eq!(allocation.upper_dir.parent(), allocation.work_dir.parent());
    assert_eq!(allocation.descriptor.created_by_operation, operation);
    assert!(!id.contains(operation.as_str()));

    let reopened =
        open_allocation(&arena, &allocation.descriptor.allocation_id).expect("reopen allocation");
    assert_eq!(reopened, allocation);

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        assert_eq!(
            std::fs::metadata(&allocation.upper_dir)
                .expect("stat upper")
                .dev(),
            std::fs::metadata(&allocation.work_dir)
                .expect("stat work")
                .dev()
        );
    }
}

#[test]
fn lease_replay_is_idempotent_and_other_issuers_conflict() {
    let root = TestRoot::new();
    let allocation =
        create_allocation(&root.path.join("arena"), &OperationId::new()).expect("allocate");
    let session = SessionId::new();
    let operation = OperationId::new();
    let first =
        issue_workspace_lease(&allocation, session.clone(), &operation).expect("first lease");
    let replay =
        issue_workspace_lease(&allocation, session, &operation).expect("response-loss replay");
    assert_eq!(replay, first);
    assert!(matches!(
        issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new()),
        Err(PocError::OwnerConflict(_))
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn only_the_current_workspace_deleter_can_destroy_an_allocation() {
    let root = TestRoot::new();
    let arena = root.path.join("arena");
    let allocation = create_allocation(&arena, &OperationId::new()).expect("allocate");
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new())
        .expect("workspace lease");
    let prefix = allocation
        .allocation_root
        .parent()
        .expect("allocation prefix")
        .to_path_buf();
    std::fs::write(allocation.upper_dir.join("payload"), b"delete me").expect("write payload");

    let mut forged = lease.deleter.clone();
    forged.nonce = Uuid::new_v4().to_string();
    assert!(matches!(
        destroy_workspace_allocation(&arena, &allocation.descriptor.allocation_id, &forged),
        Err(PocError::StaleCapability { .. })
    ));
    assert!(allocation.allocation_root.exists());

    destroy_workspace_allocation(&arena, &allocation.descriptor.allocation_id, &lease.deleter)
        .expect("authorized destroy");
    assert!(!allocation.allocation_root.exists());
    assert!(prefix.is_dir());
}

#[cfg(target_os = "linux")]
#[test]
fn payload_owned_allocation_cannot_be_destroyed_by_the_stale_workspace_deleter() {
    let fixture = Fixture::new();
    let (stable, request) = fixture.transition();
    compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request).expect("adopt");

    assert!(matches!(
        destroy_workspace_allocation(
            &fixture._root.path.join("arena"),
            &fixture.allocation.descriptor.allocation_id,
            &fixture.lease.deleter,
        ),
        Err(PocError::StaleCapability { .. })
    ));
    assert!(fixture.allocation.allocation_root.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn allocation_deletion_unlinks_payload_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let root = TestRoot::new();
    let arena = root.path.join("arena");
    let allocation = create_allocation(&arena, &OperationId::new()).expect("allocate");
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new())
        .expect("workspace lease");
    let outside = root.path.join("outside");
    std::fs::create_dir(&outside).expect("create outside directory");
    let sentinel = outside.join("sentinel");
    std::fs::write(&sentinel, b"preserve").expect("write outside sentinel");
    symlink(&outside, allocation.upper_dir.join("outside-link")).expect("install payload symlink");

    destroy_workspace_allocation(&arena, &allocation.descriptor.allocation_id, &lease.deleter)
        .expect("destroy allocation containing symlink");

    assert!(!allocation.allocation_root.exists());
    assert_eq!(
        std::fs::read(&sentinel).expect("read outside sentinel"),
        b"preserve"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn allocation_deletion_rejects_a_replacement_installed_while_owner_lock_waits() {
    let root = TestRoot::new();
    let arena = root.path.join("arena");
    let allocation = create_allocation(&arena, &OperationId::new()).expect("allocate");
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new())
        .expect("workspace lease");
    let original_payload = allocation.upper_dir.join("original");
    std::fs::write(&original_payload, b"original").expect("write original payload");
    let owner_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(allocation.owner_dir.join("LOCK"))
        .expect("open owner lock");
    rustix::fs::flock(&owner_lock, rustix::fs::FlockOperation::LockExclusive)
        .expect("hold owner lock");

    let (tid_sender, tid_receiver) = std::sync::mpsc::channel();
    let deletion_arena = arena.clone();
    let deletion_id = allocation.descriptor.allocation_id.clone();
    let deleter = lease.deleter.clone();
    let deletion_thread = std::thread::spawn(move || {
        tid_sender
            .send(rustix::thread::gettid().as_raw_nonzero().get())
            .expect("publish deletion thread ID");
        destroy_workspace_allocation(&deletion_arena, &deletion_id, &deleter)
    });
    let deletion_tid = tid_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("receive deletion thread ID");
    let blocked = wait_for_lock_wait(deletion_tid);
    let parked = allocation
        .allocation_root
        .with_file_name(format!("{}-parked", allocation.descriptor.allocation_id));
    if blocked {
        std::fs::rename(&allocation.allocation_root, &parked).expect("park pinned allocation");
        std::fs::create_dir(&allocation.allocation_root).expect("create replacement allocation");
        std::fs::write(
            allocation.allocation_root.join("replacement"),
            b"replacement",
        )
        .expect("write replacement sentinel");
    }
    rustix::fs::flock(&owner_lock, rustix::fs::FlockOperation::Unlock).expect("release owner lock");
    drop(owner_lock);
    let result = deletion_thread.join().expect("join deletion thread");

    assert!(
        blocked,
        "deletion did not reach the controlled owner-lock wait"
    );
    assert!(matches!(result, Err(PocError::RecoveryRequired(_))));
    assert_eq!(
        std::fs::read(parked.join("upper/original")).expect("read parked original"),
        b"original"
    );
    assert_eq!(
        std::fs::read(allocation.allocation_root.join("replacement"))
            .expect("read replacement sentinel"),
        b"replacement"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn allocation_deletion_rejects_a_live_aliased_directory_descriptor() {
    use std::os::unix::fs::symlink;

    let root = TestRoot::new();
    let arena = root.path.join("arena");
    let allocation = create_allocation(&arena, &OperationId::new()).expect("allocate");
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new())
        .expect("workspace lease");
    let alias = root.path.join("upper-alias");
    symlink(&allocation.upper_dir, &alias).expect("create upper alias");
    let live_upper = File::open(&alias).expect("open aliased upper directory");

    assert!(matches!(
        destroy_workspace_allocation(&arena, &allocation.descriptor.allocation_id, &lease.deleter,),
        Err(PocError::OwnerConflict(_))
    ));
    assert!(allocation.allocation_root.is_dir());

    drop(live_upper);
    destroy_workspace_allocation(&arena, &allocation.descriptor.allocation_id, &lease.deleter)
        .expect("destroy after aliased descriptor closes");
    assert!(!allocation.allocation_root.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn allocation_deletion_rejects_a_live_payload_file_descriptor() {
    let root = TestRoot::new();
    let arena = root.path.join("arena");
    let allocation = create_allocation(&arena, &OperationId::new()).expect("allocate");
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new())
        .expect("workspace lease");
    let payload = allocation.upper_dir.join("payload");
    std::fs::write(&payload, b"live").expect("write payload");
    let live_payload = File::open(&payload).expect("open payload");

    assert!(matches!(
        destroy_workspace_allocation(&arena, &allocation.descriptor.allocation_id, &lease.deleter,),
        Err(PocError::OwnerConflict(_))
    ));
    assert_eq!(
        std::fs::read(&payload).expect("read restored payload"),
        b"live"
    );

    drop(live_payload);
    destroy_workspace_allocation(&arena, &allocation.descriptor.allocation_id, &lease.deleter)
        .expect("destroy after payload descriptor closes");
    assert!(!allocation.allocation_root.exists());
}

#[test]
fn legal_adoption_is_conditional_and_never_moves_payload_bytes() {
    let fixture = Fixture::new();
    let payload = fixture.allocation.upper_dir.join("payload.bin");
    std::fs::write(&payload, b"stationary payload").expect("write payload");
    let before = file_identity(&payload);
    let (mut stable, mut request) = fixture.transition();

    stable.expected_owner_epoch += 1;
    request.expected_owner_epoch += 1;
    assert!(matches!(
        compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request),
        Err(PocError::OwnerConflict(_))
    ));
    stable.expected_owner_epoch -= 1;
    request.expected_owner_epoch -= 1;

    let receipt =
        compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request).expect("adopt");
    assert!(!receipt.idempotent_replay);
    assert_eq!(receipt.prior_owner.owner_epoch, fixture.lease.owner_epoch);
    assert_eq!(receipt.new_owner.owner_epoch, fixture.lease.owner_epoch + 1);
    assert!(matches!(
        receipt.prior_owner.subject,
        OwnerSubject::WorkspaceOwned { .. }
    ));
    assert!(matches!(
        receipt.new_owner.subject,
        OwnerSubject::PayloadOwned { .. }
    ));
    assert_eq!(
        std::fs::read(&payload).expect("read payload"),
        b"stationary payload"
    );
    assert_eq!(file_identity(&payload), before);
    assert_eq!(
        fixture.allocation.upper_dir,
        fixture.allocation.allocation_root.join("upper")
    );

    let (conflicting_stable, conflicting) = fixture.transition();
    assert!(matches!(
        compare_and_adopt(
            &fixture.allocation.allocation_root,
            &conflicting_stable,
            &conflicting
        ),
        Err(PocError::OwnerConflict(_))
    ));
}

#[test]
fn current_tokens_validate_and_fenced_tokens_fail_before_payload_access() {
    let fixture = Fixture::new();
    validate_writer(&fixture.allocation.allocation_root, &fixture.lease.writer)
        .expect("current writer");
    validate_deleter(&fixture.allocation.allocation_root, &fixture.lease.deleter)
        .expect("current deleter");

    let mut forged = fixture.lease.writer.clone();
    forged.nonce = Uuid::new_v4().to_string();
    assert_stale(
        validate_writer(&fixture.allocation.allocation_root, &forged),
        "writer",
        fixture.lease.lease_epoch,
        fixture.lease.lease_epoch,
    );

    let (stable, request) = fixture.transition();
    compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request)
        .expect("fence and adopt");

    #[cfg(unix)]
    let original_permissions = {
        use std::os::unix::fs::PermissionsExt;

        let permissions = std::fs::metadata(&fixture.allocation.upper_dir)
            .expect("stat upper")
            .permissions();
        std::fs::set_permissions(
            &fixture.allocation.upper_dir,
            std::fs::Permissions::from_mode(0o000),
        )
        .expect("make payload inaccessible");
        permissions
    };

    let writer_result = validate_writer(&fixture.allocation.allocation_root, &fixture.lease.writer);
    let deleter_result =
        validate_deleter(&fixture.allocation.allocation_root, &fixture.lease.deleter);

    #[cfg(unix)]
    std::fs::set_permissions(&fixture.allocation.upper_dir, original_permissions)
        .expect("restore payload permissions");

    assert_stale(
        writer_result,
        "writer",
        fixture.lease.lease_epoch + 1,
        fixture.lease.lease_epoch,
    );
    assert_stale(
        deleter_result,
        "deleter",
        fixture.lease.lease_epoch + 1,
        fixture.lease.lease_epoch,
    );
}

#[test]
fn adoption_retry_after_response_loss_has_exactly_one_selected_owner() {
    let fixture = Fixture::new();
    let (stable, request) = fixture.transition();
    let first =
        compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request).expect("adopt");
    let replay = compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request)
        .expect("retry adoption");

    assert!(!first.idempotent_replay);
    assert!(replay.idempotent_replay);
    assert_eq!(first.operation_id, replay.operation_id);
    assert_eq!(first.prior_owner, replay.prior_owner);
    assert_eq!(first.new_owner, replay.new_owner);
    assert_single_payload_owner(&fixture, &request);
    assert_eq!(
        std::fs::read_dir(fixture.allocation.owner_dir.join("receipts"))
            .expect("list receipts")
            .count(),
        1
    );
    let records = journal_frames(&fixture.allocation.owner_dir.join("journal.bin"));
    for record in &records {
        assert_journal_record_shape(record);
    }
    assert_eq!(
        records
            .iter()
            .map(|record| record["phase"].as_str().expect("journal phase"))
            .collect::<Vec<_>>(),
        [
            "workspace_lease_issued",
            "adoption_intent",
            "owner_committed"
        ]
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record["terminal_outcome"]
                .as_str()
                .expect("terminal outcome"))
            .collect::<Vec<_>>(),
        ["workspace_owned", "pending", "payload_owned"]
    );
}

#[test]
fn torn_journal_tail_is_detected_and_truncated_after_selector_validation() {
    let fixture = Fixture::new();
    let journal = fixture.allocation.owner_dir.join("journal.bin");
    let valid_length = std::fs::metadata(&journal).expect("stat journal").len();
    let mut file = OpenOptions::new()
        .append(true)
        .open(&journal)
        .expect("open journal");
    file.write_all(b"MPLJ\x01").expect("append torn frame");
    file.sync_data().expect("sync torn frame");

    let owner = current_owner(&fixture.allocation.allocation_root).expect("recover torn tail");
    assert!(matches!(owner.subject, OwnerSubject::WorkspaceOwned { .. }));
    assert_eq!(
        std::fs::metadata(&journal)
            .expect("stat repaired journal")
            .len(),
        valid_length
    );

    let frames = journal_frames(&journal);
    assert_eq!(frames.len(), 1);
    assert_journal_record_shape(&frames[0]);
}

#[test]
fn replay_repairs_failure_immediately_before_selector_replacement() {
    let fixture = Fixture::new();
    let (stable, request) = fixture.transition();
    let marker = fixture
        .allocation
        .owner_dir
        .join(".fault-before-owner-selector-replace");
    File::create(&marker).expect("install fault marker");

    assert!(matches!(
        compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request),
        Err(PocError::RecoveryRequired(_))
    ));
    assert_eq!(raw_selector_epoch(&fixture), fixture.lease.owner_epoch);
    assert!(matches!(
        validate_writer(&fixture.allocation.allocation_root, &fixture.lease.writer),
        Err(PocError::StaleCapability { .. })
    ));

    std::fs::remove_file(marker).expect("clear fault marker");
    let recovered =
        current_owner(&fixture.allocation.allocation_root).expect("replay committed owner");
    assert_eq!(recovered.owner_epoch, fixture.lease.owner_epoch + 1);
    let replay = compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request)
        .expect("recover response");
    assert!(replay.idempotent_replay);
    assert_single_payload_owner(&fixture, &request);
    assert_eq!(
        journal_frames(&fixture.allocation.owner_dir.join("journal.bin")).len(),
        3
    );
}

#[test]
fn replay_repairs_failure_immediately_after_selector_replacement() {
    let fixture = Fixture::new();
    let (stable, request) = fixture.transition();
    let marker = fixture
        .allocation
        .owner_dir
        .join(".fault-after-owner-selector-replace");
    File::create(&marker).expect("install fault marker");

    assert!(matches!(
        compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request),
        Err(PocError::RecoveryRequired(_))
    ));
    assert_eq!(raw_selector_epoch(&fixture), fixture.lease.owner_epoch + 1);
    std::fs::remove_file(marker).expect("clear fault marker");

    let replay = compare_and_adopt(&fixture.allocation.allocation_root, &stable, &request)
        .expect("recover lost response");
    assert!(replay.idempotent_replay);
    assert_single_payload_owner(&fixture, &request);
    assert_eq!(
        journal_frames(&fixture.allocation.owner_dir.join("journal.bin")).len(),
        3
    );
}

#[test]
fn selector_replacement_leaves_no_temporary_file() {
    let root = TestRoot::new();
    let selector = root.path.join("CURRENT");
    replace_json(&selector, &json!({"owner_epoch": 1})).expect("first selector");
    replace_json(&selector, &json!({"owner_epoch": 2})).expect("replace selector");
    let value: Value = read_json(&selector).expect("read selector");
    assert_eq!(value["owner_epoch"], 2);
    assert_eq!(
        std::fs::read_dir(&root.path)
            .expect("list selector directory")
            .count(),
        1
    );
}

fn physical_snapshot(allocation: &AllocationHandle) -> PhysicalSnapshot {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(&allocation.upper_dir).expect("stat upper");
    #[cfg(unix)]
    let (device, inode) = (metadata.dev(), metadata.ino());
    #[cfg(not(unix))]
    let (device, inode) = (0, 0);
    PhysicalSnapshot {
        allocation_id: allocation.descriptor.allocation_id.clone(),
        allocation_path: allocation.allocation_root.clone(),
        device,
        representative_inodes: vec![InodeWitness {
            relative_path: PathBuf::from("."),
            device,
            inode,
        }],
        logical_bytes: 0,
        allocated_bytes: 0,
        inode_count: 1,
        file_count: 0,
        directory_count: 1,
    }
}

#[cfg(unix)]
fn file_identity(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path).expect("stat payload");
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_identity(path: &Path) -> (u64, u64) {
    (std::fs::metadata(path).expect("stat payload").len(), 0)
}

fn assert_stale(
    result: Result<(), PocError>,
    expected_kind: &str,
    expected_epoch: u64,
    observed_epoch: u64,
) {
    match result {
        Err(PocError::StaleCapability {
            capability,
            expected_epoch: actual_expected,
            observed_epoch: actual_observed,
            ..
        }) => {
            assert_eq!(capability, expected_kind);
            assert_eq!(actual_expected, expected_epoch);
            assert_eq!(actual_observed, observed_epoch);
        }
        other => panic!("expected stale {expected_kind} capability, got {other:?}"),
    }
}

fn raw_selector_epoch(fixture: &Fixture) -> u64 {
    let selector: Value =
        read_json(&fixture.allocation.owner_dir.join("CURRENT")).expect("read selector");
    selector["owner_epoch"].as_u64().expect("owner epoch")
}

fn assert_single_payload_owner(fixture: &Fixture, request: &OwnerTransitionRequest) {
    let owner = current_owner(&fixture.allocation.allocation_root).expect("current owner");
    assert_eq!(owner.owner_epoch, fixture.lease.owner_epoch + 1);
    assert!(matches!(
        owner.subject,
        OwnerSubject::PayloadOwned { publication_id }
            if publication_id == request.publication_id
    ));
    assert_eq!(raw_selector_epoch(fixture), owner.owner_epoch);
    assert_eq!(
        std::fs::read_dir(&fixture.allocation.owner_dir)
            .expect("list owner dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() == "CURRENT")
            .count(),
        1
    );
}

fn journal_frames(path: &Path) -> Vec<Value> {
    let bytes = std::fs::read(path).expect("read journal");
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        assert_eq!(&bytes[offset..offset + 4], b"MPLJ");
        assert_eq!(
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().expect("version")),
            1
        );
        let length = usize::try_from(u64::from_le_bytes(
            bytes[offset + 8..offset + 16].try_into().expect("length"),
        ))
        .expect("usize length");
        let start = offset + 16;
        let end = start + length;
        frames.push(serde_json::from_slice(&bytes[start..end]).expect("journal JSON"));
        offset = end;
    }
    frames
}

fn assert_journal_record_shape(record: &Value) {
    for field in [
        "schema_version",
        "sequence",
        "allocation_id",
        "operation_id",
        "prior_owner",
        "new_owner",
        "prior_owner_epoch",
        "new_owner_epoch",
        "phase",
        "terminal_outcome",
        "previous_record_hash",
        "record_hash",
        "checksum_crc32c",
    ] {
        assert!(record.get(field).is_some(), "missing journal field {field}");
    }
}

#[cfg(target_os = "linux")]
fn wait_for_lock_wait(tid: i32) -> bool {
    let wchan_path = PathBuf::from(format!("/proc/self/task/{tid}/wchan"));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let wchan = std::fs::read_to_string(&wchan_path).unwrap_or_default();
        if wchan.contains("lock") && wchan.contains("wait") {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
