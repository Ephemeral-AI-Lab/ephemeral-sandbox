use std::sync::{Arc, Barrier};
use std::thread;

use anyhow::{ensure, Result};
use eos_e2e_test::unique_suffix;
use eos_operation::core::catalog;
use serde_json::json;

use crate::helpers::{pressure_levels, request_with_identity, response_result, result_committed};
use crate::support::{
    as_bool, as_i64, as_str, live_pool_or_skip, reset_isolated_workspaces, wait_for_active_leases,
};

#[test]
fn public_and_isolated_same_path_ladder_1_3_6_12() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let levels = pressure_levels(&pool)?;
    let lease = pool.acquire()?;

    for level in levels {
        reset_isolated_workspaces(&lease);
        let suffix = unique_suffix().replace('-', "_");
        let callers: Vec<String> = (0..level)
            .map(|index| format!("pressure-cross-mode-{level}-{index}-{suffix}"))
            .collect();
        for caller_id in &callers {
            let entered = lease.call_ok(
                catalog::SANDBOX_ISOLATION_ENTER,
                json!({"caller_id": caller_id}),
            )?;
            ensure!(
                !as_str(&entered, "workspace_handle_id")?.is_empty(),
                "isolated enter should return an open handle: {entered}"
            );
        }

        let body = (|| -> Result<()> {
            let barrier = Arc::new(Barrier::new(level * 2));
            let mut handles = Vec::with_capacity(level * 2);
            for (index, caller_id) in callers.iter().enumerate() {
                let path = format!("pressure/cross-mode/level-{level}/item-{index}.txt");

                let isolated_client = lease.client().clone();
                let isolated_root = lease.root().to_owned();
                let isolated_caller = caller_id.clone();
                let isolated_barrier = Arc::clone(&barrier);
                let isolated_path = path.clone();
                handles.push(thread::spawn(move || -> Result<()> {
                    isolated_barrier.wait();
                    let private = request_with_identity(
                        &isolated_client,
                        catalog::SANDBOX_FILE_WRITE,
                        &isolated_root,
                        &isolated_caller,
                        json!({
                            "path": isolated_path,
                            "content": format!("private-{level}-{index}\n"),
                            "overwrite": true
                        }),
                    )?;
                    let private_result = response_result(&private)?;
                    ensure!(
                        as_str(private_result, "workspace")? == "isolated"
                            && !as_bool(private_result, "published")?,
                        "isolated pressure write should stay private: {private}"
                    );
                    Ok(())
                }));

                let public_client = lease.client().clone();
                let public_root = lease.root().to_owned();
                let public_caller = format!("public-cross-mode-{level}-{index}-{suffix}");
                let public_barrier = Arc::clone(&barrier);
                handles.push(thread::spawn(move || -> Result<()> {
                    public_barrier.wait();
                    let public = request_with_identity(
                        &public_client,
                        catalog::SANDBOX_FILE_WRITE,
                        &public_root,
                        &public_caller,
                        json!({
                            "path": path,
                            "content": format!("public-{level}-{index}\n"),
                            "overwrite": true
                        }),
                    )?;
                    let public_result = response_result(&public)?;
                    ensure!(
                        result_committed(public_result),
                        "public pressure write should publish: {public}"
                    );
                    Ok(())
                }));
            }

            for handle in handles {
                handle.join().expect("cross-mode worker panicked")?;
            }

            for (index, caller_id) in callers.iter().enumerate() {
                let path = format!("pressure/cross-mode/level-{level}/item-{index}.txt");
                let private_read = lease.call_ok(
                    catalog::SANDBOX_FILE_READ,
                    json!({"caller_id": caller_id, "path": path}),
                )?;
                ensure!(
                    as_str(&private_read, "content")? == format!("private-{level}-{index}\n"),
                    "isolated caller should keep private same-path content at level {level}: {private_read}"
                );
            }
            Ok(())
        })();

        exit_callers(&lease, &callers);
        body?;

        for index in 0..level {
            let public_read = lease.call_ok(
                catalog::SANDBOX_FILE_READ,
                json!({"path": format!("pressure/cross-mode/level-{level}/item-{index}.txt")}),
            )?;
            ensure!(
                as_str(&public_read, "content")? == format!("public-{level}-{index}\n"),
                "public content should survive isolated discard at level {level}: {public_read}"
            );
        }
        let metrics = wait_for_active_leases(&lease, 0)?;
        ensure!(
            as_i64(&metrics, "active_leases")? == 0,
            "cross-mode pressure should release all leases at level {level}: {metrics}"
        );
    }
    Ok(())
}

fn exit_callers(lease: &eos_e2e_test::NodeLease<'_>, callers: &[String]) {
    for caller_id in callers {
        let _ = lease.call(
            catalog::SANDBOX_ISOLATION_EXIT,
            json!({"caller_id": caller_id, "grace_s": 0.1}),
        );
    }
}
