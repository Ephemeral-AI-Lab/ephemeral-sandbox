//! Workspace checkpoint adapters: LayerStack base/binding/metrics plus the
//! `eos_checkpoint` commit seam.

use std::path::PathBuf;
use std::time::Instant;

use eos_checkpoint::{CommitOutcome, CommitRequest};
use eos_layerstack::{
    build_workspace_base as build_layer_stack_workspace_base,
    ensure_workspace_base as ensure_layer_stack_workspace_base, read_workspace_binding,
    require_workspace_binding, LayerStack,
};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::request_args::{binding_to_value, require_string, timings_to_value_map};
use crate::DispatchContext;
use eos_layerstack::service::cache_snapshot;

pub(crate) fn layer_metrics(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let stack = LayerStack::open(root.clone())?;
    let manifest = stack.read_active_manifest()?;
    let metrics = stack.storage_metrics()?;
    let binding = read_workspace_binding(&root)?;
    Ok(json!({
        "success": true,
        "manifest_version": manifest.version,
        "manifest_depth": manifest.depth(),
        "active_leases": stack.active_lease_count(),
        "leased_layers": stack.leased_layers().len(),
        "layer_dirs": metrics.layer_dirs,
        "referenced_layers": manifest.layers.len(),
        "orphan_layer_count": 0,
        "missing_layer_count": 0,
        "orphan_layer_ids": [],
        "missing_layer_ids": [],
        "staging_dirs": metrics.staging_dirs,
        "storage_bytes": metrics.storage_bytes,
        "workspace_bound": binding.is_some(),
        "workspace_root": binding.as_ref().map_or("", |binding| binding.workspace_root.as_str()),
        "base_root_hash": binding.as_ref().map_or("", |binding| binding.base_root_hash.as_str()),
        "occ_runtime_service_cache": cache_snapshot(),
    }))
}

pub(crate) fn build_workspace_base(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let total_start = Instant::now();
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let workspace_root = PathBuf::from(require_string(args, "workspace_root")?);
    let reset = args.get("reset").and_then(Value::as_bool).unwrap_or(false);
    if reset {
        context
            .require_services()?
            .plugin
            .stop_services_for_layer_stack_root(&root.to_string_lossy())?;
    }
    let built = build_layer_stack_workspace_base(&root, &workspace_root, reset)?;
    let mut timings = timings_to_value_map(&built.timings);
    timings.insert(
        "api.workspace_base.total_s".to_owned(),
        json!(total_start.elapsed().as_secs_f64()),
    );
    let binding = binding_to_value(&built.binding)?;
    Ok(json!({
        "success": true,
        "created": true,
        "binding": binding,
        "timings": Value::Object(timings),
    }))
}

pub(crate) fn ensure_workspace_base(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let total_start = Instant::now();
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let workspace_root = PathBuf::from(require_string(args, "workspace_root")?);
    let (binding, created) = ensure_layer_stack_workspace_base(&root, &workspace_root)?;
    let binding = binding_to_value(&binding)?;
    let timings = json!({
        "api.workspace_base.total_s": total_start.elapsed().as_secs_f64(),
    });
    Ok(json!({
        "success": true,
        "created": created,
        "binding": binding,
        "timings": timings,
    }))
}

pub(crate) fn workspace_binding(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let binding = require_workspace_binding(&root)?;
    let binding = binding_to_value(&binding)?;
    Ok(json!({
        "success": true,
        "binding": binding,
    }))
}

pub(crate) fn commit_to_workspace(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let total_start = Instant::now();
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let workspace_root = PathBuf::from(require_string(args, "workspace_root")?);
    let mut stack = LayerStack::open(root)?;
    let (manifest, commit_timings) = stack.commit_to_workspace(&workspace_root)?;
    let mut timings = timings_to_value_map(&commit_timings);
    timings.insert(
        "api.commit_to_workspace.total_s".to_owned(),
        json!(total_start.elapsed().as_secs_f64()),
    );
    Ok(json!({
        "success": true,
        "manifest_version": manifest.version,
        "timings": Value::Object(timings),
    }))
}

pub(crate) fn commit_to_git(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let layer_stack_root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let workspace_root = PathBuf::from(require_string(args, "workspace_root")?);
    let message = require_string(args, "message")?;
    let raw_paths = raw_commit_paths(args)?;
    let outcome = eos_checkpoint::commit_to_git(&CommitRequest {
        layer_stack_root: &layer_stack_root,
        workspace_root: &workspace_root,
        message: &message,
        raw_paths,
    })?;
    Ok(commit_response(&outcome))
}

/// Lift the raw `paths` pathspecs from the envelope. Normalization (trimming,
/// binding resolution, `.git` rejection) is the host crate's responsibility;
/// this only enforces the wire shape (string, array-of-strings, or absent).
fn raw_commit_paths(args: &Value) -> Result<Vec<String>, DaemonError> {
    let Some(value) = args.get("paths") else {
        return Ok(Vec::new());
    };
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(path) => Ok(vec![path.clone()]),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| DaemonError::InvalidEnvelope("paths must be strings".to_owned()))
            })
            .collect(),
        _ => Err(DaemonError::InvalidEnvelope(
            "paths must be a string or array of strings".to_owned(),
        )),
    }
}

fn commit_response(outcome: &CommitOutcome) -> Value {
    json!({
        "success": true,
        "committed": outcome.committed,
        "commit_sha": outcome.commit_sha,
        "manifest_version": outcome.manifest_version,
        "manifest_root_hash": outcome.manifest_root_hash,
        "paths": outcome.paths,
        "worktree_mode": outcome.worktree_mode,
        "timings": Value::Object(timings_to_value_map(&outcome.timings)),
    })
}

#[cfg(test)]
#[path = "../../tests/unit/checkpoint/commit.rs"]
mod tests;
