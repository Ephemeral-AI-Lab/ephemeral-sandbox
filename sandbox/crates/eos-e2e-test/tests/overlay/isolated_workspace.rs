use anyhow::Result;
use eos_protocol::ops;
use serde_json::json;

use crate::common::{array, as_bool, as_i64, as_str, live_pool_or_skip};

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
fn isolated_read_tools_see_private_upperdir() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "iso-overlay/search.txt", "content": "needle\n", "overwrite": true}),
    )?;
    let grep = lease.call_ok(
        ops::API_V1_GREP,
        json!({"pattern": "needle", "path": "iso-overlay", "output_mode": "content"}),
    )?;
    assert_eq!(as_str(&grep, "workspace")?, "isolated");
    assert!(as_str(&grep, "content")?.contains("needle"));
    let glob = lease.call_ok(ops::API_V1_GLOB, json!({"pattern": "iso-overlay/*.txt"}))?;
    assert!(
        array(&glob, "filenames")?
            .iter()
            .any(|path| path.as_str() == Some("iso-overlay/search.txt")),
        "isolated glob should see private upperdir files: {glob}"
    );
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
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
