use anyhow::Result;
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::common::{as_bool, as_i64, live_pool_or_skip};

#[test]
fn runtime_ready_handshake() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let ready = lease.call_ok(ops::API_RUNTIME_READY, json!({}))?;
    assert!(as_bool(&ready, "ready")?, "daemon must be ready: {ready}");
    assert!(
        ready
            .get("probes")
            .and_then(Value::as_array)
            .is_some_and(|probes| !probes.is_empty()),
        "runtime.ready must include probe details: {ready}"
    );
    Ok(())
}

#[test]
fn ensure_base_creates_single_base_layer() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&metrics, "manifest_depth")?,
        1,
        "fresh root should start at the base manifest: {metrics}"
    );
    assert_eq!(
        as_i64(&metrics, "referenced_layers")?,
        1,
        "fresh root should reference only the base layer: {metrics}"
    );
    Ok(())
}

#[test]
fn ensure_base_idempotent() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let before = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    let ensure = lease.call_ok(
        ops::API_ENSURE_WORKSPACE_BASE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(
        !as_bool(&ensure, "created")?,
        "second ensure should not rebuild an existing base: {ensure}"
    );
    let after = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&after, "manifest_depth")?,
        as_i64(&before, "manifest_depth")?,
        "idempotent ensure must preserve depth: before={before} after={after}"
    );
    Ok(())
}

#[test]
fn build_base_reset_rebuilds() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "setup/reset.txt", "content": "before\n", "overwrite": true}),
    )?;
    let rebuilt = lease.call_ok(
        ops::API_BUILD_WORKSPACE_BASE,
        json!({"workspace_root": lease.workspace_root(), "reset": true}),
    )?;
    assert!(as_bool(&rebuilt, "success")?);
    assert!(
        rebuilt
            .get("timings")
            .and_then(|timings| timings.get("api.workspace_base.total_s"))
            .is_some(),
        "build_base response should expose workspace-base timing: {rebuilt}"
    );
    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&metrics, "manifest_depth")?,
        1,
        "reset rebuild should collapse to a fresh base: {metrics}"
    );
    Ok(())
}

#[test]
fn workspace_binding_roundtrip() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let binding = lease.call_ok(ops::API_WORKSPACE_BINDING, json!({}))?;
    assert_eq!(
        binding["binding"]["workspace_root"],
        Value::String(lease.workspace_root().to_owned()),
        "workspace binding should round-trip the lease workspace root: {binding}"
    );
    Ok(())
}

#[test]
fn heartbeat_inflight_idle_zero() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let heartbeat = lease.call_ok(ops::API_V1_HEARTBEAT, json!({"invocation_ids": []}))?;
    assert!(as_bool(&heartbeat, "success")?);
    let inflight = lease.call_ok(ops::API_V1_INFLIGHT_COUNT, json!({}))?;
    assert_eq!(
        as_i64(&inflight, "count")?,
        0,
        "idle lease should not have background invocations: {inflight}"
    );
    Ok(())
}
