use std::sync::{Arc, Barrier};
use std::thread;

use anyhow::Result;
use eos_e2e_test::{next_invocation_id, unique_suffix};
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{
    array, as_bool, as_i64, as_str, conflict_reason, live_pool_or_skip, wait_for_active_leases,
};

/// `docs/architecture/sandbox/occ.html` §4.4 (Throughput Model): the single
/// CommitQueue worker "batches disjoint non-atomic non-overlay changes into a
/// single transaction." The *drain-window* batching of separately submitted
/// concurrent writes is timing-dependent — one blocking request per write drains
/// each before the next arrives — so it is not reliably observable through the
/// protocol-only harness. The reliably observable form of the same invariant is
/// a single overlay operation whose M disjoint file writes are captured and
/// published as ONE layer: each published layer advances `manifest_depth` by one,
/// so M batched writes must grow the manifest by strictly fewer than M layers,
/// while every captured path is published and readable.
#[test]
fn single_overlay_exec_batches_multi_file_writes_into_one_layer() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!("occ-batch-{}", unique_suffix());
    let files: usize = 8;

    let before = as_i64(
        &lease.call_ok(ops::API_LAYER_METRICS, json!({}))?,
        "manifest_depth",
    )?;

    let mut cmd = format!("mkdir -p {dir}");
    for index in 0..files {
        cmd.push_str(&format!(" && printf '{index}\\n' > {dir}/file-{index}.txt"));
    }
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": cmd,
            "yield_time_ms": 1000,
            "timeout_seconds": 30,
            "max_output_tokens": 2000
        }),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
    assert_eq!(as_i64(&exec, "exit_code")?, 0, "{exec}");

    let changed = array(&exec, "changed_paths")?;
    for index in 0..files {
        let path = format!("{dir}/file-{index}.txt");
        assert!(
            changed
                .iter()
                .any(|value| value.as_str() == Some(path.as_str())),
            "the batched overlay capture should publish {path}: {exec}"
        );
    }

    let after = as_i64(
        &lease.call_ok(ops::API_LAYER_METRICS, json!({}))?,
        "manifest_depth",
    )?;
    let delta = after - before;
    assert!(
        delta >= 1,
        "at least one layer should publish: before={before} after={after}"
    );
    assert!(
        delta < i64::try_from(files).unwrap_or(i64::MAX),
        "one overlay capture must batch {files} disjoint writes into fewer than {files} layers (delta={delta}): before={before} after={after}"
    );

    for index in 0..files {
        let read = lease.call_ok(
            ops::API_V1_READ_FILE,
            json!({"path": format!("{dir}/file-{index}.txt")}),
        )?;
        assert_eq!(
            as_str(&read, "content")?,
            format!("{index}\n"),
            "every batched write must be readable: {read}"
        );
    }

    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0, "{metrics}");
    Ok(())
}

/// `edit_overlap_conflict` is single-threaded, and every threaded OCC test uses
/// `write_file`. Edits run anchor resolution plus OCC retry, exercised here
/// under contention: N agents each edit a DISJOINT anchor in the same file.
/// Whatever the merge policy, the invariants must hold — each edit either
/// applies or returns a structured conflict, at least one makes progress, and
/// the final file is one coherent version with no torn, duplicated, or lost
/// lines.
#[test]
fn concurrent_disjoint_anchor_edits_stay_atomic_and_coherent() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = format!("occ/concurrent-edit-{}.txt", unique_suffix());
    let lines = 6;
    let seed: String = (0..lines)
        .map(|index| format!("LINE{index}=orig\n"))
        .collect();
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": path, "content": seed, "overwrite": true}),
    )?;

    let barrier = Arc::new(Barrier::new(lines));
    let handles: Vec<_> = (0..lines)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = lease.caller_id().to_owned();
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                client.request(
                    ops::API_V1_EDIT_FILE,
                    &next_invocation_id(),
                    &json!({
                        "layer_stack_root": root,
                        "caller_id": caller_id,
                        "path": path,
                        "edits": [{
                            "old_text": format!("LINE{index}=orig"),
                            "new_text": format!("LINE{index}=edited"),
                            "replace_all": false
                        }]
                    }),
                )
            })
        })
        .collect();

    let responses: Vec<Value> = handles
        .into_iter()
        .map(|handle| handle.join().expect("concurrent edit panicked"))
        .collect::<Result<_>>()?;

    assert!(
        responses
            .iter()
            .any(|response| as_bool(response, "success").unwrap_or(false)),
        "at least one concurrent disjoint-anchor edit should apply: {responses:?}"
    );
    for response in &responses {
        assert!(
            as_bool(response, "success").unwrap_or(false)
                || response.get("conflict").is_some()
                || !conflict_reason(response).is_empty()
                || response.get("error").is_some(),
            "every concurrent edit should apply or surface a structured conflict: {response}"
        );
    }

    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    let content = as_str(&read, "content")?;
    let observed: Vec<&str> = content.lines().collect();
    assert_eq!(
        observed.len(),
        lines,
        "concurrent edits must not duplicate or drop lines: {read}"
    );
    let mut edited = 0;
    for (index, line) in observed.iter().enumerate() {
        let orig = format!("LINE{index}=orig");
        let done = format!("LINE{index}=edited");
        assert!(
            *line == orig || *line == done,
            "line {index} must be a coherent orig/edited value, not torn: {line:?}"
        );
        if *line == done {
            edited += 1;
        }
    }
    assert!(
        edited >= 1,
        "at least one edit must be reflected in the final file: {read}"
    );

    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0, "{metrics}");
    Ok(())
}

/// N agents edit the SAME anchor concurrently. At most one can apply; once a
/// winner consumes the anchor the rest must surface structured conflicts, and
/// the final content reflects exactly one whole edit.
#[test]
fn concurrent_same_anchor_edits_resolve_to_one_winner() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = format!("occ/same-anchor-{}.txt", unique_suffix());
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": path, "content": "TARGET=orig\n", "overwrite": true}),
    )?;

    let contenders = 8;
    let barrier = Arc::new(Barrier::new(contenders));
    let handles: Vec<_> = (0..contenders)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = lease.caller_id().to_owned();
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                client.request(
                    ops::API_V1_EDIT_FILE,
                    &next_invocation_id(),
                    &json!({
                        "layer_stack_root": root,
                        "caller_id": caller_id,
                        "path": path,
                        "edits": [{
                            "old_text": "TARGET=orig",
                            "new_text": format!("TARGET={index}"),
                            "replace_all": false
                        }]
                    }),
                )
            })
        })
        .collect();

    let responses: Vec<Value> = handles
        .into_iter()
        .map(|handle| handle.join().expect("same-anchor edit panicked"))
        .collect::<Result<_>>()?;

    let winners = responses
        .iter()
        .filter(|response| as_bool(response, "success").unwrap_or(false))
        .count();
    assert_eq!(
        winners, 1,
        "exactly one same-anchor editor may win the consumed anchor: {responses:?}"
    );
    for response in &responses {
        assert!(
            as_bool(response, "success").unwrap_or(false)
                || response.get("conflict").is_some()
                || !conflict_reason(response).is_empty()
                || response.get("error").is_some(),
            "every same-anchor loser should surface a structured conflict: {response}"
        );
    }

    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    let content = as_str(&read, "content")?;
    assert!(
        (0..contenders).any(|index| content == format!("TARGET={index}\n")),
        "final content must be exactly one whole winning edit: {read}"
    );

    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0, "{metrics}");
    Ok(())
}
