//! Auto-squash keeps depth AND durable storage bounded on the max_depth=8 tier.
//!
//! The existing squash tests assert the loose `<= 100` default-tier bound, which
//! is vacuous here (this tier configures `auto_squash_max_depth: 8`). These
//! assert the tight, tier-correct invariants: depth settles near the configured
//! max, and repeatedly overwriting ONE file does not grow durable storage
//! linearly (superseded layers are squashed + reclaimed).

use anyhow::Result;
use eos_protocol::ops;
use serde_json::json;

use crate::support::{as_i64, live_pool_or_skip, wait_for_active_leases};

#[test]
fn auto_squash_bounds_depth_to_configured_max() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    for version in 0..30 {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": "squash-bounds/depth.txt", "content": format!("v{version}\n"), "overwrite": true}),
        )?;
    }
    // max_depth=8 plus at most one in-flight publish before the squash lands.
    let metrics = wait_for_active_leases(&lease, 0)?;
    assert!(
        as_i64(&metrics, "manifest_depth")? <= 10,
        "auto-squash must hold depth near the configured max of 8: {metrics}"
    );
    Ok(())
}

#[test]
fn repeated_overwrite_keeps_storage_bounded() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let baseline = as_i64(&lease.call_ok(ops::API_LAYER_METRICS, json!({}))?, "storage_bytes")?;
    for version in 0..60 {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": "squash-bounds/churn.txt", "content": format!("{version:0512}"), "overwrite": true}),
        )?;
    }
    let after = wait_for_active_leases(&lease, 0)?;
    assert!(
        as_i64(&after, "manifest_depth")? <= 10,
        "overwrite churn must stay squashed: {after}"
    );
    // 60 overwrites of one ~512B file must not grow durable storage linearly
    // (that would be a layer/disk leak); squash + GC keep it bounded.
    let storage = as_i64(&after, "storage_bytes")?;
    assert!(
        storage < baseline + 200_000,
        "repeated overwrite leaked durable storage: baseline={baseline} after={storage}"
    );
    Ok(())
}

#[test]
fn squash_reclaims_superseded_layer_dirs() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    for version in 0..40 {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": "squash-bounds/gc.txt", "content": format!("v{version}\n"), "overwrite": true}),
        )?;
    }
    // The real GC oracle (the hard-coded orphan_layer_count==0 verifies nothing):
    // after 40 overwrites the on-disk layer dirs collapse toward the live manifest
    // — squash folded them AND the superseded tail was reclaimed, not orphaned.
    let metrics = wait_for_active_leases(&lease, 0)?;
    let layer_dirs = as_i64(&metrics, "layer_dirs")?;
    let referenced = as_i64(&metrics, "referenced_layers")?;
    assert!(
        layer_dirs < 40,
        "squash + GC must fold 40 overwrites well below one dir per write: {metrics}"
    );
    assert!(
        layer_dirs <= referenced + 4,
        "on-disk layer dirs must collapse toward the live manifest (no orphan accumulation): {metrics}"
    );
    Ok(())
}
