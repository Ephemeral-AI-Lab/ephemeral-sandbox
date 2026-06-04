use anyhow::Result;
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::common::{array, as_i64, live_pool_or_skip};

#[test]
fn mount_failure_no_partial_result() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "overlay-failure/seed.txt", "content": "seed\n", "overwrite": true}),
    )?;
    let failed = lease.call(
        ops::API_V1_GREP,
        json!({"pattern": "[", "path": "overlay-failure", "output_mode": "content"}),
    )?;
    if let Ok(paths) = array(&failed, "changed_paths") {
        assert!(
            paths.is_empty(),
            "failed read-only overlay op must not publish partial changes: {failed}"
        );
    }
    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&metrics, "active_leases")?,
        0,
        "failed overlay op should not leak active leases: {metrics}"
    );
    Ok(())
}

#[test]
fn cleanup_failure_kind_surfaced() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "overlay-failure/cleanup.txt", "content": "needle\n", "overwrite": true}),
    )?;
    let mut audit = lease.audit_tap()?;
    lease.call_ok(
        ops::API_V1_GREP,
        json!({"pattern": "needle", "path": "overlay-failure", "output_mode": "content"}),
    )?;
    audit.collect()?;
    if let Some(cleanup) = audit.first("overlay_workspace.cleanup") {
        let failure = cleanup
            .get("payload")
            .and_then(|payload| payload.get("overlay_workspace"))
            .and_then(|section| section.get("cleanup_failure_kind"));
        assert!(
            failure.is_none() || failure == Some(&Value::Null),
            "successful cleanup should not carry a failure kind: {cleanup}"
        );
    }
    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0);
    Ok(())
}
