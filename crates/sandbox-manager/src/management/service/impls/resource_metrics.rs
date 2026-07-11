use std::time::{SystemTime, UNIX_EPOCH};

use sandbox_operation_contract::{error, OperationRequest, OperationResponse};
use serde_json::{json, Map, Value};

use crate::operations::{ManagerServices, ResourceSample, MAX_RESOURCE_HISTORY_MS};
use crate::router::forward_sandbox_request;
use crate::{ManagerError, SandboxId, SandboxResourceMetrics};

const SANDBOX_SCOPE: &str = "sandbox";
const DEFAULT_RESOURCE_WINDOW_MS: i64 = 60_000;

pub(crate) fn dispatch_resource_metrics(
    services: &ManagerServices,
    request: &OperationRequest,
) -> OperationResponse {
    let scope = match request.optional_string("scope") {
        Ok(scope) => scope.unwrap_or_else(|| SANDBOX_SCOPE.to_owned()),
        Err(response) => return response,
    };
    if scope != SANDBOX_SCOPE {
        return match forward_sandbox_request(services, request.clone()) {
            Ok(response) => response,
            Err(error) => error.into_response(),
        };
    }

    let window_ms = match resource_window_ms(request) {
        Ok(window_ms) => window_ms,
        Err(response) => return response,
    };
    let id = match sandbox_id(request) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let samples = match resource_samples(services, id, window_ms) {
        Ok(samples) => samples,
        Err(error) => return error.into_response(),
    };
    OperationResponse::ok(json!({
        "view": "cgroup",
        "scope": SANDBOX_SCOPE,
        "series": series_value(samples),
    }))
}

fn resource_window_ms(request: &OperationRequest) -> Result<i64, OperationResponse> {
    let window_ms = request
        .optional_u64("window_ms")?
        .unwrap_or(DEFAULT_RESOURCE_WINDOW_MS as u64);
    if window_ms > MAX_RESOURCE_HISTORY_MS as u64 {
        return Err(OperationResponse::fault(
            error::INVALID_REQUEST,
            format!("window_ms exceeds max ({MAX_RESOURCE_HISTORY_MS})"),
        ));
    }
    Ok(window_ms as i64)
}

pub(crate) fn latest_resource_value(
    services: &ManagerServices,
    id: &SandboxId,
) -> Result<Value, ManagerError> {
    Ok(series_value(resource_samples(
        services,
        id.clone(),
        DEFAULT_RESOURCE_WINDOW_MS,
    )?)
    .into_iter()
    .last()
    .unwrap_or(Value::Null))
}

fn resource_samples(
    services: &ManagerServices,
    id: SandboxId,
    window_ms: i64,
) -> Result<Vec<ResourceSample>, ManagerError> {
    let metrics = services.runtime.read_sandbox_resource_metrics(&id)?;
    Ok(services.resource_history.record(
        id,
        ResourceSample {
            sampled_at_unix_ms: now_unix_ms(),
            metrics,
        },
        window_ms,
    ))
}

fn sandbox_id(request: &OperationRequest) -> Result<SandboxId, OperationResponse> {
    let sandbox_id = match &request.scope {
        sandbox_operation_contract::OperationScope::Sandbox { sandbox_id } => sandbox_id,
        sandbox_operation_contract::OperationScope::System => {
            return Err(OperationResponse::fault(
                error::INVALID_REQUEST,
                "resource metrics require sandbox scope",
            ));
        }
    };
    SandboxId::new(sandbox_id.clone()).map_err(ManagerError::into_response)
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn series_value(samples: Vec<ResourceSample>) -> Vec<Value> {
    let mut previous = None;
    samples
        .into_iter()
        .map(|sample| {
            let value = sample_value(sample, previous);
            previous = Some(sample);
            value
        })
        .collect()
}

fn sample_value(sample: ResourceSample, previous: Option<ResourceSample>) -> Value {
    let sample_delta_ms = previous.map(|prior| {
        sample
            .sampled_at_unix_ms
            .saturating_sub(prior.sampled_at_unix_ms)
    });
    let deltas = previous.map_or_else(Map::new, |prior| {
        let mut deltas = Map::new();
        insert_counter_delta(
            &mut deltas,
            "cpu_usec",
            sample.metrics.cpu_usage_usec,
            prior.metrics.cpu_usage_usec,
        );
        insert_counter_delta(
            &mut deltas,
            "io_rbytes",
            sample.metrics.io_read_bytes,
            prior.metrics.io_read_bytes,
        );
        insert_counter_delta(
            &mut deltas,
            "io_wbytes",
            sample.metrics.io_write_bytes,
            prior.metrics.io_write_bytes,
        );
        deltas
    });
    json!({
        "ts": sample.sampled_at_unix_ms,
        "sample_delta_ms": sample_delta_ms,
        "metrics": metrics_value(sample.metrics),
        "deltas": deltas,
    })
}

fn metrics_value(metrics: SandboxResourceMetrics) -> Value {
    let mut values = Map::new();
    values.insert("metrics_source".to_owned(), json!("docker_engine"));
    insert_metric(&mut values, "cpu_usec", metrics.cpu_usage_usec);
    insert_metric(&mut values, "io_rbytes", metrics.io_read_bytes);
    insert_metric(&mut values, "io_wbytes", metrics.io_write_bytes);
    if let Some(memory_current_bytes) = metrics.memory_current_bytes {
        values.insert("mem_cur".to_owned(), json!(memory_current_bytes));
    }
    if let Some(memory_limit_bytes) = metrics.memory_limit_bytes {
        values.insert("mem_max".to_owned(), json!(memory_limit_bytes));
    }
    Value::Object(values)
}

fn insert_counter_delta(
    values: &mut Map<String, Value>,
    key: &str,
    current: Option<u64>,
    previous: Option<u64>,
) {
    if let (Some(current), Some(previous)) = (current, previous) {
        values.insert(key.to_owned(), json!(current.saturating_sub(previous)));
    }
}

fn insert_metric(values: &mut Map<String, Value>, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        values.insert(key.to_owned(), json!(value));
    }
}
