use std::sync::{Arc, Barrier};
use std::thread;

use anyhow::Result;
use eos_e2e_test::next_invocation_id;
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::common::{array, as_bool, as_str, conflict_reason, live_pool_or_skip};

#[test]
fn concurrent_conflicting_writes() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["left", "right"]
        .into_iter()
        .map(|label| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let agent_id = lease.agent_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                client.request(
                    ops::API_V1_WRITE_FILE,
                    &next_invocation_id(),
                    &json!({
                        "layer_stack_root": root,
                        "agent_id": agent_id,
                        "path": "occ/conflict.txt",
                        "content": format!("{label}\n"),
                        "overwrite": true
                    }),
                )
            })
        })
        .collect();
    let responses: Vec<Value> = handles
        .into_iter()
        .map(|handle| handle.join().expect("writer thread panicked"))
        .collect::<Result<_>>()?;
    assert!(
        responses
            .iter()
            .any(|response| response.get("status").and_then(Value::as_str) == Some("committed")),
        "at least one writer should publish: {responses:?}"
    );
    for response in &responses {
        assert!(
            as_bool(response, "success").unwrap_or(false) || response.get("conflict").is_some(),
            "write should either commit or surface a conflict: {response}"
        );
    }
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": "occ/conflict.txt"}))?;
    assert!(
        matches!(as_str(&read, "content")?, "left\n" | "right\n"),
        "final content should be one coherent writer output: {read}"
    );
    Ok(())
}

#[test]
fn concurrent_disjoint_writes() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let barrier = Arc::new(Barrier::new(6));
    let handles: Vec<_> = (0..6)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let agent_id = lease.agent_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                client.request(
                    ops::API_V1_WRITE_FILE,
                    &next_invocation_id(),
                    &json!({
                        "layer_stack_root": root,
                        "agent_id": agent_id,
                        "path": format!("occ/disjoint-{index}.txt"),
                        "content": format!("{index}\n"),
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
            "disjoint write should commit: {response}"
        );
    }
    for index in 0..6 {
        let read = lease.call_ok(
            ops::API_V1_READ_FILE,
            json!({"path": format!("occ/disjoint-{index}.txt")}),
        )?;
        assert_eq!(as_str(&read, "content")?, format!("{index}\n"));
    }
    Ok(())
}

#[test]
fn edit_overlap_conflict() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "occ/overlap.txt", "content": "dup dup\n", "overwrite": true}),
    )?;
    let edit = lease.call(
        ops::API_V1_EDIT_FILE,
        json!({
            "path": "occ/overlap.txt",
            "edits": [{"old_text": "dup", "new_text": "x", "replace_all": false}]
        }),
    )?;
    assert_eq!(
        conflict_reason(&edit),
        "aborted_overlap",
        "overlap conflict expected: {edit}"
    );
    Ok(())
}

#[test]
fn retry_budget_3x_surfaces_coherent_result() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let barrier = Arc::new(Barrier::new(12));
    let handles: Vec<_> = (0..12)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let agent_id = lease.agent_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                client.request(
                    ops::API_V1_WRITE_FILE,
                    &next_invocation_id(),
                    &json!({
                        "layer_stack_root": root,
                        "agent_id": agent_id,
                        "path": "occ/retry-budget.txt",
                        "content": format!("{index}\n"),
                        "overwrite": true
                    }),
                )
            })
        })
        .collect();
    for handle in handles {
        let response = handle.join().expect("writer thread panicked")?;
        assert!(
            response.get("status").is_some() || response.get("error").is_some(),
            "concurrent writer should return a structured protocol payload: {response}"
        );
    }
    let read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "occ/retry-budget.txt"}),
    )?;
    assert!(
        as_str(&read, "content")?.trim().parse::<usize>().is_ok(),
        "final content should be one whole writer output: {read}"
    );
    Ok(())
}

#[test]
fn publish_accounting() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let mut audit = lease.audit_tap()?;
    let write = lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "occ/audit.txt", "content": "audit\n", "overwrite": true}),
    )?;
    assert!(!array(&write, "changed_paths")?.is_empty());
    audit.collect()?;
    assert!(
        audit.any("occ.publish"),
        "write publish should emit occ.publish: {:?}",
        audit.events()
    );
    Ok(())
}

#[test]
fn route_fileresult_catalog() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let committed = lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "occ/catalog.txt", "content": "one\n", "overwrite": true}),
    )?;
    assert_eq!(as_str(&committed, "status")?, "committed");
    let rejected = lease.call(
        ops::API_V1_WRITE_FILE,
        json!({"path": "occ/catalog.txt", "content": "two\n", "overwrite": false}),
    )?;
    assert_eq!(as_str(&rejected, "status")?, "rejected");
    assert_eq!(conflict_reason(&rejected), "create_only_existing");
    let missing_edit = lease.call(
        ops::API_V1_EDIT_FILE,
        json!({
            "path": "occ/missing.txt",
            "edits": [{"old_text": "x", "new_text": "y", "replace_all": false}]
        }),
    )?;
    assert_eq!(conflict_reason(&missing_edit), "aborted_version");
    Ok(())
}
