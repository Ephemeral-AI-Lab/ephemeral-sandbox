use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

// Integration test crates receive every normal `eos-daemon` dependency even
// when the test only drives public daemon APIs. These imports keep
// `unused_crate_dependencies` meaningful without suppressing it crate-wide.
use eos_daemon::wire::{decode, encode, Envelope, Request, DAEMON_AUTH_FIELD};
use eos_daemon::{DaemonServer, ServerConfig};
use eos_daemon::{DispatchContext, InFlightRegistry, OpTable};
use eos_layerstack as _;
use eos_overlay as _;
use eos_plugin as _;
use serde as _;
use serde_json::{json, Value};
use thiserror as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::time::{sleep, timeout, Duration};
use tokio_util as _;

static ISOLATED_ENV_LOCK: Mutex<()> = Mutex::new(());

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn dispatches_layerstack_read_file() -> TestResult {
    let (root, workspace) = seed_layer_stack("read_file")?;
    let request = Request {
        op: "api.v1.read_file".to_owned(),
        invocation_id: "inv-1".to_owned(),
        args: json!({
            "layer_stack_root": root,
            "path": workspace.join("README.md"),
        }),
    };

    let response = OpTable::with_builtins().dispatch(&request);

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
        op: "api.runtime.ready".to_owned(),
        invocation_id: "inv-1".to_owned(),
        args: json!({"layer_stack_root": root}),
    };

    let response = OpTable::with_builtins().dispatch(&request);

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
    let table = OpTable::with_builtins();

    let ensure = dispatch_request(
        &table,
        "api.ensure_workspace_base",
        "ensure",
        json!({
            "layer_stack_root": &root,
            "workspace_root": &workspace,
        }),
    );
    assert_workspace_base_created(&ensure, &root, &workspace);
    assert_workspace_base_symlinks(&root, &outside_target)?;

    let binding = dispatch_request(
        &table,
        "api.workspace_binding",
        "binding",
        json!({"layer_stack_root": &root}),
    );
    assert_eq!(
        binding["binding"]["base_root_hash"],
        ensure["binding"]["base_root_hash"]
    );
    assert_read_content(
        &table,
        &root,
        &json!(workspace.join("README.md")),
        "# base\n",
    );
    assert_workspace_base_idempotent(&table, &root, &workspace);

    rebuild_workspace_base(&table, &root, &workspace, &ensure)?;
    assert_read_content(&table, &root, &json!("README.md"), "# reset\n");
    Ok(())
}

#[test]
fn unknown_op_uses_structured_contract() {
    let request = Request {
        op: "api.v1.does_not_exist".to_owned(),
        invocation_id: "inv-1".to_owned(),
        args: json!({}),
    };

    let response = OpTable::with_builtins().dispatch(&request);

    assert_eq!(response["success"], Value::Bool(false));
    assert_eq!(
        response["error"]["kind"],
        Value::String("unknown_op".to_owned())
    );
    assert_eq!(
        response["error"]["details"]["op"],
        Value::String("api.v1.does_not_exist".to_owned())
    );
}

#[test]
fn isolated_workspace_ops_are_registered_and_disabled_by_default() -> TestResult {
    let _guard = ISOLATED_ENV_LOCK
        .lock()
        .map_err(|_| "isolated env lock poisoned")?;
    configure_isolated_workspace_for_test(false, None)?;
    std::env::set_var("EOS_ISOLATED_WORKSPACE_TEST_HARNESS", "true");
    let _ = OpTable::with_builtins().dispatch(&Request {
        op: "api.isolated_workspace.test_reset".to_owned(),
        invocation_id: "iws-reset".to_owned(),
        args: json!({}),
    });
    std::env::remove_var("EOS_ISOLATED_WORKSPACE_TEST_HARNESS");
    let table = OpTable::with_builtins();

    let enter = table.dispatch(&Request {
        op: "api.isolated_workspace.enter".to_owned(),
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

    let status = table.dispatch(&Request {
        op: "api.isolated_workspace.status".to_owned(),
        invocation_id: "iws-status".to_owned(),
        args: json!({"caller_id": "caller-a"}),
    });
    assert_eq!(status["success"], Value::Bool(false));
    assert_eq!(
        status["error"]["kind"],
        Value::String("feature_disabled".to_owned())
    );

    let open = table.dispatch(&Request {
        op: "api.isolated_workspace.list_open".to_owned(),
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
    let table = OpTable::with_builtins();
    assert_isolated_test_reset(&table, "iws-reset");

    let enter = dispatch_request(
        &table,
        "api.isolated_workspace.enter",
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

    assert_isolated_open_state(&table, &env.root);
    let exit = dispatch_request(
        &table,
        "api.isolated_workspace.exit",
        "iws-exit",
        json!({"caller_id": "caller-enabled"}),
    );
    assert_isolated_exit(&exit, &handle_scratch)?;
    assert_isolated_status_closed(&table);
    assert_isolated_test_reset(&table, "iws-reset-end");
    Ok(())
}

#[test]
fn isolated_workspace_ops_validate_required_arguments() -> TestResult {
    let _guard = ISOLATED_ENV_LOCK
        .lock()
        .map_err(|_| "isolated env lock poisoned")?;
    configure_isolated_workspace_for_test(false, None)?;
    let response = OpTable::with_builtins().dispatch(&Request {
        op: "api.isolated_workspace.enter".to_owned(),
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
    let table = OpTable::with_builtins();
    let registry = InFlightRegistry::new(300.0, 30.0);
    let task = tokio::spawn(std::future::pending::<()>());
    registry.register("bg-shell", task.abort_handle(), "caller-a", true);
    let context = DispatchContext::with_invocation_registry(&registry);

    let count = table.dispatch_with_context(
        &Request {
            op: "api.v1.inflight_count".to_owned(),
            invocation_id: "count".to_owned(),
            args: json!({"caller_id": "caller-a"}),
        },
        context,
    );
    assert_eq!(count["success"], Value::Bool(true));
    assert_eq!(count["count"], json!(1));

    let command_session_count = table.dispatch_with_context(
        &Request {
            op: "api.v1.command_session_count".to_owned(),
            invocation_id: "command-session-count".to_owned(),
            args: json!({"caller_id": "caller-a"}),
        },
        context,
    );
    assert_eq!(command_session_count["success"], Value::Bool(true));
    assert_eq!(command_session_count["count"], json!(0));

    let heartbeat = table.dispatch_with_context(
        &Request {
            op: "api.v1.heartbeat".to_owned(),
            invocation_id: "heartbeat".to_owned(),
            args: json!({"invocation_ids": ["bg-shell", "missing"]}),
        },
        context,
    );
    assert_eq!(heartbeat["success"], Value::Bool(true));
    assert_eq!(heartbeat["touched"], json!(1));

    let cancel = table.dispatch_with_context(
        &Request {
            op: "api.v1.cancel".to_owned(),
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
    let count = table.dispatch_with_context(
        &Request {
            op: "api.v1.inflight_count".to_owned(),
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

    let request = Envelope::Request(Request {
        op: "api.runtime.ready".to_owned(),
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
        Envelope::Response(value) => value,
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
        op: "api.runtime.ready".to_owned(),
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
        Envelope::Response(value) => value,
        other => return Err(format!("expected response, got {other:?}").into()),
    };
    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["ready"], Value::Bool(true));
    Ok(())
}

fn dispatch_request(table: &OpTable, op: &str, invocation_id: &str, args: Value) -> Value {
    table.dispatch(&Request {
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

fn assert_read_content(table: &OpTable, root: &Path, path: &Value, content: &str) {
    let read = dispatch_request(
        table,
        "api.v1.read_file",
        "read",
        json!({
            "layer_stack_root": root,
            "path": path,
        }),
    );
    assert_eq!(read["success"], Value::Bool(true));
    assert_eq!(read["content"], Value::String(content.to_owned()));
}

fn assert_workspace_base_idempotent(table: &OpTable, root: &Path, workspace: &Path) {
    let ensure_again = dispatch_request(
        table,
        "api.ensure_workspace_base",
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
    table: &OpTable,
    root: &Path,
    workspace: &Path,
    original_ensure: &Value,
) -> TestResult {
    std::fs::write(workspace.join("README.md"), "# reset\n")?;
    let rebuilt = dispatch_request(
        table,
        "api.build_workspace_base",
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
        configure_isolated_workspace_for_test(true, Some(&scratch))?;
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

fn configure_isolated_workspace_for_test(
    enabled: bool,
    scratch_root: Option<&Path>,
) -> TestResult {
    let doc = eos_config::load_prd()?;
    let daemon = doc.section::<eos_config::configs::daemon::DaemonConfig>("daemon")?;
    daemon.validate()?;
    let mut isolated = doc
        .section::<eos_config::configs::isolated_workspace::IsolatedWorkspaceConfig>(
            "isolated_workspace",
        )?;
    isolated.enabled = enabled;
    if let Some(scratch_root) = scratch_root {
        isolated.scratch_root = scratch_root.to_path_buf();
    }
    isolated.validate()?;
    let server_config = ServerConfig {
        socket_path: std::env::temp_dir().join("eos-daemon-test.sock"),
        pid_path: std::env::temp_dir().join("eos-daemon-test.pid"),
        tcp_host: None,
        tcp_port: None,
        auth_token: None,
    };
    let _server = DaemonServer::with_daemon_config(server_config, &daemon, &isolated);
    Ok(())
}

fn assert_isolated_test_reset(table: &OpTable, invocation_id: &str) {
    let reset = dispatch_request(
        table,
        "api.isolated_workspace.test_reset",
        invocation_id,
        json!({}),
    );
    assert_eq!(reset["success"], Value::Bool(true));
}

fn assert_isolated_open_state(table: &OpTable, root: &Path) {
    let status = dispatch_request(
        table,
        "api.isolated_workspace.status",
        "iws-status",
        json!({"caller_id": "caller-enabled"}),
    );
    assert_eq!(status["success"], Value::Bool(true));
    assert_eq!(status["open"], Value::Bool(true));
    assert_eq!(status["manifest_version"], json!(1));

    let duplicate = dispatch_request(
        table,
        "api.isolated_workspace.enter",
        "iws-enter-again",
        json!({
            "caller_id": "caller-enabled",
            "layer_stack_root": root,
        }),
    );
    assert_eq!(duplicate["success"], Value::Bool(false));
    assert_eq!(duplicate["error"]["kind"], "already_open");

    let open = dispatch_request(
        table,
        "api.isolated_workspace.list_open",
        "iws-list",
        json!({}),
    );
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

fn assert_isolated_status_closed(table: &OpTable) {
    let status = dispatch_request(
        table,
        "api.isolated_workspace.status",
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
