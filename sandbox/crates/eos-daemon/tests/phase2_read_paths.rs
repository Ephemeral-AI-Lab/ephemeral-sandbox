use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

// Integration test crates receive every normal `eos-daemon` dependency even
// when the test only drives public daemon APIs. These imports keep
// `unused_crate_dependencies` meaningful without suppressing it crate-wide.
use base64::Engine as _;
use eos_config::configs::daemon::PluginRuntimeConfig;
use eos_config::configs::isolated_workspace::IsolatedWorkspaceConfig;
use eos_daemon::wire::{decode, encode, Request, WireMessage, DAEMON_AUTH_FIELD};
use eos_daemon::{DaemonServer, RuntimeServices, ServerConfig};
use eos_daemon::{DispatchContext, InFlightRegistry};
use eos_layerstack as _;
use eos_namespace::protocol::{RunRequest, RunResult};
use eos_operation::plugin::{LaunchError, NsRunnerLauncher};
use eos_overlay as _;
use eos_plugin as _;
use eos_trace::decode_trace_batch;
use serde as _;
use serde_json::{json, Value};
use thiserror as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::time::{sleep, timeout, Duration};
use tokio_util as _;

static ISOLATED_ENV_LOCK: Mutex<()> = Mutex::new(());

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// One daemon under test with its own runtime services.
struct TestDaemon {
    services: RuntimeServices,
}

impl TestDaemon {
    fn new() -> Self {
        Self::with_services(test_services(
            PluginRuntimeConfig::default(),
            IsolatedWorkspaceConfig::default(),
        ))
    }

    fn with_isolated_workspace(scratch_root: &Path) -> Self {
        Self::with_services(test_services(
            PluginRuntimeConfig::default(),
            IsolatedWorkspaceConfig {
                enabled: true,
                scratch_root: scratch_root.to_path_buf(),
                ..IsolatedWorkspaceConfig::default()
            },
        ))
    }

    fn with_services(services: RuntimeServices) -> Self {
        Self { services }
    }

    fn dispatch(&self, request: &Request) -> Value {
        eos_daemon::dispatch_with_context(request, DispatchContext::with_services(&self.services))
    }
}

fn test_services(
    plugin: PluginRuntimeConfig,
    isolated_workspace: IsolatedWorkspaceConfig,
) -> RuntimeServices {
    RuntimeServices::new(plugin, isolated_workspace, Arc::new(NoLaunch))
}

struct NoLaunch;

impl NsRunnerLauncher for NoLaunch {
    fn run(&self, _request: &RunRequest) -> Result<RunResult, LaunchError> {
        Err(LaunchError::Failed(
            "test launcher does not start ns-runner".to_owned(),
        ))
    }

    fn spawn_detached(&self, _request: &RunRequest) -> Result<Child, LaunchError> {
        Err(LaunchError::Failed(
            "test launcher does not start ns-runner".to_owned(),
        ))
    }

    fn remount_in(
        &self,
        _target_pid: u32,
        _request: &RunRequest,
        _timeout: std::time::Duration,
    ) -> Result<(), LaunchError> {
        Err(LaunchError::Failed(
            "test launcher does not start ns-runner".to_owned(),
        ))
    }
}

#[test]
fn dispatches_layerstack_read_file() -> TestResult {
    let (root, workspace) = seed_layer_stack("read_file")?;
    let request = Request {
        op: "sandbox.file.read".to_owned(),
        invocation_id: "inv-1".to_owned(),
        args: json!({
            "layer_stack_root": root,
            "path": workspace.join("README.md"),
        }),
    };

    let response = eos_daemon::dispatch(&request);

    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["workspace"], Value::String("ephemeral".to_owned()));
    assert_eq!(response["content"], Value::String("# README\n".to_owned()));
    assert_eq!(response["exists"], Value::Bool(true));
    assert!(response["timings"]["api.read.layer_stack_read_s"].is_number());
    Ok(())
}

#[test]
fn dispatches_runtime_ready_probe() -> TestResult {
    let (root, _workspace) = seed_layer_stack("ready")?;
    let request = Request {
        op: "sandbox.runtime.ready".to_owned(),
        invocation_id: "inv-1".to_owned(),
        args: json!({"layer_stack_root": root}),
    };

    let response = eos_daemon::dispatch(&request);

    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["ready"], Value::Bool(true));
    assert_eq!(
        response["probes"][0]["name"],
        Value::String("control_plane".to_owned())
    );
    assert_eq!(
        response["probes"][0]["status"],
        Value::String("ok".to_owned())
    );
    Ok(())
}

#[test]
fn dispatches_workspace_base_control_ops_for_fresh_stack() -> TestResult {
    let (root, workspace, outside_target) = seed_workspace_base_fixture()?;
    let daemon = TestDaemon::new();

    let ensure = dispatch_request(
        &daemon,
        "sandbox.checkpoint.ensure_base",
        "ensure",
        json!({
            "layer_stack_root": &root,
            "workspace_root": &workspace,
        }),
    );
    assert_workspace_base_created(&ensure, &root, &workspace);
    assert_workspace_base_symlinks(&root, &outside_target)?;

    let binding = dispatch_request(
        &daemon,
        "sandbox.checkpoint.binding",
        "binding",
        json!({"layer_stack_root": &root}),
    );
    assert_eq!(
        binding["binding"]["base_root_hash"],
        ensure["binding"]["base_root_hash"]
    );
    assert_read_content(
        &daemon,
        &root,
        &json!(workspace.join("README.md")),
        "# base\n",
    );
    assert_workspace_base_idempotent(&daemon, &root, &workspace);

    rebuild_workspace_base(&daemon, &root, &workspace, &ensure)?;
    assert_read_content(&daemon, &root, &json!("README.md"), "# reset\n");
    Ok(())
}

#[test]
fn unknown_op_uses_structured_contract() {
    let request = Request {
        op: "sandbox.does_not_exist".to_owned(),
        invocation_id: "inv-1".to_owned(),
        args: json!({}),
    };

    let response = eos_daemon::dispatch(&request);

    assert_eq!(response["success"], Value::Bool(false));
    assert_eq!(
        response["error"]["kind"],
        Value::String("unknown_op".to_owned())
    );
    assert_eq!(
        response["error"]["details"]["op"],
        Value::String("sandbox.does_not_exist".to_owned())
    );
}

#[test]
fn isolated_workspace_ops_are_registered_and_disabled_by_default() -> TestResult {
    let _guard = ISOLATED_ENV_LOCK
        .lock()
        .map_err(|_| "isolated env lock poisoned")?;
    let daemon = TestDaemon::new();
    std::env::set_var("EOS_ISOLATED_WORKSPACE_TEST_HARNESS", "true");
    let _ = daemon.dispatch(&Request {
        op: "sandbox.isolation.test_reset".to_owned(),
        invocation_id: "iws-reset".to_owned(),
        args: json!({}),
    });
    std::env::remove_var("EOS_ISOLATED_WORKSPACE_TEST_HARNESS");

    let enter = daemon.dispatch(&Request {
        op: "sandbox.isolation.enter".to_owned(),
        invocation_id: "iws-enter".to_owned(),
        args: json!({
            "caller_id": "caller-a",
            "layer_stack_root": "/tmp/layer-stack",
        }),
    });
    assert_eq!(enter["success"], Value::Bool(false));
    assert_eq!(
        enter["error"]["kind"],
        Value::String("feature_disabled".to_owned())
    );

    let status = daemon.dispatch(&Request {
        op: "sandbox.isolation.status".to_owned(),
        invocation_id: "iws-status".to_owned(),
        args: json!({"caller_id": "caller-a"}),
    });
    assert_eq!(status["success"], Value::Bool(false));
    assert_eq!(
        status["error"]["kind"],
        Value::String("feature_disabled".to_owned())
    );

    let open = daemon.dispatch(&Request {
        op: "sandbox.isolation.list_open".to_owned(),
        invocation_id: "iws-list".to_owned(),
        args: json!({}),
    });
    assert_eq!(open["success"], Value::Bool(true));
    assert_eq!(open["open_caller_ids"], json!([]));
    Ok(())
}

#[test]
fn isolated_workspace_lifecycle_ops_open_status_list_and_exit_when_enabled() -> TestResult {
    let _guard = ISOLATED_ENV_LOCK
        .lock()
        .map_err(|_| "isolated env lock poisoned")?;
    let env = IsolatedLifecycleEnv::new()?;
    let daemon = TestDaemon::with_isolated_workspace(&env.scratch);
    assert_isolated_test_reset(&daemon, "iws-reset");

    let enter = dispatch_request(
        &daemon,
        "sandbox.isolation.enter",
        "iws-enter",
        json!({
            "caller_id": "caller-enabled",
            "layer_stack_root": &env.root,
        }),
    );
    assert_eq!(enter["success"], Value::Bool(true));
    assert_eq!(enter["manifest_version"], json!(1));
    assert_eq!(enter["manifest_root_hash"].as_str().map(str::len), Some(64));
    let handle_id = enter["workspace_handle_id"]
        .as_str()
        .ok_or("workspace handle id")?;
    assert!(
        handle_id.len() >= 6 && handle_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "workspace handle id should be a hex filesystem component: {handle_id}"
    );
    let handle_scratch = env.scratch.join(handle_id);
    let private_file = handle_scratch.join("upper").join("private.txt");
    std::fs::write(&private_file, "private scratch\n")?;

    assert_isolated_open_state(&daemon, &env.root);
    let exit = dispatch_request(
        &daemon,
        "sandbox.isolation.exit",
        "iws-exit",
        json!({"caller_id": "caller-enabled"}),
    );
    assert_isolated_exit(&exit, &handle_scratch)?;
    assert_isolated_status_closed(&daemon);
    assert_isolated_test_reset(&daemon, "iws-reset-end");
    Ok(())
}

#[test]
fn isolated_workspace_ops_validate_required_arguments() -> TestResult {
    let _guard = ISOLATED_ENV_LOCK
        .lock()
        .map_err(|_| "isolated env lock poisoned")?;
    let daemon = TestDaemon::new();
    let response = daemon.dispatch(&Request {
        op: "sandbox.isolation.enter".to_owned(),
        invocation_id: "iws-enter-missing-agent".to_owned(),
        args: json!({"layer_stack_root": "/tmp/layer-stack"}),
    });

    assert_eq!(response["success"], Value::Bool(false));
    assert_eq!(
        response["error"]["kind"],
        Value::String("invalid_argument".to_owned())
    );
    assert_eq!(
        response["error"]["details"]["key"],
        Value::String("caller_id".to_owned())
    );
    Ok(())
}

#[tokio::test]
async fn control_ops_use_inflight_registry() -> TestResult {
    let registry = InFlightRegistry::new(300.0, 30.0);
    let task = tokio::spawn(std::future::pending::<()>());
    registry.register("bg-shell", task.abort_handle(), "caller-a", true);
    let context = DispatchContext::with_invocation_registry(&registry);

    let count = eos_daemon::dispatch_with_context(
        &Request {
            op: "sandbox.call.count".to_owned(),
            invocation_id: "count".to_owned(),
            args: json!({"caller_id": "caller-a"}),
        },
        context,
    );
    assert_eq!(count["success"], Value::Bool(true));
    assert_eq!(count["count"], json!(1));

    let command_count = eos_daemon::dispatch_with_context(
        &Request {
            op: "sandbox.command.count".to_owned(),
            invocation_id: "command-count".to_owned(),
            args: json!({"caller_id": "caller-a"}),
        },
        context,
    );
    assert_eq!(command_count["success"], Value::Bool(true));
    assert_eq!(command_count["count"], json!(0));

    let heartbeat = eos_daemon::dispatch_with_context(
        &Request {
            op: "sandbox.call.heartbeat".to_owned(),
            invocation_id: "heartbeat".to_owned(),
            args: json!({"invocation_ids": ["bg-shell", "missing"]}),
        },
        context,
    );
    assert_eq!(heartbeat["success"], Value::Bool(true));
    assert_eq!(heartbeat["touched"], json!(1));

    let cancel = eos_daemon::dispatch_with_context(
        &Request {
            op: "sandbox.call.cancel".to_owned(),
            invocation_id: "cancel".to_owned(),
            args: json!({"invocation_id": "bg-shell"}),
        },
        context,
    );
    assert_eq!(cancel["success"], Value::Bool(true));
    assert_eq!(cancel["cancelled"], Value::Bool(true));
    match task.await {
        Ok(()) => return Err("expected task cancellation, but task completed".into()),
        Err(error) if error.is_cancelled() => {}
        Err(error) => return Err(format!("expected task cancellation, got {error}").into()),
    }

    registry.deregister("bg-shell");
    let count = eos_daemon::dispatch_with_context(
        &Request {
            op: "sandbox.call.count".to_owned(),
            invocation_id: "count-after".to_owned(),
            args: json!({"caller_id": "caller-a"}),
        },
        context,
    );
    assert_eq!(count["count"], json!(0));
    Ok(())
}

#[tokio::test]
async fn unix_server_dispatches_framed_ready_request() -> TestResult {
    let (root, _workspace) = seed_layer_stack("unix_server")?;
    let runtime_dir = root
        .parent()
        .ok_or("seeded layer-stack root must have parent")?
        .join("runtime");
    std::fs::create_dir_all(&runtime_dir)?;
    let config = ServerConfig {
        socket_path: runtime_dir.join("runtime.sock"),
        pid_path: runtime_dir.join("runtime.pid"),
        tcp_host: None,
        tcp_port: None,
        auth_token: None,
    };
    let server = DaemonServer::new(config.clone());
    let shutdown = server.shutdown_token();
    let task = tokio::spawn(server.serve());
    for _ in 0..50 {
        if config.socket_path.exists() {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    let request = WireMessage::Request(Request {
        op: "sandbox.runtime.ready".to_owned(),
        invocation_id: "inv-1".to_owned(),
        args: json!({"layer_stack_root": root}),
    });
    let mut stream = UnixStream::connect(&config.socket_path).await?;
    stream.write_all(&encode(&request)?).await?;
    stream.shutdown().await?;
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await??;
    shutdown.cancel();
    let _ = timeout(Duration::from_secs(2), task).await??;

    let response = match decode(&response)? {
        WireMessage::Response(value) => value,
        other => return Err(format!("expected response, got {other:?}").into()),
    };
    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["ready"], Value::Bool(true));
    Ok(())
}

#[tokio::test]
async fn tcp_server_dispatches_authenticated_ready_request() -> TestResult {
    let (root, _workspace) = seed_layer_stack("tcp_server")?;
    let runtime_dir = root
        .parent()
        .ok_or("seeded layer-stack root must have parent")?
        .join("runtime");
    std::fs::create_dir_all(&runtime_dir)?;
    let probe = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = probe.local_addr()?.port();
    drop(probe);
    let config = ServerConfig {
        socket_path: runtime_dir.join("runtime.sock"),
        pid_path: runtime_dir.join("runtime.pid"),
        tcp_host: Some("127.0.0.1".to_owned()),
        tcp_port: Some(port),
        auth_token: Some("secret".to_owned()),
    };
    let server = DaemonServer::new(config.clone());
    let shutdown = server.shutdown_token();
    let task = tokio::spawn(server.serve());
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    let mut value = serde_json::to_value(Request {
        op: "sandbox.runtime.ready".to_owned(),
        invocation_id: "inv-1".to_owned(),
        args: json!({"layer_stack_root": root}),
    })?;
    value
        .as_object_mut()
        .ok_or("request value object")?
        .insert(DAEMON_AUTH_FIELD.to_owned(), json!("secret"));
    let mut request = serde_json::to_vec(&value)?;
    request.push(b'\n');
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(&request).await?;
    stream.shutdown().await?;
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await??;
    shutdown.cancel();
    let _ = timeout(Duration::from_secs(2), task).await??;

    let response = match decode(&response)? {
        WireMessage::Response(value) => value,
        other => return Err(format!("expected response, got {other:?}").into()),
    };
    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["ready"], Value::Bool(true));
    Ok(())
}

#[tokio::test]
async fn tcp_server_sidecar_records_transport_dispatch_and_op_spans() -> TestResult {
    let (root, _workspace) = seed_layer_stack("tcp_trace_sidecar")?;
    let runtime_dir = root
        .parent()
        .ok_or("seeded layer-stack root must have parent")?
        .join("runtime");
    std::fs::create_dir_all(&runtime_dir)?;
    let probe = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = probe.local_addr()?.port();
    drop(probe);
    let config = ServerConfig {
        socket_path: runtime_dir.join("runtime.sock"),
        pid_path: runtime_dir.join("runtime.pid"),
        tcp_host: Some("127.0.0.1".to_owned()),
        tcp_port: Some(port),
        auth_token: Some("secret".to_owned()),
    };
    let server = DaemonServer::new(config.clone());
    let shutdown = server.shutdown_token();
    let task = tokio::spawn(server.serve());
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    let request = json!({
        "op": "sandbox.runtime.ready",
        "invocation_id": "request-sidecar",
        "args": {
            "layer_stack_root": root,
            "_eos_daemon_protocol_version": 1,
        },
        "trace": {
            "trace_id": "trace-sidecar",
            "request_id": "request-sidecar",
            "link_hints": [],
            "capture_budget_version": 1,
        },
        DAEMON_AUTH_FIELD: "secret",
    });
    let mut bytes = serde_json::to_vec(&request)?;
    bytes.push(b'\n');
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await??;
    shutdown.cancel();
    let _ = timeout(Duration::from_secs(2), task).await??;

    let response = match decode(&response)? {
        WireMessage::Response(value) => value,
        other => return Err(format!("expected response, got {other:?}").into()),
    };
    let sidecar = response["_trace_events"]
        .as_str()
        .ok_or("response carries sidecar")?;
    let batch = decode_trace_batch(&base64::engine::general_purpose::STANDARD.decode(sidecar)?)?;
    let record = batch.records.first().ok_or("trace record")?;
    assert_eq!(record.trace_id.as_str(), "trace-sidecar");
    assert_eq!(
        record.request_id.as_ref().map(eos_trace::RequestId::as_str),
        Some("request-sidecar")
    );
    let spans: Vec<_> = record.spans.iter().map(|span| span.name.as_str()).collect();
    assert!(spans.contains(&"op_request"), "{spans:?}");
    assert!(spans.contains(&"daemon.transport"), "{spans:?}");
    assert!(spans.contains(&"dispatch"), "{spans:?}");
    assert!(spans.contains(&"op.runtime.ready"), "{spans:?}");
    let events: Vec<_> = record
        .events
        .iter()
        .map(|event| (event.module.as_str(), event.name.as_str()))
        .collect();
    assert!(
        events.contains(&("daemon.transport", "accepted")),
        "{events:?}"
    );
    assert!(
        events.contains(&("daemon.transport", "read_finished")),
        "{events:?}"
    );
    assert!(
        events.contains(&("daemon.transport", "auth_checked")),
        "{events:?}"
    );
    assert!(
        events.contains(&("daemon.transport", "response_write_finished")),
        "{events:?}"
    );
    assert!(
        events.contains(&("daemon.dispatch", "dispatch_finished")),
        "{events:?}"
    );
    Ok(())
}

fn dispatch_request(daemon: &TestDaemon, op: &str, invocation_id: &str, args: Value) -> Value {
    daemon.dispatch(&Request {
        op: op.to_owned(),
        invocation_id: invocation_id.to_owned(),
        args,
    })
}

fn seed_workspace_base_fixture() -> TestResult<(PathBuf, PathBuf, PathBuf)> {
    let (root, workspace) = empty_workspace("workspace_base")?;
    std::fs::create_dir_all(workspace.join("src"))?;
    std::fs::write(workspace.join("README.md"), "# base\n")?;
    std::fs::write(workspace.join("src").join("a.py"), "print('base')\n")?;
    std::os::unix::fs::symlink("src/a.py", workspace.join("link.py"))?;
    std::fs::create_dir_all(workspace.join("links"))?;
    let outside_target = workspace
        .parent()
        .ok_or("workspace parent")?
        .join("outside.txt");
    std::fs::write(&outside_target, "outside\n")?;
    std::os::unix::fs::symlink("../src/a.py", workspace.join("links").join("inside"))?;
    std::os::unix::fs::symlink(&outside_target, workspace.join("links").join("outside"))?;
    Ok((root, workspace, outside_target))
}

fn assert_workspace_base_created(ensure: &Value, root: &Path, workspace: &Path) {
    assert_eq!(ensure["success"], Value::Bool(true));
    assert_eq!(ensure["created"], Value::Bool(true));
    assert_eq!(
        ensure["binding"]["workspace_root"],
        json!(workspace.to_string_lossy().as_ref())
    );
    assert_eq!(
        ensure["binding"]["layer_stack_root"],
        json!(root.to_string_lossy().as_ref())
    );
    assert_eq!(ensure["binding"]["base_manifest_version"], json!(1));
    assert_eq!(
        ensure["binding"]["base_root_hash"].as_str().map(str::len),
        Some(64)
    );
    assert!(ensure["timings"]["api.workspace_base.total_s"].is_number());
}

fn assert_workspace_base_symlinks(root: &Path, outside_target: &Path) -> TestResult {
    assert_eq!(
        std::fs::read_link(
            root.join("layers")
                .join("B000001-base")
                .join("links")
                .join("inside")
        )?
        .to_string_lossy(),
        "../src/a.py"
    );
    assert_eq!(
        std::fs::read_link(
            root.join("layers")
                .join("B000001-base")
                .join("links")
                .join("outside")
        )?,
        outside_target
    );
    Ok(())
}

fn assert_read_content(daemon: &TestDaemon, root: &Path, path: &Value, content: &str) {
    let read = dispatch_request(
        daemon,
        "sandbox.file.read",
        "read",
        json!({
            "layer_stack_root": root,
            "path": path,
        }),
    );
    assert_eq!(read["success"], Value::Bool(true));
    assert_eq!(read["content"], Value::String(content.to_owned()));
}

fn assert_workspace_base_idempotent(daemon: &TestDaemon, root: &Path, workspace: &Path) {
    let ensure_again = dispatch_request(
        daemon,
        "sandbox.checkpoint.ensure_base",
        "ensure-again",
        json!({
            "layer_stack_root": root,
            "workspace_root": workspace,
        }),
    );
    assert_eq!(ensure_again["success"], Value::Bool(true));
    assert_eq!(ensure_again["created"], Value::Bool(false));
}

fn rebuild_workspace_base(
    daemon: &TestDaemon,
    root: &Path,
    workspace: &Path,
    original_ensure: &Value,
) -> TestResult {
    std::fs::write(workspace.join("README.md"), "# reset\n")?;
    let rebuilt = dispatch_request(
        daemon,
        "sandbox.checkpoint.build_base",
        "rebuild",
        json!({
            "layer_stack_root": root,
            "workspace_root": workspace,
            "reset": true,
        }),
    );
    assert_eq!(rebuilt["success"], Value::Bool(true));
    assert_eq!(rebuilt["created"], Value::Bool(true));
    assert_ne!(
        rebuilt["binding"]["base_root_hash"],
        original_ensure["binding"]["base_root_hash"]
    );
    Ok(())
}

struct IsolatedLifecycleEnv {
    root: PathBuf,
    scratch: PathBuf,
}

impl IsolatedLifecycleEnv {
    fn new() -> TestResult<Self> {
        let (root, _workspace) = seed_layer_stack("isolated_lifecycle")?;
        let base = root.parent().ok_or("layer root parent")?;
        let scratch = base.join("isolated-scratch");
        std::env::set_var("EOS_ISOLATED_WORKSPACE_TEST_HARNESS", "true");
        Ok(Self { root, scratch })
    }
}

impl Drop for IsolatedLifecycleEnv {
    fn drop(&mut self) {
        std::env::remove_var("EOS_ISOLATED_WORKSPACE_TEST_HARNESS");
        if let Some(base) = self.root.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

fn assert_isolated_test_reset(daemon: &TestDaemon, invocation_id: &str) {
    let reset = dispatch_request(
        daemon,
        "sandbox.isolation.test_reset",
        invocation_id,
        json!({}),
    );
    assert_eq!(reset["success"], Value::Bool(true));
}

fn assert_isolated_open_state(daemon: &TestDaemon, root: &Path) {
    let status = dispatch_request(
        daemon,
        "sandbox.isolation.status",
        "iws-status",
        json!({"caller_id": "caller-enabled"}),
    );
    assert_eq!(status["success"], Value::Bool(true));
    assert_eq!(status["open"], Value::Bool(true));
    assert_eq!(status["manifest_version"], json!(1));

    let duplicate = dispatch_request(
        daemon,
        "sandbox.isolation.enter",
        "iws-enter-again",
        json!({
            "caller_id": "caller-enabled",
            "layer_stack_root": root,
        }),
    );
    assert_eq!(duplicate["success"], Value::Bool(false));
    assert_eq!(duplicate["error"]["kind"], "already_open");

    let open = dispatch_request(daemon, "sandbox.isolation.list_open", "iws-list", json!({}));
    assert_eq!(open["success"], Value::Bool(true));
    assert_eq!(open["open_caller_ids"], json!(["caller-enabled"]));
}

fn assert_isolated_exit(exit: &Value, handle_scratch: &Path) -> TestResult {
    assert_eq!(exit["success"], Value::Bool(true));
    assert!(exit["evicted_upperdir_bytes"].as_u64().unwrap_or(0) > 0);
    assert_eq!(exit["inspection"]["handle_registered_after"], json!(false));
    assert_eq!(exit["inspection"]["agent_registered_after"], json!(false));
    assert_eq!(exit["inspection"]["open_handle_count_after"], json!(0));
    assert_eq!(exit["inspection"]["open_agent_count_after"], json!(0));
    assert_eq!(exit["inspection"]["lease_released"], json!(true));
    assert_eq!(exit["inspection"]["active_leases_after"], json!(0));
    assert_eq!(exit["inspection"]["scratch_exists_after"], json!(false));
    assert_eq!(exit["inspection"]["upperdir_exists_after"], json!(false));
    assert_eq!(exit["inspection"]["workdir_exists_after"], json!(false));
    assert_eq!(exit["inspection"]["cgroup_exists_after"], Value::Null);
    assert!(!handle_scratch.exists());
    Ok(())
}

fn assert_isolated_status_closed(daemon: &TestDaemon) {
    let status = dispatch_request(
        daemon,
        "sandbox.isolation.status",
        "iws-status-closed",
        json!({"caller_id": "caller-enabled"}),
    );
    assert_eq!(status["success"], Value::Bool(true));
    assert_eq!(status["open"], Value::Bool(false));
}

fn seed_layer_stack(label: &str) -> TestResult<(PathBuf, PathBuf)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = PathBuf::from("/tmp").join(format!(
        "eosd-p2-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let workspace = base.join("workspace");
    let root = base.join("layer-stack");
    let layer = root.join("layers").join("B000001-base");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&layer)?;
    std::fs::create_dir_all(root.join("staging"))?;
    std::fs::write(layer.join("README.md"), "# README\n")?;
    write_json(
        &root.join("manifest.json"),
        &json!({
            "schema_version": 1,
            "version": 1,
            "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
        }),
    )?;
    write_json(
        &root.join("workspace.json"),
        &json!({
            "workspace_root": workspace,
            "layer_stack_root": root,
            "active_manifest_version": 1,
            "active_root_hash": "root",
            "base_manifest_version": 1,
            "base_root_hash": "base",
        }),
    )?;
    Ok((root, workspace))
}

fn empty_workspace(label: &str) -> TestResult<(PathBuf, PathBuf)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = PathBuf::from("/tmp").join(format!(
        "eosd-empty-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let workspace = base.join("workspace");
    let root = base.join("layer-stack");
    std::fs::create_dir_all(&workspace)?;
    Ok((root, workspace))
}

fn write_json(path: &Path, value: &Value) -> TestResult {
    let encoded = serde_json::to_string_pretty(value)?;
    std::fs::write(path, encoded)?;
    Ok(())
}
