use anyhow::{Context, Result};
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{array, as_bool, as_str, live_pool_or_skip, reset_isolated_workspaces};

#[test]
fn isolated_write_is_discarded_on_exit() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = "iso/private.txt";

    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;

    let write = lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": path, "content": "isolated-only\n", "overwrite": true}),
    )?;
    assert_eq!(
        as_str(&write, "mutation_source")?,
        "isolated_workspace",
        "write inside isolated mode must be isolated-sourced: {write}"
    );
    assert_eq!(
        as_str(&write, "status")?,
        "committed",
        "isolated write status: {write}"
    );

    let read_inside = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    assert_eq!(as_str(&read_inside, "content")?, "isolated-only\n");

    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    assert!(
        exit.get("evicted_upperdir_bytes")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            >= 0,
        "exit reports evicted upperdir bytes: {exit}"
    );

    let read_public = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    assert!(
        !as_bool(&read_public, "exists")?,
        "isolated write must not survive into the public workspace: {read_public}"
    );
    Ok(())
}

#[test]
fn isolated_exit_discards_private_upperdir() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "iso-overlay/discard.txt", "content": "discard\n", "overwrite": true}),
    )?;
    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    assert!(
        exit.get("inspection").is_some(),
        "isolated exit should report teardown inspection: {exit}"
    );
    let read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "iso-overlay/discard.txt"}),
    )?;
    assert!(
        !as_bool(&read, "exists")?,
        "private isolated write must not survive exit: {read}"
    );
    let closed = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
    assert!(
        !as_bool(&closed, "open")?,
        "status after exit should be closed: {closed}"
    );
    Ok(())
}

#[test]
fn isolated_exec_write_is_private_and_discarded() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    let path = format!(
        "iso-exec/{}.txt",
        eos_e2e_test::unique_suffix().replace('-', "_")
    );
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let mut audit = lease.audit_tap()?;

    let body = (|| -> Result<()> {
        let exec = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "cmd": format!("mkdir -p iso-exec && printf isolated-exec > {path}"),
                "yield_time_ms": 2000,
                "timeout_seconds": 10,}),
        )?;
        assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
        assert_eq!(as_str(&exec, "workspace")?, "isolated", "{exec}");
        assert_eq!(as_str(&exec, "workspace_mode")?, "isolated", "{exec}");
        assert_eq!(
            as_str(&exec, "mutation_source")?,
            "isolated_workspace",
            "{exec}"
        );
        assert!(
            array(&exec, "changed_paths")?
                .iter()
                .any(|changed| changed.as_str() == Some(path.as_str())),
            "isolated exec should report the private changed path: {exec}"
        );
        let isolated = exec
            .get("isolated_workspace")
            .context("isolated command response missing isolated_workspace metadata")?;
        assert_eq!(
            isolated.get("published").and_then(Value::as_bool),
            Some(false),
            "isolated exec must not publish to OCC: {exec}"
        );
        assert!(
            exec.get("audit").is_none(),
            "isolated exec audit payload is internal and must not be exposed on the wire: {exec}"
        );

        audit.collect()?;
        assert!(
            !audit.any("occ.publish"),
            "isolated exec must not emit occ.publish: {:?}",
            audit.events()
        );

        let read_inside = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
        assert_eq!(as_str(&read_inside, "workspace")?, "isolated");
        assert_eq!(as_str(&read_inside, "content")?, "isolated-exec");
        Ok(())
    })();

    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({"grace_s": 0.1}));
    body?;
    exit?;

    let read_public = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    assert_eq!(
        as_str(&read_public, "workspace")?,
        "ephemeral",
        "read after isolated exit should route ephemeral: {read_public}"
    );
    assert!(
        !as_bool(&read_public, "exists")?,
        "isolated exec write must be discarded on exit: {read_public}"
    );
    Ok(())
}
