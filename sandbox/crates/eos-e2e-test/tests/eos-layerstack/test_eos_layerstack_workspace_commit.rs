use std::sync::{Arc, Barrier};
use std::thread;

use anyhow::{Context, Result};
use eos_e2e_test::audit::section;
use eos_e2e_test::cas::looks_like_sha256;
use eos_e2e_test::next_invocation_id;
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{as_bool, as_i64, as_str, live_pool_or_skip, wait_for_active_leases};

#[test]
fn commit_collapses_layers() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    for index in 0..5 {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": format!("commit/collapse-{index}.txt"), "content": "x\n", "overwrite": true}),
        )?;
    }
    let before = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert!(as_i64(&before, "manifest_depth")? > 1);
    let commit = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(as_bool(&commit, "success")?);
    let after = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&after, "manifest_depth")?,
        1,
        "commit should collapse the active manifest to the workspace base: {after}"
    );
    Ok(())
}

#[test]
fn commit_materializes_merged_view() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/materialized.txt", "content": "materialized\n", "overwrite": true}),
    )?;
    lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    lease.call_ok(
        ops::API_BUILD_WORKSPACE_BASE,
        json!({"workspace_root": lease.workspace_root(), "reset": true}),
    )?;
    let read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "commit/materialized.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "materialized\n");
    Ok(())
}

#[test]
fn commit_version_monotonic() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/version.txt", "content": "v1\n", "overwrite": true}),
    )?;
    let first = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/version.txt", "content": "v2\n", "overwrite": true}),
    )?;
    let second = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(
        as_i64(&second, "manifest_version")? >= as_i64(&first, "manifest_version")?,
        "commit manifest versions should be monotonic: first={first} second={second}"
    );
    Ok(())
}

#[test]
fn commit_emits_audit() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/audit.txt", "content": "audit\n", "overwrite": true}),
    )?;
    let mut audit = lease.audit_tap()?;
    let commit = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    audit.collect()?;
    let event = audit
        .first("layer_stack.commit_completed")
        .context("layer_stack.commit_completed audit event")?;
    let layer_stack = section(event, "layer_stack").context("layer_stack audit section")?;
    assert_eq!(
        layer_stack
            .get("manifest_version")
            .and_then(serde_json::Value::as_i64),
        commit
            .get("manifest_version")
            .and_then(serde_json::Value::as_i64),
        "commit audit should report response manifest version: {event}"
    );
    assert!(
        layer_stack
            .get("manifest_root_hash")
            .and_then(serde_json::Value::as_str)
            .is_some_and(looks_like_sha256),
        "commit audit should report a CAS-shaped root hash: {event}"
    );
    Ok(())
}

#[test]
fn commit_refuses_active_snapshot_lease_then_succeeds_after_release() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/lease-guard.txt", "content": "guard\n", "overwrite": true}),
    )?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let held = wait_for_active_leases(&lease, 1)?;
    assert_eq!(
        as_i64(&held, "active_leases")?,
        1,
        "isolated enter should hold a lease on this checkout's LayerStack root: {held}"
    );

    let blocked = lease.call(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    let outcome: Result<()> = {
        assert_eq!(
            blocked.get("success").and_then(Value::as_bool),
            Some(false),
            "commit_to_workspace must reject while an isolated snapshot lease is active: {blocked}"
        );
        let message = blocked
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains("active leases"),
            "active-lease rejection should identify the guard: {blocked}"
        );
        Ok(())
    };

    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    let released = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&released, "active_leases")?, 0, "{released}");
    outcome?;
    let committed = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(as_bool(&committed, "success")?, "{committed}");
    Ok(())
}

#[test]
fn commit_projects_delete_symlink_and_replacement_write() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!("commit-projection-{}", eos_e2e_test::unique_suffix());
    let deleted = format!("{dir}/delete-me.txt");
    let old = format!("{dir}/replace/old.txt");
    let new = format!("{dir}/replace/new.txt");
    let target = format!("{dir}/target.txt");
    let link = format!("{dir}/link.txt");

    for (path, content) in [
        (&deleted, "delete me\n"),
        (&old, "old\n"),
        (&target, "target\n"),
    ] {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": path, "content": content, "overwrite": true}),
        )?;
    }
    let overlay = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!("rm -f {deleted} {old} && mkdir -p {dir}/replace && printf new > {new} && ln -s target.txt {link}"),
            "yield_time_ms": 1000,
            "timeout_seconds": 10,
            "max_output_tokens": 1000
        }),
    )?;
    assert_eq!(as_str(&overlay, "status")?, "ok", "{overlay}");

    let commit = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(as_bool(&commit, "success")?, "{commit}");
    for key in [
        "layer_stack.commit_to_workspace.project_s",
        "layer_stack.commit_to_workspace.replace_workspace_s",
        "layer_stack.commit_to_workspace.rebuild_base_s",
    ] {
        assert_timing_present(&commit, key)?;
    }

    let check = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!(
                "test ! -e {deleted} && test ! -e {old} && test -f {new} && test -L {link} && test \"$(readlink {link})\" = target.txt"
            ),
            "yield_time_ms": 1000,
            "timeout_seconds": 10,
            "max_output_tokens": 1000
        }),
    )?;
    assert_eq!(
        as_str(&check, "status")?,
        "ok",
        "projected workspace should preserve delete masking, replacement writes, and symlink target: {check}"
    );
    Ok(())
}

#[test]
fn workspace_base_rebuild_idempotent_metrics() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = "commit/base-rebuild-idempotent.txt";
    let content = "stable workspace base\n";

    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": path, "content": content, "overwrite": true}),
    )?;
    lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;

    let mut audit = lease.audit_tap()?;
    let first = rebuild_workspace_base(&lease)?;
    let first_metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    let second = rebuild_workspace_base(&lease)?;
    let second_metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    audit.collect()?;

    assert_rebuild_response(&first)?;
    assert_rebuild_response(&second)?;
    assert_eq!(
        binding_str(&second, "base_root_hash")?,
        binding_str(&first, "base_root_hash")?,
        "rebuilding an unchanged workspace base should keep the base hash stable: first={first} second={second}"
    );
    assert_rebuilt_base_metrics(&first_metrics)?;
    assert_rebuilt_base_metrics(&second_metrics)?;
    assert!(
        as_i64(&second_metrics, "storage_bytes")? <= as_i64(&first_metrics, "storage_bytes")? + 8192,
        "repeated reset rebuild should not grow durable stack storage: first={first_metrics} second={second_metrics}"
    );

    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    assert_eq!(as_str(&read, "content")?, content);

    let built_events = audit.all("workspace_base.built");
    assert!(
        built_events.len() >= 2,
        "two reset rebuilds should emit workspace_base.built audit events: {:?}",
        audit.events()
    );
    for event in built_events.into_iter().rev().take(2) {
        assert_workspace_base_built_event(event)?;
    }
    Ok(())
}

fn rebuild_workspace_base(lease: &eos_e2e_test::NodeLease<'_>) -> Result<Value> {
    lease.call_ok(
        ops::API_BUILD_WORKSPACE_BASE,
        json!({"workspace_root": lease.workspace_root(), "reset": true}),
    )
}

fn assert_rebuild_response(response: &Value) -> Result<()> {
    assert!(
        as_bool(response, "success")?,
        "rebuild should succeed: {response}"
    );
    assert!(
        as_bool(response, "created")?,
        "reset rebuild should create a base: {response}"
    );
    let binding = response
        .get("binding")
        .context("binding missing from workspace base rebuild response")?;
    assert_eq!(
        binding
            .get("active_manifest_version")
            .and_then(Value::as_i64),
        Some(1),
        "reset rebuild should publish active manifest version 1: {response}"
    );
    assert_eq!(
        binding.get("base_manifest_version").and_then(Value::as_i64),
        Some(1),
        "reset rebuild should publish base manifest version 1: {response}"
    );
    assert!(
        binding
            .get("base_root_hash")
            .and_then(Value::as_str)
            .is_some_and(looks_like_sha256),
        "reset rebuild should report a CAS-shaped base hash: {response}"
    );
    for key in [
        "api.workspace_base.total_s",
        "workspace_base.prepare_stack_s",
        "workspace_base.collect_s",
        "workspace_base.write_layer_s",
        "workspace_base.write_manifest_s",
        "workspace_base.write_binding_s",
    ] {
        assert_timing_present(response, key)?;
    }
    Ok(())
}

fn assert_rebuilt_base_metrics(metrics: &Value) -> Result<()> {
    assert_eq!(
        as_i64(metrics, "manifest_depth")?,
        1,
        "base rebuild should leave a single active base layer: {metrics}"
    );
    assert_eq!(
        as_i64(metrics, "referenced_layers")?,
        1,
        "base rebuild should reference exactly one layer: {metrics}"
    );
    assert_eq!(
        as_i64(metrics, "layer_dirs")?,
        1,
        "base rebuild should not leave superseded layer dirs: {metrics}"
    );
    assert_eq!(
        as_i64(metrics, "staging_dirs")?,
        0,
        "base rebuild should not leave staging dirs: {metrics}"
    );
    assert_eq!(
        as_i64(metrics, "active_leases")?,
        0,
        "base rebuild should leave no active leases: {metrics}"
    );
    assert_eq!(
        as_i64(metrics, "leased_layers")?,
        0,
        "base rebuild should leave no retained lease layers: {metrics}"
    );
    assert!(
        as_i64(metrics, "storage_bytes")? > 0,
        "base rebuild should expose nonzero stack storage bytes: {metrics}"
    );
    Ok(())
}

fn assert_workspace_base_built_event(event: &Value) -> Result<()> {
    let layer_stack = section(event, "layer_stack").context("layer_stack audit section")?;
    assert_eq!(
        layer_stack.get("manifest_version").and_then(Value::as_i64),
        Some(1),
        "workspace_base.built audit should report rebuilt manifest version: {event}"
    );
    assert_eq!(
        layer_stack.get("layer_count").and_then(Value::as_i64),
        Some(1),
        "workspace_base.built audit should report a single rebuilt layer: {event}"
    );
    assert!(
        layer_stack
            .get("manifest_root_hash")
            .and_then(Value::as_str)
            .is_some_and(looks_like_sha256),
        "workspace_base.built audit should report a CAS-shaped manifest hash: {event}"
    );
    assert!(
        layer_stack
            .get("total_ms")
            .and_then(Value::as_f64)
            .is_some_and(|total_ms| total_ms >= 0.0),
        "workspace_base.built audit should report nonnegative total_ms: {event}"
    );
    Ok(())
}

fn assert_timing_present(response: &Value, key: &str) -> Result<()> {
    let timing = response
        .get("timings")
        .and_then(|timings| timings.get(key))
        .and_then(Value::as_f64)
        .with_context(|| format!("timing {key} missing in {response}"))?;
    assert!(
        timing >= 0.0,
        "timing {key} should be nonnegative in response: {response}"
    );
    Ok(())
}

fn binding_str<'a>(response: &'a Value, key: &str) -> Result<&'a str> {
    response
        .get("binding")
        .and_then(|binding| binding.get(key))
        .and_then(Value::as_str)
        .with_context(|| format!("binding.{key} missing in {response}"))
}

/// `commit_refuses_active_snapshot_lease_then_succeeds_after_release` covers
/// commit versus a held lease SEQUENTIALLY. Here `commit_to_workspace` fires
/// concurrently with a disjoint write storm: the transient per-op leases mean
/// the commit may be refused with the active-lease guard or land in a gap and
/// succeed. Either way the response must be structured, every write that claimed
/// success must survive a commit + base rebuild (durable, not torn), and a
/// retried commit after the storm drains must succeed cleanly.
#[test]
fn commit_races_inflight_writes_stays_structured_and_coherent() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/race/base.txt", "content": "base\n", "overwrite": true}),
    )?;

    let writers = 6;
    let barrier = Arc::new(Barrier::new(writers + 1));

    let committer = {
        let client = lease.client().clone();
        let root = lease.root().to_owned();
        let caller_id = lease.caller_id().to_owned();
        let workspace_root = lease.workspace_root().to_owned();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            client.request(
                ops::API_COMMIT_TO_WORKSPACE,
                &next_invocation_id(),
                &json!({
                    "layer_stack_root": root,
                    "caller_id": caller_id,
                    "workspace_root": workspace_root
                }),
            )
        })
    };

    let writer_handles: Vec<_> = (0..writers)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = lease.caller_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                client.request(
                    ops::API_V1_WRITE_FILE,
                    &next_invocation_id(),
                    &json!({
                        "layer_stack_root": root,
                        "caller_id": caller_id,
                        "path": format!("commit/race/w-{index}.txt"),
                        "content": format!("w-{index}\n"),
                        "overwrite": true
                    }),
                )
            })
        })
        .collect();

    let writer_responses: Vec<Value> = writer_handles
        .into_iter()
        .map(|handle| handle.join().expect("commit-race writer panicked"))
        .collect::<Result<_>>()?;
    let commit_response = committer.join().expect("commit-race committer panicked")?;

    // The racing commit must be structured: a clean success, or the active-lease
    // guard (a transient per-op write lease was live), never a crash.
    if !as_bool(&commit_response, "success").unwrap_or(false) {
        let message = commit_response
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains("active leases"),
            "a refused racing commit must be the structured active-lease guard: {commit_response}"
        );
    }
    for response in &writer_responses {
        assert!(
            response.get("status").is_some()
                || response.get("conflict").is_some()
                || response.get("error").is_some()
                || as_bool(response, "success").unwrap_or(false),
            "every racing writer must return a structured payload: {response}"
        );
    }

    // Drain, then a clean retried commit must succeed and collapse the manifest.
    wait_for_active_leases(&lease, 0)?;
    let committed = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(as_bool(&committed, "success")?, "{committed}");

    // Rebuild from the committed base to prove durability survived the race.
    lease.call_ok(
        ops::API_BUILD_WORKSPACE_BASE,
        json!({"workspace_root": lease.workspace_root(), "reset": true}),
    )?;
    let base_read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "commit/race/base.txt"}),
    )?;
    assert_eq!(as_str(&base_read, "content")?, "base\n", "{base_read}");

    // Every writer that claimed success must be durable and coherent after the
    // concurrent commit + rebuild.
    for (index, response) in writer_responses.iter().enumerate() {
        if as_bool(response, "success").unwrap_or(false) {
            let read = lease.call_ok(
                ops::API_V1_READ_FILE,
                json!({"path": format!("commit/race/w-{index}.txt")}),
            )?;
            assert_eq!(
                as_str(&read, "content")?,
                format!("w-{index}\n"),
                "a committed racing write must survive the commit + rebuild: {read}"
            );
        }
    }

    let ready = lease.call_ok(ops::API_RUNTIME_READY, json!({}))?;
    assert!(
        as_bool(&ready, "ready")?,
        "daemon must stay ready after commit race: {ready}"
    );
    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0, "{metrics}");
    Ok(())
}
