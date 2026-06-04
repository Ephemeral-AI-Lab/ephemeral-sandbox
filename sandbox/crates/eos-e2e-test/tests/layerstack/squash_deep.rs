use anyhow::Result;
use eos_protocol::ops;
use serde_json::json;

use crate::common::{as_i64, as_str, live_pool_or_skip};

#[test]
fn deep_stack_repeated_squash() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    for version in 0..220 {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({
                "path": "pressure-squash/repeated.txt",
                "content": format!("deep-{version}\n"),
                "overwrite": true
            }),
        )?;
    }
    let read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "pressure-squash/repeated.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "deep-219\n");
    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert!(
        as_i64(&metrics, "manifest_depth")? <= 100,
        "repeated auto-squash should keep depth bounded: {metrics}"
    );
    Ok(())
}

#[test]
fn squash_storage_no_orphan() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    for version in 0..125 {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({
                "path": format!("pressure-squash/orphans-{version}.txt"),
                "content": "x\n",
                "overwrite": true
            }),
        )?;
    }
    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&metrics, "orphan_layer_count")?,
        0,
        "no orphan layers expected: {metrics}"
    );
    assert_eq!(
        as_i64(&metrics, "missing_layer_count")?,
        0,
        "no missing layers expected: {metrics}"
    );
    Ok(())
}
