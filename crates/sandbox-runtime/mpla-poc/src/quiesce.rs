use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fault::{FaultInjector, FaultPoint};
use crate::inventory::{capture_inventory, capture_physical_witness, AllocationInventory};
use crate::overlay_adapter::{PermanentOverlayMount, UnmountedOverlay};
use crate::process_tree::{ManagedProcessTree, ProcessAudit};
use crate::recovery::reach_real_operation;
use crate::{
    durable, unix_time_ms, AllocationHandle, MutableLease, NamedFaultInjector, NamedFaultPoint,
    OperationId, PocError, PocResult, SessionId, StableAllocationReceipt, SCHEMA_VERSION,
};

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
    let mut file = File::options()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary_path)
        .map_err(|error| PocError::io("create Sealing temporary", &temporary_path, error))?;
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
    std::fs::rename(&temporary_path, &final_path)
        .map_err(|error| PocError::io("replace Sealing record", &final_path, error))?;
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

/// Execute the post-Sealing terminal path: kill/reap, prove no writable
/// process pins, strict-unmount, syncfs, and take two identical inventories.
pub fn quiesce_and_stabilize(
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    process_tree: &mut ManagedProcessTree,
    overlay: PermanentOverlayMount,
    faults: &mut FaultInjector,
) -> PocResult<SealedAllocation> {
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
        Some(&allocation.upper_dir),
        true,
    )?;
    let killed_or_signaled_pids = process_tree.stop_kill_reap()?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceAfterReap,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    faults.hit(FaultPoint::AfterProcessDrain, true)?;
    let pre_unmount_audit = wait_for_clear_audit(process_tree, false)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceAfterFdAudit,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation.upper_dir),
        true,
    )?;

    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::UnmountBeforeStrict,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    let unmounted = overlay.strict_unmount()?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::UnmountAfterStrict,
        operation_id,
        [unmounted.workspace_root.clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    faults.hit(FaultPoint::AfterStrictUnmount, true)?;
    let post_unmount_audit = wait_for_clear_audit(process_tree, true)?;

    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::FlushBeforeSyncfs,
        operation_id,
        [allocation.upper_dir.clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    syncfs_path(&allocation.upper_dir)?;
    sync_directory(&allocation.owner_dir)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::FlushAfterSyncfs,
        operation_id,
        [allocation.upper_dir.clone(), allocation.owner_dir.clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    faults.hit(FaultPoint::AfterSyncfs, true)?;

    let first_inventory = capture_inventory(allocation)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::InventoryAfterFirst,
        operation_id,
        [allocation.allocation_root.join("ALLOCATION.json")],
        Some(&allocation.upper_dir),
        true,
    )?;
    faults.hit(FaultPoint::AfterFirstInventory, true)?;
    thread::yield_now();
    let second_inventory = capture_inventory(allocation)?;
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
        [allocation.allocation_root.join("ALLOCATION.json")],
        Some(&allocation.upper_dir),
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
    durable::replace_json(&session_dir.join("STABLE.json"), &stable)?;
    durable::replace_json(&session_dir.join("QUIESCENCE.json"), &quiescence)?;
    Ok(SealedAllocation {
        stable,
        quiescence,
        first_inventory,
        second_inventory,
        unmounted,
    })
}

pub fn quiesce_and_stabilize_receipt_hit(
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    process_tree: &mut ManagedProcessTree,
    overlay: PermanentOverlayMount,
    faults: &mut FaultInjector,
) -> PocResult<ReceiptSealedAllocation> {
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
        Some(&allocation.upper_dir),
        true,
    )?;
    let killed_or_signaled_pids = process_tree.stop_kill_reap()?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceAfterReap,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    faults.hit(FaultPoint::AfterProcessDrain, true)?;
    let pre_unmount_audit = wait_for_clear_audit(process_tree, false)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::QuiesceAfterFdAudit,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation.upper_dir),
        true,
    )?;

    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::UnmountBeforeStrict,
        operation_id,
        [state_paths[0].clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    let unmounted = overlay.strict_unmount()?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::UnmountAfterStrict,
        operation_id,
        [unmounted.workspace_root.clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    faults.hit(FaultPoint::AfterStrictUnmount, true)?;
    let post_unmount_audit = wait_for_clear_audit(process_tree, true)?;

    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::FlushBeforeSyncfs,
        operation_id,
        [allocation.upper_dir.clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    syncfs_path(&allocation.upper_dir)?;
    sync_directory(&allocation.owner_dir)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::FlushAfterSyncfs,
        operation_id,
        [allocation.upper_dir.clone(), allocation.owner_dir.clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    faults.hit(FaultPoint::AfterSyncfs, true)?;

    let before = capture_physical_witness(allocation, &input.affected_paths)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::InventoryAfterFirst,
        operation_id,
        [input.affected_stream.clone()],
        Some(&allocation.upper_dir),
        true,
    )?;
    faults.hit(FaultPoint::AfterFirstInventory, true)?;
    thread::yield_now();
    let after = capture_physical_witness(allocation, &input.affected_paths)?;
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
        Some(&allocation.upper_dir),
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
    durable::replace_json(&session_dir.join("STABLE.json"), &stable)?;
    durable::replace_json(&session_dir.join("QUIESCENCE.json"), &quiescence)?;
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
    if sealing.operation_id != *operation_id
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

fn wait_for_clear_audit(
    process_tree: &ManagedProcessTree,
    include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    let deadline = Instant::now() + AUDIT_RETRY_BUDGET;
    loop {
        let audit = process_tree.audit(include_mount_namespaces)?;
        if audit.is_clear() {
            return Ok(audit);
        }
        if Instant::now() >= deadline {
            return Err(PocError::RecoveryRequired(format!(
                "workspace quiescence proof failed: {audit:?}"
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
