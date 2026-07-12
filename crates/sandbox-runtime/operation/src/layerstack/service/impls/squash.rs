//! The daemon-local `squash_layerstack` operation: storage squash, the
//! per-session remount sweep, and result assembly.
//!
//! The storage commit is the correctness boundary; the sweep is best-effort
//! cleanup inside the same singleflight (the `SquashOutcome` flight guard
//! stays alive across it). `replaced_layers` derives from post-sweep disk
//! truth; `blocked_reasons` maps `Leased` sessions onto blocks by pre-attempt
//! manifest membership (never-straddle makes that whole-or-none); faulty
//! sessions are reported in the result line and destroyed through the
//! ordinary path.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use sandbox_observability_telemetry::record::names;
use sandbox_observability_telemetry::{sample_layerstack, TraceContext, WalkBudget};
use sandbox_operation_catalog::internal::runtime::SQUASH_LAYERSTACK;
use sandbox_operation_contract::OperationScopeKind;
use sandbox_runtime_layerstack::{
    manifest_root_hash, LayerStack, LayerStackError, SquashPhase, SquashPhaseObserver,
};
use serde_json::{json, Value};

use crate::operations::dispatch::OperationEntry;
use crate::services::SandboxRuntimeOperations;
use crate::workspace_crate::WorkspaceSessionId;
use crate::workspace_session::{SweptDisposition, SweptSession};

const SQUASH_LAYERSTACK_ENTRY: OperationEntry = OperationEntry {
    scope_kind: OperationScopeKind::Sandbox,
    name: SQUASH_LAYERSTACK,
    spec: None,
    dispatch: dispatch_squash_layerstack,
};

const OPERATIONS: &[OperationEntry] = &[SQUASH_LAYERSTACK_ENTRY];

pub(crate) const fn operation_entries() -> &'static [OperationEntry] {
    OPERATIONS
}

fn dispatch_squash_layerstack(
    operations: &SandboxRuntimeOperations,
    _request: &sandbox_operation_contract::OperationRequest,
) -> sandbox_operation_contract::OperationResponse {
    match run_squash_layerstack(operations) {
        Ok(value) => sandbox_operation_contract::OperationResponse::ok(value),
        Err(message) => sandbox_operation_contract::OperationResponse::fault_with_details(
            "operation_failed",
            message,
            json!({}),
        ),
    }
}

fn run_squash_layerstack(operations: &SandboxRuntimeOperations) -> Result<Value, String> {
    operations
        .layerstack
        .obs
        .scope(names::LAYERSTACK_SQUASH, |span| {
            let root = operations.layerstack.layer_stack_root().to_path_buf();
            let mut stack = LayerStack::open(root.clone()).map_err(|error| error.to_string())?;
            let phase_observer = TelemetrySquashPhaseObserver {
                observer: &operations.layerstack.obs,
            };
            let outcome = stack
                .squash_with_observer(&phase_observer)
                .map_err(|error| error.to_string())?;
            span.attr("manifest_version", outcome.manifest.version);
            span.attr("s2_root_hash", manifest_root_hash(&outcome.manifest));
            span.attr("blocks", outcome.blocks.len());
            attach_post_commit_snapshot(span, &root);

            let ids = operations.workspace_session.session_ids();
            span.attr("swept", ids.len());
            span.attr(
                "sweep_width",
                operations.layerstack.config.remount_sweep_width,
            );
            let swept = operations
                .layerstack
                .obs
                .scope(names::LAYERSTACK_SQUASH_REMOUNT_SWEEP, |sweep_span| {
                    sweep_span.attr("sessions", ids.len());
                    sweep_span.attr("width", operations.layerstack.config.remount_sweep_width);
                    Ok::<_, std::convert::Infallible>(remount_sweep(
                        operations,
                        &ids,
                        operations.layerstack.obs.context(),
                    ))
                })
                .unwrap_or_else(|never| match never {});
            if swept
                .iter()
                .any(|session| session.disposition == SweptDisposition::Migrated)
            {
                let _ = operations.workspace_session.persist_handles();
            }

            let mut faulty_sessions = Vec::new();
            for session in &swept {
                if let SweptDisposition::Faulty { class_detail } = &session.disposition {
                    let lease_errors = operations
                        .workspace_session
                        .destroy_faulty_session(&session.workspace_session_id);
                    faulty_sessions.push(json!({
                        "session_id": session.workspace_session_id.0,
                        "class_detail": class_detail,
                        "lease_errors": lease_errors,
                    }));
                }
            }

            let squashed_blocks: Vec<Value> = outcome
                .blocks
                .iter()
                .map(|block| {
                    let replaced_ids: BTreeSet<&str> = block
                        .replaced
                        .iter()
                        .map(|layer| layer.layer_id.as_str())
                        .collect();
                    let reclaimed = block
                        .replaced
                        .iter()
                        .all(|layer| !root.join(&layer.path).exists());
                    let mut entry = json!({
                        "squashed_layer_id": block.squashed_layer.layer_id,
                        "replaced_layer_ids": block
                            .replaced
                            .iter()
                            .map(|layer| layer.layer_id.clone())
                            .collect::<Vec<_>>(),
                        "replaced_layers": if reclaimed { "reclaimed" } else { "leased" },
                    });
                    if !reclaimed {
                        let mut reasons: Vec<String> = swept
                            .iter()
                            .filter_map(|session| match &session.disposition {
                                SweptDisposition::Leased { reason }
                                    if session
                                        .pre_manifest_layer_ids
                                        .iter()
                                        .any(|id| replaced_ids.contains(id.as_str())) =>
                                {
                                    Some(reason.clone())
                                }
                                _ => None,
                            })
                            .collect();
                        reasons.sort();
                        reasons.dedup();
                        if reasons.is_empty() {
                            reasons.push("pinned:lease_holder_not_swept".to_owned());
                        }
                        entry["blocked_reasons"] = json!(reasons);
                    }
                    entry
                })
                .collect();

            let swept_sessions: Vec<Value> = swept
                .iter()
                .map(|session| {
                    let mut entry = json!({
                        "session_id": session.workspace_session_id.0,
                        "disposition": disposition_name(&session.disposition),
                    });
                    match &session.disposition {
                        SweptDisposition::Leased { reason } => entry["reason"] = json!(reason),
                        SweptDisposition::Faulty { class_detail } => {
                            entry["class_detail"] = json!(class_detail);
                        }
                        SweptDisposition::SessionGone
                        | SweptDisposition::Identity
                        | SweptDisposition::Migrated => {}
                    }
                    entry
                })
                .collect();

            let mut result = json!({
                "manifest_version": outcome.manifest.version,
                "squashed_blocks": squashed_blocks,
                "swept_sessions": swept_sessions,
            });
            if !faulty_sessions.is_empty() {
                result["faulty_sessions"] = json!(faulty_sessions);
            }
            Ok(result)
        })
}

/// Capture S2 after the atomic storage commit and before any remount sweep.
/// The sample is attached to the enclosing squash span so collector overhead is
/// excluded from the registered plan/flatten/commit phase durations.
fn attach_post_commit_snapshot(
    span: &sandbox_observability_telemetry::SpanGuard,
    storage_root: &std::path::Path,
) {
    let snapshot = sample_layerstack(storage_root, WalkBudget::default());
    span.attr("s2_layer_count", snapshot.layers.len());
    if let Some(value) = snapshot.total_bytes {
        span.attr("s2_active_logical_bytes", value);
    }
    if let Some(value) = snapshot.total_allocated_bytes {
        span.attr("s2_active_allocated_bytes", value);
    }
    if let Some(value) = snapshot.storage_logical_bytes {
        span.attr("s2_storage_logical_bytes", value);
    }
    if let Some(value) = snapshot.storage_allocated_bytes {
        span.attr("s2_storage_allocated_bytes", value);
    }
    if let Some(value) = snapshot.staging_entry_count {
        span.attr("s2_staging_entry_count", value);
    }
}

struct TelemetrySquashPhaseObserver<'a> {
    observer: &'a sandbox_observability_telemetry::Observer,
}

impl SquashPhaseObserver for TelemetrySquashPhaseObserver<'_> {
    fn observe<T>(
        &self,
        phase: SquashPhase,
        body: impl FnOnce() -> Result<T, LayerStackError>,
    ) -> Result<T, LayerStackError> {
        let name = match phase {
            SquashPhase::Plan => names::LAYERSTACK_SQUASH_PLAN,
            SquashPhase::Flatten => names::LAYERSTACK_SQUASH_FLATTEN,
            SquashPhase::Commit => names::LAYERSTACK_SQUASH_COMMIT,
        };
        self.observer.scope(name, |_| body())
    }
}

fn disposition_name(disposition: &SweptDisposition) -> &'static str {
    match disposition {
        SweptDisposition::SessionGone => "session_gone",
        SweptDisposition::Identity => "identity",
        SweptDisposition::Migrated => "migrated",
        SweptDisposition::Leased { .. } => "leased",
        SweptDisposition::Faulty { .. } => "faulty",
    }
}

/// The post-commit remount sweep: attempt every live session's remount with
/// bounded concurrency. Each per-session remount is independent — it holds only
/// that session's admission gate, freezes only that session's tasks, and mutates
/// only shared manager state under a brief re-lock inside `remount_workspace` —
/// so the expensive quiesce + staged-switch runner overlap across sessions
/// instead of serializing. Results land in plan (session-id) order. `ctx` re-
/// enters the squash trace on each worker so the per-session spans still record.
fn remount_sweep(
    operations: &SandboxRuntimeOperations,
    ids: &[WorkspaceSessionId],
    ctx: Option<TraceContext>,
) -> Vec<SweptSession> {
    if ids.is_empty() {
        return Vec::new();
    }
    let width = operations
        .layerstack
        .config
        .remount_sweep_width
        .min(ids.len());
    if width <= 1 {
        return ids
            .iter()
            .map(|id| operations.workspace_session.remount_session(id))
            .collect();
    }
    let cursor = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<SweptSession>>> =
        (0..ids.len()).map(|_| Mutex::new(None)).collect();
    let obs = &operations.layerstack.obs;
    std::thread::scope(|scope| {
        for _ in 0..width {
            scope.spawn(|| loop {
                let index = cursor.fetch_add(1, Ordering::Relaxed);
                let Some(id) = ids.get(index) else {
                    break;
                };
                let swept = obs.with_context(ctx.clone(), || {
                    operations.workspace_session.remount_session(id)
                });
                *slots[index].lock().unwrap_or_else(PoisonError::into_inner) = Some(swept);
            });
        }
    });
    slots
        .into_iter()
        .filter_map(|slot| slot.into_inner().unwrap_or_else(PoisonError::into_inner))
        .collect()
}
