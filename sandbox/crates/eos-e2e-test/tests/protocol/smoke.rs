use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_e2e_test::audit::section;
use eos_e2e_test::cas::looks_like_sha256;
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

fn wait_for_active_leases(lease: &eos_e2e_test::NodeLease<'_>, expected: i64) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
        if as_i64(&metrics, "active_leases")? == expected {
            return Ok(metrics);
        }
        if Instant::now() >= deadline {
            bail!("active_leases did not reach {expected}: {metrics}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn setup_readiness_metrics_and_audit_are_protocol_visible() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let ready = lease.call_ok(ops::API_RUNTIME_READY, json!({}))?;
    assert!(as_bool(&ready, "success")?);
    assert!(as_bool(&ready, "ready")?);

    let heartbeat = lease.call_ok(ops::API_V1_HEARTBEAT, json!({"invocation_ids": []}))?;
    assert!(as_bool(&heartbeat, "success")?);

    let binding = lease.call_ok(ops::API_WORKSPACE_BINDING, json!({}))?;
    assert_eq!(
        binding["binding"]["workspace_root"],
        Value::String(lease.workspace_root().to_owned())
    );

    let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert!(as_bool(&metrics, "workspace_bound")?);
    assert_eq!(as_i64(&metrics, "active_leases")?, 0);

    let snapshot = lease.call_ok(ops::API_AUDIT_SNAPSHOT, json!({}))?;
    assert!(as_bool(&snapshot, "success")?);

    let mut audit = lease.audit_tap()?;
    let ensure = lease.call_ok(
        ops::API_ENSURE_WORKSPACE_BASE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(as_bool(&ensure, "success")?);
    audit.collect()?;
    if let Some(event) = audit.first("workspace_base.ensured") {
        let layer_stack = section(event, "layer_stack").context("layer_stack audit section")?;
        assert_eq!(
            layer_stack.get("manifest_version").and_then(Value::as_i64),
            Some(1),
            "workspace_base audit should include the active manifest version: {event}"
        );
        assert!(
            layer_stack
                .get("manifest_root_hash")
                .and_then(Value::as_str)
                .is_some_and(looks_like_sha256),
            "workspace_base audit should include a CAS-shaped manifest hash: {event}"
        );
    }
    Ok(())
}

#[test]
fn file_tool_calls_round_trip_through_protocol() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = "e2e/hello.txt";

    let write = lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": path, "content": "hello from protocol\n", "overwrite": true}),
    )?;
    assert!(as_bool(&write, "success")?);

    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    assert_eq!(as_str(&read, "content")?, "hello from protocol\n");

    let edit = lease.call_ok(
        ops::API_V1_EDIT_FILE,
        json!({
            "path": path,
            "edits": [{"old_text": "hello", "new_text": "hi", "replace_all": false}]
        }),
    )?;
    assert!(as_bool(&edit, "success")?);

    let grep = lease.call_ok(
        ops::API_V1_GREP,
        json!({"pattern": "hi from protocol", "path": "e2e", "output_mode": "content"}),
    )?;
    assert!(as_str(&grep, "content")?.contains("hi from protocol"));

    let glob = lease.call_ok(ops::API_V1_GLOB, json!({"pattern": "e2e/*.txt"}))?;
    let names = glob
        .get("filenames")
        .and_then(Value::as_array)
        .context("glob filenames missing")?;
    assert!(names.iter().any(|name| name.as_str() == Some(path)));
    Ok(())
}

#[test]
fn command_sessions_accept_stdin_and_release_on_cancel() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
    assert_eq!(as_i64(&count, "count")?, 0);

    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "python3 -u -c 'import sys,time; print(\"ready\", flush=True); line=sys.stdin.readline().strip(); print(\"got:\" + line, flush=True); time.sleep(60)'",
            "yield_time_ms": 500,
            "timeout_seconds": 120,
            "max_output_tokens": 2000
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running");
    let session_id = as_str(&started, "command_session_id")?.to_owned();
    assert!(
        started["output"]["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("ready"),
        "expected initial stdout to contain readiness marker: {started}"
    );
    let leased = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert!(
        as_i64(&leased, "active_leases")? >= 1,
        "running command should hold a layer lease: {leased}"
    );

    let stdin = lease.call_ok(
        ops::API_V1_WRITE_STDIN,
        json!({
            "command_session_id": session_id,
            "chars": "line-one\n",
            "yield_time_ms": 2000,
            "max_output_tokens": 2000
        }),
    )?;
    if as_str(&stdin, "status")? != "running" {
        bail!("expected session to keep running after stdin write: {stdin}");
    }
    assert!(
        stdin["output"]["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("got:line-one"),
        "expected stdin echo in stdout: {stdin}"
    );

    let cancel = lease.call_ok(
        ops::API_V1_COMMAND_CANCEL,
        json!({"command_session_id": session_id, "max_output_tokens": 2000}),
    )?;
    assert!(matches!(
        as_str(&cancel, "status")?,
        "cancelled" | "ok" | "error"
    ));

    let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
    assert_eq!(as_i64(&count, "count")?, 0);
    let released = wait_for_active_leases(&lease, 0)?;
    assert_eq!(
        as_i64(&released, "active_leases")?,
        0,
        "cancelled command should release its layer lease: {released}"
    );
    Ok(())
}

#[test]
fn commit_to_workspace_survives_protocol_rebuild() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = "e2e/committed.txt";

    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": path, "content": "committed through protocol\n", "overwrite": true}),
    )?;
    let mut audit = lease.audit_tap()?;
    let commit = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(as_bool(&commit, "success")?);
    audit.collect()?;
    if let Some(event) = audit.first("layer_stack.commit_completed") {
        let layer_stack = section(event, "layer_stack").context("layer_stack audit section")?;
        assert_eq!(
            layer_stack.get("manifest_version").and_then(Value::as_i64),
            commit.get("manifest_version").and_then(Value::as_i64),
            "commit audit manifest_version should match response: {event}"
        );
        assert!(
            layer_stack
                .get("manifest_root_hash")
                .and_then(Value::as_str)
                .is_some_and(looks_like_sha256),
            "commit audit should include a CAS-shaped manifest hash: {event}"
        );
    }

    let rebuilt = lease.call_ok(
        ops::API_BUILD_WORKSPACE_BASE,
        json!({"workspace_root": lease.workspace_root(), "reset": true}),
    )?;
    assert!(as_bool(&rebuilt, "success")?);

    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    assert_eq!(as_str(&read, "content")?, "committed through protocol\n");
    Ok(())
}
