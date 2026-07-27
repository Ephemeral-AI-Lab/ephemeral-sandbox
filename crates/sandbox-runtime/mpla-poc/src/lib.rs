use std::time::{SystemTime, UNIX_EPOCH};

pub mod allocation;
pub mod config;
pub mod docker_protocol;
pub mod durable;
pub mod error;
pub mod evidence;
pub mod evidence_schema;
pub mod id;
pub mod lease;
pub mod owner;
pub mod protocol;
pub mod qualify;
pub mod state;

pub use config::PocConfig;
pub use error::{PocError, PocResult};
pub use evidence_schema::{
    ArtifactStatus, EnvironmentReceipt, InodeWitness, PhysicalSnapshot, ProbeReceipt, ProbeStatus,
    QualificationReceipt,
};
pub use id::{ActivationOperationId, AllocationId, OperationId, PublicationId, RunId, SessionId};
pub use protocol::{
    AdoptionReceipt, AllocationDescriptor, AllocationHandle, DeletionCapability, MutableLease,
    OwnerTransitionRequest, QualificationRequest, StableAllocationReceipt, WriterCapability,
};
pub use state::{OwnerGeneration, OwnerSubject, PublicationPhase, SessionPhase};

pub const INTERFACE_VERSION: &str = "m0-iface-v1";
pub const SCHEMA_VERSION: u32 = 1;

pub fn unix_time_ms() -> PocResult<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| PocError::Clock(error.to_string()))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| PocError::Clock("system time does not fit in u64 milliseconds".to_owned()))
}
