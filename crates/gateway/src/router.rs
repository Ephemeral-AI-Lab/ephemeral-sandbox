use std::time::Instant;

use protocol::catalog::HostVerb;
use protocol::ProtocolErrorKind;
use serde_json::{json, Value};

use crate::catalog::{Catalog, Route, Visibility};
use crate::engine::Engine;
use crate::wire::{elapsed_us, error_response_for, ok_response, ClientRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    Client,
    Operator,
}

impl Surface {
    const fn allows(self, visibility: Visibility) -> bool {
        match visibility {
            Visibility::Public => true,
            Visibility::Operator => matches!(self, Self::Operator),
            Visibility::Internal | Visibility::Test => false,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Operator => "operator",
        }
    }
}

pub(crate) fn handle(
    catalog: &Catalog,
    engine: &dyn Engine,
    surface: Surface,
    request: &ClientRequest,
) -> Value {
    let Some(entry) = catalog.lookup(&request.op) else {
        record_route_event(
            engine,
            request,
            "route_rejected",
            json!({
                "op": request.op,
                "route": "rejected",
                "surface": surface.label(),
                "error_kind": ProtocolErrorKind::UnknownOp.as_str(),
            }),
        );
        return error_response_for(
            request,
            ProtocolErrorKind::UnknownOp.as_str(),
            &format!("unknown op: {}", request.op),
        );
    };
    if !surface.allows(entry.visibility) {
        record_route_event(
            engine,
            request,
            "route_rejected",
            json!({
                "op": entry.name,
                "route": "rejected",
                "surface": surface.label(),
                "visibility": entry.visibility.label(),
                "error_kind": ProtocolErrorKind::Forbidden.as_str(),
            }),
        );
        return error_response_for(
            request,
            ProtocolErrorKind::Forbidden.as_str(),
            &format!("op {} is not served on this socket", entry.name),
        );
    }
    match entry.route {
        Route::Daemon => forward(
            engine,
            request,
            entry.mutates_state,
            entry.family,
            "daemon",
            entry.visibility.label(),
        ),
        Route::Host(verb) => {
            record_route_event(
                engine,
                request,
                "route_selected",
                json!({
                    "op": entry.name,
                    "route": "host",
                    "visibility": entry.visibility.label(),
                    "family": entry.family,
                    "mutates_state": entry.mutates_state,
                }),
            );
            host_call(engine, verb, request)
        }
    }
}

fn record_route_event(engine: &dyn Engine, request: &ClientRequest, event: &str, details: Value) {
    if let Some(sandbox_id) = request.sandbox_id.as_deref() {
        engine.record_trace_event(sandbox_id, &request.trace, "gateway.route", event, details);
    }
}

fn forward(
    engine: &dyn Engine,
    request: &ClientRequest,
    mutates_state: bool,
    family: &str,
    route: &str,
    visibility: &str,
) -> Value {
    let Some(sandbox_id) = request.sandbox_id.as_deref() else {
        return error_response_for(
            request,
            ProtocolErrorKind::InvalidRequest.as_str(),
            "sandbox_id is required for this op",
        );
    };
    let mut trace = request.trace.clone();
    trace.push_gateway_event(
        "gateway.route",
        "route_selected",
        json!({
            "op": request.op,
            "sandbox_id": sandbox_id,
            "route": route,
            "visibility": visibility,
            "family": family,
            "mutates_state": mutates_state,
        }),
    );
    trace.push_gateway_event(
        "gateway.route",
        "engine_forward_started",
        json!({
            "op": request.op,
            "sandbox_id": sandbox_id,
            "family": family,
            "mutates_state": mutates_state,
        }),
    );
    let trace_for_result = trace.clone();
    let started = Instant::now();
    match engine.forward(host::HostForwardRequest {
        sandbox_id,
        mutates_state,
        family,
        op: &request.op,
        invocation_id: &request.invocation_id,
        args: &request.args,
        trace,
    }) {
        Some(Ok(mut response)) => {
            engine.record_trace_event(
                sandbox_id,
                &trace_for_result,
                "gateway.route",
                "engine_forward_finished",
                json!({
                    "op": request.op,
                    "sandbox_id": sandbox_id,
                    "family": family,
                    "mutates_state": mutates_state,
                    "duration_us": elapsed_us(started),
                }),
            );
            host::strip_trace_sidecar(&mut response);
            response
        }
        Some(Err(err)) => {
            let (kind, message) = match err {
                host::ForwardError::TraceUnavailable(e) => ("trace_unavailable", e.to_string()),
                host::ForwardError::UncertainOutcome(m) => ("uncertain_outcome", m),
                host::ForwardError::SandboxUnavailable(m) => ("sandbox_unavailable", m),
            };
            engine.record_trace_event(
                sandbox_id,
                &trace_for_result,
                "gateway.route",
                "engine_forward_failed",
                json!({"op": request.op, "sandbox_id": sandbox_id, "error_kind": kind, "duration_us": elapsed_us(started)}),
            );
            error_response_for(request, kind, &message)
        }
        None => unknown_sandbox(request, sandbox_id),
    }
}

fn host_call(engine: &dyn Engine, verb: HostVerb, request: &ClientRequest) -> Value {
    match verb {
        HostVerb::Acquire => match engine.acquire(&request.trace, &request.args) {
            Ok(sandbox_id) => ok_response(request, json!({"sandbox_id": sandbox_id})),
            Err(err) => error_response_for(
                request,
                "trace_unavailable",
                &format!("acquire failed: {err:#}"),
            ),
        },
        HostVerb::List => ok_response(request, json!({"sandboxes": engine.list()})),
        HostVerb::Release => {
            let Some(sandbox_id) = request.sandbox_id.as_deref() else {
                return error_response_for(
                    request,
                    ProtocolErrorKind::InvalidRequest.as_str(),
                    "sandbox_id is required for this op",
                );
            };
            match engine.release(sandbox_id, &request.trace, &request.args) {
                Ok(true) => ok_response(request, json!({"sandbox_id": sandbox_id})),
                Ok(false) => unknown_sandbox(request, sandbox_id),
                Err(err) => error_response_for(
                    request,
                    "trace_unavailable",
                    &format!("release failed: {err:#}"),
                ),
            }
        }
        HostVerb::Status => {
            let Some(sandbox_id) = request.sandbox_id.as_deref() else {
                return error_response_for(
                    request,
                    ProtocolErrorKind::InvalidRequest.as_str(),
                    "sandbox_id is required for this op",
                );
            };
            match engine.status(sandbox_id) {
                Some(status) => ok_response(request, status),
                None => unknown_sandbox(request, sandbox_id),
            }
        }
        HostVerb::TraceRequests => trace_response(
            request,
            engine.trace_requests(&request.trace, &request.args),
        ),
        HostVerb::TraceShow => {
            trace_response(request, engine.trace_show(&request.trace, &request.args))
        }
        HostVerb::TraceVerify => {
            trace_response(request, engine.trace_verify(&request.trace, &request.args))
        }
        HostVerb::ImageProfilesList => host_value_response(
            request,
            engine.image_profiles_list(&request.trace, &request.args),
        ),
        HostVerb::ImageList => {
            host_value_response(request, engine.image_list(&request.trace, &request.args))
        }
        HostVerb::ImagePull => {
            host_value_response(request, engine.image_pull(&request.trace, &request.args))
        }
        HostVerb::ContainerList => host_value_response(
            request,
            engine.container_list(&request.trace, &request.args),
        ),
        HostVerb::ContainerStart => host_value_response(
            request,
            engine.container_start(&request.trace, &request.args),
        ),
        HostVerb::ContainerAdopt => host_value_response(
            request,
            engine.container_adopt(&request.trace, &request.args),
        ),
        HostVerb::ContainerStop => host_value_response(
            request,
            engine.container_stop(&request.trace, &request.args),
        ),
        HostVerb::ContainerRemove => host_value_response(
            request,
            engine.container_remove(&request.trace, &request.args),
        ),
    }
}

fn host_value_response(request: &ClientRequest, result: anyhow::Result<Value>) -> Value {
    match result {
        Ok(value) => ok_response(request, value),
        Err(err) => error_response_for(request, host_error_kind(&err), &err.to_string()),
    }
}

fn host_error_kind(err: &anyhow::Error) -> &'static str {
    let message = err.to_string();
    if message.ends_with(" is required") || message.ends_with(" must be a non-empty string") {
        ProtocolErrorKind::InvalidRequest.as_str()
    } else {
        "host_operation_failed"
    }
}

fn trace_response(request: &ClientRequest, result: anyhow::Result<Value>) -> Value {
    match result {
        Ok(value) => ok_response(request, value),
        Err(err) => error_response_for(request, trace_error_kind(&err), &err.to_string()),
    }
}

fn trace_error_kind(err: &anyhow::Error) -> &'static str {
    let message = err.to_string();
    if message.ends_with(" is required") || message.ends_with(" must be a non-empty string") {
        return ProtocolErrorKind::InvalidRequest.as_str();
    }
    "trace_unavailable"
}

fn unknown_sandbox(request: &ClientRequest, sandbox_id: &str) -> Value {
    error_response_for(
        request,
        "unknown_sandbox",
        &format!("unknown sandbox: {sandbox_id}"),
    )
}
