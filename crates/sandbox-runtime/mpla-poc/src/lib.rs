use std::time::{SystemTime, UNIX_EPOCH};

pub mod activation;
pub mod allocation;
#[cfg(target_os = "linux")]
pub mod atomic_cgroup_process;
pub mod config;
pub mod controls;
pub mod docker_protocol;
pub mod durable;
pub mod error;
pub mod evacuation;
pub mod evidence;
pub mod evidence_schema;
pub mod external_publication;
pub mod fault;
pub mod fixtures;
pub mod id;
pub mod inventory;
pub mod lease;
pub mod locator;
pub mod m1_contract;
pub mod occ;
pub mod overlay_adapter;
pub mod owner;
pub mod prepared_fixture;
pub mod process_tree;
pub mod projection;
pub mod protocol;
pub mod publication;
pub mod publication_qualification;
pub mod qualify;
pub mod quiesce;
pub mod reconcile;
pub mod recovery;
pub mod ref_store;
pub mod report;
pub mod resources;
pub mod semantic;
pub mod session;
pub mod state;
pub mod storage_admin;

pub use activation::{
    inherit_projection_root_metadata, recover_exact_activation, ActivatedSession,
    ActivationBinding, ActivationReceipt, ActivationRecoveryDisposition, ActivationRecoveryReceipt,
    AllocationPhysicalIdentity, ExactActivationRequest,
};
pub use config::PocConfig;
pub use controls::{
    bind_product_catalog, collect_control_changes, run_current_i2_closing,
    run_current_i2_materialization, CatalogBinding, CatalogCoverageReceipt, ControlApiCoverage,
    ControlBoundary, ControlCacheExpectation, ControlCacheMatch, ControlCatalogFacts,
    ControlChangeSet, ControlCollectionLimits, ControlIntent, ControlMaterializationOutcome,
    ControlOperationReceipt, ControlPublicationOutcome, ControlSelectionKey, ControlSourceProfile,
    ControlVerdict, CurrentI2ClosingRequest, CurrentI2MaterializationRequest,
    ExternalReadinessReceipt, MonotonicClock, MonotonicSpan, MonotonicTimer,
    MATCHED_PUBLICATION_START_BOUNDARY, MATCHED_PUBLICATION_STOP_BOUNDARY,
};
pub use error::{PocError, PocResult};
pub use evidence_schema::{
    ArtifactStatus, EnvironmentReceipt, InodeWitness, PhysicalSnapshot, ProbeReceipt, ProbeStatus,
    QualificationReceipt,
};
pub use external_publication::{
    stationary_adopt_prepared, ExternalStationaryPublicationReceipt, ExternalStationarySeal,
};
pub use fault::{
    physical_reach, FaultInjector, FaultPoint, NamedFaultInjector, NamedFaultPoint,
    PhysicalFaultMarker,
};
pub use fixtures::{
    fixture_plan, populate_empty_fixture_root, prepare_fixture, FixtureId, FixturePlan,
    FixtureReceipt, FixtureTier,
};
pub use id::{
    ActivationOperationId, AllocationId, AttributionRootId, LocatorGeneration, OperationId,
    PublicationId, RefSequence, RootId, RunId, SessionId,
};
pub use inventory::{AllocationInventory, InventoryEntry, InventoryEntryKind};
pub use lease::TerminalLeaseFenceWitness;
pub use m1_contract::{
    AttributionInput, CanonicalDurabilityReceipt, CanonicalRootPair, LocatorDurabilityReceipt,
    LocatorRefCandidate, PairedRefValue, SemanticBuildReceipt, SemanticBuildRequest,
    SemanticPhaseSpan,
};
pub use overlay_adapter::{PermanentOverlayMount, UnmountedOverlay};
pub use prepared_fixture::{
    prepared_fixture_manifest_path, prepared_fixture_storage_requirement,
    read_prepared_fixture_manifest, validate_prepared_fixture_cache_layout,
    write_prepared_fixture_manifest, PreparedFixtureBranch, PreparedFixtureControlSource,
    PreparedFixtureLayoutReceipt, PreparedFixtureManifest, PreparedFixtureStorageRequirement,
    PREPARED_FIXTURE_ALLOCATION_COUNT, PREPARED_FIXTURE_BASE_SHA256,
    PREPARED_FIXTURE_BUILDER_HEADROOM_BYTES, PREPARED_FIXTURE_CHAIN_DEPTH,
    PREPARED_FIXTURE_CONTROL_ROOT, PREPARED_FIXTURE_CONTROL_SOURCE,
    PREPARED_FIXTURE_CONTROL_SOURCE_BYTES, PREPARED_FIXTURE_CONTROL_SOURCE_MANIFEST_SHA256,
    PREPARED_FIXTURE_DEPTH_EIGHT_BYTES, PREPARED_FIXTURE_DEPTH_FIVE_BYTES,
    PREPARED_FIXTURE_LARGE_DELTA_SHA256, PREPARED_FIXTURE_MANIFEST,
    PREPARED_FIXTURE_MARKER_LAYER_BYTES, PREPARED_FIXTURE_MINIMUM_AVAILABLE_INODES,
    PREPARED_FIXTURE_PAYLOAD_ROOT, PREPARED_FIXTURE_PROFILE, PREPARED_FIXTURE_ROOT,
    PREPARED_FIXTURE_RUN_ID, PREPARED_FIXTURE_SINGLE_FILE_LAYER_BYTES,
    PREPARED_FIXTURE_SMALL_DELTA_SHA256,
};
pub use process_tree::{CommandReceipt, ManagedProcessTree, ProcessAudit};
pub use projection::{ExactProjectionReceipt, ProjectionRecipe};
pub use protocol::{
    AdoptionReceipt, AllocationDescriptor, AllocationHandle, DeletionCapability, MutableLease,
    OwnerTransitionRequest, QualificationRequest, StableAllocationReceipt, StorageAdminAction,
    StorageAdminAuthorization, StorageAdminDurability, StorageAdminOutcome, StorageAdminReceipt,
    StorageAdminRequest, StorageAdminScope, WriterCapability,
};
pub use publication::{
    PublicationOperationRecord, ReceiptHitPublicationReceipt, StationaryPublicationReceipt,
    StationaryPublicationRequest,
};
pub use quiesce::{
    QuiescenceReceipt, ReceiptHitSealInput, ReceiptSealedAllocation, SealedAllocation,
    SealingRecord,
};
pub use reconcile::{
    LeakCounts, ReconciliationReceipt, StorageCategoryReceipt, StorageCategoryRoot,
};
pub use report::{
    AssertionReceipt, CaseOutcome, CaseReceipt, EvidenceClass, ManifestEntry, ManifestReceipt,
};
pub use resources::{
    AdmissionController, AdmissionGuard, AdmissionReceipt, AdmissionTier, ResourceSnapshot,
};
pub use semantic::SemanticResourceMaxima;
pub use session::{
    prepare_external_session, recover_session_seal, MplaSession, PreparedExternalSession,
    SessionRecord, SessionSealCleanupWitness, SessionSealRecoveryDisposition,
    SessionSealRecoveryReceipt, SessionSealRecoveryRequest,
};
pub use state::{OwnerGeneration, OwnerSubject, PublicationPhase, SessionPhase};

pub const INTERFACE_VERSION: &str = "m2r-iface-v1";
pub const STORAGE_ADMIN_PROFILE_ID: &str = "mpla-storage-admin-v1";
/// Dedicated physical-qualification profile for the one OverlayFS VFS
/// credential contract identified in Stage 04.6. It is not a production
/// default and can only be selected by the loaded daemon configuration.
pub const STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID: &str =
    "mpla-storage-admin-overlayfs-dac-override-qualification-v1";
pub const STORAGE_ADMIN_TRUSTED_EXECUTABLE: &str =
    "/usr/local/libexec/ephemeral-sandbox/mpla-storage-admin-v1";
pub const STORAGE_ADMIN_EFFECTIVE_CAPABILITIES: &[&str] = &["CAP_SYS_ADMIN"];
pub const STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_EFFECTIVE_CAPABILITIES: &[&str] =
    &["CAP_SYS_ADMIN", "CAP_DAC_OVERRIDE"];
pub const STORAGE_ADMIN_PRIVILEGED_SYSCALLS: &[&str] = &["mount", "umount2", "setns", "syncfs"];
pub const SCHEMA_VERSION: u32 = 1;

pub fn unix_time_ms() -> PocResult<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| PocError::Clock(error.to_string()))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| PocError::Clock("system time does not fit in u64 milliseconds".to_owned()))
}
