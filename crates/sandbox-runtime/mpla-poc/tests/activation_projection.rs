#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::fs::{File, FileTimes};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

#[cfg(target_os = "linux")]
use sandbox_runtime_mpla_poc::activation::activate_exact;
use sandbox_runtime_mpla_poc::projection::{select_exact, MAX_RECENT_DELTAS};
use sandbox_runtime_mpla_poc::{
    inherit_projection_root_metadata, AllocationId, AttributionRootId, CanonicalRootPair,
    ProjectionRecipe, RootId, SCHEMA_VERSION,
};
#[cfg(target_os = "linux")]
use sandbox_runtime_mpla_poc::{
    ActivationOperationId, AllocationHandle, ExactActivationRequest, LocatorGeneration,
    OperationId, PairedRefValue, PocError, PublicationId, RefSequence, SessionId,
};
use uuid::Uuid;

fn roots() -> CanonicalRootPair {
    CanonicalRootPair {
        root_id: RootId::parse("11".repeat(32)).expect("root"),
        attribution_root_id: AttributionRootId::parse("22".repeat(32)).expect("attribution"),
    }
}

#[test]
fn exact_projection_is_zero_build_and_bounded() {
    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: AllocationId::new(),
        net_delta_carrier_id: Some(AllocationId::new()),
        recent_delta_ids: (0..MAX_RECENT_DELTAS)
            .map(|_| AllocationId::new())
            .collect(),
    };
    let receipt = select_exact(&recipe).expect("exact selection");
    assert_eq!(receipt.kernel_lower_count, 10);
    assert_eq!(receipt.reconstructed_payload_bytes, 0);
    assert_eq!(receipt.hydrated_payload_bytes, 0);
    assert_eq!(receipt.base_bytes_copied, 0);
    assert!(!receipt.projection_built_during_activation);
}

#[test]
fn projection_rejects_depth_and_aliasing() {
    let base = AllocationId::new();
    let too_deep = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: base.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: (0..=MAX_RECENT_DELTAS)
            .map(|_| AllocationId::new())
            .collect(),
    };
    assert!(too_deep.validate().is_err());

    let aliased = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: base.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: vec![base],
    };
    assert!(aliased.validate().is_err());
}

#[test]
fn fresh_upper_inherits_projection_root_semantics() {
    let temporary = Temporary::new("activation-root");
    let source = temporary.path.join("source");
    let target = temporary.path.join("target");
    std::fs::create_dir(&source).expect("source root");
    std::fs::create_dir(&target).expect("target root");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o751))
        .expect("source permissions");
    File::open(&source)
        .expect("open source root")
        .set_times(
            FileTimes::new()
                .set_accessed(UNIX_EPOCH + Duration::from_secs(1_700_000_001))
                .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_002)),
        )
        .expect("source timestamps");
    File::open(&target)
        .expect("open target root")
        .set_times(
            FileTimes::new()
                .set_accessed(UNIX_EPOCH + Duration::from_secs(1_800_000_001))
                .set_modified(UNIX_EPOCH + Duration::from_secs(1_800_000_002)),
        )
        .expect("target timestamps");

    inherit_projection_root_metadata(&source, &target).expect("inherit root metadata");

    let source_metadata = std::fs::symlink_metadata(&source).expect("source metadata");
    let target_metadata = std::fs::symlink_metadata(&target).expect("target metadata");
    assert_eq!(
        target_metadata.permissions().mode() & 0o7777,
        source_metadata.permissions().mode() & 0o7777
    );
    assert_eq!(target_metadata.uid(), source_metadata.uid());
    assert_eq!(target_metadata.gid(), source_metadata.gid());
    assert_eq!(target_metadata.atime(), source_metadata.atime());
    assert_eq!(target_metadata.atime_nsec(), source_metadata.atime_nsec());
    assert_eq!(target_metadata.mtime(), source_metadata.mtime());
    assert_eq!(target_metadata.mtime_nsec(), source_metadata.mtime_nsec());
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_keeps_activation_state_on_the_locked_directory_after_ancestor_replacement() {
    let temporary = Temporary::new("activation-control-anchor-race");
    let payload_operation = OperationId::from_string("activation-control-anchor-payload");
    let payload_arena = temporary.path.join("payload-arena");
    let payload =
        sandbox_runtime_mpla_poc::allocation::create_allocation(&payload_arena, &payload_operation)
            .expect("create payload allocation");
    sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &payload,
        SessionId::new(),
        &payload_operation,
    )
    .expect("select payload owner");

    let live_parent = temporary.path.join("live-control-parent");
    let control_root = live_parent.join("control");
    let arena_root = temporary.path.join("fresh-arena");
    std::fs::create_dir_all(&control_root).expect("create control root");
    std::fs::create_dir(&arena_root).expect("create fresh arena");
    let request = activation_recovery_request(
        "activation-control-anchor",
        "activation-control-anchor-fresh",
        payload,
        arena_root,
        control_root.clone(),
    );
    let session_id = SessionId::from_string("activation-control-anchor-session");
    persist_activation_plan(&request, &session_id);

    let owner_lock = lock_test_owner(&request.payload_allocations[0]);
    let (tid_sender, tid_receiver) = std::sync::mpsc::channel();
    let recovery_request = request.clone();
    let recovery_thread = std::thread::spawn(move || {
        // SAFETY: `gettid` has no pointers or preconditions.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        tid_sender.send(tid).expect("publish recovery thread ID");
        sandbox_runtime_mpla_poc::recover_exact_activation(&recovery_request)
    });
    let recovery_tid = tid_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("receive recovery thread ID");
    let blocked = wait_for_lock_wait(recovery_tid);
    let anchored_parent = temporary.path.join("anchored-control-parent");
    let decoy_activation = control_root
        .join("activations")
        .join(request.activation_operation_id.as_str());
    if blocked {
        std::fs::rename(&live_parent, &anchored_parent).expect("rename locked control ancestor");
        std::fs::create_dir_all(&decoy_activation)
            .expect("create replacement activation directory");
        std::fs::write(decoy_activation.join("DECOY"), b"replacement")
            .expect("write replacement marker");
    }
    unlock_test_owner(owner_lock);

    let recovery = recovery_thread.join().expect("join recovery thread");
    assert!(
        blocked,
        "recovery did not reach the controlled owner-lock wait"
    );
    let receipt = recovery.expect("recover through pinned activation directory");
    assert_eq!(
        receipt.allocation_removed,
        receipt.fresh_allocation.is_some()
    );
    let anchored_activation = anchored_parent
        .join("control/activations")
        .join(request.activation_operation_id.as_str());
    assert!(anchored_activation.join("RECOVERY-INTENT.json").is_file());
    assert!(anchored_activation.join("RECOVERY.json").is_file());
    assert_eq!(
        std::fs::read(decoy_activation.join("DECOY")).expect("read replacement marker"),
        b"replacement"
    );
    assert!(!decoy_activation.join("RECOVERY-INTENT.json").exists());
    assert!(!decoy_activation.join("RECOVERY.json").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn activation_keeps_operation_state_on_the_locked_directory_after_ancestor_replacement() {
    let temporary = Temporary::new("activation-operation-anchor-race");
    let payload_operation = OperationId::from_string("activation-operation-anchor-payload");
    let payload = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &temporary.path.join("payload-arena"),
        &payload_operation,
    )
    .expect("create payload allocation");
    std::fs::write(payload.upper_dir.join("ready"), b"ready")
        .expect("create activation readiness fixture");
    sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &payload,
        SessionId::new(),
        &payload_operation,
    )
    .expect("select payload owner");
    let live_parent = temporary.path.join("live-control-parent");
    let control_root = live_parent.join("control");
    std::fs::create_dir_all(&control_root).expect("create control root");
    let arena_root = temporary.path.join("fresh-arena");
    let request = activation_recovery_request(
        "activation-operation-anchor",
        "activation-operation-anchor-fresh",
        payload,
        arena_root.clone(),
        control_root.clone(),
    );
    let fresh = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &arena_root,
        &request.allocation_operation_id,
    )
    .expect("create private activation allocation");
    let owner_lock = lock_test_owner(&fresh);
    let (tid_sender, tid_receiver) = std::sync::mpsc::channel();
    let activation_request = request.clone();
    let activation_thread = std::thread::spawn(move || {
        // SAFETY: `gettid` has no pointers or preconditions.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        tid_sender.send(tid).expect("publish activation thread ID");
        activate_exact(activation_request)
    });
    let activation_tid = tid_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("receive activation thread ID");
    let blocked = wait_for_lock_wait(activation_tid);
    let anchored_parent = temporary.path.join("anchored-control-parent");
    if blocked {
        std::fs::rename(&live_parent, &anchored_parent).expect("rename locked control ancestor");
        std::fs::create_dir_all(&control_root).expect("create replacement control root");
        std::fs::write(control_root.join("sessions"), b"replacement")
            .expect("block replacement session creation");
    }
    unlock_test_owner(owner_lock);

    let activation = activation_thread.join().expect("join activation thread");
    assert!(
        blocked,
        "activation did not reach the controlled owner-lock wait"
    );
    let activation = activation.expect("activate through the pinned control root");
    let anchored_activation = anchored_parent
        .join("control/activations")
        .join(request.activation_operation_id.as_str());
    assert!(anchored_activation.join("PLAN.json").is_file());
    assert!(anchored_activation.join("LOCATOR_PIN.json").is_file());
    assert!(anchored_activation.join("FRESH.json").is_file());
    let decoy_activation = control_root
        .join("activations")
        .join(request.activation_operation_id.as_str());
    assert!(!decoy_activation.exists());
    let anchored_session = anchored_parent
        .join("control/sessions")
        .join(activation.receipt.session_id.as_str());
    assert!(anchored_session.join("SESSION.json").is_file());
    assert!(anchored_session.join("MOUNT.json").is_file());
    assert_eq!(
        std::fs::read(control_root.join("sessions")).expect("read replacement sessions poison"),
        b"replacement"
    );
    assert_eq!(
        activation.session.session_dir(),
        control_root
            .join("sessions")
            .join(activation.receipt.session_id.as_str())
    );
}

#[cfg(target_os = "linux")]
#[test]
fn activation_rejects_a_replacement_fresh_upper_after_pinning() {
    let temporary = Temporary::new("activation-upper-anchor-race");
    let payload_operation = OperationId::from_string("activation-upper-anchor-payload");
    let payload = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &temporary.path.join("payload-arena"),
        &payload_operation,
    )
    .expect("create payload allocation");
    std::fs::write(payload.upper_dir.join("ready"), b"ready")
        .expect("create activation readiness fixture");
    sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &payload,
        SessionId::new(),
        &payload_operation,
    )
    .expect("select payload owner");
    let control_root = temporary.path.join("control");
    std::fs::create_dir(&control_root).expect("create control root");
    let arena_root = temporary.path.join("fresh-arena");
    let request = activation_recovery_request(
        "activation-upper-anchor",
        "activation-upper-anchor-fresh",
        payload,
        arena_root.clone(),
        control_root.clone(),
    );
    let fresh = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &arena_root,
        &request.allocation_operation_id,
    )
    .expect("create private activation allocation");
    let owner_lock = lock_test_owner(&fresh);
    let (tid_sender, tid_receiver) = std::sync::mpsc::channel();
    let activation_thread = std::thread::spawn(move || {
        // SAFETY: `gettid` has no pointers or preconditions.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        tid_sender.send(tid).expect("publish activation thread ID");
        activate_exact(request)
    });
    let activation_tid = tid_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("receive activation thread ID");
    let blocked = wait_for_lock_wait(activation_tid);
    let original_upper = fresh.allocation_root.join("upper-original");
    if blocked {
        std::fs::rename(&fresh.upper_dir, &original_upper).expect("swap out pinned fresh upper");
        std::fs::create_dir(&fresh.upper_dir).expect("plant replacement fresh upper");
        std::fs::write(fresh.upper_dir.join("DECOY"), b"replacement-upper")
            .expect("write replacement upper poison");
    }
    unlock_test_owner(owner_lock);

    let activation = activation_thread.join().expect("join activation thread");
    assert!(
        blocked,
        "activation did not reach the controlled owner-lock wait"
    );
    let error = activation.expect_err("replacement fresh upper must fail closed");
    assert!(matches!(
        error,
        PocError::RecoveryRequired(message)
            if message.contains("pinned activation upper changed after it was pinned")
    ));
    assert!(original_upper.is_dir());
    assert_eq!(
        std::fs::read(fresh.upper_dir.join("DECOY")).expect("read replacement upper poison"),
        b"replacement-upper"
    );
    assert!(
        !control_root.join("sessions").exists(),
        "fresh upper replacement must be rejected before session creation"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_removes_only_pinned_session_and_allocation_after_ancestor_replacement() {
    let temporary = Temporary::new("activation-cleanup-anchor-race");
    let payload_operation = OperationId::from_string("activation-cleanup-anchor-payload");
    let payload = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &temporary.path.join("payload-arena"),
        &payload_operation,
    )
    .expect("create payload allocation");
    sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &payload,
        SessionId::new(),
        &payload_operation,
    )
    .expect("select payload owner");

    let control_live_parent = temporary.path.join("live-control-parent");
    let control_root = control_live_parent.join("control");
    std::fs::create_dir_all(&control_root).expect("create control root");
    let arena_live_parent = temporary.path.join("live-arena-parent");
    let arena_root = arena_live_parent.join("arena");
    let request = activation_recovery_request(
        "activation-cleanup-anchor",
        "activation-cleanup-anchor-fresh",
        payload,
        arena_root.clone(),
        control_root.clone(),
    );
    let session_id = SessionId::from_string("activation-cleanup-anchor-session");
    persist_activation_plan(&request, &session_id);
    let fresh = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &arena_root,
        &request.allocation_operation_id,
    )
    .expect("create private activation allocation");
    let original_session = control_root.join("sessions").join(session_id.as_str());
    std::fs::create_dir_all(original_session.join("mount/nested"))
        .expect("create unrecorded private session");
    std::fs::write(original_session.join("mount/nested/original"), b"original")
        .expect("write original session marker");

    let owner_lock = lock_test_owner(&fresh);
    let (tid_sender, tid_receiver) = std::sync::mpsc::channel();
    let recovery_request = request.clone();
    let recovery_thread = std::thread::spawn(move || {
        // SAFETY: `gettid` has no pointers or preconditions.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        tid_sender.send(tid).expect("publish recovery thread ID");
        sandbox_runtime_mpla_poc::recover_exact_activation(&recovery_request)
    });
    let recovery_tid = tid_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("receive recovery thread ID");
    let blocked = wait_for_lock_wait(recovery_tid);
    let anchored_control_parent = temporary.path.join("anchored-control-parent");
    let anchored_arena_parent = temporary.path.join("anchored-arena-parent");
    let decoy_session = control_root.join("sessions").join(session_id.as_str());
    let allocation_prefix = &fresh.descriptor.allocation_id.as_str()[..2];
    let decoy_allocation = arena_root
        .join(allocation_prefix)
        .join(fresh.descriptor.allocation_id.as_str());
    if blocked {
        std::fs::rename(&control_live_parent, &anchored_control_parent)
            .expect("rename pinned control ancestor");
        std::fs::rename(&arena_live_parent, &anchored_arena_parent)
            .expect("rename pinned arena ancestor");
        std::fs::create_dir_all(decoy_session.join("mount/nested"))
            .expect("create replacement session");
        std::fs::write(decoy_session.join("mount/nested/decoy"), b"session-decoy")
            .expect("write replacement session marker");
        std::fs::create_dir_all(&decoy_allocation).expect("create replacement allocation");
        std::fs::write(decoy_allocation.join("DECOY"), b"allocation-decoy")
            .expect("write replacement allocation marker");
    }
    unlock_test_owner(owner_lock);

    let recovery = recovery_thread.join().expect("join recovery thread");
    assert!(
        blocked,
        "recovery did not reach the controlled owner-lock wait"
    );
    let receipt = recovery.expect("recover through pinned cleanup roots");
    assert!(receipt.allocation_removed);
    let anchored_session = anchored_control_parent
        .join("control/sessions")
        .join(session_id.as_str());
    assert!(!anchored_session.exists());
    let anchored_allocation = anchored_arena_parent
        .join("arena")
        .join(allocation_prefix)
        .join(fresh.descriptor.allocation_id.as_str());
    assert!(!anchored_allocation.exists());
    assert_eq!(
        std::fs::read(decoy_session.join("mount/nested/decoy"))
            .expect("read replacement session marker"),
        b"session-decoy"
    );
    assert_eq!(
        std::fs::read(decoy_allocation.join("DECOY")).expect("read replacement allocation marker"),
        b"allocation-decoy"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_fences_the_pinned_owner_and_rejects_a_replacement_owner_leaf() {
    let temporary = Temporary::new("activation-owner-leaf-race");
    let payload_operation = OperationId::from_string("activation-owner-leaf-payload");
    let payload = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &temporary.path.join("payload-arena"),
        &payload_operation,
    )
    .expect("create payload allocation");
    sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &payload,
        SessionId::new(),
        &payload_operation,
    )
    .expect("select payload owner");

    let control_root = temporary.path.join("control");
    let arena_root = temporary.path.join("fresh-arena");
    let request = activation_recovery_request(
        "activation-owner-leaf",
        "activation-owner-leaf-fresh",
        payload,
        arena_root.clone(),
        control_root,
    );
    persist_activation_plan(
        &request,
        &SessionId::from_string("activation-owner-leaf-session"),
    );
    let fresh = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &arena_root,
        &request.allocation_operation_id,
    )
    .expect("create private activation allocation");
    let owner_lock = lock_test_owner(&fresh);
    let (tid_sender, tid_receiver) = std::sync::mpsc::channel();
    let recovery_request = request.clone();
    let recovery_thread = std::thread::spawn(move || {
        // SAFETY: `gettid` has no pointers or preconditions.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        tid_sender.send(tid).expect("publish recovery thread ID");
        sandbox_runtime_mpla_poc::recover_exact_activation(&recovery_request)
    });
    let recovery_tid = tid_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("receive recovery thread ID");
    let blocked = wait_for_lock_wait(recovery_tid);
    let original_owner = fresh.allocation_root.join("owner-original");
    if blocked {
        std::fs::rename(&fresh.owner_dir, &original_owner).expect("rename pinned owner leaf");
        std::fs::create_dir(&fresh.owner_dir).expect("create replacement owner leaf");
        std::fs::write(fresh.owner_dir.join("LOCK"), b"").expect("create replacement owner lock");
        std::fs::write(fresh.owner_dir.join("CURRENT"), b"replacement-owner")
            .expect("write replacement owner poison");
    }
    unlock_test_owner(owner_lock);

    let recovery = recovery_thread.join().expect("join recovery thread");
    assert!(
        blocked,
        "recovery did not reach the controlled owner-lock wait"
    );
    assert!(
        recovery.is_err(),
        "replacement owner binding must fail closed"
    );
    assert!(original_owner.is_dir());
    assert_eq!(
        std::fs::read(fresh.owner_dir.join("CURRENT")).expect("read replacement owner poison"),
        b"replacement-owner"
    );
}

#[cfg(target_os = "linux")]
fn activation_recovery_request(
    activation_id: &str,
    allocation_operation: &str,
    payload: AllocationHandle,
    arena_root: PathBuf,
    control_root: PathBuf,
) -> ExactActivationRequest {
    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: payload.descriptor.allocation_id.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: Vec::new(),
    };
    ExactActivationRequest {
        activation_operation_id: ActivationOperationId::from_string(activation_id),
        allocation_operation_id: OperationId::from_string(allocation_operation),
        selected_ref: PairedRefValue {
            schema_version: SCHEMA_VERSION,
            operation_id: OperationId::from_string(format!("{activation_id}-selected-ref")),
            publication_id: PublicationId::from_string(format!("{activation_id}-publication")),
            roots: recipe.roots.clone(),
            locator_generation: LocatorGeneration::INITIAL,
            sequence: RefSequence::ZERO,
            checksum_sha256: "00".repeat(32),
        },
        recipe,
        payload_allocations: vec![payload],
        arena_root,
        control_root,
        cgroup_procs_path: None,
        readiness_path: PathBuf::from("ready"),
        readiness_contains: None,
        readiness_timeout: Duration::from_secs(1),
    }
}

#[cfg(target_os = "linux")]
fn persist_activation_plan(request: &ExactActivationRequest, session_id: &SessionId) {
    let activation_directory = request
        .control_root
        .join("activations")
        .join(request.activation_operation_id.as_str());
    std::fs::create_dir_all(&activation_directory).expect("create activation directory");
    let payload_physical_identities = request
        .payload_allocations
        .iter()
        .map(|payload| {
            let allocation =
                std::fs::metadata(&payload.allocation_root).expect("stat payload allocation");
            let upper = std::fs::metadata(&payload.upper_dir).expect("stat payload upper");
            let work = std::fs::metadata(&payload.work_dir).expect("stat payload work");
            let owner = std::fs::metadata(&payload.owner_dir).expect("stat payload owner");
            serde_json::json!({
                "allocation_device": allocation.dev(),
                "allocation_inode": allocation.ino(),
                "upper_device": upper.dev(),
                "upper_inode": upper.ino(),
                "work_device": work.dev(),
                "work_inode": work.ino(),
                "owner_device": owner.dev(),
                "owner_inode": owner.ino(),
            })
        })
        .collect::<Vec<_>>();
    let plan = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "activation_operation_id": &request.activation_operation_id,
        "allocation_operation_id": &request.allocation_operation_id,
        "session_id": session_id,
        "selected_ref": &request.selected_ref,
        "recipe": &request.recipe,
        "payload_allocations": &request.payload_allocations,
        "payload_physical_identities": payload_physical_identities,
        "arena_root": &request.arena_root,
        "control_root": &request.control_root,
        "cgroup_procs_path": &request.cgroup_procs_path,
        "readiness_path": &request.readiness_path,
        "readiness_contains": &request.readiness_contains,
        "readiness_timeout_ns": 1_000_000_000_u64,
        "created_unix_ms": 0_u64,
    });
    std::fs::write(
        activation_directory.join("PLAN.json"),
        serde_json::to_vec(&plan).expect("encode activation plan"),
    )
    .expect("write activation plan");
}

#[cfg(target_os = "linux")]
fn lock_test_owner(allocation: &AllocationHandle) -> File {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(allocation.owner_dir.join("LOCK"))
        .expect("open owner lock");
    // SAFETY: `flock(2)` consumes only the valid borrowed descriptor.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    lock
}

#[cfg(target_os = "linux")]
fn unlock_test_owner(lock: File) {
    // SAFETY: `flock(2)` consumes only the valid borrowed descriptor.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
}

#[cfg(target_os = "linux")]
fn wait_for_lock_wait(tid: libc::c_long) -> bool {
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

struct Temporary {
    path: PathBuf,
}

impl Temporary {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{label}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("temporary directory");
        Self { path }
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
