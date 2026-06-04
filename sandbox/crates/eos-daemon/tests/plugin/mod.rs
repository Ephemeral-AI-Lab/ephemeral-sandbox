mod connected_ppc;
mod status;
mod support;

use super::*;

use support::*;

use crate::dispatcher::OpTable;
use eos_layerstack::LayerStack;
use eos_protocol::Request;
use std::io::Write;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

#[test]
fn ensure_records_manifest_services_and_status_lists_them() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let response = op_ensure(
        &json!({
            "manifest": lsp_manifest("digest-a", "hover"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/eos/plugin/workspace"
        }),
        DispatchContext::empty(),
    )?;
    assert_eq!(response["success"], true);
    assert_eq!(response["registered_ops"], json!(["plugin.lsp.hover"]));
    assert_eq!(
        response["operation_routes"][0]["dispatch_mode"],
        "read_only_service"
    );
    assert_eq!(response["services"][0]["state"], "stopped");
    assert_eq!(response["service_processes"][0]["service_id"], "pyright");
    assert!(value_str(
        &response["service_processes"][0]["socket_path"],
        "socket path must be a string"
    )?
    .starts_with("/eos/plugin/ppc/"));

    let status = op_status(&json!({}), DispatchContext::empty())?;
    assert_eq!(status["loaded_plugins"][0]["name"], "lsp");
    Ok(())
}

#[test]
fn ensure_is_idempotent_for_same_digest() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let first = op_ensure(
        &json!({"plugin": "demo", "digest": "a"}),
        DispatchContext::empty(),
    )?;
    let second = op_ensure(
        &json!({"plugin": "demo", "digest": "a"}),
        DispatchContext::empty(),
    )?;
    assert_eq!(first["already_loaded"], false);
    assert_eq!(second["already_loaded"], true);
    Ok(())
}

#[test]
fn ensure_reloads_same_digest_when_workspace_root_changes() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let first = op_ensure(
        &json!({
            "manifest": lsp_manifest("digest-a", "hover"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/testbed"
        }),
        DispatchContext::empty(),
    )?;
    let second = op_ensure(
        &json!({
            "manifest": lsp_manifest("digest-a", "hover"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/ephemeral-os"
        }),
        DispatchContext::empty(),
    )?;

    assert_eq!(first["already_loaded"], false);
    assert_eq!(second["already_loaded"], false);
    assert_eq!(
        first["service_processes"][0]["env"]["EOS_PLUGIN_WORKSPACE_ROOT"],
        "/testbed"
    );
    assert_eq!(
        second["service_processes"][0]["env"]["EOS_PLUGIN_WORKSPACE_ROOT"],
        "/ephemeral-os"
    );
    Ok(())
}

#[test]
fn build_workspace_base_reset_stops_plugin_service_snapshots_for_layer_root() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = OpTable::with_builtins();
    let (layer_stack_root, workspace_root) = test_bound_workspace("reset-plugin-service")?;
    let ensure = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-reset-service".to_owned(),
        args: json!({
            "manifest": lsp_manifest("digest-a", "hover"),
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "workspace_root": workspace_root.to_string_lossy().into_owned()
        }),
    });
    assert_eq!(ensure["success"], true);
    let _ = attach_service_snapshot_for_tests("plugin.lsp.hover")?;
    assert_eq!(
        LayerStack::open(layer_stack_root.clone())?.active_lease_count(),
        1
    );

    let reset = table.dispatch(&Request {
        op: "api.build_workspace_base".to_owned(),
        invocation_id: "workspace-base-reset-stops-service".to_owned(),
        args: json!({
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "workspace_root": workspace_root.to_string_lossy().into_owned(),
            "reset": true
        }),
    });

    assert_eq!(reset["success"], true, "{reset:?}");
    assert_eq!(
        LayerStack::open(layer_stack_root.clone())?.active_lease_count(),
        0
    );
    let status = table.dispatch(&Request {
        op: "api.plugin.status".to_owned(),
        invocation_id: "plugin-status-reset-service".to_owned(),
        args: json!({}),
    });
    assert_eq!(
        status["loaded_plugins"][0]["services"][0]["state"],
        "stopped"
    );
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn op_table_registers_plugin_status_and_ensure() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = OpTable::with_builtins();
    let ensure = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-test".to_owned(),
        args: json!({"plugin": "demo", "digest": "a"}),
    });
    assert_eq!(ensure["success"], true);

    let status = table.dispatch(&Request {
        op: "api.plugin.status".to_owned(),
        invocation_id: "plugin-status-test".to_owned(),
        args: json!({}),
    });
    assert_eq!(status["success"], true);
    let loaded = value_array(&status["loaded_plugins"], "loaded_plugins must be an array")?;
    assert!(loaded.iter().any(|plugin| plugin["name"] == "demo"));
    Ok(())
}

#[test]
fn registered_plugin_op_routes_to_deferred_dispatch_not_unknown_op() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = OpTable::with_builtins();
    let ensure = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-test".to_owned(),
        args: json!({
            "manifest": lsp_manifest("digest-a", "hover"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/eos/plugin/workspace"
        }),
    });
    assert_eq!(ensure["success"], true);

    let routed = table.dispatch(&Request {
        op: "plugin.lsp.hover".to_owned(),
        invocation_id: "plugin-hover-test".to_owned(),
        args: json!({"agent_id": "agent-plugin"}),
    });
    assert_eq!(routed["success"], false);
    assert_eq!(routed["status"], "deferred");
    assert_eq!(routed["error"]["kind"], "plugin_dispatch_deferred");
    assert_eq!(routed["dispatch_mode"], "read_only_service");

    let missing = table.dispatch(&Request {
        op: "plugin.lsp.missing".to_owned(),
        invocation_id: "plugin-missing-test".to_owned(),
        args: json!({}),
    });
    assert_eq!(missing["error"]["kind"], "unknown_op");
    Ok(())
}

#[test]
fn dynamic_plugin_op_is_blocked_in_isolated_workspace_before_route_lookup() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = OpTable::with_builtins();
    let (layer_stack_root, _workspace_root) = test_bound_workspace("plugin-iws-block")?;
    let scratch = some_value(
        layer_stack_root.parent(),
        "test layer root must have a parent",
    )?
    .join("scratch");
    let _enabled = TestEnvVar::set("EOS_ISOLATED_WORKSPACE_ENABLED", "true");
    let _harness = TestEnvVar::set("EOS_ISOLATED_WORKSPACE_TEST_HARNESS", "true");
    let _scratch = TestEnvVar::set(
        "EOS_ISOLATED_WORKSPACE_TEST_SCRATCH_ROOT",
        &scratch.to_string_lossy(),
    );

    let _ = table.dispatch(&Request {
        op: "api.isolated_workspace.test_reset".to_owned(),
        invocation_id: "iws-reset-before-plugin-block".to_owned(),
        args: json!({}),
    });
    let entered = table.dispatch(&Request {
        op: "api.isolated_workspace.enter".to_owned(),
        invocation_id: "iws-enter-before-plugin-block".to_owned(),
        args: json!({
            "agent_id": "agent-plugin",
            "layer_stack_root": layer_stack_root.to_string_lossy(),
        }),
    });
    assert_eq!(entered["success"], true);

    let blocked = table.dispatch(&Request {
        op: "plugin.lsp.not_loaded_yet".to_owned(),
        invocation_id: "plugin-dynamic-iws-block".to_owned(),
        args: json!({"agent_id": "agent-plugin"}),
    });
    assert_eq!(blocked["error"]["kind"], "forbidden_in_isolated_workspace");

    let exited = table.dispatch(&Request {
        op: "api.isolated_workspace.exit".to_owned(),
        invocation_id: "iws-exit-after-plugin-block".to_owned(),
        args: json!({"agent_id": "agent-plugin"}),
    });
    assert_eq!(exited["success"], true);
    let _ = table.dispatch(&Request {
        op: "api.isolated_workspace.test_reset".to_owned(),
        invocation_id: "iws-reset-after-plugin-block".to_owned(),
        args: json!({}),
    });
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn exited_service_process_fails_closed_before_dispatch() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = OpTable::with_builtins();
    let (layer_stack_root, workspace_root) = test_bound_workspace("exited-service")?;
    let ensure = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-exited-service".to_owned(),
        args: json!({
            "manifest": lsp_manifest_with_command(
                "digest-a",
                "hover",
                vec!["/bin/sh", "-c", "true"]
            ),
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "workspace_root": workspace_root.to_string_lossy().into_owned()
        }),
    });
    assert_eq!(ensure["success"], true);

    let spec = {
        let state = lock_state()?;
        some_value(
            state
                .loaded
                .values()
                .flat_map(|loaded| loaded.service_processes.iter())
                .next()
                .cloned(),
            "service process spec missing",
        )?
    };
    let service_instance_id = spec.service_instance_id();
    let mut process = spec.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(1);
    while process.status_json()["running"].as_bool().unwrap_or(true) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(process.status_json()["running"], false);
    {
        let mut state = lock_state()?;
        state.service_processes.insert(service_instance_id, process);
    }

    let routed = table.dispatch(&Request {
        op: "plugin.lsp.hover".to_owned(),
        invocation_id: "plugin-hover-exited-service".to_owned(),
        args: json!({"agent_id": "agent-plugin"}),
    });
    assert_eq!(routed["success"], false);
    assert!(
        value_str(
            &routed["error"]["message"],
            "error message must be a string"
        )?
        .contains("process exited before plugin dispatch"),
        "routed response: {routed:?}"
    );

    let status = table.dispatch(&Request {
        op: "api.plugin.status".to_owned(),
        invocation_id: "plugin-status-exited-service".to_owned(),
        args: json!({}),
    });
    assert_eq!(
        status["loaded_plugins"][0]["services"][0]["state"],
        "stopped"
    );
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn ensure_records_oneshot_overlay_route_without_starting_process() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let response = op_ensure(
        &json!({
            "manifest": oneshot_overlay_manifest("digest-a", "write"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/eos/plugin/workspace",
            "start_services": true
        }),
        DispatchContext::empty(),
    )?;

    assert_eq!(response["success"], true);
    assert_eq!(response["service_processes"], json!([]));
    assert_eq!(response["service_processes_started"], false);
    assert_eq!(
        response["operation_routes"][0]["dispatch_mode"],
        "write_allowed_oneshot_overlay"
    );
    assert_eq!(
        response["operation_routes"][0]["service_mode"],
        "oneshot_overlay"
    );
    assert_eq!(
        response["operation_routes"][0]["service_command"],
        json!(["python3", "/eos/plugin/oneshot.py"])
    );
    assert_eq!(
        response["services"][0]["last_error"],
        "oneshot overlay worker starts per operation"
    );
    Ok(())
}

#[test]
fn digest_reload_replaces_dynamic_plugin_routes() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = OpTable::with_builtins();
    let first = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-a".to_owned(),
        args: json!({
            "manifest": lsp_manifest("digest-a", "hover"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/eos/plugin/workspace"
        }),
    });
    assert_eq!(first["registered_ops"], json!(["plugin.lsp.hover"]));

    let second = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-b".to_owned(),
        args: json!({
            "manifest": lsp_manifest("digest-b", "diagnostics"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/eos/plugin/workspace"
        }),
    });
    assert_eq!(second["registered_ops"], json!(["plugin.lsp.diagnostics"]));

    let old = table.dispatch(&Request {
        op: "plugin.lsp.hover".to_owned(),
        invocation_id: "plugin-hover-old".to_owned(),
        args: json!({}),
    });
    assert_eq!(old["error"]["kind"], "unknown_op");

    let current = table.dispatch(&Request {
        op: "plugin.lsp.diagnostics".to_owned(),
        invocation_id: "plugin-diagnostics-current".to_owned(),
        args: json!({}),
    });
    assert_eq!(current["error"]["kind"], "plugin_dispatch_deferred");
    Ok(())
}

#[test]
fn plugin_response_payload_rejects_over_8_mib_body() {
    let reply = PpcEnvelope {
        message_id: "plugin-large-reply".to_owned(),
        direction: PpcDirection::Reply,
        op: "reply".to_owned(),
        body: "x".repeat(MAX_PLUGIN_RESPONSE_BYTES + 1),
    };

    assert!(matches!(
        response_payload_from_reply(&reply),
        Err(DaemonError::Plugin(PluginError::Ppc(message)))
            if message.contains("plugin response exceeds")
    ));
}

#[test]
fn plugin_caller_fields_reject_nul_long_and_non_string_values() {
    assert!(matches!(
        validate_plugin_caller_fields(&json!({"agent_id": "agent\0plugin"})),
        Err(DaemonError::Plugin(PluginError::Ppc(message)))
            if message.contains("contains NUL")
    ));

    assert!(matches!(
        validate_plugin_caller_fields(&json!({"caller": {"task_id": "x".repeat(MAX_PLUGIN_CALLER_FIELD_CHARS + 1)}})),
        Err(DaemonError::Plugin(PluginError::Ppc(message)))
            if message.contains("exceeds")
    ));

    assert!(matches!(
        validate_plugin_caller_fields(&json!({"caller": {"request_id": 42}})),
        Err(DaemonError::Plugin(PluginError::Ppc(message)))
            if message.contains("must be a string")
    ));
}

#[test]
fn read_only_service_refreshes_after_peer_publish_before_request() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = OpTable::with_builtins();
    let (layer_stack_root, workspace_root) = test_bound_workspace("read-only-refresh")?;
    let ensure = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-test".to_owned(),
        args: json!({
            "manifest": lsp_manifest("digest-a", "hover"),
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "workspace_root": workspace_root.to_string_lossy().into_owned()
        }),
    });
    assert_eq!(ensure["success"], true);

    let (client_stream, mut server_stream) = ppc_stream_pair()?;
    register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;

    let write = table.dispatch(&Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: "peer-write".to_owned(),
        args: json!({
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "path": workspace_root.join("peer.txt").to_string_lossy().into_owned(),
            "content": "peer\n"
        }),
    });
    assert_eq!(write["success"], true, "write response: {write:?}");

    let server = std::thread::spawn(move || -> TestResult {
        let mut refresh_types = Vec::new();
        let mut current_manifest_key = String::new();
        loop {
            let request = read_ppc_request(&mut server_stream, "read ppc request")?;
            if request.op == WORKSPACE_SNAPSHOT_REFRESH_OP {
                let body: Value = serde_json::from_str(&request.body)?;
                refresh_types
                    .push(value_str(&body["type"], "refresh type must be a string")?.to_owned());
                if let Some(key) = body
                    .get("target_manifest_key")
                    .or_else(|| body.get("manifest_key"))
                    .and_then(Value::as_str)
                {
                    current_manifest_key = key.to_owned();
                }
                let refresh_reply = json!({
                        "manifest_key": current_manifest_key,
                        "accepted": true
                });
                write_ppc_reply_json_result(
                    &mut server_stream,
                    request.message_id,
                    &refresh_reply,
                )?;
                continue;
            }

            assert_eq!(request.message_id, "plugin-hover-after-peer-write");
            assert_eq!(request.op, "plugin.lsp.hover");
            assert!(refresh_types.contains(&"prepare_refresh".to_owned()));
            assert!(refresh_types.contains(&"swap_workspace".to_owned()));
            assert!(refresh_types.contains(&"health".to_owned()));
            write_ppc_reply_result(
                &mut server_stream,
                request.message_id,
                r#"{"success":true,"after_refresh":true}"#,
            )?;
            break Ok(());
        }
    });

    let routed = table.dispatch(&Request {
        op: "plugin.lsp.hover".to_owned(),
        invocation_id: "plugin-hover-after-peer-write".to_owned(),
        args: json!({"agent_id": "agent-plugin"}),
    });
    assert_eq!(routed["success"], true, "routed response: {routed:?}");
    assert_eq!(routed["after_refresh"], true);

    let status = table.dispatch(&Request {
        op: "api.plugin.status".to_owned(),
        invocation_id: "plugin-status-after-refresh".to_owned(),
        args: json!({}),
    });
    assert_eq!(status["loaded_plugins"][0]["services"][0]["state"], "ready");
    assert_eq!(
        status["loaded_plugins"][0]["services"][0]["refresh_count"],
        1
    );
    join_test_thread(server, "server thread panicked")?;
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn concurrent_read_only_refresh_is_singleflight_before_requests() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = Arc::new(OpTable::with_builtins());
    let (layer_stack_root, workspace_root) =
        test_bound_workspace("read-only-refresh-singleflight")?;
    let ensure = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-test".to_owned(),
        args: json!({
            "manifest": lsp_manifest("digest-a", "hover"),
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "workspace_root": workspace_root.to_string_lossy().into_owned()
        }),
    });
    assert_eq!(ensure["success"], true);

    let (client_stream, mut server_stream) = ppc_stream_pair()?;
    register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;

    let write = table.dispatch(&Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: "peer-write".to_owned(),
        args: json!({
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "path": workspace_root.join("peer.txt").to_string_lossy().into_owned(),
            "content": "peer\n"
        }),
    });
    assert_eq!(write["success"], true, "write response: {write:?}");

    let (refresh_started_tx, refresh_started_rx) = mpsc::channel();
    let (continue_refresh_tx, continue_refresh_rx) = mpsc::channel();
    let server = std::thread::spawn(move || -> TestResult {
        let mut refresh_types = Vec::new();
        let mut current_manifest_key = String::new();
        let first_op = loop {
            let request = read_ppc_request(&mut server_stream, "read ppc request")?;
            if request.op != WORKSPACE_SNAPSHOT_REFRESH_OP {
                break request;
            }
            let body: Value = serde_json::from_str(&request.body)?;
            let refresh_type =
                value_str(&body["type"], "refresh type must be a string")?.to_owned();
            if refresh_types.is_empty() {
                assert_eq!(refresh_type, "prepare_refresh");
                refresh_started_tx.send(())?;
                continue_refresh_rx.recv_timeout(Duration::from_secs(1))?;
            }
            refresh_types.push(refresh_type);
            if let Some(key) = body
                .get("target_manifest_key")
                .or_else(|| body.get("manifest_key"))
                .and_then(Value::as_str)
            {
                current_manifest_key = key.to_owned();
            }
            let refresh_reply = json!({
                "manifest_key": current_manifest_key,
                "accepted": true
            });
            write_ppc_reply_json_result(&mut server_stream, request.message_id, &refresh_reply)?;
        };
        assert_eq!(
            refresh_types,
            vec![
                "prepare_refresh".to_owned(),
                "quiesce".to_owned(),
                "swap_workspace".to_owned(),
                "notify_refresh".to_owned(),
                "resume".to_owned(),
                "health".to_owned(),
            ]
        );

        let second_op = read_ppc_request(&mut server_stream, "read second plugin request")?;
        let mut message_ids = vec![first_op.message_id.clone(), second_op.message_id.clone()];
        message_ids.sort();
        assert_eq!(
            message_ids,
            vec![
                "plugin-hover-concurrent-refresh-a".to_owned(),
                "plugin-hover-concurrent-refresh-b".to_owned(),
            ]
        );
        assert_eq!(first_op.op, "plugin.lsp.hover");
        assert_eq!(second_op.op, "plugin.lsp.hover");
        write_ppc_reply_result(
            &mut server_stream,
            second_op.message_id,
            r#"{"success":true,"seq":2}"#,
        )?;
        write_ppc_reply_result(
            &mut server_stream,
            first_op.message_id,
            r#"{"success":true,"seq":1}"#,
        )?;
        Ok(())
    });

    let first_table = Arc::clone(&table);
    let first = std::thread::spawn(move || -> Result<Value, TestError> {
        Ok(first_table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-concurrent-refresh-a".to_owned(),
            args: json!({"agent_id": "agent-plugin", "request": "a"}),
        }))
    });
    refresh_started_rx.recv_timeout(Duration::from_secs(1))?;

    let (second_started_tx, second_started_rx) = mpsc::channel();
    let second_table = Arc::clone(&table);
    let second = std::thread::spawn(move || -> Result<Value, TestError> {
        second_started_tx.send(())?;
        Ok(second_table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-concurrent-refresh-b".to_owned(),
            args: json!({"agent_id": "agent-plugin", "request": "b"}),
        }))
    });
    second_started_rx.recv_timeout(Duration::from_secs(1))?;
    continue_refresh_tx.send(())?;

    let first_response = join_value_thread(first, "first dispatch thread panicked")?;
    let second_response = join_value_thread(second, "second dispatch thread panicked")?;
    assert_eq!(first_response["success"], true);
    assert_eq!(second_response["success"], true);

    let status = table.dispatch(&Request {
        op: "api.plugin.status".to_owned(),
        invocation_id: "plugin-status-after-refresh-singleflight".to_owned(),
        args: json!({}),
    });
    assert_eq!(status["loaded_plugins"][0]["services"][0]["state"], "ready");
    assert_eq!(
        status["loaded_plugins"][0]["services"][0]["refresh_count"],
        1
    );
    join_test_thread(server, "server thread panicked")?;
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn restart_service_strategy_restarts_after_peer_publish_before_request() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = OpTable::with_builtins();
    let socket_root = test_socket_root("restart-service");
    let (layer_stack_root, workspace_root) = test_bound_workspace("restart-service")?;
    let (allow_reconnect_tx, allow_reconnect_rx) = mpsc::channel();
    let connector = spawn_restart_connector(
        socket_root.clone(),
        allow_reconnect_rx,
        r#"{"success":true,"from_restart_service":true}"#,
    );
    let command = vec![
        "/bin/sh",
        "-c",
        "test \"$EOS_PLUGIN_SERVICE_ID\" = pyright && sleep 30",
    ];
    let ensure = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-restart-service".to_owned(),
        args: json!({
            "manifest": lsp_restart_manifest("digest-a", "hover", command),
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "workspace_root": workspace_root.to_string_lossy().into_owned(),
            "ppc_socket_root": socket_root.to_string_lossy().into_owned(),
            "start_services": true
        }),
    });
    assert_eq!(ensure["success"], true, "ensure response: {ensure:?}");
    assert_eq!(ensure["service_processes_started"], true);

    let status_before = table.dispatch(&Request {
        op: "api.plugin.status".to_owned(),
        invocation_id: "plugin-status-before-restart".to_owned(),
        args: json!({}),
    });
    assert_eq!(
        status_before["loaded_plugins"][0]["services"][0]["restart_count"],
        0
    );
    let initial_manifest_key = value_str(
        &status_before["loaded_plugins"][0]["services"][0]["manifest_key"],
        "initial manifest key must be a string",
    )?
    .to_owned();

    let write = table.dispatch(&Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: "peer-write-before-restart".to_owned(),
        args: json!({
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "path": workspace_root.join("restart-peer.txt").to_string_lossy().into_owned(),
            "content": "peer restart\n"
        }),
    });
    assert_eq!(write["success"], true, "write response: {write:?}");
    allow_reconnect_tx.send(())?;

    let routed = table.dispatch(&Request {
        op: "plugin.lsp.hover".to_owned(),
        invocation_id: "plugin-hover-after-restart".to_owned(),
        args: json!({"agent_id": "agent-plugin"}),
    });
    assert_eq!(routed["success"], true, "routed response: {routed:?}");
    assert_eq!(routed["from_restart_service"], true);

    let status_after = table.dispatch(&Request {
        op: "api.plugin.status".to_owned(),
        invocation_id: "plugin-status-after-restart".to_owned(),
        args: json!({}),
    });
    let service = &status_after["loaded_plugins"][0]["services"][0];
    assert_eq!(service["state"], "ready");
    assert_eq!(service["refresh_count"], 0);
    assert_eq!(service["restart_count"], 1);
    assert_ne!(
        value_str(
            &service["manifest_key"],
            "restarted manifest key must be a string"
        )?,
        initial_manifest_key
    );

    join_test_thread(connector, "connector thread panicked")?;
    let _ = std::fs::remove_dir_all(socket_root);
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn connected_self_managed_plugin_op_services_occ_callback() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let layer_stack_root = test_layer_stack_root("self-managed-callback")?;
    let table = OpTable::with_builtins();
    let ensure = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-test".to_owned(),
        args: json!({
            "manifest": lsp_self_managed_manifest("digest-a", "apply"),
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "workspace_root": "/eos/plugin/workspace"
        }),
    });
    assert_eq!(ensure["success"], true);
    assert_eq!(
        ensure["operation_routes"][0]["dispatch_mode"],
        "self_managed_callback"
    );

    let (client_stream, mut server_stream) = ppc_stream_pair()?;
    register_ppc_client_for_tests("plugin.lsp.apply", client_stream)?;
    let callback_root = layer_stack_root.clone();
    let server = std::thread::spawn(move || -> TestResult {
        let request = read_ppc_request(&mut server_stream, "read ppc request")?;
        assert_eq!(request.message_id, "plugin-apply-test");
        assert_eq!(request.op, "plugin.lsp.apply");

        let callback = PpcEnvelope {
            message_id: "plugin-apply-callback".to_owned(),
            direction: PpcDirection::Request,
            op: occ_callbacks::OCC_APPLY_CHANGESET_OP.to_owned(),
            body: serde_json::to_string(&json!({
                "layer_stack_root": callback_root.to_string_lossy().into_owned(),
                "changes": [{
                    "kind": "write",
                    "path": "src/main.py",
                    "content_utf8": "print('from callback')\n"
                }]
            }))?,
        };
        server_stream.write_all(&callback.encode()?)?;
        let callback_reply = read_ppc_request(&mut server_stream, "read callback reply")?;
        assert_eq!(callback_reply.message_id, "plugin-apply-callback");
        let callback_body: Value = serde_json::from_str(&callback_reply.body)?;
        assert_eq!(callback_body["success"], true);
        assert_eq!(callback_body["files"][0]["status"], "committed");

        write_ppc_reply_result(
            &mut server_stream,
            request.message_id,
            r#"{"success":true,"from_self_managed":true}"#,
        )?;
        Ok(())
    });

    let routed = table.dispatch(&Request {
        op: "plugin.lsp.apply".to_owned(),
        invocation_id: "plugin-apply-test".to_owned(),
        args: json!({"agent_id": "agent-plugin"}),
    });
    assert_eq!(routed["success"], true, "routed response: {routed:?}");
    assert_eq!(routed["from_self_managed"], true);
    assert_eq!(
        read_layer_text(&layer_stack_root, "src/main.py")?,
        "print('from callback')\n"
    );

    join_test_thread(server, "server thread panicked")?;
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn self_managed_service_refreshes_after_peer_publish_before_request() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let table = OpTable::with_builtins();
    let (layer_stack_root, workspace_root) = test_bound_workspace("self-managed-refresh")?;
    let ensure = table.dispatch(&Request {
        op: "api.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-test".to_owned(),
        args: json!({
            "manifest": lsp_self_managed_manifest("digest-a", "apply"),
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "workspace_root": workspace_root.to_string_lossy().into_owned()
        }),
    });
    assert_eq!(ensure["success"], true);

    let (client_stream, mut server_stream) = ppc_stream_pair()?;
    register_ppc_client_for_tests("plugin.lsp.apply", client_stream)?;

    let write = table.dispatch(&Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: "peer-write".to_owned(),
        args: json!({
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "path": workspace_root.join("peer.txt").to_string_lossy().into_owned(),
            "content": "peer\n"
        }),
    });
    assert_eq!(write["success"], true, "write response: {write:?}");

    let server = std::thread::spawn(move || -> TestResult {
        let mut refresh_types = Vec::new();
        let mut current_manifest_key = String::new();
        loop {
            let request = read_ppc_request(&mut server_stream, "read ppc request")?;
            if request.op == WORKSPACE_SNAPSHOT_REFRESH_OP {
                let body: Value = serde_json::from_str(&request.body)?;
                refresh_types
                    .push(value_str(&body["type"], "refresh type must be a string")?.to_owned());
                if let Some(key) = body
                    .get("target_manifest_key")
                    .or_else(|| body.get("manifest_key"))
                    .and_then(Value::as_str)
                {
                    current_manifest_key = key.to_owned();
                }
                let refresh_reply = json!({
                    "manifest_key": current_manifest_key,
                    "accepted": true
                });
                write_ppc_reply_json_result(
                    &mut server_stream,
                    request.message_id,
                    &refresh_reply,
                )?;
                continue;
            }

            assert_eq!(request.message_id, "plugin-apply-after-peer-write");
            assert_eq!(request.op, "plugin.lsp.apply");
            assert!(refresh_types.contains(&"prepare_refresh".to_owned()));
            assert!(refresh_types.contains(&"swap_workspace".to_owned()));
            assert!(refresh_types.contains(&"health".to_owned()));
            write_ppc_reply_result(
                &mut server_stream,
                request.message_id,
                r#"{"success":true,"self_managed_after_refresh":true}"#,
            )?;
            break Ok(());
        }
    });

    let routed = table.dispatch(&Request {
        op: "plugin.lsp.apply".to_owned(),
        invocation_id: "plugin-apply-after-peer-write".to_owned(),
        args: json!({"agent_id": "agent-plugin"}),
    });
    assert_eq!(routed["success"], true, "routed response: {routed:?}");
    assert_eq!(routed["self_managed_after_refresh"], true);

    let status = table.dispatch(&Request {
        op: "api.plugin.status".to_owned(),
        invocation_id: "plugin-status-after-self-managed-refresh".to_owned(),
        args: json!({}),
    });
    assert_eq!(status["loaded_plugins"][0]["services"][0]["state"], "ready");
    assert_eq!(
        status["loaded_plugins"][0]["services"][0]["refresh_count"],
        1
    );
    join_test_thread(server, "server thread panicked")?;
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn ensure_can_start_and_status_reports_service_process() -> TestResult {
    let _guard = PluginTestGuard::new()?;
    let socket_root = test_socket_root("ensure-start");
    let (layer_stack_root, workspace_root) = test_bound_workspace("ensure-start")?;
    let connector = spawn_replying_connector(
        socket_root.clone(),
        r#"{"success":true,"from_started_service":true}"#,
    );
    let command = vec![
        "/bin/sh",
        "-c",
        "test \"$EOS_PLUGIN_SERVICE_ID\" = pyright && sleep 30",
    ];
    let response = op_ensure(
        &json!({
            "manifest": lsp_manifest_with_command("digest-a", "hover", command),
            "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
            "workspace_root": workspace_root.to_string_lossy().into_owned(),
            "ppc_socket_root": socket_root.to_string_lossy().into_owned(),
            "start_services": true
        }),
        DispatchContext::empty(),
    )?;

    assert_eq!(response["success"], true);
    assert_eq!(response["service_processes_started"], true);
    assert_eq!(
        response["running_service_processes"][0]["service_id"],
        "pyright"
    );
    assert_eq!(response["running_service_processes"][0]["running"], true);

    let status = op_status(&json!({}), DispatchContext::empty())?;
    assert_eq!(
        status["running_service_processes"][0]["service_id"],
        "pyright"
    );
    assert_eq!(status["running_service_processes"][0]["running"], true);

    let table = OpTable::with_builtins();
    let routed = table.dispatch(&Request {
        op: "plugin.lsp.hover".to_owned(),
        invocation_id: "plugin-hover-started-service".to_owned(),
        args: json!({"agent_id": "agent-plugin"}),
    });
    assert_eq!(routed["success"], true, "routed response: {routed:?}");
    assert_eq!(routed["from_started_service"], true);

    join_test_thread(connector, "connector thread panicked")?;
    let _ = std::fs::remove_dir_all(socket_root);
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}
