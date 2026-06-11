//! Registered plugin op routing.
//!
//! The caller-family gate (caller-field validation + the isolated-workspace
//! refusal) runs in the `ops::plugin` adapter before any of this; routing here
//! trusts already-validated args.

use eos_namespace::protocol::Intent;
use serde_json::{json, Value};

use super::overlay::{dispatch_oneshot_overlay_route, PluginOverlayOutcome};
use super::state::PluginRuntime;
use crate::error::DaemonError;
use eos_plugin_runtime::route::PluginOperationRoute;

/// Result of dispatching one registered plugin op. Connected routes carry the
/// plugin's reply payload through unchanged; oneshot overlay runs come back
/// typed so the adapter can shape the wire response and splice telemetry.
pub(crate) enum PluginDispatchOutcome {
    Response(Value),
    OneshotOverlay(Box<PluginOverlayOutcome>),
}

impl PluginRuntime {
    /// Dispatch a dynamically registered `plugin.*` op, or `None` when no
    /// loaded plugin claims it.
    pub(crate) fn dispatch_registered_op(
        &self,
        op: &str,
        invocation_id: &str,
        args: &Value,
    ) -> Option<Result<PluginDispatchOutcome, DaemonError>> {
        let route = match self.route_for_op(op) {
            Ok(Some(route)) => route,
            Ok(None) => return None,
            Err(err) => return Some(Err(err)),
        };
        Some(self.dispatch_registered_route(&route, invocation_id, args))
    }

    pub(super) fn route_for_op(
        &self,
        op: &str,
    ) -> Result<Option<PluginOperationRoute>, DaemonError> {
        let state = self.lock_state()?;
        Ok(state
            .loaded
            .values()
            .find_map(|loaded| loaded.operation_routes.get(op).cloned()))
    }

    fn dispatch_registered_route(
        &self,
        route: &PluginOperationRoute,
        invocation_id: &str,
        args: &Value,
    ) -> Result<PluginDispatchOutcome, DaemonError> {
        if route.intent == Intent::ReadOnly && route.service_id.is_some() {
            if let Some(response) =
                self.dispatch_connected_read_only_route(route, invocation_id, args)?
            {
                return Ok(PluginDispatchOutcome::Response(response));
            }
        }
        if route.intent == Intent::WriteAllowed && route.auto_workspace_overlay {
            if let Some(outcome) = dispatch_oneshot_overlay_route(route, invocation_id, args)? {
                return Ok(PluginDispatchOutcome::OneshotOverlay(Box::new(outcome)));
            }
        }
        if route.intent == Intent::WriteAllowed
            && !route.auto_workspace_overlay
            && route.service_id.is_some()
        {
            if let Some(response) =
                self.dispatch_connected_self_managed_route(route, invocation_id, args)?
            {
                return Ok(PluginDispatchOutcome::Response(response));
            }
        }
        Ok(PluginDispatchOutcome::Response(dispatch_deferred_route(
            route,
        )))
    }
}

fn dispatch_deferred_route(route: &PluginOperationRoute) -> Value {
    json!({
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
    })
}
