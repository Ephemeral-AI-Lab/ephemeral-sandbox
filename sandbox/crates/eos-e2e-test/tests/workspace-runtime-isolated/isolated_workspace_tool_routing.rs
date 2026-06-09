use anyhow::{ensure, Result};
use eos_protocol::ops;
use serde_json::json;

use crate::support::{
    array, as_bool, as_i64, as_str, conflict_message, conflict_reason, live_pool_or_skip,
    reset_isolated_workspaces,
};

#[test]
fn isolated_write_does_not_publish_or_release_lease() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    // Baseline AFTER enter so enter's own lease_acquired is excluded; the isolated
    // write must not release a public lease (it writes the private upperdir only).
    let mut audit = lease.audit_tap()?;
    let body = (|| -> Result<()> {
        let write = lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": "iso/no-publish.txt", "content": "private\n", "overwrite": true}),
        )?;
        ensure!(
            !as_bool(&write, "published")?,
            "isolated write must not publish to OCC: {write}"
        );
        audit.collect()?;
        ensure!(
            !audit.any("layer_stack.lease_released"),
            "isolated write must not release a public layer lease: {:?}",
            audit.events()
        );
        Ok(())
    })();
    let _ = lease.call(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}));
    body
}

#[test]
fn isolated_read_after_exit_routes_ephemeral() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let path = "iso/private-only.txt";
    let result = (|| -> Result<()> {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": path, "content": "secret\n", "overwrite": true}),
        )?;
        Ok(())
    })();
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    result?;
    // After exit the router falls back to the ephemeral workspace; the private
    // upperdir write was never published, so it is invisible there.
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    assert_eq!(
        as_str(&read, "workspace")?,
        "ephemeral",
        "read after isolated exit must route ephemeral: {read}"
    );
    assert!(
        !as_bool(&read, "exists")?,
        "an isolated-only write must not be visible after exit: {read}"
    );
    Ok(())
}

#[test]
fn isolated_enter_status_reports_manifest_pin() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let enter = lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let version = as_i64(&enter, "manifest_version")?;
    let hash = as_str(&enter, "manifest_root_hash")?.to_owned();
    assert_eq!(
        hash.len(),
        64,
        "enter should report CAS-shaped manifest hash: {enter}"
    );
    let status = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
    assert!(
        as_bool(&status, "open")?,
        "status should report open: {status}"
    );
    assert_eq!(as_i64(&status, "manifest_version")?, version);
    assert_eq!(as_str(&status, "manifest_root_hash")?, hash);
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    Ok(())
}

#[test]
fn isolated_write_response_fields() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let write = lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "iso-overlay/private.txt", "content": "private\n", "overwrite": true}),
    )?;
    assert_eq!(as_str(&write, "workspace")?, "isolated");
    assert_eq!(as_str(&write, "workspace_mode")?, "isolated");
    assert_eq!(as_str(&write, "mutation_source")?, "isolated_workspace");
    assert_eq!(as_str(&write, "status")?, "committed");
    assert!(!as_bool(&write, "published")?);
    assert!(
        array(&write, "changed_paths")?
            .iter()
            .any(|path| path.as_str() == Some("iso-overlay/private.txt")),
        "isolated write should report the private changed path: {write}"
    );
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    Ok(())
}

#[test]
fn isolated_read_file_sees_private_upperdir() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "iso-overlay/search.txt", "content": "needle\n", "overwrite": true}),
    )?;
    let read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "iso-overlay/search.txt"}),
    )?;
    assert_eq!(as_str(&read, "workspace")?, "isolated");
    assert_eq!(as_str(&read, "content")?, "needle\n");
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    Ok(())
}

#[test]
fn isolated_edit_conflict_response_fields() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "iso-overlay/edit.txt", "content": "present\n", "overwrite": true}),
    )?;
    let edit = lease.call(
        ops::API_V1_EDIT_FILE,
        json!({
            "path": "iso-overlay/edit.txt",
            "edits": [{"old_text": "absent", "new_text": "replacement", "replace_all": false}]
        }),
    )?;

    assert_eq!(as_str(&edit, "workspace")?, "isolated", "{edit}");
    assert_eq!(as_str(&edit, "workspace_mode")?, "isolated", "{edit}");
    assert_eq!(as_str(&edit, "status")?, "aborted_overlap", "{edit}");
    assert!(!as_bool(&edit, "published")?);
    assert_eq!(as_i64(&edit, "applied_edits")?, 0);
    assert_eq!(conflict_reason(&edit), "aborted_overlap");
    assert!(
        conflict_message(&edit).contains("anchor not found"),
        "isolated edit should preserve conflict message: {edit}"
    );
    assert!(
        array(&edit, "changed_paths")?.is_empty(),
        "conflicted isolated edit should not report changed paths: {edit}"
    );
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    Ok(())
}
