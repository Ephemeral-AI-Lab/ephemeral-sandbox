use anyhow::{bail, ensure, Result};
use eos_e2e_test::{client::ProtocolClient, next_invocation_id, NodePool};
use serde_json::Value;

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
    client.request(op, &next_invocation_id(), &Value::Object(args))
}
