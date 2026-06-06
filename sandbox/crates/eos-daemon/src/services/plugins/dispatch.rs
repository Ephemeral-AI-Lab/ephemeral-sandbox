//! Registered plugin op routing.

use eos_protocol::Intent;
use serde_json::{json, Value};

use super::{
    connected::{dispatch_connected_read_only_route, dispatch_connected_self_managed_route},
    ensure_plugin_family_allowed,
    overlay::dispatch_oneshot_overlay_route,
    state::{lock_state, PluginOperationRoute},
};
use crate::{dispatcher::DispatchContext, error::DaemonError};

pub(super) fn dispatch_registered_op(
    op: &str,
    invocation_id: &str,
    args: &Value,
    _context: DispatchContext<'_>,
) -> Option<Result<Value, DaemonError>> {
    if !op.starts_with("plugin.") {
        return None;
    }
    // Single caller-family gate for the whole registered-op dispatch chain; the
    // private `dispatch_registered_route`/`dispatch_deferred_route` helpers below
    // are only reachable from here and trust the already-validated args.
    if let Err(err) = ensure_plugin_family_allowed(args) {
        return Some(Err(err));
    }
    let route = match route_for_op(op) {
        Ok(Some(route)) => route,
        Ok(None) => return None,
        Err(err) => return Some(Err(err)),
    };
    Some(dispatch_registered_route(&route, invocation_id, args))
}

pub(super) fn route_for_op(op: &str) -> Result<Option<PluginOperationRoute>, DaemonError> {
    let state = lock_state()?;
    Ok(state
        .loaded
        .values()
        .find_map(|loaded| loaded.operation_routes.get(op).cloned()))
}

fn dispatch_registered_route(
    route: &PluginOperationRoute,
    invocation_id: &str,
    args: &Value,
) -> Result<Value, DaemonError> {
    if route.intent == Intent::ReadOnly && route.service_id.is_some() {
        if let Some(response) = dispatch_connected_read_only_route(route, invocation_id, args)? {
            return Ok(response);
        }
    }
    if route.intent == Intent::WriteAllowed && route.auto_workspace_overlay {
        if let Some(response) = dispatch_oneshot_overlay_route(route, invocation_id, args)? {
            return Ok(response);
        }
    }
    if route.intent == Intent::WriteAllowed
        && !route.auto_workspace_overlay
        && route.service_id.is_some()
    {
        if let Some(response) = dispatch_connected_self_managed_route(route, invocation_id, args)? {
            return Ok(response);
        }
    }
    dispatch_deferred_route(route)
}

fn dispatch_deferred_route(route: &PluginOperationRoute) -> Result<Value, DaemonError> {
    Ok(json!({
        "success": false,
        "status": "deferred",
        "op": route.public_op,
        "plugin": route.plugin_id,
        "op_name": route.op_name,
        "intent": route.intent,
        "auto_workspace_overlay": route.auto_workspace_overlay,
        "service_id": route.service_id,
        "dispatch_mode": route.dispatch_mode(),
        "error": {
            "kind": "plugin_dispatch_deferred",
            "message": "plugin service is not connected for this route",
            "details": {
                "op": route.public_op,
                "dispatch_mode": route.dispatch_mode(),
            },
        },
    }))
}
