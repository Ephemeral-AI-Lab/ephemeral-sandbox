//! Workspace checkpoint adapters: LayerStack base/binding/metrics plus the
//! `operation::checkpoint` commit module.

use std::collections::BTreeMap;
use std::time::Instant;

use layerstack::WorkspaceBinding;
use layerstack::{
    build_workspace_base as build_layer_stack_workspace_base,
    ensure_workspace_base as ensure_layer_stack_workspace_base, read_workspace_binding,
    require_workspace_binding, LayerStack,
};
use operation::checkpoint::contract::{
    BindingInput, BindingOutput, BuildBaseInput, CommitInput, CommitOutput, CommitToWorkspaceInput,
    CommitToWorkspaceOutput, EnsureBaseInput, LayerMetricsInput, LayerMetricsOutput,
    WorkspaceBaseOutput,
};
use operation::checkpoint::{CommitOutcome, CommitRequest};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::DispatchContext;
use layerstack::service::cache_snapshot;

use super::to_wire_value;

pub(crate) fn layer_metrics(
    input: LayerMetricsInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    record_checkpoint_route(&context, "layer_metrics_reads_layerstack_directly");
    let root = input.layer_stack_root;
    let stack = LayerStack::open(root.clone())?;
    let manifest = stack.read_active_manifest()?;
    let metrics = stack.storage_metrics()?;
    let binding = read_workspace_binding(&root)?;
    Ok(to_wire_value(LayerMetricsOutput {
        success: true,
        manifest_version: manifest.version,
        manifest_depth: manifest.depth(),
        active_leases: stack.active_lease_count(),
        leased_layers: stack.leased_layers().len(),
        layer_dirs: metrics.layer_dirs,
        referenced_layers: manifest.layers.len(),
        orphan_layer_count: 0,
        missing_layer_count: 0,
        orphan_layer_ids: Vec::new(),
        missing_layer_ids: Vec::new(),
        staging_dirs: metrics.staging_dirs,
        storage_bytes: metrics.storage_bytes,
        workspace_bound: binding.is_some(),
        workspace_root: binding
            .as_ref()
            .map_or_else(String::new, |binding| binding.workspace_root.clone()),
        base_root_hash: binding
            .as_ref()
            .map_or_else(String::new, |binding| binding.base_root_hash.clone()),
        occ_runtime_service_cache: cache_snapshot(),
    }))
}

pub(crate) fn build_workspace_base(
    input: BuildBaseInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    record_checkpoint_route(&context, "workspace_base_builds_layerstack_directly");
    let total_start = Instant::now();
    let root = input.layer_stack_root;
    let workspace_root = input.workspace_root;
    let reset = input.reset;
    let built = build_layer_stack_workspace_base(&root, &workspace_root, reset)?;
    let mut timings = built.timings;
    timings.insert(
        "sandbox.checkpoint.workspace_base.total_s".to_owned(),
        total_start.elapsed().as_secs_f64(),
    );
    record_workspace_base_finished(&context, "build", true, reset, &timings);
    let binding = binding_to_value(&built.binding)?;
    Ok(to_wire_value(WorkspaceBaseOutput {
        success: true,
        created: true,
        binding,
    }))
}

pub(crate) fn ensure_workspace_base(
    input: EnsureBaseInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    record_checkpoint_route(&context, "workspace_base_ensures_layerstack_directly");
    let total_start = Instant::now();
    let root = input.layer_stack_root;
    let workspace_root = input.workspace_root;
    let (binding, created) = ensure_layer_stack_workspace_base(&root, &workspace_root)?;
    let binding = binding_to_value(&binding)?;
    let timings = BTreeMap::from([(
        "sandbox.checkpoint.workspace_base.total_s".to_owned(),
        total_start.elapsed().as_secs_f64(),
    )]);
    record_workspace_base_finished(&context, "ensure", created, false, &timings);
    Ok(to_wire_value(WorkspaceBaseOutput {
        success: true,
        created,
        binding,
    }))
}

pub(crate) fn workspace_binding(
    input: BindingInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    record_checkpoint_route(&context, "workspace_binding_reads_layerstack_directly");
    let root = input.layer_stack_root;
    let binding = require_workspace_binding(&root)?;
    let binding = binding_to_value(&binding)?;
    Ok(to_wire_value(BindingOutput {
        success: true,
        binding,
    }))
}

pub(crate) fn commit_to_workspace(
    input: CommitToWorkspaceInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    record_checkpoint_route(&context, "commit_to_workspace_writes_layerstack_directly");
    let total_start = Instant::now();
    let root = input.layer_stack_root;
    let workspace_root = input.workspace_root;
    let mut stack = LayerStack::open(root)?;
    let (manifest, mut timings) = stack.commit_to_workspace(&workspace_root)?;
    timings.insert(
        "sandbox.checkpoint.commit_to_workspace.total_s".to_owned(),
        total_start.elapsed().as_secs_f64(),
    );
    record_commit_to_workspace_finished(&context, manifest.version, &timings);
    Ok(to_wire_value(CommitToWorkspaceOutput {
        success: true,
        manifest_version: manifest.version,
    }))
}

pub(crate) fn commit_to_git(
    input: CommitInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    record_checkpoint_route(&context, "commit_to_git_uses_layerstack_worktree");
    let outcome = operation::checkpoint::commit_to_git_with_trace_recorder(
        &CommitRequest {
            layer_stack_root: &input.layer_stack_root,
            workspace_root: &input.workspace_root,
            message: &input.message,
            raw_paths: input.paths,
        },
        |event| context.record_trace_event(event.module, event.event, event.details),
    )?;
    record_commit_to_git_finished(&context, &outcome);
    Ok(commit_response(&outcome))
}

fn record_checkpoint_route(context: &DispatchContext<'_>, reason: &'static str) {
    context.record_trace_event(
        "workspace.route",
        "route_selected",
        json!({"kind": "fast_path", "reason": reason}),
    );
}

fn record_workspace_base_finished(
    context: &DispatchContext<'_>,
    action: &'static str,
    created: bool,
    reset: bool,
    timings: &BTreeMap<String, f64>,
) {
    context.record_trace_event(
        "checkpoint",
        "workspace_base_finished",
        json!({
            "action": action,
            "created": created,
            "reset": reset,
            "duration_s": timing(timings, "sandbox.checkpoint.workspace_base.total_s"),
            "phase_count": timings.len(),
            "phases": timings,
        }),
    );
}

fn record_commit_to_workspace_finished(
    context: &DispatchContext<'_>,
    manifest_version: i64,
    timings: &BTreeMap<String, f64>,
) {
    context.record_trace_event(
        "layer_stack",
        "commit_to_workspace_finished",
        json!({
            "success": true,
            "manifest_version": manifest_version,
            "duration_s": timing(timings, "sandbox.checkpoint.commit_to_workspace.total_s"),
            "phase_count": timings.len(),
            "phases": timings,
        }),
    );
}

fn record_commit_to_git_finished(context: &DispatchContext<'_>, outcome: &CommitOutcome) {
    context.record_trace_event(
        "layer_stack",
        "snapshot_lease_used",
        json!({
            "manifest_version": outcome.manifest_version,
            "manifest_depth": timing(&outcome.timings, "resource.layer_stack.manifest_depth"),
            "manifest_path_count": timing(&outcome.timings, "resource.layer_stack.manifest_path_count"),
        }),
    );
    context.record_trace_event(
        "checkpoint",
        "commit_to_git_finished",
        json!({
            "success": true,
            "committed": outcome.committed,
            "worktree_mode": outcome.worktree_mode,
            "manifest_version": outcome.manifest_version,
            "manifest_root_hash": outcome.manifest_root_hash,
            "path_count": outcome.paths.len(),
            "duration_s": timing(
                &outcome.timings,
                "sandbox.checkpoint.commit_to_git.total_s",
            ),
            "phase_count": outcome.timings.len(),
            "phases": outcome.timings,
        }),
    );
}

fn timing(timings: &BTreeMap<String, f64>, key: &str) -> Option<f64> {
    timings.get(key).copied()
}

fn commit_response(outcome: &CommitOutcome) -> Value {
    to_wire_value(CommitOutput {
        success: true,
        committed: outcome.committed,
        commit_sha: outcome.commit_sha.clone(),
        manifest_version: outcome.manifest_version,
        manifest_root_hash: outcome.manifest_root_hash.clone(),
        paths: outcome.paths.clone(),
        worktree_mode: outcome.worktree_mode.to_owned(),
    })
}

fn binding_to_value(binding: &WorkspaceBinding) -> Result<Value, DaemonError> {
    serde_json::to_value(binding).map_err(|err| DaemonError::InvalidRequest(err.to_string()))
}

#[cfg(test)]
#[path = "../../tests/unit/checkpoint/commit.rs"]
mod tests;
