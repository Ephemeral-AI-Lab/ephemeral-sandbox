//! The `OpenAPI` document, assembled from the backend DTOs' JSON schemas plus
//! the route table, and served at `GET /openapi.json`.
//!
//! Component schemas are generated from the `JsonSchema` derives on the public
//! DTOs (one `schemars` openapi3 generator), so the contract tests pin the
//! request/response shapes against the same types the handlers (de)serialize.

use axum::Json;
use schemars::gen::{SchemaGenerator, SchemaSettings};
use schemars::JsonSchema;
use serde_json::{json, Value};

use eos_backend_types::{
    AgentRunStat, ApiRunStatus, CorrectnessStats, CreateUserRequest, CreateUserRequestResponse,
    EventRecord, PerformanceStats, RunRecord, SandboxView, UserRequestDetail,
};
use eos_types::{AgentRun, Task};

/// `GET /openapi.json` — the assembled `OpenAPI` 3.0 document.
pub(crate) async fn openapi_doc() -> Json<Value> {
    Json(document())
}

/// Build the document: component schemas from the DTO derives, plus the static
/// path table mirroring [`build_router`](crate::build_router).
fn document() -> Value {
    let mut generator = SchemaGenerator::new(SchemaSettings::openapi3());
    reg::<CreateUserRequest>(&mut generator);
    reg::<CreateUserRequestResponse>(&mut generator);
    reg::<RunRecord>(&mut generator);
    reg::<UserRequestDetail>(&mut generator);
    reg::<ApiRunStatus>(&mut generator);
    reg::<EventRecord>(&mut generator);
    reg::<SandboxView>(&mut generator);
    reg::<PerformanceStats>(&mut generator);
    reg::<CorrectnessStats>(&mut generator);
    reg::<AgentRunStat>(&mut generator);
    reg::<Task>(&mut generator);
    reg::<AgentRun>(&mut generator);

    let schemas = serde_json::to_value(generator.definitions()).unwrap_or_else(|_| json!({}));

    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "EphemeralOS backend API",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "paths": paths(),
        "components": { "schemas": schemas },
    })
}

/// Register a DTO (and its transitive references) into the component schemas.
fn reg<T: JsonSchema>(generator: &mut SchemaGenerator) {
    let _ = generator.subschema_for::<T>();
}

/// `$ref` to a component schema by type name.
fn schema_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

/// A JSON response body of the given component schema.
fn json_body(schema_name: &str) -> Value {
    json!({ "content": { "application/json": { "schema": schema_ref(schema_name) } } })
}

/// The static path table. Resource names are plural and id paths use
/// `/collection/{id}` form (no `/collection={id}`).
fn paths() -> Value {
    json!({
        "/api/agent-core/requests": {
            "post": {
                "summary": "Accept a prompt and launch an agent-core run",
                "requestBody": json_body("CreateUserRequest"),
                "responses": { "202": json_body("CreateUserRequestResponse") },
            },
            "get": {
                "summary": "List backend run records",
                "responses": { "200": { "description": "page of run records" } },
            },
        },
        "/api/agent-core/requests/{request_id}": {
            "get": {
                "summary": "Backend lifecycle joined with the agent-core outcome",
                "responses": { "200": json_body("UserRequestDetail") },
            },
            "delete": {
                "summary": "Request backend-local cancellation",
                "responses": {
                    "202": { "description": "cancellation requested" },
                    "404": { "description": "no such user request" },
                    "409": { "description": "run already finished" },
                },
            },
        },
        "/api/agent-core/requests/{request_id}/events": {
            "get": {
                "summary": "Replay persisted milestone events",
                "responses": { "200": { "description": "milestone event records" } },
            },
        },
        "/api/agent-core/requests/{request_id}/stream": {
            "get": {
                "summary": "SSE-only milestone stream with replay from last_seq",
                "responses": { "200": { "description": "server-sent event stream" } },
            },
        },
        "/api/agent-core/requests/{request_id}/tasks": {
            "get": {
                "summary": "The request task tree from agent-core state",
                "responses": { "200": { "description": "tasks" } },
            },
        },
        "/api/agent-core/tasks/{task_id}": {
            "get": {
                "summary": "Task detail and related agent run",
                "responses": { "200": { "description": "task detail" } },
            },
        },
        "/api/agent-core/tasks/{task_id}/transcript": {
            "get": {
                "summary": "Model/tool transcript for a task",
                "responses": { "200": { "description": "transcript" } },
            },
        },
        "/api/agent-core/agent-runs/{agent_run_id}/messages": {
            "get": {
                "summary": "Raw node-local messages.jsonl bytes for an agent run",
                "responses": { "200": { "description": "application/x-ndjson messages" } },
            },
        },
        "/api/agent-core/agent-runs/{agent_run_id}/events": {
            "get": {
                "summary": "Replay node-local events.jsonl rows for an agent run",
                "responses": { "200": { "description": "agent-run node events" } },
            },
        },
        "/api/agent-core/agent-runs/{agent_run_id}/stream": {
            "get": {
                "summary": "SSE-only node-local event stream with replay from last_seq",
                "responses": { "200": { "description": "server-sent event stream" } },
            },
        },
        "/api/stats/performance": {
            "get": {
                "summary": "Timing and resource summaries",
                "responses": { "200": json_body("PerformanceStats") },
            },
        },
        "/api/stats/correctness": {
            "get": {
                "summary": "Correctness summaries from persisted outcomes and obs events",
                "responses": { "200": json_body("CorrectnessStats") },
            },
        },
        "/api/stats/agent-runs": {
            "get": {
                "summary": "Per-agent-run stats",
                "responses": { "200": { "description": "agent-run stats" } },
            },
        },
        "/api/stats/events": {
            "get": {
                "summary": "Normalized observability events",
                "responses": { "200": { "description": "observability events" } },
            },
        },
        "/api/sandboxes": {
            "get": {
                "summary": "List backend-owned sandboxes and lifecycle state",
                "responses": { "200": { "description": "sanitized sandbox views" } },
            },
        },
        "/api/sandboxes/{sandbox_id}": {
            "get": {
                "summary": "A sanitized sandbox view",
                "responses": { "200": json_body("SandboxView") },
            },
            "delete": {
                "summary": "Destroy a backend-owned sandbox when unreferenced",
                "responses": {
                    "204": { "description": "destroyed" },
                    "404": { "description": "unknown sandbox" },
                    "409": { "description": "sandbox is referenced (active or retained)" },
                },
            },
        },
    })
}
