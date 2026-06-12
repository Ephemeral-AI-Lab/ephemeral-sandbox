use std::sync::{Arc, Barrier};
use std::thread;

use anyhow::Result;
use eos_operation::core::catalog;
use serde_json::{json, Value};

use crate::helpers::{
    optional_response_result, pressure_levels, request_with_identity, response_result,
    result_committed, result_structured,
};
use crate::support::{as_i64, as_str, live_pool_or_skip, wait_for_active_leases};

/// Production runs N *distinct* agents (distinct `caller_id`s) concurrently on
/// one shared LayerStack — see `docs/architecture/sandbox/space-model.html`
/// §9.3.1 ("disjoint file-API writes from N agents batch into one OCC publish").
/// Every other pressure ladder fans out a SINGLE caller across threads, so
/// per-caller lease accounting and the shared-stack publish path are never
/// exercised under the documented multi-agent shape. This ladder gives every
/// concurrent writer its own caller id.
#[test]
fn distinct_callers_disjoint_writes_ladder_1_3_6_12() -> Result<()> {
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
                let caller_id = format!("{}-agent-{level}-{index}", lease.caller_id());
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    request_with_identity(
                        &client,
                        catalog::SANDBOX_FILE_WRITE,
                        &root,
                        &caller_id,
                        json!({
                            "path": format!("pressure/multi-caller/level-{level}/agent-{index}.txt"),
                            "content": format!("agent-{level}-{index}\n"),
                            "overwrite": true
                        }),
                    )
                })
            })
            .collect();

        for handle in handles {
            let response = handle.join().expect("distinct-caller writer panicked")?;
            let result = response_result(&response)?;
            assert!(
                result_committed(result),
                "distinct-caller disjoint write should commit at level {level}: {response}"
            );
        }

        // Every agent's write is visible on the shared stack: the artifact is the
        // channel, so a distinct caller still reads peers' published content.
        for index in 0..level {
            let read = lease.call_ok(
                catalog::SANDBOX_FILE_READ,
                json!({"path": format!("pressure/multi-caller/level-{level}/agent-{index}.txt")}),
            )?;
            assert_eq!(
                as_str(&read, "content")?,
                format!("agent-{level}-{index}\n"),
                "shared-stack readback should match across distinct callers at level {level}: {read}"
            );
        }

        let metrics = wait_for_active_leases(&lease, 0)?;
        assert_eq!(
            as_i64(&metrics, "active_leases")?,
            0,
            "distinct-caller ladder must not leak per-caller leases at level {level}: {metrics}"
        );
    }
    Ok(())
}

/// N distinct agents race the SAME path. OCC must serialize them to one coherent
/// winner regardless of which caller wins, and every loser must return a
/// structured payload — the per-caller conflict path on a shared stack that the
/// single-caller OCC ladder cannot reach.
#[test]
fn distinct_callers_same_path_conflict_resolves_to_one_winner() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let level = 8;
    let path = "pressure/multi-caller/contended.txt";
    let barrier = Arc::new(Barrier::new(level));

    let handles: Vec<_> = (0..level)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = format!("{}-contender-{index}", lease.caller_id());
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                request_with_identity(
                    &client,
                    catalog::SANDBOX_FILE_WRITE,
                    &root,
                    &caller_id,
                    json!({"path": path, "content": format!("agent-{index}\n"), "overwrite": true}),
                )
            })
        })
        .collect();

    let responses: Vec<Value> = handles
        .into_iter()
        .map(|handle| handle.join().expect("same-path contender panicked"))
        .collect::<Result<_>>()?;

    assert!(
        responses.iter().any(|response| {
            optional_response_result(response)
                .ok()
                .flatten()
                .is_some_and(result_committed)
        }),
        "same-path distinct-caller pressure should leave at least one committed writer: {responses:?}"
    );
    for response in &responses {
        let result = optional_response_result(response)?;
        assert!(
            result.is_some_and(result_structured) || response.get("error").is_some(),
            "every distinct-caller contender should return a structured payload: {response}"
        );
    }

    let read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": path}))?;
    let content = as_str(&read, "content")?;
    assert!(
        (0..level).any(|index| content == format!("agent-{index}\n")),
        "final content must be exactly one whole contender's payload: {read}"
    );

    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0, "{metrics}");
    Ok(())
}
