use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use eos_e2e_test::next_invocation_id;
use eos_operation::core::catalog;
use serde_json::{json, Value};

use crate::helpers::{pressure_levels, request_with_identity};
use crate::support::{
    as_bool, as_i64, as_str, finalize_foreground_command, live_pool_or_skip, seed_base_files,
    wait_for_active_leases,
};

#[test]
fn n_concurrent_mixed_ops() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": "pressure/mixed-seed.txt", "content": "needle\n", "overwrite": true}),
    )?;
    let barrier = Arc::new(Barrier::new(10));
    let handles: Vec<_> = (0..10)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = lease.caller_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                let args = match index % 3 {
                    0 => json!({
                        "layer_stack_root": root,
                        "caller_id": caller_id,
                        "path": format!("pressure/mixed-{index}.txt"),
                        "content": format!("mixed-{index}\n"),
                        "overwrite": true
                    }),
                    1 => json!({
                        "layer_stack_root": root,
                        "caller_id": caller_id,
                        "path": "pressure/mixed-seed.txt"
                    }),
                    _ => json!({
                        "layer_stack_root": root,
                        "caller_id": caller_id,
                        "cmd": "printf pressure",
                        "yield_time_ms": 1000,
                        "timeout_seconds": 10
                    }),
                };
                let op = match index % 3 {
                    0 => catalog::SANDBOX_FILE_WRITE,
                    1 => catalog::SANDBOX_FILE_READ,
                    _ => catalog::SANDBOX_COMMAND_EXEC,
                };
                client.request(op, &next_invocation_id(), &args)
            })
        })
        .collect();
    for handle in handles {
        let response = handle.join().expect("mixed op thread panicked")?;
        // A concurrent exec can outlast the 1s yield under emulation and return
        // status "running" (no success/error yet, lease still held); settle just
        // those so the structured-payload check and the lease drain below are
        // deterministic. Write/read responses carry no "status" and pass through.
        let response = if response.get("status").and_then(Value::as_str) == Some("running") {
            finalize_foreground_command(&lease, response, Instant::now() + Duration::from_secs(15))?
        } else {
            response
        };
        assert!(
            as_bool(&response, "success").unwrap_or(false) || response.get("error").is_some(),
            "mixed pressure op should return a structured payload: {response}"
        );
    }
    // Poll: lease release is asynchronous, so a settled exec's lease may still be
    // draining the instant the loop ends.
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn file_ops_ladder_1_3_6_12() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let levels = pressure_levels(&pool)?;
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
                        catalog::SANDBOX_FILE_WRITE,
                        &root,
                        &caller_id,
                        json!({
                            "path": format!("pressure/ladder/file/level-{level}/item-{index}.txt"),
                            "content": format!("file-level-{level}-item-{index}\n"),
                            "overwrite": true
                        }),
                    )
                })
            })
            .collect();

        for handle in handles {
            let response = handle.join().expect("file writer thread panicked")?;
            assert!(
                as_bool(&response, "success")?,
                "file ladder write should commit at level {level}: {response}"
            );
        }

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
                        catalog::SANDBOX_FILE_READ,
                        &root,
                        &caller_id,
                        json!({
                            "path": format!("pressure/ladder/file/level-{level}/item-{index}.txt")
                        }),
                    )
                })
            })
            .collect();

        for (index, handle) in handles.into_iter().enumerate() {
            let response = handle.join().expect("file reader thread panicked")?;
            assert_eq!(
                as_str(&response, "content")?,
                format!("file-level-{level}-item-{index}\n"),
                "file ladder readback should match at level {level}: {response}"
            );
        }
        let metrics = wait_for_active_leases(&lease, 0)?;
        assert_eq!(
            as_i64(&metrics, "active_leases")?,
            0,
            "file ladder should not leak leases at level {level}: {metrics}"
        );
    }
    Ok(())
}

#[test]
fn write_storm_squash_under_load() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    for version in 0..115 {
        lease.call_ok(
            catalog::SANDBOX_FILE_WRITE,
            json!({
                "path": "pressure/storm.txt",
                "content": format!("storm-{version}\n"),
                "overwrite": true
            }),
        )?;
        if version % 20 == 0 {
            let read = lease.call_ok(
                catalog::SANDBOX_FILE_READ,
                json!({"path": "pressure/storm.txt"}),
            )?;
            assert!(as_str(&read, "content")?.contains("storm"));
        }
    }
    let read = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": "pressure/storm.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "storm-114\n");
    let metrics = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    assert!(
        as_i64(&metrics, "manifest_depth")? <= 100,
        "write storm should remain within auto-squash depth target: {metrics}"
    );
    Ok(())
}

fn timing_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    value
        .get("timings")
        .and_then(|timings| timings.get(key))
        .and_then(serde_json::Value::as_f64)
}

#[test]
fn concurrent_overlay_execs_share_lowerdir_storage_is_o1() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // Seed a large shared base into the lowerdir layer stack (4 sub-cap files;
    // the daemon caps one write at 2 MiB).
    let base_bytes = seed_base_files(&lease, "pressure/o1/base", 4, 1_000_000)? as i64;
    let before = wait_for_active_leases(&lease, 0)?;
    let storage_before = as_i64(&before, "storage_bytes")?;

    // N concurrent overlay execs each touch a tiny disjoint delta over the same
    // shared base. The mount(2) lowerdir is shared, not duplicated per lease.
    let level = 6;
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
                        "cmd": format!("mkdir -p pressure/o1 && printf d{index} > pressure/o1/delta-{index}.txt"),
                        "yield_time_ms": 1000,
                        "timeout_seconds": 30,}),
                )
            })
        })
        .collect();

    let mut max_upperdir = 0.0_f64;
    for handle in handles {
        let response = handle.join().expect("overlay exec thread panicked")?;
        // Finalize yielded ("running") execs to the finalized payload so both the
        // terminal status and the upperdir timing below are present under emulation.
        let response = finalize_foreground_command(
            &lease,
            response,
            Instant::now() + Duration::from_secs(35),
        )?;
        assert_eq!(
            as_str(&response, "status")?,
            "ok",
            "concurrent overlay exec should succeed: {response}"
        );
        if let Some(upperdir) = timing_f64(&response, "resource.command_exec.upperdir_tree_bytes") {
            max_upperdir = max_upperdir.max(upperdir);
        }
    }
    // No per-op copy-up under concurrency: each upperdir stays delta-sized.
    assert!(
        max_upperdir < 100_000.0,
        "concurrent overlay upperdir must stay delta-sized, not copy the {base_bytes}-byte base (got {max_upperdir})"
    );

    let after = wait_for_active_leases(&lease, 0)?;
    let storage_after = as_i64(&after, "storage_bytes")?;
    // Shared lowerdir is not duplicated per lease: storage grows by the small
    // deltas, not by N * base (which would be ~24MB for level=6, base=4MB).
    assert!(
        storage_after - storage_before < base_bytes,
        "concurrent execs over a shared base must not multiply lowerdir storage: before={storage_before} after={storage_after} base={base_bytes}"
    );
    Ok(())
}

/// `write_storm_squash_under_load` crosses the auto-squash threshold with
/// SEQUENTIAL writes. This variant keeps that guaranteed trigger — a driver
/// thread overwrites one path past the depth target — while a concurrent pool
/// hammers disjoint paths, so auto-squash runs *while* concurrent publishes
/// land. Squash must keep the manifest depth bounded, strand no superseded
/// layers, lose no data, and leak no leases under that race.
#[test]
fn concurrent_writes_during_squash_keep_manifest_bounded_and_coherent() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // > the default auto_squash_max_depth (100) so the driver guarantees squash.
    let driver_writes = 105;
    let fanout = 3;
    let rounds_each = 20;
    let barrier = Arc::new(Barrier::new(fanout + 1));

    let driver = {
        let client = lease.client().clone();
        let root = lease.root().to_owned();
        let caller_id = lease.caller_id().to_owned();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || -> Result<()> {
            barrier.wait();
            for version in 0..driver_writes {
                let response = request_with_identity(
                    &client,
                    catalog::SANDBOX_FILE_WRITE,
                    &root,
                    &caller_id,
                    json!({
                        "path": "pressure/squash-race/driver.txt",
                        "content": format!("driver-{version}\n"),
                        "overwrite": true
                    }),
                )?;
                // Concurrent disjoint publishes can move the manifest version
                // under a same-path overwrite; with the retry budget it commits,
                // but a structured conflict is also acceptable mid-race.
                assert!(
                    response.get("status").is_some() || response.get("error").is_some(),
                    "driver overwrite should return a structured payload: {response}"
                );
            }
            Ok(())
        })
    };

    let fanout_handles: Vec<_> = (0..fanout)
        .map(|writer| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = lease.caller_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Result<()> {
                barrier.wait();
                for round in 0..rounds_each {
                    let response = request_with_identity(
                        &client,
                        catalog::SANDBOX_FILE_WRITE,
                        &root,
                        &caller_id,
                        json!({
                            "path": format!("pressure/squash-race/w-{writer}/r-{round}.txt"),
                            "content": format!("w-{writer}-r-{round}\n"),
                            "overwrite": true
                        }),
                    )?;
                    assert!(
                        as_bool(&response, "success")?,
                        "disjoint fanout write should commit during squash: {response}"
                    );
                }
                Ok(())
            })
        })
        .collect();

    driver.join().expect("squash-race driver panicked")?;
    for handle in fanout_handles {
        handle.join().expect("squash-race fanout panicked")?;
    }

    // The stack stays writable and coherent after the concurrent squash storm.
    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": "pressure/squash-race/driver.txt", "content": "final\n", "overwrite": true}),
    )?;
    let driver_read = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": "pressure/squash-race/driver.txt"}),
    )?;
    assert_eq!(as_str(&driver_read, "content")?, "final\n", "{driver_read}");

    // No fanout data was lost to the squash race.
    for writer in 0..fanout {
        let read = lease.call_ok(
            catalog::SANDBOX_FILE_READ,
            json!({"path": format!("pressure/squash-race/w-{writer}/r-{}.txt", rounds_each - 1)}),
        )?;
        assert_eq!(
            as_str(&read, "content")?,
            format!("w-{writer}-r-{}\n", rounds_each - 1),
            "fanout readback should survive concurrent squash: {read}"
        );
    }

    let metrics = wait_for_active_leases(&lease, 0)?;
    assert!(
        as_i64(&metrics, "manifest_depth")? <= 100,
        "auto-squash must keep depth bounded under concurrent publish pressure: {metrics}"
    );
    assert_eq!(
        as_i64(&metrics, "orphan_layer_count")?,
        0,
        "concurrent squash must not strand superseded layer dirs: {metrics}"
    );
    assert_eq!(as_i64(&metrics, "active_leases")?, 0, "{metrics}");
    Ok(())
}
