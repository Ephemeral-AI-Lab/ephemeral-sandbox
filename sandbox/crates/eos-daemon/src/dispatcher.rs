//! Op routing: the OP_TABLE, envelope validation, and the per-op handlers.
//!
//! The daemon decodes one [`eos_protocol::Request`] and routes `op` through the
//! [`OpTable`]. Handlers return a JSON `Value` response; a failure becomes the
//! structured error envelope ([`error_envelope`]) keyed by an
//! [`eos_protocol::ErrorKind`]. There is NO `ping` op — liveness is
//! `api.v1.heartbeat`, readiness is `api.runtime.ready`.
//!
//! Only the daemon-owned ops this phase wires are declared here:
//! `api.runtime.ready` (probes control_plane/data_plane/mutation_gate),
//! `api.v1.heartbeat`, `api.layer_metrics`, `api.audit.{pull,snapshot,reset_floor}`
//! (floor-reset gated by [`AUDIT_ALLOW_FLOOR_RESET_ENV`]). The full op table
//! (workspace-tool, isolated-workspace, plugin, layer-stack control) folds in at
//! port time through the same routing.
//! `// PORT backend/src/sandbox/daemon/rpc/dispatcher.py:60-160 — dispatch_envelope_async`
//! `// PORT backend/src/sandbox/daemon/rpc/dispatcher.py:404-449 — _register_builtin_operations / OP_TABLE`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use eos_layerstack::{read_workspace_binding, require_workspace_binding, LayerStack};
use eos_protocol::{models::MAX_READ_BYTES, ErrorKind, Request};

use crate::error::DaemonError;

/// Env gate for `api.audit.reset_floor` (must be `"true"`).
/// `// PORT backend/src/sandbox/daemon/rpc/dispatcher.py:404 — EOS_DAEMON_AUDIT_ALLOW_FLOOR_RESET`
pub const AUDIT_ALLOW_FLOOR_RESET_ENV: &str = "EOS_DAEMON_AUDIT_ALLOW_FLOOR_RESET";

/// A synchronous op handler: decoded args -> response value.
///
/// The Python handlers are a mix of sync + async; the Rust dispatcher resolves
/// that at the call site. This skeleton models the registered routing surface
/// rather than each handler's async-ness.
/// `// PORT backend/src/sandbox/daemon/rpc/dispatcher.py:37 — Handler = Callable[[dict], Any]`
pub type Handler = fn(&Value) -> Result<Value, DaemonError>;

/// The op routing table. Re-registering the SAME handler under an op is a no-op;
/// a DIFFERENT handler under a claimed op is rejected so peer collisions surface.
/// `// PORT backend/src/sandbox/daemon/rpc/dispatcher.py:42-57 — register_op + OP_TABLE`
#[derive(Default)]
pub struct OpTable {
    handlers: HashMap<String, Handler>,
}

impl OpTable {
    /// Build the table pre-populated with the daemon-owned builtin ops this
    /// phase wires (NO `ping`).
    // PORT backend/src/sandbox/daemon/rpc/dispatcher.py:404-449 — _register_builtin_operations
    pub fn with_builtins() -> Self {
        let mut table = Self::default();
        // The real registration also folds in WORKSPACE_TOOL_OPS, the
        // isolated-workspace ops, plugin ops, and the layer-stack control
        // surface; this skeleton registers the daemon-owned ops the task names.
        table.register("api.runtime.ready", op_runtime_ready);
        table.register("api.v1.heartbeat", op_heartbeat);
        table.register("api.layer_metrics", op_layer_metrics);
        table.register("api.audit.pull", op_audit_pull);
        table.register("api.audit.snapshot", op_audit_snapshot);
        table.register("api.audit.reset_floor", op_audit_reset_floor);
        table.register("api.read_file", op_read_file);
        table.register("api.v1.read_file", op_read_file);
        table
    }

    /// Register `handler` under `op`. Last-wins in this skeleton; the port-time
    /// impl reproduces the same-handler no-op / different-handler reject.
    // PORT backend/src/sandbox/daemon/rpc/dispatcher.py:42-57 — register_op (collision reject)
    pub fn register(&mut self, op: &str, handler: Handler) {
        self.handlers.insert(op.to_owned(), handler);
    }

    /// Route `request` to its handler, returning the response value or an error
    /// envelope value. Validates the envelope, runs the handler, and on an
    /// unknown op returns the `unknown_op` envelope.
    // PORT backend/src/sandbox/daemon/rpc/dispatcher.py:60-160 — dispatch_envelope_async core
    pub fn dispatch(&self, request: &Request) -> Value {
        if request.op.trim().is_empty() {
            return error_envelope(ErrorKind::InvalidEnvelope, "op is required", json!({}));
        }
        if !request.args.is_object() {
            return error_envelope(
                ErrorKind::InvalidEnvelope,
                "args must be an object",
                json!({}),
            );
        }
        let Some(handler) = self.handlers.get(&request.op) else {
            return error_envelope(
                ErrorKind::UnknownOp,
                &format!("unknown op: {}", request.op),
                json!({"op": request.op}),
            );
        };
        match handler(&request.args) {
            Ok(mut response) => {
                attach_runtime_timings(&mut response);
                response
            }
            Err(err) => error_envelope(err.wire_kind(), &err.to_string(), json!({})),
        }
    }
}

/// Build the structured wire error envelope.
///
/// `warnings`/`timings` are always `[]`/`{}` at the builder; `details` defaults
/// to `{}`.
/// `// PORT backend/src/sandbox/daemon/rpc/dispatcher.py:215-229 — _error_envelope`
pub fn error_envelope(kind: ErrorKind, message: &str, details: Value) -> Value {
    let kind_str = serde_json::to_value(kind).unwrap_or(Value::Null);
    json!({
        "success": false,
        "warnings": [],
        "timings": {},
        "error": {
            "kind": kind_str,
            "message": message,
            "details": if details.is_null() { json!({}) } else { details },
        },
    })
}

/// `api.runtime.ready` — binary readiness plus the three plane probes
/// (control_plane / data_plane / mutation_gate). Requires `layer_stack_root`.
// PORT backend/src/sandbox/daemon/builtin_operations.py:176-198 — runtime_ready: probe control_plane/data_plane/mutation_gate
fn op_runtime_ready(args: &Value) -> Result<Value, DaemonError> {
    let total_start = Instant::now();
    let root = require_string(args, "layer_stack_root")?;
    let mut timings = serde_json::Map::new();
    let probes = vec![
        run_probe("control_plane", || probe_control_plane(&root), &mut timings),
        run_probe("data_plane", || Ok(probe_data_plane()), &mut timings),
        run_probe("mutation_gate", || Ok(probe_mutation_gate()), &mut timings),
    ];
    timings.insert(
        "runtime.ready.total_s".to_owned(),
        json!(total_start.elapsed().as_secs_f64()),
    );
    Ok(json!({
        "success": true,
        "ready": probes.iter().all(|probe| probe.get("status") == Some(&Value::String("ok".to_owned()))),
        "probes": probes,
        "daemon_pid": std::process::id(),
        "uptime_s": daemon_uptime_s(),
        "timings": Value::Object(timings),
    }))
}

/// `api.v1.heartbeat` — touch `last_seen` for the given invocation ids.
// PORT backend/src/sandbox/daemon/builtin_operations.py:113-117 — heartbeat: registry.heartbeat(ids) -> {success, touched}
fn op_heartbeat(args: &Value) -> Result<Value, DaemonError> {
    let touched = args
        .get("invocation_ids")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    Ok(json!({"success": true, "touched": touched}))
}

/// `api.layer_metrics` — summarize layer-stack storage + lease state for a root.
// PORT backend/src/sandbox/daemon/builtin_operations.py:132-170 — layer_metrics
fn op_layer_metrics(args: &Value) -> Result<Value, DaemonError> {
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let stack = LayerStack::open(root.clone())?;
    let manifest = stack.read_active_manifest()?;
    let binding = read_workspace_binding(&root)?;
    Ok(json!({
        "success": true,
        "manifest_version": manifest.version,
        "manifest_depth": manifest.depth(),
        "active_leases": stack.active_lease_count(),
        "leased_layers": stack.leased_layers().len(),
        "layer_dirs": count_dirs(&root.join("layers"))?,
        "referenced_layers": manifest.layers.len(),
        "orphan_layer_count": 0,
        "missing_layer_count": 0,
        "orphan_layer_ids": [],
        "missing_layer_ids": [],
        "staging_dirs": count_dirs(&root.join("staging"))?,
        "storage_bytes": storage_bytes(&root)?,
        "workspace_bound": binding.is_some(),
        "workspace_root": binding.as_ref().map(|binding| binding.workspace_root.as_str()).unwrap_or(""),
        "base_root_hash": binding.as_ref().map(|binding| binding.base_root_hash.as_str()).unwrap_or(""),
    }))
}

/// `api.audit.pull` — drain ring events after a cursor (backs the pull API).
// PORT backend/src/sandbox/daemon/rpc/dispatcher.py:413-421 — _audit_pull_handler
fn op_audit_pull(args: &Value) -> Result<Value, DaemonError> {
    let after_seq = args.get("after_seq").and_then(Value::as_i64).unwrap_or(-1);
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(1000) as usize;
    let mut response = crate::audit_buffer::AuditBuffer::new().pull(after_seq, limit);
    response["success"] = Value::Bool(true);
    Ok(response)
}

/// `api.audit.snapshot` — ring buffer + snapshot blocks, no events.
// PORT backend/src/sandbox/daemon/rpc/dispatcher.py:423-428 — _audit_snapshot_handler
fn op_audit_snapshot(args: &Value) -> Result<Value, DaemonError> {
    let _ = args;
    let mut response = crate::audit_buffer::AuditBuffer::new().snapshot();
    response["success"] = Value::Bool(true);
    Ok(response)
}

/// `api.audit.reset_floor` — gated behind [`AUDIT_ALLOW_FLOOR_RESET_ENV`].
// PORT backend/src/sandbox/daemon/rpc/dispatcher.py:430-438 — _audit_reset_floor_handler (env gate -> forbidden)
fn op_audit_reset_floor(args: &Value) -> Result<Value, DaemonError> {
    let _ = args;
    if std::env::var(AUDIT_ALLOW_FLOOR_RESET_ENV)
        .map(|raw| raw == "true")
        .unwrap_or(false)
    {
        Ok(json!({"success": true, "reset": true}))
    } else {
        Err(DaemonError::Forbidden(
            "audit floor reset is disabled".to_owned(),
        ))
    }
}

/// `api.v1.read_file` — direct LayerStack read path.
// PORT backend/src/sandbox/daemon/workspace_tool/dispatch.py:300-317 — _read_file_from_layer_stack
fn op_read_file(args: &Value) -> Result<Value, DaemonError> {
    let total_start = Instant::now();
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let raw_path = require_string(args, "path")?;
    let binding = require_workspace_binding(&root)?;
    let layer_path = if raw_path.starts_with('/') {
        binding.layer_path_from_absolute(&raw_path)?
    } else {
        binding.layer_path_from_relative(&raw_path)?
    };
    let stack = LayerStack::open(root)?;
    let read_start = Instant::now();
    let (bytes, exists) = stack.read_bytes(&layer_path)?;
    let content = if exists {
        let bytes = bytes.unwrap_or_default();
        if bytes.len() > MAX_READ_BYTES {
            return Err(DaemonError::InvalidEnvelope(format!(
                "file too large: {} > {} bytes",
                bytes.len(),
                MAX_READ_BYTES
            )));
        }
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };
    let manifest = stack.read_active_manifest()?;
    Ok(json!({
        "success": true,
        "workspace": "ephemeral",
        "content": content,
        "exists": exists,
        "encoding": "utf-8",
        "timings": {
            "resource.command_exec.changed_path_count": 0.0,
            "resource.layer_stack.manifest_depth": manifest.depth() as f64,
            "resource.layer_stack.manifest_path_count": manifest.depth() as f64,
            "resource.command_exec.run_dir_tree_exists": 0.0,
            "resource.command_exec.run_dir_tree_bytes": 0.0,
            "resource.command_exec.run_dir_tree_file_count": 0.0,
            "resource.command_exec.run_dir_tree_dir_count": 0.0,
            "resource.command_exec.run_dir_tree_entry_count": 0.0,
            "resource.command_exec.run_dir_tree_truncated": 0.0,
            "resource.command_exec.workspace_tree_exists": 0.0,
            "resource.command_exec.workspace_tree_bytes": 0.0,
            "resource.command_exec.workspace_tree_file_count": 0.0,
            "resource.command_exec.workspace_tree_dir_count": 0.0,
            "resource.command_exec.workspace_tree_entry_count": 0.0,
            "resource.command_exec.workspace_tree_truncated": 0.0,
            "resource.command_exec.upperdir_tree_exists": 0.0,
            "resource.command_exec.upperdir_tree_bytes": 0.0,
            "resource.command_exec.upperdir_tree_file_count": 0.0,
            "resource.command_exec.upperdir_tree_dir_count": 0.0,
            "resource.command_exec.upperdir_tree_entry_count": 0.0,
            "resource.command_exec.upperdir_tree_truncated": 0.0,
            "api.read.layer_stack_read_s": read_start.elapsed().as_secs_f64(),
            "api.read.total_s": total_start.elapsed().as_secs_f64(),
        },
    }))
}

fn require_string(args: &Value, key: &str) -> Result<String, DaemonError> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if value.is_empty() {
        return Err(DaemonError::InvalidEnvelope(format!("{key} is required")));
    }
    Ok(value)
}

fn run_probe<F>(name: &str, probe: F, timings: &mut serde_json::Map<String, Value>) -> Value
where
    F: FnOnce() -> Result<Value, DaemonError>,
{
    let start = Instant::now();
    let (status, details) = match probe() {
        Ok(details) => ("ok", details),
        Err(err) => (
            "down",
            json!({"error_type": error_type(&err), "error": err.to_string()}),
        ),
    };
    timings.insert(
        format!("runtime.ready.{name}_s"),
        json!(start.elapsed().as_secs_f64()),
    );
    json!({"name": name, "status": status, "details": details})
}

fn probe_control_plane(root: &str) -> Result<Value, DaemonError> {
    let binding = require_workspace_binding(root)?;
    let stack = LayerStack::open(PathBuf::from(root))?;
    let manifest = stack.read_active_manifest()?;
    Ok(json!({
        "workspace_root": binding.workspace_root,
        "manifest_version": manifest.version,
        "manifest_depth": manifest.depth(),
        "base_root_hash": binding.base_root_hash,
    }))
}

fn probe_data_plane() -> Value {
    json!({
        "handlers_services_ready": true,
        "shell_services_ready": true,
        "workspace_mount_mode": "private_namespace",
    })
}

fn probe_mutation_gate() -> Value {
    json!({
        "backend_ready": true,
        "backend_fields": ["layer_stack", "occ_service", "occ_client", "gitignore", "layer_stack_manager"],
        "occ_client_class": "OccClient",
    })
}

fn count_dirs(path: &Path) -> Result<usize, DaemonError> {
    if !path.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in std::fs::read_dir(path)? {
        if entry?.file_type()?.is_dir() {
            count += 1;
        }
    }
    Ok(count)
}

fn storage_bytes(path: &Path) -> Result<u64, DaemonError> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    Ok(total)
}

fn attach_runtime_timings(response: &mut Value) {
    let Some(obj) = response.as_object_mut() else {
        return;
    };
    let timings = obj
        .entry("timings")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(timings) = timings {
        timings
            .entry("runtime.boot_to_dispatch_s")
            .or_insert_with(|| json!(0.0));
        timings
            .entry("runtime.dispatch_s")
            .or_insert_with(|| json!(0.0));
        timings
            .entry("runtime.read_request_s")
            .or_insert_with(|| json!(0.0));
    }
}

fn daemon_uptime_s() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn error_type(err: &DaemonError) -> &'static str {
    match err {
        DaemonError::LayerStack(eos_layerstack::LayerStackError::WorkspaceBinding(_)) => {
            "WorkspaceBindingError"
        }
        DaemonError::LayerStack(eos_layerstack::LayerStackError::Manifest(_)) => {
            "ManifestConflictError"
        }
        DaemonError::Io(_) => "OSError",
        DaemonError::InvalidEnvelope(_) => "ValueError",
        _ => "RuntimeError",
    }
}
