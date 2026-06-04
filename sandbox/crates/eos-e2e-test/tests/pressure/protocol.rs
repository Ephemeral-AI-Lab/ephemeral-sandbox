use std::sync::{Arc, Barrier};
use std::thread;

use anyhow::{Context, Result};
use eos_e2e_test::{live_pool, NodePool};
use eos_protocol::ops;
use serde_json::{json, Value};

fn live_pool_or_skip() -> Result<Option<Arc<NodePool>>> {
    let Some(pool) = live_pool()? else {
        eprintln!("skipping live eos-e2e-test; enable with `--features e2e`");
        return Ok(None);
    };
    Ok(Some(pool))
}

fn as_bool(value: &Value, key: &str) -> Result<bool> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .with_context(|| format!("{key} missing or not bool in {value}"))
}

fn as_i64(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("{key} missing or not i64 in {value}"))
}

fn as_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("{key} missing or not string in {value}"))
}

#[test]
fn overlay_exec_publishes_file_back_to_layerstack() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "mkdir -p e2e_overlay && printf overlay-ok > e2e_overlay/from_exec.txt",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,
            "max_output_tokens": 2000
        }),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok");
    assert_eq!(as_i64(&exec, "exit_code")?, 0);

    let read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "e2e_overlay/from_exec.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "overlay-ok");

    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&metrics, "active_leases")?,
        0,
        "completed overlay command should not leak leases: {metrics}"
    );
    Ok(())
}

#[test]
fn layerstack_auto_squash_keeps_depth_bounded() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    for version in 0..105 {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({
                "path": "e2e_squash/depth.txt",
                "content": format!("version-{version}\n"),
                "overwrite": true
            }),
        )?;
    }

    let read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "e2e_squash/depth.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "version-104\n");

    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert!(
        as_i64(&metrics, "manifest_depth")? <= 100,
        "auto-squash should keep manifest depth at or below the operational target: {metrics}"
    );
    assert_eq!(as_i64(&metrics, "active_leases")?, 0);
    Ok(())
}

#[test]
fn occ_merges_concurrent_disjoint_protocol_writes() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let client = lease.client().clone();
    let root = lease.root().to_owned();
    let agent_id = lease.agent_id().to_owned();
    let barrier = Arc::new(Barrier::new(8));

    let handles: Vec<_> = (0..8)
        .map(|index| {
            let client = client.clone();
            let root = root.clone();
            let agent_id = agent_id.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Result<Value> {
                barrier.wait();
                client.request(
                    ops::API_V1_WRITE_FILE,
                    &format!("occ-merge-{index}"),
                    &json!({
                        "layer_stack_root": root,
                        "agent_id": agent_id,
                        "path": format!("e2e_occ/file-{index}.txt"),
                        "content": format!("merge-{index}\n"),
                        "overwrite": true
                    }),
                )
            })
        })
        .collect();

    for handle in handles {
        let response = handle.join().expect("writer thread panicked")?;
        assert!(
            as_bool(&response, "success")?,
            "disjoint write should publish successfully: {response}"
        );
    }

    for index in 0..8 {
        let read = lease.call_ok(
            ops::API_V1_READ_FILE,
            json!({"path": format!("e2e_occ/file-{index}.txt")}),
        )?;
        assert_eq!(as_str(&read, "content")?, format!("merge-{index}\n"));
    }
    Ok(())
}
