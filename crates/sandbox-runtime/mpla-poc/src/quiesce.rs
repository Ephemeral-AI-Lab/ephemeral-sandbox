use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fault::{FaultInjector, FaultPoint};
use crate::inventory::{capture_inventory, capture_physical_witness, AllocationInventory};
#[cfg(target_os = "linux")]
use crate::inventory::{capture_inventory_anchored, capture_physical_witness_anchored};
use crate::overlay_adapter::{PermanentOverlayMount, UnmountedOverlay};
use crate::process_tree::{
    live_workspace_audit_identity, AnchoredWorkspaceAuditIdentity, ManagedProcessTree, ProcessAudit,
};
use crate::recovery::reach_real_operation;
use crate::{
    durable, unix_time_ms, AllocationHandle, MutableLease, NamedFaultInjector, NamedFaultPoint,
    OperationId, PocError, PocResult, SessionId, StableAllocationReceipt, SCHEMA_VERSION,
};

enum AllocationStabilizationSource<'a> {
    Lexical(std::marker::PhantomData<&'a ()>),
    #[cfg(target_os = "linux")]
    Anchored(&'a crate::session::AnchoredAllocationAuthority),
}

impl AllocationStabilizationSource<'_> {
    fn revalidate(&self, allocation: &AllocationHandle) -> PocResult<()> {
        match self {
            Self::Lexical(_) => Ok(()),
            #[cfg(target_os = "linux")]
            Self::Anchored(authority) => authority.revalidate(allocation),
        }
    }

    fn root_path(&self, allocation: &AllocationHandle) -> PathBuf {
        match self {
            Self::Lexical(_) => allocation.allocation_root.clone(),
            #[cfg(target_os = "linux")]
            Self::Anchored(authority) => authority.root_path(),
        }
    }

    fn upper_path(&self, allocation: &AllocationHandle) -> PathBuf {
        match self {
            Self::Lexical(_) => allocation.upper_dir.clone(),
            #[cfg(target_os = "linux")]
            Self::Anchored(authority) => authority.upper_path(),
        }
    }

    fn sync(&self, allocation: &AllocationHandle) -> PocResult<()> {
        match self {
            Self::Lexical(_) => {
                syncfs_path(&allocation.upper_dir)?;
                sync_directory(&allocation.owner_dir)
            }
            #[cfg(target_os = "linux")]
            Self::Anchored(authority) => {
                syncfs_descriptor(authority.upper(), &allocation.upper_dir)?;
                rustix::fs::fsync(authority.owner()).map_err(|error| {
                    PocError::io(
                        "fsync pinned allocation owner",
                        &allocation.owner_dir,
                        std::io::Error::from(error),
                    )
                })
            }
        }
    }

    fn capture_inventory(&self, allocation: &AllocationHandle) -> PocResult<AllocationInventory> {
        match self {
            Self::Lexical(_) => capture_inventory(allocation),
            #[cfg(target_os = "linux")]
            Self::Anchored(authority) => capture_inventory_anchored(allocation, authority.upper()),
        }
    }

    fn capture_physical_witness(
        &self,
        allocation: &AllocationHandle,
        affected_paths: &[PathBuf],
    ) -> PocResult<crate::PhysicalSnapshot> {
        match self {
            Self::Lexical(_) => capture_physical_witness(allocation, affected_paths),
            #[cfg(target_os = "linux")]
            Self::Anchored(authority) => {
                capture_physical_witness_anchored(allocation, authority.upper(), affected_paths)
            }
        }
    }
}

const AUDIT_RETRY_BUDGET: Duration = Duration::from_secs(1);
const AUDIT_RETRY_DELAY: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SealingRecord {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub session_id: SessionId,
    pub allocation_id: crate::AllocationId,
    pub lease_epoch: u64,
    pub owner_epoch: u64,
    pub durable_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuiescenceReceipt {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub session_id: SessionId,
    pub killed_or_signaled_pids: Vec<i32>,
    pub pre_unmount_audit: ProcessAudit,
    pub post_unmount_audit: ProcessAudit,
    pub workspace_root: PathBuf,
    pub allocation_root: PathBuf,
    pub syncfs_completed: bool,
    pub first_inventory_sha256: String,
    pub second_inventory_sha256: String,
}

#[derive(Clone, Debug)]
pub struct SealedAllocation {
    pub stable: StableAllocationReceipt,
    pub quiescence: QuiescenceReceipt,
    pub first_inventory: AllocationInventory,
    pub second_inventory: AllocationInventory,
    pub unmounted: UnmountedOverlay,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReceiptHitSealInput {
    pub schema_version: u32,
    pub affected_stream: PathBuf,
    pub affected_stream_sha256: String,
    pub affected_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ReceiptSealedAllocation {
    pub stable: StableAllocationReceipt,
    pub quiescence: QuiescenceReceipt,
    pub affected_stream_sha256: String,
    pub affected_paths: Vec<PathBuf>,
    pub unmounted: UnmountedOverlay,
}

pub fn sealing_record_path(session_dir: &Path) -> PathBuf {
    session_dir.join("SEALING.json")
}

pub fn validate_receipt_hit_input(input: &ReceiptHitSealInput) -> PocResult<()> {
    if input.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(format!(
            "unsupported receipt-hit seal schema {}",
            input.schema_version
        )));
    }
    if input.affected_paths.is_empty() || input.affected_paths.len() > 64 {
        return Err(PocError::Integrity(
            "receipt-hit seal must name between one and 64 affected paths".to_owned(),
        ));
    }
    let mut paths = input.affected_paths.clone();
    paths.sort();
    paths.dedup();
    if paths.len() != input.affected_paths.len()
        || paths.iter().any(|path| {
            path.as_os_str().is_empty()
                || path
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
        })
    {
        return Err(PocError::Integrity(
            "receipt-hit affected paths must be unique normalized relatives".to_owned(),
        ));
    }
    if input.affected_stream_sha256.len() != 64
        || !input
            .affected_stream_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(PocError::Integrity(
            "receipt-hit affected stream digest is not lowercase SHA-256".to_owned(),
        ));
    }
    let observed = sha256_file(&input.affected_stream)?;
    if observed != input.affected_stream_sha256 {
        return Err(PocError::Integrity(
            "receipt-hit affected stream digest mismatch".to_owned(),
        ));
    }
    let stream_paths = crate::semantic::affected_stream_paths(&input.affected_stream)?;
    if stream_paths != paths {
        return Err(PocError::Integrity(
            "receipt-hit affected paths do not exactly match the authenticated stream".to_owned(),
        ));
    }
    Ok(())
}

/// Persist the terminal boundary. A caller may reopen only when this function
/// fails and the final path is absent; once the record exists, recovery must
/// roll the same operation forward.
pub fn persist_sealing(
    session_dir: &Path,
    operation_id: &OperationId,
    lease: &MutableLease,
    stationary_payload_path: &Path,
    faults: &mut NamedFaultInjector,
) -> PocResult<SealingRecord> {
    require_normalized_operation_component(operation_id)?;
    let record = SealingRecord {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        session_id: lease.session_id.clone(),
        allocation_id: lease.allocation_id.clone(),
        lease_epoch: lease.lease_epoch,
        owner_epoch: lease.owner_epoch,
        durable_unix_ms: unix_time_ms()?,
    };
    let final_path = sealing_record_path(session_dir);
    let temporary_path = session_dir.join(format!(".SEALING.{}.tmp", operation_id.as_str()));
    reach_real_operation(
        faults,
        NamedFaultPoint::SealingBeforeWrite,
        operation_id,
        [session_dir.join("SESSION.json")],
        Some(stationary_payload_path),
        false,
    )?;
    let mut bytes = serde_json::to_vec(&record)?;
    bytes.push(b'\n');
    let mut file = create_sealing_temporary(&temporary_path)?;
    file.write_all(&bytes)
        .map_err(|error| PocError::io("write Sealing temporary", &temporary_path, error))?;
    file.sync_all()
        .map_err(|error| PocError::io("fsync Sealing temporary", &temporary_path, error))?;
    reach_real_operation(
        faults,
        NamedFaultPoint::SealingAfterFileFsync,
        operation_id,
        [temporary_path.clone()],
        Some(stationary_payload_path),
        false,
    )?;
    drop(file);
    install_sealing_no_replace(&temporary_path, &final_path, &record)?;
    durable::fsync_dir(session_dir)?;
    reach_real_operation(
        faults,
        NamedFaultPoint::SealingAfterDirFsync,
        operation_id,
        [final_path],
        Some(stationary_payload_path),
        true,
    )?;
    Ok(record)
}

fn require_normalized_operation_component(operation_id: &OperationId) -> PocResult<()> {
    let value = operation_id.as_str();
    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if component.to_str() == Some(value) => Ok(()),
        _ => Err(PocError::Integrity(format!(
            "Sealing operation ID is not one normalized path component: {value:?}"
        ))),
    }
}

fn create_sealing_temporary(path: &Path) -> PocResult<File> {
    let mut options = File::options();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    options
        .open(path)
        .map_err(|error| PocError::io("create Sealing temporary", path, error))
}

fn install_sealing_no_replace(
    temporary_path: &Path,
    final_path: &Path,
    expected: &SealingRecord,
) -> PocResult<()> {
    let install_result = match std::fs::hard_link(temporary_path, final_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match read_sealing_no_follow(final_path) {
                Ok(observed) if &observed == expected => Ok(()),
                Ok(_) => Err(PocError::RecoveryRequired(
                    "immutable Sealing record collision differs from the exact operation"
                        .to_owned(),
                )),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(PocError::io(
            "install immutable Sealing record",
            final_path,
            error,
        )),
    };
    let remove_result = std::fs::remove_file(temporary_path)
        .map_err(|error| PocError::io("remove Sealing temporary", temporary_path, error));
    install_result?;
    remove_result
}

fn read_sealing_no_follow(path: &Path) -> PocResult<SealingRecord> {
    let before = std::fs::symlink_metadata(path)
        .map_err(|error| PocError::io("inspect immutable Sealing record", path, error))?;
    if !before.file_type().is_file() {
        return Err(PocError::RecoveryRequired(
            "immutable Sealing record is not a no-follow regular file".to_owned(),
        ));
    }
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options
        .open(path)
        .map_err(|error| PocError::io("open immutable Sealing record", path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| PocError::io("stat immutable Sealing record", path, error))?;
    #[cfg(unix)]
    if before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err(PocError::RecoveryRequired(
            "immutable Sealing record changed while opening".to_owned(),
        ));
    }
    if opened.len() > 64 * 1024 {
        return Err(PocError::RecoveryRequired(
            "immutable Sealing record is oversized".to_owned(),
        ));
    }
    serde_json::from_reader(file).map_err(PocError::from)
}

/// Execute the post-Sealing terminal path: kill/reap, prove no writable
/// process pins, strict-unmount, syncfs, and take two identical inventories.
pub(crate) fn quiesce_and_stabilize(
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    process_tree: &mut ManagedProcessTree,
    overlay: PermanentOverlayMount,
    faults: &mut FaultInjector,
) -> PocResult<SealedAllocation> {
    quiesce_and_stabilize_from(
        session_dir,
        operation_id,
        allocation,
        lease,
        process_tree,
        overlay,
        faults,
        AllocationStabilizationSource::Lexical(std::marker::PhantomData),
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn quiesce_and_stabilize_anchored(
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    authority: &crate::session::AnchoredAllocationAuthority,
    lease: &MutableLease,
    process_tree: &mut ManagedProcessTree,
    overlay: PermanentOverlayMount,
    faults: &mut FaultInjector,
) -> PocResult<SealedAllocation> {
    quiesce_and_stabilize_from(
        session_dir,
        operation_id,
        allocation,
        lease,
        process_tree,
        overlay,
        faults,
        AllocationStabilizationSource::Anchored(authority),
    )
}

#[allow(clippy::too_many_arguments)]
fn quiesce_and_stabilize_from(
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    process_tree: &mut ManagedProcessTree,
    overlay: PermanentOverlayMount,
    faults: &mut FaultInjector,
    source: AllocationStabilizationSource<'_>,
) -> PocResult<SealedAllocation> {
    source.revalidate(allocation)?;
    let audit_identity = live_workspace_audit_identity(&overlay)?;
    let allocation_root = source.root_path(allocation);
    let allocation_upper = source.upper_path(allocation);
    let sealing: SealingRecord = durable::read_json(&sealing_record_path(session_dir))?;
    validate_sealing_scope(&sealing, operation_id, lease)?;

    let state_paths = [
        sealing_record_path(session_dir),
        session_dir.join("STABLE.json"),
        session_dir.join("QUIESCENCE.json"),
    ];
    let mut named_faults = NamedFaultInjector::default()
        .with_physical_context(operation_id.as_str(), state_paths.clone());
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceBeforeStop,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation_upper),
        true,
    )?;
    let killed_or_signaled_pids = process_tree.stop_kill_reap_anchored(&audit_identity)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceAfterReap,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation_upper),
        true,
    )?;
    faults.hit(FaultPoint::AfterProcessDrain, true)?;
    let pre_unmount_audit = wait_for_clear_audit_anchored(process_tree, &audit_identity, false)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceAfterFdAudit,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation_upper),
        true,
    )?;

    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::UnmountBeforeStrict,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation_upper),
        true,
    )?;
    let unmounted = overlay.strict_unmount()?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::UnmountAfterStrict,
        operation_id,
        [unmounted.workspace_root.clone()],
        Some(&allocation_upper),
        true,
    )?;
    faults.hit(FaultPoint::AfterStrictUnmount, true)?;
    let post_unmount_audit = wait_for_clear_audit_anchored(process_tree, &audit_identity, true)?;

    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::FlushBeforeSyncfs,
        operation_id,
        [allocation_upper.clone()],
        Some(&allocation_upper),
        true,
    )?;
    source.sync(allocation)?;
    source.revalidate(allocation)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::FlushAfterSyncfs,
        operation_id,
        [
            allocation_upper.clone(),
            source.root_path(allocation).join("owner"),
        ],
        Some(&allocation_upper),
        true,
    )?;
    faults.hit(FaultPoint::AfterSyncfs, true)?;

    let first_inventory = source.capture_inventory(allocation)?;
    source.revalidate(allocation)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::InventoryAfterFirst,
        operation_id,
        [allocation_root.join("ALLOCATION.json")],
        Some(&allocation_upper),
        true,
    )?;
    faults.hit(FaultPoint::AfterFirstInventory, true)?;
    thread::yield_now();
    let second_inventory = source.capture_inventory(allocation)?;
    if first_inventory != second_inventory {
        return Err(PocError::RecoveryRequired(format!(
            "allocation {} changed between post-syncfs inventories",
            allocation.descriptor.allocation_id
        )));
    }
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::InventoryAfterStableSecond,
        operation_id,
        [allocation_root.join("ALLOCATION.json")],
        Some(&allocation_upper),
        true,
    )?;
    let stable = StableAllocationReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        allocation: allocation.descriptor.clone(),
        expected_owner_epoch: lease.owner_epoch,
        before: first_inventory.physical.clone(),
        after: second_inventory.physical.clone(),
        sync_completed: true,
    };
    faults.hit(FaultPoint::AfterStableAllocation, true)?;
    let quiescence = QuiescenceReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        session_id: lease.session_id.clone(),
        killed_or_signaled_pids,
        pre_unmount_audit,
        post_unmount_audit,
        workspace_root: unmounted.workspace_root.clone(),
        allocation_root: unmounted.allocation_root.clone(),
        syncfs_completed: true,
        first_inventory_sha256: first_inventory.inventory_sha256.clone(),
        second_inventory_sha256: second_inventory.inventory_sha256.clone(),
    };
    source.revalidate(allocation)?;
    durable::replace_json(&session_dir.join("STABLE.json"), &stable)?;
    durable::replace_json(&session_dir.join("QUIESCENCE.json"), &quiescence)?;
    source.revalidate(allocation)?;
    Ok(SealedAllocation {
        stable,
        quiescence,
        first_inventory,
        second_inventory,
        unmounted,
    })
}

pub(crate) fn quiesce_and_stabilize_receipt_hit(
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    process_tree: &mut ManagedProcessTree,
    overlay: PermanentOverlayMount,
    faults: &mut FaultInjector,
) -> PocResult<ReceiptSealedAllocation> {
    quiesce_and_stabilize_receipt_hit_from(
        session_dir,
        operation_id,
        allocation,
        lease,
        process_tree,
        overlay,
        faults,
        AllocationStabilizationSource::Lexical(std::marker::PhantomData),
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn quiesce_and_stabilize_receipt_hit_anchored(
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    authority: &crate::session::AnchoredAllocationAuthority,
    lease: &MutableLease,
    process_tree: &mut ManagedProcessTree,
    overlay: PermanentOverlayMount,
    faults: &mut FaultInjector,
) -> PocResult<ReceiptSealedAllocation> {
    quiesce_and_stabilize_receipt_hit_from(
        session_dir,
        operation_id,
        allocation,
        lease,
        process_tree,
        overlay,
        faults,
        AllocationStabilizationSource::Anchored(authority),
    )
}

#[allow(clippy::too_many_arguments)]
fn quiesce_and_stabilize_receipt_hit_from(
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    process_tree: &mut ManagedProcessTree,
    overlay: PermanentOverlayMount,
    faults: &mut FaultInjector,
    source: AllocationStabilizationSource<'_>,
) -> PocResult<ReceiptSealedAllocation> {
    source.revalidate(allocation)?;
    let audit_identity = live_workspace_audit_identity(&overlay)?;
    let allocation_upper = source.upper_path(allocation);
    let input: ReceiptHitSealInput = durable::read_json(&session_dir.join("RECEIPT-HIT.json"))?;
    validate_receipt_hit_input(&input)?;
    let sealing: SealingRecord = durable::read_json(&sealing_record_path(session_dir))?;
    validate_sealing_scope(&sealing, operation_id, lease)?;

    let state_paths = [
        sealing_record_path(session_dir),
        session_dir.join("STABLE.json"),
        session_dir.join("QUIESCENCE.json"),
    ];
    let mut named_faults = NamedFaultInjector::default()
        .with_physical_context(operation_id.as_str(), state_paths.clone());
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceBeforeStop,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation_upper),
        true,
    )?;
    let killed_or_signaled_pids = process_tree.stop_kill_reap_anchored(&audit_identity)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceAfterReap,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation_upper),
        true,
    )?;
    faults.hit(FaultPoint::AfterProcessDrain, true)?;
    let pre_unmount_audit = wait_for_clear_audit_anchored(process_tree, &audit_identity, false)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceAfterFdAudit,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation_upper),
        true,
    )?;

    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::UnmountBeforeStrict,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation_upper),
        true,
    )?;
    let unmounted = overlay.strict_unmount()?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::UnmountAfterStrict,
        operation_id,
        [unmounted.workspace_root.clone()],
        Some(&allocation_upper),
        true,
    )?;
    faults.hit(FaultPoint::AfterStrictUnmount, true)?;
    let post_unmount_audit = wait_for_clear_audit_anchored(process_tree, &audit_identity, true)?;

    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::FlushBeforeSyncfs,
        operation_id,
        [allocation_upper.clone()],
        Some(&allocation_upper),
        true,
    )?;
    source.sync(allocation)?;
    source.revalidate(allocation)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::FlushAfterSyncfs,
        operation_id,
        [
            allocation_upper.clone(),
            source.root_path(allocation).join("owner"),
        ],
        Some(&allocation_upper),
        true,
    )?;
    faults.hit(FaultPoint::AfterSyncfs, true)?;

    let before = source.capture_physical_witness(allocation, &input.affected_paths)?;
    source.revalidate(allocation)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::InventoryAfterFirst,
        operation_id,
        [input.affected_stream.clone()],
        Some(&allocation_upper),
        true,
    )?;
    faults.hit(FaultPoint::AfterFirstInventory, true)?;
    thread::yield_now();
    let after = source.capture_physical_witness(allocation, &input.affected_paths)?;
    if before != after {
        return Err(PocError::RecoveryRequired(format!(
            "allocation {} changed between receipt-hit witnesses",
            allocation.descriptor.allocation_id
        )));
    }
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::InventoryAfterStableSecond,
        operation_id,
        [input.affected_stream.clone()],
        Some(&allocation_upper),
        true,
    )?;
    let witness_sha256 = digest_json(&before)?;
    let stable = StableAllocationReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        allocation: allocation.descriptor.clone(),
        expected_owner_epoch: lease.owner_epoch,
        before,
        after,
        sync_completed: true,
    };
    faults.hit(FaultPoint::AfterStableAllocation, true)?;
    let quiescence = QuiescenceReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        session_id: lease.session_id.clone(),
        killed_or_signaled_pids,
        pre_unmount_audit,
        post_unmount_audit,
        workspace_root: unmounted.workspace_root.clone(),
        allocation_root: unmounted.allocation_root.clone(),
        syncfs_completed: true,
        first_inventory_sha256: witness_sha256.clone(),
        second_inventory_sha256: witness_sha256,
    };
    source.revalidate(allocation)?;
    durable::replace_json(&session_dir.join("STABLE.json"), &stable)?;
    durable::replace_json(&session_dir.join("QUIESCENCE.json"), &quiescence)?;
    source.revalidate(allocation)?;
    Ok(ReceiptSealedAllocation {
        stable,
        quiescence,
        affected_stream_sha256: input.affected_stream_sha256,
        affected_paths: input.affected_paths,
        unmounted,
    })
}

fn validate_sealing_scope(
    sealing: &SealingRecord,
    operation_id: &OperationId,
    lease: &MutableLease,
) -> PocResult<()> {
    if sealing.schema_version != SCHEMA_VERSION
        || sealing.operation_id != *operation_id
        || sealing.session_id != lease.session_id
        || sealing.allocation_id != lease.allocation_id
        || sealing.lease_epoch != lease.lease_epoch
        || sealing.owner_epoch != lease.owner_epoch
    {
        return Err(PocError::RecoveryRequired(
            "durable Sealing scope does not match the requested lease tuple".to_owned(),
        ));
    }
    Ok(())
}

fn wait_for_clear_audit_anchored(
    process_tree: &ManagedProcessTree,
    identity: &AnchoredWorkspaceAuditIdentity,
    include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    let deadline = Instant::now() + AUDIT_RETRY_BUDGET;
    loop {
        let audit = process_tree.audit_anchored(identity, include_mount_namespaces)?;
        if audit.is_clear() {
            return Ok(audit);
        }
        if Instant::now() >= deadline {
            return Err(PocError::RecoveryRequired(format!(
                "anchored workspace quiescence proof failed: {audit:?}"
            )));
        }
        thread::sleep(AUDIT_RETRY_DELAY);
    }
}

#[cfg(target_os = "linux")]
fn syncfs_path(path: &Path) -> PocResult<()> {
    let file = File::open(path)
        .map_err(|error| PocError::io("open allocation for syncfs", path, error))?;
    // SAFETY: `syncfs(2)` only consumes the valid borrowed file descriptor and
    // does not retain it or dereference user memory.
    let result = unsafe { libc::syncfs(file.as_raw_fd()) };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::io(
            "syncfs allocation filesystem",
            path,
            std::io::Error::last_os_error(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn syncfs_descriptor(directory: &std::os::fd::OwnedFd, display_path: &Path) -> PocResult<()> {
    // SAFETY: `syncfs(2)` only consumes the valid borrowed descriptor and does
    // not retain it or dereference user memory.
    let result = unsafe { libc::syncfs(directory.as_raw_fd()) };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::io(
            "syncfs pinned allocation filesystem",
            display_path,
            std::io::Error::last_os_error(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn syncfs_path(path: &Path) -> PocResult<()> {
    let file =
        File::open(path).map_err(|error| PocError::io("open allocation for sync", path, error))?;
    file.sync_all()
        .map_err(|error| PocError::io("sync allocation", path, error))
}

fn sync_directory(path: &Path) -> PocResult<()> {
    fs::create_dir_all(path)
        .map_err(|error| PocError::io("create durable directory", path, error))?;
    let directory =
        File::open(path).map_err(|error| PocError::io("open durable directory", path, error))?;
    directory
        .sync_all()
        .map_err(|error| PocError::io("fsync durable directory", path, error))
}

fn sha256_file(path: &Path) -> PocResult<String> {
    let file = File::open(path)
        .map_err(|error| PocError::io("open receipt-hit affected stream", path, error))?;
    let mut reader = BufReader::with_capacity(32 * 1024, file);
    let mut buffer = [0_u8; 32 * 1024];
    let mut digest = Sha256::new();
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| PocError::io("hash receipt-hit affected stream", path, error))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex_digest(digest.finalize()))
}

fn digest_json<T: Serialize>(value: &T) -> PocResult<String> {
    Ok(hex_digest(Sha256::digest(serde_json::to_vec(value)?)))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;

    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}
