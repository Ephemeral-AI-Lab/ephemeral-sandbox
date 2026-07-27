use std::time::{SystemTime, UNIX_EPOCH};

pub mod activation;
pub mod allocation;
pub mod config;
pub mod docker_protocol;
pub mod durable;
pub mod error;
pub mod evidence;
pub mod evidence_schema;
pub mod fault;
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
pub use error::{PocError, PocResult};
pub use evidence_schema::{
    ArtifactStatus, EnvironmentReceipt, InodeWitness, PhysicalSnapshot, ProbeReceipt, ProbeStatus,
    QualificationReceipt,
};
pub use fault::{FaultInjector, FaultPoint, NamedFaultInjector, NamedFaultPoint};
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
    PublicationOperationRecord, StationaryPublicationReceipt, StationaryPublicationRequest,
};
pub use quiesce::{QuiescenceReceipt, SealedAllocation, SealingRecord};
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

pub const INTERFACE_VERSION: &str = "m1-iface-v1";
pub const SCHEMA_VERSION: u32 = 1;

pub fn unix_time_ms() -> PocResult<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| PocError::Clock(error.to_string()))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| PocError::Clock("system time does not fit in u64 milliseconds".to_owned()))
}
