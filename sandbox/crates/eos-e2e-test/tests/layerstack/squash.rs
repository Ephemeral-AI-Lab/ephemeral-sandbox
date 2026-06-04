use anyhow::{Context, Result};
use eos_e2e_test::audit::section;
use eos_e2e_test::cas::looks_like_sha256;
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::common::{as_i64, as_str, live_pool_or_skip};

fn grow_past_auto_squash(lease: &eos_e2e_test::NodeLease<'_>, path: &str) -> Result<Value> {
    let mut last = Value::Null;
    for version in 0..105 {
        last = lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": path, "content": format!("version-{version}\n"), "overwrite": true}),
        )?;
    }
    Ok(last)
}

#[test]
fn auto_squash_triggers_past_depth() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let mut audit = lease.audit_tap()?;
    grow_past_auto_squash(&lease, "squash/depth.txt")?;
    audit.collect()?;
    assert!(
        audit.any("layer_stack.squash_triggered"),
        "auto-squash should emit a trigger event after depth pressure: {:?}",
        audit.events()
    );
    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert!(
        as_i64(&metrics, "manifest_depth")? <= 100,
        "auto-squash should keep depth bounded: {metrics}"
    );
    Ok(())
}

#[test]
fn checkpoint_layer_reduces_result_depth() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let mut audit = lease.audit_tap()?;
    grow_past_auto_squash(&lease, "squash/checkpoint.txt")?;
    audit.collect()?;
    let completed = audit
        .first("layer_stack.squash_completed")
        .context("layer_stack.squash_completed event")?;
    let layer_stack = section(completed, "layer_stack").context("layer_stack section")?;
    let input = layer_stack
        .get("squash_input_layers")
        .and_then(Value::as_i64)
        .context("squash_input_layers")?;
    let output = layer_stack
        .get("squash_result_layers")
        .and_then(Value::as_i64)
        .context("squash_result_layers")?;
    assert!(
        output < input,
        "squash should replace many inputs with a checkpoint: {completed}"
    );
    Ok(())
}

#[test]
fn head_readable_after_squash() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    grow_past_auto_squash(&lease, "squash/head.txt")?;
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": "squash/head.txt"}))?;
    assert_eq!(as_str(&read, "content")?, "version-104\n");
    Ok(())
}

#[test]
fn squash_cas_hash_is_protocol_visible() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let mut audit = lease.audit_tap()?;
    grow_past_auto_squash(&lease, "squash/hash.txt")?;
    audit.collect()?;
    let completed = audit
        .first("layer_stack.squash_completed")
        .context("layer_stack.squash_completed event")?;
    let layer_stack = section(completed, "layer_stack").context("layer_stack section")?;
    assert!(
        layer_stack
            .get("manifest_root_hash")
            .and_then(Value::as_str)
            .is_some_and(looks_like_sha256),
        "squash audit should include a CAS-shaped manifest hash: {completed}"
    );
    Ok(())
}

#[test]
fn squash_not_raced_single_client() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let mut audit = lease.audit_tap()?;
    grow_past_auto_squash(&lease, "squash/no-race.txt")?;
    audit.collect()?;
    assert!(
        !audit.any("layer_stack.squash_failed"),
        "single-client growth should not produce squash_failed: {:?}",
        audit.events()
    );
    Ok(())
}
