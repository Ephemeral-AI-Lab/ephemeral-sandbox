use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;
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
    AllocationDescriptor, AllocationHandle, FaultInjector, ManagedProcessTree, OperationId,
    PocError, ReceiptHitSealInput, SCHEMA_VERSION,
};

fn assert_terminal_recovery_rejected(error: PocError) {
    #[cfg(target_os = "linux")]
    assert!(matches!(error, PocError::RecoveryRequired(_)), "{error:?}");
    #[cfg(not(target_os = "linux"))]
    assert!(matches!(error, PocError::Unsupported(_)), "{error:?}");
}

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
    assert!(!prepared.session_dir().join(".mount-unmount").exists());
    let record: sandbox_runtime_mpla_poc::SessionRecord = serde_json::from_slice(
        &fs::read(prepared.session_dir().join("SESSION.json")).expect("read session record"),
    )
    .expect("parse session record");
    assert_eq!(record.session_id, lease.session_id);
    assert_eq!(record.allocation_id, allocation.descriptor.allocation_id);
    assert_eq!(record.phase, sandbox_runtime_mpla_poc::SessionPhase::Open);
    assert_eq!(record.workspace_root, prepared.workspace_root());
}

#[cfg(unix)]
#[test]
fn external_sealing_guard_excludes_recovery_until_released() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    let root = TestDirectory::new("external-sealing-owner-lock");
    let operation = OperationId::from_string("external-sealing-owner-lock");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let prepared = sandbox_runtime_mpla_poc::prepare_external_session(
        &root.0.join("control"),
        &allocation,
        &lease,
    )
    .expect("prepare external session");
    let guard = prepared
        .begin_sealing(
            &allocation,
            &lease,
            &operation,
            &mut FaultInjector::default(),
        )
        .expect("ratify external Sealing");
    assert_eq!(guard.record().operation_id, operation);

    let contender = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(allocation.owner_dir.join("LOCK"))
        .expect("open owner lock contender");
    // SAFETY: flock only consumes the valid borrowed contender descriptor.
    assert_eq!(
        unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        -1
    );
    let error = std::io::Error::last_os_error();
    let raw_error = error.raw_os_error();
    assert!(
        raw_error == Some(libc::EAGAIN) || raw_error == Some(libc::EWOULDBLOCK),
        "unexpected owner lock probe failure: {error}"
    );

    drop(guard);
    // SAFETY: flock only consumes the same valid borrowed contender descriptor.
    assert_eq!(
        unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    // SAFETY: flock only consumes the same valid borrowed contender descriptor.
    assert_eq!(
        unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_UN) },
        0
    );
}

#[cfg(target_os = "linux")]
#[test]
fn public_session_seal_never_follows_rebound_allocation_or_session_names() {
    fn overlay_mount_unavailable(error: &PocError) -> bool {
        match error {
            PocError::Unsupported(message) => {
                message == "Linux statx did not report STATX_MNT_ID_UNIQUE"
            }
            PocError::Io {
                operation, source, ..
            } => {
                matches!(
                    *operation,
                    "open overlay mount context"
                        | "configure overlay lowerdir+"
                        | "configure overlay userxattr"
                        | "configure pinned overlay upper"
                        | "configure pinned overlay work"
                        | "create anchored overlay"
                        | "fsmount anchored overlay"
                        | "attach anchored overlay"
                        | "statx unique mount identity"
                ) && matches!(
                    source.raw_os_error(),
                    Some(libc::EPERM | libc::EACCES | libc::ENOSYS | libc::EOPNOTSUPP)
                )
            }
            _ => false,
        }
    }

    let root = TestDirectory::new("public-session-allocation-authority");
    let live_parent = root.0.join("live-parent");
    let lower = live_parent.join("lower");
    fs::create_dir_all(&lower).expect("create lower layer");
    fs::write(lower.join("lower-sentinel"), b"lower").expect("write lower sentinel");
    let operation = OperationId::from_string("public-session-allocation-authority");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &live_parent.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    fs::write(allocation.upper_dir.join("PINNED"), b"pinned-upper")
        .expect("write pinned upper sentinel");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let control_root = live_parent.join("control");
    let mut session = match sandbox_runtime_mpla_poc::MplaSession::open(
        &control_root,
        allocation.clone(),
        lease.clone(),
        vec![lower],
        None,
    ) {
        Ok(session) => session,
        Err(error) if overlay_mount_unavailable(&error) => return,
        Err(error) => panic!("open public overlay session: {error}"),
    };
    let workspace = session
        .workspace_root()
        .expect("public session has a live workspace")
        .to_path_buf();
    assert!(workspace.join("PINNED").is_file());
    let expected_inventory = capture_inventory(&allocation).expect("inventory pinned upper");
    let session_relative = session
        .session_dir()
        .strip_prefix(&live_parent)
        .expect("session is below the live ancestor")
        .to_path_buf();
    let allocation_relative = allocation
        .allocation_root
        .strip_prefix(&live_parent)
        .expect("allocation is below the live ancestor")
        .to_path_buf();

    let anchored_parent = root.0.join("anchored-parent");
    fs::rename(&live_parent, &anchored_parent).expect("rename live session ancestor");
    for path in [
        &allocation.upper_dir,
        &allocation.work_dir,
        &allocation.owner_dir,
    ] {
        fs::create_dir_all(path).expect("create replacement allocation directory");
    }
    fs::write(
        allocation.allocation_root.join("ALLOCATION.json"),
        serde_json::to_vec(&allocation.descriptor).expect("encode replacement descriptor"),
    )
    .expect("write replacement descriptor");
    fs::write(allocation.owner_dir.join("LOCK"), b"").expect("write replacement owner lock");
    fs::write(allocation.upper_dir.join("DECOY"), b"replacement-upper")
        .expect("write replacement upper sentinel");
    let decoy_session = control_root
        .join("sessions")
        .join(lease.session_id.as_str());
    fs::create_dir_all(decoy_session.join("mount")).expect("create replacement session mountpoint");
    fs::write(decoy_session.join("mount/DECOY"), b"replacement-session")
        .expect("write replacement session sentinel");

    let anchored_session = anchored_parent.join(session_relative);
    match session.seal(&operation, &mut FaultInjector::default()) {
        Ok(sealed) => {
            assert_eq!(
                sealed.first_inventory.inventory_sha256,
                expected_inventory.inventory_sha256
            );
            assert_eq!(
                sealed.second_inventory.inventory_sha256,
                expected_inventory.inventory_sha256
            );
            assert_eq!(sealed.stable.before, expected_inventory.physical);
            assert_eq!(sealed.stable.after, expected_inventory.physical);
            assert!(sealed.first_inventory.entries.iter().any(|entry| {
                entry.relative_path == Path::new("PINNED") && entry.content_sha256.is_some()
            }));
            assert!(!sealed
                .first_inventory
                .entries
                .iter()
                .any(|entry| entry.relative_path == Path::new("DECOY")));
            assert!(anchored_session.join("STABLE.json").is_file());
            assert!(anchored_session.join("QUIESCENCE.json").is_file());
        }
        Err(PocError::RecoveryRequired(_)) => {
            assert!(!anchored_session.join("STABLE.json").exists());
            assert!(!anchored_session.join("QUIESCENCE.json").exists());
        }
        Err(error) => panic!("post-open allocation rebind did not fail closed: {error}"),
    }
    drop(session);

    assert_eq!(
        fs::read(allocation.upper_dir.join("DECOY")).expect("read replacement upper sentinel"),
        b"replacement-upper"
    );
    assert_eq!(
        fs::read(decoy_session.join("mount/DECOY")).expect("read replacement session sentinel"),
        b"replacement-session"
    );
    assert!(!decoy_session.join("SESSION.json").exists());
    assert!(!decoy_session.join("SEALING.json").exists());
    assert!(!decoy_session.join("STABLE.json").exists());
    assert!(!decoy_session.join("QUIESCENCE.json").exists());
    assert!(anchored_session.join("SESSION.json").is_file());
    assert!(anchored_session.join("MOUNT.json").is_file());
    assert!(anchored_parent
        .join(allocation_relative)
        .join("upper/PINNED")
        .is_file());
    assert!(
        !anchored_session.join("mount/PINNED").exists(),
        "public open must retain an exact destructive guard for the renamed workspace"
    );
}

#[cfg(target_os = "linux")]
fn inventory_regression_overlay_unavailable(error: &PocError) -> bool {
    match error {
        PocError::Unsupported(message) => {
            message == "Linux statx did not report STATX_MNT_ID_UNIQUE"
        }
        PocError::Io {
            operation, source, ..
        } => {
            matches!(
                *operation,
                "open overlay mount context"
                    | "configure overlay lowerdir+"
                    | "configure overlay userxattr"
                    | "configure pinned overlay upper"
                    | "configure pinned overlay work"
                    | "create anchored overlay"
                    | "fsmount anchored overlay"
                    | "attach anchored overlay"
                    | "statx unique mount identity"
            ) && matches!(
                source.raw_os_error(),
                Some(libc::EPERM | libc::EACCES | libc::ENOSYS | libc::EOPNOTSUPP)
            )
        }
        _ => false,
    }
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_shortened_lower_accepts_a_total_path_over_255_bytes() {
    use std::os::unix::ffi::OsStrExt;

    let root = TestDirectory::new("long-overlay-lower");
    let lower = root
        .0
        .join("a".repeat(96))
        .join("b".repeat(96))
        .join("c".repeat(96));
    assert!(lower.as_os_str().as_bytes().len() > 255);
    fs::create_dir_all(&lower).expect("create long lower path");
    fs::write(lower.join("LOWER"), b"long-lower").expect("write long lower sentinel");
    let operation = OperationId::from_string("descriptor-shortened-long-lower");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");

    let mut session = match sandbox_runtime_mpla_poc::MplaSession::open(
        &root.0.join("control"),
        allocation,
        lease,
        vec![lower.clone()],
        None,
    ) {
        Ok(session) => session,
        Err(error) if inventory_regression_overlay_unavailable(&error) => return,
        Err(error) => panic!("open overlay with descriptor-shortened lower: {error}"),
    };

    let workspace = session
        .workspace_root()
        .expect("session has a live workspace")
        .to_path_buf();
    assert_eq!(
        fs::read(workspace.join("LOWER")).expect("read long lower sentinel through overlay"),
        b"long-lower"
    );
    session
        .seal(
            &OperationId::from_string("seal-descriptor-shortened-long-lower"),
            &mut FaultInjector::default(),
        )
        .expect("seal overlay with descriptor-shortened lower");
    assert!(workspace.is_dir());
    assert!(!workspace.join("LOWER").exists());
    assert_eq!(
        fs::read(lower.join("LOWER")).expect("reread original long lower sentinel"),
        b"long-lower"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_shortened_lower_rejects_a_symlinked_ancestor() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("symlinked-overlay-lower");
    let real_parent = root.0.join("real-parent");
    let real_lower = real_parent.join("lower");
    fs::create_dir_all(&real_lower).expect("create real lower path");
    let linked_parent = root.0.join("linked-parent");
    symlink(&real_parent, &linked_parent).expect("create lower ancestor symlink");
    let operation = OperationId::from_string("descriptor-shortened-symlinked-lower");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");

    assert!(sandbox_runtime_mpla_poc::MplaSession::open(
        &root.0.join("control"),
        allocation,
        lease,
        vec![linked_parent.join("lower")],
        None,
    )
    .is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_shortened_lower_rejects_post_pin_replacement() {
    let root = TestDirectory::new("replaced-overlay-lower");
    let lower = root.0.join("lower");
    let original = root.0.join("original-lower");
    fs::create_dir(&lower).expect("create original lower");
    fs::write(lower.join("ORIGINAL"), b"original").expect("write original lower sentinel");
    let pinned = match sandbox_runtime_mpla_poc::overlay_adapter::PinnedOverlayLower::pin(&lower) {
        Ok(pinned) => pinned,
        Err(PocError::Unsupported(message))
            if message == "Linux statx did not report STATX_MNT_ID_UNIQUE" =>
        {
            return;
        }
        Err(error) => panic!("pin original lower: {error}"),
    };
    fs::rename(&lower, &original).expect("move pinned lower aside");
    fs::create_dir(&lower).expect("create replacement lower");
    fs::write(lower.join("REPLACEMENT"), b"replacement").expect("write replacement lower sentinel");

    assert!(matches!(
        pinned.revalidate(),
        Err(PocError::RecoveryRequired(_))
    ));
    assert_eq!(
        fs::read(original.join("ORIGINAL")).expect("reread original lower sentinel"),
        b"original"
    );
    assert_eq!(
        fs::read(lower.join("REPLACEMENT")).expect("reread replacement lower sentinel"),
        b"replacement"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn receipt_hit_rejects_an_intermediate_symlink_escape_from_the_pinned_upper() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("receipt-hit-intermediate-symlink");
    let lower = root.0.join("lower");
    fs::create_dir(&lower).expect("create lower layer");
    let allocation_operation = OperationId::from_string("receipt-hit-intermediate-symlink");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &allocation_operation,
    )
    .expect("create permanent allocation");
    let outside = root.0.join("outside");
    fs::create_dir(&outside).expect("create outside directory");
    fs::write(outside.join("file"), b"outside").expect("write outside fixture");
    symlink(&outside, allocation.upper_dir.join("link")).expect("create escaping upper symlink");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &allocation_operation,
    )
    .expect("issue workspace lease");
    let mut session = match sandbox_runtime_mpla_poc::MplaSession::open(
        &root.0.join("control"),
        allocation,
        lease,
        vec![lower],
        None,
    ) {
        Ok(session) => session,
        Err(error) if inventory_regression_overlay_unavailable(&error) => return,
        Err(error) => panic!("open receipt-hit overlay session: {error}"),
    };
    let session_dir = session.session_dir().to_path_buf();
    let affected_stream = root.0.join("affected.records");
    let affected_stream_sha256 = write_affected_stream(
        &affected_stream,
        [RecordMutation::Replace(SemanticRecord::Node(node_record(
            b"link/file",
        )))],
    )
    .expect("write affected stream");
    let input = ReceiptHitSealInput {
        schema_version: SCHEMA_VERSION,
        affected_stream,
        affected_stream_sha256,
        affected_paths: vec![PathBuf::from("link/file")],
    };
    let seal_operation = OperationId::from_string("seal-receipt-hit-intermediate-symlink");

    let result = session.seal_receipt_hit(&seal_operation, &input, &mut FaultInjector::default());

    assert!(matches!(result, Err(PocError::Integrity(_))));
    assert_eq!(
        fs::read(outside.join("file")).expect("reread outside fixture"),
        b"outside"
    );
    assert!(!session_dir.join("STABLE.json").exists());
    assert!(!session_dir.join("QUIESCENCE.json").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn full_seal_rejects_a_directory_entry_replaced_during_descriptor_inventory() {
    use std::os::unix::fs::MetadataExt;

    fn descriptor_is_open(device: u64, inode: u64) -> bool {
        fs::read_dir("/proc/self/fd")
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                fs::metadata(entry.path())
                    .map(|metadata| metadata.dev() == device && metadata.ino() == inode)
                    .unwrap_or(false)
            })
    }

    let root = TestDirectory::new("inventory-directory-replacement");
    let lower = root.0.join("lower");
    fs::create_dir(&lower).expect("create lower layer");
    let allocation_operation = OperationId::from_string("inventory-directory-replacement");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &allocation_operation,
    )
    .expect("create permanent allocation");
    let moving = allocation.upper_dir.join("moving");
    let replacement = allocation.upper_dir.join("replacement");
    fs::create_dir(&moving).expect("create moving directory");
    fs::create_dir(&replacement).expect("create replacement directory");
    let target = moving.join("large");
    fs::File::create(&target)
        .expect("create inventory target")
        .set_len(64 * 1024 * 1024)
        .expect("size inventory target");
    fs::write(replacement.join("replacement"), b"replacement").expect("write replacement fixture");
    fs::File::create(allocation.upper_dir.join("padding"))
        .expect("create inventory padding")
        .set_len(16 * 1024 * 1024)
        .expect("size inventory padding");
    let target_metadata = fs::metadata(&target).expect("stat inventory target");
    let moving_metadata = fs::metadata(&moving).expect("stat moving directory");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &allocation_operation,
    )
    .expect("issue workspace lease");
    let mut session = match sandbox_runtime_mpla_poc::MplaSession::open(
        &root.0.join("control"),
        allocation,
        lease,
        vec![lower],
        None,
    ) {
        Ok(session) => session,
        Err(error) if inventory_regression_overlay_unavailable(&error) => return,
        Err(error) => panic!("open directory-replacement overlay session: {error}"),
    };
    let session_dir = session.session_dir().to_path_buf();
    let target_identity = (target_metadata.dev(), target_metadata.ino());
    let directory_identity = (moving_metadata.dev(), moving_metadata.ino());
    let racer = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !descriptor_is_open(target_identity.0, target_identity.1) {
            assert!(
                std::time::Instant::now() < deadline,
                "inventory never opened the target file descriptor"
            );
            std::thread::yield_now();
        }
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &moving,
            rustix::fs::CWD,
            &replacement,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .expect("exchange inventoried directory entry");
        while descriptor_is_open(target_identity.0, target_identity.1)
            || descriptor_is_open(directory_identity.0, directory_identity.1)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "inventory retained replaced directory descriptors"
            );
            std::thread::yield_now();
        }
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &moving,
            rustix::fs::CWD,
            &replacement,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .expect("restore inventoried directory entry");
    });
    let seal_operation = OperationId::from_string("seal-inventory-directory-replacement");

    let result = session.seal(&seal_operation, &mut FaultInjector::default());
    racer.join().expect("join inventory replacement racer");

    assert!(matches!(result, Err(PocError::RecoveryRequired(_))));
    assert!(!session_dir.join("STABLE.json").exists());
    assert!(!session_dir.join("QUIESCENCE.json").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn strict_unmount_releases_every_target_fd_then_uses_protected_parent_name() {
    use rustix::fd::AsFd;
    use std::os::fd::AsRawFd;

    fn unavailable(error: rustix::io::Errno) -> bool {
        matches!(
            error,
            rustix::io::Errno::PERM
                | rustix::io::Errno::ACCESS
                | rustix::io::Errno::NOSYS
                | rustix::io::Errno::OPNOTSUPP
        )
    }

    let root = TestDirectory::new("strict-unmount-fsmount-reference");
    let workspace = root.0.join("mount");
    fs::create_dir(&workspace).expect("create detached-mount target");
    let covered = rustix::fs::open(
        &workspace,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .expect("pin detached-mount target");
    let covered_identity = rustix::fs::fstat(&covered).expect("stat covered mountpoint");
    let fsfd = match rustix::mount::fsopen("tmpfs", rustix::mount::FsOpenFlags::FSOPEN_CLOEXEC) {
        Ok(fsfd) => fsfd,
        Err(error) if unavailable(error) => return,
        Err(error) => panic!("open tmpfs mount context: {error}"),
    };
    if let Err(error) = rustix::mount::fsconfig_create(fsfd.as_fd()) {
        if unavailable(error) {
            return;
        }
        panic!("create tmpfs mount context: {error}");
    }
    let mounted = match rustix::mount::fsmount(
        fsfd.as_fd(),
        rustix::mount::FsMountFlags::FSMOUNT_CLOEXEC,
        rustix::mount::MountAttrFlags::empty(),
    ) {
        Ok(mounted) => mounted,
        Err(error) if unavailable(error) => return,
        Err(error) => panic!("create detached tmpfs mount: {error}"),
    };
    if let Err(error) = rustix::mount::move_mount(
        mounted.as_fd(),
        "",
        covered.as_fd(),
        "",
        rustix::mount::MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH
            | rustix::mount::MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
    ) {
        if unavailable(error) {
            return;
        }
        panic!("attach detached tmpfs mount: {error}");
    }
    let exact = rustix::fs::open(
        &workspace,
        rustix::fs::OFlags::PATH
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .expect("pin exact O_PATH mount target");
    let exact_workspace = std::ffi::CString::new(format!("/proc/self/fd/{}", exact.as_raw_fd()))
        .expect("encode exact O_PATH mount target");
    let protected_parent = rustix::fs::open(
        &root.0,
        rustix::fs::OFlags::PATH
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .expect("pin protected mount parent");
    let protected_workspace = std::ffi::CString::new(format!(
        "/proc/self/fd/{}/mount",
        protected_parent.as_raw_fd()
    ))
    .expect("encode protected-parent mount target");

    // SAFETY: the exact descriptor path is a live NUL-terminated CString and
    // zero flags request a strict, non-lazy unmount.
    assert_eq!(unsafe { libc::umount2(exact_workspace.as_ptr(), 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBUSY)
    );
    drop(mounted);
    // SAFETY: the exact descriptor path remains a live NUL-terminated CString;
    // its retained target-mount reference must keep strict unmount busy.
    assert_eq!(unsafe { libc::umount2(exact_workspace.as_ptr(), 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBUSY)
    );
    drop(exact);
    // SAFETY: the parent descriptor and fixed child name form a live
    // NUL-terminated CString; no target-mount descriptor remains live and zero
    // flags request a strict, non-lazy unmount.
    assert_eq!(unsafe { libc::umount2(protected_workspace.as_ptr(), 0) }, 0);
    let restored = rustix::fs::open(
        &workspace,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .expect("reopen restored covered mountpoint");
    let restored_identity = rustix::fs::fstat(&restored).expect("stat restored mountpoint");
    assert_eq!(restored_identity.st_dev, covered_identity.st_dev);
    assert_eq!(restored_identity.st_ino, covered_identity.st_ino);
    assert!(workspace.is_dir());
}

#[cfg(target_os = "linux")]
#[test]
fn strict_unmount_exact_descriptor_never_unmounts_stacked_decoy() {
    use rustix::fd::AsFd;
    use std::os::fd::{AsRawFd, OwnedFd};

    fn unavailable(error: rustix::io::Errno) -> bool {
        matches!(
            error,
            rustix::io::Errno::PERM
                | rustix::io::Errno::ACCESS
                | rustix::io::Errno::NOSYS
                | rustix::io::Errno::OPNOTSUPP
        )
    }

    fn detached_tmpfs() -> Result<OwnedFd, rustix::io::Errno> {
        let fsfd = rustix::mount::fsopen("tmpfs", rustix::mount::FsOpenFlags::FSOPEN_CLOEXEC)?;
        rustix::mount::fsconfig_create(fsfd.as_fd())?;
        rustix::mount::fsmount(
            fsfd.as_fd(),
            rustix::mount::FsMountFlags::FSMOUNT_CLOEXEC,
            rustix::mount::MountAttrFlags::empty(),
        )
    }

    let root = TestDirectory::new("strict-unmount-exact-stacked-decoy");
    let workspace = root.0.join("mount");
    fs::create_dir(&workspace).expect("create stacked-mount target");
    let covered = rustix::fs::open(
        &workspace,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .expect("pin covered stacked-mount target");
    let base_mount = match detached_tmpfs() {
        Ok(mount) => mount,
        Err(error) if unavailable(error) => return,
        Err(error) => panic!("create base tmpfs mount: {error}"),
    };
    if let Err(error) = rustix::mount::move_mount(
        base_mount.as_fd(),
        "",
        covered.as_fd(),
        "",
        rustix::mount::MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH
            | rustix::mount::MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
    ) {
        if unavailable(error) {
            return;
        }
        panic!("attach base tmpfs mount: {error}");
    }
    fs::write(workspace.join("BASE"), b"base").expect("write base mount sentinel");
    let exact_base = rustix::fs::open(
        &workspace,
        rustix::fs::OFlags::PATH
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .expect("pin exact base mount");
    let protected_parent = rustix::fs::open(
        &root.0,
        rustix::fs::OFlags::PATH
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .expect("pin protected stacked-mount parent");
    drop(base_mount);

    let decoy_mount = match detached_tmpfs() {
        Ok(mount) => mount,
        Err(error) => panic!("create decoy tmpfs mount: {error}"),
    };
    rustix::mount::move_mount(
        decoy_mount.as_fd(),
        "",
        exact_base.as_fd(),
        "",
        rustix::mount::MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH
            | rustix::mount::MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
    )
    .expect("stack decoy tmpfs over exact base mount");
    fs::write(workspace.join("DECOY"), b"decoy").expect("write decoy mount sentinel");
    drop(decoy_mount);

    let exact_base_path =
        std::ffi::CString::new(format!("/proc/self/fd/{}", exact_base.as_raw_fd()))
            .expect("encode exact base procfd path");
    // SAFETY: the path is a live NUL-terminated CString and zero flags request
    // a strict, non-lazy unmount of the exact base descriptor.
    assert_eq!(unsafe { libc::umount2(exact_base_path.as_ptr(), 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBUSY),
        "the exact base mount must remain busy beneath its stacked child"
    );
    assert_eq!(
        fs::read(workspace.join("DECOY")).expect("read surviving decoy sentinel"),
        b"decoy",
        "exact-descriptor teardown must not unmount the stacked decoy"
    );

    let workspace_path = std::ffi::CString::new(workspace.to_string_lossy().as_bytes())
        .expect("encode stacked-mount target");
    // SAFETY: the path is a live NUL-terminated CString and zero flags request
    // strict cleanup of the top decoy mount.
    assert_eq!(unsafe { libc::umount2(workspace_path.as_ptr(), 0) }, 0);
    assert_eq!(
        fs::read(workspace.join("BASE")).expect("read restored base sentinel"),
        b"base"
    );
    drop(exact_base);
    let protected_workspace = std::ffi::CString::new(format!(
        "/proc/self/fd/{}/mount",
        protected_parent.as_raw_fd()
    ))
    .expect("encode protected-parent stacked-mount target");
    // SAFETY: the authenticated parent descriptor and fixed child name form a
    // live NUL-terminated CString; the exact target descriptor has been
    // released and zero flags request strict cleanup.
    assert_eq!(unsafe { libc::umount2(protected_workspace.as_ptr(), 0) }, 0);
}

#[test]
fn ratified_external_session_cannot_be_reopened_or_rewritten() {
    let root = TestDirectory::new("ratified-session-no-reopen");
    let operation = OperationId::from_string("ratified-session-no-reopen");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let control_root = root.0.join("control");
    let prepared =
        sandbox_runtime_mpla_poc::prepare_external_session(&control_root, &allocation, &lease)
            .expect("prepare external session");
    drop(
        prepared
            .begin_sealing(
                &allocation,
                &lease,
                &operation,
                &mut FaultInjector::default(),
            )
            .expect("ratify Sealing"),
    );
    let record_path = prepared.session_dir().join("SESSION.json");
    let before = fs::read(&record_path).expect("read ratified session record");

    let error =
        sandbox_runtime_mpla_poc::prepare_external_session(&control_root, &allocation, &lease)
            .expect_err("ratified session must not reopen");

    assert!(matches!(error, PocError::RecoveryRequired(_)));
    assert_eq!(
        fs::read(&record_path).expect("reread ratified session record"),
        before
    );
}

#[test]
fn restart_recovery_fails_closed_without_exact_mount_attestation() {
    let root = TestDirectory::new("recovery-needs-mount-attestation");
    let operation = OperationId::from_string("recovery-needs-mount-attestation");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let control_root = root.0.join("control");
    let prepared =
        sandbox_runtime_mpla_poc::prepare_external_session(&control_root, &allocation, &lease)
            .expect("prepare external session");
    let record_path = prepared.session_dir().join("SESSION.json");
    let before = fs::read(&record_path).expect("read open session record");
    let recovery = sandbox_runtime_mpla_poc::SessionSealRecoveryRequest::from_lease(
        &lease,
        operation.clone(),
        operation.clone(),
    );
    let encoded = serde_json::to_string(&recovery).expect("encode secret-free recovery request");
    assert!(!encoded.contains(&lease.writer.nonce));
    assert!(!encoded.contains(&lease.deleter.nonce));

    sandbox_runtime_mpla_poc::session::recover_session_seal(&control_root, &allocation, &recovery)
        .expect_err("recovery without exact mount attestation must fail closed");

    assert_eq!(
        fs::read(&record_path).expect("reread open session record"),
        before
    );
    assert!(!prepared.session_dir().join("SEAL-RECOVERY.json").exists());
    assert!(!prepared.session_dir().join("STABLE.json").exists());
    assert!(!prepared.session_dir().join("QUIESCENCE.json").exists());
    sandbox_runtime_mpla_poc::prepare_external_session(&control_root, &allocation, &lease)
        .expect_err("partial session without mount attestation must not be reopened");
}

#[test]
fn restart_recovery_rejects_a_different_ratified_operation_without_mutation() {
    let root = TestDirectory::new("recovery-operation-mismatch");
    let operation = OperationId::from_string("recovery-operation-mismatch");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let control_root = root.0.join("control");
    let prepared =
        sandbox_runtime_mpla_poc::prepare_external_session(&control_root, &allocation, &lease)
            .expect("prepare external session");
    drop(
        prepared
            .begin_sealing(
                &allocation,
                &lease,
                &operation,
                &mut FaultInjector::default(),
            )
            .expect("ratify Sealing"),
    );
    let sealing_path = prepared.session_dir().join("SEALING.json");
    let before = fs::read(&sealing_path).expect("read Sealing record");

    let recovery = sandbox_runtime_mpla_poc::SessionSealRecoveryRequest::from_lease(
        &lease,
        operation,
        OperationId::from_string("wrong-recovery-operation"),
    );
    let error = sandbox_runtime_mpla_poc::session::recover_session_seal(
        &control_root,
        &allocation,
        &recovery,
    )
    .expect_err("different operation must fail before mount cleanup");

    assert_terminal_recovery_rejected(error);
    assert_eq!(
        fs::read(&sealing_path).expect("reread Sealing record"),
        before
    );
    assert!(!prepared.session_dir().join("SEAL-RECOVERY.json").exists());
}

#[test]
fn restart_recovery_rejects_non_component_identity_before_path_lookup() {
    let root = TestDirectory::new("recovery-component-identity");
    let operation = OperationId::from_string("recovery-component-identity");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let mut recovery = sandbox_runtime_mpla_poc::SessionSealRecoveryRequest::from_lease(
        &lease,
        operation.clone(),
        operation,
    );
    recovery.session_id = sandbox_runtime_mpla_poc::SessionId::from_string("../escape");

    let error = sandbox_runtime_mpla_poc::recover_session_seal(
        &root.0.join("control"),
        &allocation,
        &recovery,
    )
    .expect_err("path-bearing session identity must fail closed");

    assert_terminal_recovery_rejected(error);
    sandbox_runtime_mpla_poc::lease::validate_writer(&allocation.allocation_root, &lease.writer)
        .expect("identity rejection must not fence the active writer");
}

#[cfg(unix)]
#[test]
fn restart_recovery_rejects_symlinked_mount_attestation_before_fencing() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("recovery-symlink-attestation");
    let operation = OperationId::from_string("recovery-symlink-attestation");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let control_root = root.0.join("control");
    let prepared =
        sandbox_runtime_mpla_poc::prepare_external_session(&control_root, &allocation, &lease)
            .expect("prepare external session");
    symlink(
        prepared.session_dir().join("SESSION.json"),
        prepared.session_dir().join("MOUNT.json"),
    )
    .expect("install substituted mount attestation");
    let recovery = sandbox_runtime_mpla_poc::SessionSealRecoveryRequest::from_lease(
        &lease,
        operation.clone(),
        operation,
    );

    let error =
        sandbox_runtime_mpla_poc::recover_session_seal(&control_root, &allocation, &recovery)
            .expect_err("symlinked attestation must fail closed");

    assert_terminal_recovery_rejected(error);
    sandbox_runtime_mpla_poc::lease::validate_writer(&allocation.allocation_root, &lease.writer)
        .expect("attestation rejection must not fence the active writer");
}

#[cfg(unix)]
#[test]
fn restart_recovery_rejects_a_symlinked_control_ancestor_before_fencing() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("recovery-symlink-control-ancestor");
    let operation = OperationId::from_string("recovery-symlink-control-ancestor");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let real_parent = root.0.join("real-parent");
    fs::create_dir(&real_parent).expect("create real control parent");
    let control_root = real_parent.join("control");
    let _prepared =
        sandbox_runtime_mpla_poc::prepare_external_session(&control_root, &allocation, &lease)
            .expect("prepare external session");
    let alias_parent = root.0.join("alias-parent");
    symlink(&real_parent, &alias_parent).expect("plant control ancestor symlink");
    let recovery = sandbox_runtime_mpla_poc::SessionSealRecoveryRequest::from_lease(
        &lease,
        operation.clone(),
        operation,
    );

    let error = sandbox_runtime_mpla_poc::recover_session_seal(
        &alias_parent.join("control"),
        &allocation,
        &recovery,
    )
    .expect_err("symlinked control ancestor must fail closed");

    assert_terminal_recovery_rejected(error);
    sandbox_runtime_mpla_poc::lease::validate_writer(&allocation.allocation_root, &lease.writer)
        .expect("ancestor rejection must happen before terminal fencing");
}

#[cfg(unix)]
#[test]
fn external_sealing_rejects_a_dangling_final_without_reopening() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("dangling-sealing-final");
    let operation = OperationId::from_string("dangling-sealing-final");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let prepared = sandbox_runtime_mpla_poc::prepare_external_session(
        &root.0.join("control"),
        &allocation,
        &lease,
    )
    .expect("prepare external session");
    symlink(
        prepared.session_dir().join("missing-final-target"),
        prepared.session_dir().join("SEALING.json"),
    )
    .expect("plant dangling Sealing final");

    let error = prepared
        .begin_sealing(
            &allocation,
            &lease,
            &operation,
            &mut FaultInjector::default(),
        )
        .expect_err("dangling Sealing final must fail closed");

    assert!(matches!(
        error,
        PocError::RecoveryRequired(_) | PocError::Io { .. }
    ));
    assert!(!prepared
        .session_dir()
        .join(format!(".SEALING.{}.tmp", operation.as_str()))
        .exists());
}

#[cfg(target_os = "linux")]
#[test]
fn unratified_absent_attested_mount_recovery_rejects_without_signaling() {
    use std::os::unix::fs::MetadataExt;
    use std::process::Command;

    let root = TestDirectory::new("recovery-absent-mount-no-signal");
    let operation = OperationId::from_string("recovery-absent-mount-no-signal");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let control_root = root.0.join("control");
    let prepared =
        sandbox_runtime_mpla_poc::prepare_external_session(&control_root, &allocation, &lease)
            .expect("prepare external session");
    let workspace_metadata = fs::metadata(prepared.workspace_root()).expect("stat workspace");
    let namespace_metadata = fs::metadata("/proc/self/ns/mnt").expect("stat mount namespace");
    let attestation = sandbox_runtime_mpla_poc::overlay_adapter::OverlayMountAttestation {
        schema_version: SCHEMA_VERSION,
        allocation_id: lease.allocation_id.clone(),
        session_id: lease.session_id.clone(),
        lease_epoch: lease.lease_epoch,
        owner_epoch: lease.owner_epoch,
        workspace_root: prepared.workspace_root().to_path_buf(),
        allocation_root: allocation.allocation_root.clone(),
        allocation_upper: allocation.upper_dir.clone(),
        allocation_work: allocation.work_dir.clone(),
        allocation_root_device: fs::metadata(&allocation.allocation_root)
            .expect("stat allocation root")
            .dev(),
        allocation_root_inode: fs::metadata(&allocation.allocation_root)
            .expect("stat allocation root")
            .ino(),
        allocation_upper_device: fs::metadata(&allocation.upper_dir)
            .expect("stat allocation upper")
            .dev(),
        allocation_upper_inode: fs::metadata(&allocation.upper_dir)
            .expect("stat allocation upper")
            .ino(),
        allocation_work_device: fs::metadata(&allocation.work_dir)
            .expect("stat allocation work")
            .dev(),
        allocation_work_inode: fs::metadata(&allocation.work_dir)
            .expect("stat allocation work")
            .ino(),
        allocation_owner_device: fs::metadata(&allocation.owner_dir)
            .expect("stat allocation owner")
            .dev(),
        allocation_owner_inode: fs::metadata(&allocation.owner_dir)
            .expect("stat allocation owner")
            .ino(),
        cgroup_procs_path: None,
        cgroup_procs_device: None,
        cgroup_procs_inode: None,
        mount_namespace_inode: namespace_metadata.ino(),
        mount_id: 1,
        mount_unique_id: 1,
        target_device: workspace_metadata.dev(),
        target_inode: workspace_metadata.ino(),
        covered_workspace_device: workspace_metadata.dev(),
        covered_workspace_inode: workspace_metadata.ino(),
        covered_workspace_mount_unique_id: 2,
        filesystem_type: "overlay".to_owned(),
        source: "overlay".to_owned(),
        mount_options: vec!["rw".to_owned()],
        super_options: vec![
            format!("upperdir={}", allocation.upper_dir.display()),
            format!("workdir={}", allocation.work_dir.display()),
        ],
    };
    fs::write(
        prepared.session_dir().join("MOUNT.json"),
        serde_json::to_vec(&attestation).expect("encode absent mount attestation"),
    )
    .expect("write absent mount attestation");
    let mut holder = Command::new("/bin/sleep")
        .arg("5")
        .current_dir(prepared.workspace_root())
        .spawn()
        .expect("spawn workspace holder");
    let recovery = sandbox_runtime_mpla_poc::SessionSealRecoveryRequest::from_lease(
        &lease,
        operation.clone(),
        operation,
    );

    let error =
        sandbox_runtime_mpla_poc::recover_session_seal(&control_root, &allocation, &recovery)
            .expect_err("unratified absent mount must fail before PID targeting");

    assert!(matches!(error, PocError::RecoveryRequired(_)));
    assert!(
        holder.try_wait().expect("poll workspace holder").is_none(),
        "absent-mount replay must not signal an unauthenticated process"
    );
    holder.kill().expect("stop workspace holder");
    holder.wait().expect("reap workspace holder");
}

#[cfg(target_os = "linux")]
#[test]
fn restart_recovery_keeps_the_opened_session_when_its_ancestor_is_replaced() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let root = TestDirectory::new("recovery-ancestor-replacement");
    let operation = OperationId::from_string("recovery-ancestor-replacement");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("payload/allocations"),
        &operation,
    )
    .expect("create permanent allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue workspace lease");
    let live_parent = root.0.join("live-parent");
    fs::create_dir(&live_parent).expect("create live control parent");
    let control_root = live_parent.join("control");
    let prepared =
        sandbox_runtime_mpla_poc::prepare_external_session(&control_root, &allocation, &lease)
            .expect("prepare external session");
    let workspace_metadata = fs::metadata(prepared.workspace_root()).expect("stat workspace");
    let namespace_metadata = fs::metadata("/proc/self/ns/mnt").expect("stat mount namespace");
    let attestation = sandbox_runtime_mpla_poc::overlay_adapter::OverlayMountAttestation {
        schema_version: SCHEMA_VERSION,
        allocation_id: lease.allocation_id.clone(),
        session_id: lease.session_id.clone(),
        lease_epoch: lease.lease_epoch,
        owner_epoch: lease.owner_epoch,
        workspace_root: prepared.workspace_root().to_path_buf(),
        allocation_root: allocation.allocation_root.clone(),
        allocation_upper: allocation.upper_dir.clone(),
        allocation_work: allocation.work_dir.clone(),
        allocation_root_device: fs::metadata(&allocation.allocation_root)
            .expect("stat allocation root")
            .dev(),
        allocation_root_inode: fs::metadata(&allocation.allocation_root)
            .expect("stat allocation root")
            .ino(),
        allocation_upper_device: fs::metadata(&allocation.upper_dir)
            .expect("stat allocation upper")
            .dev(),
        allocation_upper_inode: fs::metadata(&allocation.upper_dir)
            .expect("stat allocation upper")
            .ino(),
        allocation_work_device: fs::metadata(&allocation.work_dir)
            .expect("stat allocation work")
            .dev(),
        allocation_work_inode: fs::metadata(&allocation.work_dir)
            .expect("stat allocation work")
            .ino(),
        allocation_owner_device: fs::metadata(&allocation.owner_dir)
            .expect("stat allocation owner")
            .dev(),
        allocation_owner_inode: fs::metadata(&allocation.owner_dir)
            .expect("stat allocation owner")
            .ino(),
        cgroup_procs_path: None,
        cgroup_procs_device: None,
        cgroup_procs_inode: None,
        mount_namespace_inode: namespace_metadata.ino(),
        mount_id: u64::MAX,
        mount_unique_id: u64::MAX,
        target_device: workspace_metadata.dev(),
        target_inode: workspace_metadata.ino(),
        covered_workspace_device: workspace_metadata.dev(),
        covered_workspace_inode: workspace_metadata.ino(),
        covered_workspace_mount_unique_id: u64::MAX - 1,
        filesystem_type: "overlay".to_owned(),
        source: "overlay".to_owned(),
        mount_options: vec!["rw".to_owned()],
        super_options: vec![
            format!("upperdir={}", allocation.upper_dir.display()),
            format!("workdir={}", allocation.work_dir.display()),
        ],
    };
    fs::write(
        prepared.session_dir().join("MOUNT.json"),
        serde_json::to_vec(&attestation).expect("encode absent mount attestation"),
    )
    .expect("write absent mount attestation");
    let recovery = sandbox_runtime_mpla_poc::SessionSealRecoveryRequest::from_lease(
        &lease,
        operation.clone(),
        operation,
    );
    let owner_lock_path = allocation.owner_dir.join("LOCK");
    let owner_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&owner_lock_path)
        .expect("open owner lock");
    // SAFETY: `flock(2)` consumes only the valid borrowed descriptor.
    assert_eq!(
        unsafe { libc::flock(owner_lock.as_raw_fd(), libc::LOCK_EX) },
        0
    );
    let (tid_sender, tid_receiver) = std::sync::mpsc::channel();
    let recovery_control_root = control_root.clone();
    let recovery_allocation = allocation.clone();
    let recovery_thread = std::thread::spawn(move || {
        // SAFETY: `gettid` has no pointers or preconditions.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        tid_sender.send(tid).expect("publish recovery thread ID");
        sandbox_runtime_mpla_poc::recover_session_seal(
            &recovery_control_root,
            &recovery_allocation,
            &recovery,
        )
    });
    let recovery_tid = tid_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("receive recovery thread ID");
    let wchan_path = PathBuf::from(format!("/proc/self/task/{recovery_tid}/wchan"));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let blocked_after_open = loop {
        let wchan = fs::read_to_string(&wchan_path).unwrap_or_default();
        if wchan.contains("lock") && wchan.contains("wait") {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let anchored_parent = root.0.join("anchored-parent");
    if blocked_after_open {
        fs::rename(&live_parent, &anchored_parent).expect("rename opened control ancestor");
        let decoy_session = control_root
            .join("sessions")
            .join(lease.session_id.as_str());
        fs::create_dir_all(decoy_session.join("mount")).expect("create replacement session");
        fs::write(decoy_session.join("SESSION.json"), b"{}")
            .expect("write replacement session record");
    }
    // SAFETY: `flock(2)` consumes only the valid borrowed descriptor.
    assert_eq!(
        unsafe { libc::flock(owner_lock.as_raw_fd(), libc::LOCK_UN) },
        0
    );
    drop(owner_lock);
    let recovery_result = recovery_thread.join().expect("join recovery thread");

    assert!(
        blocked_after_open,
        "recovery never reached the held owner lock"
    );
    assert!(matches!(
        recovery_result,
        Err(PocError::RecoveryRequired(_))
    ));
    assert!(
        sandbox_runtime_mpla_poc::lease::validate_writer(
            &allocation.allocation_root,
            &lease.writer,
        )
        .is_err(),
        "recovery must fence from the descriptor-pinned original session"
    );
    let decoy_session = control_root
        .join("sessions")
        .join(lease.session_id.as_str());
    assert_eq!(
        fs::read(decoy_session.join("SESSION.json")).expect("read replacement session record"),
        b"{}"
    );
    assert!(!decoy_session.join("SEAL-RECOVERY.json").exists());
    assert!(anchored_parent
        .join("control/sessions")
        .join(lease.session_id.as_str())
        .join("MOUNT.json")
        .is_file());
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mpla-poc-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create test directory");
        Self(fs::canonicalize(path).expect("canonicalize test directory"))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn post_freeze_double_inventory_captures_pre_freeze_mutation_and_detects_later_mutation() {
    let root = TestDirectory::new("inventory");
    let allocation = allocation_handle(&root.0);
    fs::create_dir_all(allocation.upper_dir.join("nested")).expect("create nested directory");
    fs::write(allocation.upper_dir.join("nested/file"), b"first").expect("write fixture");
    let pre_freeze = capture_inventory(&allocation).expect("capture pre-freeze inventory");

    fs::write(
        allocation.upper_dir.join("nested/file"),
        b"late-before-freeze",
    )
    .expect("complete late pre-freeze mutation");

    let (before, after) = capture_stable_pair(&allocation).expect("stable inventory");
    assert_eq!(before, after);
    assert_eq!(before.physical.file_count, 1);
    assert_eq!(before.physical.logical_bytes, 18);
    assert_ne!(pre_freeze.inventory_sha256, before.inventory_sha256);

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
    let mut tree =
        ManagedProcessTree::new(root.0.clone(), None).expect("create managed process tree");
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
    let mut tree =
        ManagedProcessTree::new(root.0.clone(), None).expect("create managed process tree");

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

#[cfg(target_os = "linux")]
#[test]
fn managed_process_tree_rejects_a_live_cgroup_membership_leaf_swap() {
    let root = TestDirectory::new("managed-process-cgroup-leaf-swap");
    let cgroup = root.0.join("cgroup");
    let membership = cgroup.join("cgroup.procs");
    let original = cgroup.join("cgroup.procs.original");
    fs::create_dir(&cgroup).expect("create cgroup fixture");
    fs::write(&membership, b"").expect("create membership fixture");
    let mut tree = ManagedProcessTree::new(root.0.clone(), Some(membership.clone()))
        .expect("pin live cgroup membership");
    fs::rename(&membership, &original).expect("swap out pinned membership leaf");
    fs::write(&membership, b"").expect("plant membership decoy");

    assert!(matches!(
        tree.audit(false),
        Err(PocError::RecoveryRequired(_))
    ));
    assert!(matches!(
        tree.run(
            std::path::Path::new("/bin/true"),
            &[],
            Duration::from_secs(1),
        ),
        Err(PocError::RecoveryRequired(_))
    ));
    assert_eq!(fs::read(&membership).expect("read membership decoy"), b"");
    assert!(original.is_file());
}

#[cfg(target_os = "linux")]
#[test]
fn managed_process_tree_scopes_audit_to_exact_attested_cgroup_members() {
    let root = TestDirectory::new("managed-process-cgroup-scoped-audit");
    let cgroup = root.0.join("cgroup");
    let membership = cgroup.join("cgroup.procs");
    fs::create_dir(&cgroup).expect("create cgroup fixture");
    fs::write(&membership, format!("{}\n", std::process::id()))
        .expect("create exact membership fixture");
    let tree = ManagedProcessTree::new(root.0.clone(), Some(membership))
        .expect("pin exact cgroup membership");

    let audit = tree.audit(false).expect("audit exact cgroup members");
    assert!(audit.is_clear(), "{audit:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn managed_process_tree_does_not_classify_exact_cgroup_members_through_procfs() {
    let root = TestDirectory::new("managed-process-cgroup-without-procfs");
    let cgroup = root.0.join("cgroup");
    let membership = cgroup.join("cgroup.procs");
    fs::create_dir(&cgroup).expect("create cgroup fixture");
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn exact cgroup member");
    let child_pid = i32::try_from(child.id()).expect("child PID fits i32");
    fs::write(&membership, format!("{child_pid}\n")).expect("create exact membership fixture");
    let tree = ManagedProcessTree::new(PathBuf::from("/"), Some(membership))
        .expect("pin exact cgroup membership");

    let audit_result = tree.audit(false);
    child.kill().expect("kill exact cgroup member");
    child.wait().expect("reap exact cgroup member");

    let audit = audit_result.expect("audit exact cgroup member without procfs classification");
    assert_eq!(audit.cgroup_members, vec![child_pid]);
    assert!(!audit.is_clear(), "{audit:?}");
    assert!(audit.cwd_or_root_pins.is_empty(), "{audit:?}");
    assert!(audit.fd_pins.is_empty(), "{audit:?}");
    assert!(audit.writable_map_pins.is_empty(), "{audit:?}");
    assert!(audit.mount_namespace_pins.is_empty(), "{audit:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn managed_process_tree_rejects_malformed_or_nonpositive_cgroup_members() {
    for (case, members) in [("malformed", "not-a-pid\n"), ("zero", "0\n")] {
        let root = TestDirectory::new(&format!("managed-process-cgroup-{case}"));
        let cgroup = root.0.join("cgroup");
        let membership = cgroup.join("cgroup.procs");
        fs::create_dir(&cgroup).expect("create cgroup fixture");
        fs::write(&membership, members).expect("create invalid membership fixture");
        let tree = ManagedProcessTree::new(root.0.clone(), Some(membership))
            .expect("pin invalid cgroup membership for audit");

        assert!(matches!(
            tree.audit(false),
            Err(PocError::RecoveryRequired(_))
        ));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn process_audit_parsers_are_component_aware_and_decode_mountinfo_escapes() {
    use sandbox_runtime_mpla_poc::process_tree::{
        map_line_has_writable_pin, mountinfo_line_has_mount,
    };

    let workspace = std::path::Path::new("/srv/sessions/exact/mount");
    assert!(map_line_has_writable_pin(
        "7f00-7f10 rw-s 00000000 00:01 1 /srv/sessions/exact/mount/data",
        workspace,
    ));
    assert!(map_line_has_writable_pin(
        "7f00-7f10 rw-p 00000000 00:01 1 /srv/sessions/exact/mount/file with spaces (deleted)",
        workspace,
    ));
    assert!(!map_line_has_writable_pin(
        "7f00-7f10 rw-s 00000000 00:01 1 /srv/sessions/exact/mount-other/data",
        workspace,
    ));
    assert!(!map_line_has_writable_pin(
        "7f00-7f10 r--s 00000000 00:01 1 /srv/sessions/exact/mount/data",
        workspace,
    ));
    assert!(!map_line_has_writable_pin(
        "7f00-7f10 rw-p 00000000 00:00 0 [heap]",
        workspace,
    ));

    let escaped_workspace = std::path::Path::new("/srv/session root/tab\\slash");
    assert!(mountinfo_line_has_mount(
        "42 24 0:40 / /srv/session\\040root/tab\\134slash rw - overlay overlay rw",
        escaped_workspace,
    )
    .expect("decode exact escaped mountpoint"));
    assert!(!mountinfo_line_has_mount(
        "42 24 0:40 / /srv/session\\040root/tab\\134slash-other rw - overlay overlay rw",
        escaped_workspace,
    )
    .expect("reject escaped sibling mountpoint"));
    mountinfo_line_has_mount(
        "42 24 0:40 / /srv/session\\04 rw - overlay overlay rw",
        escaped_workspace,
    )
    .expect_err("truncated mountinfo escape must fail closed");
}

#[cfg(target_os = "linux")]
#[test]
fn attested_cgroup_reopen_survives_parent_rename_and_rejects_member_replacement() {
    use sandbox_runtime_mpla_poc::process_tree::AttestedCgroupMembership;
    use std::os::unix::fs::{symlink, MetadataExt};

    let root = TestDirectory::new("cgroup-identity");
    let original_directory = root.0.join("original-cgroup");
    let moved_directory = root.0.join("moved-cgroup");
    fs::create_dir(&original_directory).expect("create original cgroup directory");
    let path = original_directory.join("cgroup.procs");
    fs::write(&path, b"123\n").expect("write original cgroup membership");
    let metadata = fs::metadata(&path).expect("stat original cgroup membership");
    let attested = AttestedCgroupMembership::open(&path, metadata.dev(), metadata.ino())
        .expect("open exact cgroup membership");

    fs::rename(&original_directory, &moved_directory).expect("rename cgroup directory");
    fs::create_dir(&original_directory).expect("create replacement cgroup directory");
    fs::write(&path, b"456\n").expect("write replacement path membership");
    assert_eq!(
        attested
            .read_exact()
            .expect("read membership from pinned renamed cgroup"),
        "123\n"
    );

    fs::remove_file(moved_directory.join("cgroup.procs")).expect("remove pinned cgroup membership");
    attested
        .read_exact()
        .expect_err("missing membership under pinned cgroup must fail closed");

    let symlink_path = moved_directory.join("cgroup.procs");
    symlink(&path, &symlink_path).expect("plant member symlink under pinned cgroup");
    attested
        .read_exact()
        .expect_err("symlinked membership under pinned cgroup must fail closed");

    AttestedCgroupMembership::open(
        &root.0.join("missing/cgroup.procs"),
        metadata.dev(),
        metadata.ino(),
    )
    .expect_err("initially missing cgroup membership must fail closed");
}

#[cfg(target_os = "linux")]
#[test]
fn attested_mount_tree_refresh_includes_late_descendants_and_bind_mounts_only() {
    use sandbox_runtime_mpla_poc::overlay_adapter::{
        attested_mount_tree_ids_from_mountinfo, frozen_mount_operation_requires_retry,
        require_attested_mount_tree_read_only_from_mountinfo,
        require_attested_mount_unstacked_from_mountinfo, OverlayMountAttestation,
    };
    use std::os::unix::fs::MetadataExt;

    let root = TestDirectory::new("dynamic-attested-mount-tree");
    let operation = OperationId::from_string("dynamic-attested-mount-tree");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("allocations"),
        &operation,
    )
    .expect("create mount-tree allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue mount-tree lease");
    let workspace_root = root.0.join("session/mount");
    let super_options = format!(
        "rw,lowerdir=/lower,upperdir={},workdir={}",
        allocation.upper_dir.display(),
        allocation.work_dir.display()
    );
    let attestation = OverlayMountAttestation {
        schema_version: SCHEMA_VERSION,
        allocation_id: lease.allocation_id.clone(),
        session_id: lease.session_id.clone(),
        lease_epoch: lease.lease_epoch,
        owner_epoch: lease.owner_epoch,
        workspace_root: workspace_root.clone(),
        allocation_root: allocation.allocation_root.clone(),
        allocation_upper: allocation.upper_dir.clone(),
        allocation_work: allocation.work_dir.clone(),
        allocation_root_device: fs::metadata(&allocation.allocation_root)
            .expect("stat allocation root")
            .dev(),
        allocation_root_inode: fs::metadata(&allocation.allocation_root)
            .expect("stat allocation root")
            .ino(),
        allocation_upper_device: fs::metadata(&allocation.upper_dir)
            .expect("stat allocation upper")
            .dev(),
        allocation_upper_inode: fs::metadata(&allocation.upper_dir)
            .expect("stat allocation upper")
            .ino(),
        allocation_work_device: fs::metadata(&allocation.work_dir)
            .expect("stat allocation work")
            .dev(),
        allocation_work_inode: fs::metadata(&allocation.work_dir)
            .expect("stat allocation work")
            .ino(),
        allocation_owner_device: fs::metadata(&allocation.owner_dir)
            .expect("stat allocation owner")
            .dev(),
        allocation_owner_inode: fs::metadata(&allocation.owner_dir)
            .expect("stat allocation owner")
            .ino(),
        cgroup_procs_path: None,
        cgroup_procs_device: None,
        cgroup_procs_inode: None,
        mount_namespace_inode: 1,
        mount_id: 42,
        mount_unique_id: 4200,
        target_device: libc::makedev(0, 55) as u64,
        target_inode: 1,
        covered_workspace_device: 1,
        covered_workspace_inode: 1,
        covered_workspace_mount_unique_id: 4100,
        filesystem_type: "overlay".to_owned(),
        source: "overlay".to_owned(),
        mount_options: vec!["rw".to_owned()],
        super_options: super_options.split(',').map(str::to_owned).collect(),
    };
    let mountinfo = format!(
        "1 0 0:1 / / rw - ext4 /dev/root rw\n\
         42 1 0:55 / {} rw - overlay overlay {}\n\
         43 42 0:99 / /late-child rw - tmpfs tmpfs rw\n\
         77 1 0:55 /subtree /elsewhere ro - overlay overlay {}\n\
         78 77 0:100 / /bind-child rw - tmpfs tmpfs rw\n\
         90 1 0:56 / /unrelated rw - overlay overlay rw,lowerdir=/lower,upperdir=/other/upper,workdir=/other/work\n\
         91 90 0:101 / /unrelated-child rw - tmpfs tmpfs rw",
        workspace_root.display(),
        super_options,
        super_options,
    );

    let ids = attested_mount_tree_ids_from_mountinfo(&mountinfo, &attestation)
        .expect("derive current attested mount tree");
    assert_eq!(ids.into_iter().collect::<Vec<_>>(), vec![42, 43, 77, 78]);
    require_attested_mount_unstacked_from_mountinfo(&mountinfo, &attestation)
        .expect("accept the single exact protected mount name");
    let stacked = format!(
        "{mountinfo}\n92 1 0:99 / {} rw - tmpfs tmpfs rw",
        workspace_root.display()
    );
    require_attested_mount_unstacked_from_mountinfo(&stacked, &attestation)
        .expect_err("a stacked overmount must fail closed");
    let replacement = mountinfo.replace(
        &format!("42 1 0:55 / {} rw", workspace_root.display()),
        &format!("92 1 0:99 / {} rw", workspace_root.display()),
    );
    require_attested_mount_unstacked_from_mountinfo(&replacement, &attestation)
        .expect_err("a replacement mount must fail closed");
    require_attested_mount_tree_read_only_from_mountinfo(&mountinfo, &attestation)
        .expect_err("pre-freeze writable state must not pass the stable audit boundary");

    let frozen_mountinfo = mountinfo
        .replace(
            &format!("42 1 0:55 / {} rw - overlay", workspace_root.display()),
            &format!("42 1 0:55 / {} ro - overlay", workspace_root.display()),
        )
        .replace(
            "43 42 0:99 / /late-child rw - tmpfs",
            "43 42 0:99 / /late-child ro - tmpfs",
        )
        .replace(
            "78 77 0:100 / /bind-child rw - tmpfs",
            "78 77 0:100 / /bind-child ro - tmpfs",
        );
    require_attested_mount_tree_read_only_from_mountinfo(&frozen_mountinfo, &attestation)
        .expect("already-frozen restart must authenticate the complete mount tree");
    let late_writable_bind = frozen_mountinfo.replace(
        "78 77 0:100 / /bind-child ro - tmpfs",
        "78 77 0:100 / /bind-child rw - tmpfs",
    );
    require_attested_mount_tree_read_only_from_mountinfo(&late_writable_bind, &attestation)
        .expect_err("a late writable bind must invalidate the post-freeze proof");
    assert!(frozen_mount_operation_requires_retry(libc::EBUSY));
    assert!(!frozen_mount_operation_requires_retry(libc::EPERM));

    let collision = mountinfo.replace(
        &format!(
            "42 1 0:55 / {} rw - overlay overlay {}",
            workspace_root.display(),
            super_options
        ),
        &format!(
            "42 1 0:66 / {} rw - tmpfs tmpfs rw",
            workspace_root.display()
        ),
    );
    attested_mount_tree_ids_from_mountinfo(&collision, &attestation)
        .expect_err("reused attested mount ID must fail closed");
}

#[cfg(unix)]
#[test]
fn sealing_publication_rejects_preplanted_temporary_entries() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("sealing-temporary-collisions");
    let operation = OperationId::from_string("sealing-temporary-collisions");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("allocations"),
        &operation,
    )
    .expect("create allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::from_string("sealing-temporary-session"),
        &operation,
    )
    .expect("issue workspace lease");
    let session_dir = root.0.join("session");
    fs::create_dir(&session_dir).expect("create session directory");
    let temporary = session_dir.join(format!(".SEALING.{}.tmp", operation.as_str()));
    let victim = root.0.join("victim");
    fs::write(&victim, b"must remain intact").expect("write temporary symlink victim");
    symlink(&victim, &temporary).expect("plant temporary symlink");

    sandbox_runtime_mpla_poc::quiesce::persist_sealing(
        &session_dir,
        &operation,
        &lease,
        &allocation.upper_dir,
        &mut sandbox_runtime_mpla_poc::NamedFaultInjector::default(),
    )
    .expect_err("preplanted temporary symlink must fail closed");
    assert_eq!(
        fs::read(&victim).expect("reread victim"),
        b"must remain intact"
    );

    fs::remove_file(&temporary).expect("remove temporary symlink");
    fs::write(&temporary, b"stale").expect("plant stale temporary");
    sandbox_runtime_mpla_poc::quiesce::persist_sealing(
        &session_dir,
        &operation,
        &lease,
        &allocation.upper_dir,
        &mut sandbox_runtime_mpla_poc::NamedFaultInjector::default(),
    )
    .expect_err("stale regular temporary must not be overwritten");
    assert_eq!(
        fs::read(&temporary).expect("reread stale temporary"),
        b"stale"
    );
}

#[cfg(unix)]
#[test]
fn sealing_publication_never_replaces_symlink_or_corrupt_final() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("sealing-final-collisions");
    let operation = OperationId::from_string("sealing-final-collisions");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("allocations"),
        &operation,
    )
    .expect("create allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::from_string("sealing-final-session"),
        &operation,
    )
    .expect("issue workspace lease");
    let session_dir = root.0.join("session");
    fs::create_dir(&session_dir).expect("create session directory");
    let final_path = session_dir.join("SEALING.json");
    let victim = root.0.join("victim");
    fs::write(&victim, b"must remain intact").expect("write final symlink victim");
    symlink(&victim, &final_path).expect("plant final symlink");

    sandbox_runtime_mpla_poc::quiesce::persist_sealing(
        &session_dir,
        &operation,
        &lease,
        &allocation.upper_dir,
        &mut sandbox_runtime_mpla_poc::NamedFaultInjector::default(),
    )
    .expect_err("final symlink must fail closed");
    assert_eq!(
        fs::read(&victim).expect("reread victim"),
        b"must remain intact"
    );

    fs::remove_file(&final_path).expect("remove final symlink");
    fs::write(&final_path, b"not-json").expect("plant corrupt final");
    sandbox_runtime_mpla_poc::quiesce::persist_sealing(
        &session_dir,
        &operation,
        &lease,
        &allocation.upper_dir,
        &mut sandbox_runtime_mpla_poc::NamedFaultInjector::default(),
    )
    .expect_err("corrupt final must fail closed");
    assert_eq!(
        fs::read(&final_path).expect("reread corrupt final"),
        b"not-json"
    );
}

#[cfg(unix)]
#[test]
fn post_file_fsync_retry_preserves_stale_sealing_temporary() {
    let root = TestDirectory::new("post-fsync-retry");
    let operation = OperationId::from_string("post-fsync-retry");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.0.join("allocations"),
        &operation,
    )
    .expect("create allocation");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::from_string("post-fsync-retry-session"),
        &operation,
    )
    .expect("issue workspace lease");
    let session_dir = root.0.join("session");
    fs::create_dir(&session_dir).expect("create session directory");
    let mut faults = sandbox_runtime_mpla_poc::NamedFaultInjector::armed([(
        sandbox_runtime_mpla_poc::NamedFaultPoint::SealingAfterFileFsync,
        1,
    )]);

    sandbox_runtime_mpla_poc::quiesce::persist_sealing(
        &session_dir,
        &operation,
        &lease,
        &allocation.upper_dir,
        &mut faults,
    )
    .expect_err("post-file-fsync fault must interrupt publication");
    let temporary = session_dir.join(format!(".SEALING.{}.tmp", operation.as_str()));
    let before = fs::read(&temporary).expect("read durable stale temporary");

    sandbox_runtime_mpla_poc::quiesce::persist_sealing(
        &session_dir,
        &operation,
        &lease,
        &allocation.upper_dir,
        &mut sandbox_runtime_mpla_poc::NamedFaultInjector::default(),
    )
    .expect_err("retry must not overwrite the stale temporary");
    assert_eq!(
        fs::read(&temporary).expect("reread stale temporary"),
        before
    );
    assert!(!session_dir.join("SEALING.json").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn live_seal_retains_recovery_lock_at_strict_unmount() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::{Command, Stdio};

    let root = TestDirectory::new("seal-lock-at-strict-unmount");
    let marker_path = root.0.join("strict-unmount-marker.json");
    let stderr_path = root.0.join("seal-child.stderr");
    let stderr = std::fs::File::create(&stderr_path).expect("create seal child stderr");
    let mut child = Command::new(std::env::current_exe().expect("locate integration test binary"))
        .args([
            "--ignored",
            "--exact",
            "live_seal_recovery_lock_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MPLA_POC_SEAL_LOCK_TEST_ROOT", &root.0)
        .env(
            "MPLA_POC_PHYSICAL_FAULT_POINT",
            sandbox_runtime_mpla_poc::NamedFaultPoint::UnmountBeforeStrict.as_str(),
        )
        .env("MPLA_POC_PHYSICAL_FAULT_ORDINAL", "1")
        .env("MPLA_POC_PHYSICAL_FAULT_ARMED_PATH", &marker_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn seal child");
    let child_pid = i32::try_from(child.id()).expect("seal child PID fits pid_t");
    let mut stopped_status = 0;
    // SAFETY: child_pid names the live child created immediately above, and
    // stopped_status is writable for the duration of waitpid.
    let waited = unsafe { libc::waitpid(child_pid, &mut stopped_status, libc::WUNTRACED) };
    if waited == child_pid && libc::WIFEXITED(stopped_status) {
        let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
        if libc::WEXITSTATUS(stopped_status) == 0 {
            return;
        }
        panic!("seal child exited before strict unmount: {stderr}");
    }
    if waited != child_pid
        || !libc::WIFSTOPPED(stopped_status)
        || libc::WSTOPSIG(stopped_status) != libc::SIGSTOP
    {
        let _ = child.kill();
        let _ = child.wait();
        let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
        panic!(
            "seal child did not stop at strict unmount: waited={waited} status={stopped_status} stderr={stderr}"
        );
    }

    let lock_observation = (|| -> Result<(), String> {
        if !marker_path.is_file() {
            return Err("physical strict-unmount marker was not published".to_owned());
        }
        let lock_path = PathBuf::from(
            fs::read_to_string(root.0.join("owner-lock-path"))
                .map_err(|error| format!("read owner lock handshake: {error}"))?,
        );
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|error| format!("open owner lock handshake path: {error}"))?;
        // SAFETY: flock only reads the valid descriptor borrowed from lock.
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            // SAFETY: flock only reads the same valid descriptor borrowed from lock.
            let _ = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
            return Err("recovery acquired owner lock during live strict unmount".to_owned());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(format!("owner lock probe failed unexpectedly: {error}"));
        }
        Ok(())
    })();

    // SAFETY: child_pid remains the stopped, unreaped child observed above.
    assert_eq!(unsafe { libc::kill(child_pid, libc::SIGCONT) }, 0);
    let completed = child.wait().expect("wait for resumed seal child");
    let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
    assert!(
        completed.success(),
        "resumed seal child failed strict teardown: {completed:?} stderr={stderr}"
    );
    lock_observation.expect("seal must exclude terminal recovery through strict unmount");
}

#[cfg(target_os = "linux")]
#[ignore]
#[test]
fn live_seal_recovery_lock_child() {
    let Some(root) = std::env::var_os("MPLA_POC_SEAL_LOCK_TEST_ROOT").map(PathBuf::from) else {
        return;
    };
    let lower = root.join("lower");
    fs::create_dir_all(&lower).expect("create seal child lower");
    fs::write(lower.join("LOWER"), b"lower").expect("write seal child lower");
    let operation = OperationId::from_string("seal-lock-at-strict-unmount");
    let allocation = sandbox_runtime_mpla_poc::allocation::create_allocation(
        &root.join("payload/allocations"),
        &operation,
    )
    .expect("create seal child allocation");
    fs::write(
        root.join("owner-lock-path"),
        allocation
            .owner_dir
            .join("LOCK")
            .to_string_lossy()
            .as_bytes(),
    )
    .expect("write owner lock handshake");
    let lease = sandbox_runtime_mpla_poc::lease::issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &operation,
    )
    .expect("issue seal child lease");
    let mut session = match sandbox_runtime_mpla_poc::MplaSession::open(
        &root.join("control"),
        allocation,
        lease,
        vec![lower],
        None,
    ) {
        Ok(session) => session,
        Err(error) if seal_lock_overlay_mount_unavailable(&error) => return,
        Err(error) => panic!("open seal child session: {error}"),
    };
    session
        .seal(&operation, &mut FaultInjector::default())
        .expect("seal child completes after strict-unmount boundary resumes");
}

#[cfg(target_os = "linux")]
fn seal_lock_overlay_mount_unavailable(error: &PocError) -> bool {
    match error {
        PocError::Unsupported(message) => {
            message == "Linux statx did not report STATX_MNT_ID_UNIQUE"
        }
        PocError::Io { source, .. } => matches!(
            source.raw_os_error(),
            Some(libc::EPERM | libc::EACCES | libc::ENOSYS | libc::EOPNOTSUPP)
        ),
        _ => false,
    }
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
