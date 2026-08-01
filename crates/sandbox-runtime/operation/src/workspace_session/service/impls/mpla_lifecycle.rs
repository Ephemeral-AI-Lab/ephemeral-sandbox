use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::PoisonError;
use std::time::Instant;

use rustix::fs::{flock, FlockOperation};
use sandbox_runtime_mpla_poc::allocation::open_allocation;
use sandbox_runtime_mpla_poc::durable::{
    begin_durability_batch, read_json, replace_json, sync_all,
};
use sandbox_runtime_mpla_poc::inventory::{
    capture_metadata_inventory, capture_stable_metadata_pair, InventoryEntryKind,
};
use sandbox_runtime_mpla_poc::locator::{
    ForwardLocatorEntry, LocatorDelta, LocatorExtent, LocatorStore, PayloadRootId,
    ReverseLocatorEntry, SealedLocatorStore,
};
use sandbox_runtime_mpla_poc::projection::select_exact;
use sandbox_runtime_mpla_poc::publication::StationaryPublicationRequest;
use sandbox_runtime_mpla_poc::ref_store::{
    PairedRefStore, RefCommitOutcome, RefCommitReceipt, ResolvedPairedRef,
};
use sandbox_runtime_mpla_poc::semantic::record::SemanticRecord;
use sandbox_runtime_mpla_poc::semantic::{
    build_incremental, capture_affected_paths_with_maxima, write_affected_stream_from_snapshots,
    AffectedPathSnapshot, IncrementalBuildRequest,
};
use sandbox_runtime_mpla_poc::{
    read_prepared_fixture_manifest, stationary_adopt_prepared,
    validate_prepared_fixture_cache_layout, AllocationId, CanonicalRootPair,
    ExactProjectionReceipt, ExternalStationarySeal, FaultInjector, LocatorRefCandidate,
    MonotonicTimer, NamedFaultInjector, OperationId, PairedRefValue, PocError,
    PreparedFixtureManifest, ProjectionRecipe, PublicationId, RefSequence, RunId,
    SemanticBuildReceipt, SemanticBuildRequest, SemanticResourceMaxima,
    PREPARED_FIXTURE_ALLOCATION_COUNT, PREPARED_FIXTURE_CONTROL_ROOT,
    PREPARED_FIXTURE_PAYLOAD_ROOT, PREPARED_FIXTURE_PROFILE, PREPARED_FIXTURE_RUN_ID,
    SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::workspace_crate::{DestroyWorkspaceRequest, WorkspaceSessionId};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::{
    ActivateMplaWorkspaceSessionResult, AttachMplaPreparedFixtureResult, FinalizationState,
    ForkMplaWorkspaceSessionResult, MplaActivationTimings, MplaLifecycleReceipt, MplaStoragePhase,
    MplaWorkspaceBinding, PublishMplaWorkspaceSessionResult, RollbackMplaWorkspaceSessionResult,
    SquashMplaBranchResult, WorkspaceSessionHandler,
};
use super::super::mpla_policy::publication_attribution;

const LIFECYCLE_IDENTITY_FORMAT: &str = "mpla-runtime-lifecycle-identity-v1";
const ACTIVATION_OUTCOME_FORMAT: &str = "mpla-runtime-activation-outcome-v1";
const PUBLICATION_OUTCOME_FORMAT: &str = "mpla-runtime-publication-outcome-v1";
const PREPARED_FIXTURE_ATTACHMENT_FORMAT: &str = "mpla-runtime-prepared-fixture-attachment-v1";

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LifecycleIdentity {
    schema_version: u32,
    format: String,
    operation_id: String,
    operation_kind: String,
    run_id: String,
    branch: String,
    secondary_branch: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ActivationOutcome {
    schema_version: u32,
    format: String,
    operation_id: String,
    operation_kind: String,
    run_id: String,
    branch: String,
    selected_ref: String,
    workspace_session_id: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PublicationOutcome {
    schema_version: u32,
    format: String,
    operation_id: String,
    run_id: String,
    branch: String,
    workspace_session_id: String,
    selected: PairedRefValue,
    affected_path_count: u64,
    roots: CanonicalRootPair,
    #[serde(default)]
    semantic: Option<SemanticBuildReceipt>,
    #[serde(default)]
    semantic_resource_maxima: Option<SemanticResourceMaxima>,
    #[serde(default)]
    stationary: Option<sandbox_runtime_mpla_poc::ExternalStationaryPublicationReceipt>,
    #[serde(default)]
    affected_payload_bytes_read: u64,
    #[serde(default)]
    affected_input_bytes: u64,
    #[serde(default)]
    semantic_affected_record_count: Option<u64>,
    #[serde(default)]
    prior_node_bytes_read: u64,
    #[serde(default)]
    immutable_payload_bytes_read: u64,
    #[serde(default)]
    semantic_root_record_debug: Option<String>,
    evicted_upperdir_bytes: u64,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PreparedFixtureAttachment {
    schema_version: u32,
    format: String,
    run_id: String,
    fixture_profile: String,
    attached_branches: Vec<String>,
    cached_allocation_ids: Vec<AllocationId>,
}

struct PublicationSemantic {
    receipt: SemanticBuildReceipt,
    resource_maxima: Option<SemanticResourceMaxima>,
    affected_input_bytes: u64,
    affected_record_count: Option<u64>,
    prior_node_bytes_read: u64,
    immutable_payload_bytes_read: u64,
    root_record_debug: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PriorRootManifest {
    record_stream_sha256: String,
}

enum PublicationParent {
    Initial,
    Incremental(Box<IncrementalPublicationParent>),
}

struct IncrementalPublicationParent {
    selected: ResolvedPairedRef,
    recipe: ProjectionRecipe,
}

struct LifecycleOperationLock {
    operation_dir: PathBuf,
    file: File,
}

struct ActivationOperationJournal {
    file: File,
    identity_end: u64,
    outcome: Option<ActivationOutcome>,
}

enum ActivationOutcomeStore<'a> {
    Journal(&'a ActivationOperationJournal),
    Legacy(&'a LifecycleOperationLock),
}

enum ActivationOperationGuard {
    Journal(ActivationOperationJournal),
    Legacy(LifecycleOperationLock),
}

impl ActivationOperationGuard {
    fn outcome_store(&self) -> ActivationOutcomeStore<'_> {
        match self {
            Self::Journal(journal) => ActivationOutcomeStore::Journal(journal),
            Self::Legacy(operation_lock) => ActivationOutcomeStore::Legacy(operation_lock),
        }
    }
}

impl Drop for LifecycleOperationLock {
    fn drop(&mut self) {
        let _ = flock(&self.file, FlockOperation::Unlock);
    }
}

impl Drop for ActivationOperationJournal {
    fn drop(&mut self) {
        let _ = flock(&self.file, FlockOperation::Unlock);
    }
}

impl WorkspaceSessionService {
    pub fn publish_mpla_workspace_session(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        branch: &str,
        sandbox_id: &str,
        operation_id: OperationId,
    ) -> Result<PublishMplaWorkspaceSessionResult, WorkspaceSessionError> {
        let started = Instant::now();
        validate_path_component(branch, "branch").map_err(|reason| {
            WorkspaceSessionError::MplaLifecycle {
                workspace_session_id: workspace_session_id.clone(),
                reason,
            }
        })?;
        let roots = self.mpla_roots()?;
        let operation_lock =
            lock_lifecycle_operation(&roots.control_root, &operation_id).map_err(|reason| {
                mpla_session_error(
                    workspace_session_id,
                    format!("acquire publication operation lock: {reason}"),
                )
            })?;
        self.publication_checkpoint(
            &operation_id,
            workspace_session_id,
            "operation_lock_acquired",
            &started,
        );
        let outcome_path = operation_lock.operation_dir.join("PUBLICATION.json");
        if outcome_path.exists() {
            return replay_publication_outcome(
                &operation_lock,
                workspace_session_id,
                branch,
                &operation_id,
                elapsed_ns(started),
            );
        }

        let gate = self.session_gate(workspace_session_id);
        let matched_publication_timer = MonotonicTimer::start().map_err(|reason| {
            mpla_session_error(
                workspace_session_id,
                format!("start matched publication clock: {reason}"),
            )
        })?;
        let _admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
        let (handler, binding) = {
            let mut sessions = self.lock_sessions()?;
            let session = sessions
                .get_mut(workspace_session_id)
                .ok_or_else(|| WorkspaceSessionError::not_found(workspace_session_id))?;
            if !self.workspace().holder_is_live(&session.handle) {
                return Err(WorkspaceSessionError::HolderExited {
                    workspace_session_id: workspace_session_id.clone(),
                    reason: self
                        .workspace()
                        .holder_exit_reason(&session.handle)
                        .unwrap_or_else(|| "exit-status:unknown".to_owned()),
                    cleanup_state: session.finalization_state,
                });
            }
            if session.finalization_state != FinalizationState::Active {
                return Err(WorkspaceSessionError::not_found(workspace_session_id));
            }
            if !session.active_commands.is_empty() {
                return Err(WorkspaceSessionError::ActiveCommands {
                    workspace_session_id: workspace_session_id.clone(),
                    active_command_session_ids: session.active_commands.iter().cloned().collect(),
                });
            }
            let binding = session.mpla_binding.clone().ok_or_else(|| {
                mpla_session_error(
                    workspace_session_id,
                    "MPLA publication requires a dedicated MPLA session",
                )
            })?;
            if binding.phase != MplaStoragePhase::Mounted {
                return Err(mpla_session_error(
                    workspace_session_id,
                    format!(
                        "MPLA publication requires mounted storage (current phase: {})",
                        binding.phase.as_str()
                    ),
                ));
            }
            let cgroup_procs = session
                .cgroup_path
                .as_ref()
                .map(|path| path.join("cgroup.procs"))
                .ok_or_else(|| {
                    mpla_session_error(
                        workspace_session_id,
                        "MPLA publication requires a workload cgroup",
                    )
                })?;
            require_empty_cgroup(&cgroup_procs).map_err(|reason| {
                mpla_session_error(
                    workspace_session_id,
                    format!("MPLA publication requires an empty workload cgroup: {reason}"),
                )
            })?;
            session.finalization_state = FinalizationState::Finalizing;
            (session.handler(), binding)
        };

        if let Err(error) = ensure_lifecycle_identity(
            &operation_lock,
            &operation_id,
            "publish",
            &binding.run_id,
            branch,
            Some(&workspace_session_id.0),
        ) {
            self.restore_mpla_active(workspace_session_id);
            return Err(error);
        }
        let run_control_root = run_root(&roots.control_root, &binding.run_id);
        let locator_store = match LocatorStore::open(run_control_root.join("locators")) {
            Ok(store) => store,
            Err(error) => {
                self.restore_mpla_active(workspace_session_id);
                return Err(mpla_session_error(
                    workspace_session_id,
                    format!("open MPLA locator store: {error}"),
                ));
            }
        };
        let ref_store = match PairedRefStore::open(run_control_root.join("refs")) {
            Ok(store) => store,
            Err(error) => {
                self.restore_mpla_active(workspace_session_id);
                return Err(mpla_session_error(
                    workspace_session_id,
                    format!("open MPLA ref store: {error}"),
                ));
            }
        };
        let publication_parent = match select_publication_parent(
            &ref_store,
            &locator_store,
            branch,
            &binding,
            &run_control_root,
        ) {
            Ok(parent) => parent,
            Err(error) => {
                self.restore_mpla_active(workspace_session_id);
                return Err(error);
            }
        };
        self.publication_checkpoint(
            &operation_id,
            workspace_session_id,
            "publication_parent_selected",
            &started,
        );
        let affected_paths = match &publication_parent {
            PublicationParent::Initial => Vec::new(),
            PublicationParent::Incremental(_) => {
                self.publication_checkpoint(
                    &operation_id,
                    workspace_session_id,
                    "affected_path_discovery_started",
                    &started,
                );
                match publication_affected_paths(&binding) {
                    Ok(paths) => {
                        self.publication_checkpoint(
                            &operation_id,
                            workspace_session_id,
                            "affected_path_discovery_completed",
                            &started,
                        );
                        paths
                    }
                    Err(reason) => {
                        self.restore_mpla_active(workspace_session_id);
                        return Err(mpla_session_error(workspace_session_id, reason));
                    }
                }
            }
        };
        if matches!(publication_parent, PublicationParent::Incremental(_)) {
            if let Err(reason) = require_paths_absent_from_lowers(&binding, &affected_paths) {
                self.restore_mpla_active(workspace_session_id);
                return Err(mpla_session_error(workspace_session_id, reason));
            }
        }
        self.publication_checkpoint(
            &operation_id,
            workspace_session_id,
            "sealing_started",
            &started,
        );
        let seal_recovery_guard = match binding.prepared.begin_sealing(
            &binding.allocation,
            &binding.lease,
            &operation_id,
            &mut FaultInjector::default(),
        ) {
            Ok(guard) => guard,
            Err(error) => {
                if sandbox_runtime_mpla_poc::quiesce::sealing_record_path(
                    binding.prepared.session_dir(),
                )
                .exists()
                {
                    self.fail_mpla_publication(&binding, workspace_session_id);
                } else {
                    self.restore_mpla_active(workspace_session_id);
                }
                return Err(mpla_session_error(
                    workspace_session_id,
                    format!("persist terminal MPLA Sealing record: {error}"),
                ));
            }
        };
        self.publication_checkpoint(
            &operation_id,
            workspace_session_id,
            "sealing_completed",
            &started,
        );

        // The semantic parent must describe the same merged OverlayFS view a
        // child will activate.  `upper_dir` is only the writable delta: using
        // it for an initial publication silently drops every lower-layer and
        // base entry from the canonical root.  Sealing has closed admission
        // and the workload cgroup was proven empty above, so this mounted view
        // is now a stable, read-only semantic source.  It must be consumed
        // before the storage-admin sequence strictly unmounts it.
        //
        // Keep this only on the initial-publication path. Incremental
        // publication retains its bounded affected-upper scan and parallel
        // semantic/adoption work on the latency-critical hot path.
        let prebuilt_initial_semantic = if matches!(publication_parent, PublicationParent::Initial)
        {
            self.publication_checkpoint(
                &operation_id,
                workspace_session_id,
                "initial_merged_semantic_snapshot_started",
                &started,
            );
            let task_started = Instant::now();
            let full_request = SemanticBuildRequest {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                allocation_id: binding.allocation.descriptor.allocation_id.clone(),
                sealed_tree: binding.prepared.workspace_root().to_path_buf(),
                spool_dir: operation_lock.operation_dir.join("initial-semantic-spool"),
                canonical_object_dir: run_control_root.join("canonical-objects"),
                attribution: publication_attribution(&binding.run_id),
            };
            let result = (|| {
                let selected_profile = self.mpla_storage_admin_profile().map_err(|error| {
                    PocError::Integrity(format!(
                        "select holder-namespace semantic snapshot profile: {error}"
                    ))
                })?;
                let scope = binding.mount_scope.clone().ok_or_else(|| {
                    PocError::Integrity(
                        "initial semantic snapshot requires durable mount authority".to_owned(),
                    )
                })?;
                let storage_request = sandbox_runtime_mpla_poc::StorageAdminRequest {
                    schema_version: SCHEMA_VERSION,
                    interface_version: sandbox_runtime_mpla_poc::INTERFACE_VERSION.to_owned(),
                    profile_id: selected_profile.profile_id().to_owned(),
                    operation_id: operation_id.clone(),
                    action: sandbox_runtime_mpla_poc::StorageAdminAction::Quiesce,
                    scope,
                };
                let invocation = super::mpla_storage_admin::bind_storage_admin_invocation(
                    &operation_id,
                    sandbox_id,
                    &storage_request,
                    &handler,
                    &binding,
                    selected_profile,
                )
                .map_err(PocError::Integrity)?;
                super::mpla_storage_admin::run_fixed_holder_namespace_semantic_snapshot(
                    invocation,
                    full_request,
                )
                .map_err(PocError::Integrity)
            })()
            .map(|snapshot| {
                let affected_input_bytes = snapshot.semantic.bytes_read;
                PublicationSemantic {
                    receipt: snapshot.semantic,
                    resource_maxima: None,
                    affected_input_bytes,
                    affected_record_count: None,
                    prior_node_bytes_read: 0,
                    immutable_payload_bytes_read: 0,
                    root_record_debug: None,
                }
            });
            self.publication_checkpoint(
                &operation_id,
                workspace_session_id,
                "initial_merged_semantic_snapshot_completed",
                &started,
            );
            Some((result, elapsed_ns(task_started)))
        } else {
            None
        };

        let pre_storage_elapsed_ns = elapsed_ns(started);
        let transaction = (|| {
            let storage_started = Instant::now();
            self.publication_checkpoint(
                &operation_id,
                workspace_session_id,
                "storage_sequence_started",
                &started,
            );
            let storage_sequence = self.execute_mpla_publication_storage_sequence_under_gate(
                workspace_session_id,
                sandbox_id,
                [
                    child_operation_id(&operation_id, "quiesce"),
                    child_operation_id(&operation_id, "unmount"),
                    child_operation_id(&operation_id, "cleanup"),
                ],
                || {
                    // The storage sequence proves that its trusted helper is
                    // the only workload-cgroup member before this callback,
                    // then proves the cgroup is empty after its target-only
                    // cleanup completes.  Keeping the observation here lets
                    // that cleanup overlap the independent stable inventory.
                    self.publication_checkpoint(
                        &operation_id,
                        workspace_session_id,
                        "stable_inventory_started",
                        &started,
                    );
                    let (first_inventory, second_inventory) =
                        capture_stable_metadata_pair(&binding.allocation).map_err(|error| {
                            mpla_session_error(
                                workspace_session_id,
                                format!("capture stable MPLA publication inventory: {error}"),
                            )
                        })?;
                    self.publication_checkpoint(
                        &operation_id,
                        workspace_session_id,
                        "stable_inventory_completed",
                        &started,
                    );
                    let initial_entry_count =
                        u64::try_from(first_inventory.entries.len()).unwrap_or(u64::MAX);
                    let (
                        incremental_inputs,
                        affected_payload_bytes_read,
                        semantic_root_record_debug,
                        affected_scan_resource_maxima,
                    ) = match &publication_parent {
                        PublicationParent::Initial => (None, 0, None, None),
                        PublicationParent::Incremental(parent) => {
                            self.publication_checkpoint(
                                &operation_id,
                                workspace_session_id,
                                "affected_snapshot_started",
                                &started,
                            );
                            let selected = &parent.selected;
                            let mut semantic_affected_paths =
                                Vec::with_capacity(affected_paths.len().saturating_add(1));
                            semantic_affected_paths.push(PathBuf::new());
                            semantic_affected_paths.extend(affected_paths.iter().cloned());
                            let (after, selected_scan_maxima) = capture_affected_paths_with_maxima(
                                &binding.allocation.upper_dir,
                                &semantic_affected_paths,
                                &operation_lock.operation_dir.join("selected-path-scan"),
                            )
                            .map_err(|error| {
                                mpla_session_error(
                                    workspace_session_id,
                                    format!("capture incremental MPLA paths: {error}"),
                                )
                            })?;
                            self.publication_checkpoint(
                                &operation_id,
                                workspace_session_id,
                                "affected_snapshot_completed",
                                &started,
                            );
                            let semantic_root_record_debug =
                                after.records.iter().find_map(|record| match record {
                                    SemanticRecord::Node(node) if node.path.is_empty() => {
                                        Some(format!("{node:?}"))
                                    }
                                    _ => None,
                                });
                            let affected_stream =
                                operation_lock.operation_dir.join("affected.delta");
                            let affected_stream_sha256 = if affected_stream.exists() {
                                sha256_file(&affected_stream).map_err(|reason| {
                                    mpla_session_error(
                                        workspace_session_id,
                                        format!("hash replayed affected stream: {reason}"),
                                    )
                                })?
                            } else {
                                write_affected_stream_from_snapshots(
                                    &affected_stream,
                                    &AffectedPathSnapshot {
                                        paths: semantic_affected_paths,
                                        records: Vec::new(),
                                        payload_bytes_read: 0,
                                    },
                                    &after,
                                )
                                .map_err(|error| {
                                    mpla_session_error(
                                        workspace_session_id,
                                        format!("persist affected semantic stream: {error}"),
                                    )
                                })?
                            };
                            let prior_manifest: PriorRootManifest =
                                read_json(&selected.canonical.root_manifest).map_err(|error| {
                                    mpla_session_error(
                                        workspace_session_id,
                                        format!("read prior canonical root manifest: {error}"),
                                    )
                                })?;
                            (
                                Some(IncrementalBuildRequest {
                                    schema_version: SCHEMA_VERSION,
                                    operation_id: operation_id.clone(),
                                    prior_manifest: selected.canonical.root_manifest.clone(),
                                    expected_prior_roots: selected.value.roots.clone(),
                                    expected_prior_record_stream_sha256: prior_manifest
                                        .record_stream_sha256,
                                    affected_stream,
                                    affected_stream_sha256,
                                    affected_ranges_complete: true,
                                    canonical_object_dir: run_control_root
                                        .join("canonical-objects"),
                                    attribution: selected.canonical.semantic_attribution.clone(),
                                }),
                                after.payload_bytes_read,
                                semantic_root_record_debug,
                                Some(selected_scan_maxima),
                            )
                        }
                    };
                    Ok((
                        first_inventory,
                        second_inventory,
                        initial_entry_count,
                        incremental_inputs,
                        affected_payload_bytes_read,
                        semantic_root_record_debug,
                        affected_scan_resource_maxima,
                    ))
                },
            )?;
            drop(seal_recovery_guard);
            self.publication_checkpoint(
                &operation_id,
                workspace_session_id,
                "storage_sequence_completed",
                &started,
            );
            let storage_sequence_elapsed_ns = elapsed_ns(storage_started);
            let quiesce = storage_sequence.quiesce;
            let strict_unmount = storage_sequence.strict_unmount;
            let publication_inputs = storage_sequence.checkpoint;
            let storage_helper_to_unmount_elapsed_ns =
                storage_sequence.helper_to_unmount_elapsed_ns;
            let storage_stable_callback_elapsed_ns = storage_sequence.stable_callback_elapsed_ns;
            let storage_helper_cleanup_elapsed_ns = storage_sequence.helper_cleanup_elapsed_ns;
            let storage_helper_input_encode_elapsed_ns =
                storage_sequence.helper_input_encode_elapsed_ns;
            let storage_helper_launch_elapsed_ns = storage_sequence.helper_launch_elapsed_ns;
            let storage_helper_cgroup_placement_elapsed_ns =
                storage_sequence.helper_cgroup_placement_elapsed_ns;
            let storage_helper_request_write_elapsed_ns =
                storage_sequence.helper_request_write_elapsed_ns;
            let storage_helper_response_wait_elapsed_ns =
                storage_sequence.helper_response_wait_elapsed_ns;
            let storage_helper_unmount_response_decode_elapsed_ns =
                storage_sequence.helper_unmount_response_decode_elapsed_ns;
            let storage_helper_cgroup_release_elapsed_ns =
                storage_sequence.helper_cgroup_release_elapsed_ns;
            let storage_helper_input_decode_elapsed_ns =
                storage_sequence.helper_input_decode_elapsed_ns;
            let storage_helper_validation_elapsed_ns =
                storage_sequence.helper_validation_elapsed_ns;
            let storage_helper_process_preparation_elapsed_ns =
                storage_sequence.helper_process_preparation_elapsed_ns;
            let storage_quiesce_lifecycle_elapsed_ns =
                storage_sequence.quiesce_lifecycle_elapsed_ns;
            let storage_quiesce_operation_elapsed_ns =
                storage_sequence.quiesce_operation_elapsed_ns;
            let storage_strict_unmount_lifecycle_elapsed_ns =
                storage_sequence.strict_unmount_lifecycle_elapsed_ns;
            let storage_strict_unmount_operation_elapsed_ns =
                storage_sequence.strict_unmount_operation_elapsed_ns;
            let (
                first_inventory,
                second_inventory,
                initial_entry_count,
                incremental_inputs,
                affected_payload_bytes_read,
                semantic_root_record_debug,
                affected_scan_resource_maxima,
            ) = publication_inputs;

            // Cleanup completed in the same prepared helper process after
            // strict unmount and before owner adoption. Its target-only work
            // overlapped the independent immutable stable-pair inventory.

            let publication_id =
                PublicationId::from_string(child_identifier(&operation_id, "publication"));
            let request = StationaryPublicationRequest {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                publication_id: publication_id.clone(),
            };
            let external_seal = ExternalStationarySeal {
                quiesce,
                strict_unmount,
                first_inventory,
                second_inventory,
                workload_cgroup_empty: true,
            };
            let semantic_adoption_started = Instant::now();
            let checkpoint_context = self.obs().context();
            let (adoption, semantic, destroyed) = std::thread::scope(|scope| {
                let adoption_task = scope.spawn(|| {
                    let task_started = Instant::now();
                    self.obs().with_context(checkpoint_context.clone(), || {
                        self.publication_checkpoint(
                            &operation_id,
                            workspace_session_id,
                            "stationary_adoption_started",
                            &started,
                        );
                    });
                    let result = stationary_adopt_prepared(
                        &binding.prepared,
                        &binding.allocation,
                        &binding.lease,
                        &request,
                        &run_control_root.join("operations"),
                        external_seal,
                        &mut FaultInjector::default(),
                    );
                    self.obs().with_context(checkpoint_context.clone(), || {
                        self.publication_checkpoint(
                            &operation_id,
                            workspace_session_id,
                            "stationary_adoption_completed",
                            &started,
                        );
                    });
                    (result, elapsed_ns(task_started))
                });
                let semantic_task = incremental_inputs.as_ref().map(|request| {
                    scope.spawn(|| {
                        let task_started = Instant::now();
                        self.obs().with_context(checkpoint_context.clone(), || {
                            self.publication_checkpoint(
                                &operation_id,
                                workspace_session_id,
                                "semantic_build_started",
                                &started,
                            );
                        });
                        let result = build_incremental(request).map(|output| {
                            let resource_maxima = affected_scan_resource_maxima.map_or_else(
                                || output.resource_maxima.clone(),
                                |phase| output.resource_maxima.with_sequential_phase(phase),
                            );
                            PublicationSemantic {
                                receipt: output.receipt,
                                resource_maxima: Some(resource_maxima),
                                affected_input_bytes: output.affected_input_bytes,
                                affected_record_count: Some(output.affected_record_count),
                                prior_node_bytes_read: output.prior_node_bytes_read,
                                immutable_payload_bytes_read: output.immutable_payload_bytes_read,
                                root_record_debug: semantic_root_record_debug.clone(),
                            }
                        });
                        self.obs().with_context(checkpoint_context.clone(), || {
                            self.publication_checkpoint(
                                &operation_id,
                                workspace_session_id,
                                "semantic_build_completed",
                                &started,
                            );
                        });
                        (result, elapsed_ns(task_started))
                    })
                });
                // Durable Sealing makes this session terminal and prevents
                // allocation deletion. Runtime teardown therefore need not
                // serialize behind stationary owner adoption or semantic
                // construction; all three results are still joined before
                // the publication response.
                let destroy_task = scope.spawn(|| {
                    let task_started = Instant::now();
                    self.obs().with_context(checkpoint_context.clone(), || {
                        self.publication_checkpoint(
                            &operation_id,
                            workspace_session_id,
                            "session_destroy_started",
                            &started,
                        );
                    });
                    let result = self.destroy_session_under_gate(
                        handler.clone(),
                        DestroyWorkspaceRequest::default(),
                    );
                    self.obs().with_context(checkpoint_context.clone(), || {
                        self.publication_checkpoint(
                            &operation_id,
                            workspace_session_id,
                            "session_destroy_completed",
                            &started,
                        );
                    });
                    (result, elapsed_ns(task_started))
                });
                let adoption = adoption_task.join().map_err(|_| {
                    mpla_session_error(workspace_session_id, "stationary adoption task panicked")
                })?;
                let semantic = match (semantic_task, prebuilt_initial_semantic) {
                    (Some(task), None) => task.join().map_err(|_| {
                        mpla_session_error(
                            workspace_session_id,
                            "publication semantic task panicked",
                        )
                    })?,
                    (None, Some(semantic)) => semantic,
                    (Some(_), Some(_)) | (None, None) => {
                        return Err(mpla_session_error(
                            workspace_session_id,
                            "MPLA publication selected an invalid semantic construction path",
                        ));
                    }
                };
                let destroyed = destroy_task.join().map_err(|_| {
                    mpla_session_error(workspace_session_id, "terminal session teardown panicked")
                })?;
                Ok::<_, WorkspaceSessionError>((adoption, semantic, destroyed))
            })?;
            self.publication_checkpoint(
                &operation_id,
                workspace_session_id,
                "parallel_tasks_completed",
                &started,
            );
            let semantic_adoption_elapsed_ns = elapsed_ns(semantic_adoption_started);
            let (adoption, stationary_adoption_elapsed_ns) = adoption;
            let (semantic, semantic_build_elapsed_ns) = semantic;
            let (destroyed, session_destroy_elapsed_ns) = destroyed;
            let adoption = adoption.map_err(|error| {
                mpla_session_error(
                    workspace_session_id,
                    format!("adopt stationary MPLA allocation: {error}"),
                )
            })?;
            let semantic = semantic.map_err(|error| {
                mpla_session_error(
                    workspace_session_id,
                    format!("build MPLA publication semantic root: {error}"),
                )
            })?;
            let destroyed = destroyed?;
            let ref_commit_started = Instant::now();
            self.publication_checkpoint(
                &operation_id,
                workspace_session_id,
                "ref_commit_started",
                &started,
            );
            let committed = install_publication_ref(
                &locator_store,
                &ref_store,
                branch,
                &operation_id,
                &publication_id,
                &binding,
                &publication_parent,
                &semantic.receipt,
                adoption.adoption.new_owner.owner_epoch,
                adoption.stable.after.allocated_bytes.max(1),
                &run_control_root,
            )
            .map_err(|reason| mpla_session_error(workspace_session_id, reason))?;
            self.publication_checkpoint(
                &operation_id,
                workspace_session_id,
                "ref_commit_completed",
                &started,
            );
            let ref_commit_elapsed_ns = elapsed_ns(ref_commit_started);
            Ok::<_, WorkspaceSessionError>((
                committed,
                adoption,
                semantic,
                match publication_parent {
                    PublicationParent::Initial => initial_entry_count,
                    PublicationParent::Incremental(_) => {
                        u64::try_from(affected_paths.len()).unwrap_or(u64::MAX)
                    }
                },
                affected_payload_bytes_read,
                storage_sequence_elapsed_ns,
                storage_helper_to_unmount_elapsed_ns,
                storage_stable_callback_elapsed_ns,
                storage_helper_cleanup_elapsed_ns,
                storage_helper_input_encode_elapsed_ns,
                storage_helper_launch_elapsed_ns,
                storage_helper_cgroup_placement_elapsed_ns,
                storage_helper_request_write_elapsed_ns,
                storage_helper_response_wait_elapsed_ns,
                storage_helper_unmount_response_decode_elapsed_ns,
                storage_helper_cgroup_release_elapsed_ns,
                storage_helper_input_decode_elapsed_ns,
                storage_helper_validation_elapsed_ns,
                storage_helper_process_preparation_elapsed_ns,
                storage_quiesce_lifecycle_elapsed_ns,
                storage_quiesce_operation_elapsed_ns,
                storage_strict_unmount_lifecycle_elapsed_ns,
                storage_strict_unmount_operation_elapsed_ns,
                semantic_adoption_elapsed_ns,
                stationary_adoption_elapsed_ns,
                semantic_build_elapsed_ns,
                ref_commit_elapsed_ns,
                destroyed,
                session_destroy_elapsed_ns,
            ))
        })();

        let (
            committed,
            adoption,
            semantic,
            affected_path_count,
            affected_payload_bytes_read,
            storage_sequence_elapsed_ns,
            storage_helper_to_unmount_elapsed_ns,
            storage_stable_callback_elapsed_ns,
            storage_helper_cleanup_elapsed_ns,
            storage_helper_input_encode_elapsed_ns,
            storage_helper_launch_elapsed_ns,
            storage_helper_cgroup_placement_elapsed_ns,
            storage_helper_request_write_elapsed_ns,
            storage_helper_response_wait_elapsed_ns,
            storage_helper_unmount_response_decode_elapsed_ns,
            storage_helper_cgroup_release_elapsed_ns,
            storage_helper_input_decode_elapsed_ns,
            storage_helper_validation_elapsed_ns,
            storage_helper_process_preparation_elapsed_ns,
            storage_quiesce_lifecycle_elapsed_ns,
            storage_quiesce_operation_elapsed_ns,
            storage_strict_unmount_lifecycle_elapsed_ns,
            storage_strict_unmount_operation_elapsed_ns,
            semantic_adoption_elapsed_ns,
            stationary_adoption_elapsed_ns,
            semantic_build_elapsed_ns,
            ref_commit_elapsed_ns,
            destroyed,
            session_destroy_elapsed_ns,
        ) = match transaction {
            Ok(result) => result,
            Err(error) => {
                self.fail_mpla_publication(&binding, workspace_session_id);
                return Err(error);
            }
        };
        if !committed.parent_directory_synced {
            self.fail_mpla_publication(&binding, workspace_session_id);
            return Err(mpla_session_error(
                workspace_session_id,
                "matched publication stopped before the durable ref parent directory was synced",
            ));
        }
        let matched_publication_span = matched_publication_timer.finish().map_err(|reason| {
            mpla_session_error(
                workspace_session_id,
                format!("finish matched publication clock: {reason}"),
            )
        })?;
        let outcome_persist_started = Instant::now();
        self.publication_checkpoint(
            &operation_id,
            workspace_session_id,
            "outcome_persist_started",
            &started,
        );
        let outcome = PublicationOutcome {
            schema_version: SCHEMA_VERSION,
            format: PUBLICATION_OUTCOME_FORMAT.to_owned(),
            operation_id: operation_id.as_str().to_owned(),
            run_id: binding.run_id.as_str().to_owned(),
            branch: branch.to_owned(),
            workspace_session_id: workspace_session_id.0.clone(),
            selected: committed.value.clone(),
            affected_path_count,
            roots: committed.value.roots.clone(),
            semantic: Some(semantic.receipt.clone()),
            semantic_resource_maxima: semantic.resource_maxima.clone(),
            stationary: Some(adoption.clone()),
            affected_payload_bytes_read,
            affected_input_bytes: semantic.affected_input_bytes,
            semantic_affected_record_count: semantic.affected_record_count,
            prior_node_bytes_read: semantic.prior_node_bytes_read,
            immutable_payload_bytes_read: semantic.immutable_payload_bytes_read,
            semantic_root_record_debug: semantic.root_record_debug.clone(),
            evicted_upperdir_bytes: destroyed.evicted_upperdir_bytes,
        };
        replace_json(&outcome_path, &outcome).map_err(|error| {
            mpla_session_error(
                workspace_session_id,
                format!("persist MPLA publication outcome: {error}"),
            )
        })?;
        self.publication_checkpoint(
            &operation_id,
            workspace_session_id,
            "outcome_persisted",
            &started,
        );
        let outcome_persist_elapsed_ns = elapsed_ns(outcome_persist_started);
        let elapsed = elapsed_ns(started);
        Ok(PublishMplaWorkspaceSessionResult {
            workspace_session_id: workspace_session_id.clone(),
            run_id: binding.run_id.as_str().to_owned(),
            branch: branch.to_owned(),
            lifecycle: lifecycle_receipt(
                &operation_id,
                branch,
                &committed.value,
                committed.idempotent_replay,
                elapsed,
            ),
            affected_path_count,
            roots: committed.value.roots,
            semantic: Some(semantic.receipt),
            semantic_resource_maxima: semantic.resource_maxima,
            stationary: Some(adoption),
            affected_payload_bytes_read,
            affected_input_bytes: semantic.affected_input_bytes,
            semantic_affected_record_count: semantic.affected_record_count,
            prior_node_bytes_read: semantic.prior_node_bytes_read,
            immutable_payload_bytes_read: semantic.immutable_payload_bytes_read,
            semantic_root_record_debug: semantic.root_record_debug,
            destroyed: true,
            evicted_upperdir_bytes: destroyed.evicted_upperdir_bytes,
            pre_storage_elapsed_ns,
            storage_sequence_elapsed_ns,
            storage_helper_to_unmount_elapsed_ns,
            storage_stable_callback_elapsed_ns,
            storage_helper_cleanup_elapsed_ns,
            storage_helper_input_encode_elapsed_ns,
            storage_helper_launch_elapsed_ns,
            storage_helper_cgroup_placement_elapsed_ns,
            storage_helper_request_write_elapsed_ns,
            storage_helper_response_wait_elapsed_ns,
            storage_helper_unmount_response_decode_elapsed_ns,
            storage_helper_cgroup_release_elapsed_ns,
            storage_helper_input_decode_elapsed_ns,
            storage_helper_validation_elapsed_ns,
            storage_helper_process_preparation_elapsed_ns,
            storage_quiesce_lifecycle_elapsed_ns,
            storage_quiesce_operation_elapsed_ns,
            storage_strict_unmount_lifecycle_elapsed_ns,
            storage_strict_unmount_operation_elapsed_ns,
            semantic_adoption_elapsed_ns,
            stationary_adoption_elapsed_ns,
            semantic_build_elapsed_ns,
            ref_commit_elapsed_ns,
            session_destroy_elapsed_ns,
            outcome_persist_elapsed_ns,
            matched_publication_span: Some(matched_publication_span),
            service_elapsed_ns: elapsed,
        })
    }

    pub fn activate_mpla_workspace_session(
        &self,
        run_id: RunId,
        branch: &str,
        sandbox_id: &str,
        operation_id: OperationId,
    ) -> Result<ActivateMplaWorkspaceSessionResult, WorkspaceSessionError> {
        let started = Instant::now();
        let roots = self.mpla_roots()?;
        let operation_journal = lock_activation_operation(
            &roots.control_root,
            &operation_id,
            "activate",
            &run_id,
            branch,
            None,
        )
        .map_err(|reason| {
            lifecycle_error(format!("acquire activation operation journal: {reason}"))
        })?;
        if let Some(outcome) = operation_journal.outcome.as_ref() {
            let (handler, selected) = self.replay_activation_outcome(
                outcome,
                "activate",
                &run_id,
                branch,
                &operation_id,
            )?;
            let projection = select_exact(&load_projection_recipe(
                &run_root(&roots.control_root, &run_id),
                &selected,
            )?)
            .map_err(|error| lifecycle_error(format!("select exact MPLA projection: {error}")))?;
            let fresh_allocation_id =
                self.mpla_fresh_allocation_id(&handler.workspace_session_id)?;
            let elapsed = elapsed_ns(started);
            return Ok(ActivateMplaWorkspaceSessionResult {
                workspace_session_id: handler.workspace_session_id,
                fresh_allocation_id,
                run_id: run_id.as_str().to_owned(),
                branch: branch.to_owned(),
                projection,
                lifecycle: lifecycle_receipt(&operation_id, branch, &selected, true, elapsed),
                timings: MplaActivationTimings {
                    admission_elapsed_ns: elapsed,
                    response_elapsed_ns: elapsed,
                    ..MplaActivationTimings::default()
                },
                service_elapsed_ns: elapsed,
            });
        }
        let outcome_store = ActivationOutcomeStore::Journal(&operation_journal);
        let locator_store =
            LocatorStore::open(run_root(&roots.control_root, &run_id).join("locators"))
                .map_err(|error| lifecycle_error(format!("open MPLA locator store: {error}")))?;
        let ref_store =
            PairedRefStore::open(run_root(&roots.control_root, &run_id).join("refs"))
                .map_err(|error| lifecycle_error(format!("open MPLA ref store: {error}")))?;
        let selected = ref_store
            .read_resolved(branch, &locator_store)
            .map_err(|error| lifecycle_error(format!("resolve MPLA branch {branch}: {error}")))?
            .ok_or_else(|| lifecycle_error(format!("MPLA branch {branch} does not exist")))?;
        let admission_elapsed_ns = elapsed_ns(started);
        let (handler, idempotent_replay, projection, mut timings, _storage_admin_scope) = self
            .activate_mpla_under_lock(
                &roots.payload_root,
                &run_root(&roots.control_root, &run_id),
                &outcome_store,
                "activate",
                &run_id,
                branch,
                sandbox_id,
                &operation_id,
                &selected.value,
                &locator_store,
            )?;
        let fresh_allocation_id = self.mpla_fresh_allocation_id(&handler.workspace_session_id)?;
        let elapsed = elapsed_ns(started);
        timings.admission_elapsed_ns = admission_elapsed_ns;
        timings.response_elapsed_ns = elapsed;
        Ok(ActivateMplaWorkspaceSessionResult {
            workspace_session_id: handler.workspace_session_id,
            fresh_allocation_id,
            run_id: run_id.as_str().to_owned(),
            branch: branch.to_owned(),
            projection,
            lifecycle: lifecycle_receipt(
                &operation_id,
                branch,
                &selected.value,
                idempotent_replay,
                elapsed,
            ),
            timings,
            service_elapsed_ns: elapsed,
        })
    }

    pub fn fork_mpla_workspace_session(
        &self,
        run_id: RunId,
        source_branch: &str,
        branch: &str,
        operation_id: OperationId,
    ) -> Result<ForkMplaWorkspaceSessionResult, WorkspaceSessionError> {
        let started = Instant::now();
        let roots = self.mpla_roots()?;
        let operation_lock = lock_lifecycle_operation(&roots.control_root, &operation_id)
            .map_err(|reason| lifecycle_error(format!("acquire fork operation lock: {reason}")))?;
        ensure_lifecycle_identity(
            &operation_lock,
            &operation_id,
            "fork",
            &run_id,
            branch,
            Some(source_branch),
        )?;
        let run_root = run_root(&roots.control_root, &run_id);
        let locator_store = LocatorStore::open(run_root.join("locators"))
            .map_err(|error| lifecycle_error(format!("open MPLA locator store: {error}")))?;
        let ref_store = PairedRefStore::open(run_root.join("refs"))
            .map_err(|error| lifecycle_error(format!("open MPLA ref store: {error}")))?;
        let receipt = if let Some(receipt) = ref_store
            .recover_committed(branch, operation_id.as_str(), &locator_store)
            .map_err(|error| lifecycle_error(format!("recover MPLA fork: {error}")))?
        {
            receipt
        } else {
            let source = ref_store
                .read_resolved(source_branch, &locator_store)
                .map_err(|error| {
                    lifecycle_error(format!(
                        "resolve MPLA source branch {source_branch}: {error}"
                    ))
                })?
                .ok_or_else(|| {
                    lifecycle_error(format!("MPLA source branch {source_branch} does not exist"))
                })?;
            commit_ref(
                &ref_store,
                &locator_store,
                branch,
                &operation_id,
                &source,
                RefSequence::ZERO,
                false,
            )?
        };
        let elapsed = elapsed_ns(started);
        Ok(ForkMplaWorkspaceSessionResult {
            run_id: run_id.as_str().to_owned(),
            source_branch: source_branch.to_owned(),
            branch: branch.to_owned(),
            lifecycle: lifecycle_receipt(
                &operation_id,
                branch,
                &receipt.value,
                receipt.idempotent_replay,
                elapsed,
            ),
            service_elapsed_ns: elapsed,
        })
    }

    /// Attach the closed prepared fixture to a fresh run-local ref and
    /// locator store.  The only bytes written here are metadata.  The cache is
    /// mounted read-only by the normal runtime profile, while activation still
    /// creates its upper and lease under this sandbox's lifecycle roots.
    pub fn attach_mpla_prepared_fixture(
        &self,
        run_id: RunId,
        fixture_profile: &str,
        operation_id: OperationId,
    ) -> Result<AttachMplaPreparedFixtureResult, WorkspaceSessionError> {
        let started = Instant::now();
        if fixture_profile != PREPARED_FIXTURE_PROFILE {
            return Err(lifecycle_error("unsupported prepared fixture profile"));
        }
        let manifest = read_prepared_fixture_manifest().map_err(|error| {
            lifecycle_error(format!("validate prepared fixture cache: {error}"))
        })?;
        let layout = validate_prepared_fixture_cache_layout(&manifest).map_err(|error| {
            lifecycle_error(format!("validate prepared fixture sparse layout: {error}"))
        })?;
        if layout.allocation_count != PREPARED_FIXTURE_ALLOCATION_COUNT
            || layout.payload_bytes_read != 0
        {
            return Err(lifecycle_error(
                "prepared fixture sparse layout receipt is invalid",
            ));
        }
        let expected_cached_allocation_ids = prepared_fixture_allocation_ids(&manifest)?;
        let roots = self.mpla_roots()?;
        let operation_lock =
            lock_lifecycle_operation(&roots.control_root, &operation_id).map_err(|reason| {
                lifecycle_error(format!("acquire prepared-fixture attach lock: {reason}"))
            })?;
        ensure_lifecycle_identity(
            &operation_lock,
            &operation_id,
            "attach_prepared_fixture",
            &run_id,
            fixture_profile,
            None,
        )?;
        let local_run_root = run_root(&roots.control_root, &run_id);
        let attachment_path = local_run_root.join("PREPARED-FIXTURE-ATTACHMENT.json");
        if attachment_path.exists() {
            let attachment: PreparedFixtureAttachment =
                read_json(&attachment_path).map_err(|error| {
                    lifecycle_error(format!("read prepared-fixture attachment: {error}"))
                })?;
            validate_prepared_fixture_attachment(
                &attachment,
                &run_id,
                fixture_profile,
                &expected_cached_allocation_ids,
            )?;
            return Ok(AttachMplaPreparedFixtureResult {
                run_id: run_id.as_str().to_owned(),
                fixture_profile: fixture_profile.to_owned(),
                attached_branches: attachment.attached_branches,
                cached_allocation_count: u64::try_from(attachment.cached_allocation_ids.len())
                    .map_err(|_| lifecycle_error("prepared fixture allocation count overflow"))?,
                payload_bytes_copied: 0,
                service_elapsed_ns: elapsed_ns(started),
            });
        }

        let cache_run_root = Path::new(PREPARED_FIXTURE_CONTROL_ROOT)
            .join("runs")
            .join(PREPARED_FIXTURE_RUN_ID);
        require_prepared_fixture_cache_layout(&cache_run_root)?;
        let cache_locator_store = SealedLocatorStore::open(cache_run_root.join("locators"))
            .map_err(|error| {
                lifecycle_error(format!("open prepared fixture locator store: {error}"))
            })?;
        let cache_ref_store = sandbox_runtime_mpla_poc::ref_store::SealedPairedRefStore::open(
            cache_run_root.join("refs"),
        )
        .map_err(|error| lifecycle_error(format!("open prepared fixture ref store: {error}")))?;

        std::fs::create_dir_all(&local_run_root).map_err(|error| {
            lifecycle_error(format!("create local prepared-fixture run root: {error}"))
        })?;
        let locator_store =
            LocatorStore::open(local_run_root.join("locators")).map_err(|error| {
                lifecycle_error(format!(
                    "open local prepared-fixture locator store: {error}"
                ))
            })?;
        if locator_store
            .selected()
            .map_err(|error| {
                lifecycle_error(format!("read local prepared-fixture locator: {error}"))
            })?
            .is_some()
        {
            return Err(lifecycle_error(
                "prepared fixture attach requires a fresh run-local locator store",
            ));
        }
        let ref_store = PairedRefStore::open(local_run_root.join("refs")).map_err(|error| {
            lifecycle_error(format!("open local prepared-fixture ref store: {error}"))
        })?;
        let publication_id =
            PublicationId::from_string(child_identifier(&operation_id, "prepared-fixture"));
        let mut forward = Vec::with_capacity(manifest.branches.len());
        let mut reverse_by_allocation =
            std::collections::BTreeMap::<AllocationId, ReverseLocatorEntry>::new();
        let mut cached_allocation_ids = BTreeSet::new();
        let projections_root = local_run_root.join("projections");
        std::fs::create_dir_all(&projections_root).map_err(|error| {
            lifecycle_error(format!("create prepared-fixture projections: {error}"))
        })?;

        for branch in &manifest.branches {
            let cache_ref = cache_ref_store
                .read_resolved(&branch.branch, &cache_locator_store)
                .map_err(|error| {
                    lifecycle_error(format!(
                        "resolve prepared fixture branch {}: {error}",
                        branch.branch
                    ))
                })?
                .ok_or_else(|| {
                    lifecycle_error(format!(
                        "prepared fixture branch {} is absent",
                        branch.branch
                    ))
                })?;
            if cache_ref.value.roots != branch.roots || cache_ref.canonical != branch.canonical {
                return Err(lifecycle_error(
                    "prepared fixture manifest differs from its durable branch receipt",
                ));
            }
            let payload_root =
                PayloadRootId::parse(branch.roots.root_id.as_str()).map_err(|error| {
                    lifecycle_error(format!("parse prepared fixture payload root: {error}"))
                })?;
            let source = cache_locator_store
                .resolve(&payload_root)
                .map_err(|error| {
                    lifecycle_error(format!("resolve prepared fixture locator: {error}"))
                })?
                .ok_or_else(|| lifecycle_error("prepared fixture root has no cache allocation"))?;
            if !branch
                .projection
                .lower_allocation_ids_newest_first()
                .contains(&&source.allocation_id)
            {
                return Err(lifecycle_error(
                    "prepared fixture root locator is absent from its exact projection",
                ));
            }
            let accounted_bytes = source.extents.iter().try_fold(0_u64, |total, extent| {
                total
                    .checked_add(extent.length)
                    .ok_or_else(|| lifecycle_error("prepared fixture locator accounting overflow"))
            })?;
            if accounted_bytes == 0 {
                return Err(lifecycle_error(
                    "prepared fixture locator has no accounted bytes",
                ));
            }
            let reverse = reverse_by_allocation
                .entry(source.allocation_id.clone())
                .or_insert_with(|| ReverseLocatorEntry {
                    allocation_id: source.allocation_id.clone(),
                    owner_epoch: source.owner_epoch,
                    operation_id: operation_id.clone(),
                    publication_id: publication_id.clone(),
                    payload_roots: Vec::new(),
                    accounted_bytes,
                });
            if reverse.owner_epoch != source.owner_epoch
                || reverse.accounted_bytes != accounted_bytes
            {
                return Err(lifecycle_error(
                    "prepared fixture reuses an allocation with conflicting locator ownership",
                ));
            }
            reverse.payload_roots.push(payload_root);
            forward.push(source);
            cached_allocation_ids.extend(
                branch
                    .projection
                    .lower_allocation_ids_newest_first()
                    .into_iter()
                    .cloned(),
            );
            replace_json(
                &projections_root.join(format!("{}.json", branch.roots.root_id.as_str())),
                &branch.projection,
            )
            .map_err(|error| {
                lifecycle_error(format!("persist prepared fixture projection: {error}"))
            })?;
        }

        let locator = locator_store
            .install(
                &LocatorDelta {
                    schema_version: SCHEMA_VERSION,
                    operation_id: operation_id.clone(),
                    publication_id,
                    expected_parent: None,
                    forward,
                    reverse: reverse_by_allocation.into_values().collect(),
                },
                &mut NamedFaultInjector::default(),
            )
            .map_err(|error| {
                lifecycle_error(format!("install prepared-fixture locator: {error}"))
            })?;
        for (index, branch) in manifest.branches.iter().enumerate() {
            let branch_operation = child_operation_id(&operation_id, &format!("prepared-{index}"));
            let candidate = LocatorRefCandidate {
                schema_version: SCHEMA_VERSION,
                operation_id: branch_operation,
                publication_id: PublicationId::from_string(child_identifier(
                    &operation_id,
                    &format!("prepared-{index}-publication"),
                )),
                roots: branch.roots.clone(),
                locator_generation: locator.generation,
                expected_sequence: RefSequence::ZERO,
            };
            match ref_store
                .commit(
                    &branch.branch,
                    &candidate,
                    &branch.canonical,
                    &locator,
                    &locator_store,
                    &mut NamedFaultInjector::default(),
                )
                .map_err(|error| {
                    lifecycle_error(format!(
                        "commit prepared fixture branch {}: {error}",
                        branch.branch
                    ))
                })? {
                RefCommitOutcome::Committed(_) => {}
                RefCommitOutcome::ExpectedParent { expected, observed } => {
                    return Err(lifecycle_error(format!(
                        "prepared fixture branch {} expected sequence {expected}, observed {observed}",
                        branch.branch
                    )));
                }
            }
        }
        if cached_allocation_ids != expected_cached_allocation_ids {
            return Err(lifecycle_error(
                "prepared fixture resolved allocations differ from its sealed exact projection",
            ));
        }
        let attachment = PreparedFixtureAttachment {
            schema_version: SCHEMA_VERSION,
            format: PREPARED_FIXTURE_ATTACHMENT_FORMAT.to_owned(),
            run_id: run_id.as_str().to_owned(),
            fixture_profile: fixture_profile.to_owned(),
            attached_branches: manifest
                .branches
                .iter()
                .map(|branch| branch.branch.clone())
                .collect(),
            cached_allocation_ids: cached_allocation_ids.into_iter().collect(),
        };
        replace_json(&attachment_path, &attachment).map_err(|error| {
            lifecycle_error(format!("persist prepared-fixture attachment: {error}"))
        })?;
        Ok(AttachMplaPreparedFixtureResult {
            run_id: run_id.as_str().to_owned(),
            fixture_profile: fixture_profile.to_owned(),
            attached_branches: attachment.attached_branches,
            cached_allocation_count: u64::try_from(attachment.cached_allocation_ids.len())
                .map_err(|_| lifecycle_error("prepared fixture allocation count overflow"))?,
            payload_bytes_copied: 0,
            service_elapsed_ns: elapsed_ns(started),
        })
    }

    pub fn rollback_mpla_workspace_session(
        &self,
        run_id: RunId,
        branch: &str,
        target_branch: &str,
        sandbox_id: &str,
        operation_id: OperationId,
    ) -> Result<RollbackMplaWorkspaceSessionResult, WorkspaceSessionError> {
        let roots = self.mpla_roots()?;
        let operation_guard =
            if legacy_lifecycle_operation_exists(&roots.control_root, &operation_id).map_err(
                |reason| lifecycle_error(format!("inspect legacy rollback operation: {reason}")),
            )? {
                let operation_lock = lock_lifecycle_operation(&roots.control_root, &operation_id)
                    .map_err(|reason| {
                    lifecycle_error(format!("acquire legacy rollback operation lock: {reason}"))
                })?;
                ensure_lifecycle_identity(
                    &operation_lock,
                    &operation_id,
                    "rollback",
                    &run_id,
                    branch,
                    Some(target_branch),
                )?;
                ActivationOperationGuard::Legacy(operation_lock)
            } else {
                ActivationOperationGuard::Journal(
                    lock_activation_operation(
                        &roots.control_root,
                        &operation_id,
                        "rollback",
                        &run_id,
                        branch,
                        Some(target_branch),
                    )
                    .map_err(|reason| {
                        lifecycle_error(format!("acquire rollback activation journal: {reason}"))
                    })?,
                )
            };
        // The public caller owns the outer rollback-to-ready clock.  This
        // service subspan is deliberately the logical ref-selection service
        // only; activation remains inside the caller's outer interval.
        let selector_started = Instant::now();
        let run_root = run_root(&roots.control_root, &run_id);
        let locator_store = LocatorStore::open(run_root.join("locators"))
            .map_err(|error| lifecycle_error(format!("open MPLA locator store: {error}")))?;
        let ref_store = PairedRefStore::open(run_root.join("refs"))
            .map_err(|error| lifecycle_error(format!("open MPLA ref store: {error}")))?;
        let selector_receipt = ref_store
            .rollback_to_branch(
                branch,
                target_branch,
                &operation_id,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .map_err(|error| lifecycle_error(format!("commit MPLA rollback: {error}")))?;
        let selector_elapsed = elapsed_ns(selector_started);
        let outcome_store = operation_guard.outcome_store();
        let (handler, activation_replay, projection, timings, storage_admin_scope) = self
            .activate_mpla_under_lock(
                &roots.payload_root,
                &run_root,
                &outcome_store,
                "rollback",
                &run_id,
                branch,
                sandbox_id,
                &operation_id,
                &selector_receipt.value,
                &locator_store,
            )?;
        let fresh_allocation_id = self.mpla_fresh_allocation_id(&handler.workspace_session_id)?;
        Ok(RollbackMplaWorkspaceSessionResult {
            workspace_session_id: handler.workspace_session_id,
            fresh_allocation_id,
            run_id: run_id.as_str().to_owned(),
            branch: branch.to_owned(),
            target_branch: target_branch.to_owned(),
            projection,
            timings,
            lifecycle: lifecycle_receipt(
                &operation_id,
                branch,
                &selector_receipt.value,
                selector_receipt.idempotent_replay || activation_replay,
                selector_elapsed,
            ),
            service_elapsed_ns: selector_elapsed,
            storage_admin_scope,
        })
    }

    pub fn squash_mpla_branch(
        &self,
        run_id: RunId,
        branch: &str,
        operation_id: OperationId,
    ) -> Result<SquashMplaBranchResult, WorkspaceSessionError> {
        let roots = self.mpla_roots()?;
        let operation_lock =
            lock_lifecycle_operation(&roots.control_root, &operation_id).map_err(|reason| {
                lifecycle_error(format!("acquire squash operation lock: {reason}"))
            })?;
        ensure_lifecycle_identity(
            &operation_lock,
            &operation_id,
            "squash",
            &run_id,
            branch,
            None,
        )?;
        // Logical squash is a metadata-selector service.  The public caller
        // separately records its full request/response interval.
        let selector_started = Instant::now();
        let run_root = run_root(&roots.control_root, &run_id);
        let locator_store = LocatorStore::open(run_root.join("locators"))
            .map_err(|error| lifecycle_error(format!("open MPLA locator store: {error}")))?;
        let ref_store = PairedRefStore::open(run_root.join("refs"))
            .map_err(|error| lifecycle_error(format!("open MPLA ref store: {error}")))?;
        let receipt = ref_store
            .squash_branch(
                branch,
                &operation_id,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .map_err(|error| lifecycle_error(format!("commit MPLA squash: {error}")))?;
        let selector_elapsed = elapsed_ns(selector_started);
        Ok(SquashMplaBranchResult {
            run_id: run_id.as_str().to_owned(),
            branch: branch.to_owned(),
            roots: receipt.value.roots.clone(),
            ref_sequence: receipt.value.sequence.get(),
            lifecycle: lifecycle_receipt(
                &operation_id,
                branch,
                &receipt.value,
                receipt.idempotent_replay,
                selector_elapsed,
            ),
            service_elapsed_ns: selector_elapsed,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn activate_mpla_under_lock(
        &self,
        payload_root: &Path,
        run_control_root: &Path,
        outcome_store: &ActivationOutcomeStore<'_>,
        operation_kind: &str,
        run_id: &RunId,
        branch: &str,
        sandbox_id: &str,
        operation_id: &OperationId,
        selected: &PairedRefValue,
        locator_store: &LocatorStore,
    ) -> Result<
        (
            WorkspaceSessionHandler,
            bool,
            ExactProjectionReceipt,
            MplaActivationTimings,
            sandbox_runtime_mpla_poc::StorageAdminScope,
        ),
        WorkspaceSessionError,
    > {
        let activation_started = Instant::now();
        let selected_ref = selected_ref(branch, selected);
        if let Some(outcome) = outcome_store.read()? {
            let expected = ActivationOutcome {
                schema_version: SCHEMA_VERSION,
                format: ACTIVATION_OUTCOME_FORMAT.to_owned(),
                operation_id: operation_id.as_str().to_owned(),
                operation_kind: operation_kind.to_owned(),
                run_id: run_id.as_str().to_owned(),
                branch: branch.to_owned(),
                selected_ref: selected_ref.clone(),
                workspace_session_id: outcome.workspace_session_id.clone(),
            };
            if outcome != expected {
                return Err(lifecycle_error(
                    "stable operation ID was reused for another activation",
                ));
            }
            let workspace_session_id = WorkspaceSessionId(outcome.workspace_session_id);
            let handler =
                self.require_live_activated_session(&workspace_session_id, run_id, selected)?;
            let projection = select_exact(&load_projection_recipe(run_control_root, selected)?)
                .map_err(|error| {
                    lifecycle_error(format!("select exact MPLA projection: {error}"))
                })?;
            let storage_admin_scope =
                self.mpla_storage_scope(&handler.workspace_session_id, sandbox_id)?;
            return Ok((
                handler,
                true,
                projection,
                MplaActivationTimings {
                    projection_elapsed_ns: elapsed_ns(activation_started),
                    ..MplaActivationTimings::default()
                },
                storage_admin_scope,
            ));
        }

        if let Some((handler, phase)) =
            self.find_mpla_session_by_operation(operation_id, run_id, selected)?
        {
            let mount_started = Instant::now();
            let storage_admin_scope = if phase == MplaStoragePhase::Prepared {
                self.mount_mpla_workspace_session(
                    &handler.workspace_session_id,
                    sandbox_id,
                    operation_id,
                )?
                .scope
            } else if phase != MplaStoragePhase::Mounted {
                return Err(lifecycle_error(format!(
                    "activation recovery found MPLA session in phase {}",
                    phase.as_str()
                )));
            } else {
                self.mpla_storage_scope(&handler.workspace_session_id, sandbox_id)?
            };
            let storage_mount_elapsed_ns = elapsed_ns(mount_started);
            let outcome_started = Instant::now();
            outcome_store.persist(activation_outcome(
                operation_kind,
                run_id,
                branch,
                operation_id,
                &selected_ref,
                &handler.workspace_session_id,
            ))?;
            let outcome_persist_elapsed_ns = elapsed_ns(outcome_started);
            let projection = select_exact(&load_projection_recipe(run_control_root, selected)?)
                .map_err(|error| {
                    lifecycle_error(format!("select exact MPLA projection: {error}"))
                })?;
            return Ok((
                handler,
                true,
                projection,
                MplaActivationTimings {
                    projection_elapsed_ns: elapsed_ns(activation_started)
                        .saturating_sub(storage_mount_elapsed_ns)
                        .saturating_sub(outcome_persist_elapsed_ns),
                    storage_mount_elapsed_ns,
                    outcome_persist_elapsed_ns,
                    ..MplaActivationTimings::default()
                },
                storage_admin_scope,
            ));
        }

        let projection_started = Instant::now();
        let recipe = load_projection_recipe(run_control_root, selected)?;
        let projection = select_exact(&recipe)
            .map_err(|error| lifecycle_error(format!("select exact MPLA projection: {error}")))?;
        let payload_root_id = PayloadRootId::parse(selected.roots.root_id.as_str())
            .map_err(|error| lifecycle_error(format!("parse selected payload root: {error}")))?;
        let locator = locator_store
            .resolve(&payload_root_id)
            .map_err(|error| lifecycle_error(format!("resolve selected payload root: {error}")))?
            .ok_or_else(|| {
                lifecycle_error(format!(
                    "selected payload root {} has no current allocation",
                    payload_root_id.as_str()
                ))
            })?;
        if !projection
            .lower_allocation_ids_newest_first
            .contains(&locator.allocation_id)
        {
            return Err(lifecycle_error(
                "current payload locator is absent from the exact projection recipe",
            ));
        }
        let prepared_fixture_allocations =
            prepared_fixture_cache_allocations(run_control_root, run_id)?;
        let lower_dirs_newest_first = projection
            .lower_allocation_ids_newest_first
            .iter()
            .map(|allocation_id| {
                let allocation_root = if prepared_fixture_allocations.contains(allocation_id) {
                    Path::new(PREPARED_FIXTURE_PAYLOAD_ROOT).join("allocations")
                } else {
                    payload_root.join("allocations")
                };
                open_allocation(&allocation_root, allocation_id)
                    .map(|allocation| allocation.upper_dir)
                    .map_err(|error| {
                        lifecycle_error(format!(
                            "open exact MPLA projection allocation {allocation_id}: {error}"
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let projection_elapsed_ns = elapsed_ns(projection_started);
        let session_create_started = Instant::now();
        let (handler, session_timings) = self.create_mpla_workspace_session_with_projection(
            run_id.clone(),
            operation_id.clone(),
            Some((selected.clone(), lower_dirs_newest_first)),
            false,
        )?;
        let session_create_elapsed_ns = elapsed_ns(session_create_started);
        let storage_mount_started = Instant::now();
        let storage_admin_scope = self
            .mount_mpla_workspace_session(&handler.workspace_session_id, sandbox_id, operation_id)?
            .scope;
        let storage_mount_elapsed_ns = elapsed_ns(storage_mount_started);
        let outcome_persist_started = Instant::now();
        outcome_store.persist(activation_outcome(
            operation_kind,
            run_id,
            branch,
            operation_id,
            &selected_ref,
            &handler.workspace_session_id,
        ))?;
        let outcome_persist_elapsed_ns = elapsed_ns(outcome_persist_started);
        Ok((
            handler,
            false,
            projection,
            MplaActivationTimings {
                projection_elapsed_ns,
                session_create_elapsed_ns,
                session_identity_elapsed_ns: session_timings.session_identity_elapsed_ns,
                allocation_create_elapsed_ns: session_timings.allocation_create_elapsed_ns,
                allocation_lease_elapsed_ns: session_timings.allocation_lease_elapsed_ns,
                projection_metadata_elapsed_ns: session_timings.projection_metadata_elapsed_ns,
                external_session_prepare_elapsed_ns: session_timings
                    .external_session_prepare_elapsed_ns,
                durability_commit_elapsed_ns: session_timings.durability_commit_elapsed_ns,
                workspace_create_elapsed_ns: session_timings.workspace_create_elapsed_ns,
                launch_material_elapsed_ns: session_timings.launch_material_elapsed_ns,
                cgroup_prepare_elapsed_ns: session_timings.cgroup_prepare_elapsed_ns,
                session_register_elapsed_ns: session_timings.session_register_elapsed_ns,
                session_commit_elapsed_ns: session_timings.session_commit_elapsed_ns,
                storage_mount_elapsed_ns,
                outcome_persist_elapsed_ns,
                ..MplaActivationTimings::default()
            },
            storage_admin_scope,
        ))
    }

    fn mpla_fresh_allocation_id(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<AllocationId, WorkspaceSessionError> {
        let sessions = self.lock_sessions()?;
        let session = sessions.get(workspace_session_id).ok_or_else(|| {
            lifecycle_error("activated MPLA session disappeared before response construction")
        })?;
        let binding = session
            .mpla_binding
            .as_ref()
            .ok_or_else(|| lifecycle_error("activated workspace session lacks an MPLA binding"))?;
        Ok(binding.allocation.descriptor.allocation_id.clone())
    }

    fn replay_activation_outcome(
        &self,
        outcome: &ActivationOutcome,
        operation_kind: &str,
        run_id: &RunId,
        branch: &str,
        operation_id: &OperationId,
    ) -> Result<(WorkspaceSessionHandler, PairedRefValue), WorkspaceSessionError> {
        if outcome.schema_version != SCHEMA_VERSION
            || outcome.format != ACTIVATION_OUTCOME_FORMAT
            || outcome.operation_id != operation_id.as_str()
            || outcome.operation_kind != operation_kind
            || outcome.run_id != run_id.as_str()
            || outcome.branch != branch
        {
            return Err(lifecycle_error(
                "stable operation ID was reused for another activation",
            ));
        }
        let workspace_session_id = WorkspaceSessionId(outcome.workspace_session_id.clone());
        let sessions = self.lock_sessions()?;
        let session = sessions.get(&workspace_session_id).ok_or_else(|| {
            lifecycle_error(
                "durable activation outcome has no live runtime session; recovery is required",
            )
        })?;
        let binding = session.mpla_binding.as_ref().ok_or_else(|| {
            lifecycle_error("durable activation outcome points to a non-MPLA session")
        })?;
        let selected = binding.selected_ref.as_ref().ok_or_else(|| {
            lifecycle_error("durable activation outcome has no selected MPLA ref")
        })?;
        if binding.run_id != *run_id
            || binding.phase != MplaStoragePhase::Mounted
            || !self.workspace().holder_is_live(&session.handle)
            || outcome.selected_ref != selected_ref(branch, selected)
        {
            return Err(lifecycle_error(
                "durable activation outcome does not match a ready live MPLA session",
            ));
        }
        Ok((session.handler(), selected.clone()))
    }

    fn find_mpla_session_by_operation(
        &self,
        operation_id: &OperationId,
        run_id: &RunId,
        selected: &PairedRefValue,
    ) -> Result<Option<(WorkspaceSessionHandler, MplaStoragePhase)>, WorkspaceSessionError> {
        let sessions = self.lock_sessions()?;
        for session in sessions.values() {
            let Some(binding) = session.mpla_binding.as_ref() else {
                continue;
            };
            if binding.lease_operation_id != *operation_id {
                continue;
            }
            if binding.run_id != *run_id || binding.selected_ref.as_ref() != Some(selected) {
                return Err(lifecycle_error(
                    "stable operation ID is already bound to another MPLA session",
                ));
            }
            if !self.workspace().holder_is_live(&session.handle) {
                return Err(lifecycle_error(
                    "activation recovery requires a live MPLA namespace holder",
                ));
            }
            return Ok(Some((session.handler(), binding.phase)));
        }
        Ok(None)
    }

    fn require_live_activated_session(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        run_id: &RunId,
        selected: &PairedRefValue,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        let sessions = self.lock_sessions()?;
        let session = sessions.get(workspace_session_id).ok_or_else(|| {
            lifecycle_error(
                "durable activation outcome has no live runtime session; recovery is required",
            )
        })?;
        let binding = session.mpla_binding.as_ref().ok_or_else(|| {
            lifecycle_error("durable activation outcome points to a non-MPLA session")
        })?;
        if binding.run_id != *run_id
            || binding.selected_ref.as_ref() != Some(selected)
            || binding.phase != MplaStoragePhase::Mounted
            || !self.workspace().holder_is_live(&session.handle)
        {
            return Err(lifecycle_error(
                "durable activation outcome does not match a ready live MPLA session",
            ));
        }
        Ok(session.handler())
    }

    fn restore_mpla_active(&self, workspace_session_id: &WorkspaceSessionId) {
        if let Ok(mut sessions) = self.lock_sessions() {
            if let Some(session) = sessions.get_mut(workspace_session_id) {
                if session.finalization_state == FinalizationState::Finalizing
                    && session
                        .mpla_binding
                        .as_ref()
                        .is_some_and(|binding| binding.phase == MplaStoragePhase::Mounted)
                {
                    session.finalization_state = FinalizationState::Active;
                }
            }
        }
    }

    fn mark_mpla_finalize_failed(&self, workspace_session_id: &WorkspaceSessionId) {
        if let Ok(mut sessions) = self.lock_sessions() {
            if let Some(session) = sessions.get_mut(workspace_session_id) {
                session.finalization_state = FinalizationState::FinalizeFailed;
                session.holder_cleanup_terminal = true;
            }
        }
    }

    fn fail_mpla_publication(
        &self,
        binding: &MplaWorkspaceBinding,
        workspace_session_id: &WorkspaceSessionId,
    ) {
        let _ = binding
            .prepared
            .mark_recovery_required(&binding.allocation, &binding.lease);
        self.mark_mpla_finalize_failed(workspace_session_id);
    }

    fn publication_checkpoint(
        &self,
        operation_id: &OperationId,
        workspace_session_id: &WorkspaceSessionId,
        phase: &'static str,
        started: &Instant,
    ) {
        self.obs().event(
            "mpla_publication.checkpoint",
            json!({
                "schema_version": 1,
                "operation_id": operation_id.as_str(),
                "workspace_session_id": workspace_session_id.0.as_str(),
                "phase": phase,
                "elapsed_ns": u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            }),
        );
    }

    fn mpla_roots(&self) -> Result<super::super::core::MplaLifecycleRoots, WorkspaceSessionError> {
        self.mpla_lifecycle_roots
            .clone()
            .ok_or_else(|| lifecycle_error("MPLA lifecycle roots are not configured"))
    }
}

fn replay_publication_outcome(
    operation_lock: &LifecycleOperationLock,
    workspace_session_id: &WorkspaceSessionId,
    branch: &str,
    operation_id: &OperationId,
    service_elapsed_ns: u64,
) -> Result<PublishMplaWorkspaceSessionResult, WorkspaceSessionError> {
    let outcome: PublicationOutcome =
        read_json(&operation_lock.operation_dir.join("PUBLICATION.json")).map_err(|error| {
            mpla_session_error(
                workspace_session_id,
                format!("read MPLA publication outcome: {error}"),
            )
        })?;
    if outcome.schema_version != SCHEMA_VERSION
        || outcome.format != PUBLICATION_OUTCOME_FORMAT
        || outcome.operation_id != operation_id.as_str()
        || outcome.branch != branch
        || outcome.workspace_session_id != workspace_session_id.0
        || outcome.roots != outcome.selected.roots
    {
        return Err(mpla_session_error(
            workspace_session_id,
            "stable operation ID was reused for another MPLA publication",
        ));
    }
    let run_id = RunId::parse(outcome.run_id.clone()).map_err(|error| {
        mpla_session_error(
            workspace_session_id,
            format!("parse replayed MPLA run ID: {error}"),
        )
    })?;
    ensure_lifecycle_identity(
        operation_lock,
        operation_id,
        "publish",
        &run_id,
        branch,
        Some(&workspace_session_id.0),
    )?;
    Ok(PublishMplaWorkspaceSessionResult {
        workspace_session_id: workspace_session_id.clone(),
        run_id: outcome.run_id,
        branch: outcome.branch,
        lifecycle: lifecycle_receipt(
            operation_id,
            branch,
            &outcome.selected,
            true,
            service_elapsed_ns,
        ),
        affected_path_count: outcome.affected_path_count,
        roots: outcome.roots,
        semantic: outcome.semantic,
        semantic_resource_maxima: outcome.semantic_resource_maxima,
        stationary: outcome.stationary,
        affected_payload_bytes_read: outcome.affected_payload_bytes_read,
        affected_input_bytes: outcome.affected_input_bytes,
        semantic_affected_record_count: outcome.semantic_affected_record_count,
        prior_node_bytes_read: outcome.prior_node_bytes_read,
        immutable_payload_bytes_read: outcome.immutable_payload_bytes_read,
        semantic_root_record_debug: outcome.semantic_root_record_debug,
        destroyed: true,
        evicted_upperdir_bytes: outcome.evicted_upperdir_bytes,
        pre_storage_elapsed_ns: 0,
        storage_sequence_elapsed_ns: 0,
        storage_helper_to_unmount_elapsed_ns: 0,
        storage_stable_callback_elapsed_ns: 0,
        storage_helper_cleanup_elapsed_ns: 0,
        storage_helper_input_encode_elapsed_ns: 0,
        storage_helper_launch_elapsed_ns: 0,
        storage_helper_cgroup_placement_elapsed_ns: 0,
        storage_helper_request_write_elapsed_ns: 0,
        storage_helper_response_wait_elapsed_ns: 0,
        storage_helper_unmount_response_decode_elapsed_ns: 0,
        storage_helper_cgroup_release_elapsed_ns: 0,
        storage_helper_input_decode_elapsed_ns: 0,
        storage_helper_validation_elapsed_ns: 0,
        storage_helper_process_preparation_elapsed_ns: 0,
        storage_quiesce_lifecycle_elapsed_ns: 0,
        storage_quiesce_operation_elapsed_ns: 0,
        storage_strict_unmount_lifecycle_elapsed_ns: 0,
        storage_strict_unmount_operation_elapsed_ns: 0,
        semantic_adoption_elapsed_ns: 0,
        stationary_adoption_elapsed_ns: 0,
        semantic_build_elapsed_ns: 0,
        ref_commit_elapsed_ns: 0,
        session_destroy_elapsed_ns: 0,
        outcome_persist_elapsed_ns: 0,
        matched_publication_span: None,
        service_elapsed_ns,
    })
}

fn select_publication_parent(
    ref_store: &PairedRefStore,
    locator_store: &LocatorStore,
    branch: &str,
    binding: &MplaWorkspaceBinding,
    run_control_root: &Path,
) -> Result<PublicationParent, WorkspaceSessionError> {
    if binding.selected_ref.is_none() {
        let selected = ref_store
            .read_resolved(branch, locator_store)
            .map_err(|error| {
                lifecycle_error(format!(
                    "resolve initial MPLA publication branch {branch}: {error}"
                ))
            })?;
        if selected.is_some() {
            return Err(lifecycle_error(format!(
                "MPLA branch {branch} already exists; initial publication requires a fresh branch"
            )));
        }
        if locator_store
            .selected()
            .map_err(|error| lifecycle_error(format!("read initial MPLA locator: {error}")))?
            .is_some()
        {
            return Err(lifecycle_error(
                "initial MPLA publication requires a fresh run locator",
            ));
        }
        return Ok(PublicationParent::Initial);
    }

    let selected = require_selected_publication_parent(ref_store, locator_store, branch, binding)
        .map_err(lifecycle_error)?;
    let recipe = load_projection_recipe(run_control_root, &selected.value)?;
    Ok(PublicationParent::Incremental(Box::new(
        IncrementalPublicationParent { selected, recipe },
    )))
}

fn require_selected_publication_parent(
    ref_store: &PairedRefStore,
    locator_store: &LocatorStore,
    branch: &str,
    binding: &MplaWorkspaceBinding,
) -> Result<ResolvedPairedRef, String> {
    let activated = binding
        .selected_ref
        .as_ref()
        .ok_or_else(|| "MPLA publication requires an activated branch ref".to_owned())?;
    let selected = ref_store
        .read_resolved(branch, locator_store)
        .map_err(|error| format!("resolve MPLA publication branch {branch}: {error}"))?
        .ok_or_else(|| format!("MPLA branch {branch} does not exist"))?;
    if selected.value != *activated {
        return Err(
            "MPLA branch advanced after activation; publication requires exact selected parent"
                .to_owned(),
        );
    }
    Ok(selected)
}

fn publication_affected_paths(binding: &MplaWorkspaceBinding) -> Result<Vec<PathBuf>, String> {
    let inventory = capture_metadata_inventory(&binding.allocation)
        .map_err(|error| format!("capture MPLA publication inventory: {error}"))?;
    if inventory.entries.is_empty() || inventory.entries.len() > 64 {
        return Err(format!(
            "incremental MPLA publication requires 1..=64 upper entries, observed {}",
            inventory.entries.len()
        ));
    }
    let mut paths = Vec::with_capacity(inventory.entries.len());
    for entry in inventory.entries {
        if entry.kind != InventoryEntryKind::Regular
            || entry.link_count != 1
            || entry.relative_path.components().count() != 1
            || !entry
                .relative_path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(format!(
                "incremental publication accepts only new single-link regular files at the workspace root; rejected {}",
                entry.relative_path.display()
            ));
        }
        paths.push(entry.relative_path);
    }
    paths.sort_by(|left, right| {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            left.as_os_str()
                .as_bytes()
                .cmp(right.as_os_str().as_bytes())
        }
        #[cfg(not(unix))]
        {
            left.cmp(right)
        }
    });
    Ok(paths)
}

fn require_paths_absent_from_lowers(
    binding: &MplaWorkspaceBinding,
    affected_paths: &[PathBuf],
) -> Result<(), String> {
    for lower in &binding.lower_dirs_newest_first {
        for relative_path in affected_paths {
            match std::fs::symlink_metadata(lower.join(relative_path)) {
                Ok(_) => {
                    return Err(format!(
                        "incremental publication path {} already exists in a selected lower",
                        relative_path.display()
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "inspect selected lower path {}: {error}",
                        relative_path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn require_empty_cgroup(cgroup_procs: &Path) -> Result<(), String> {
    let members = std::fs::read_to_string(cgroup_procs)
        .map_err(|error| format!("read {}: {error}", cgroup_procs.display()))?;
    if members.lines().any(|line| !line.trim().is_empty()) {
        return Err(format!("{} is populated", cgroup_procs.display()));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn install_publication_ref(
    locator_store: &LocatorStore,
    ref_store: &PairedRefStore,
    branch: &str,
    operation_id: &OperationId,
    publication_id: &PublicationId,
    binding: &MplaWorkspaceBinding,
    publication_parent: &PublicationParent,
    semantic: &SemanticBuildReceipt,
    owner_epoch: u64,
    accounted_bytes: u64,
    run_control_root: &Path,
) -> Result<RefCommitReceipt, String> {
    let (expected_sequence, recipe) = match publication_parent {
        PublicationParent::Initial => (
            RefSequence::ZERO,
            ProjectionRecipe {
                schema_version: SCHEMA_VERSION,
                roots: semantic.roots.clone(),
                base_allocation_id: binding.allocation.descriptor.allocation_id.clone(),
                net_delta_carrier_id: None,
                recent_delta_ids: Vec::new(),
            },
        ),
        PublicationParent::Incremental(parent) => {
            let selected = &parent.selected;
            let recipe = &parent.recipe;
            let mut recent_delta_ids =
                Vec::with_capacity(recipe.recent_delta_ids.len().saturating_add(1));
            recent_delta_ids.push(binding.allocation.descriptor.allocation_id.clone());
            recent_delta_ids.extend(recipe.recent_delta_ids.iter().cloned());
            (
                selected.value.sequence,
                ProjectionRecipe {
                    schema_version: SCHEMA_VERSION,
                    roots: semantic.roots.clone(),
                    base_allocation_id: recipe.base_allocation_id.clone(),
                    net_delta_carrier_id: recipe.net_delta_carrier_id.clone(),
                    recent_delta_ids,
                },
            )
        }
    };
    let payload_root = PayloadRootId::parse(semantic.roots.root_id.as_str())
        .map_err(|error| format!("parse published payload root: {error}"))?;
    recipe
        .validate()
        .map_err(|error| format!("validate published MPLA projection recipe: {error}"))?;
    let projections_root = run_control_root.join("projections");
    std::fs::create_dir_all(&projections_root)
        .map_err(|error| format!("create MPLA projection directory: {error}"))?;
    let recipe_path = projections_root.join(format!("{}.json", semantic.roots.root_id.as_str()));

    // The projection recipe is derived entirely from the candidate semantic
    // roots and allocation binding. It is independent of the locator's CAS
    // installation, but the ref journal must remain last: no durable ref may
    // select either artifact until both have completed. Overlap these two
    // preconditions so their independent file/parent syncs do not serialize.
    // A failed locator may leave an unreferenced recipe, and a failed recipe
    // may leave an unreferenced locator; both are recoverable orphan artifacts
    // and neither is externally selectable without the final journal append.
    let (locator, recipe_result) = std::thread::scope(|scope| {
        let recipe_task = scope.spawn(|| replace_json(&recipe_path, &recipe));
        let locator = (|| -> Result<_, String> {
            let mut locator = None;
            for attempt in 0..64_u8 {
                let expected_parent = match publication_parent {
                    PublicationParent::Initial => None,
                    PublicationParent::Incremental(_) => Some(
                        locator_store
                            .selected()
                            .map_err(|error| format!("read current MPLA locator: {error}"))?
                            .ok_or_else(|| {
                                "incremental MPLA publication requires a selected locator"
                                    .to_owned()
                            })?
                            .receipt
                            .generation,
                    ),
                };
                let result = locator_store.install(
                    &LocatorDelta {
                        schema_version: SCHEMA_VERSION,
                        operation_id: operation_id.clone(),
                        publication_id: publication_id.clone(),
                        expected_parent,
                        forward: vec![ForwardLocatorEntry {
                            payload_root: payload_root.clone(),
                            allocation_id: binding.allocation.descriptor.allocation_id.clone(),
                            owner_epoch,
                            extents: vec![LocatorExtent {
                                relative_path: "upper".to_owned(),
                                offset: 0,
                                length: accounted_bytes,
                            }],
                        }],
                        reverse: vec![ReverseLocatorEntry {
                            allocation_id: binding.allocation.descriptor.allocation_id.clone(),
                            owner_epoch,
                            operation_id: operation_id.clone(),
                            publication_id: publication_id.clone(),
                            payload_roots: vec![payload_root.clone()],
                            accounted_bytes,
                        }],
                    },
                    &mut NamedFaultInjector::default(),
                );
                match result {
                    Ok(receipt) => {
                        locator = Some(receipt);
                        break;
                    }
                    Err(PocError::OwnerConflict(message))
                        if attempt < 63 && message.starts_with("locator expected parent ") =>
                    {
                        continue;
                    }
                    Err(error) => {
                        return Err(format!("install MPLA publication locator: {error}"));
                    }
                }
            }
            locator.ok_or_else(|| {
                "install MPLA publication locator: locator compare-and-install retry bound exhausted"
                    .to_owned()
            })
        })();
        let recipe_result = recipe_task
            .join()
            .map_err(|_| "persist published MPLA projection recipe task panicked".to_owned())
            .and_then(|result| {
                result.map_err(|error| format!("persist published MPLA projection recipe: {error}"))
            });
        (locator, recipe_result)
    });
    let locator = locator?;
    recipe_result?;

    let outcome = ref_store
        .commit(
            branch,
            &LocatorRefCandidate {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                publication_id: publication_id.clone(),
                roots: semantic.roots.clone(),
                locator_generation: locator.generation,
                expected_sequence,
            },
            &semantic.durability,
            &locator,
            locator_store,
            &mut NamedFaultInjector::default(),
        )
        .map_err(|error| format!("commit MPLA publication ref: {error}"))?;
    match outcome {
        RefCommitOutcome::Committed(receipt) => Ok(receipt),
        RefCommitOutcome::ExpectedParent { expected, observed } => Err(format!(
            "MPLA publication expected branch sequence {expected}, observed {observed}"
        )),
    }
}

fn child_operation_id(operation_id: &OperationId, label: &str) -> OperationId {
    OperationId::from_string(child_identifier(operation_id, label))
}

fn child_identifier(operation_id: &OperationId, label: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mpla-runtime-publication-child-v1\0");
    digest.update(operation_id.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(label.as_bytes());
    format!("mpla-{label}-{:x}", digest.finalize())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn mpla_session_error(
    workspace_session_id: &WorkspaceSessionId,
    reason: impl Into<String>,
) -> WorkspaceSessionError {
    WorkspaceSessionError::MplaLifecycle {
        workspace_session_id: workspace_session_id.clone(),
        reason: reason.into(),
    }
}

fn commit_ref(
    ref_store: &PairedRefStore,
    locator_store: &LocatorStore,
    branch: &str,
    operation_id: &OperationId,
    source: &ResolvedPairedRef,
    expected_sequence: RefSequence,
    rollback: bool,
) -> Result<RefCommitReceipt, WorkspaceSessionError> {
    let selected_locator = locator_store
        .selected()
        .map_err(|error| lifecycle_error(format!("read current MPLA locator: {error}")))?
        .ok_or_else(|| lifecycle_error("MPLA locator has no selected generation"))?;
    let payload_root = PayloadRootId::parse(source.value.roots.root_id.as_str())
        .map_err(|error| lifecycle_error(format!("parse selected payload root: {error}")))?;
    if !selected_locator
        .forward
        .iter()
        .any(|entry| entry.payload_root == payload_root)
    {
        return Err(lifecycle_error(format!(
            "selected payload root {} is absent from the current locator",
            payload_root.as_str()
        )));
    }
    let candidate = LocatorRefCandidate {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: source.value.publication_id.clone(),
        roots: source.value.roots.clone(),
        locator_generation: selected_locator.receipt.generation,
        expected_sequence,
    };
    let mut faults = NamedFaultInjector::default();
    let outcome = if rollback {
        ref_store.commit_rollback(
            branch,
            &candidate,
            &source.canonical,
            &selected_locator.receipt,
            locator_store,
            &mut faults,
        )
    } else {
        ref_store.commit(
            branch,
            &candidate,
            &source.canonical,
            &selected_locator.receipt,
            locator_store,
            &mut faults,
        )
    }
    .map_err(|error| lifecycle_error(format!("commit MPLA branch {branch}: {error}")))?;
    match outcome {
        RefCommitOutcome::Committed(receipt) => Ok(receipt),
        RefCommitOutcome::ExpectedParent { expected, observed } => Err(lifecycle_error(format!(
            "MPLA branch {branch} expected sequence {expected}, observed {observed}"
        ))),
    }
}

fn lock_lifecycle_operation(
    control_root: &Path,
    operation_id: &OperationId,
) -> Result<LifecycleOperationLock, String> {
    validate_path_component(operation_id.as_str(), "operation ID")?;
    let operations_root = control_root.join("runtime-lifecycle").join("operations");
    std::fs::create_dir_all(&operations_root)
        .map_err(|error| format!("create lifecycle operations root: {error}"))?;
    let operation_dir = operations_root.join(operation_id.as_str());
    std::fs::create_dir_all(&operation_dir)
        .map_err(|error| format!("create lifecycle operation directory: {error}"))?;
    File::open(&operations_root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync lifecycle operations root: {error}"))?;
    let lock_path = operation_dir.join("LOCK");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("open lifecycle operation lock: {error}"))?;
    flock(&file, FlockOperation::LockExclusive)
        .map_err(|error| format!("lock lifecycle operation: {error}"))?;
    Ok(LifecycleOperationLock {
        operation_dir,
        file,
    })
}

fn legacy_lifecycle_operation_exists(
    control_root: &Path,
    operation_id: &OperationId,
) -> Result<bool, String> {
    validate_path_component(operation_id.as_str(), "operation ID")?;
    let operation_dir = control_root
        .join("runtime-lifecycle")
        .join("operations")
        .join(operation_id.as_str());
    match std::fs::symlink_metadata(&operation_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err("legacy lifecycle operation path is not a real directory".to_owned())
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("stat legacy lifecycle operation path: {error}")),
    }
}

fn lock_activation_operation(
    control_root: &Path,
    operation_id: &OperationId,
    operation_kind: &str,
    run_id: &RunId,
    branch: &str,
    secondary_branch: Option<&str>,
) -> Result<ActivationOperationJournal, String> {
    const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;

    validate_path_component(operation_id.as_str(), "operation ID")?;
    validate_path_component(branch, "branch")?;
    if let Some(secondary_branch) = secondary_branch {
        validate_path_component(secondary_branch, "secondary branch")?;
    }
    let expected = LifecycleIdentity {
        schema_version: SCHEMA_VERSION,
        format: LIFECYCLE_IDENTITY_FORMAT.to_owned(),
        operation_id: operation_id.as_str().to_owned(),
        operation_kind: operation_kind.to_owned(),
        run_id: run_id.as_str().to_owned(),
        branch: branch.to_owned(),
        secondary_branch: secondary_branch.map(str::to_owned),
    };
    let identity_durability = begin_durability_batch();
    let lifecycle_root = control_root.join("runtime-lifecycle");
    ensure_durable_directory(control_root, &lifecycle_root, "lifecycle root")?;
    let journal_root = lifecycle_root.join("activation-operations");
    ensure_durable_directory(&lifecycle_root, &journal_root, "activation operations root")?;
    let journal_path = journal_root.join(format!("{}.journal", operation_id.as_str()));
    let file = match OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&journal_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&journal_path)
                .map_err(|error| format!("stat activation operation journal: {error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("activation operation journal is not a regular file".to_owned());
            }
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&journal_path)
                .map_err(|error| format!("open activation operation journal: {error}"))?
        }
        Err(error) => return Err(format!("create activation operation journal: {error}")),
    };
    if !file
        .metadata()
        .map_err(|error| format!("stat opened activation operation journal: {error}"))?
        .is_file()
    {
        return Err("opened activation operation journal is not a regular file".to_owned());
    }
    flock(&file, FlockOperation::LockExclusive)
        .map_err(|error| format!("lock activation operation journal: {error}"))?;

    let length = file
        .metadata()
        .map_err(|error| format!("stat activation operation journal: {error}"))?
        .len();
    if length > MAX_JOURNAL_BYTES {
        return Err(format!(
            "activation operation journal exceeds {MAX_JOURNAL_BYTES} bytes"
        ));
    }
    if length == 0 {
        let mut bytes = serde_json::to_vec(&expected)
            .map_err(|error| format!("encode activation operation identity: {error}"))?;
        bytes.push(b'\n');
        let mut writer = file
            .try_clone()
            .map_err(|error| format!("clone activation operation journal: {error}"))?;
        writer
            .seek(SeekFrom::Start(0))
            .and_then(|_| writer.write_all(&bytes))
            .and_then(|_| writer.set_len(u64::try_from(bytes.len()).unwrap_or(u64::MAX)))
            .and_then(|_| sync_all(&writer))
            .map_err(|error| format!("persist activation operation identity: {error}"))?;
        File::open(&journal_root)
            .and_then(|directory| sync_all(&directory))
            .map_err(|error| format!("sync activation operations root: {error}"))?;
        identity_durability
            .commit(&[control_root])
            .map_err(|error| format!("commit activation operation identity: {error}"))?;
        return Ok(ActivationOperationJournal {
            file,
            identity_end: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            outcome: None,
        });
    }

    drop(identity_durability);
    let mut reader = file
        .try_clone()
        .map_err(|error| format!("clone activation operation journal: {error}"))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("seek activation operation journal: {error}"))?;
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read activation operation journal: {error}"))?;
    let identity_end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .ok_or_else(|| "activation operation journal has a torn identity".to_owned())?;
    let observed: LifecycleIdentity = serde_json::from_slice(&bytes[..identity_end - 1])
        .map_err(|error| format!("decode activation operation identity: {error}"))?;
    if observed != expected {
        return Err("stable operation ID was reused for another MPLA lifecycle request".to_owned());
    }
    let identity_end_u64 = u64::try_from(identity_end).unwrap_or(u64::MAX);
    let tail = &bytes[identity_end..];
    if tail.is_empty() {
        return Ok(ActivationOperationJournal {
            file,
            identity_end: identity_end_u64,
            outcome: None,
        });
    }
    if !tail.ends_with(b"\n") {
        file.set_len(identity_end_u64)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("recover torn activation operation outcome: {error}"))?;
        return Ok(ActivationOperationJournal {
            file,
            identity_end: identity_end_u64,
            outcome: None,
        });
    }
    let outcome: ActivationOutcome = serde_json::from_slice(&tail[..tail.len() - 1])
        .map_err(|error| format!("decode activation operation outcome: {error}"))?;
    if outcome.schema_version != SCHEMA_VERSION
        || outcome.format != ACTIVATION_OUTCOME_FORMAT
        || outcome.operation_id != operation_id.as_str()
        || outcome.operation_kind != operation_kind
        || outcome.run_id != run_id.as_str()
        || outcome.branch != branch
    {
        return Err("activation operation outcome contradicts its identity".to_owned());
    }
    Ok(ActivationOperationJournal {
        file,
        identity_end: identity_end_u64,
        outcome: Some(outcome),
    })
}

fn ensure_durable_directory(parent: &Path, path: &Path, label: &str) -> Result<(), String> {
    match std::fs::create_dir(path) {
        Ok(()) => File::open(parent)
            .and_then(|directory| sync_all(&directory))
            .map_err(|error| format!("sync parent of {label}: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|error| format!("stat {label}: {error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                Err(format!("{label} is not a real directory"))
            } else {
                Ok(())
            }
        }
        Err(error) => Err(format!("create {label}: {error}")),
    }
}

impl ActivationOutcomeStore<'_> {
    fn read(&self) -> Result<Option<ActivationOutcome>, WorkspaceSessionError> {
        match self {
            Self::Journal(journal) => Ok(journal.outcome.clone()),
            Self::Legacy(operation_lock) => {
                let path = operation_lock.operation_dir.join("ACTIVATION.json");
                if !path.exists() {
                    return Ok(None);
                }
                read_json(&path)
                    .map(Some)
                    .map_err(|error| lifecycle_error(format!("read activation outcome: {error}")))
            }
        }
    }

    fn persist(&self, outcome: ActivationOutcome) -> Result<(), WorkspaceSessionError> {
        match self {
            Self::Journal(journal) => journal.persist(outcome),
            Self::Legacy(operation_lock) => replace_json(
                &operation_lock.operation_dir.join("ACTIVATION.json"),
                &outcome,
            )
            .map_err(|error| lifecycle_error(format!("persist activation outcome: {error}"))),
        }
    }
}

impl ActivationOperationJournal {
    fn persist(&self, outcome: ActivationOutcome) -> Result<(), WorkspaceSessionError> {
        if let Some(observed) = self.outcome.as_ref() {
            if observed == &outcome {
                return Ok(());
            }
            return Err(lifecycle_error(
                "stable operation ID was reused for another activation outcome",
            ));
        }
        let mut bytes = serde_json::to_vec(&outcome)
            .map_err(|error| lifecycle_error(format!("encode activation outcome: {error}")))?;
        bytes.push(b'\n');
        let mut writer = self.file.try_clone().map_err(|error| {
            lifecycle_error(format!("clone activation operation journal: {error}"))
        })?;
        writer
            .seek(SeekFrom::Start(self.identity_end))
            .and_then(|_| writer.write_all(&bytes))
            .and_then(|_| {
                writer.set_len(
                    self.identity_end
                        .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
                )
            })
            // This tail is a same-daemon response-loss replay cache, not the
            // crash-safety authority. The operation identity and its
            // operation-bound allocation/session graph were committed before
            // holder publication. After daemon loss the namespace holder is
            // gone, so recovery fails closed instead of replaying this cache.
            .map_err(|error| {
                lifecycle_error(format!("persist activation operation outcome: {error}"))
            })
    }
}

fn ensure_lifecycle_identity(
    operation_lock: &LifecycleOperationLock,
    operation_id: &OperationId,
    operation_kind: &str,
    run_id: &RunId,
    branch: &str,
    secondary_branch: Option<&str>,
) -> Result<(), WorkspaceSessionError> {
    validate_path_component(branch, "branch").map_err(lifecycle_error)?;
    if let Some(secondary_branch) = secondary_branch {
        validate_path_component(secondary_branch, "secondary branch").map_err(lifecycle_error)?;
    }
    let expected = LifecycleIdentity {
        schema_version: SCHEMA_VERSION,
        format: LIFECYCLE_IDENTITY_FORMAT.to_owned(),
        operation_id: operation_id.as_str().to_owned(),
        operation_kind: operation_kind.to_owned(),
        run_id: run_id.as_str().to_owned(),
        branch: branch.to_owned(),
        secondary_branch: secondary_branch.map(str::to_owned),
    };
    let identity_path = operation_lock.operation_dir.join("IDENTITY.json");
    if identity_path.exists() {
        let observed: LifecycleIdentity = read_json(&identity_path)
            .map_err(|error| lifecycle_error(format!("read lifecycle identity: {error}")))?;
        if observed != expected {
            return Err(lifecycle_error(
                "stable operation ID was reused for another MPLA lifecycle request",
            ));
        }
        return Ok(());
    }
    replace_json(&identity_path, &expected)
        .map_err(|error| lifecycle_error(format!("persist lifecycle identity: {error}")))
}

fn activation_outcome(
    operation_kind: &str,
    run_id: &RunId,
    branch: &str,
    operation_id: &OperationId,
    selected_ref: &str,
    workspace_session_id: &WorkspaceSessionId,
) -> ActivationOutcome {
    ActivationOutcome {
        schema_version: SCHEMA_VERSION,
        format: ACTIVATION_OUTCOME_FORMAT.to_owned(),
        operation_id: operation_id.as_str().to_owned(),
        operation_kind: operation_kind.to_owned(),
        run_id: run_id.as_str().to_owned(),
        branch: branch.to_owned(),
        selected_ref: selected_ref.to_owned(),
        workspace_session_id: workspace_session_id.0.clone(),
    }
}

fn load_projection_recipe(
    run_control_root: &Path,
    selected: &PairedRefValue,
) -> Result<ProjectionRecipe, WorkspaceSessionError> {
    let recipe_path = run_control_root
        .join("projections")
        .join(format!("{}.json", selected.roots.root_id.as_str()));
    if !recipe_path.exists() {
        return Err(lifecycle_error(format!(
            "exact projection recipe is absent for selected root {}",
            selected.roots.root_id
        )));
    }
    let recipe: ProjectionRecipe = read_json(&recipe_path)
        .map_err(|error| lifecycle_error(format!("read exact projection recipe: {error}")))?;
    recipe
        .validate()
        .map_err(|error| lifecycle_error(format!("validate exact projection recipe: {error}")))?;
    if recipe.roots != selected.roots {
        return Err(lifecycle_error(
            "exact projection recipe roots differ from the selected paired ref",
        ));
    }
    Ok(recipe)
}

fn require_prepared_fixture_cache_layout(
    cache_run_root: &Path,
) -> Result<(), WorkspaceSessionError> {
    for path in [
        cache_run_root.join("locators").join("LOCK"),
        cache_run_root.join("locators").join("CURRENT"),
        cache_run_root.join("refs").join("LOCK"),
        cache_run_root.join("refs").join("JOURNAL"),
    ] {
        if !path.is_file() {
            return Err(lifecycle_error(format!(
                "prepared fixture cache is incomplete: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_prepared_fixture_attachment(
    attachment: &PreparedFixtureAttachment,
    run_id: &RunId,
    fixture_profile: &str,
    expected_cached_allocation_ids: &BTreeSet<AllocationId>,
) -> Result<(), WorkspaceSessionError> {
    if attachment.schema_version != SCHEMA_VERSION
        || attachment.format != PREPARED_FIXTURE_ATTACHMENT_FORMAT
        || attachment.run_id != run_id.as_str()
        || attachment.fixture_profile != fixture_profile
        || attachment.attached_branches
            != [
                "fixture-depth-1".to_owned(),
                "fixture-depth-5".to_owned(),
                "fixture-depth-8".to_owned(),
            ]
        || attachment.cached_allocation_ids.len()
            != usize::try_from(PREPARED_FIXTURE_ALLOCATION_COUNT).unwrap_or(usize::MAX)
    {
        return Err(lifecycle_error(
            "prepared fixture attachment receipt is invalid",
        ));
    }
    let unique = attachment
        .cached_allocation_ids
        .iter()
        .collect::<BTreeSet<_>>();
    if unique.len() != attachment.cached_allocation_ids.len() {
        return Err(lifecycle_error(
            "prepared fixture attachment repeats a cache allocation",
        ));
    }
    if attachment
        .cached_allocation_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != *expected_cached_allocation_ids
    {
        return Err(lifecycle_error(
            "prepared fixture attachment allocations differ from its sealed exact projection",
        ));
    }
    Ok(())
}

fn prepared_fixture_allocation_ids(
    manifest: &PreparedFixtureManifest,
) -> Result<BTreeSet<AllocationId>, WorkspaceSessionError> {
    let depth_eight = manifest
        .branches
        .iter()
        .find(|branch| branch.chain_depth == PREPARED_FIXTURE_ALLOCATION_COUNT)
        .ok_or_else(|| lifecycle_error("prepared fixture omits its depth-eight projection"))?;
    let expected = depth_eight
        .projection
        .lower_allocation_ids_newest_first()
        .into_iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if expected.len() != usize::try_from(PREPARED_FIXTURE_ALLOCATION_COUNT).unwrap_or(usize::MAX) {
        return Err(lifecycle_error(
            "prepared fixture depth-eight projection has the wrong allocation count",
        ));
    }
    Ok(expected)
}

fn prepared_fixture_cache_allocations(
    run_control_root: &Path,
    run_id: &RunId,
) -> Result<BTreeSet<AllocationId>, WorkspaceSessionError> {
    let path = run_control_root.join("PREPARED-FIXTURE-ATTACHMENT.json");
    if !path.exists() {
        return Ok(BTreeSet::new());
    }
    let attachment: PreparedFixtureAttachment = read_json(&path)
        .map_err(|error| lifecycle_error(format!("read prepared fixture attachment: {error}")))?;
    let manifest = read_prepared_fixture_manifest()
        .map_err(|error| lifecycle_error(format!("validate prepared fixture cache: {error}")))?;
    let expected_cached_allocation_ids = prepared_fixture_allocation_ids(&manifest)?;
    validate_prepared_fixture_attachment(
        &attachment,
        run_id,
        PREPARED_FIXTURE_PROFILE,
        &expected_cached_allocation_ids,
    )?;
    Ok(attachment.cached_allocation_ids.into_iter().collect())
}

fn lifecycle_receipt(
    operation_id: &OperationId,
    branch: &str,
    selected: &PairedRefValue,
    idempotent_replay: bool,
    service_elapsed_ns: u64,
) -> MplaLifecycleReceipt {
    MplaLifecycleReceipt {
        operation_id: operation_id.as_str().to_owned(),
        committed: true,
        idempotent_replay,
        selected_ref: selected_ref(branch, selected),
        service_elapsed_ns,
    }
}

fn selected_ref(branch: &str, selected: &PairedRefValue) -> String {
    format!(
        "{branch}@{}#{}",
        selected.sequence.get(),
        selected.checksum_sha256
    )
}

fn run_root(control_root: &Path, run_id: &RunId) -> PathBuf {
    control_root.join("runs").join(run_id.as_str())
}

fn validate_path_component(value: &str, label: &str) -> Result<(), String> {
    let valid = !value.is_empty()
        && value.len() <= 255
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(format!("{label} is not a safe path component"))
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn lifecycle_error(reason: impl Into<String>) -> WorkspaceSessionError {
    WorkspaceSessionError::MplaLifecycle {
        workspace_session_id: WorkspaceSessionId("unallocated".to_owned()),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal_control_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "mpla-activation-journal-{}-{label}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create journal test root");
        root
    }

    fn test_activation_outcome(operation_id: &OperationId, run_id: &RunId) -> ActivationOutcome {
        activation_outcome(
            "activate",
            run_id,
            "main",
            operation_id,
            "main@1#checksum",
            &WorkspaceSessionId("session-1".to_owned()),
        )
    }

    fn prepared_allocation_ids() -> BTreeSet<AllocationId> {
        (0..PREPARED_FIXTURE_ALLOCATION_COUNT)
            .map(|index| AllocationId::from_string(format!("fixture-allocation-{index}")))
            .collect()
    }

    fn prepared_attachment(
        run_id: &RunId,
        allocation_ids: &BTreeSet<AllocationId>,
    ) -> PreparedFixtureAttachment {
        PreparedFixtureAttachment {
            schema_version: SCHEMA_VERSION,
            format: PREPARED_FIXTURE_ATTACHMENT_FORMAT.to_owned(),
            run_id: run_id.as_str().to_owned(),
            fixture_profile: PREPARED_FIXTURE_PROFILE.to_owned(),
            attached_branches: vec![
                "fixture-depth-1".to_owned(),
                "fixture-depth-5".to_owned(),
                "fixture-depth-8".to_owned(),
            ],
            cached_allocation_ids: allocation_ids.iter().cloned().collect(),
        }
    }

    #[test]
    fn repeated_warm_attachment_requires_the_exact_sealed_allocations() {
        let run_id = RunId::parse("fixture-attach-replay").expect("run ID");
        let expected = prepared_allocation_ids();
        let attachment = prepared_attachment(&run_id, &expected);

        validate_prepared_fixture_attachment(
            &attachment,
            &run_id,
            PREPARED_FIXTURE_PROFILE,
            &expected,
        )
        .expect("the exact repeat attachment must remain valid");

        let mut substituted = attachment;
        substituted.cached_allocation_ids.pop();
        substituted
            .cached_allocation_ids
            .push(AllocationId::from_string("fixture-allocation-substitute"));
        assert!(validate_prepared_fixture_attachment(
            &substituted,
            &run_id,
            PREPARED_FIXTURE_PROFILE,
            &expected,
        )
        .is_err());
    }

    #[test]
    fn repeated_warm_attachment_rejects_duplicate_or_missing_allocations() {
        let run_id = RunId::parse("fixture-attach-corrupt").expect("run ID");
        let expected = prepared_allocation_ids();
        let mut missing = prepared_attachment(&run_id, &expected);
        missing.cached_allocation_ids.pop();
        assert!(validate_prepared_fixture_attachment(
            &missing,
            &run_id,
            PREPARED_FIXTURE_PROFILE,
            &expected,
        )
        .is_err());

        let mut duplicate = prepared_attachment(&run_id, &expected);
        duplicate.cached_allocation_ids[1] = duplicate.cached_allocation_ids[0].clone();
        assert!(validate_prepared_fixture_attachment(
            &duplicate,
            &run_id,
            PREPARED_FIXTURE_PROFILE,
            &expected,
        )
        .is_err());
    }

    #[test]
    fn activation_journal_commits_fresh_identity_before_work() {
        let root = journal_control_root("committed");
        let operation_id = OperationId::from_string("operation-committed");
        let run_id = RunId::parse("run-committed").expect("run ID");
        let journal =
            lock_activation_operation(&root, &operation_id, "activate", &run_id, "main", None)
                .expect("create committed activation journal");
        drop(journal);

        let reopened =
            lock_activation_operation(&root, &operation_id, "activate", &run_id, "main", None)
                .expect("reopen committed activation journal");
        assert!(reopened.outcome.is_none());
        drop(reopened);
        std::fs::remove_dir_all(&root).expect("remove journal test root");
    }

    #[test]
    fn activation_journal_replays_exact_outcome_and_rejects_identity_collision() {
        let root = journal_control_root("replay");
        let operation_id = OperationId::from_string("operation-replay");
        let run_id = RunId::parse("run-replay").expect("run ID");
        let journal =
            lock_activation_operation(&root, &operation_id, "activate", &run_id, "main", None)
                .expect("create activation journal");
        let outcome = test_activation_outcome(&operation_id, &run_id);
        journal.persist(outcome.clone()).expect("persist outcome");
        drop(journal);

        let replay =
            lock_activation_operation(&root, &operation_id, "activate", &run_id, "main", None)
                .expect("reopen activation journal");
        assert_eq!(replay.outcome.as_ref(), Some(&outcome));
        drop(replay);
        assert!(lock_activation_operation(
            &root,
            &operation_id,
            "activate",
            &run_id,
            "other",
            None,
        )
        .is_err());
        std::fs::remove_dir_all(&root).expect("remove journal test root");
    }

    #[test]
    fn activation_journal_recovers_only_an_unterminated_outcome_tail() {
        let root = journal_control_root("torn");
        let operation_id = OperationId::from_string("operation-torn");
        let run_id = RunId::parse("run-torn").expect("run ID");
        let journal =
            lock_activation_operation(&root, &operation_id, "activate", &run_id, "main", None)
                .expect("create activation journal");
        let identity_end = journal.identity_end;
        drop(journal);
        let path = root
            .join("runtime-lifecycle")
            .join("activation-operations")
            .join("operation-torn.journal");
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open activation journal tail");
        file.write_all(br#"{"schema_version":"#)
            .expect("write torn outcome");
        file.sync_all().expect("sync torn outcome");
        drop(file);

        let recovered =
            lock_activation_operation(&root, &operation_id, "activate", &run_id, "main", None)
                .expect("recover torn activation journal");
        assert!(recovered.outcome.is_none());
        assert_eq!(
            recovered.file.metadata().expect("journal metadata").len(),
            identity_end
        );
        drop(recovered);
        std::fs::remove_dir_all(&root).expect("remove journal test root");
    }

    #[test]
    fn activation_journal_fails_closed_on_terminated_corruption() {
        let root = journal_control_root("corrupt");
        let operation_id = OperationId::from_string("operation-corrupt");
        let run_id = RunId::parse("run-corrupt").expect("run ID");
        let journal =
            lock_activation_operation(&root, &operation_id, "activate", &run_id, "main", None)
                .expect("create activation journal");
        drop(journal);
        let path = root
            .join("runtime-lifecycle")
            .join("activation-operations")
            .join("operation-corrupt.journal");
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open activation journal tail");
        file.write_all(b"{not-json}\n")
            .expect("write corrupt outcome");
        file.sync_all().expect("sync corrupt outcome");
        drop(file);

        assert!(
            lock_activation_operation(&root, &operation_id, "activate", &run_id, "main", None,)
                .is_err()
        );
        std::fs::remove_dir_all(&root).expect("remove journal test root");
    }
}
