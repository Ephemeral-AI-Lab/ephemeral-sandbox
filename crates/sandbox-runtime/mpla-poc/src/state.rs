use serde::{Deserialize, Serialize};

use crate::{OperationId, PublicationId, SessionId};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OwnerSubject {
    WorkspaceOwned {
        session_id: SessionId,
        lease_epoch: u64,
    },
    OwnerTransitionIntent {
        operation_id: OperationId,
        session_id: SessionId,
        expected_owner_epoch: u64,
        publication_id: PublicationId,
    },
    PayloadOwned {
        publication_id: PublicationId,
    },
    RecoveryRequired {
        operation_id: OperationId,
        phase: String,
    },
    TerminalError {
        operation_id: OperationId,
        code: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OwnerGeneration {
    pub schema_version: u32,
    pub allocation_id: crate::AllocationId,
    pub owner_epoch: u64,
    pub previous_owner_epoch: Option<u64>,
    pub subject: OwnerSubject,
    pub operation_id: OperationId,
    pub written_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Open,
    Closing,
    Sealing,
    PublicationCommitted,
    RecoveryRequired,
    RejectedBeforeAdoption,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationPhase {
    Prepared,
    Sealing,
    StableAllocation,
    OwnerIntentDurable,
    PayloadOwned,
    CanonicalDurable,
    LocatorDurable,
    RefCommitted,
    PublicationCommitted,
    RecoveryRequired,
    RejectedBeforeAdoption,
}
