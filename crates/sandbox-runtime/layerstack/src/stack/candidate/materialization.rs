use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::generation::{
    digest_string, CarrierDescriptor, GenerationError, GenerationManifest, GenerationSelection,
    GenerationStore, MaterializationKey,
};
use super::materialization_operation::{
    MaterializationCheckpoint, MaterializationOperation, MaterializationOperationBuild,
    MaterializationOperationError, MaterializationPhase, MaterializationTerminalOutcome,
};
use super::materialization_publication::{
    publication_subject, DisabledMaterializationGcBridge, MaterializationGcBridge,
    MaterializationPublisher,
};
use super::native_backend::{
    NativeBackend, NativeBackendError, NativeReconstructionResources, MAX_HYDRATION_STREAM_BYTES,
    MIN_HYDRATION_STREAM_BYTES,
};
use super::object_store::LooseObjectStore;
use super::squash::CandidateSquashProducer;
use super::tree::PersistentPages;
use crate::lock::StorageWriterLockLease;
use crate::stack::HiddenValidationObservation;
use crate::supervisor::{
    shared_supervisor_for_root, MaterializationAdmission, MaterializationOwner, StorageSupervisor,
    SupervisorError, MAX_METADATA_QUEUE_ITEMS,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MaterializationDisposition {
    Built,
    Reused,
    Shared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MaterializationStage {
    CarrierSynced,
    GenerationAllocated,
    CarrierInstalled,
    ManifestDurable,
    CurrentDurable,
    BeforeTerminal,
}

#[derive(Clone, Debug)]
pub(crate) struct MaterializationRequest {
    pub(crate) key: MaterializationKey,
    pub(crate) deadline: Instant,
    pub(crate) cancellation: Arc<AtomicBool>,
    pub(crate) fail_after: Option<MaterializationStage>,
    pub(crate) hydration_byte_permit_bytes: usize,
    pub(crate) metadata_queue_depth: usize,
}

impl MaterializationRequest {
    pub(crate) fn new(key: MaterializationKey, timeout: Duration) -> Self {
        Self {
            key,
            deadline: Instant::now() + timeout,
            cancellation: Arc::new(AtomicBool::new(false)),
            fail_after: None,
            hydration_byte_permit_bytes: MAX_HYDRATION_STREAM_BYTES,
            metadata_queue_depth: MAX_METADATA_QUEUE_ITEMS,
        }
    }

    pub(crate) const fn with_hydration_byte_permit_bytes(mut self, bytes: usize) -> Self {
        self.hydration_byte_permit_bytes = bytes;
        self
    }

    pub(crate) const fn with_metadata_queue_depth(mut self, depth: usize) -> Self {
        self.metadata_queue_depth = depth;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MaterializationOutcome {
    pub(crate) disposition: MaterializationDisposition,
    pub(crate) operation_id: String,
    pub(crate) selection: GenerationSelection,
    pub(crate) maximum_buffer_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MaterializationError {
    Cancelled,
    Deadline,
    Generation(String),
    Operation(String),
    Native(String),
    ObjectStore(String),
    Lock(String),
    Coordination(String),
    BridgeUnavailable(String),
    Injected(MaterializationStage),
}

impl fmt::Display for MaterializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(formatter, "materialization request was cancelled"),
            Self::Deadline => write!(formatter, "materialization request deadline expired"),
            Self::Generation(message) => {
                write!(formatter, "materialization generation failed: {message}")
            }
            Self::Operation(message) => {
                write!(formatter, "materialization operation failed: {message}")
            }
            Self::Native(message) => write!(formatter, "native reconstruction failed: {message}"),
            Self::ObjectStore(message) => {
                write!(formatter, "materialization object store failed: {message}")
            }
            Self::Lock(message) => {
                write!(formatter, "materialization writer lock failed: {message}")
            }
            Self::Coordination(message) => {
                write!(formatter, "materialization coordination failed: {message}")
            }
            Self::BridgeUnavailable(message) => {
                write!(
                    formatter,
                    "materialization GC bridge unavailable: {message}"
                )
            }
            Self::Injected(stage) => {
                write!(formatter, "injected materialization stop after {stage:?}")
            }
        }
    }
}

impl std::error::Error for MaterializationError {}

impl From<GenerationError> for MaterializationError {
    fn from(error: GenerationError) -> Self {
        Self::Generation(error.to_string())
    }
}

impl From<MaterializationOperationError> for MaterializationError {
    fn from(error: MaterializationOperationError) -> Self {
        match error {
            MaterializationOperationError::Generation(message) => Self::Generation(message),
            error => Self::Operation(error.to_string()),
        }
    }
}

impl From<NativeBackendError> for MaterializationError {
    fn from(error: NativeBackendError) -> Self {
        match error {
            NativeBackendError::Cancelled(message) if message == "cancelled" => Self::Cancelled,
            NativeBackendError::Cancelled(message) if message == "deadline" => Self::Deadline,
            error => Self::Native(error.to_string()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct MaterializationCoordinator {
    storage_root: PathBuf,
    generations: GenerationStore,
    backend: NativeBackend,
    observation: Option<HiddenValidationObservation>,
    supervisor: Arc<StorageSupervisor>,
    bridge: Arc<dyn MaterializationGcBridge>,
}

impl MaterializationCoordinator {
    pub(crate) fn new(storage_root: PathBuf) -> Result<Self, MaterializationError> {
        let supervisor = shared_supervisor_for_root(&storage_root).map_err(supervisor_error)?;
        Self::new_supervised(storage_root, supervisor)
    }

    pub(crate) fn new_supervised(
        storage_root: PathBuf,
        supervisor: Arc<StorageSupervisor>,
    ) -> Result<Self, MaterializationError> {
        Self::new_supervised_with_bridge(
            storage_root,
            supervisor,
            Arc::new(DisabledMaterializationGcBridge),
        )
    }

    pub(crate) fn new_supervised_with_bridge(
        storage_root: PathBuf,
        supervisor: Arc<StorageSupervisor>,
        bridge: Arc<dyn MaterializationGcBridge>,
    ) -> Result<Self, MaterializationError> {
        Ok(Self {
            generations: GenerationStore::new(storage_root.clone())?,
            storage_root,
            backend: NativeBackend::new(),
            observation: None,
            supervisor,
            bridge,
        })
    }

    pub(crate) fn new_observed(
        storage_root: PathBuf,
        observation: HiddenValidationObservation,
    ) -> Result<Self, MaterializationError> {
        let mut coordinator = Self::new(storage_root)?;
        coordinator.observation = Some(observation);
        Ok(coordinator)
    }

    pub(crate) fn new_supervised_observed(
        storage_root: PathBuf,
        supervisor: Arc<StorageSupervisor>,
        observation: HiddenValidationObservation,
    ) -> Result<Self, MaterializationError> {
        let mut coordinator = Self::new_supervised(storage_root, supervisor)?;
        coordinator.observation = Some(observation);
        Ok(coordinator)
    }

    pub(crate) fn lookup(
        &self,
        key: &MaterializationKey,
    ) -> Result<Option<GenerationSelection>, MaterializationError> {
        self.lookup_verified(key)
    }

    /// Resolve a selected generation for warm activation without reopening
    /// logical objects or hashing the immutable carrier tree.
    pub(crate) fn lookup_warm(
        &self,
        key: &MaterializationKey,
    ) -> Result<Option<GenerationSelection>, MaterializationError> {
        key.validate()?;
        let Some(selection) = self.generations.lookup_current(key)? else {
            return Ok(None);
        };
        self.backend.validate_warm_capabilities(
            &selection.manifest.required_capabilities,
            &selection.manifest.provided_capabilities,
        )?;
        Ok(Some(selection))
    }

    pub(crate) fn recover_operation_path(
        &self,
        operation_path: &Path,
        writer_lock: &StorageWriterLockLease,
    ) -> Result<(), MaterializationError> {
        let mut operation =
            MaterializationOperation::load_path(self.storage_root.clone(), operation_path)?;
        let key = operation.key()?;
        match operation.state().phase {
            MaterializationPhase::Building
                if operation.state().checkpoint < MaterializationCheckpoint::ManifestDurable =>
            {
                let tuple = operation.state().generation.zip(operation.state().fence);
                operation.transition(
                    MaterializationPhase::Terminal,
                    tuple,
                    None,
                    Some("recovery_reaped".to_owned()),
                    unix_now()?,
                )?;
                operation.reap_work()?;
                return Ok(());
            }
            MaterializationPhase::Building => {
                let tuple = operation_tuple(&operation)?;
                operation.transition(
                    MaterializationPhase::Ready,
                    Some(tuple),
                    None,
                    None,
                    unix_now()?,
                )?;
            }
            MaterializationPhase::Ready | MaterializationPhase::Published => {}
            MaterializationPhase::Terminal => {
                operation.reap_work()?;
                return Ok(());
            }
        }
        let (generation, fence) = operation_tuple(&operation)?;
        let selection = self.generations.read_generation(&key, generation)?;
        if selection.manifest.fence != fence {
            return Err(MaterializationError::Generation(
                "recovery generation fence differs from STATE".to_owned(),
            ));
        }
        self.verify_selection(&key, &selection)?;
        MaterializationPublisher::new(self.generations.clone(), self.bridge.clone()).publish(
            &key,
            &selection,
            &mut operation,
            writer_lock,
            unix_now()?,
        )?;
        if operation.state().phase == MaterializationPhase::Published {
            operation.transition(
                MaterializationPhase::Terminal,
                Some((generation, fence)),
                None,
                None,
                unix_now()?,
            )?;
        }
        operation.reap_work()?;
        Ok(())
    }

    pub(crate) fn materialize(
        &self,
        request: &MaterializationRequest,
        writer_lock: &StorageWriterLockLease,
    ) -> Result<MaterializationOutcome, MaterializationError> {
        check_request(request)?;
        request.key.validate()?;

        let key = request.key.id()?.hex();
        for _ in 0..8 {
            match self
                .supervisor
                .admit_materialization(key.clone(), request.deadline, request.cancellation.as_ref())
                .map_err(supervisor_error)?
            {
                MaterializationAdmission::Owner(owner) => {
                    let _observed_owner = self
                        .observation
                        .as_ref()
                        .map(HiddenValidationObservation::begin_materialization_owner);
                    return catch_unwind(AssertUnwindSafe(|| {
                        self.run_owner(request, writer_lock, &owner, None)
                    }))
                    .unwrap_or_else(|payload| {
                        Err(MaterializationError::Coordination(format!(
                            "materialization owner panicked: {}",
                            panic_message(payload)
                        )))
                    });
                }
                MaterializationAdmission::Waiter(waiter) => {
                    let _observed_waiter = self
                        .observation
                        .as_ref()
                        .map(HiddenValidationObservation::begin_materialization_waiter);
                    waiter
                        .wait(request.deadline, request.cancellation.as_ref())
                        .map_err(supervisor_error)?;
                    check_request(request)?;
                    if let Some(selection) = self.lookup_verified(&request.key)? {
                        let mut outcome =
                            self.reuse_selection(&request.key, selection, writer_lock)?;
                        outcome.disposition = MaterializationDisposition::Shared;
                        return Ok(outcome);
                    }
                }
            }
        }
        Err(MaterializationError::Coordination(
            "same-key owner retry limit exceeded".to_owned(),
        ))
    }

    /// Reconstruct the selected materialization as a private Ready generation
    /// and publish it through the exact same bounded publisher as a cold build.
    ///
    /// Squash preserves the typed logical and attribution roots. The producer
    /// owns no selector switch; replacement remains disabled unless the injected
    /// Stage 05 bridge can admit the new root and accept the exact old subject.
    pub(crate) fn squash(
        &self,
        request: &MaterializationRequest,
        expected_prior: &GenerationSelection,
        writer_lock: &StorageWriterLockLease,
    ) -> Result<MaterializationOutcome, MaterializationError> {
        check_request(request)?;
        request.key.validate()?;
        let prior = self
            .generations
            .read_generation(&request.key, expected_prior.manifest.generation)?;
        if publication_subject(&prior) != publication_subject(expected_prior) {
            return Err(MaterializationError::Generation(
                "candidate squash prior subject differs from the installed generation".to_owned(),
            ));
        }
        // The squash output is reconstructed independently from the typed
        // logical root and fully verified before installation. Hashing the
        // immutable prior carrier here would not strengthen that proof; the
        // exact prior manifest subject and CURRENT are checked below.
        let producer = CandidateSquashProducer::new(prior);
        let key = request.key.id()?.hex();
        for _ in 0..8 {
            match self
                .supervisor
                .admit_materialization(key.clone(), request.deadline, request.cancellation.as_ref())
                .map_err(supervisor_error)?
            {
                MaterializationAdmission::Owner(owner) => {
                    let _observed_owner = self
                        .observation
                        .as_ref()
                        .map(HiddenValidationObservation::begin_materialization_owner);
                    return catch_unwind(AssertUnwindSafe(|| {
                        self.run_owner(request, writer_lock, &owner, Some(&producer))
                    }))
                    .unwrap_or_else(|payload| {
                        Err(MaterializationError::Coordination(format!(
                            "candidate squash owner panicked: {}",
                            panic_message(payload)
                        )))
                    });
                }
                MaterializationAdmission::Waiter(waiter) => {
                    let _observed_waiter = self
                        .observation
                        .as_ref()
                        .map(HiddenValidationObservation::begin_materialization_waiter);
                    waiter
                        .wait(request.deadline, request.cancellation.as_ref())
                        .map_err(supervisor_error)?;
                    check_request(request)?;
                    if let Some(selection) = self.lookup_verified(&request.key)? {
                        let operation = producer.open_operation(
                            self.storage_root.clone(),
                            &request.key,
                            &self.generations,
                            unix_now()?,
                        )?;
                        if publication_subject(&selection) != producer.prior_subject()
                            && producer.selected_by(&operation, &selection)
                        {
                            producer.validate_ready(&request.key, &selection)?;
                            return Ok(MaterializationOutcome {
                                disposition: MaterializationDisposition::Shared,
                                operation_id: operation.operation_id().to_owned(),
                                selection,
                                maximum_buffer_bytes: operation.state().maximum_buffer_bytes,
                            });
                        }
                    }
                }
            }
        }
        Err(MaterializationError::Coordination(
            "same-key squash owner retry limit exceeded".to_owned(),
        ))
    }

    fn run_owner(
        &self,
        request: &MaterializationRequest,
        writer_lock: &StorageWriterLockLease,
        owner: &MaterializationOwner,
        squash: Option<&CandidateSquashProducer>,
    ) -> Result<MaterializationOutcome, MaterializationError> {
        check_request(request)?;
        let now = unix_now()?;
        let mut prepared_ready = None;
        let mut prepared_terminal = None;
        let mut published_selection = None;
        let mut squash_operation = match squash {
            Some(squash) => Some(squash.open_operation(
                self.storage_root.clone(),
                &request.key,
                &self.generations,
                now,
            )?),
            None => None,
        };
        if let (Some(squash), Some(operation)) = (squash, squash_operation.as_mut()) {
            if operation.state().phase == MaterializationPhase::Terminal
                && operation.state().terminal_outcome
                    == Some(MaterializationTerminalOutcome::Succeeded)
            {
                let selection = self.lookup_verified(&request.key)?.ok_or_else(|| {
                    MaterializationError::Generation(
                        "terminal squash operation has no valid CURRENT".to_owned(),
                    )
                })?;
                if !squash.selected_by(operation, &selection) {
                    return Err(MaterializationError::Operation(
                        "terminal squash operation does not select CURRENT".to_owned(),
                    ));
                }
                squash.validate_ready(&request.key, &selection)?;
                return Ok(MaterializationOutcome {
                    disposition: MaterializationDisposition::Reused,
                    operation_id: operation.operation_id().to_owned(),
                    selection,
                    maximum_buffer_bytes: operation.state().maximum_buffer_bytes,
                });
            }
            if operation.state().phase == MaterializationPhase::Published {
                let generation = operation.state().generation.ok_or_else(|| {
                    MaterializationError::Operation(
                        "published squash operation omitted its generation".to_owned(),
                    )
                })?;
                let ready = self.generations.read_generation(&request.key, generation)?;
                squash.validate_ready(&request.key, &ready)?;
                let selection =
                    MaterializationPublisher::new(self.generations.clone(), self.bridge.clone())
                        .publish(&request.key, &ready, operation, writer_lock, unix_now()?)?;
                operation.transition(
                    MaterializationPhase::Terminal,
                    Some((selection.manifest.generation, selection.manifest.fence)),
                    None,
                    None,
                    unix_now()?,
                )?;
                operation.reap_work()?;
                return Ok(MaterializationOutcome {
                    disposition: MaterializationDisposition::Reused,
                    operation_id: operation.operation_id().to_owned(),
                    selection,
                    maximum_buffer_bytes: operation.state().maximum_buffer_bytes,
                });
            }
        }
        let mut target = owner
            .acquire_target(
                request.hydration_byte_permit_bytes,
                request.deadline,
                request.cancellation.as_ref(),
            )
            .map_err(supervisor_error)?;
        let (byte_permits, fd_permits) = target.reserved_permits();
        let mut observed_target = self
            .observation
            .as_ref()
            .map(|observation| observation.begin_materialization_target(byte_permits, fd_permits));
        if let Some(squash) = squash {
            if let Some(subject) = self.generations.lookup_current_subject(&request.key)? {
                // Recheck the selector subject here so a concurrent
                // replacement still fails closed.
                squash.validate_current(&subject)?;
            } else {
                return Err(MaterializationError::Generation(
                    "candidate squash source CURRENT disappeared".to_owned(),
                ));
            }
        } else if let Some(selection) = self.lookup_verified(&request.key)? {
            return self.reuse_selection(&request.key, selection, writer_lock);
        }

        let prior_generation = match squash {
            Some(squash) => Some(squash.prior().clone()),
            None => self.generations.lookup_current(&request.key)?,
        };
        let mut operation = match squash {
            Some(_) => squash_operation.take().ok_or_else(|| {
                MaterializationError::Operation(
                    "candidate squash omitted its common operation".to_owned(),
                )
            })?,
            None => MaterializationOperation::open_with_holds(
                self.storage_root.clone(),
                &request.key,
                Vec::new(),
                prior_generation.as_ref().map(publication_subject),
                now,
            )?,
        };
        if operation.state().phase == MaterializationPhase::Terminal
            && !matches!(
                operation.state().terminal_outcome,
                Some(MaterializationTerminalOutcome::Succeeded)
            )
        {
            operation.restart(now)?;
        }
        if operation.state().phase == MaterializationPhase::Terminal {
            let selection = self.lookup_verified(&request.key)?.ok_or_else(|| {
                MaterializationError::Generation(
                    "terminal operation has no valid CURRENT".to_owned(),
                )
            })?;
            if let Some(squash) = squash {
                if !squash.selected_by(&operation, &selection) {
                    return Err(MaterializationError::Operation(
                        "terminal squash operation does not select CURRENT".to_owned(),
                    ));
                }
                squash.validate_ready(&request.key, &selection)?;
            }
            return Ok(MaterializationOutcome {
                disposition: MaterializationDisposition::Reused,
                operation_id: operation.operation_id().to_owned(),
                selection,
                maximum_buffer_bytes: operation.state().maximum_buffer_bytes,
            });
        }

        let store = LooseObjectStore::new(self.storage_root.clone())
            .map_err(|error| MaterializationError::ObjectStore(error.to_string()))?;
        let mut pages = PersistentPages::new(&store);
        validate_attribution_binding(&mut pages, &request.key)?;
        let workspace_profile = self.supervisor.workspace_profile();
        let (required_capabilities, build_reservation_bytes) = match squash {
            Some(squash) => {
                let prior = &squash.prior().manifest;
                // Squash reconstructs the same immutable typed root on the
                // same target filesystem. The verified prior target is the
                // tightest available allocation prediction, while warm
                // capability validation still fails closed if the backend
                // profile changed.
                self.backend.validate_warm_capabilities(
                    &prior.required_capabilities,
                    &prior.provided_capabilities,
                )?;
                (
                    prior.required_capabilities.clone(),
                    self.backend
                        .build_reservation_from_verified_target(prior.allocated_bytes)?,
                )
            }
            None => {
                let preflight = self.backend.preflight(
                    &mut pages,
                    request.key.root,
                    workspace_profile.allocation_unit,
                )?;
                (
                    preflight.required_capabilities,
                    preflight.build_reservation_bytes,
                )
            }
        };
        target
            .reserve_workspace(
                build_reservation_bytes,
                request.deadline,
                request.cancellation.as_ref(),
            )
            .map_err(supervisor_error)?;
        if let Some(observed_target) = observed_target.as_mut() {
            observed_target.reserve_workspace(build_reservation_bytes);
        }
        let provided_capabilities = self.backend.provided_capabilities();
        check_request(request)?;
        if operation.state().phase == MaterializationPhase::Building
            && operation.state().checkpoint == MaterializationCheckpoint::Admitted
        {
            let installed_carrier = if operation.has_preallocated_build() {
                let (generation, _) = operation_tuple(&operation)?;
                self.generations
                    .installed_carrier(request.key.id()?, generation)?
            } else {
                None
            };
            operation.reap_work()?;
            let build = match installed_carrier {
                Some(installed_carrier) => {
                    match self
                        .backend
                        .verify(&mut pages, request.key.root, &installed_carrier)
                    {
                        Ok(verified) => verified,
                        Err(error) => {
                            operation.transition(
                                MaterializationPhase::Terminal,
                                None,
                                None,
                                Some("native_verification".to_owned()),
                                unix_now()?,
                            )?;
                            operation.reap_work()?;
                            return Err(error.into());
                        }
                    }
                }
                None => {
                    let build = self.backend.reconstruct_bounded(
                        &mut pages,
                        request.key.root,
                        &operation.work_carrier(),
                        NativeReconstructionResources {
                            hydration_byte_permit_bytes: request.hydration_byte_permit_bytes,
                            metadata_queue_depth: request.metadata_queue_depth,
                            target: &target,
                            observation: self.observation.as_ref(),
                        },
                        || match check_request(request) {
                            Ok(()) => Ok(()),
                            Err(MaterializationError::Cancelled) => {
                                Err(NativeBackendError::Cancelled("cancelled".to_owned()))
                            }
                            Err(MaterializationError::Deadline) => {
                                Err(NativeBackendError::Cancelled("deadline".to_owned()))
                            }
                            Err(error) => Err(NativeBackendError::Cancelled(error.to_string())),
                        },
                    );
                    let build = match build {
                        Ok(build) => build,
                        Err(error) => {
                            let code = match &error {
                                NativeBackendError::Cancelled(message) if message == "deadline" => {
                                    "deadline"
                                }
                                NativeBackendError::Cancelled(_) => "cancelled",
                                _ => "native_reconstruction",
                            };
                            operation.transition(
                                MaterializationPhase::Terminal,
                                None,
                                None,
                                Some(code.to_owned()),
                                unix_now()?,
                            )?;
                            operation.reap_work()?;
                            return Err(error.into());
                        }
                    };
                    let verified = match self.backend.verify(
                        &mut pages,
                        request.key.root,
                        &operation.work_carrier(),
                    ) {
                        Ok(verified) => verified,
                        Err(error) => {
                            operation.transition(
                                MaterializationPhase::Terminal,
                                None,
                                None,
                                Some("native_verification".to_owned()),
                                unix_now()?,
                            )?;
                            operation.reap_work()?;
                            return Err(error.into());
                        }
                    };
                    if build.native_tree_sha256 != verified.native_tree_sha256
                        || build.entry_count != verified.entry_count
                        || build.logical_bytes != verified.logical_bytes
                        || build.allocated_bytes != verified.allocated_bytes
                    {
                        let error = NativeBackendError::Invalid(
                            "reconstructed carrier summary differs from verified carrier"
                                .to_owned(),
                        );
                        operation.transition(
                            MaterializationPhase::Terminal,
                            None,
                            None,
                            Some("native_verification".to_owned()),
                            unix_now()?,
                        )?;
                        operation.reap_work()?;
                        return Err(error.into());
                    }
                    super::native_backend::NativeBuildResult {
                        maximum_buffer_bytes: build
                            .maximum_buffer_bytes
                            .max(verified.maximum_buffer_bytes),
                        ..build
                    }
                }
            };
            check_request(request)?;
            let build = MaterializationOperationBuild {
                native_tree_sha256: build.native_tree_sha256,
                entry_count: build.entry_count,
                logical_bytes: build.logical_bytes,
                allocated_bytes: build.allocated_bytes,
                maximum_buffer_bytes: build.maximum_buffer_bytes,
                required_capabilities: required_capabilities.clone(),
                provided_capabilities: provided_capabilities.clone(),
            };
            if operation.has_preallocated_build() {
                operation.accept_preallocated_build(build, unix_now()?)?;
            } else {
                let old = self.generations.lookup_current(&request.key)?;
                let tuple = self
                    .generations
                    .next_generation(request.key.id()?, old.as_ref())?;
                operation.advance(
                    MaterializationCheckpoint::GenerationAllocated,
                    Some(tuple),
                    Some(build),
                    unix_now()?,
                )?;
            }
            fail_after(request, MaterializationStage::CarrierSynced)?;
            fail_after(request, MaterializationStage::GenerationAllocated)?;
        }

        if operation.state().phase == MaterializationPhase::Building
            && operation.state().checkpoint == MaterializationCheckpoint::GenerationAllocated
        {
            check_request(request)?;
            let (generation, _) = operation_tuple(&operation)?;
            self.generations.install_carrier(
                request.key.id()?,
                generation,
                &operation.work_carrier(),
            )?;
            fail_after(request, MaterializationStage::CarrierInstalled)?;
            check_request(request)?;
            let manifest = manifest_from_operation(&request.key, &operation, unix_now()?)?;
            let ready = self.generations.publish_manifest(&request.key, &manifest)?;
            if let Some(squash) = squash {
                squash.validate_ready(&request.key, &ready)?;
            }
            let publisher =
                MaterializationPublisher::new(self.generations.clone(), self.bridge.clone());
            let (old, prepared_publication) = match squash {
                Some(squash) => publisher.prepare_with_verified_old(
                    &ready,
                    &mut operation,
                    Some(squash.prior().clone()),
                    unix_now()?,
                )?,
                None => publisher.prepare(&request.key, &ready, &mut operation, unix_now()?)?,
            };
            prepared_ready = Some((ready, old, prepared_publication));
            fail_after(request, MaterializationStage::ManifestDurable)?;
        }

        if operation.state().phase == MaterializationPhase::Ready {
            check_request(request)?;
            let publisher =
                MaterializationPublisher::new(self.generations.clone(), self.bridge.clone());
            let (ready, old, prepared_publication) = match prepared_ready.take() {
                Some(prepared) => prepared,
                None => {
                    let (generation, _) = operation_tuple(&operation)?;
                    let ready = self.generations.read_generation(&request.key, generation)?;
                    self.verify_selection(&request.key, &ready)?;
                    if let Some(squash) = squash {
                        squash.validate_ready(&request.key, &ready)?;
                    }
                    let (old, prepared_publication) = match squash {
                        Some(squash) => publisher.prepare_with_verified_old(
                            &ready,
                            &mut operation,
                            Some(squash.prior().clone()),
                            unix_now()?,
                        )?,
                        None => {
                            publisher.prepare(&request.key, &ready, &mut operation, unix_now()?)?
                        }
                    };
                    (ready, old, prepared_publication)
                }
            };
            let (selection, terminal) = publisher.publish_prepared(
                &request.key,
                &ready,
                old.as_ref(),
                Some(prepared_publication),
                &mut operation,
                writer_lock,
            )?;
            published_selection = Some(selection);
            prepared_terminal = terminal;
            fail_after(request, MaterializationStage::CurrentDurable)?;
        }

        if operation.state().phase == MaterializationPhase::Published {
            fail_after(request, MaterializationStage::BeforeTerminal)?;
            let tuple = operation_tuple(&operation)?;
            // STATE and work share the exact operation directory. The Terminal
            // STATE replacement's parent fsync makes both this cleanup and the
            // terminal witness durable in one barrier.
            operation.reap_work_before_terminal()?;
            match prepared_terminal.take() {
                Some(prepared_terminal) => {
                    operation.commit_prepared_terminal(prepared_terminal)?;
                }
                None => {
                    operation.transition(
                        MaterializationPhase::Terminal,
                        Some(tuple),
                        None,
                        None,
                        unix_now()?,
                    )?;
                }
            }
        }

        let selection = match published_selection {
            Some(selection) => selection,
            None => self.lookup_verified(&request.key)?.ok_or_else(|| {
                MaterializationError::Generation(
                    "completed operation has no valid CURRENT".to_owned(),
                )
            })?,
        };
        if let Some(squash) = squash {
            squash.validate_ready(&request.key, &selection)?;
        }
        Ok(MaterializationOutcome {
            disposition: MaterializationDisposition::Built,
            operation_id: operation.operation_id().to_owned(),
            selection,
            maximum_buffer_bytes: operation.state().maximum_buffer_bytes,
        })
    }

    fn reuse_selection(
        &self,
        key: &MaterializationKey,
        selection: GenerationSelection,
        writer_lock: &StorageWriterLockLease,
    ) -> Result<MaterializationOutcome, MaterializationError> {
        let mut maximum_buffer_bytes = None;
        if let Some(mut operation) = MaterializationOperation::load(self.storage_root.clone(), key)?
        {
            if operation.operation_id() != selection.manifest.build_operation_id {
                return Err(MaterializationError::Operation(
                    "CURRENT build operation does not match deterministic operation".to_owned(),
                ));
            }
            let tuple = (selection.manifest.generation, selection.manifest.fence);
            match operation.state().phase {
                MaterializationPhase::Building
                    if operation.state().checkpoint
                        == MaterializationCheckpoint::ManifestDurable =>
                {
                    operation.transition(
                        MaterializationPhase::Ready,
                        Some(tuple),
                        None,
                        None,
                        unix_now()?,
                    )?;
                    MaterializationPublisher::new(self.generations.clone(), self.bridge.clone())
                        .publish(key, &selection, &mut operation, writer_lock, unix_now()?)?;
                }
                MaterializationPhase::Ready => {
                    MaterializationPublisher::new(self.generations.clone(), self.bridge.clone())
                        .publish(key, &selection, &mut operation, writer_lock, unix_now()?)?;
                }
                MaterializationPhase::Published => {}
                MaterializationPhase::Terminal
                    if operation.state().terminal_outcome
                        == Some(MaterializationTerminalOutcome::Succeeded) => {}
                phase => {
                    return Err(MaterializationError::Operation(format!(
                        "valid CURRENT conflicts with operation phase {phase:?}"
                    )));
                }
            }
            if operation.state().phase == MaterializationPhase::Published {
                operation.transition(
                    MaterializationPhase::Terminal,
                    Some(tuple),
                    None,
                    None,
                    unix_now()?,
                )?;
                operation.reap_work()?;
            }
            maximum_buffer_bytes = operation.state().maximum_buffer_bytes;
        }
        Ok(MaterializationOutcome {
            disposition: MaterializationDisposition::Reused,
            operation_id: selection.manifest.build_operation_id.clone(),
            selection,
            maximum_buffer_bytes,
        })
    }

    fn lookup_verified(
        &self,
        key: &MaterializationKey,
    ) -> Result<Option<GenerationSelection>, MaterializationError> {
        key.validate()?;
        let Some(selection) = self.generations.lookup_current(key)? else {
            return Ok(None);
        };
        self.verify_selection(key, &selection)?;
        Ok(Some(selection))
    }

    fn verify_selection(
        &self,
        key: &MaterializationKey,
        selection: &GenerationSelection,
    ) -> Result<(), MaterializationError> {
        let store = LooseObjectStore::new(self.storage_root.clone())
            .map_err(|error| MaterializationError::ObjectStore(error.to_string()))?;
        let mut pages = PersistentPages::new(&store);
        validate_attribution_binding(&mut pages, key)?;
        let required_capabilities = self
            .backend
            .preflight(
                &mut pages,
                key.root,
                self.supervisor.workspace_profile().allocation_unit,
            )?
            .required_capabilities;
        let provided_capabilities = self.backend.provided_capabilities();
        let verified = self
            .backend
            .verify(&mut pages, key.root, &selection.carrier_path)?;
        let manifest = &selection.manifest;
        if required_capabilities != manifest.required_capabilities
            || provided_capabilities != manifest.provided_capabilities
            || verified.native_tree_sha256 != manifest.native_tree_sha256
            || verified.entry_count != manifest.entry_count
            || verified.logical_bytes != manifest.logical_bytes
            || verified.allocated_bytes != manifest.allocated_bytes
        {
            return Err(MaterializationError::Native(
                "selected native carrier verification differs from manifest".to_owned(),
            ));
        }
        Ok(())
    }
}

fn validate_attribution_binding(
    pages: &mut PersistentPages<'_>,
    key: &MaterializationKey,
) -> Result<(), MaterializationError> {
    let (content, _) = pages
        .load_attribution_root(key.attribution_root)
        .map_err(|error| {
            MaterializationError::Native(format!(
                "materialization attribution root is invalid: {error}"
            ))
        })?;
    if content != key.root {
        return Err(MaterializationError::Native(
            "materialization attribution root names another content root".to_owned(),
        ));
    }
    Ok(())
}

fn manifest_from_operation(
    key: &MaterializationKey,
    operation: &MaterializationOperation,
    completed_unix_seconds: u64,
) -> Result<GenerationManifest, MaterializationError> {
    let state = operation.state();
    let (generation, fence) = operation_tuple(operation)?;
    let native_tree_sha256 = state
        .native_tree_sha256
        .clone()
        .ok_or_else(|| MaterializationError::Operation("build digest is absent".to_owned()))?;
    Ok(GenerationManifest {
        schema: "layerstack-materialization-generation-v2".to_owned(),
        schema_version: 2,
        materialization_id: key.id()?.hex(),
        root_id: digest_string(key.root.digest()),
        attribution_root_id: digest_string(key.attribution_root.digest()),
        backend_kind: key.backend_kind.clone(),
        backend_format_version: key.backend_format_version,
        target_profile: key.target_profile.clone(),
        generation,
        fence,
        carriers: vec![CarrierDescriptor {
            carrier_id: "native".to_owned(),
            relative_path: "carriers/native".to_owned(),
            native_tree_sha256: native_tree_sha256.clone(),
        }],
        required_capabilities: state.required_capabilities.clone().ok_or_else(|| {
            MaterializationError::Operation("required capability record is absent".to_owned())
        })?,
        provided_capabilities: state.provided_capabilities.clone().ok_or_else(|| {
            MaterializationError::Operation("provided capability record is absent".to_owned())
        })?,
        logical_verification_root: digest_string(key.root.digest()),
        native_tree_sha256,
        entry_count: state
            .entry_count
            .ok_or_else(|| MaterializationError::Operation("entry count is absent".to_owned()))?,
        logical_bytes: state.logical_bytes.ok_or_else(|| {
            MaterializationError::Operation("logical byte count is absent".to_owned())
        })?,
        allocated_bytes: state.allocated_bytes.ok_or_else(|| {
            MaterializationError::Operation("allocated byte count is absent".to_owned())
        })?,
        allocation_method: "stat.st_blocks*512".to_owned(),
        build_operation_id: operation.operation_id().to_owned(),
        completed_unix_seconds,
    })
}

fn operation_tuple(
    operation: &MaterializationOperation,
) -> Result<(u64, u64), MaterializationError> {
    operation
        .state()
        .generation
        .zip(operation.state().fence)
        .ok_or_else(|| MaterializationError::Operation("generation tuple is absent".to_owned()))
}

fn check_request(request: &MaterializationRequest) -> Result<(), MaterializationError> {
    if !(MIN_HYDRATION_STREAM_BYTES..=MAX_HYDRATION_STREAM_BYTES)
        .contains(&request.hydration_byte_permit_bytes)
    {
        return Err(MaterializationError::Coordination(format!(
            "hydration byte permit must be in {MIN_HYDRATION_STREAM_BYTES}..={MAX_HYDRATION_STREAM_BYTES}"
        )));
    }
    if !(1..=MAX_METADATA_QUEUE_ITEMS).contains(&request.metadata_queue_depth) {
        return Err(MaterializationError::Coordination(format!(
            "metadata queue depth must be in 1..={MAX_METADATA_QUEUE_ITEMS}"
        )));
    }
    if request.cancellation.load(Ordering::Acquire) {
        return Err(MaterializationError::Cancelled);
    }
    if Instant::now() >= request.deadline {
        return Err(MaterializationError::Deadline);
    }
    Ok(())
}

fn fail_after(
    request: &MaterializationRequest,
    stage: MaterializationStage,
) -> Result<(), MaterializationError> {
    if request.fail_after == Some(stage) {
        Err(MaterializationError::Injected(stage))
    } else {
        Ok(())
    }
}

fn unix_now() -> Result<u64, MaterializationError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| MaterializationError::Coordination(error.to_string()))
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn supervisor_error(error: SupervisorError) -> MaterializationError {
    match error {
        SupervisorError::Cancelled => MaterializationError::Cancelled,
        SupervisorError::Deadline => MaterializationError::Deadline,
        error => MaterializationError::Coordination(error.to_string()),
    }
}
