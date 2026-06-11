//! Op routing: the `OP_TABLE` and envelope validation.
//!
//! The daemon decodes one [`crate::wire::Request`] and routes `op` through the
//! [`OpTable`]. Handlers return a JSON `Value` response; a failure becomes the
//! structured error envelope ([`error_envelope`]) keyed by an
//! [`crate::wire::ErrorKind`]. There is NO `ping` op — liveness is
//! `api.v1.heartbeat`, readiness is `api.runtime.ready`.
//!
//! Built-in handlers are described in [`crate::dispatch::registry`]. Dynamic plugin
//! handlers are intentionally deferred: after a built-in miss, the dispatcher
//! asks the plugin service registry whether the op was installed at runtime.

use std::collections::HashMap;
#[cfg(test)]
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Value};

use crate::wire::{ErrorKind, Request};
#[cfg(test)]
use eos_layerstack::LayerStack;

use super::registry::BUILTIN_OPS;
use crate::error::DaemonError;
use crate::ops::plugin;
#[cfg(test)]
use crate::response::{insert_tree_resource_timings, resource_timings, TreeResourceStats};
use crate::runtime::context::DispatchContext;
#[cfg(test)]
use crate::runtime::invocation_registry::InFlightRegistry;

/// A synchronous op handler: decoded args -> response value.
///
/// The daemon keeps the routing surface explicit here and lets each op facade
/// own request shaping before delegating to its implementation service.
pub(crate) type Handler = for<'ctx> fn(&Value, DispatchContext<'ctx>) -> Result<Value, DaemonError>;

/// The op routing table.
///
/// Re-registering the same handler under an op is a no-op; a different handler
/// under a claimed op is rejected so peer collisions surface.
#[derive(Clone, Default)]
pub struct OpTable {
    handlers: HashMap<String, Handler>,
}

impl OpTable {
    /// Build the table pre-populated with the daemon-owned builtin ops (NO
    /// `ping`). Every op is registered under its canonical `sandbox.*` name.
    pub fn with_builtins() -> Self {
        let mut table = Self::default();
        for op in BUILTIN_OPS {
            table.register_builtin(op.spec.name, op.handler);
        }
        table
    }

    /// Register `handler` under `op`.
    ///
    /// Returns `true` when the handler was inserted or already registered.
    /// Returns `false` when `op` is already claimed by a different handler,
    /// leaving the original route intact.
    #[must_use = "registration collisions are rejected; callers must check the result"]
    fn register(&mut self, op: &str, handler: Handler) -> bool {
        if let Some(existing) = self.handlers.get(op) {
            return std::ptr::fn_addr_eq(*existing, handler);
        }
        self.handlers.insert(op.to_owned(), handler);
        true
    }

    fn register_builtin(&mut self, op: &str, handler: Handler) {
        assert!(
            self.register(op, handler),
            "builtin op registered with a different handler: {op}"
        );
    }

    /// Route `request` to its handler, returning the response value or an error
    /// envelope value. Validates the envelope, runs the handler, and on an
    /// unknown op returns the `unknown_op` envelope.
    #[must_use]
    pub fn dispatch(&self, request: &Request) -> Value {
        self.dispatch_with_context(request, DispatchContext::empty())
    }

    /// Route `request` with daemon runtime context.
    #[must_use]
    pub fn dispatch_with_context(&self, request: &Request, context: DispatchContext<'_>) -> Value {
        let dispatch_start = Instant::now();
        let boot_to_dispatch_s = daemon_uptime_s();
        let read_request_s = context.read_request_s().unwrap_or(0.0);
        let finalize = |response| {
            finalize_response(response, boot_to_dispatch_s, dispatch_start, read_request_s)
        };
        if request.op.trim().is_empty() {
            return finalize(error_envelope(
                ErrorKind::InvalidEnvelope,
                "op is required",
                json!({}),
            ));
        }
        if !request.args.is_object() {
            return finalize(error_envelope(
                ErrorKind::InvalidEnvelope,
                "args must be an object",
                json!({}),
            ));
        }
        let Some(handler) = self.handlers.get(&request.op) else {
            if let Some(response) = plugin::dispatch_registered_op(
                &request.op,
                &request.invocation_id,
                &request.args,
                context,
            ) {
                return finalize(match response {
                    Ok(response) => response,
                    Err(err) => error_envelope(err.wire_kind(), &err.to_string(), json!({})),
                });
            }
            return finalize(error_envelope(
                ErrorKind::UnknownOp,
                &format!("unknown op: {}", request.op),
                json!({"op": request.op}),
            ));
        };
        finalize(match handler(&request.args, context) {
            Ok(response) => response,
            Err(err) => error_envelope(err.wire_kind(), &err.to_string(), json!({})),
        })
    }
}

/// Attach the dispatch runtime timings to `response` and return the finalized
/// response.
fn finalize_response(
    mut response: Value,
    boot_to_dispatch_s: f64,
    dispatch_start: Instant,
    read_request_s: f64,
) -> Value {
    let dispatch_s = dispatch_start.elapsed().as_secs_f64();
    attach_runtime_timings(
        &mut response,
        boot_to_dispatch_s,
        dispatch_s,
        read_request_s,
    );
    response
}

/// Build the structured wire error envelope.
///
/// `warnings`/`timings` are always `[]`/`{}` at the builder. `details`
/// defaults to `{}` and `internal_error` responses receive a generated
/// `details.error_id` when the caller did not provide one.
#[must_use]
pub(crate) fn error_envelope(kind: ErrorKind, message: &str, details: Value) -> Value {
    let is_internal_error = kind == ErrorKind::InternalError;
    let kind_str = serde_json::to_value(kind).unwrap_or(Value::Null);
    let details = error_details(is_internal_error, details);
    json!({
        "success": false,
        "warnings": [],
        "timings": {},
        "error": {
            "kind": kind_str,
            "message": message,
            "details": details,
        },
    })
}

fn error_details(is_internal_error: bool, details: Value) -> Value {
    if !is_internal_error {
        return if details.is_null() {
            json!({})
        } else {
            details
        };
    }
    let mut details = match details {
        Value::Null => serde_json::Map::new(),
        Value::Object(details) => details,
        other => {
            let mut object = serde_json::Map::new();
            object.insert("value".to_owned(), other);
            object
        }
    };
    details
        .entry("error_id")
        .or_insert_with(|| Value::String(new_error_id()));
    Value::Object(details)
}

fn new_error_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn attach_runtime_timings(
    response: &mut Value,
    boot_to_dispatch_s: f64,
    dispatch_s: f64,
    read_request_s: f64,
) {
    let Some(obj) = response.as_object_mut() else {
        return;
    };
    let timings = obj
        .entry("timings")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(timings) = timings {
        timings.insert(
            "runtime.boot_to_dispatch_s".to_owned(),
            json!(boot_to_dispatch_s),
        );
        timings.insert("runtime.dispatch_s".to_owned(), json!(dispatch_s));
        timings.insert("runtime.read_request_s".to_owned(), json!(read_request_s));
    }
}

pub(crate) fn daemon_uptime_s() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[cfg(test)]
#[path = "../../tests/unit/dispatcher/mod.rs"]
mod tests;
