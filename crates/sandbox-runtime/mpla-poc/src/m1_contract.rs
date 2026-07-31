use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    AllocationId, AttributionRootId, LocatorGeneration, OperationId, PublicationId, RefSequence,
    RootId,
};

pub const SEMANTIC_FORMAT_VERSION: &str = "mpla-poc-semantic-v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CanonicalRootPair {
    pub root_id: RootId,
    pub attribution_root_id: AttributionRootId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AttributionInput {
    pub actor_id: String,
    pub semantic_operation_id: String,
}

/// The physical paths are scanner inputs only. Candidate encoders must reject
/// them (and allocation/locator identity) from every canonical hash input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticBuildRequest {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub allocation_id: AllocationId,
    pub sealed_tree: PathBuf,
    pub spool_dir: PathBuf,
    pub canonical_object_dir: PathBuf,
    pub attribution: AttributionInput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticPhaseSpan {
    pub phase: String,
    pub elapsed_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CanonicalDurabilityReceipt {
    pub root_manifest: PathBuf,
    pub semantic_attribution: AttributionInput,
    pub immutable_object_count: u64,
    pub immutable_object_bytes: u64,
    pub object_set_sha256: String,
    pub files_fsynced: bool,
    pub object_directory_fsynced: bool,
    pub manifest_fsynced: bool,
    pub manifest_directory_fsynced: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticBuildReceipt {
    pub schema_version: u32,
    pub semantic_format: String,
    pub operation_id: OperationId,
    pub roots: CanonicalRootPair,
    pub record_stream_sha256: String,
    pub entry_count: u64,
    pub bytes_read: u64,
    pub spool_runs: u64,
    pub spool_bytes: u64,
    pub peak_open_data_fds: u16,
    pub peak_data_workers: u16,
    pub phase_spans: Vec<SemanticPhaseSpan>,
    pub durability: CanonicalDurabilityReceipt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocatorRefCandidate {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub roots: CanonicalRootPair,
    pub locator_generation: LocatorGeneration,
    pub expected_sequence: RefSequence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairedRefValue {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub roots: CanonicalRootPair,
    pub locator_generation: LocatorGeneration,
    pub sequence: RefSequence,
    pub checksum_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocatorDurabilityReceipt {
    pub generation: LocatorGeneration,
    pub forward_manifest_sha256: String,
    pub reverse_manifest_sha256: String,
    pub generation_manifest_sha256: String,
    pub forward_durable: bool,
    pub reverse_durable: bool,
    pub manifest_durable: bool,
    pub selector_parent_synced: bool,
}
