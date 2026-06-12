use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use eos_operation::core::catalog;
use serde_json::{json, Value};

use crate::helpers::{pressure_levels, request_with_identity, workload_timeout_s};
use crate::support::{
    as_bool, as_i64, as_str, finalize_foreground_command, live_pool_or_skip, wait_for_active_leases,
};

#[test]
fn overlay_exec_publishes_file_back_to_layerstack() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "mkdir -p e2e_overlay && printf overlay-ok > e2e_overlay/from_exec.txt",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,}),
    )?;
    // Settle the yielded exec under emulation before asserting its terminal status.
    let exec = finalize_foreground_command(&lease, exec, Instant::now() + Duration::from_secs(15))?;
    assert_eq!(as_str(&exec, "status")?, "ok");
    assert_eq!(as_i64(&exec, "exit_code")?, 0);

    let read = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": "e2e_overlay/from_exec.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "overlay-ok");

    let metrics = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&metrics, "active_leases")?,
        0,
        "completed overlay command should not leak leases: {metrics}"
    );
    Ok(())
}

#[test]
fn ephemeral_exec_ladder_1_3_6_12() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let levels = pressure_levels(&pool)?;
    let timeout_s = workload_timeout_s(&pool);
    let lease = pool.acquire()?;

    for level in levels {
        let barrier = Arc::new(Barrier::new(level));
        let handles: Vec<_> = (0..level)
            .map(|index| {
                let client = lease.client().clone();
                let root = lease.root().to_owned();
                let caller_id = lease.caller_id().to_owned();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    request_with_identity(
                        &client,
                        catalog::SANDBOX_COMMAND_EXEC,
                        &root,
                        &caller_id,
                        json!({
                            "cmd": format!(
                                "mkdir -p pressure/ladder/exec/level-{level} && printf 'exec-level-{level}-item-{index}\\n' > pressure/ladder/exec/level-{level}/item-{index}.txt"
                            ),
                            "yield_time_ms": 1000,
                            "timeout_seconds": timeout_s,}),
                    )
                })
            })
            .collect();

        for handle in handles {
            let response = handle.join().expect("exec thread panicked")?;
            // Under emulation at higher concurrency the trivial command can outlast
            // the 1s yield and return status "running"; finalize it to its terminal
            // outcome (also publishing the upperdir before the read-back below).
            // Settle is a no-op for an already-terminal reply.
            let response = finalize_foreground_command(
                &lease,
                response,
                Instant::now() + Duration::from_secs(timeout_s + 5),
            )?;
            assert_eq!(
                as_str(&response, "status")?,
                "ok",
                "ephemeral exec should finish at level {level}: {response}"
            );
            assert_eq!(as_i64(&response, "exit_code")?, 0, "{response}");
        }

        for index in 0..level {
            let read = lease.call_ok(
                catalog::SANDBOX_FILE_READ,
                json!({"path": format!("pressure/ladder/exec/level-{level}/item-{index}.txt")}),
            )?;
            assert_eq!(
                as_str(&read, "content")?,
                format!("exec-level-{level}-item-{index}\n"),
                "ephemeral exec output should publish at level {level}: {read}"
            );
        }
        let metrics = wait_for_active_leases(&lease, 0)?;
        assert_eq!(
            as_i64(&metrics, "active_leases")?,
            0,
            "ephemeral exec ladder should release leases at level {level}: {metrics}"
        );
    }
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
            catalog::SANDBOX_FILE_WRITE,
            json!({
                "path": "e2e_squash/depth.txt",
                "content": format!("version-{version}\n"),
                "overwrite": true
            }),
        )?;
    }

    let read = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": "e2e_squash/depth.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "version-104\n");

    let metrics = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
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
    let caller_id = lease.caller_id().to_owned();
    let barrier = Arc::new(Barrier::new(8));

    let handles: Vec<_> = (0..8)
        .map(|index| {
            let client = client.clone();
            let root = root.clone();
            let caller_id = caller_id.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Result<Value> {
                barrier.wait();
                Ok(client.request(
                    catalog::SANDBOX_FILE_WRITE,
                    &format!("occ-merge-{index}"),
                    &json!({
                        "layer_stack_root": root,
                        "caller_id": caller_id,
                        "path": format!("e2e_occ/file-{index}.txt"),
                        "content": format!("merge-{index}\n"),
                        "overwrite": true
                    }),
                )?)
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
            catalog::SANDBOX_FILE_READ,
            json!({"path": format!("e2e_occ/file-{index}.txt")}),
        )?;
        assert_eq!(as_str(&read, "content")?, format!("merge-{index}\n"));
    }
    Ok(())
}

#[test]
fn occ_ladder_1_3_6_12() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let levels = pressure_levels(&pool)?;
    let lease = pool.acquire()?;

    for level in levels {
        run_disjoint_occ_level(&lease, level)?;
        run_same_path_occ_level(&lease, level)?;
        let metrics = wait_for_active_leases(&lease, 0)?;
        assert_eq!(
            as_i64(&metrics, "active_leases")?,
            0,
            "OCC ladder should not leak leases at level {level}: {metrics}"
        );
    }
    Ok(())
}

fn run_disjoint_occ_level(lease: &eos_e2e_test::NodeLease<'_>, level: usize) -> Result<()> {
    let barrier = Arc::new(Barrier::new(level));
    let handles: Vec<_> = (0..level)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = lease.caller_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                request_with_identity(
                    &client,
                    catalog::SANDBOX_FILE_WRITE,
                    &root,
                    &caller_id,
                    json!({
                        "path": format!("pressure/ladder/occ/disjoint/level-{level}/item-{index}.txt"),
                        "content": format!("occ-disjoint-level-{level}-item-{index}\n"),
                        "overwrite": true
                    }),
                )
            })
        })
        .collect();

    for handle in handles {
        let response = handle.join().expect("disjoint OCC writer panicked")?;
        assert!(
            as_bool(&response, "success")?,
            "disjoint OCC write should publish at level {level}: {response}"
        );
    }

    for index in 0..level {
        let read = lease.call_ok(
            catalog::SANDBOX_FILE_READ,
            json!({"path": format!("pressure/ladder/occ/disjoint/level-{level}/item-{index}.txt")}),
        )?;
        assert_eq!(
            as_str(&read, "content")?,
            format!("occ-disjoint-level-{level}-item-{index}\n"),
            "disjoint OCC readback should match at level {level}: {read}"
        );
    }
    Ok(())
}

fn run_same_path_occ_level(lease: &eos_e2e_test::NodeLease<'_>, level: usize) -> Result<()> {
    let barrier = Arc::new(Barrier::new(level));
    let handles: Vec<_> = (0..level)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = lease.caller_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                request_with_identity(
                    &client,
                    catalog::SANDBOX_FILE_WRITE,
                    &root,
                    &caller_id,
                    json!({
                        "path": format!("pressure/ladder/occ/conflict-level-{level}.txt"),
                        "content": format!("occ-conflict-level-{level}-writer-{index}\n"),
                        "overwrite": true
                    }),
                )
            })
        })
        .collect();

    let responses: Vec<Value> = handles
        .into_iter()
        .map(|handle| handle.join().expect("same-path OCC writer panicked"))
        .collect::<Result<_>>()?;
    assert!(
        responses.iter().any(|response| {
            response.get("status").and_then(Value::as_str) == Some("committed")
                || as_bool(response, "success").unwrap_or(false)
        }),
        "same-path OCC pressure should leave at least one committed writer at level {level}: {responses:?}"
    );
    for response in &responses {
        assert!(
            response.get("status").is_some()
                || response.get("conflict").is_some()
                || response.get("error").is_some(),
            "same-path OCC write should return a structured payload at level {level}: {response}"
        );
    }

    let read = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": format!("pressure/ladder/occ/conflict-level-{level}.txt")}),
    )?;
    let content = as_str(&read, "content")?;
    let expected_prefix = format!("occ-conflict-level-{level}-writer-");
    assert!(
        content.starts_with(&expected_prefix) && content.ends_with('\n'),
        "same-path OCC final content should be one whole writer payload at level {level}: {read}"
    );
    Ok(())
}
