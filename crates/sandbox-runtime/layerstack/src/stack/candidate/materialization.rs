use std::collections::HashMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::generation::{
    digest_string, CarrierDescriptor, GenerationError, GenerationManifest, GenerationSelection,
    GenerationSnapshot, GenerationStore, MaterializationId, MaterializationKey,
};
use super::materialization_operation::{
    MaterializationOperation, MaterializationOperationBuild, MaterializationOperationError,
    MaterializationPhase,
};
use super::native_backend::{
    NativeBackend, NativeBackendError, MAX_HYDRATION_STREAM_BYTES, MIN_HYDRATION_STREAM_BYTES,
};
use super::object_store::LooseObjectStore;
use super::refs::root_has_pin_or_source_lease;
use super::tree::PersistentPages;
use crate::lock::StorageWriterLockLease;
use crate::stack::HiddenValidationObservation;
use crate::Sha256Digest;

const MAX_BUILD_WORKERS: usize = 4;
const WAIT_SLICE: Duration = Duration::from_millis(50);

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
}

impl MaterializationRequest {
    pub(crate) fn new(key: MaterializationKey, timeout: Duration) -> Self {
        Self {
            key,
            deadline: Instant::now() + timeout,
            cancellation: Arc::new(AtomicBool::new(false)),
            fail_after: None,
            hydration_byte_permit_bytes: MAX_HYDRATION_STREAM_BYTES,
        }
    }

    pub(crate) const fn with_hydration_byte_permit_bytes(mut self, bytes: usize) -> Self {
        self.hydration_byte_permit_bytes = bytes;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GenerationRetentionReason {
    CurrentSelection,
    PinOrSourceLease,
    ExactGenerationLease,
    LastVerifiedNativeLocator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GenerationRetirementTicket {
    key: MaterializationKey,
    materialization_id: MaterializationId,
    generation: u64,
    snapshot: GenerationSnapshot,
    observed_unix_seconds: u64,
    not_before_unix_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GenerationRetirementOutcome {
    Protected(GenerationRetentionReason),
    GraceStarted(GenerationRetirementTicket),
    GracePending(GenerationRetirementTicket),
    Deleted,
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
        Self::Operation(error.to_string())
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
}

impl MaterializationCoordinator {
    pub(crate) fn new(storage_root: PathBuf) -> Result<Self, MaterializationError> {
        Ok(Self {
            generations: GenerationStore::new(storage_root.clone())?,
            storage_root,
            backend: NativeBackend::new(),
            observation: None,
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

    pub(crate) fn materialize(
        &self,
        request: &MaterializationRequest,
        writer_lock: &StorageWriterLockLease,
    ) -> Result<MaterializationOutcome, MaterializationError> {
        check_request(request)?;
        request.key.validate()?;
        if let Some(selection) = self.lookup_verified(&request.key)? {
            return self.reuse_selection(&request.key, selection);
        }

        let flight_key = FlightKey {
            storage_root: crate::fs::canonical_key(&self.storage_root),
            materialization_id: request.key.id()?,
        };
        let (flight, owner) = join_flight(&flight_key)?;
        if !owner {
            return wait_for_flight(flight, request);
        }

        let result = catch_unwind(AssertUnwindSafe(|| self.run_owner(request, writer_lock)))
            .unwrap_or_else(|payload| {
                Err(MaterializationError::Coordination(format!(
                    "materialization owner panicked: {}",
                    panic_message(payload)
                )))
            });
        finish_flight(&flight_key, &flight, result.clone());
        result
    }

    pub(crate) fn begin_generation_retirement(
        &self,
        key: &MaterializationKey,
        generation: u64,
        now_unix_seconds: u64,
        grace_seconds: u64,
        writer_lock: &StorageWriterLockLease,
    ) -> Result<GenerationRetirementOutcome, MaterializationError> {
        if grace_seconds == 0 {
            return Err(MaterializationError::Coordination(
                "generation retirement grace period must be nonzero".to_owned(),
            ));
        }
        let not_before_unix_seconds =
            now_unix_seconds.checked_add(grace_seconds).ok_or_else(|| {
                MaterializationError::Coordination(
                    "generation retirement grace period overflowed".to_owned(),
                )
            })?;
        let _guard = writer_lock
            .exclusive()
            .map_err(|error| MaterializationError::Lock(error.to_string()))?;
        let candidate = self.retirement_candidate(key, generation, now_unix_seconds)?;
        let Some(snapshot) = candidate else {
            return Ok(GenerationRetirementOutcome::Protected(
                self.retention_reason(key, generation, now_unix_seconds)?,
            ));
        };
        Ok(GenerationRetirementOutcome::GraceStarted(
            GenerationRetirementTicket {
                key: key.clone(),
                materialization_id: key.id()?,
                generation,
                snapshot,
                observed_unix_seconds: now_unix_seconds,
                not_before_unix_seconds,
            },
        ))
    }

    pub(crate) fn finish_generation_retirement(
        &self,
        ticket: &GenerationRetirementTicket,
        now_unix_seconds: u64,
        writer_lock: &StorageWriterLockLease,
    ) -> Result<GenerationRetirementOutcome, MaterializationError> {
        if now_unix_seconds < ticket.observed_unix_seconds
            || now_unix_seconds < ticket.not_before_unix_seconds
        {
            return Ok(GenerationRetirementOutcome::GracePending(ticket.clone()));
        }
        if ticket.key.id()? != ticket.materialization_id {
            return Err(MaterializationError::Coordination(
                "generation retirement ticket identity changed".to_owned(),
            ));
        }
        let _guard = writer_lock
            .exclusive()
            .map_err(|error| MaterializationError::Lock(error.to_string()))?;
        let candidate =
            self.retirement_candidate(&ticket.key, ticket.generation, now_unix_seconds)?;
        let Some(snapshot) = candidate else {
            return Ok(GenerationRetirementOutcome::Protected(
                self.retention_reason(&ticket.key, ticket.generation, now_unix_seconds)?,
            ));
        };
        if snapshot != ticket.snapshot {
            return Err(MaterializationError::Generation(
                "generation changed during retirement grace period".to_owned(),
            ));
        }
        self.generations.remove_generation(
            ticket.materialization_id,
            ticket.generation,
            &ticket.snapshot,
        )?;
        Ok(GenerationRetirementOutcome::Deleted)
    }

    fn run_owner(
        &self,
        request: &MaterializationRequest,
        writer_lock: &StorageWriterLockLease,
    ) -> Result<MaterializationOutcome, MaterializationError> {
        let _worker = acquire_worker(request)?;
        check_request(request)?;
        if let Some(selection) = self.lookup_verified(&request.key)? {
            return self.reuse_selection(&request.key, selection);
        }

        let store = LooseObjectStore::new(self.storage_root.clone())
            .map_err(|error| MaterializationError::ObjectStore(error.to_string()))?;
        let mut pages = PersistentPages::new(&store);
        let required_capabilities = self.backend.preflight(&mut pages, request.key.root)?;
        let provided_capabilities = self.backend.provided_capabilities();
        check_request(request)?;

        let now = unix_now()?;
        let mut operation =
            MaterializationOperation::open(self.storage_root.clone(), &request.key, now)?;
        if matches!(
            operation.state().phase,
            MaterializationPhase::Failed | MaterializationPhase::Cancelled
        ) {
            operation.restart(now)?;
        }
        if operation.state().phase == MaterializationPhase::TerminalBuilt {
            let selection = self.lookup_verified(&request.key)?.ok_or_else(|| {
                MaterializationError::Generation(
                    "terminal operation has no valid CURRENT".to_owned(),
                )
            })?;
            return Ok(MaterializationOutcome {
                disposition: MaterializationDisposition::Reused,
                operation_id: operation.operation_id().to_owned(),
                selection,
                maximum_buffer_bytes: operation.state().maximum_buffer_bytes,
            });
        }

        if matches!(
            operation.state().phase,
            MaterializationPhase::Owned | MaterializationPhase::Building
        ) {
            if operation.state().phase == MaterializationPhase::Building {
                operation.reap_work()?;
            }
            operation.transition(
                MaterializationPhase::Building,
                None,
                None,
                None,
                unix_now()?,
            )?;
            let build = self.backend.reconstruct_bounded(
                &mut pages,
                request.key.root,
                &operation.work_carrier(),
                request.hydration_byte_permit_bytes,
                self.observation.as_ref(),
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
                    let phase = match error {
                        NativeBackendError::Cancelled(ref message) if message == "cancelled" => {
                            MaterializationPhase::Cancelled
                        }
                        _ => MaterializationPhase::Failed,
                    };
                    let code = match &error {
                        NativeBackendError::Cancelled(message) if message == "deadline" => {
                            "deadline"
                        }
                        NativeBackendError::Cancelled(_) => "cancelled",
                        _ => "native_reconstruction",
                    };
                    operation.transition(phase, None, None, Some(code.to_owned()), unix_now()?)?;
                    operation.reap_work()?;
                    return Err(error.into());
                }
            };
            let verified =
                match self
                    .backend
                    .verify(&mut pages, request.key.root, &operation.work_carrier())
                {
                    Ok(verified) => verified,
                    Err(error) => {
                        operation.transition(
                            MaterializationPhase::Failed,
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
                    "reconstructed carrier summary differs from verified carrier".to_owned(),
                );
                operation.transition(
                    MaterializationPhase::Failed,
                    None,
                    None,
                    Some("native_verification".to_owned()),
                    unix_now()?,
                )?;
                operation.reap_work()?;
                return Err(error.into());
            }
            let build = super::native_backend::NativeBuildResult {
                maximum_buffer_bytes: build
                    .maximum_buffer_bytes
                    .max(verified.maximum_buffer_bytes),
                ..build
            };
            operation.transition(
                MaterializationPhase::CarrierSynced,
                None,
                Some(MaterializationOperationBuild {
                    native_tree_sha256: build.native_tree_sha256,
                    entry_count: build.entry_count,
                    logical_bytes: build.logical_bytes,
                    allocated_bytes: build.allocated_bytes,
                    maximum_buffer_bytes: build.maximum_buffer_bytes,
                    required_capabilities: required_capabilities.clone(),
                    provided_capabilities: provided_capabilities.clone(),
                }),
                None,
                unix_now()?,
            )?;
            fail_after(request, MaterializationStage::CarrierSynced)?;
        }

        if operation.state().phase == MaterializationPhase::CarrierSynced {
            check_request(request)?;
            let _guard = writer_lock
                .exclusive()
                .map_err(|error| MaterializationError::Lock(error.to_string()))?;
            if let Some(selection) = self.lookup_verified(&request.key)? {
                operation.transition(
                    MaterializationPhase::CurrentDurable,
                    Some((selection.manifest.generation, selection.manifest.fence)),
                    None,
                    None,
                    unix_now()?,
                )?;
            } else {
                let tuple = self.generations.next_generation(request.key.id()?)?;
                operation.transition(
                    MaterializationPhase::GenerationAllocated,
                    Some(tuple),
                    None,
                    None,
                    unix_now()?,
                )?;
            }
            drop(_guard);
            fail_after(request, MaterializationStage::GenerationAllocated)?;
        }

        if operation.state().phase == MaterializationPhase::GenerationAllocated {
            check_request(request)?;
            let (generation, fence) = operation_tuple(&operation)?;
            let _guard = writer_lock
                .exclusive()
                .map_err(|error| MaterializationError::Lock(error.to_string()))?;
            self.generations.install_carrier(
                request.key.id()?,
                generation,
                &operation.work_carrier(),
            )?;
            operation.transition(
                MaterializationPhase::CarrierInstalled,
                Some((generation, fence)),
                None,
                None,
                unix_now()?,
            )?;
            drop(_guard);
            fail_after(request, MaterializationStage::CarrierInstalled)?;
        }

        if operation.state().phase == MaterializationPhase::CarrierInstalled {
            check_request(request)?;
            let manifest = manifest_from_operation(&request.key, &operation, unix_now()?)?;
            let _guard = writer_lock
                .exclusive()
                .map_err(|error| MaterializationError::Lock(error.to_string()))?;
            self.generations.publish_manifest(&request.key, &manifest)?;
            operation.transition(
                MaterializationPhase::ManifestDurable,
                Some((manifest.generation, manifest.fence)),
                None,
                None,
                unix_now()?,
            )?;
            drop(_guard);
            fail_after(request, MaterializationStage::ManifestDurable)?;
        }

        if operation.state().phase == MaterializationPhase::ManifestDurable {
            check_request(request)?;
            let (generation, fence) = operation_tuple(&operation)?;
            let _guard = writer_lock
                .exclusive()
                .map_err(|error| MaterializationError::Lock(error.to_string()))?;
            self.generations
                .promote_generation(&request.key, generation)?;
            operation.transition(
                MaterializationPhase::CurrentDurable,
                Some((generation, fence)),
                None,
                None,
                unix_now()?,
            )?;
            drop(_guard);
            fail_after(request, MaterializationStage::CurrentDurable)?;
        }

        if operation.state().phase == MaterializationPhase::CurrentDurable {
            fail_after(request, MaterializationStage::BeforeTerminal)?;
            let tuple = operation_tuple(&operation)?;
            operation.transition(
                MaterializationPhase::TerminalBuilt,
                Some(tuple),
                None,
                None,
                unix_now()?,
            )?;
            operation.reap_work()?;
        }

        let selection = self.lookup_verified(&request.key)?.ok_or_else(|| {
            MaterializationError::Generation("completed operation has no valid CURRENT".to_owned())
        })?;
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
                MaterializationPhase::CarrierSynced | MaterializationPhase::ManifestDurable => {
                    operation.transition(
                        MaterializationPhase::CurrentDurable,
                        Some(tuple),
                        None,
                        None,
                        unix_now()?,
                    )?;
                }
                MaterializationPhase::CurrentDurable | MaterializationPhase::TerminalBuilt => {}
                phase => {
                    return Err(MaterializationError::Operation(format!(
                        "valid CURRENT conflicts with operation phase {phase:?}"
                    )));
                }
            }
            if operation.state().phase == MaterializationPhase::CurrentDurable {
                operation.transition(
                    MaterializationPhase::TerminalBuilt,
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
        let required_capabilities = self.backend.preflight(&mut pages, key.root)?;
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

    fn retirement_candidate(
        &self,
        key: &MaterializationKey,
        generation: u64,
        now_unix_seconds: u64,
    ) -> Result<Option<GenerationSnapshot>, MaterializationError> {
        if self
            .generations
            .lookup_current(key)?
            .is_some_and(|selection| selection.manifest.generation == generation)
        {
            return Ok(None);
        }
        let mut digest = Sha256Digest;
        if root_has_pin_or_source_lease(&self.storage_root, key.root.digest(), &mut digest)
            .map_err(|error| MaterializationError::Coordination(error.to_string()))?
        {
            return Ok(None);
        }
        let id = key.id()?;
        if self
            .generations
            .active_generation_lease_exists(id, generation, now_unix_seconds)?
        {
            return Ok(None);
        }
        let snapshot = self.generations.generation_snapshot(id, generation)?;
        let mut has_other_verified_locator = false;
        for candidate_generation in self.generations.generation_numbers(id)? {
            if candidate_generation == generation {
                continue;
            }
            let selection = match self.generations.read_generation(key, candidate_generation) {
                Ok(selection) => selection,
                Err(GenerationError::Corrupt(_) | GenerationError::NotFound) => continue,
                Err(error) => return Err(error.into()),
            };
            if self.verify_selection(key, &selection).is_ok() {
                has_other_verified_locator = true;
                break;
            }
        }
        if !has_other_verified_locator {
            return Ok(None);
        }
        Ok(Some(snapshot))
    }

    fn retention_reason(
        &self,
        key: &MaterializationKey,
        generation: u64,
        now_unix_seconds: u64,
    ) -> Result<GenerationRetentionReason, MaterializationError> {
        if self
            .generations
            .lookup_current(key)?
            .is_some_and(|selection| selection.manifest.generation == generation)
        {
            return Ok(GenerationRetentionReason::CurrentSelection);
        }
        let mut digest = Sha256Digest;
        if root_has_pin_or_source_lease(&self.storage_root, key.root.digest(), &mut digest)
            .map_err(|error| MaterializationError::Coordination(error.to_string()))?
        {
            return Ok(GenerationRetentionReason::PinOrSourceLease);
        }
        if self.generations.active_generation_lease_exists(
            key.id()?,
            generation,
            now_unix_seconds,
        )? {
            return Ok(GenerationRetentionReason::ExactGenerationLease);
        }
        Ok(GenerationRetentionReason::LastVerifiedNativeLocator)
    }
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
        schema: "layerstack-materialization-generation-v1".to_owned(),
        schema_version: 1,
        materialization_id: key.id()?.hex(),
        root_id: digest_string(key.root.digest()),
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FlightKey {
    storage_root: String,
    materialization_id: MaterializationId,
}

#[derive(Debug, Default)]
struct Flight {
    state: Mutex<FlightState>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct FlightState {
    result: Option<Result<MaterializationOutcome, MaterializationError>>,
}

fn flights() -> &'static Mutex<HashMap<FlightKey, Arc<Flight>>> {
    static FLIGHTS: OnceLock<Mutex<HashMap<FlightKey, Arc<Flight>>>> = OnceLock::new();
    FLIGHTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn join_flight(key: &FlightKey) -> Result<(Arc<Flight>, bool), MaterializationError> {
    let mut flights = flights()
        .lock()
        .map_err(|_| MaterializationError::Coordination("flight registry poisoned".to_owned()))?;
    if let Some(flight) = flights.get(key) {
        return Ok((flight.clone(), false));
    }
    let flight = Arc::new(Flight::default());
    flights.insert(key.clone(), flight.clone());
    Ok((flight, true))
}

fn finish_flight(
    key: &FlightKey,
    flight: &Arc<Flight>,
    result: Result<MaterializationOutcome, MaterializationError>,
) {
    {
        let mut state = flight
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.result = Some(result);
    }
    flight.ready.notify_all();
    let mut registry = flights()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if registry
        .get(key)
        .is_some_and(|candidate| Arc::ptr_eq(candidate, flight))
    {
        registry.remove(key);
    }
}

fn wait_for_flight(
    flight: Arc<Flight>,
    request: &MaterializationRequest,
) -> Result<MaterializationOutcome, MaterializationError> {
    let mut state = flight
        .state
        .lock()
        .map_err(|_| MaterializationError::Coordination("flight state poisoned".to_owned()))?;
    loop {
        if let Some(result) = state.result.clone() {
            return result.map(|mut outcome| {
                if outcome.disposition == MaterializationDisposition::Built {
                    outcome.disposition = MaterializationDisposition::Shared;
                }
                outcome
            });
        }
        check_request(request)?;
        let remaining = request.deadline.saturating_duration_since(Instant::now());
        let wait = remaining.min(WAIT_SLICE);
        let (next, _) = flight
            .ready
            .wait_timeout(state, wait)
            .map_err(|_| MaterializationError::Coordination("flight wait poisoned".to_owned()))?;
        state = next;
    }
}

#[derive(Debug)]
struct WorkerGate {
    active: Mutex<usize>,
    ready: Condvar,
}

fn worker_gate() -> &'static WorkerGate {
    static WORKERS: OnceLock<WorkerGate> = OnceLock::new();
    WORKERS.get_or_init(|| WorkerGate {
        active: Mutex::new(0),
        ready: Condvar::new(),
    })
}

struct WorkerPermit;

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        let gate = worker_gate();
        let mut active = gate
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.saturating_sub(1);
        drop(active);
        gate.ready.notify_one();
    }
}

fn acquire_worker(request: &MaterializationRequest) -> Result<WorkerPermit, MaterializationError> {
    let gate = worker_gate();
    let mut active = gate
        .active
        .lock()
        .map_err(|_| MaterializationError::Coordination("worker gate poisoned".to_owned()))?;
    loop {
        if *active < MAX_BUILD_WORKERS {
            *active += 1;
            return Ok(WorkerPermit);
        }
        check_request(request)?;
        let remaining = request.deadline.saturating_duration_since(Instant::now());
        let wait = remaining.min(WAIT_SLICE);
        let (next, _) = gate
            .ready
            .wait_timeout(active, wait)
            .map_err(|_| MaterializationError::Coordination("worker wait poisoned".to_owned()))?;
        active = next;
    }
}
