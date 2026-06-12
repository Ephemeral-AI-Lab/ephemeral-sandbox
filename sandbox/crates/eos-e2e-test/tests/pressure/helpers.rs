use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use eos_e2e_test::{client::ProtocolClient, next_invocation_id, NodeLease, NodePool};
use eos_operation::core::catalog;
use eos_trace::ResourceStatsKind;
use serde_json::Value;

use crate::support::{as_str, envelope_meta, envelope_result, trace_record};

pub(crate) fn pressure_levels(pool: &NodePool) -> Result<Vec<usize>> {
    let levels = pool.workload().concurrency_levels.clone();
    ensure!(
        levels == vec![1, 3, 6, 12],
        "pressure workload.concurrency_levels must be the spec ladder [1, 3, 6, 12], got {levels:?}"
    );
    Ok(levels)
}

pub(crate) fn workload_timeout_s(pool: &NodePool) -> u64 {
    pool.workload().timeout.as_secs().max(1)
}

pub(crate) fn request_with_identity(
    client: &ProtocolClient,
    op: &str,
    root: &str,
    caller_id: &str,
    args: Value,
) -> Result<Value> {
    let mut args = match args {
        Value::Object(args) => args,
        other => bail!("pressure request args must be an object, got {other}"),
    };
    args.entry("layer_stack_root".to_owned())
        .or_insert_with(|| Value::String(root.to_owned()));
    args.entry("caller_id".to_owned())
        .or_insert_with(|| Value::String(caller_id.to_owned()));
    Ok(client.request(op, &next_invocation_id(), &Value::Object(args))?)
}

pub(crate) fn response_result(response: &Value) -> Result<&Value> {
    if response.get("status").and_then(Value::as_str).is_some() {
        envelope_result(response)
    } else {
        Ok(response)
    }
}

pub(crate) fn optional_response_result(response: &Value) -> Result<Option<&Value>> {
    if response.get("status").and_then(Value::as_str).is_some() {
        Ok(response.get("result"))
    } else if response.get("error").is_some() {
        Ok(None)
    } else {
        Ok(Some(response))
    }
}

pub(crate) fn result_committed(result: &Value) -> bool {
    matches!(
        result.get("status").and_then(Value::as_str),
        Some("committed" | "ok")
    ) || result.get("success").and_then(Value::as_bool) == Some(true)
}

pub(crate) fn result_structured(result: &Value) -> bool {
    result.get("status").is_some()
        || result.get("conflict").is_some()
        || result.get("error").is_some()
        || result.get("success").and_then(Value::as_bool).is_some()
}

pub(crate) fn finalize_foreground_command_wire(
    lease: &NodeLease<'_>,
    response: Value,
    deadline: Instant,
) -> Result<(Value, Value)> {
    let result = response_result(&response)?.clone();
    if as_str(&result, "status")? != "running" {
        return Ok((response, result));
    }

    let command_id = as_str(&result, "command_id")?.to_owned();
    loop {
        let progress = lease.call(
            catalog::SANDBOX_COMMAND_POLL,
            serde_json::json!({"command_id": &command_id, "last_n_lines": 50}),
        )?;
        let progress_result = response_result(&progress)?.clone();
        if as_str(&progress_result, "status")? != "running" {
            return Ok(strip_result_command_id(progress, progress_result));
        }
        if Instant::now() >= deadline {
            bail!("foreground command {command_id} did not finalize before deadline: {progress}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn finalize_foreground_command_result(
    lease: &NodeLease<'_>,
    response: Value,
    deadline: Instant,
) -> Result<Value> {
    Ok(finalize_foreground_command_wire(lease, response, deadline)?.1)
}

pub(crate) fn ensure_response_step(response: &Value, kind: &str) -> Result<()> {
    let meta = envelope_meta(response)?;
    ensure!(
        meta.steps.iter().any(|step| step.kind == kind),
        "response meta should include {kind} step: {response}"
    );
    Ok(())
}

pub(crate) fn trace_resource_number(
    response: &Value,
    stats_kind: ResourceStatsKind,
    source: &str,
    path: &[&str],
) -> Result<f64> {
    let record = trace_record(response)?;
    record
        .resources
        .iter()
        .filter(|resource| resource.meta.stats_kind == stats_kind && resource.meta.source == source)
        .filter_map(|resource| nested_number(&resource.payload.value, path))
        .next()
        .with_context(|| {
            format!(
                "trace missing numeric resource {source}.{} in {record:?}",
                path.join(".")
            )
        })
}

pub(crate) fn ensure_trace_resource(
    response: &Value,
    stats_kind: ResourceStatsKind,
    source: &str,
) -> Result<()> {
    let record = trace_record(response)?;
    ensure!(
        record.resources.iter().any(|resource| {
            resource.meta.stats_kind == stats_kind && resource.meta.source == source
        }),
        "trace missing {stats_kind:?} resource for {source}: {record:?}"
    );
    Ok(())
}

fn strip_result_command_id(mut response: Value, result: Value) -> (Value, Value) {
    let Some(result_object) = response.get_mut("result").and_then(Value::as_object_mut) else {
        return (response, result);
    };
    result_object.remove("command_id");
    let result = Value::Object(result_object.clone());
    (response, result)
}

fn nested_number(value: &Value, path: &[&str]) -> Option<f64> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_f64)
}
