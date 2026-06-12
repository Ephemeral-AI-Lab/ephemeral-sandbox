use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_e2e_test::unique_suffix;
use eos_operation::core::catalog;
use eos_sandbox_host::protocol::TraceWireContext;
use serde_json::{json, Value};

use crate::support::{has_trace_event, live_pool_or_skip, trace_record};

fn assert_connected_routes(value: &Value, expected: &[&str]) -> Result<()> {
    let mut actual = value
        .get("connected_ppc_routes")
        .and_then(Value::as_array)
        .with_context(|| format!("connected_ppc_routes missing from {value}"))?
        .iter()
        .map(|route| {
            route
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("connected_ppc_routes contains non-string route: {value}"))
        })
        .collect::<Result<Vec<_>>>()?;
    actual.sort();
    let mut expected = expected.iter().map(ToString::to_string).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(
        actual, expected,
        "connected PPC routes should match expected set: {value}"
    );
    Ok(())
}

#[test]
fn host_ensure_plugin_package_installs_generic_package() -> Result<()> {
    generic_package_installs_and_sets_up()
}

#[test]
fn generic_package_installs_and_sets_up() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");
    let staged = stage_generic_package(&lease, &digest)?;

    let warm = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": manifest(&digest, &setup_digest),
        }),
    )?;
    assert_eq!(
        warm["needs_upload"], true,
        "missing package should request upload: {warm}"
    );

    let cold = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": manifest(&digest, &setup_digest),
            "staged_package_root": staged,
        }),
    )?;
    assert_eq!(cold["success"], true);
    assert_eq!(cold["package"]["package_published"], true);
    assert_eq!(cold["package"]["setup_ran"], true);

    assert_container_path(
        &lease,
        &format!("/eos/runtime/plugins/catalog/generic/{digest}/.package-sha256"),
    )?;
    assert_container_path(
        &lease,
        &format!("/eos/runtime/plugins/catalog/generic/{digest}/.setup-sha256"),
    )?;
    assert_container_path(
        &lease,
        &format!("/eos/runtime/packages/generic/{digest}/cache/setup.txt"),
    )?;
    assert_container_path(
        &lease,
        &format!("/eos/scratch/setup/generic/{digest}/tmp/setup.tmp"),
    )?;
    Ok(())
}

#[test]
fn generic_package_reensure_is_idempotent() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");
    let staged = stage_generic_package(&lease, &digest)?;

    let _ = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": manifest(&digest, &setup_digest),
            "staged_package_root": staged,
        }),
    )?;
    let warm = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": manifest(&digest, &setup_digest),
        }),
    )?;
    assert_eq!(warm["success"], true);
    assert_eq!(warm["package"]["needs_upload"], false);
    assert_eq!(warm["package"]["setup_ran"], false);
    let count = read_container_file(
        &lease,
        &format!("/eos/runtime/packages/generic/{digest}/cache/setup-count"),
    )?;
    assert_eq!(count.trim(), "1");
    Ok(())
}

#[test]
fn plugin_setup_and_manifest_failures_are_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let malformed_digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let mut malformed = service_manifest(&malformed_digest, &format!("setup-{malformed_digest}"));
    malformed["operations"][0]
        .as_object_mut()
        .context("operation manifest object")?
        .remove("intent");
    let manifest_error = lease.call(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": malformed,
            "start_services": true,
        }),
    )?;
    assert_eq!(
        manifest_error.get("success").and_then(Value::as_bool),
        Some(false),
        "invalid manifest must return a structured error response: {manifest_error}"
    );
    assert!(
        manifest_error
            .get("error")
            .and_then(|error| error.get("kind"))
            .and_then(Value::as_str)
            .is_some(),
        "manifest error must carry a stable error kind: {manifest_error}"
    );
    assert!(
        manifest_error
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("intent"),
        "manifest error should identify the missing intent: {manifest_error}"
    );

    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");
    let staged = stage_generic_package(&lease, &digest)?;
    let mut setup_manifest = manifest(&digest, &setup_digest);
    setup_manifest["setup"]["command"] = json!(["./missing-setup.sh"]);
    let setup_error = lease.call(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": setup_manifest,
            "staged_package_root": staged,
        }),
    )?;
    assert_eq!(
        setup_error.get("success").and_then(Value::as_bool),
        Some(false),
        "setup failure must return a structured error response: {setup_error}"
    );
    assert!(
        setup_error
            .get("error")
            .and_then(|error| error.get("kind"))
            .and_then(Value::as_str)
            .is_some(),
        "setup failure must carry a stable error kind: {setup_error}"
    );
    assert!(
        setup_error
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("missing-setup"),
        "setup failure should identify the failing setup command: {setup_error}"
    );
    Ok(())
}

#[test]
fn generic_plugin_dispatch_roundtrip() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");
    ensure_generic_service_package(&lease, &digest, &setup_digest)?;

    let response = lease.call_ok(
        "plugin.generic.query",
        json!({"path": "missing.txt", "request": "roundtrip"}),
    )?;
    assert_eq!(response["success"], true);
    assert_eq!(response["op"], "plugin.generic.query");
    assert_eq!(response["request"]["request"], "roundtrip");
    assert_eq!(
        response["package_root"],
        format!("/eos/runtime/plugins/catalog/generic/{digest}")
    );
    assert_eq!(
        response["dependency_root"],
        format!("/eos/runtime/packages/generic/{digest}")
    );
    Ok(())
}

#[test]
fn generic_plugin_refreshes_after_workspace_edit() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");
    ensure_generic_service_package(&lease, &digest, &setup_digest)?;

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": "phase5/refresh.txt", "content": "after-refresh\n", "overwrite": true}),
    )?;
    let response = lease.call_ok(
        "plugin.generic.query",
        json!({"path": "phase5/refresh.txt"}),
    )?;
    assert_eq!(response["success"], true);
    assert_eq!(response["content"], "after-refresh\n");
    assert!(
        response["refresh_events"].as_u64().unwrap_or_default() > 0,
        "dispatch after write should refresh service workspace: {response}"
    );

    let status = lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?;
    assert!(
        status["loaded_plugins"][0]["services"][0]["refresh_count"]
            .as_u64()
            .unwrap_or_default()
            > 0,
        "status should record service refresh: {status}"
    );
    Ok(())
}

#[test]
fn concurrent_plugin_refresh_singleflight() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let concurrency = pool
        .workload()
        .concurrency_levels
        .iter()
        .copied()
        .find(|level| *level >= 6)
        .unwrap_or(6);
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");
    ensure_generic_service_package(&lease, &digest, &setup_digest)?;

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": "phase5/singleflight.txt",
            "content": "after-singleflight\n",
            "overwrite": true
        }),
    )?;

    let mut handles = Vec::with_capacity(concurrency);
    for index in 0..concurrency {
        let client = lease.client().clone();
        let root = lease.root().to_owned();
        let caller_id = lease.caller_id().to_owned();
        let invocation_id = format!("plugin-singleflight-{index}-{}", unique_suffix());
        handles.push(thread::spawn(move || {
            client.request(
                "plugin.generic.query",
                &invocation_id,
                &json!({
                    "layer_stack_root": root,
                    "caller_id": caller_id,
                    "path": "phase5/singleflight.txt",
                    "request": format!("singleflight-{index}")
                }),
            )
        }));
    }

    for handle in handles {
        let response = handle
            .join()
            .map_err(|_| anyhow::anyhow!("plugin refresh thread panicked"))??;
        assert_eq!(response["success"], true, "{response}");
        assert_eq!(response["content"], "after-singleflight\n", "{response}");
    }

    let status = lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?;
    assert_eq!(
        service_refresh_count(&status),
        1,
        "concurrent stale dispatches should share one refresh: {status}"
    );
    Ok(())
}

#[test]
fn service_health_probe_reports_connected_service() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");
    ensure_generic_service_package(&lease, &digest, &setup_digest)?;

    // probe_services drives a live PPC health round-trip to the worker, which the
    // generic server already answers; the success path is otherwise untested.
    let status = lease.call_ok(
        catalog::SANDBOX_PLUGIN_STATUS,
        json!({"probe_services": true, "probe_timeout_ms": 5000}),
    )?;
    let health = status
        .get("service_health")
        .and_then(Value::as_array)
        .context("status.service_health array")?;
    assert!(
        !health.is_empty(),
        "probe_services must populate service_health: {status}"
    );
    assert_eq!(
        health[0]["success"],
        json!(true),
        "the service health probe must succeed: {status}"
    );
    assert_eq!(
        health[0]["accepted"],
        json!(true),
        "the worker must accept the health probe: {status}"
    );
    Ok(())
}

#[test]
fn package_reload_reaps_old_service_and_routes() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let first_digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let first_setup_digest = format!("setup-{first_digest}");
    ensure_generic_service_package(&lease, &first_digest, &first_setup_digest)?;
    let first_status = lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?;
    let first_pid = running_service_pid(&first_status)?;

    let second_digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let second_setup_digest = format!("setup-{second_digest}");
    let staged = stage_generic_service_package(&lease, &second_digest)?;
    let upload_root = staged
        .strip_suffix("/package")
        .context("staged package path must end with /package")?
        .to_owned();
    let reloaded = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": service_manifest(&second_digest, &second_setup_digest),
            "staged_package_root": staged,
            "start_services": true,
        }),
    )?;
    assert_eq!(reloaded["success"], true, "{reloaded}");
    assert_eq!(reloaded["service_processes_started"], true, "{reloaded}");
    assert_eq!(
        reloaded["connected_ppc_routes"],
        json!(["plugin.generic.query"]),
        "{reloaded}"
    );

    wait_for_container_path_absent(&lease, &format!("/proc/{first_pid}"))?;
    assert_container_absent(&lease, &upload_root)?;

    let status = lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?;
    assert_eq!(status["loaded_plugins"][0]["digest"], second_digest);
    assert_eq!(
        status["loaded_plugins"][0]["services"][0]["key"]["plugin_digest"],
        second_digest
    );
    assert_eq!(
        status["connected_ppc_routes"],
        json!(["plugin.generic.query"])
    );
    let second_pid = running_service_pid(&status)?;
    assert_ne!(
        second_pid, first_pid,
        "reload should replace the worker process: {status}"
    );

    let routed = lease.call_ok("plugin.generic.query", json!({"path": "missing.txt"}))?;
    assert_eq!(
        routed["package_root"],
        format!("/eos/runtime/plugins/catalog/generic/{second_digest}"),
        "dynamic route should point at the reloaded package: {routed}"
    );
    Ok(())
}

#[test]
fn restart_service_strategy_restarts_on_workspace_edit() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");

    // A different update policy than the covered remount_workspace_and_notify:
    // a workspace edit restarts (kills + respawns) the service process.
    let warm = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": service_manifest_with_strategy(&digest, &setup_digest, "restart_service"),
            "start_services": true,
        }),
    )?;
    assert_eq!(warm["needs_upload"], json!(true), "{warm}");
    let staged = stage_generic_service_package(&lease, &digest)?;
    let cold = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": service_manifest_with_strategy(&digest, &setup_digest, "restart_service"),
            "staged_package_root": staged,
            "start_services": true,
        }),
    )?;
    assert_eq!(cold["service_processes_started"], json!(true), "{cold}");
    assert_eq!(
        restart_count(&lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?),
        0
    );

    // Advance the workspace manifest, then a dispatch forces the refresh, which
    // for restart_service is a process restart (used only to trigger; may defer).
    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": "restart/edit.txt", "content": "after-restart\n", "overwrite": true}),
    )?;
    let _ = lease.call("plugin.generic.query", json!({"path": "restart/edit.txt"}));

    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?;
        if restart_count(&status) >= 1 {
            // A restart bumps restart_count, NOT refresh_count (that is the remount
            // policy's signal) — the discriminating observable between policies.
            assert_eq!(
                status["loaded_plugins"][0]["services"][0]["refresh_count"]
                    .as_i64()
                    .unwrap_or(-1),
                0,
                "restart_service must restart, not remount: {status}"
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("restart_service did not restart the worker after a workspace edit");
        }
        let _ = lease.call("plugin.generic.query", json!({"path": "restart/edit.txt"}));
        thread::sleep(Duration::from_millis(150));
    }
}

#[test]
fn oneshot_overlay_plugin_write_publishes_through_occ() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    ensure_generic_oneshot_package(&lease, &digest)?;
    let before = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?
        ["manifest_version"]
        .as_i64()
        .context("manifest_version before plugin write")?;

    let response = lease.call_ok(
        "plugin.generic.write",
        json!({
            "path": "plugin/oneshot-write.txt",
            "content": "written by oneshot plugin\n"
        }),
    )?;
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["status"], "committed", "{response}");
    assert!(
        response["plugin_overlay"]["changed_paths"]
            .as_array()
            .is_some_and(|paths| paths.iter().any(|path| path == "plugin/oneshot-write.txt")),
        "plugin overlay response should report changed path: {response}"
    );

    let read = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": "plugin/oneshot-write.txt"}),
    )?;
    assert_eq!(read["content"], "written by oneshot plugin\n", "{read}");
    let after = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?
        ["manifest_version"]
        .as_i64()
        .context("manifest_version after plugin write")?;
    assert!(
        after > before,
        "plugin overlay write should publish through daemon OCC: before={before} after={after}"
    );
    Ok(())
}

#[test]
fn live_trace_plugin_callback_occ_publish_parents_under_plugin_op_trace() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");
    ensure_generic_callback_service_package(&lease, &digest, &setup_digest)?;

    let suffix = unique_suffix().replace('-', "_");
    let path = format!("plugin/callback-{suffix}.txt");
    let content = format!("callback publish {suffix}\n");
    let trace_id = format!("phase04-plugin-callback-{suffix}");
    let request_id = format!("{trace_id}-apply");
    let response = lease.call_traced(
        "plugin.generic.apply",
        json!({
            "path": &path,
            "content": &content,
        }),
        &TraceWireContext {
            trace_id: trace_id.clone(),
            request_id: request_id.clone(),
            parent_span_id: None,
            link_hints: Vec::new(),
            capture_budget_version: 1,
        },
    )?;
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["callback"]["files"][0]["status"], "committed");
    let read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": path}))?;
    assert_eq!(read["content"], content, "{read}");

    let record = trace_record(&response)?;
    assert_eq!(record.trace_id.as_str(), trace_id);
    assert_eq!(
        record.request_id.as_ref().map(eos_trace::RequestId::as_str),
        Some(request_id.as_str())
    );
    assert!(
        has_trace_event(&record, "plugin", "callback_request", |details| {
            details.get("parent_message_id").and_then(Value::as_str) == Some(request_id.as_str())
                && details.get("op").and_then(Value::as_str) == Some("daemon.occ.apply_changeset")
        }) && has_trace_event(&record, "plugin", "callback_response", |details| {
            details.get("message_id").and_then(Value::as_str)
                == response.get("callback_message_id").and_then(Value::as_str)
                && details.get("parent_message_id").and_then(Value::as_str)
                    == Some(request_id.as_str())
        }) && has_trace_event(&record, "occ", "commit_finished", |details| {
            details.get("source").and_then(Value::as_str) == Some("plugin_callback")
                && details.get("parent_message_id").and_then(Value::as_str)
                    == Some(request_id.as_str())
                && details.get("success").and_then(Value::as_bool) == Some(true)
                && details
                    .get("committed_count")
                    .and_then(Value::as_u64)
                    .is_some_and(|count| count > 0)
        }),
        "plugin callback trace must parent PPC and OCC publish facts under the plugin op: {:?}",
        record.events
    );
    Ok(())
}

/// `package_reload_reaps_old_service_and_routes` and
/// `concurrent_plugin_refresh_singleflight` cover reload and concurrent refresh
/// SEPARATELY. This races them: N `plugin.generic.query` dispatches run while a
/// package reload (`sandbox.plugin.ensure` with a new staged package) swaps the
/// worker underneath them. Whatever the swap window does, the invariants must
/// hold — the reload succeeds, every concurrent dispatch returns a structured
/// payload (a success, or a structured error during the reap window; never a
/// transport crash), and the post-reload steady state routes to the new package
/// with a single worker.
#[test]
fn concurrent_dispatch_during_reload_stays_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let first_digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let first_setup_digest = format!("setup-{first_digest}");
    ensure_generic_service_package(&lease, &first_digest, &first_setup_digest)?;
    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": "reload-race/probe.txt", "content": "probe\n", "overwrite": true}),
    )?;

    let second_digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let second_setup_digest = format!("setup-{second_digest}");
    let staged = stage_generic_service_package(&lease, &second_digest)?;

    let dispatchers = 6;
    let queries_each = 4;
    let barrier = Arc::new(Barrier::new(dispatchers + 1));

    let reloader = {
        let client = lease.client().clone();
        let root = lease.root().to_owned();
        let caller_id = lease.caller_id().to_owned();
        let workspace_root = lease.workspace_root().to_owned();
        let manifest = service_manifest(&second_digest, &second_setup_digest);
        let staged = staged.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            client.request(
                catalog::SANDBOX_PLUGIN_ENSURE,
                &format!("reload-race-ensure-{}", unique_suffix()),
                &json!({
                    "layer_stack_root": root,
                    "caller_id": caller_id,
                    "workspace_root": workspace_root,
                    "manifest": manifest,
                    "staged_package_root": staged,
                    "start_services": true
                }),
            )
        })
    };

    let dispatch_handles: Vec<_> = (0..dispatchers)
        .map(|index| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = lease.caller_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Result<Vec<Value>> {
                barrier.wait();
                let mut out = Vec::with_capacity(queries_each);
                for query in 0..queries_each {
                    out.push(client.request(
                        "plugin.generic.query",
                        &format!("reload-race-d{index}-q{query}-{}", unique_suffix()),
                        &json!({
                            "layer_stack_root": root,
                            "caller_id": caller_id,
                            "path": "reload-race/probe.txt",
                            "request": format!("dispatch-{index}-{query}")
                        }),
                    )?);
                }
                Ok(out)
            })
        })
        .collect();

    let mut dispatches = Vec::new();
    for handle in dispatch_handles {
        dispatches.extend(
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("reload-race dispatcher panicked"))??,
        );
    }
    let reloaded = reloader
        .join()
        .map_err(|_| anyhow::anyhow!("reload-race reloader panicked"))??;

    assert_eq!(reloaded["success"], true, "reload must succeed: {reloaded}");
    assert_eq!(
        reloaded["connected_ppc_routes"],
        json!(["plugin.generic.query"]),
        "reload must reconnect the route: {reloaded}"
    );

    for dispatch in &dispatches {
        assert!(
            dispatch.is_object()
                && (dispatch.get("success").is_some()
                    || dispatch.get("error").is_some()
                    || dispatch.get("status").is_some()),
            "every dispatch during reload must return a structured payload: {dispatch}"
        );
    }

    // Post-reload steady state routes to the new package via a single worker.
    let routed = lease.call_ok(
        "plugin.generic.query",
        json!({"path": "reload-race/probe.txt", "request": "after-reload"}),
    )?;
    assert_eq!(routed["success"], true, "{routed}");
    assert_eq!(routed["content"], "probe\n", "{routed}");
    let status = lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?;
    assert_eq!(
        status["loaded_plugins"][0]["digest"], second_digest,
        "steady state must run the reloaded digest: {status}"
    );
    assert!(
        status
            .get("running_service_processes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
            <= 1,
        "reload must not strand extra worker processes: {status}"
    );
    Ok(())
}

fn restart_count(status: &Value) -> i64 {
    status["loaded_plugins"][0]["services"][0]["restart_count"]
        .as_i64()
        .unwrap_or(-1)
}

fn manifest(digest: &str, setup_digest: &str) -> Value {
    json!({
        "plugin_id": "generic",
        "plugin_version": "0.1.0",
        "plugin_digest": digest,
        "package": {
            "runtime_dir": "runtime",
            "dependency_scope": "package_digest"
        },
        "setup": {
            "command": ["./setup.sh"],
            "working_dir": ".",
            "setup_marker_digest": setup_digest,
            "timeout_ms": 30000
        },
        "services": [],
        "operations": []
    })
}

fn service_manifest(digest: &str, setup_digest: &str) -> Value {
    service_manifest_with_strategy(digest, setup_digest, "remount_workspace_and_notify")
}

fn callback_service_manifest(digest: &str, setup_digest: &str) -> Value {
    let mut manifest = service_manifest(digest, setup_digest);
    manifest["operations"] = json!([
        {
            "op_name": "query",
            "intent": "read_only",
            "service_id": "worker",
            "timeout_ms": 5000
        },
        {
            "op_name": "apply",
            "intent": "write_allowed",
            "auto_workspace_overlay": false,
            "service_id": "worker",
            "timeout_ms": 5000
        }
    ]);
    manifest
}

fn service_manifest_with_strategy(
    digest: &str,
    setup_digest: &str,
    refresh_strategy: &str,
) -> Value {
    json!({
        "plugin_id": "generic",
        "plugin_version": "0.1.0",
        "plugin_digest": digest,
        "package": {
            "runtime_dir": "runtime",
            "dependency_scope": "package_digest"
        },
        "setup": {
            "command": ["./setup.sh"],
            "working_dir": ".",
            "setup_marker_digest": setup_digest,
            "timeout_ms": 30000
        },
        "services": [{
            "service_id": "worker",
            "service_profile_digest": format!("profile-{digest}"),
            "service_mode": "workspace_snapshot_refresh",
            "refresh_strategy": refresh_strategy,
            "command": ["./server.py"],
            "working_dir": "runtime",
            "ppc_protocol_version": 1
        }],
        "operations": [{
            "op_name": "query",
            "intent": "read_only",
            "service_id": "worker",
            "timeout_ms": 5000
        }]
    })
}

fn oneshot_manifest(digest: &str) -> Value {
    json!({
        "plugin_id": "generic",
        "plugin_version": "0.1.0",
        "plugin_digest": digest,
        "package": {
            "runtime_dir": "runtime",
            "dependency_scope": "package_digest"
        },
        "services": [{
            "service_id": "worker",
            "service_profile_digest": format!("oneshot-profile-{digest}"),
            "service_mode": "oneshot_overlay",
            "refresh_strategy": "restart_service",
            "command": ["./oneshot.py"],
            "working_dir": "runtime",
            "ppc_protocol_version": 1
        }],
        "operations": [{
            "op_name": "write",
            "intent": "write_allowed",
            "service_id": "worker",
            "timeout_ms": 5000
        }]
    })
}

fn stage_generic_package(lease: &eos_e2e_test::NodeLease<'_>, digest: &str) -> Result<String> {
    let staged = format!("/eos/scratch/uploads/plugins/generic/{digest}/upload-1/package");
    let cmd = format!(
        r#"set -eu
pkg="{staged}"
rm -rf "$pkg"
mkdir -p "$pkg/runtime"
printf '%s' "{digest}" > "$pkg/.package-sha256"
printf '{{}}' > "$pkg/sandbox-plugin.json"
printf '#!/bin/sh\n' > "$pkg/runtime/server.sh"
cat > "$pkg/setup.sh" <<'SH'
#!/bin/sh
set -eu
count_file="$EOS_PLUGIN_DEPENDENCY_ROOT/cache/setup-count"
count=0
if [ -f "$count_file" ]; then count="$(cat "$count_file")"; fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf setup-ok > "$EOS_PLUGIN_DEPENDENCY_ROOT/cache/setup.txt"
printf tmp-ok > "$TMPDIR/setup.tmp"
SH
chmod +x "$pkg/setup.sh"
"#
    );
    // Stage through the daemon container directly: a model-facing `exec_command`
    // runs in the fresh namespace where `/eos` is a masked empty tmpfs and cannot
    // write the upload tree the daemon reads back.
    lease
        .container()
        .exec(&["sh", "-lc", &cmd])
        .context("stage generic package")?;
    Ok(staged)
}

fn ensure_generic_service_package(
    lease: &eos_e2e_test::NodeLease<'_>,
    digest: &str,
    setup_digest: &str,
) -> Result<Value> {
    let manifest = service_manifest(digest, setup_digest);
    let warm = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": manifest,
            "start_services": true,
        }),
    )?;
    assert_eq!(warm["needs_upload"], true);
    let staged = stage_generic_service_package(lease, digest)?;
    let cold = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": service_manifest(digest, setup_digest),
            "staged_package_root": staged,
            "start_services": true,
        }),
    )?;
    assert_eq!(cold["success"], true);
    assert_eq!(cold["service_processes_started"], true);
    assert_eq!(
        cold["connected_ppc_routes"],
        json!(["plugin.generic.query"])
    );
    Ok(cold)
}

fn ensure_generic_callback_service_package(
    lease: &eos_e2e_test::NodeLease<'_>,
    digest: &str,
    setup_digest: &str,
) -> Result<Value> {
    let warm = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": callback_service_manifest(digest, setup_digest),
            "start_services": true,
        }),
    )?;
    assert_eq!(warm["needs_upload"], true);
    let staged = stage_generic_service_package(lease, digest)?;
    let cold = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": callback_service_manifest(digest, setup_digest),
            "staged_package_root": staged,
            "start_services": true,
        }),
    )?;
    assert_eq!(cold["success"], true);
    assert_eq!(cold["service_processes_started"], true);
    assert_connected_routes(&cold, &["plugin.generic.query", "plugin.generic.apply"])?;
    Ok(cold)
}

fn ensure_generic_oneshot_package(
    lease: &eos_e2e_test::NodeLease<'_>,
    digest: &str,
) -> Result<Value> {
    let manifest = oneshot_manifest(digest);
    let warm = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": manifest,
        }),
    )?;
    assert_eq!(warm["needs_upload"], true, "{warm}");
    let staged = stage_generic_oneshot_package(lease, digest)?;
    let cold = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": oneshot_manifest(digest),
            "staged_package_root": staged,
            "start_services": true,
        }),
    )?;
    assert_eq!(cold["success"], true, "{cold}");
    assert_eq!(cold["service_processes_started"], false, "{cold}");
    assert_eq!(
        cold["operation_routes"][0]["dispatch_mode"], "write_allowed_oneshot_overlay",
        "{cold}"
    );
    Ok(cold)
}

fn stage_generic_service_package(
    lease: &eos_e2e_test::NodeLease<'_>,
    digest: &str,
) -> Result<String> {
    let staged = format!("/eos/scratch/uploads/plugins/generic/{digest}/upload-1/package");
    let cmd = format!(
        r#"set -eu
pkg="{staged}"
rm -rf "$pkg"
mkdir -p "$pkg/runtime"
printf '%s' "{digest}" > "$pkg/.package-sha256"
printf '{{}}' > "$pkg/sandbox-plugin.json"
cat > "$pkg/setup.sh" <<'SH'
#!/bin/sh
set -eu
printf setup-ok > "$EOS_PLUGIN_DEPENDENCY_ROOT/cache/service-setup.txt"
SH
chmod +x "$pkg/setup.sh"
cat > "$pkg/runtime/server.py" <<'PY'
#!/usr/bin/env python3
import json
import os
import socket

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(os.environ["EOS_PLUGIN_PPC_SOCKET"])
buffer = b""
manifest_key = "initial"
refresh_events = 0

def send(message_id, body):
    frame = {{
        "op": "reply",
        "invocation_id": message_id,
        "args": {{"direction": "reply", "body": json.dumps(body, separators=(",", ":"))}},
    }}
    sock.sendall(json.dumps(frame, separators=(",", ":")).encode() + b"\n")

while True:
    while b"\n" not in buffer:
        chunk = sock.recv(65536)
        if not chunk:
            raise SystemExit(0)
        buffer += chunk
    line, buffer = buffer.split(b"\n", 1)
    request = json.loads(line.decode())
    body = json.loads(request["args"]["body"])
    if request["op"] == "daemon.workspace_snapshot_refresh":
        key = body.get("target_manifest_key") or body.get("manifest_key") or manifest_key
        manifest_key = key
        refresh_events += 1
        send(request["invocation_id"], {{"manifest_key": manifest_key, "accepted": True}})
        continue

    if request["op"] == "plugin.generic.apply":
        relative = body.get("path", "plugin/callback-apply.txt")
        content = body.get("content", "")
        callback_id = request["invocation_id"] + "-occ"
        callback_body = {{
            "layer_stack_root": body["layer_stack_root"],
            "changes": [{{
                "kind": "write",
                "path": relative,
                "content_utf8": content,
            }}],
        }}
        frame = {{
            "op": "daemon.occ.apply_changeset",
            "invocation_id": callback_id,
            "args": {{
                "direction": "request",
                "parent_message_id": request["invocation_id"],
                "body": json.dumps(callback_body, separators=(",", ":")),
            }},
        }}
        sock.sendall(json.dumps(frame, separators=(",", ":")).encode() + b"\n")
        while b"\n" not in buffer:
            chunk = sock.recv(65536)
            if not chunk:
                raise SystemExit(0)
            buffer += chunk
        callback_line, buffer = buffer.split(b"\n", 1)
        callback_reply = json.loads(callback_line.decode())
        callback_reply_body = json.loads(callback_reply["args"]["body"])
        send(request["invocation_id"], {{
            "success": callback_reply_body.get("success"),
            "op": request["op"],
            "callback_message_id": callback_id,
            "callback": callback_reply_body,
            "path": relative,
        }})
        continue

    path = body.get("path")
    content = None
    if path:
        try:
            with open(os.path.join(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], path), "r", encoding="utf-8") as handle:
                content = handle.read()
        except FileNotFoundError:
            content = None
    send(request["invocation_id"], {{
        "success": True,
        "op": request["op"],
        "request": body,
        "content": content,
        "manifest_key": manifest_key,
        "refresh_events": refresh_events,
        "package_root": os.environ["EOS_PLUGIN_PACKAGE_ROOT"],
        "dependency_root": os.environ["EOS_PLUGIN_DEPENDENCY_ROOT"],
    }})
PY
chmod +x "$pkg/runtime/server.py"
"#
    );
    lease
        .container()
        .exec(&["sh", "-lc", &cmd])
        .context("stage generic service package")?;
    Ok(staged)
}

fn stage_generic_oneshot_package(
    lease: &eos_e2e_test::NodeLease<'_>,
    digest: &str,
) -> Result<String> {
    let staged = format!("/eos/scratch/uploads/plugins/generic/{digest}/upload-1/package");
    let cmd = format!(
        r#"set -eu
pkg="{staged}"
rm -rf "$pkg"
mkdir -p "$pkg/runtime"
printf '%s' "{digest}" > "$pkg/.package-sha256"
printf '{{}}' > "$pkg/sandbox-plugin.json"
cat > "$pkg/runtime/oneshot.py" <<'PY'
#!/usr/bin/env python3
import json
import os
from pathlib import Path

request = json.loads(Path(os.environ["EOS_PLUGIN_REQUEST_PATH"]).read_text(encoding="utf-8"))
args = request.get("args", {{}})
relative = args.get("path", "plugin/oneshot.txt")
content = args.get("content", "")
workspace = Path(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"])
target = workspace / relative
target.parent.mkdir(parents=True, exist_ok=True)
target.write_text(content, encoding="utf-8")
Path(os.environ["EOS_PLUGIN_RESULT_PATH"]).write_text(
    json.dumps({{"success": True, "wrote": relative}}, separators=(",", ":")),
    encoding="utf-8",
)
PY
chmod +x "$pkg/runtime/oneshot.py"
"#
    );
    lease
        .container()
        .exec(&["sh", "-lc", &cmd])
        .context("stage generic oneshot package")?;
    Ok(staged)
}

fn assert_container_path(lease: &eos_e2e_test::NodeLease<'_>, path: &str) -> Result<()> {
    // Probe the real container filesystem: published packages and upload roots
    // live under the masked `/eos`, invisible to a model-facing `exec_command`.
    lease
        .container()
        .exec(&["sh", "-lc", &format!("test -f {}", shell_quote(path))])
        .with_context(|| format!("expected container path {path}"))?;
    Ok(())
}

fn assert_container_absent(lease: &eos_e2e_test::NodeLease<'_>, path: &str) -> Result<()> {
    lease
        .container()
        .exec(&["sh", "-lc", &format!("test ! -e {}", shell_quote(path))])
        .with_context(|| format!("expected container path to be absent {path}"))?;
    Ok(())
}

fn wait_for_container_path_absent(lease: &eos_e2e_test::NodeLease<'_>, path: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if assert_container_absent(lease, path).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            assert_container_absent(lease, path)?;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn read_container_file(lease: &eos_e2e_test::NodeLease<'_>, path: &str) -> Result<String> {
    // Read the real container filesystem: setup writes the count under the masked
    // `/eos`, invisible to a model-facing `exec_command`.
    lease
        .container()
        .exec(&["sh", "-lc", &format!("cat {}", shell_quote(path))])
        .with_context(|| format!("read container file {path}"))
}

fn running_service_pid(status: &Value) -> Result<i64> {
    status
        .get("running_service_processes")
        .and_then(Value::as_array)
        .and_then(|processes| {
            processes
                .iter()
                .find(|process| process["running"] == true)
                .and_then(|process| process["pid"].as_i64())
        })
        .with_context(|| format!("running service pid missing in {status}"))
}

fn service_refresh_count(status: &Value) -> i64 {
    status["loaded_plugins"][0]["services"][0]["refresh_count"]
        .as_i64()
        .unwrap_or(-1)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
