use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::fault::{FaultInjector, FaultPoint};
use crate::inventory::{capture_inventory, AllocationInventory};
use crate::overlay_adapter::{PermanentOverlayMount, UnmountedOverlay};
use crate::process_tree::{ManagedProcessTree, ProcessAudit};
use crate::{
    durable, unix_time_ms, AllocationHandle, MutableLease, OperationId, PocError, PocResult,
    SessionId, StableAllocationReceipt, SCHEMA_VERSION,
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

pub fn sealing_record_path(session_dir: &Path) -> PathBuf {
    session_dir.join("SEALING.json")
}

/// Persist the terminal boundary. A caller may reopen only when this function
/// fails and the final path is absent; once the record exists, recovery must
/// roll the same operation forward.
pub fn persist_sealing(
    session_dir: &Path,
    operation_id: &OperationId,
    lease: &MutableLease,
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
    durable::replace_json(&sealing_record_path(session_dir), &record)?;
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

    let killed_or_signaled_pids = process_tree.stop_kill_reap()?;
    faults.hit(FaultPoint::AfterProcessDrain, true)?;
    let pre_unmount_audit = wait_for_clear_audit(process_tree, false)?;

    let unmounted = overlay.strict_unmount()?;
    faults.hit(FaultPoint::AfterStrictUnmount, true)?;
    let post_unmount_audit = wait_for_clear_audit(process_tree, true)?;

    syncfs_path(&allocation.upper_dir)?;
    sync_directory(&allocation.owner_dir)?;
    faults.hit(FaultPoint::AfterSyncfs, true)?;

    let first_inventory = capture_inventory(allocation)?;
    faults.hit(FaultPoint::AfterFirstInventory, true)?;
    thread::yield_now();
    let second_inventory = capture_inventory(allocation)?;
    if first_inventory != second_inventory {
        return Err(PocError::RecoveryRequired(format!(
            "allocation {} changed between post-syncfs inventories",
            allocation.descriptor.allocation_id
        )));
    }
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
