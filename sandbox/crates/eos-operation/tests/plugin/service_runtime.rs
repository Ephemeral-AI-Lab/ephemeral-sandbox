//! Plugin service runtime behavior: ensure/start, connected PPC dispatch,
//! manifest refresh, restart/recovery, oneshot overlay routing, and health
//! probes — driven through the typed `PluginRuntime` API with a fake
//! ns-runner launcher (services spawn directly, remounts are no-ops). The
//! daemon's wire shaping over these outcomes is covered by daemon tests.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use eos_config::configs::daemon::PluginRuntimeConfig;
use eos_layerstack::{LayerChange, LayerPath, LayerStack};
use eos_operation::plugin::contract::{
    PluginAuditFields, PluginEnsureInput, PluginPackageInput, MAX_PLUGIN_CALLER_FIELD_CHARS,
};
use eos_operation::plugin::ensure::validate_plugin_caller_fields;
use eos_operation::plugin::{
    read_message_bytes, EnsureOutcome, EnsureReady, LaunchError, NsRunnerLauncher,
    PluginDispatchOutcome, PluginRuntime, PluginRuntimeError,
};
use eos_operation::CallerId;
use eos_plugin::{PluginError, PpcDirection, PpcMessage};
use serde_json::{json, Value};

type TestError = Box<dyn std::error::Error + Send + Sync + 'static>;
type TestResult = Result<(), TestError>;

const REFRESH_OP: &str = "daemon.workspace_snapshot_refresh";

#[test]
fn ensure_starts_service_and_dispatches_connected_route() -> TestResult {
    let socket_root = test_socket_root("ensure-start");
    let runtime = test_runtime(&socket_root);
    let (layer_stack_root, workspace_root) = test_bound_workspace("ensure-start")?;
    let connector = spawn_replying_connector(
        socket_root.clone(),
        r#"{"success":true,"from_started_service":true}"#,
    );

    let ready = ensure_started(&runtime, &layer_stack_root, &workspace_root)?;
    assert_eq!(ready.started_count, 1);
    assert!(!ready.already_loaded);
    assert_eq!(ready.running_service_processes.len(), 1);
    assert!(ready.running_service_processes[0].running);
    assert_eq!(ready.running_service_processes[0].service_id, "worker");
    assert_eq!(ready.connected_ppc_services.len(), 1);
    assert_eq!(ready.connected_ppc_routes, vec!["plugin.generic.hover"]);

    let status = runtime.status(false, None)?;
    assert_eq!(status.loaded_plugins.len(), 1);
    assert_eq!(status.loaded_plugins[0].name, "generic");
    assert!(status.running_service_processes[0].running);

    let routed = dispatch_response(&runtime, "plugin.generic.hover", "plugin-hover-started")?;
    assert_eq!(routed["from_started_service"], true);

    join_test_thread(connector, "connector thread panicked")?;
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn concurrent_connected_ops_share_one_client_with_out_of_order_replies() -> TestResult {
    let socket_root = test_socket_root("concurrent-out-of-order");
    let runtime = Arc::new(test_runtime(&socket_root));
    let (layer_stack_root, workspace_root) = test_bound_workspace("concurrent-out-of-order")?;
    let (both_seen_tx, both_seen_rx) = mpsc::channel();
    let server = std::thread::spawn({
        let socket_root = socket_root.clone();
        move || -> TestResult {
            let mut stream = connect_ppc_socket(&socket_root)?;
            let first = read_ppc_request(&mut stream, "read first ppc request")?;
            let second = read_ppc_request(&mut stream, "read second ppc request")?;
            let mut message_ids = vec![first.message_id.clone(), second.message_id.clone()];
            message_ids.sort();
            both_seen_tx.send(message_ids)?;
            // Reply out of order: "concurrent-b" first, then "concurrent-a".
            write_ppc_reply(
                &mut stream,
                "concurrent-b".to_owned(),
                r#"{"success":true,"seq":2}"#,
            )?;
            write_ppc_reply(
                &mut stream,
                "concurrent-a".to_owned(),
                r#"{"success":true,"seq":1}"#,
            )?;
            Ok(())
        }
    });

    ensure_started(&runtime, &layer_stack_root, &workspace_root)?;

    let first_runtime = Arc::clone(&runtime);
    let first = std::thread::spawn(move || -> Result<Value, TestError> {
        dispatch_response(&first_runtime, "plugin.generic.hover", "concurrent-a")
    });
    let second_runtime = Arc::clone(&runtime);
    let second = std::thread::spawn(move || -> Result<Value, TestError> {
        dispatch_response(&second_runtime, "plugin.generic.hover", "concurrent-b")
    });

    let seen = both_seen_rx.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(
        seen,
        vec!["concurrent-a".to_owned(), "concurrent-b".to_owned()]
    );
    let first_response = join_value_thread(first, "first dispatch thread panicked")?;
    let second_response = join_value_thread(second, "second dispatch thread panicked")?;
    assert_eq!(first_response["seq"], 1);
    assert_eq!(second_response["seq"], 2);
    join_test_thread(server, "server thread panicked")?;
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn broken_ppc_drops_connected_route_then_recovers_with_restart() -> TestResult {
    let socket_root = test_socket_root("recover-after-ppc-failure");
    let runtime = test_runtime(&socket_root);
    let (layer_stack_root, workspace_root) = test_bound_workspace("recover-after-ppc-failure")?;
    let connector = std::thread::spawn({
        let socket_root = socket_root.clone();
        move || -> TestResult {
            // Connect for the handshake, then drop the stream immediately.
            let _stream = connect_ppc_socket(&socket_root)?;
            Ok(())
        }
    });
    ensure_started(&runtime, &layer_stack_root, &workspace_root)?;
    join_test_thread(connector, "handshake connector panicked")?;

    let failed = runtime
        .dispatch_registered_op("plugin.generic.hover", "hover-broken", &json!({}))
        .ok_or("route should dispatch")?;
    assert!(failed.is_err(), "broken PPC stream must fail the dispatch");

    let status = runtime.status(false, None)?;
    assert!(status.connected_ppc_routes.is_empty());
    assert!(status.connected_ppc_services.is_empty());
    assert_eq!(
        status.loaded_plugins[0].services[0].state,
        eos_plugin::PluginServiceState::Stopped
    );

    let connector = spawn_replying_connector(
        socket_root.clone(),
        r#"{"success":true,"from_recovered_service":true}"#,
    );
    let recovered = dispatch_response(&runtime, "plugin.generic.hover", "hover-recovered")?;
    assert_eq!(recovered["from_recovered_service"], true);

    let status = runtime.status(false, None)?;
    let service = &status.loaded_plugins[0].services[0];
    assert_eq!(service.state, eos_plugin::PluginServiceState::Ready);
    assert_eq!(service.restart_count, 1);
    assert_eq!(status.connected_ppc_routes, vec!["plugin.generic.hover"]);

    join_test_thread(connector, "connector thread panicked")?;
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn read_only_service_refreshes_after_peer_publish_before_request() -> TestResult {
    let socket_root = test_socket_root("read-only-refresh");
    let runtime = test_runtime(&socket_root);
    let (layer_stack_root, workspace_root) = test_bound_workspace("read-only-refresh")?;
    let server = spawn_refresh_server(
        &socket_root,
        "plugin.generic.hover",
        "hover-after-peer-write",
        r#"{"success":true,"after_refresh":true}"#,
    );
    ensure_started(&runtime, &layer_stack_root, &workspace_root)?;

    publish_peer_change(&layer_stack_root)?;

    let routed = dispatch_response(&runtime, "plugin.generic.hover", "hover-after-peer-write")?;
    assert_eq!(routed["after_refresh"], true);

    let status = runtime.status(false, None)?;
    let service = &status.loaded_plugins[0].services[0];
    assert_eq!(service.state, eos_plugin::PluginServiceState::Ready);
    assert_eq!(service.refresh_count, 1);
    join_test_thread(server, "server thread panicked")?;
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn self_managed_service_refreshes_and_services_occ_callback() -> TestResult {
    let socket_root = test_socket_root("self-managed");
    let runtime = test_runtime(&socket_root);
    let (layer_stack_root, workspace_root) = test_bound_workspace("self-managed")?;
    let callback_root = layer_stack_root.clone();
    let server = std::thread::spawn({
        let socket_root = socket_root.clone();
        move || -> TestResult {
            let mut stream = connect_ppc_socket(&socket_root)?;
            let request = serve_refresh_until_op(&mut stream)?;
            assert_eq!(request.op, "plugin.generic.apply");

            let callback = PpcMessage {
                message_id: "apply-callback".to_owned(),
                direction: PpcDirection::Request,
                op: "daemon.occ.apply_changeset".to_owned(),
                body: serde_json::to_string(&json!({
                    "layer_stack_root": callback_root.to_string_lossy().into_owned(),
                    "changes": [{
                        "kind": "write",
                        "path": "src/main.py",
                        "content_utf8": "print('from callback')\n"
                    }]
                }))?,
            };
            stream.write_all(&callback.encode()?)?;
            let callback_reply = read_ppc_request(&mut stream, "read callback reply")?;
            assert_eq!(callback_reply.message_id, "apply-callback");
            let callback_body: Value = serde_json::from_str(&callback_reply.body)?;
            assert_eq!(callback_body["success"], true);
            assert_eq!(callback_body["files"][0]["status"], "committed");

            write_ppc_reply(
                &mut stream,
                request.message_id,
                r#"{"success":true,"from_self_managed":true}"#,
            )?;
            Ok(())
        }
    });

    let mut args = ensure_args(&layer_stack_root, &workspace_root, true);
    args["manifest"]["operations"][0] = json!({
        "op_name": "apply",
        "intent": "write_allowed",
        "auto_workspace_overlay": false,
        "service_id": "worker"
    });
    let ready = ensure_ready(&runtime, &args)?;
    assert_eq!(ready.started_count, 1);

    publish_peer_change(&layer_stack_root)?;

    let routed = dispatch_response(&runtime, "plugin.generic.apply", "apply-after-peer-write")?;
    assert_eq!(routed["from_self_managed"], true);
    assert_eq!(
        LayerStack::open(layer_stack_root.clone())?
            .read_text("src/main.py")?
            .0,
        "print('from callback')\n"
    );
    let status = runtime.status(false, None)?;
    assert_eq!(status.loaded_plugins[0].services[0].refresh_count, 1);

    join_test_thread(server, "server thread panicked")?;
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn restart_strategy_restarts_after_peer_publish_before_request() -> TestResult {
    let socket_root = test_socket_root("restart-service");
    let runtime = test_runtime(&socket_root);
    let (layer_stack_root, workspace_root) = test_bound_workspace("restart-service")?;
    let (allow_reconnect_tx, allow_reconnect_rx) = mpsc::channel();
    let connector = std::thread::spawn({
        let socket_root = socket_root.clone();
        move || -> TestResult {
            let _old_stream = connect_ppc_socket(&socket_root)?;
            allow_reconnect_rx.recv()?;

            let mut stream = connect_ppc_socket(&socket_root)?;
            let request = read_ppc_request(&mut stream, "read restarted ppc request")?;
            assert_eq!(request.op, "plugin.generic.hover");
            write_ppc_reply(
                &mut stream,
                request.message_id,
                r#"{"success":true,"from_restart_service":true}"#,
            )?;
            Ok(())
        }
    });

    let mut args = ensure_args(&layer_stack_root, &workspace_root, true);
    args["manifest"]["services"][0]["refresh_strategy"] = json!("restart_service");
    let ready = ensure_ready(&runtime, &args)?;
    assert_eq!(ready.started_count, 1);
    let initial_manifest_key = runtime.status(false, None)?.loaded_plugins[0].services[0]
        .manifest_key
        .clone()
        .ok_or("started service should carry a manifest key")?;

    publish_peer_change(&layer_stack_root)?;
    allow_reconnect_tx.send(())?;

    let routed = dispatch_response(&runtime, "plugin.generic.hover", "hover-after-restart")?;
    assert_eq!(routed["from_restart_service"], true);

    let status = runtime.status(false, None)?;
    let service = &status.loaded_plugins[0].services[0];
    assert_eq!(service.state, eos_plugin::PluginServiceState::Ready);
    assert_eq!(service.refresh_count, 0);
    assert_eq!(service.restart_count, 1);
    assert_ne!(
        service.manifest_key.as_deref(),
        Some(initial_manifest_key.as_str())
    );

    join_test_thread(connector, "connector thread panicked")?;
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn exited_service_process_fails_closed_before_dispatch() -> TestResult {
    let socket_root = test_socket_root("exited-service");
    let runtime = test_runtime(&socket_root);
    let (layer_stack_root, workspace_root) = test_bound_workspace("exited-service")?;
    let (connector_stop, connector) = spawn_idle_connector(socket_root.clone());
    let ready = ensure_started(&runtime, &layer_stack_root, &workspace_root)?;
    let pid = ready.running_service_processes[0].pid;

    kill_process(pid)?;
    wait_until_dead(pid);

    let failed = runtime
        .dispatch_registered_op("plugin.generic.hover", "hover-exited", &json!({}))
        .ok_or("route should dispatch")?;
    let err = failed.err().ok_or("dead service must fail closed")?;
    assert!(
        err.to_string()
            .contains("process exited before plugin dispatch"),
        "unexpected error: {err}"
    );

    let status = runtime.status(false, None)?;
    assert_eq!(
        status.loaded_plugins[0].services[0].state,
        eos_plugin::PluginServiceState::Stopped
    );
    let _ = connector_stop.send(());
    join_test_thread(connector, "connector thread panicked")?;
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn status_probe_reports_health_and_drops_failed_service() -> TestResult {
    let socket_root = test_socket_root("status-health");
    let runtime = test_runtime(&socket_root);
    let (layer_stack_root, workspace_root) = test_bound_workspace("status-health")?;
    let server = std::thread::spawn({
        let socket_root = socket_root.clone();
        move || -> TestResult {
            let mut stream = connect_ppc_socket(&socket_root)?;
            // First probe: ack on the expected manifest key.
            let request = read_ppc_request(&mut stream, "read health request")?;
            assert_eq!(request.op, REFRESH_OP);
            let body: Value = serde_json::from_str(&request.body)?;
            assert_eq!(body["type"], "health");
            let manifest_key = body["manifest_key"]
                .as_str()
                .ok_or("manifest key in probe")?
                .to_owned();
            write_ppc_reply_json(
                &mut stream,
                request.message_id,
                &json!({"manifest_key": manifest_key, "accepted": true}),
            )?;
            // Second probe: ack on the wrong manifest key.
            let request = read_ppc_request(&mut stream, "read second health request")?;
            write_ppc_reply_json(
                &mut stream,
                request.message_id,
                &json!({"manifest_key": "wrong-manifest", "accepted": true}),
            )?;
            Ok(())
        }
    });
    ensure_started(&runtime, &layer_stack_root, &workspace_root)?;

    let healthy = runtime.status(true, Some(Duration::from_secs(1)))?;
    assert_eq!(healthy.service_health.len(), 1);
    assert!(healthy.service_health[0].success);
    assert_eq!(healthy.service_health[0].service_id, "worker");
    assert_eq!(healthy.service_health[0].accepted, Some(true));

    let failed = runtime.status(true, Some(Duration::from_secs(1)))?;
    assert!(!failed.service_health[0].success);
    assert!(failed.service_health[0]
        .error
        .as_deref()
        .is_some_and(|error| error.contains("manifest")));
    assert!(failed.connected_ppc_routes.is_empty());
    assert_eq!(
        failed.loaded_plugins[0].services[0].state,
        eos_plugin::PluginServiceState::Stopped
    );
    assert_eq!(
        LayerStack::open(layer_stack_root.clone())?.active_lease_count(),
        0,
        "failed health probe should release the retained snapshot lease"
    );

    join_test_thread(server, "server thread panicked")?;
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn stop_services_for_layer_root_releases_snapshot_leases() -> TestResult {
    let socket_root = test_socket_root("stop-services");
    let runtime = test_runtime(&socket_root);
    let (layer_stack_root, workspace_root) = test_bound_workspace("stop-services")?;
    let (connector_stop, connector) = spawn_idle_connector(socket_root.clone());
    ensure_started(&runtime, &layer_stack_root, &workspace_root)?;
    assert_eq!(
        LayerStack::open(layer_stack_root.clone())?.active_lease_count(),
        1
    );

    let stopped =
        runtime.stop_services_for_layer_stack_root(&layer_stack_root.to_string_lossy())?;
    assert_eq!(stopped, 1);
    assert_eq!(
        LayerStack::open(layer_stack_root.clone())?.active_lease_count(),
        0
    );
    let status = runtime.status(false, None)?;
    assert_eq!(
        status.loaded_plugins[0].services[0].state,
        eos_plugin::PluginServiceState::Stopped
    );
    let _ = connector_stop.send(());
    join_test_thread(connector, "connector thread panicked")?;
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn oversized_connected_reply_is_rejected_by_configured_limit() -> TestResult {
    let socket_root = test_socket_root("oversized-reply");
    let mut config = PluginRuntimeConfig {
        ppc_root: socket_root.clone(),
        ..PluginRuntimeConfig::default()
    };
    config.max_response_bytes = 64;
    let runtime = PluginRuntime::new(config, Arc::new(FakeNsRunnerLauncher));
    let (layer_stack_root, workspace_root) = test_bound_workspace("oversized-reply")?;
    let connector = spawn_replying_connector(
        socket_root.clone(),
        r#"{"success":true,"padding":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#,
    );
    ensure_started(&runtime, &layer_stack_root, &workspace_root)?;

    let outcome = runtime
        .dispatch_registered_op("plugin.generic.hover", "hover-oversized", &json!({}))
        .ok_or("route should dispatch")?;
    assert!(matches!(
        outcome,
        Err(PluginRuntimeError::Plugin(PluginError::Ppc(message)))
            if message.contains("plugin response exceeds 64 byte limit")
    ));
    let _ = connector.join();
    cleanup(&socket_root, &layer_stack_root);
    Ok(())
}

#[test]
fn plugin_caller_fields_reject_nul_long_and_non_string_values() {
    assert!(matches!(
        validate_plugin_caller_fields(&json!({"caller_id": "agent\0plugin"})),
        Err(PluginError::Ppc(message))
            if message.contains("contains NUL")
    ));

    assert!(matches!(
        validate_plugin_caller_fields(&json!({"caller": {"source_id": "x".repeat(MAX_PLUGIN_CALLER_FIELD_CHARS + 1)}})),
        Err(PluginError::Ppc(message))
            if message.contains("exceeds")
    ));

    assert!(matches!(
        validate_plugin_caller_fields(&json!({"caller": {"request_id": 42}})),
        Err(PluginError::Ppc(message))
            if message.contains("must be a string")
    ));
}

// ---- support ---------------------------------------------------------------

/// Launcher fake: spawns the service command from the run request directly
/// (no ns-runner binary, no namespaces) and treats remounts as no-ops.
struct FakeNsRunnerLauncher;

impl NsRunnerLauncher for FakeNsRunnerLauncher {
    fn run(
        &self,
        _request: &eos_namespace::protocol::RunRequest,
    ) -> Result<eos_namespace::protocol::RunResult, LaunchError> {
        Err(LaunchError::Failed(
            "fake launcher cannot run oneshot overlay requests".to_owned(),
        ))
    }

    fn spawn_detached(
        &self,
        request: &eos_namespace::protocol::RunRequest,
    ) -> Result<std::process::Child, LaunchError> {
        let args = &request.tool_call.args;
        let command: Vec<String> = serde_json::from_value(args["command"].clone())
            .map_err(|err| LaunchError::InvalidRequest(err.to_string()))?;
        let env: std::collections::BTreeMap<String, String> =
            serde_json::from_value(args["env"].clone())
                .map_err(|err| LaunchError::InvalidRequest(err.to_string()))?;
        let (program, rest) = command
            .split_first()
            .ok_or_else(|| LaunchError::InvalidRequest("empty service command".to_owned()))?;
        let mut child = std::process::Command::new(program);
        child
            .args(rest)
            .envs(env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        {
            use std::os::unix::process::CommandExt;
            child.process_group(0);
        }
        Ok(child.spawn()?)
    }

    fn remount_in(
        &self,
        _target_pid: u32,
        _request: &eos_namespace::protocol::RunRequest,
        _timeout: Duration,
    ) -> Result<(), LaunchError> {
        Ok(())
    }
}

fn test_runtime(ppc_root: &Path) -> PluginRuntime {
    PluginRuntime::new(
        PluginRuntimeConfig {
            ppc_root: ppc_root.to_path_buf(),
            ..PluginRuntimeConfig::default()
        },
        Arc::new(FakeNsRunnerLauncher),
    )
}

/// The generic worker manifest args: a `workspace_snapshot_refresh` service
/// with a sleeping command and one read-only `hover` operation.
fn ensure_args(layer_stack_root: &Path, workspace_root: &Path, start: bool) -> Value {
    json!({
        "manifest": {
            "plugin_id": "generic",
            "plugin_version": "0.1.0",
            "plugin_digest": "digest-a",
            "services": [{
                "service_id": "worker",
                "service_profile_digest": "profile-digest-a",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace_and_notify",
                "command": ["/bin/sh", "-c", "test \"$EOS_PLUGIN_SERVICE_ID\" = worker && sleep 30"],
                "ppc_protocol_version": 1
            }],
            "operations": [{
                "op_name": "hover",
                "intent": "read_only",
                "service_id": "worker"
            }]
        },
        "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
        "workspace_root": workspace_root.to_string_lossy().into_owned(),
        "start_services": start
    })
}

fn ensure_started(
    runtime: &PluginRuntime,
    layer_stack_root: &Path,
    workspace_root: &Path,
) -> Result<Box<EnsureReady>, TestError> {
    ensure_ready(
        runtime,
        &ensure_args(layer_stack_root, workspace_root, true),
    )
}

fn ensure_ready(runtime: &PluginRuntime, args: &Value) -> Result<Box<EnsureReady>, TestError> {
    let input = plugin_ensure_input(args);
    match runtime.ensure(&input)? {
        EnsureOutcome::Ready(ready) => Ok(ready),
        EnsureOutcome::NeedsUpload { .. } => Err("unexpected needs-upload ensure".into()),
    }
}

fn plugin_ensure_input(args: &Value) -> PluginEnsureInput {
    PluginEnsureInput {
        plugin: args
            .get("plugin")
            .and_then(Value::as_str)
            .map(str::to_owned),
        digest: args
            .get("digest")
            .and_then(Value::as_str)
            .map(str::to_owned),
        manifest: args.get("manifest").cloned(),
        layer_stack_root: optional_trimmed_string(args, "layer_stack_root"),
        workspace_root: optional_trimmed_string(args, "workspace_root"),
        package: PluginPackageInput {
            package_runtime_root: None,
            package_dependency_root: None,
            package_upload_root: None,
            package_setup_root: None,
            staged_package_root: args
                .get("staged_package_root")
                .and_then(Value::as_str)
                .map(str::to_owned),
            staged_package_root_present: args.get("staged_package_root").is_some(),
        },
        start_services: args
            .get("start_services")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        caller: CallerId::from_wire(args),
        audit: PluginAuditFields {
            invocation_id: args
                .get("invocation_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            caller: BTreeMap::new(),
        },
    }
}

fn optional_trimmed_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn dispatch_response(
    runtime: &PluginRuntime,
    op: &str,
    invocation_id: &str,
) -> Result<Value, TestError> {
    let outcome = runtime
        .dispatch_registered_op(op, invocation_id, &json!({"caller_id": "caller-plugin"}))
        .ok_or("registered route missing")??;
    match outcome {
        PluginDispatchOutcome::Response(response) => {
            assert_eq!(response["success"], true, "response: {response:?}");
            Ok(response)
        }
        PluginDispatchOutcome::OneshotOverlay(_) => Err("unexpected oneshot overlay".into()),
    }
}

/// Publish a peer change so the active manifest moves past the service's
/// start-time snapshot.
fn publish_peer_change(layer_stack_root: &Path) -> TestResult {
    LayerStack::open(layer_stack_root.to_path_buf())?.publish_layer(&[LayerChange::Write {
        path: LayerPath::parse("peer.txt")?,
        content: b"peer\n".to_vec(),
    }])?;
    Ok(())
}

/// A connector serving the workspace-snapshot refresh protocol until the
/// final `expected_op` request arrives, then replying with `reply_body`.
fn spawn_refresh_server(
    socket_root: &Path,
    expected_op: &'static str,
    expected_message_id: &'static str,
    reply_body: &'static str,
) -> std::thread::JoinHandle<TestResult> {
    let socket_root = socket_root.to_path_buf();
    std::thread::spawn(move || -> TestResult {
        let mut stream = connect_ppc_socket(&socket_root)?;
        let request = serve_refresh_until_op(&mut stream)?;
        assert_eq!(request.message_id, expected_message_id);
        assert_eq!(request.op, expected_op);
        write_ppc_reply(&mut stream, request.message_id, reply_body)?;
        Ok(())
    })
}

/// Answer refresh-protocol requests until a non-refresh op arrives, asserting
/// the prepare/swap/health steps were all seen.
fn serve_refresh_until_op(
    stream: &mut std::os::unix::net::UnixStream,
) -> Result<PpcMessage, TestError> {
    let mut refresh_types = Vec::new();
    let mut current_manifest_key = String::new();
    loop {
        let request = read_ppc_request(stream, "read ppc request")?;
        if request.op != REFRESH_OP {
            assert!(
                refresh_types.contains(&"prepare_refresh".to_owned()),
                "refresh steps seen: {refresh_types:?}"
            );
            assert!(refresh_types.contains(&"swap_workspace".to_owned()));
            assert!(refresh_types.contains(&"health".to_owned()));
            return Ok(request);
        }
        let body: Value = serde_json::from_str(&request.body)?;
        refresh_types.push(
            body["type"]
                .as_str()
                .ok_or("refresh type must be a string")?
                .to_owned(),
        );
        if let Some(key) = body
            .get("target_manifest_key")
            .or_else(|| body.get("manifest_key"))
            .and_then(Value::as_str)
        {
            current_manifest_key = key.to_owned();
        }
        write_ppc_reply_json(
            stream,
            request.message_id,
            &json!({"manifest_key": current_manifest_key, "accepted": true}),
        )?;
    }
}

fn read_ppc_request(
    stream: &mut std::os::unix::net::UnixStream,
    context: &'static str,
) -> Result<PpcMessage, TestError> {
    let message = read_message_bytes(stream)?;
    PpcMessage::decode(&message)
        .map_err(|err| std::io::Error::other(format!("{context}: {err}")).into())
}

fn write_ppc_reply(
    stream: &mut std::os::unix::net::UnixStream,
    message_id: String,
    body: &'static str,
) -> TestResult {
    let reply = PpcMessage {
        message_id,
        direction: PpcDirection::Reply,
        op: "reply".to_owned(),
        body: body.to_owned(),
    };
    stream.write_all(&reply.encode()?)?;
    Ok(())
}

fn write_ppc_reply_json(
    stream: &mut std::os::unix::net::UnixStream,
    message_id: String,
    body: &Value,
) -> TestResult {
    let reply = PpcMessage {
        message_id,
        direction: PpcDirection::Reply,
        op: "reply".to_owned(),
        body: serde_json::to_string(body)?,
    };
    stream.write_all(&reply.encode()?)?;
    Ok(())
}

fn spawn_replying_connector(
    socket_root: PathBuf,
    reply_body: &'static str,
) -> std::thread::JoinHandle<TestResult> {
    std::thread::spawn(move || -> TestResult {
        let mut stream = connect_ppc_socket(&socket_root)?;
        let request = read_ppc_request(&mut stream, "read ppc request")?;
        write_ppc_reply(&mut stream, request.message_id, reply_body)?;
        Ok(())
    })
}

fn spawn_idle_connector(
    socket_root: PathBuf,
) -> (mpsc::Sender<()>, std::thread::JoinHandle<TestResult>) {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || -> TestResult {
        let _stream = connect_ppc_socket(&socket_root)?;
        let _ = stop_rx.recv_timeout(Duration::from_secs(5));
        Ok(())
    });
    (stop_tx, handle)
}

fn connect_ppc_socket(root: &Path) -> Result<std::os::unix::net::UnixStream, std::io::Error> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("sock") {
                    continue;
                }
                if let Ok(stream) = std::os::unix::net::UnixStream::connect(path) {
                    return Ok(stream);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out connecting to socket under {}", root.display()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn kill_process(pid: u32) -> TestResult {
    let status = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill -KILL {pid} failed: {status}").into())
    }
}

fn wait_until_dead(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let alive = std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !alive {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn test_socket_root(name: &str) -> PathBuf {
    let root = PathBuf::from("/tmp").join(format!(
        "eos-operation-plugin-ppc-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn test_bound_workspace(name: &str) -> Result<(PathBuf, PathBuf), TestError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "eos-operation-plugin-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let layer_stack_root = base.join("layer-stack");
    let workspace_root = base.join("workspace");
    std::fs::create_dir_all(&workspace_root)?;
    std::fs::write(workspace_root.join("seed.txt"), "seed\n")?;
    eos_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, true)?;
    Ok((layer_stack_root, workspace_root))
}

fn cleanup(socket_root: &Path, layer_stack_root: &Path) {
    let _ = std::fs::remove_dir_all(socket_root);
    if let Some(base) = layer_stack_root.parent() {
        let _ = std::fs::remove_dir_all(base);
    }
}

fn join_test_thread(
    handle: std::thread::JoinHandle<TestResult>,
    context: &'static str,
) -> TestResult {
    handle
        .join()
        .map_err(|_| std::io::Error::other(context))??;
    Ok(())
}

fn join_value_thread(
    handle: std::thread::JoinHandle<Result<Value, TestError>>,
    context: &'static str,
) -> Result<Value, TestError> {
    handle.join().map_err(|_| std::io::Error::other(context))?
}
