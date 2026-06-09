//! Dispatch and workspace audit event emission.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use eos_layerstack::LayerStack;
use eos_protocol::{
    audit::{
        build_event, BackgroundToolSection, Lane, LayerStackSection, OccSection, OsResourceSection,
        OverlayWorkspaceSection, ToolCallSection,
    },
    manifest_root_hash, Request,
};
use serde::Serialize;
use serde_json::Value;

const OS_RESOURCE_SAMPLED: &str = "os_resource.sampled";

fn emit_section<T: Serialize>(event_type: &str, section_key: &str, section: &T, lane: Lane) {
    let Ok(section) = serde_json::to_value(section) else {
        return;
    };
    crate::audit::buffer::safe_emit(build_event(event_type, section_key, section), lane);
}

pub(crate) fn emit_dispatch_audit(request: &Request, response: &Value, dispatch_s: f64) {
    if !should_emit_tool_call_event(&request.op) {
        return;
    }
    let total_ms = dispatch_s * 1000.0;
    let invocation_id = request
        .args
        .get("invocation_id")
        .and_then(Value::as_str)
        .unwrap_or(&request.invocation_id);
    let caller_id = request.args.get("caller_id").and_then(Value::as_str);
    let workspace_mode = response
        .get("workspace_mode")
        .or_else(|| response.get("workspace"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let exit_status = response
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| {
            response
                .get("success")
                .and_then(Value::as_bool)
                .map(|success| if success { "ok" } else { "error" })
        })
        .unwrap_or("unknown");
    emit_section(
        "tool_call.completed",
        "tool_call",
        &ToolCallSection {
            tool_use_id: invocation_id.to_owned(),
            tool_name: request.op.clone(),
            caller_id: caller_id.map(str::to_owned),
            workspace_mode,
            workspace_handle_id: string_field(response, "workspace_handle_id")
                .or_else(|| string_arg(request, "workspace_handle_id")),
            phase: None,
            duration_ms: Some(total_ms),
            total_ms: Some(total_ms),
            exit_status: Some(exit_status.to_owned()),
            bytes_in: None,
            bytes_out: None,
            phase_totals_rollup: phase_totals_rollup(response),
        },
        Lane::Normal,
    );

    emit_os_resource_audit(request, response);
    emit_workspace_base_audit(request, response);
    emit_commit_audit(request, response);
    emit_occ_audit(request, response);
    emit_auto_squash_audit(request, response);
    emit_workspace_lifecycle_audit(request, response, total_ms);
    emit_background_audit(request, response, total_ms);
}

pub(crate) fn should_emit_tool_call_event(op: &str) -> bool {
    !op.starts_with("api.audit.")
        && !matches!(
            op,
            "api.runtime.ready"
                | "api.v1.heartbeat"
                | "api.v1.inflight_count"
                | "api.v1.command_session_count"
        )
}

fn emit_occ_audit(request: &Request, response: &Value) {
    if (!is_occ_op(&request.op) && !is_plugin_overlay_response(response))
        || response_workspace_isolated(response)
    {
        return;
    }
    let changed_path_count = response
        .get("changed_paths")
        .and_then(Value::as_array)
        .map_or(0_i64, |paths| usize_to_i64_saturating(paths.len()));
    let conflict = response.get("conflict").filter(|value| !value.is_null());
    let event_type = if conflict.is_some() {
        "occ.conflict"
    } else {
        "occ.publish"
    };
    let conflict_kind = conflict
        .and_then(|value| value.get("reason"))
        .and_then(Value::as_str)
        .or_else(|| response.get("conflict_reason").and_then(Value::as_str));
    emit_section(
        event_type,
        "occ",
        &OccSection {
            operation_id: Some(request.invocation_id.clone()),
            changed_path_count: Some(changed_path_count),
            prepare_ms: timing_ms(response, "occ.prepare.total_s"),
            apply_ms: timing_ms(response, "command_exec.occ_apply_s")
                .or_else(|| timing_ms(response, "api.write.occ_apply_s"))
                .or_else(|| timing_ms(response, "api.edit.occ_apply_s")),
            commit_ms: timing_ms(response, "occ.commit.total_s"),
            publish_layer_ms: timing_ms(response, "occ.commit.publish_layer_s"),
            conflict_kind: conflict_kind.map(str::to_owned),
            conflict_path: conflict
                .and_then(|value| value.get("conflict_file"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            conflict_reason: string_field(response, "conflict_reason"),
            current_manifest_version: timing_i64(response, "resource.layer_stack.manifest_depth"),
            ..OccSection::default()
        },
        Lane::Normal,
    );
}

fn emit_workspace_lifecycle_audit(request: &Request, response: &Value, total_ms: f64) {
    if request.op == "api.layer_metrics" {
        emit_section(
            "layer_stack.maintenance",
            "layer_stack",
            &LayerStackSection {
                operation_id: Some(request.invocation_id.clone()),
                manifest_version: response.get("manifest_version").and_then(Value::as_i64),
                layer_count: response.get("manifest_depth").and_then(Value::as_i64),
                lease_hold_ms: Some(total_ms),
                total_ms: Some(total_ms),
                ..LayerStackSection::default()
            },
            Lane::Normal,
        );
        return;
    }
    if !uses_overlay_or_lease(&request.op, response) {
        return;
    }
    if let Some(lease_wait_ms) = timing_ms(response, "layer_stack.acquire_snapshot.total_s") {
        emit_section(
            "layer_stack.lease_acquired",
            "layer_stack",
            &LayerStackSection {
                operation_id: Some(request.invocation_id.clone()),
                owner_request_id: Some(request.invocation_id.clone()),
                manifest_version: timing_i64(response, "resource.layer_stack.manifest_depth"),
                layer_count: timing_i64(response, "resource.layer_stack.manifest_path_count"),
                lease_wait_ms: Some(lease_wait_ms),
                ..LayerStackSection::default()
            },
            Lane::Normal,
        );
    }
    emit_section(
        "layer_stack.lease_released",
        "layer_stack",
        &LayerStackSection {
            operation_id: Some(request.invocation_id.clone()),
            owner_request_id: Some(request.invocation_id.clone()),
            manifest_version: timing_i64(response, "resource.layer_stack.manifest_depth"),
            layer_count: timing_i64(response, "resource.layer_stack.manifest_path_count"),
            lease_hold_ms: Some(total_ms),
            total_ms: Some(total_ms),
            ..LayerStackSection::default()
        },
        Lane::Normal,
    );
    emit_section(
        "overlay_workspace.cleanup",
        "overlay_workspace",
        &OverlayWorkspaceSection {
            operation_id: Some(request.invocation_id.clone()),
            workspace_mode: response
                .get("workspace_mode")
                .or_else(|| response.get("workspace"))
                .and_then(Value::as_str)
                .unwrap_or("ephemeral")
                .to_owned(),
            cleanup_ms: Some(total_ms),
            scratch_removed: Some(true),
            changed_path_count: response
                .get("changed_paths")
                .and_then(Value::as_array)
                .map(|paths| usize_to_i64_saturating(paths.len())),
            ..OverlayWorkspaceSection::default()
        },
        Lane::Normal,
    );
}

/// Emit a `layer_stack` completion event whose section is fully derived from the
/// active manifest stats. Shared by the workspace-base and commit audits, which
/// differ only in their event type, manifest-version source, and total timing.
fn emit_layer_stack_completion(
    request: &Request,
    event_type: &str,
    manifest_version: Option<i64>,
    total_ms: f64,
) {
    let active = active_manifest_stats(request, manifest_version);
    emit_section(
        event_type,
        "layer_stack",
        &LayerStackSection {
            operation_id: Some(request.invocation_id.clone()),
            manifest_version,
            manifest_root_hash: active.as_ref().map(|stats| stats.root_hash.clone()),
            layer_count: active.map(|stats| stats.depth),
            total_ms: Some(total_ms),
            ..LayerStackSection::default()
        },
        Lane::Normal,
    );
}

fn emit_workspace_base_audit(request: &Request, response: &Value) {
    let Some(total_ms) = timing_ms(response, "api.workspace_base.total_s") else {
        return;
    };
    let event_type = match request.op.as_str() {
        "api.ensure_workspace_base" => "workspace_base.ensured",
        "api.build_workspace_base" => "workspace_base.built",
        _ => return,
    };
    let manifest_version = response
        .get("binding")
        .and_then(|binding| binding.get("active_manifest_version"))
        .and_then(Value::as_i64);
    emit_layer_stack_completion(request, event_type, manifest_version, total_ms);
}

fn emit_commit_audit(request: &Request, response: &Value) {
    let Some(total_ms) = timing_ms(response, "api.commit_to_workspace.total_s") else {
        return;
    };
    if request.op != "api.commit_to_workspace" {
        return;
    }
    let manifest_version = response.get("manifest_version").and_then(Value::as_i64);
    emit_layer_stack_completion(
        request,
        "layer_stack.commit_completed",
        manifest_version,
        total_ms,
    );
}

pub(crate) fn emit_auto_squash_audit(request: &Request, response: &Value) {
    let Some(input_layers) = timing_i64(response, "layer_stack.auto_squash.depth_before") else {
        return;
    };
    let total_ms = timing_ms(response, "layer_stack.auto_squash.total_s");
    emit_section(
        "layer_stack.squash_triggered",
        "layer_stack",
        &LayerStackSection {
            operation_id: Some(request.invocation_id.clone()),
            owner_request_id: Some(request.invocation_id.clone()),
            squash_trigger_reason: Some("post_publish_depth".to_owned()),
            squash_input_layers: Some(input_layers),
            ..LayerStackSection::default()
        },
        Lane::Critical,
    );
    if timing_f64(response, "layer_stack.auto_squash.raced").unwrap_or(0.0) > 0.0 {
        emit_section(
            "layer_stack.squash_failed",
            "layer_stack",
            &LayerStackSection {
                operation_id: Some(request.invocation_id.clone()),
                owner_request_id: Some(request.invocation_id.clone()),
                squash_trigger_reason: Some("post_publish_depth".to_owned()),
                squash_input_layers: Some(input_layers),
                squash_failure_kind: Some("raced_or_plan_aborted".to_owned()),
                total_ms,
                ..LayerStackSection::default()
            },
            Lane::Critical,
        );
        return;
    }
    let Some(result_layers) = timing_i64(response, "layer_stack.auto_squash.depth_after") else {
        return;
    };
    let manifest_version = timing_i64(response, "layer_stack.auto_squash.manifest_version");
    emit_section(
        "layer_stack.squash_completed",
        "layer_stack",
        &LayerStackSection {
            operation_id: Some(request.invocation_id.clone()),
            owner_request_id: Some(request.invocation_id.clone()),
            manifest_root_hash: active_manifest_root_hash(request, manifest_version),
            squash_trigger_reason: Some("post_publish_depth".to_owned()),
            squash_input_layers: Some(input_layers),
            squash_result_layers: Some(result_layers),
            total_ms,
            ..LayerStackSection::default()
        },
        Lane::Critical,
    );
}

fn active_manifest_root_hash(request: &Request, expected_version: Option<i64>) -> Option<String> {
    active_manifest_stats(request, expected_version).map(|stats| stats.root_hash)
}

struct ActiveManifestStats {
    root_hash: String,
    depth: i64,
}

fn active_manifest_stats(
    request: &Request,
    expected_version: Option<i64>,
) -> Option<ActiveManifestStats> {
    let expected_version = expected_version?;
    let root = request
        .args
        .get("layer_stack_root")
        .and_then(Value::as_str)?;
    let manifest = LayerStack::open(PathBuf::from(root))
        .ok()?
        .read_active_manifest()
        .ok()?;
    (manifest.version == expected_version).then(|| ActiveManifestStats {
        root_hash: manifest_root_hash(&manifest),
        depth: usize_to_i64_saturating(manifest.depth()),
    })
}

fn emit_background_audit(request: &Request, response: &Value, total_ms: f64) {
    let Some((event_type, work_kind)) = background_event_kind(request, response) else {
        return;
    };
    let command_session_id = request
        .args
        .get("command_session_id")
        .and_then(Value::as_str)
        .or_else(|| response.get("command_session_id").and_then(Value::as_str))
        .unwrap_or(&request.invocation_id);
    emit_section(
        event_type,
        "background_tool",
        &BackgroundToolSection {
            background_work_id: command_session_id.to_owned(),
            work_kind: Some(work_kind.to_owned()),
            tool_name: Some(request.op.clone()),
            caller_id: string_arg(request, "caller_id"),
            uptime_ms: None,
            status: string_field(response, "status"),
            exit_code: response.get("exit_code").and_then(Value::as_i64),
            duration_ms: Some(total_ms),
            error_kind: response
                .get("error")
                .and_then(|error| error.get("kind"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            cancel_reason: string_field(response, "cancel_reason"),
            delivery_latency_ms: None,
        },
        Lane::Normal,
    );
}

pub(crate) fn background_event_kind(
    request: &Request,
    response: &Value,
) -> Option<(&'static str, &'static str)> {
    match request.op.as_str() {
        "api.v1.exec_command" if response.get("command_session_id").is_some() => {
            Some(("background_tool.started", "command_session"))
        }
        "api.v1.write_stdin" => Some(("background_tool.input", "command_session")),
        "api.v1.command.cancel" => Some(("background_tool.cancelled", "command_session")),
        "api.v1.command.collect_completed" => {
            Some(("background_tool.completed", "command_session"))
        }
        _ => None,
    }
}

fn is_occ_op(op: &str) -> bool {
    matches!(
        op,
        "api.v1.write_file" | "api.v1.edit_file" | "api.v1.exec_command"
    )
}

pub(crate) fn uses_overlay_or_lease(op: &str, response: &Value) -> bool {
    if response_workspace_isolated(response) {
        return false;
    }
    if op == "api.v1.command.cancel" {
        return true;
    }
    if op == "api.v1.exec_command" {
        return response
            .get("command_session_id")
            .and_then(Value::as_str)
            .is_none();
    }
    if is_plugin_overlay_response(response) {
        return true;
    }
    false
}

fn response_workspace_isolated(response: &Value) -> bool {
    response
        .get("workspace_mode")
        .or_else(|| response.get("workspace"))
        .and_then(Value::as_str)
        == Some("isolated")
}

fn is_plugin_overlay_response(response: &Value) -> bool {
    response.get("plugin_overlay").is_some()
}

fn emit_os_resource_audit(request: &Request, response: &Value) {
    let invocation_id = request
        .args
        .get("invocation_id")
        .and_then(Value::as_str)
        .unwrap_or(&request.invocation_id)
        .to_owned();
    let section = OsResourceSection {
        operation_id: Some(request.invocation_id.clone()),
        tool_use_id: Some(invocation_id),
        caller_id: string_arg(request, "caller_id"),
        sampled_at_monotonic_s: audit_monotonic_s(),
        rss_bytes: timing_i64(response, "resource.process.rss_bytes"),
        max_rss_bytes: timing_i64(response, "resource.process.max_rss_bytes"),
        memory_current_bytes: timing_i64(response, "resource.cgroup.memory_current_bytes"),
        memory_peak_bytes: timing_i64(response, "resource.cgroup.memory_peak_bytes"),
        cpu_user_s: timing_f64(response, "resource.cgroup.cpu_user_usec").map(usec_to_seconds),
        cpu_system_s: timing_f64(response, "resource.cgroup.cpu_system_usec").map(usec_to_seconds),
        cpu_throttled_us: timing_i64(response, "resource.cgroup.cpu_throttled_usec"),
        io_read_bytes: timing_i64(response, "resource.cgroup.io_rbytes"),
        io_write_bytes: timing_i64(response, "resource.cgroup.io_wbytes"),
        io_read_ops: timing_i64(response, "resource.cgroup.io_rios"),
        io_write_ops: timing_i64(response, "resource.cgroup.io_wios"),
    };
    if !has_os_resource_values(&section) {
        return;
    }
    emit_section(OS_RESOURCE_SAMPLED, "os_resource", &section, Lane::Sample);
}

fn has_os_resource_values(section: &OsResourceSection) -> bool {
    section.rss_bytes.is_some()
        || section.max_rss_bytes.is_some()
        || section.memory_current_bytes.is_some()
        || section.memory_peak_bytes.is_some()
        || section.cpu_user_s.is_some()
        || section.cpu_system_s.is_some()
        || section.cpu_throttled_us.is_some()
        || section.io_read_bytes.is_some()
        || section.io_write_bytes.is_some()
        || section.io_read_ops.is_some()
        || section.io_write_ops.is_some()
}

fn phase_totals_rollup(response: &Value) -> Option<BTreeMap<String, f64>> {
    let timings = response.get("timings")?.as_object()?;
    let rollup = timings
        .iter()
        .filter_map(|(key, value)| value.as_f64().map(|value| (key.clone(), value)))
        .collect::<BTreeMap<_, _>>();
    (!rollup.is_empty()).then_some(rollup)
}

fn string_arg(request: &Request, key: &str) -> Option<String> {
    request
        .args
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn usec_to_seconds(usec: f64) -> f64 {
    usec / 1_000_000.0
}

fn audit_monotonic_s() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn timing_ms(response: &Value, key: &str) -> Option<f64> {
    timing_f64(response, key).map(|seconds| seconds * 1000.0)
}

fn timing_i64(response: &Value, key: &str) -> Option<i64> {
    timing_f64(response, key).map(f64_to_i64_rounded_saturating)
}

fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn f64_to_i64_rounded_saturating(value: f64) -> i64 {
    value.round() as i64
}

fn timing_f64(response: &Value, key: &str) -> Option<f64> {
    response
        .get("timings")
        .and_then(Value::as_object)
        .and_then(|timings| timings.get(key))
        .and_then(Value::as_f64)
}

#[cfg(test)]
#[path = "../../tests/audit_events/mod.rs"]
mod tests;
