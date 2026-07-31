use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::fault::{FaultInjector, FaultPoint};
use crate::overlay_adapter::{mount_permanent_overlay, PermanentOverlayMount};
use crate::process_tree::{CommandReceipt, ManagedProcessTree};
use crate::quiesce::{
    self, ReceiptHitSealInput, ReceiptSealedAllocation, SealedAllocation, SealingRecord,
};
use crate::recovery::reach_real_operation;
use crate::{
    durable, lease, unix_time_ms, AllocationHandle, MutableLease, NamedFaultInjector,
    NamedFaultPoint, OperationId, PocError, PocResult, SessionId, SessionPhase, WriterCapability,
    SCHEMA_VERSION,
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

/// Durable MPLA session control state prepared by the public runtime before
/// the storage-admin helper mounts the allocation into a holder namespace.
///
/// This deliberately contains no mount or process-tree authority.  The
/// caller may pass its exact `workspace_root` to the typed storage-admin
/// request, but only the helper is allowed to make it a mountpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedExternalSession {
    session_dir: PathBuf,
    workspace_root: PathBuf,
}

impl PreparedExternalSession {
    #[must_use]
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn begin_sealing(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
        operation_id: &OperationId,
        faults: &mut FaultInjector,
    ) -> PocResult<SealingRecord> {
        let record = self.validate_binding(allocation, lease)?;
        faults.hit(FaultPoint::BeforeSealing, false)?;
        let sealing_path = quiesce::sealing_record_path(&self.session_dir);
        if sealing_path.exists() {
            let sealing: SealingRecord = durable::read_json(&sealing_path)?;
            validate_external_sealing_record(&sealing, operation_id, lease)?;
            if !matches!(
                record.phase,
                SessionPhase::Open
                    | SessionPhase::Closing
                    | SessionPhase::Sealing
                    | SessionPhase::RecoveryRequired
                    | SessionPhase::PublicationCommitted
            ) {
                return Err(PocError::Integrity(format!(
                    "external session {} cannot resume Sealing from {:?}",
                    lease.session_id, record.phase
                )));
            }
            return Ok(sealing);
        }
        if !matches!(record.phase, SessionPhase::Open | SessionPhase::Closing) {
            return Err(PocError::Integrity(format!(
                "external session {} cannot begin Sealing from {:?}",
                lease.session_id, record.phase
            )));
        }
        // Admission is already closed in memory by the public service. The
        // durable Sealing record below is the ratified terminal boundary, so
        // an intermediate durable Closing rewrite adds no recovery state.
        let mut named_faults = NamedFaultInjector::default().with_physical_context(
            operation_id.as_str(),
            [self.session_dir.join("SESSION.json"), sealing_path.clone()],
        );
        let sealing = match quiesce::persist_sealing(
            &self.session_dir,
            operation_id,
            lease,
            &allocation.upper_dir,
            &mut named_faults,
        ) {
            Ok(sealing) => sealing,
            Err(error) if sealing_path.exists() => {
                return Err(PocError::RecoveryRequired(format!(
                    "Sealing record became visible but durability returned an error: {error}"
                )));
            }
            Err(error) => return Err(error),
        };
        faults.hit(FaultPoint::AfterSealingDurable, true)?;
        Ok(sealing)
    }

    /// Return whether the session has crossed its durable publication boundary.
    ///
    /// `SEALING.json` is the ratified boundary for an external session.  The
    /// original `SESSION.json` phase may therefore remain `Open`; callers that
    /// could otherwise delete an unpublished allocation must consult this
    /// record and fail closed if its scope is malformed.
    pub fn has_ratified_sealing(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<bool> {
        self.validate_binding(allocation, lease)?;
        let sealing_path = quiesce::sealing_record_path(&self.session_dir);
        if !sealing_path.exists() {
            return Ok(false);
        }
        let sealing: SealingRecord = durable::read_json(&sealing_path)?;
        validate_external_sealing_scope(&sealing, lease)?;
        Ok(true)
    }

    pub fn mark_publication_committed(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<()> {
        let record = self.validate_binding(allocation, lease)?;
        if record.phase == SessionPhase::PublicationCommitted {
            return Ok(());
        }
        if !matches!(
            record.phase,
            SessionPhase::Open
                | SessionPhase::Closing
                | SessionPhase::Sealing
                | SessionPhase::RecoveryRequired
        ) {
            return Err(PocError::Integrity(format!(
                "external session {} cannot commit publication from {:?}",
                lease.session_id, record.phase
            )));
        }
        if !self.has_ratified_sealing(allocation, lease)? {
            return Err(PocError::Integrity(
                "external session cannot commit publication before durable Sealing".to_owned(),
            ));
        }
        persist_session_record(
            &self.session_dir,
            lease,
            SessionPhase::PublicationCommitted,
            &self.workspace_root,
        )
    }

    pub fn mark_recovery_required(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<()> {
        let record = self.validate_binding(allocation, lease)?;
        if record.phase == SessionPhase::RecoveryRequired {
            return Ok(());
        }
        if !matches!(
            record.phase,
            SessionPhase::Open | SessionPhase::Closing | SessionPhase::Sealing
        ) {
            return Err(PocError::Integrity(
                "pre-Sealing external session cannot be marked terminal recovery".to_owned(),
            ));
        }
        if !self.has_ratified_sealing(allocation, lease)? {
            return Err(PocError::Integrity(
                "pre-Sealing external session cannot be marked terminal recovery".to_owned(),
            ));
        }
        persist_session_record(
            &self.session_dir,
            lease,
            SessionPhase::RecoveryRequired,
            &self.workspace_root,
        )
    }

    pub(crate) fn validate_stationary_binding(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
        operation_id: &OperationId,
    ) -> PocResult<SessionRecord> {
        let record = self.validate_binding(allocation, lease)?;
        if !matches!(
            record.phase,
            SessionPhase::Open
                | SessionPhase::Closing
                | SessionPhase::Sealing
                | SessionPhase::RecoveryRequired
                | SessionPhase::PublicationCommitted
        ) {
            return Err(PocError::Integrity(format!(
                "external session {} is not terminally sealed: {:?}",
                lease.session_id, record.phase
            )));
        }
        let sealing: SealingRecord =
            durable::read_json(&quiesce::sealing_record_path(&self.session_dir))?;
        validate_external_sealing_record(&sealing, operation_id, lease)?;
        Ok(record)
    }

    fn validate_binding(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<SessionRecord> {
        if allocation.descriptor.allocation_id != lease.allocation_id {
            return Err(PocError::Integrity(
                "external session lease allocation does not match allocation handle".to_owned(),
            ));
        }
        let expected_session_dir = self
            .session_dir
            .parent()
            .and_then(Path::parent)
            .map(|control_root| {
                control_root
                    .join("sessions")
                    .join(lease.session_id.as_str())
            })
            .ok_or_else(|| {
                PocError::Integrity("external session directory has no control root".to_owned())
            })?;
        if self.session_dir != expected_session_dir
            || self.workspace_root != self.session_dir.join("mount")
        {
            return Err(PocError::Integrity(
                "external session paths do not match the lease identity".to_owned(),
            ));
        }
        let record: SessionRecord = durable::read_json(&self.session_dir.join("SESSION.json"))?;
        if record.schema_version != SCHEMA_VERSION
            || record.session_id != lease.session_id
            || record.allocation_id != lease.allocation_id
            || record.lease_epoch != lease.lease_epoch
            || record.owner_epoch != lease.owner_epoch
            || record.workspace_root != self.workspace_root
        {
            return Err(PocError::Integrity(
                "durable external session record differs from its allocation and lease".to_owned(),
            ));
        }
        Ok(record)
    }
}

fn validate_external_sealing_record(
    sealing: &SealingRecord,
    operation_id: &OperationId,
    lease: &MutableLease,
) -> PocResult<()> {
    validate_external_sealing_scope(sealing, lease)?;
    if sealing.operation_id != *operation_id {
        return Err(PocError::RecoveryRequired(
            "durable external Sealing operation does not match the requested operation".to_owned(),
        ));
    }
    Ok(())
}

fn validate_external_sealing_scope(sealing: &SealingRecord, lease: &MutableLease) -> PocResult<()> {
    if sealing.schema_version != SCHEMA_VERSION
        || sealing.session_id != lease.session_id
        || sealing.allocation_id != lease.allocation_id
        || sealing.lease_epoch != lease.lease_epoch
        || sealing.owner_epoch != lease.owner_epoch
    {
        return Err(PocError::RecoveryRequired(
            "durable external Sealing scope does not match the requested lease tuple".to_owned(),
        ));
    }
    Ok(())
}

/// Create the durable control-plane state for a lease-backed MPLA session
/// without mounting the allocation or admitting a workload.
pub fn prepare_external_session(
    control_root: &Path,
    allocation: &AllocationHandle,
    lease: &MutableLease,
) -> PocResult<PreparedExternalSession> {
    if allocation.descriptor.allocation_id != lease.allocation_id {
        return Err(PocError::Integrity(
            "session lease allocation does not match allocation handle".to_owned(),
        ));
    }
    let activation_root = control_root.join("activations");
    std::fs::create_dir_all(control_root)
        .map_err(|error| PocError::io("create session control root", control_root, error))?;
    match std::fs::create_dir(&activation_root) {
        Ok(()) => durable::fsync_dir(control_root)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !activation_root.is_dir() {
                return Err(PocError::Integrity(format!(
                    "activation control root is not a directory: {}",
                    activation_root.display()
                )));
            }
        }
        Err(error) => {
            return Err(PocError::io(
                "create activation control root",
                &activation_root,
                error,
            ));
        }
    }
    let session_dir = control_root
        .join("sessions")
        .join(lease.session_id.as_str());
    let workspace_root = session_dir.join("mount");
    std::fs::create_dir_all(&workspace_root)
        .map_err(|error| PocError::io("create session mount directory", &workspace_root, error))?;
    durable::fsync_dir(
        session_dir
            .parent()
            .ok_or_else(|| PocError::Integrity("session directory has no parent".to_owned()))?,
    )?;
    durable::fsync_dir(control_root)?;
    persist_session_record(&session_dir, lease, SessionPhase::Open, &workspace_root)?;
    Ok(PreparedExternalSession {
        session_dir,
        workspace_root,
    })
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
        let prepared = prepare_external_session(control_root, &allocation, &lease)?;
        let overlay = mount_permanent_overlay(
            &allocation,
            lower_dirs_newest_first,
            prepared.workspace_root(),
        )?;
        let process_tree =
            ManagedProcessTree::new(prepared.workspace_root.clone(), cgroup_procs_path);
        let session = Self {
            session_dir: prepared.session_dir,
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

    pub fn probe_readiness(
        &mut self,
        capability: &WriterCapability,
        relative_path: &Path,
        contains: Option<&[u8]>,
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
        self.process_tree
            .probe_file(relative_path, contains, timeout)
    }

    /// Cross the terminal Sealing boundary and produce a stable allocation
    /// receipt. Only a failure proven to precede the durable Sealing record may
    /// restore this session to Open.
    pub fn seal(
        &mut self,
        operation_id: &OperationId,
        faults: &mut FaultInjector,
    ) -> PocResult<SealedAllocation> {
        let overlay = self.begin_sealing(operation_id, faults)?;
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

    pub fn seal_receipt_hit(
        &mut self,
        operation_id: &OperationId,
        input: &ReceiptHitSealInput,
        faults: &mut FaultInjector,
    ) -> PocResult<ReceiptSealedAllocation> {
        self.ensure_open_for_sealing()?;
        quiesce::validate_receipt_hit_input(input)?;
        durable::replace_json(&self.session_dir.join("RECEIPT-HIT.json"), input)?;
        let overlay = self.begin_sealing(operation_id, faults)?;
        quiesce::quiesce_and_stabilize_receipt_hit(
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

    fn begin_sealing(
        &mut self,
        operation_id: &OperationId,
        faults: &mut FaultInjector,
    ) -> PocResult<PermanentOverlayMount> {
        self.ensure_open_for_sealing()?;
        let state_paths = [
            self.session_dir.join("SESSION.json"),
            quiesce::sealing_record_path(&self.session_dir),
        ];
        let mut named_faults = NamedFaultInjector::default()
            .with_physical_context(operation_id.as_str(), state_paths.clone());
        faults.hit(FaultPoint::BeforeSealing, false)?;
        reach_real_operation(
            &mut named_faults,
            NamedFaultPoint::FenceBeforeClose,
            operation_id,
            [self.session_dir.join("SESSION.json")],
            Some(&self.allocation.upper_dir),
            false,
        )?;
        self.phase = SessionPhase::Closing;
        self.process_tree.fence();
        if let Err(error) = self.persist_record() {
            self.phase = SessionPhase::Open;
            self.process_tree.unfence();
            return Err(error);
        }
        reach_real_operation(
            &mut named_faults,
            NamedFaultPoint::FenceAfterClose,
            operation_id,
            [self.session_dir.join("SESSION.json")],
            Some(&self.allocation.upper_dir),
            false,
        )?;
        reach_real_operation(
            &mut named_faults,
            NamedFaultPoint::FenceAfterDrain,
            operation_id,
            [self.session_dir.join("SESSION.json")],
            Some(&self.allocation.upper_dir),
            false,
        )?;

        let sealing_path = quiesce::sealing_record_path(&self.session_dir);
        if let Err(error) = quiesce::persist_sealing(
            &self.session_dir,
            operation_id,
            &self.lease,
            &self.allocation.upper_dir,
            &mut named_faults,
        ) {
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

        self.overlay.take().ok_or_else(|| {
            PocError::RecoveryRequired("sealed session has no live overlay guard".to_owned())
        })
    }

    fn ensure_open_for_sealing(&self) -> PocResult<()> {
        if self.phase == SessionPhase::Open {
            Ok(())
        } else {
            Err(PocError::Integrity(format!(
                "session {} cannot seal from {:?}",
                self.lease.session_id, self.phase
            )))
        }
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
        persist_session_record(&self.session_dir, &self.lease, self.phase, &workspace_root)
    }
}

fn persist_session_record(
    session_dir: &Path,
    lease: &MutableLease,
    phase: SessionPhase,
    workspace_root: &Path,
) -> PocResult<()> {
    durable::replace_json(
        &session_dir.join("SESSION.json"),
        &SessionRecord {
            schema_version: SCHEMA_VERSION,
            session_id: lease.session_id.clone(),
            allocation_id: lease.allocation_id.clone(),
            lease_epoch: lease.lease_epoch,
            owner_epoch: lease.owner_epoch,
            phase,
            workspace_root: workspace_root.to_path_buf(),
            updated_unix_ms: unix_time_ms()?,
        },
    )
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
