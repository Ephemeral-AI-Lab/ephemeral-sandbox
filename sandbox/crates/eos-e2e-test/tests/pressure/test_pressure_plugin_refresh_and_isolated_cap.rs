use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_e2e_test::unique_suffix;
use eos_operation::core::catalog;
use serde_json::{json, Value};

use crate::helpers::{
    optional_response_result, pressure_levels, request_with_identity, response_result,
    result_committed,
};
use crate::support::{
    as_bool, as_i64, as_str, finalize_foreground_command, live_pool_or_skip,
    reset_isolated_workspaces, wait_for_active_leases,
};

#[test]
fn plugin_refresh_ladder_1_3_6_12() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let levels = pressure_levels(&pool)?;
    let lease = pool.acquire()?;
    let digest = format!("digest-{}", unique_suffix().replace('-', "_"));
    let setup_digest = format!("setup-{digest}");
    ensure_generic_service_package(&lease, &digest, &setup_digest)?;

    for level in levels {
        let path = format!("pressure/plugin/refresh-level-{level}.txt");
        let content = format!("plugin-refresh-level-{level}\n");
        lease.call_ok(
            catalog::SANDBOX_FILE_WRITE,
            json!({"path": path, "content": content, "overwrite": true}),
        )?;
        let before =
            generic_refresh_count(&lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?)?;

        let barrier = Arc::new(Barrier::new(level));
        let handles: Vec<_> = (0..level)
            .map(|index| {
                let client = lease.client().clone();
                let root = lease.root().to_owned();
                let caller_id = lease.caller_id().to_owned();
                let path = path.clone();
                let content = content.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let response = request_with_identity(
                        &client,
                        "plugin.generic.query",
                        &root,
                        &caller_id,
                        json!({"path": path, "request": format!("level-{level}-{index}")}),
                    )?;
                    let result = response_result(&response)?.clone();
                    assert_eq!(
                        result["content"], content,
                        "plugin dispatch should observe refreshed workspace content at level {level}: {result}"
                    );
                    Ok::<Value, anyhow::Error>(result)
                })
            })
            .collect();

        for handle in handles {
            let response = handle.join().expect("plugin dispatch thread panicked")?;
            assert!(
                as_bool(&response, "success")?,
                "plugin dispatch should succeed at level {level}: {response}"
            );
        }

        let status = lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?;
        let after = generic_refresh_count(&status)?;
        assert!(
            after >= before,
            "plugin refresh count should be monotonic at level {level}: {status}"
        );
        assert!(
            after - before <= u64::try_from(level).unwrap_or(u64::MAX),
            "plugin refresh count should remain bounded by concurrent dispatches at level {level}: {status}"
        );
        assert!(
            status
                .get("running_service_processes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
                <= 1,
            "remount refresh pressure should not spawn extra generic workers at level {level}: {status}"
        );
    }
    Ok(())
}

#[test]
fn isolated_handle_cap_ladder() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let levels = pressure_levels(&pool)?;
    let max_level = levels.iter().copied().max().unwrap_or(1);
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);

    for level in levels {
        let callers: Vec<String> = (0..level)
            .map(|index| format!("{}-iws-{level}-{index}", lease.caller_id()))
            .collect();
        let responses = enter_isolated_callers(&lease, &callers)?;
        let mut opened = Vec::new();
        let mut rejected = Vec::new();
        for (caller, response) in callers.iter().zip(responses.iter()) {
            match optional_response_result(response)? {
                Some(result)
                    if result
                        .get("workspace_handle_id")
                        .and_then(Value::as_str)
                        .is_some_and(|handle| !handle.is_empty()) =>
                {
                    assert!(
                        !as_str(result, "workspace_handle_id")?.is_empty(),
                        "successful enter should return a handle id at level {level}: {result}"
                    );
                    opened.push(caller.clone());
                }
                _ => {
                    assert_stable_isolated_cap_error(response, level)?;
                    rejected.push(response.clone());
                }
            }
        }

        let listing = lease.call_ok(catalog::SANDBOX_ISOLATION_LIST_OPEN, json!({}))?;
        let open_count = open_count_for(&listing, &opened);
        assert_eq!(
            open_count,
            opened.len(),
            "list_open should report all successful isolated handles at level {level}: {listing}"
        );

        if rejected.is_empty() && level == max_level {
            let extra = format!("{}-iws-extra-{level}", lease.caller_id());
            let response = enter_isolated_callers(&lease, std::slice::from_ref(&extra))?
                .pop()
                .context("extra isolated enter response")?;
            if optional_response_result(&response)?.is_some_and(|result| {
                result
                    .get("workspace_handle_id")
                    .and_then(Value::as_str)
                    .is_some_and(|handle| !handle.is_empty())
            }) {
                opened.push(extra);
            } else {
                let kind = response_error_kind(&response).unwrap_or_default();
                assert_eq!(
                    kind, "quota_exceeded",
                    "one handle above the configured ladder cap should reject with quota_exceeded: {response}"
                );
            }
        }

        exit_isolated_callers(&lease, &opened);
        reset_isolated_workspaces(&lease);
    }
    Ok(())
}

#[test]
fn protocol_only_bundled_sandbox_capstone() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    let suffix = eos_e2e_test::unique_suffix();
    let digest = format!("digest-{}", suffix.replace('-', "_"));
    let setup_digest = format!("setup-{digest}");

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": "capstone/scaffold.txt", "content": "scaffold\n", "overwrite": true}),
    )?;
    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "mkdir -p capstone && printf exec > capstone/exec.txt",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,}),
    )?;
    // Settle the yielded exec under emulation before asserting its terminal status.
    let exec = finalize_foreground_command(&lease, exec, Instant::now() + Duration::from_secs(15))?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");

    let conflict_path = "capstone/conflict.txt";
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["left", "right"]
        .into_iter()
        .map(|label| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = lease.caller_id().to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                request_with_identity(
                    &client,
                    catalog::SANDBOX_FILE_WRITE,
                    &root,
                    &caller_id,
                    json!({
                        "path": conflict_path,
                        "content": format!("{label}\n"),
                        "overwrite": true
                    }),
                )
            })
        })
        .collect();
    let conflict_responses = handles
        .into_iter()
        .map(|handle| handle.join().expect("capstone conflict writer panicked"))
        .collect::<Result<Vec<_>>>()?;
    assert!(
        conflict_responses.iter().any(|response| {
            optional_response_result(response)
                .ok()
                .flatten()
                .is_some_and(result_committed)
        }),
        "capstone same-path pressure should commit at least one writer: {conflict_responses:?}"
    );

    for version in 0..12 {
        lease.call_ok(
            catalog::SANDBOX_FILE_WRITE,
            json!({
                "path": "capstone/squash.txt",
                "content": format!("squash-{version}\n"),
                "overwrite": true
            }),
        )?;
    }

    let isolated_caller = format!("capstone-iws-{suffix}");
    lease.call_ok(
        catalog::SANDBOX_ISOLATION_ENTER,
        json!({"caller_id": isolated_caller}),
    )?;
    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "caller_id": isolated_caller,
            "path": "capstone/private.txt",
            "content": "private\n",
            "overwrite": true
        }),
    )?;
    lease.call_ok(
        catalog::SANDBOX_ISOLATION_EXIT,
        json!({"caller_id": isolated_caller, "grace_s": 0.1}),
    )?;
    let private_read = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": "capstone/private.txt"}),
    )?;
    assert!(
        !as_bool(&private_read, "exists")?,
        "isolated capstone write must be private after exit: {private_read}"
    );
    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0, "{metrics}");

    let commit = lease.call_ok(
        catalog::SANDBOX_CHECKPOINT_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(as_bool(&commit, "success")?, "{commit}");
    let scaffold = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": "capstone/scaffold.txt"}),
    )?;
    assert_eq!(as_str(&scaffold, "content")?, "scaffold\n", "{scaffold}");
    let exec_read = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": "capstone/exec.txt"}),
    )?;
    assert_eq!(as_str(&exec_read, "content")?, "exec", "{exec_read}");

    ensure_generic_service_package(&lease, &digest, &setup_digest)?;
    let plugin = lease.call_ok(
        "plugin.generic.query",
        json!({"path": "capstone/scaffold.txt", "request": "capstone"}),
    )?;
    assert_eq!(plugin["success"], true, "{plugin}");
    assert_eq!(plugin["content"], "scaffold\n", "{plugin}");
    Ok(())
}

fn enter_isolated_callers(
    lease: &eos_e2e_test::NodeLease<'_>,
    callers: &[String],
) -> Result<Vec<Value>> {
    let barrier = Arc::new(Barrier::new(callers.len()));
    let handles: Vec<_> = callers
        .iter()
        .map(|caller_id| {
            let client = lease.client().clone();
            let root = lease.root().to_owned();
            let caller_id = caller_id.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                request_with_identity(
                    &client,
                    catalog::SANDBOX_ISOLATION_ENTER,
                    &root,
                    &caller_id,
                    json!({}),
                )
            })
        })
        .collect();

    handles
        .into_iter()
        .map(|handle| handle.join().expect("isolated enter thread panicked"))
        .collect()
}

fn exit_isolated_callers(lease: &eos_e2e_test::NodeLease<'_>, callers: &[String]) {
    for caller_id in callers {
        let _ = lease.call(
            catalog::SANDBOX_ISOLATION_EXIT,
            json!({"caller_id": caller_id, "grace_s": 0.0}),
        );
    }
}

fn assert_stable_isolated_cap_error(response: &Value, level: usize) -> Result<()> {
    let kind = response_error_kind(response).unwrap_or_default();
    if matches!(kind, "quota_exceeded" | "host_ram_pressure") {
        return Ok(());
    }
    bail!("isolated handle pressure returned an unexpected error at level {level}: {response}")
}

fn response_error_kind(response: &Value) -> Option<&str> {
    response
        .get("error")
        .and_then(|error| error.get("kind"))
        .and_then(Value::as_str)
}

fn open_count_for(listing: &Value, callers: &[String]) -> usize {
    listing
        .get("open_caller_ids")
        .and_then(Value::as_array)
        .map(|open| {
            callers
                .iter()
                .filter(|caller| {
                    open.iter()
                        .any(|value| value.as_str().is_some_and(|open| open == caller.as_str()))
                })
                .count()
        })
        .unwrap_or_default()
}

fn ensure_generic_service_package(
    lease: &eos_e2e_test::NodeLease<'_>,
    digest: &str,
    setup_digest: &str,
) -> Result<Value> {
    let warm = lease.call_ok(
        catalog::SANDBOX_PLUGIN_ENSURE,
        json!({
            "workspace_root": lease.workspace_root(),
            "manifest": service_manifest(digest, setup_digest),
            "start_services": true,
        }),
    )?;
    assert_eq!(warm["needs_upload"], true, "{warm}");
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
    assert_eq!(cold["success"], true, "{cold}");
    assert_eq!(cold["service_processes_started"], true, "{cold}");
    let routes = cold
        .get("connected_ppc_routes")
        .and_then(Value::as_array)
        .context("connected_ppc_routes array")?;
    assert!(
        routes
            .iter()
            .any(|route| route.as_str() == Some("plugin.generic.query")),
        "generic service route should be connected: {cold}"
    );
    Ok(cold)
}

fn service_manifest(digest: &str, setup_digest: &str) -> Value {
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
            "refresh_strategy": "remount_workspace_and_notify",
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
        manifest_key = body.get("target_manifest_key") or body.get("manifest_key") or manifest_key
        refresh_events += 1
        send(request["invocation_id"], {{"manifest_key": manifest_key, "accepted": True}})
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
    }})
PY
chmod +x "$pkg/runtime/server.py"
"#
    );
    // Stage through the daemon container directly: a model-facing `exec_command`
    // runs in the fresh namespace where `/eos` is a masked empty tmpfs and cannot
    // write the upload tree the daemon reads back.
    lease
        .container()
        .exec(&["sh", "-lc", &cmd])
        .context("stage generic service package")?;
    Ok(staged)
}

fn generic_refresh_count(status: &Value) -> Result<u64> {
    let loaded_plugins = status
        .get("loaded_plugins")
        .and_then(Value::as_array)
        .context("status.loaded_plugins array")?;
    for plugin in loaded_plugins {
        if plugin.get("name").and_then(Value::as_str) != Some("generic") {
            continue;
        }
        let services = plugin
            .get("services")
            .and_then(Value::as_array)
            .context("generic services array")?;
        for service in services {
            let service_id = service
                .get("key")
                .and_then(|key| key.get("service_id"))
                .and_then(Value::as_str);
            if service_id == Some("worker") {
                return service
                    .get("refresh_count")
                    .and_then(Value::as_u64)
                    .context("generic worker refresh_count");
            }
        }
    }
    bail!("generic worker service not found in plugin status: {status}")
}
