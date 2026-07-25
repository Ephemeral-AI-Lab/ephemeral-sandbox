use std::path::PathBuf;

use crate::LayerRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub manifest_version: i64,
    pub root_hash: String,
    pub layer_paths: Vec<PathBuf>,
}

/// One fully verified native carrier selected by the private candidate route.
///
/// This is deliberately separate from [`Snapshot`]: it never changes public
/// LayerStack authority and cannot be interpreted as a v1 manifest.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateGenerationSelection {
    pub materialization_id: String,
    pub root_id: String,
    pub backend_kind: String,
    pub backend_format_version: u16,
    pub target_profile: String,
    pub generation: u64,
    pub fence: u64,
    pub manifest_sha256: String,
    pub carrier_path: PathBuf,
    pub native_tree_sha256: String,
    pub build_operation_id: String,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateMaterializationDisposition {
    Built,
    Reused,
    Shared,
}

/// Result of an explicit cold materialization request.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateMaterializationResult {
    pub disposition: CandidateMaterializationDisposition,
    pub selection: CandidateGenerationSelection,
    pub maximum_buffer_bytes: Option<u64>,
}

/// Durable lease over one exact materialization generation and fence.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateGenerationLease {
    pub lease_id: String,
    pub materialization_id: String,
    pub generation: u64,
    pub fence: u64,
    pub owner: String,
    pub session_id: String,
    pub acquired_unix_seconds: u64,
    pub renewed_unix_seconds: u64,
    pub expires_unix_seconds: u64,
    pub checksum_sha256: String,
}

/// Atomic strict-admission result: a verified selected carrier plus its
/// already-durable exact generation lease.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateGenerationAdmission {
    pub selection: CandidateGenerationSelection,
    pub lease: CandidateGenerationLease,
}

/// Live lease state of a single active-manifest layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStatus {
    pub layer: LayerRef,
    pub leased_by_workspaces: usize,
}

/// Per-layer breakdown of the active manifest, computed from the live leases.
///
/// `layers` is ordered newest → base; the booked-by relation is a pure function
/// of this order plus `leased_by_workspaces`, so it is derived at render rather
/// than stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackObservation {
    pub manifest_version: i64,
    pub root_hash: String,
    pub active_lease_count: usize,
    pub route: LayerStackRouteSnapshot,
    pub resources: LayerStackResourceSnapshot,
    pub layers: Vec<LayerStatus>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageRolloutMode {
    #[default]
    Legacy,
    Validation,
    StrictCandidate,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageAuthority {
    #[default]
    LegacyV1,
}

/// Cumulative accounting for the private native-candidate route.
///
/// The first four counters identify successful routing progress. The
/// remaining counters authenticate the work classes that are forbidden in a
/// warm command/file/PTY admission. A verifier compares two snapshots around
/// each warm sample and requires every forbidden-work delta to be zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NativeRouteCounters {
    pub lookup_count: u64,
    pub validation_count: u64,
    pub admission_count: u64,
    pub mount_count: u64,
    pub cdc_count: u64,
    pub object_traversal_count: u64,
    pub hash_count: u64,
    pub locator_merge_count: u64,
    pub compaction_count: u64,
    pub pack_count: u64,
    pub gc_count: u64,
    pub squash_count: u64,
    pub materialization_count: u64,
    pub fallback_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LayerStackRouteSnapshot {
    pub schema_version: u16,
    pub observation_epoch: u64,
    pub configured_mode: StorageRolloutMode,
    pub write_authority: StorageAuthority,
    pub read_authority: StorageAuthority,
    pub fallback_count: u64,
    pub fallback_reason_counts: [u64; 0],
    pub mismatch_count: u64,
    pub shadow_comparison_count: u64,
    pub shadow_completed_count: u64,
    pub bytes_scanned: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub bytes_hashed: u64,
    pub bytes_reused: u64,
    pub bytes_newly_retained: u64,
    pub native_route: NativeRouteCounters,
    pub last_quiescence_epoch: u64,
    pub counter_saturated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LayerStackResourceSnapshot {
    pub schema_version: u16,
    pub observation_epoch: u64,
    pub live_owned_bytes: u64,
    pub high_water_owned_bytes: u64,
    pub active_operations: u32,
    pub high_water_active_operations: u32,
    pub active_publications: u32,
    pub high_water_active_publications: u32,
    pub active_buffers: u32,
    pub high_water_active_buffers: u32,
    pub active_tasks: u32,
    pub high_water_active_tasks: u32,
    pub active_workers: u32,
    pub high_water_active_workers: u32,
    pub queued_items: u32,
    pub high_water_queued_items: u32,
    pub queued_bytes: u64,
    pub high_water_queued_bytes: u64,
    pub byte_permits_in_use: u64,
    pub high_water_byte_permits_in_use: u64,
    pub active_leases: u32,
    pub high_water_active_leases: u32,
    pub open_transactions: u32,
    pub high_water_open_transactions: u32,
    pub staging_owners: u32,
    pub high_water_staging_owners: u32,
    pub cache_entries: u32,
    pub high_water_cache_entries: u32,
    pub registry_entries: u32,
    pub high_water_registry_entries: u32,
    pub open_file_descriptors: Option<u32>,
    pub high_water_open_file_descriptors: Option<u32>,
    pub mapped_bytes: Option<u64>,
    pub high_water_mapped_bytes: Option<u64>,
    pub logical_cleanup_complete: bool,
    pub quiescence_ms: Option<u64>,
    pub counter_saturated: bool,
}
