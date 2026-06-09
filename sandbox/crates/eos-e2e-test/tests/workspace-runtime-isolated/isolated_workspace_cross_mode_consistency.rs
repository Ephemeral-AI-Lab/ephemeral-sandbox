use anyhow::{ensure, Result};
use eos_e2e_test::{next_invocation_id, unique_suffix};
use eos_protocol::ops;
use serde_json::json;

use crate::support::{
    as_bool, as_i64, as_str, live_pool_or_skip, reset_isolated_workspaces, wait_for_active_leases,
};

#[test]
fn isolated_private_same_path_does_not_overwrite_public_publish() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    let suffix = unique_suffix().replace('-', "_");
    let path = format!("cross-mode/same-path-{suffix}.txt");
    let public_caller = format!("public-cross-mode-{suffix}");

    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": path, "content": "base\n", "overwrite": true}),
    )?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;

    let body = (|| -> Result<()> {
        let private = lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": path, "content": "private\n", "overwrite": true}),
        )?;
        ensure!(
            as_str(&private, "workspace")? == "isolated" && !as_bool(&private, "published")?,
            "same-path isolated write should be private: {private}"
        );

        let public = lease.client().request(
            ops::API_V1_WRITE_FILE,
            &next_invocation_id(),
            &json!({
                "layer_stack_root": lease.root(),
                "caller_id": public_caller,
                "path": path,
                "content": "public\n",
                "overwrite": true
            }),
        )?;
        ensure!(
            as_bool(&public, "success")?,
            "foreign public write should publish while isolated caller remains open: {public}"
        );

        let isolated_read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
        ensure!(
            as_str(&isolated_read, "workspace")? == "isolated",
            "isolated caller should still route to its private workspace: {isolated_read}"
        );
        ensure!(
            as_str(&isolated_read, "content")? == "private\n",
            "isolated caller should keep its private same-path content while open: {isolated_read}"
        );
        Ok(())
    })();

    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({"grace_s": 0.1}));
    body?;
    exit?;

    let public_read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    ensure!(
        as_str(&public_read, "workspace")? == "ephemeral",
        "read after isolated exit should route to public/ephemeral workspace: {public_read}"
    );
    ensure!(
        as_str(&public_read, "content")? == "public\n",
        "public write must survive isolated same-path private discard: {public_read}"
    );
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn isolated_pin_hides_later_public_paths_until_exit() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    let suffix = unique_suffix().replace('-', "_");
    let public_path = format!("cross-mode/public-after-enter-{suffix}.txt");
    let public_caller = format!("pin-public-{suffix}");

    let enter = lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let pinned_version = as_i64(&enter, "manifest_version")?;
    let pinned_hash = as_str(&enter, "manifest_root_hash")?.to_owned();

    let body = (|| -> Result<()> {
        for index in 0..3 {
            let public = lease.client().request(
                ops::API_V1_WRITE_FILE,
                &next_invocation_id(),
                &json!({
                    "layer_stack_root": lease.root(),
                    "caller_id": public_caller,
                    "path": format!("{public_path}-{index}"),
                    "content": format!("public-{index}\n"),
                    "overwrite": true
                }),
            )?;
            ensure!(
                as_bool(&public, "success")?,
                "public write after isolated enter should publish: {public}"
            );
        }

        let status = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
        ensure!(
            as_i64(&status, "manifest_version")? == pinned_version,
            "isolated status should keep the enter-time manifest version: {status}"
        );
        ensure!(
            as_str(&status, "manifest_root_hash")? == pinned_hash,
            "isolated status should keep the enter-time manifest hash: {status}"
        );
        let hidden = lease.call_ok(
            ops::API_V1_READ_FILE,
            json!({"path": format!("{public_path}-2")}),
        )?;
        ensure!(
            as_str(&hidden, "workspace")? == "isolated",
            "read while isolated should stay in isolated mode: {hidden}"
        );
        ensure!(
            !as_bool(&hidden, "exists")?,
            "isolated pinned snapshot should not see later public paths: {hidden}"
        );
        Ok(())
    })();

    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({"grace_s": 0.1}));
    body?;
    exit?;

    let public_read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": format!("{public_path}-2")}),
    )?;
    ensure!(
        as_str(&public_read, "content")? == "public-2\n",
        "public paths written during isolated pin should be visible after exit: {public_read}"
    );
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}
