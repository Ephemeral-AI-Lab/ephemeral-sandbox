use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::fault::{FaultInjector, FaultPoint};
use crate::overlay_adapter::{mount_permanent_overlay, PermanentOverlayMount};
use crate::process_tree::{CommandReceipt, ManagedProcessTree};
use crate::quiesce::{self, SealedAllocation};
use crate::{
    durable, lease, unix_time_ms, AllocationHandle, MutableLease, OperationId, PocError, PocResult,
    SessionId, SessionPhase, WriterCapability, SCHEMA_VERSION,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionRecord {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub allocation_id: crate::AllocationId,
    pub lease_epoch: u64,
    pub owner_epoch: u64,
    pub phase: SessionPhase,
    pub workspace_root: PathBuf,
    pub updated_unix_ms: u64,
}

/// M0 session binding. Payload state lives only in `allocation`; the session
/// directory holds control metadata plus a disposable mountpoint.
#[derive(Debug)]
pub struct MplaSession {
    session_dir: PathBuf,
    allocation: AllocationHandle,
    lease: MutableLease,
    phase: SessionPhase,
    process_tree: ManagedProcessTree,
    overlay: Option<PermanentOverlayMount>,
}

impl MplaSession {
    pub fn open(
        control_root: &Path,
        allocation: AllocationHandle,
        lease: MutableLease,
        lower_dirs_newest_first: Vec<PathBuf>,
        cgroup_procs_path: Option<PathBuf>,
    ) -> PocResult<Self> {
        if allocation.descriptor.allocation_id != lease.allocation_id {
            return Err(PocError::Integrity(
                "session lease allocation does not match allocation handle".to_owned(),
            ));
        }
        let session_dir = control_root
            .join("sessions")
            .join(lease.session_id.as_str());
        let workspace_root = session_dir.join("mount");
        std::fs::create_dir_all(&session_dir)
            .map_err(|error| PocError::io("create session directory", &session_dir, error))?;
        let overlay =
            mount_permanent_overlay(&allocation, lower_dirs_newest_first, &workspace_root)?;
        let process_tree = ManagedProcessTree::new(workspace_root.clone(), cgroup_procs_path);
        let session = Self {
            session_dir,
            allocation,
            lease,
            phase: SessionPhase::Open,
            process_tree,
            overlay: Some(overlay),
        };
        session.persist_record()?;
        Ok(session)
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.lease.session_id
    }

    #[must_use]
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    #[must_use]
    pub fn allocation(&self) -> &AllocationHandle {
        &self.allocation
    }

    #[must_use]
    pub fn mutable_lease(&self) -> &MutableLease {
        &self.lease
    }

    #[must_use]
    pub const fn phase(&self) -> SessionPhase {
        self.phase
    }

    #[must_use]
    pub fn workspace_root(&self) -> Option<&Path> {
        self.overlay
            .as_ref()
            .map(PermanentOverlayMount::workspace_root)
    }

    pub fn execute(
        &mut self,
        capability: &WriterCapability,
        program: &Path,
        arguments: &[String],
        timeout: Duration,
    ) -> PocResult<CommandReceipt> {
        if self.phase != SessionPhase::Open {
            return Err(PocError::StaleCapability {
                capability: "writer",
                allocation_id: self.lease.allocation_id.to_string(),
                expected_epoch: self.lease.lease_epoch,
                observed_epoch: capability.lease_epoch,
            });
        }
        lease::validate_writer(&self.allocation.allocation_root, capability)?;
        self.process_tree.run(program, arguments, timeout)
    }

    /// Cross the terminal Sealing boundary and produce a stable allocation
    /// receipt. Only a failure proven to precede the durable Sealing record may
    /// restore this session to Open.
    pub fn seal(
        &mut self,
        operation_id: &OperationId,
        faults: &mut FaultInjector,
    ) -> PocResult<SealedAllocation> {
        if self.phase != SessionPhase::Open {
            return Err(PocError::Integrity(format!(
                "session {} cannot seal from {:?}",
                self.lease.session_id, self.phase
            )));
        }
        faults.hit(FaultPoint::BeforeSealing, false)?;
        self.phase = SessionPhase::Closing;
        self.process_tree.fence();
        if let Err(error) = self.persist_record() {
            self.phase = SessionPhase::Open;
            self.process_tree.unfence();
            return Err(error);
        }

        let sealing_path = quiesce::sealing_record_path(&self.session_dir);
        if let Err(error) = quiesce::persist_sealing(&self.session_dir, operation_id, &self.lease) {
            if sealing_path.exists() {
                self.phase = SessionPhase::Sealing;
                let _ = self.persist_record();
                return Err(PocError::RecoveryRequired(format!(
                    "Sealing record became visible but durability returned an error: {error}"
                )));
            }
            self.phase = SessionPhase::Open;
            self.process_tree.unfence();
            let _ = self.persist_record();
            return Err(error);
        }
        self.phase = SessionPhase::Sealing;
        self.persist_record().map_err(|error| {
            PocError::RecoveryRequired(format!(
                "session phase write failed after durable Sealing: {error}"
            ))
        })?;
        faults.hit(FaultPoint::AfterSealingDurable, true)?;

        let overlay = self.overlay.take().ok_or_else(|| {
            PocError::RecoveryRequired("sealed session has no live overlay guard".to_owned())
        })?;
        quiesce::quiesce_and_stabilize(
            &self.session_dir,
            operation_id,
            &self.allocation,
            &self.lease,
            &mut self.process_tree,
            overlay,
            faults,
        )
        .inspect_err(|_error| {
            self.phase = SessionPhase::RecoveryRequired;
            let _ = self.persist_record();
        })
    }

    pub fn mark_publication_committed(&mut self) -> PocResult<()> {
        if self.phase != SessionPhase::Sealing {
            return Err(PocError::Integrity(format!(
                "session {} cannot commit publication from {:?}",
                self.lease.session_id, self.phase
            )));
        }
        self.phase = SessionPhase::PublicationCommitted;
        self.persist_record()
    }

    pub fn mark_recovery_required(&mut self) -> PocResult<()> {
        if self.phase == SessionPhase::Open || self.phase == SessionPhase::Closing {
            return Err(PocError::Integrity(
                "pre-Sealing session cannot be marked terminal recovery by this path".to_owned(),
            ));
        }
        self.phase = SessionPhase::RecoveryRequired;
        self.persist_record()
    }

    fn persist_record(&self) -> PocResult<()> {
        let workspace_root = self
            .overlay
            .as_ref()
            .map(|overlay| overlay.workspace_root().to_path_buf())
            .unwrap_or_else(|| self.session_dir.join("mount"));
        durable::replace_json(
            &self.session_dir.join("SESSION.json"),
            &SessionRecord {
                schema_version: SCHEMA_VERSION,
                session_id: self.lease.session_id.clone(),
                allocation_id: self.lease.allocation_id.clone(),
                lease_epoch: self.lease.lease_epoch,
                owner_epoch: self.lease.owner_epoch,
                phase: self.phase,
                workspace_root,
                updated_unix_ms: unix_time_ms()?,
            },
        )
    }
}

impl Drop for MplaSession {
    fn drop(&mut self) {
        self.process_tree.fence();
        let _ = self.process_tree.stop_kill_reap();
        if let Some(overlay) = self.overlay.take() {
            let _ = overlay.strict_unmount();
        }
    }
}
