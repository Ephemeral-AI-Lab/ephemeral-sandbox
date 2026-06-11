//! Registered plugin op routing.
//!
//! The caller-family gate (caller-field validation + isolated-workspace
//! refusal) runs through `RuntimeServices` before any of this; routing here
//! trusts already-validated args.

use std::path::PathBuf;
use std::time::Duration;

use eos_namespace::protocol::Intent;
use eos_plugin::{PluginError, PpcDirection, PpcMessage};
use serde_json::{json, Value};

use super::callbacks as occ_callbacks;
use super::overlay::PluginOverlayOutcome;
use super::state::PluginRuntime;
use crate::route::PluginOperationRoute;
use crate::PluginRuntimeError;

/// Result of dispatching one registered plugin op. Connected routes carry the
/// plugin's reply payload through unchanged; oneshot overlay runs come back
/// typed so the adapter can shape the wire response and splice telemetry.
pub enum PluginDispatchOutcome {
    Response(Value),
    OneshotOverlay(Box<PluginOverlayOutcome>),
}

impl PluginRuntime {
    /// Dispatch a dynamically registered `plugin.*` op, or `None` when no
    /// loaded plugin claims it.
    pub fn dispatch_registered_op(
        &self,
        op: &str,
        invocation_id: &str,
        args: &Value,
    ) -> Option<Result<PluginDispatchOutcome, PluginRuntimeError>> {
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
    ) -> Result<Option<PluginOperationRoute>, PluginRuntimeError> {
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
    ) -> Result<PluginDispatchOutcome, PluginRuntimeError> {
        if route.intent == Intent::ReadOnly && route.service_id.is_some() {
            if let Some(response) =
                self.dispatch_connected_read_only_route(route, invocation_id, args)?
            {
                return Ok(PluginDispatchOutcome::Response(response));
            }
        }
        if route.intent == Intent::WriteAllowed && route.auto_workspace_overlay {
            if let Some(outcome) =
                self.dispatch_oneshot_overlay_route(route, invocation_id, args)?
            {
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

    pub(super) fn dispatch_connected_read_only_route(
        &self,
        route: &PluginOperationRoute,
        invocation_id: &str,
        args: &Value,
    ) -> Result<Option<Value>, PluginRuntimeError> {
        self.round_trip_connected_route(route, invocation_id, args, None)
    }

    pub(super) fn dispatch_connected_self_managed_route(
        &self,
        route: &PluginOperationRoute,
        invocation_id: &str,
        args: &Value,
    ) -> Result<Option<Value>, PluginRuntimeError> {
        let Some(layer_stack_root) = route.layer_stack_root.clone() else {
            return Ok(None);
        };
        self.round_trip_connected_route(
            route,
            invocation_id,
            args,
            Some(PathBuf::from(layer_stack_root)),
        )
    }

    fn round_trip_connected_route(
        &self,
        route: &PluginOperationRoute,
        invocation_id: &str,
        args: &Value,
        layer_stack_root: Option<PathBuf>,
    ) -> Result<Option<Value>, PluginRuntimeError> {
        let Some(service_instance_id) = route.service_instance_id.clone() else {
            return Ok(None);
        };
        let Some(client) = self.ensure_connected_service_current(route, invocation_id)? else {
            return Ok(None);
        };
        let timeout = Duration::from_millis(route.timeout_ms.unwrap_or(self.config.ppc_timeout_ms));
        let request = PpcMessage {
            message_id: invocation_id.to_owned(),
            direction: PpcDirection::Request,
            op: route.public_op.clone(),
            body: serde_json::to_string(args).map_err(|err| PluginError::Ppc(err.to_string()))?,
        };
        let reply = match layer_stack_root {
            Some(expected_root) => {
                client.round_trip_with_callbacks(&request, timeout, move |callback| {
                    occ_callbacks::handle_callback_for_root(&expected_root, callback)
                        .map_err(|err| crate::PpcError::Callback(err.to_string()))
                })
            }
            None => client.round_trip(&request, timeout),
        };
        let reply = match reply {
            Ok(reply) => reply,
            Err(err) => {
                self.teardown_failed_connected_service(&service_instance_id, &err.to_string())?;
                return Err(err.into());
            }
        };
        self.response_payload_from_reply(&reply)
    }

    pub(super) fn response_payload_from_reply(
        &self,
        reply: &PpcMessage,
    ) -> Result<Option<Value>, PluginRuntimeError> {
        let max_response_bytes = self.config.max_response_bytes;
        if reply.body.len() > max_response_bytes {
            return Err(PluginRuntimeError::Plugin(PluginError::Ppc(format!(
                "plugin response exceeds {max_response_bytes} byte limit"
            ))));
        }
        let payload: Value =
            serde_json::from_str(&reply.body).map_err(|err| PluginError::Ppc(err.to_string()))?;
        if payload.is_object() {
            Ok(Some(payload))
        } else {
            Ok(Some(json!({
                "success": true,
                "result": payload,
            })))
        }
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
