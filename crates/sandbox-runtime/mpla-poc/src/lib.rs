use std::time::{SystemTime, UNIX_EPOCH};

pub mod activation;
pub mod allocation;
pub mod config;
pub mod controls;
pub mod docker_protocol;
pub mod durable;
pub mod error;
pub mod evacuation;
pub mod evidence;
pub mod evidence_schema;
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
pub mod process_tree;
pub mod projection;
pub mod protocol;
pub mod publication;
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

pub use activation::{
    ActivatedSession, ActivationBinding, ActivationReceipt, ExactActivationRequest,
};
pub use config::PocConfig;
pub use controls::{
    bind_product_catalog, collect_control_changes, run_current_i2_closing,
    run_current_i2_materialization, CatalogBinding, CatalogCoverageReceipt, ControlApiCoverage,
    ControlBoundary, ControlCacheExpectation, ControlCacheMatch, ControlCatalogFacts,
    ControlChangeSet, ControlCollectionLimits, ControlIntent, ControlMaterializationOutcome,
    ControlOperationReceipt, ControlPublicationOutcome, ControlSelectionKey, ControlSourceProfile,
    ControlVerdict, CurrentI2ClosingRequest, CurrentI2MaterializationRequest,
    ExternalReadinessReceipt, MonotonicClock, MonotonicSpan,
};
pub use error::{PocError, PocResult};
pub use evidence_schema::{
    ArtifactStatus, EnvironmentReceipt, InodeWitness, PhysicalSnapshot, ProbeReceipt, ProbeStatus,
    QualificationReceipt,
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
pub use m1_contract::{
    AttributionInput, CanonicalDurabilityReceipt, CanonicalRootPair, LocatorDurabilityReceipt,
    LocatorRefCandidate, PairedRefValue, SemanticBuildReceipt, SemanticBuildRequest,
    SemanticPhaseSpan,
};
pub use overlay_adapter::{PermanentOverlayMount, UnmountedOverlay};
pub use process_tree::{CommandReceipt, ManagedProcessTree, ProcessAudit};
pub use projection::{ExactProjectionReceipt, ProjectionRecipe};
pub use protocol::{
    AdoptionReceipt, AllocationDescriptor, AllocationHandle, DeletionCapability, MutableLease,
    OwnerTransitionRequest, QualificationRequest, StableAllocationReceipt, WriterCapability,
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
pub use session::{MplaSession, SessionRecord};
pub use state::{OwnerGeneration, OwnerSubject, PublicationPhase, SessionPhase};

pub const INTERFACE_VERSION: &str = "m2-iface-v1";
pub const SCHEMA_VERSION: u32 = 1;

pub fn unix_time_ms() -> PocResult<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| PocError::Clock(error.to_string()))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| PocError::Clock("system time does not fit in u64 milliseconds".to_owned()))
}
