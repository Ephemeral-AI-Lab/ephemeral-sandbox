//! Connected plugin operation dispatch.

use std::path::PathBuf;
use std::time::Duration;

use eos_plugin::{PluginError, PpcDirection, PpcEnvelope};
use serde_json::{json, Value};

use super::{
    callbacks as occ_callbacks, plugin_runtime_config,
    refresh::{ensure_connected_service_current, teardown_failed_connected_service},
};
use crate::error::DaemonError;
use eos_plugin::host::route::PluginOperationRoute;

pub(super) fn dispatch_connected_read_only_route(
    route: &PluginOperationRoute,
    invocation_id: &str,
    args: &Value,
) -> Result<Option<Value>, DaemonError> {
    round_trip_connected_route(route, invocation_id, args, None)
}

pub(super) fn dispatch_connected_self_managed_route(
    route: &PluginOperationRoute,
    invocation_id: &str,
    args: &Value,
) -> Result<Option<Value>, DaemonError> {
    let Some(layer_stack_root) = route.layer_stack_root.clone() else {
        return Ok(None);
    };
    round_trip_connected_route(
        route,
        invocation_id,
        args,
        Some(PathBuf::from(layer_stack_root)),
    )
}

fn round_trip_connected_route(
    route: &PluginOperationRoute,
    invocation_id: &str,
    args: &Value,
    layer_stack_root: Option<PathBuf>,
) -> Result<Option<Value>, DaemonError> {
    let Some(service_instance_id) = route.service_instance_id.clone() else {
        return Ok(None);
    };
    let Some(client) = ensure_connected_service_current(route, invocation_id)? else {
        return Ok(None);
    };
    let timeout = Duration::from_millis(
        route
            .timeout_ms
            .unwrap_or_else(|| plugin_runtime_config().ppc_timeout_ms),
    );
    let request = PpcEnvelope {
        message_id: invocation_id.to_owned(),
        direction: PpcDirection::Request,
        op: route.public_op.clone(),
        body: serde_json::to_string(args).map_err(|err| PluginError::Ppc(err.to_string()))?,
    };
    let reply = match layer_stack_root {
        Some(expected_root) => {
            client.round_trip_with_callbacks(&request, timeout, move |callback| {
                // The OCC writer stays daemon-owned: the injected callback runs
                // `handle_callback_for_root` and its `DaemonError` is carried
                // verbatim through the host's `PpcError::Callback` (the daemon's
                // `From<PpcError>` re-wraps it on the way out).
                occ_callbacks::handle_callback_for_root(&expected_root, callback)
                    .map_err(|err| eos_plugin::host::PpcError::Callback(err.to_string()))
            })
        }
        None => client.round_trip(&request, timeout),
    };
    let reply = match reply {
        Ok(reply) => reply,
        Err(err) => {
            teardown_failed_connected_service(&service_instance_id, &err.to_string())?;
            return Err(err.into());
        }
    };
    response_payload_from_reply(&reply)
}

pub(super) fn response_payload_from_reply(
    reply: &PpcEnvelope,
) -> Result<Option<Value>, DaemonError> {
    let max_response_bytes = plugin_runtime_config().max_response_bytes;
    if reply.body.len() > max_response_bytes {
        return Err(DaemonError::Plugin(PluginError::Ppc(format!(
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
