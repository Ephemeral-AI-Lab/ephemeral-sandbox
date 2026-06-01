use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use eos_daemon::{DaemonServer, ServerConfig};
use eos_daemon::{DispatchContext, InFlightRegistry, OpTable};
use eos_protocol::{decode, encode, Envelope, Request, DAEMON_AUTH_FIELD};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::time::{sleep, timeout, Duration};

#[test]
fn dispatches_layerstack_read_file() {
    let (root, workspace) = seed_layer_stack("read_file");
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
}

#[test]
fn dispatches_runtime_ready_probe() {
    let (root, _workspace) = seed_layer_stack("ready");
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
}

#[test]
fn dispatches_workspace_base_control_ops_for_fresh_stack() {
    let (root, workspace) = empty_workspace("workspace_base");
    std::fs::create_dir_all(workspace.join("src")).expect("create src");
    std::fs::write(workspace.join("README.md"), "# base\n").expect("write readme");
    std::fs::write(workspace.join("src").join("a.py"), "print('base')\n").expect("write source");
    std::os::unix::fs::symlink("src/a.py", workspace.join("link.py")).expect("create symlink");
    std::fs::create_dir_all(workspace.join("links")).expect("create links");
    let outside_target = workspace
        .parent()
        .expect("workspace parent")
        .join("outside.txt");
    std::fs::write(&outside_target, "outside\n").expect("write outside target");
    std::os::unix::fs::symlink("../src/a.py", workspace.join("links").join("inside"))
        .expect("create relative symlink with parent component");
    std::os::unix::fs::symlink(&outside_target, workspace.join("links").join("outside"))
        .expect("create absolute symlink");
    let table = OpTable::with_builtins();

    let ensure = table.dispatch(&Request {
        op: "api.ensure_workspace_base".to_owned(),
        invocation_id: "ensure".to_owned(),
        args: json!({
            "layer_stack_root": &root,
            "workspace_root": &workspace,
        }),
    });

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
    assert_eq!(
        std::fs::read_link(
            root.join("layers")
                .join("B000001-base")
                .join("links")
                .join("inside")
        )
        .expect("inside symlink")
        .to_string_lossy(),
        "../src/a.py"
    );
    assert_eq!(
        std::fs::read_link(
            root.join("layers")
                .join("B000001-base")
                .join("links")
                .join("outside")
        )
        .expect("outside symlink"),
        outside_target
    );

    let binding = table.dispatch(&Request {
        op: "api.workspace_binding".to_owned(),
        invocation_id: "binding".to_owned(),
        args: json!({"layer_stack_root": &root}),
    });
    assert_eq!(binding["success"], Value::Bool(true));
    assert_eq!(
        binding["binding"]["base_root_hash"],
        ensure["binding"]["base_root_hash"]
    );

    let read = table.dispatch(&Request {
        op: "api.v1.read_file".to_owned(),
        invocation_id: "read".to_owned(),
        args: json!({
            "layer_stack_root": &root,
            "path": workspace.join("README.md"),
        }),
    });
    assert_eq!(read["success"], Value::Bool(true));
    assert_eq!(read["content"], Value::String("# base\n".to_owned()));

    let ensure_again = table.dispatch(&Request {
        op: "api.ensure_workspace_base".to_owned(),
        invocation_id: "ensure-again".to_owned(),
        args: json!({
            "layer_stack_root": &root,
            "workspace_root": &workspace,
        }),
    });
    assert_eq!(ensure_again["success"], Value::Bool(true));
    assert_eq!(ensure_again["created"], Value::Bool(false));

    std::fs::write(workspace.join("README.md"), "# reset\n").expect("rewrite readme");
    let rebuilt = table.dispatch(&Request {
        op: "api.build_workspace_base".to_owned(),
        invocation_id: "rebuild".to_owned(),
        args: json!({
            "layer_stack_root": &root,
            "workspace_root": &workspace,
            "reset": true,
        }),
    });
    assert_eq!(rebuilt["success"], Value::Bool(true));
    assert_eq!(rebuilt["created"], Value::Bool(true));
    assert_ne!(
        rebuilt["binding"]["base_root_hash"],
        ensure["binding"]["base_root_hash"]
    );

    let read_after_reset = table.dispatch(&Request {
        op: "api.v1.read_file".to_owned(),
        invocation_id: "read-after-reset".to_owned(),
        args: json!({
            "layer_stack_root": &root,
            "path": "README.md",
        }),
    });
    assert_eq!(read_after_reset["success"], Value::Bool(true));
    assert_eq!(
        read_after_reset["content"],
        Value::String("# reset\n".to_owned())
    );
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

#[tokio::test]
async fn control_ops_use_inflight_registry() {
    let table = OpTable::with_builtins();
    let registry = InFlightRegistry::new(300.0, 30.0);
    let task = tokio::spawn(std::future::pending::<()>());
    registry.register(
        "bg-shell",
        task.abort_handle(),
        "agent-a",
        "api.v1.shell",
        true,
    );
    let context = DispatchContext::with_in_flight(&registry);

    let count = table.dispatch_with_context(
        &Request {
            op: "api.v1.inflight_count".to_owned(),
            invocation_id: "count".to_owned(),
            args: json!({"agent_id": "agent-a"}),
        },
        context,
    );
    assert_eq!(count["success"], Value::Bool(true));
    assert_eq!(count["count"], json!(1));

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
    assert!(task
        .await
        .expect_err("task should be cancelled")
        .is_cancelled());

    registry.deregister("bg-shell");
    let count = table.dispatch_with_context(
        &Request {
            op: "api.v1.inflight_count".to_owned(),
            invocation_id: "count-after".to_owned(),
            args: json!({"agent_id": "agent-a"}),
        },
        context,
    );
    assert_eq!(count["count"], json!(0));
}

#[tokio::test]
async fn unix_server_dispatches_framed_ready_request() {
    let (root, _workspace) = seed_layer_stack("unix_server");
    let runtime_dir = root
        .parent()
        .expect("seeded layer-stack root must have parent")
        .join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    let config = ServerConfig {
        socket_path: runtime_dir.join("runtime.sock"),
        pid_path: runtime_dir.join("runtime.pid"),
        tcp_host: None,
        tcp_port: None,
        auth_token: None,
    };
    let (server, occ_queue) = DaemonServer::new(config.clone());
    let shutdown = server.shutdown_token();
    let task = tokio::spawn(server.serve(occ_queue));
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
    let mut stream = UnixStream::connect(&config.socket_path)
        .await
        .expect("connect to daemon unix socket");
    stream
        .write_all(&encode(&request).expect("encode request"))
        .await
        .expect("write request");
    stream.shutdown().await.expect("shutdown request writer");
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .expect("daemon response read timed out")
        .expect("read daemon response");
    shutdown.cancel();
    let _ = timeout(Duration::from_secs(2), task)
        .await
        .expect("daemon shutdown timed out")
        .expect("daemon task join failed");

    let response = match decode(&response).expect("decode daemon response") {
        Envelope::Response(value) => value,
        other => panic!("expected response, got {other:?}"),
    };
    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["ready"], Value::Bool(true));
}

#[tokio::test]
async fn tcp_server_dispatches_authenticated_ready_request() {
    let (root, _workspace) = seed_layer_stack("tcp_server");
    let runtime_dir = root
        .parent()
        .expect("seeded layer-stack root must have parent")
        .join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    let probe = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("reserve tcp port");
    let port = probe.local_addr().expect("tcp local addr").port();
    drop(probe);
    let config = ServerConfig {
        socket_path: runtime_dir.join("runtime.sock"),
        pid_path: runtime_dir.join("runtime.pid"),
        tcp_host: Some("127.0.0.1".to_owned()),
        tcp_port: Some(port),
        auth_token: Some("secret".to_owned()),
    };
    let (server, occ_queue) = DaemonServer::new(config.clone());
    let shutdown = server.shutdown_token();
    let task = tokio::spawn(server.serve(occ_queue));
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
    })
    .expect("encode request value");
    value
        .as_object_mut()
        .expect("request value object")
        .insert(DAEMON_AUTH_FIELD.to_owned(), json!("secret"));
    let mut request = serde_json::to_vec(&value).expect("encode authenticated request");
    request.push(b'\n');
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to daemon tcp socket");
    stream
        .write_all(&request)
        .await
        .expect("write authenticated request");
    stream.shutdown().await.expect("shutdown request writer");
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .expect("daemon tcp response read timed out")
        .expect("read daemon tcp response");
    shutdown.cancel();
    let _ = timeout(Duration::from_secs(2), task)
        .await
        .expect("daemon shutdown timed out")
        .expect("daemon task join failed");

    let response = match decode(&response).expect("decode daemon response") {
        Envelope::Response(value) => value,
        other => panic!("expected response, got {other:?}"),
    };
    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["ready"], Value::Bool(true));
}

fn seed_layer_stack(label: &str) -> (PathBuf, PathBuf) {
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
    std::fs::create_dir_all(&workspace).expect("create workspace dir");
    std::fs::create_dir_all(&layer).expect("create base layer dir");
    std::fs::create_dir_all(root.join("staging")).expect("create staging dir");
    std::fs::write(layer.join("README.md"), "# README\n").expect("write read fixture");
    write_json(
        &root.join("manifest.json"),
        &json!({
            "schema_version": 1,
            "version": 1,
            "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
        }),
    );
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
    );
    (root, workspace)
}

fn empty_workspace(label: &str) -> (PathBuf, PathBuf) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = PathBuf::from("/tmp").join(format!(
        "eosd-empty-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let workspace = base.join("workspace");
    let root = base.join("layer-stack");
    std::fs::create_dir_all(&workspace).expect("create workspace dir");
    (root, workspace)
}

fn write_json(path: &Path, value: &Value) {
    let encoded = serde_json::to_string_pretty(value).expect("serialize fixture json");
    std::fs::write(path, encoded).expect("write fixture json");
}
