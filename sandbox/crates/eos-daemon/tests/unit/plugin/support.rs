use super::super::service::{
    acquire_service_snapshot, mark_service_ready, release_service_snapshot,
};
use super::super::PluginRuntime;

use crate::dispatcher::OpTable;
use crate::runtime::context::DispatchContext;
use crate::runtime::services::Services;
use crate::wire::Request;
use eos_config::configs::daemon::PluginRuntimeConfig;
use eos_config::configs::isolated_workspace::IsolatedWorkspaceConfig;
use eos_layerstack::LayerStack;
use eos_plugin::{PpcDirection, PpcEnvelope};
use serde_json::{json, Value};
use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(super) type TestError = Box<dyn Error + Send + Sync + 'static>;
pub(super) type TestResult = Result<(), TestError>;

/// One isolated daemon under test: an op table plus its own `Services`
/// instance (no process-global state survives between tests).
pub(super) struct TestDaemon {
    table: OpTable,
    pub(super) services: Services,
}

impl TestDaemon {
    pub(super) fn new() -> Self {
        Self::with_services(Services::default())
    }

    pub(super) fn with_ppc_root(ppc_root: &Path) -> Self {
        Self::with_services(Services::new(
            PluginRuntimeConfig {
                ppc_root: ppc_root.to_path_buf(),
                ..PluginRuntimeConfig::default()
            },
            IsolatedWorkspaceConfig::default(),
        ))
    }

    pub(super) fn with_isolated_workspace(scratch_root: &Path, workspace_root: &Path) -> Self {
        Self::with_services(Services::new(
            PluginRuntimeConfig::default(),
            IsolatedWorkspaceConfig {
                enabled: true,
                scratch_root: scratch_root.to_path_buf(),
                workspace_root: workspace_root.to_path_buf(),
                ..IsolatedWorkspaceConfig::default()
            },
        ))
    }

    fn with_services(services: Services) -> Self {
        Self {
            table: OpTable::with_builtins(),
            services,
        }
    }

    pub(super) fn plugin(&self) -> &PluginRuntime {
        &self.services.plugin
    }

    /// `api.plugin.ensure` through the adapter (arg parsing + response shaping
    /// + caller gate), without the dispatcher envelope decoration.
    pub(super) fn op_ensure(&self, args: &Value) -> Result<Value, crate::error::DaemonError> {
        crate::ops::plugin::op_ensure(args, self.context())
    }

    /// `api.plugin.status` through the adapter.
    pub(super) fn op_status(&self, args: &Value) -> Result<Value, crate::error::DaemonError> {
        crate::ops::plugin::op_status(args, self.context())
    }

    pub(super) fn context(&self) -> DispatchContext<'_> {
        DispatchContext::with_services(&self.services)
    }

    pub(super) fn dispatch(&self, request: &Request) -> Value {
        self.table.dispatch_with_context(request, self.context())
    }
}

pub(super) struct TestEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl TestEnvVar {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for TestEnvVar {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

pub(super) fn value_array<'a>(
    value: &'a Value,
    context: &'static str,
) -> Result<&'a Vec<Value>, TestError> {
    value
        .as_array()
        .ok_or_else(|| std::io::Error::other(context).into())
}

pub(super) fn value_str<'a>(value: &'a Value, context: &'static str) -> Result<&'a str, TestError> {
    value
        .as_str()
        .ok_or_else(|| std::io::Error::other(context).into())
}

pub(super) fn some_value<T>(value: Option<T>, context: &'static str) -> Result<T, TestError> {
    value.ok_or_else(|| std::io::Error::other(context).into())
}

pub(super) fn ppc_stream_pair() -> Result<
    (
        std::os::unix::net::UnixStream,
        std::os::unix::net::UnixStream,
    ),
    TestError,
> {
    Ok(std::os::unix::net::UnixStream::pair()?)
}

pub(super) fn read_ppc_request(
    stream: &mut std::os::unix::net::UnixStream,
    context: &'static str,
) -> Result<PpcEnvelope, TestError> {
    let frame = eos_plugin_runtime::read_frame(stream)?;
    PpcEnvelope::decode(&frame)
        .map_err(|err| std::io::Error::other(format!("{context}: {err}")).into())
}

pub(super) fn write_ppc_reply_json_result(
    stream: &mut std::os::unix::net::UnixStream,
    message_id: String,
    body: &Value,
) -> TestResult {
    let reply = PpcEnvelope {
        message_id,
        direction: PpcDirection::Reply,
        op: "reply".to_owned(),
        body: serde_json::to_string(body)?,
    };
    stream.write_all(&reply.encode()?)?;
    Ok(())
}

pub(super) fn write_ppc_reply_result(
    stream: &mut std::os::unix::net::UnixStream,
    message_id: String,
    body: &'static str,
) -> TestResult {
    let reply = PpcEnvelope {
        message_id,
        direction: PpcDirection::Reply,
        op: "reply".to_owned(),
        body: body.to_owned(),
    };
    stream.write_all(&reply.encode()?)?;
    Ok(())
}

pub(super) fn join_test_thread(
    handle: std::thread::JoinHandle<TestResult>,
    context: &'static str,
) -> TestResult {
    handle
        .join()
        .map_err(|_| std::io::Error::other(context))??;
    Ok(())
}

pub(super) fn join_value_thread(
    handle: std::thread::JoinHandle<Result<Value, TestError>>,
    context: &'static str,
) -> Result<Value, TestError> {
    handle.join().map_err(|_| std::io::Error::other(context))?
}

pub(super) fn generic_service_manifest(digest: &str, op_name: &str) -> Value {
    json!({
        "plugin_id": "generic",
        "plugin_version": "0.1.0",
        "plugin_digest": digest,
        "services": [{
            "service_id": "worker",
            "service_profile_digest": format!("profile-{digest}"),
            "service_mode": "workspace_snapshot_refresh",
            "refresh_strategy": "remount_workspace_and_notify",
            "command": ["generic-service", "--stdio"],
            "ppc_protocol_version": 1
        }],
        "operations": [{
            "op_name": op_name,
            "intent": "read_only",
            "service_id": "worker"
        }]
    })
}

pub(super) fn generic_service_manifest_with_command(
    digest: &str,
    op_name: &str,
    command: Vec<&str>,
) -> Value {
    let mut manifest = generic_service_manifest(digest, op_name);
    manifest["services"][0]["command"] =
        Value::Array(command.into_iter().map(|item| json!(item)).collect());
    manifest
}

pub(super) fn generic_restart_manifest(digest: &str, op_name: &str, command: Vec<&str>) -> Value {
    let mut manifest = generic_service_manifest_with_command(digest, op_name, command);
    manifest["services"][0]["refresh_strategy"] = json!("restart_service");
    manifest
}

pub(super) fn generic_self_managed_manifest(digest: &str, op_name: &str) -> Value {
    let mut manifest = generic_service_manifest(digest, op_name);
    manifest["operations"][0]["intent"] = json!("write_allowed");
    manifest["operations"][0]["auto_workspace_overlay"] = json!(false);
    manifest
}

pub(super) fn oneshot_overlay_manifest(digest: &str, op_name: &str) -> Value {
    json!({
        "plugin_id": "generic",
        "plugin_version": "0.1.0",
        "plugin_digest": digest,
        "services": [{
            "service_id": "worker",
            "service_profile_digest": format!("oneshot-profile-{digest}"),
            "service_mode": "oneshot_overlay",
            "refresh_strategy": "restart_service",
            "command": ["python3", "/eos/plugin/oneshot.py"],
            "ppc_protocol_version": 1
        }],
        "operations": [{
            "op_name": op_name,
            "intent": "write_allowed",
            "service_id": "worker",
            "timeout_ms": 5000
        }]
    })
}

pub(super) fn test_socket_root(name: &str) -> PathBuf {
    let root = PathBuf::from("target").join(format!("ppc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

pub(super) fn test_layer_stack_root(name: &str) -> Result<PathBuf, TestError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "eos-plugin-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("layer-stack");
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

pub(super) fn test_bound_workspace(name: &str) -> Result<(PathBuf, PathBuf), TestError> {
    let layer_stack_root = test_layer_stack_root(name)?;
    let base = some_value(layer_stack_root.parent(), "layer root must have a parent")?;
    let workspace_root = base.join("workspace");
    std::fs::create_dir_all(&workspace_root)?;
    std::fs::write(workspace_root.join("seed.txt"), "seed\n")?;
    eos_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, true)?;
    Ok((layer_stack_root, workspace_root))
}

pub(super) fn attach_service_snapshot_for_tests(
    plugin: &PluginRuntime,
    op: &str,
) -> Result<(String, String), TestError> {
    let route = some_value(plugin.route_for_op(op)?, "registered plugin route missing")?;
    let service_key = some_value(route.service_key, "service key missing")?;
    let service_instance_id = some_value(route.service_instance_id, "service instance id missing")?;
    let snapshot = acquire_service_snapshot(&service_key, "test-health")?;
    let manifest_key = snapshot.manifest_key.clone();
    let old_snapshot = {
        let mut state = plugin.lock_state()?;
        mark_service_ready(&mut state, &service_instance_id, &snapshot, false)?;
        state
            .service_snapshots
            .insert(service_instance_id.clone(), snapshot)
    };
    if let Some(old_snapshot) = old_snapshot {
        release_service_snapshot(&old_snapshot);
    }
    Ok((service_instance_id, manifest_key))
}

pub(super) fn remove_test_tree(layer_stack_root: &Path) -> TestResult {
    let base = some_value(
        layer_stack_root.parent(),
        "test layer root must have a parent",
    )?;
    let _ = std::fs::remove_dir_all(base);
    Ok(())
}

pub(super) fn read_layer_text(root: &Path, path: &str) -> Result<String, TestError> {
    Ok(LayerStack::open(root.to_path_buf())?.read_text(path)?.0)
}

pub(super) fn spawn_replying_connector(
    socket_root: PathBuf,
    reply_body: &'static str,
) -> std::thread::JoinHandle<TestResult> {
    std::thread::spawn(move || -> TestResult {
        let socket = wait_for_socket(&socket_root)?;
        let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
        let request = read_ppc_request(&mut stream, "read ppc request")?;
        write_ppc_reply_result(&mut stream, request.message_id, reply_body)?;
        Ok(())
    })
}

pub(super) fn spawn_restart_connector(
    socket_root: PathBuf,
    allow_reconnect_rx: mpsc::Receiver<()>,
    reply_body: &'static str,
) -> std::thread::JoinHandle<TestResult> {
    std::thread::spawn(move || -> TestResult {
        let _old_stream = connect_ppc_socket(&socket_root)?;
        allow_reconnect_rx.recv()?;

        let mut stream = connect_ppc_socket(&socket_root)?;
        let request = read_ppc_request(&mut stream, "read restarted ppc request")?;
        assert_eq!(request.op, "plugin.generic.hover");
        write_ppc_reply_result(&mut stream, request.message_id, reply_body)?;
        Ok(())
    })
}

fn wait_for_socket(root: &Path) -> Result<PathBuf, std::io::Error> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("sock") {
                    return Ok(path);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out waiting for socket under {}", root.display()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
