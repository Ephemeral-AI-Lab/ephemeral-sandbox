use std::sync::{Arc, Barrier};
use std::thread;

use anyhow::Result;
use eos_protocol::ops;
use serde_json::json;

use crate::helpers::request_with_identity;
use crate::support::{
    as_bool, as_i64, as_str, live_pool_or_skip, wait_for_active_leases, wait_for_session_count,
};

/// Point-in-time leak checks (`active_leases == 0` after one drain) can miss a
/// slow leak that only manifests after sustained operation. This soak runs a
/// mixed file/exec/command-cancel round over a FIXED working set many times and
/// asserts the leak counters return to zero *every* round, auto-squash keeps the
/// manifest bounded under sustained load (so storage does not creep with round
/// count), and the daemon stays ready throughout.
#[test]
fn mixed_workload_soak_keeps_counters_and_storage_bounded() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let rounds = 12;
    let fanout = 4;
    let hot_overwrites = 10;
    let mut storage_samples = Vec::with_capacity(rounds);

    for round in 0..rounds {
        // Concurrent overwrites of a FIXED set of paths (no per-round directory):
        // a leak would accumulate, but real data stays constant-size.
        let barrier = Arc::new(Barrier::new(fanout));
        let handles: Vec<_> = (0..fanout)
            .map(|writer| {
                let client = lease.client().clone();
                let root = lease.root().to_owned();
                let caller_id = lease.caller_id().to_owned();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    request_with_identity(
                        &client,
                        ops::API_V1_WRITE_FILE,
                        &root,
                        &caller_id,
                        json!({
                            "path": format!("soak/w-{writer}.txt"),
                            "content": format!("round-{round}-writer-{writer}\n"),
                            "overwrite": true
                        }),
                    )
                })
            })
            .collect();
        for handle in handles {
            let response = handle.join().expect("soak writer panicked")?;
            assert!(
                as_bool(&response, "success")?,
                "soak fanout write should commit at round {round}: {response}"
            );
        }

        // An overlay exec that publishes back into the fixed set.
        let exec = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "cmd": "mkdir -p soak && printf exec > soak/exec.txt",
                "yield_time_ms": 1000,
                "timeout_seconds": 20,
                "max_output_tokens": 1000
            }),
        )?;
        assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");

        // A long-running command session that must start, cancel, and fully drain.
        let session = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "cmd": format!("sh -c 'echo soak-{round}; sleep 60'"),
                "yield_time_ms": 100,
                "timeout_seconds": 120,
                "max_output_tokens": 500
            }),
        )?;
        assert_eq!(as_str(&session, "status")?, "running", "{session}");
        lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": as_str(&session, "command_session_id")?}),
        )?;
        wait_for_session_count(&lease, 0)?;

        // Overwrite a single hot path repeatedly so the manifest crosses the
        // auto-squash depth target over the soak; squash must reclaim so storage
        // does not grow with round count.
        for version in 0..hot_overwrites {
            lease.call_ok(
                ops::API_V1_WRITE_FILE,
                json!({
                    "path": "soak/hot.txt",
                    "content": format!("hot-{round}-{version}\n"),
                    "overwrite": true
                }),
            )?;
        }

        // Both leak surfaces must return to zero every round.
        let metrics = wait_for_active_leases(&lease, 0)?;
        assert_eq!(
            as_i64(&metrics, "active_leases")?,
            0,
            "soak must not leak leases at round {round}: {metrics}"
        );
        let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
        assert_eq!(
            as_i64(&count, "count")?,
            0,
            "soak must not leak command sessions at round {round}: {count}"
        );
        storage_samples.push((
            as_i64(&metrics, "manifest_depth")?,
            as_i64(&metrics, "storage_bytes")?,
        ));
    }

    // Auto-squash keeps the manifest bounded under sustained mixed load.
    let (final_depth, _) = storage_samples[rounds - 1];
    assert!(
        final_depth <= 100,
        "auto-squash must keep manifest depth bounded across the soak: depths={:?}",
        storage_samples
            .iter()
            .map(|(depth, _)| *depth)
            .collect::<Vec<_>>()
    );

    // Storage must not creep with round count. With a fixed working set, squash
    // makes storage a bounded sawtooth (grow per round, reclaim at squash), so
    // the storage *ceiling* in the second half stays near the first half rather
    // than climbing as a staircase. Comparing half-maxima is robust to exactly
    // where a squash lands relative to the sampling boundary.
    let storages: Vec<i64> = storage_samples.iter().map(|(_, bytes)| *bytes).collect();
    let mid = rounds / 2;
    let first_half_max = storages[..mid].iter().copied().max().unwrap_or(0);
    let second_half_max = storages[mid..].iter().copied().max().unwrap_or(0);
    assert!(
        second_half_max <= first_half_max * 2,
        "soak storage ceiling must not grow with round count (first_half_max={first_half_max}, second_half_max={second_half_max}): {storage_samples:?}"
    );
    // Squash must actively reclaim during the soak: at least one round-over-round
    // storage decrease, proving the bound comes from reclamation, not luck.
    assert!(
        storages.windows(2).any(|pair| pair[1] < pair[0]),
        "auto-squash should reclaim storage at least once during the soak: {storage_samples:?}"
    );

    let ready = lease.call_ok(ops::API_RUNTIME_READY, json!({}))?;
    assert!(
        as_bool(&ready, "ready")?,
        "daemon must stay ready after the soak: {ready}"
    );
    Ok(())
}
