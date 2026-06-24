use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sandbox_manager::LocalSandboxDaemonInstaller;
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, ManagerServices, SandboxDaemonClient,
    SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxId, SandboxRecord, SandboxRuntime,
    SandboxState, SandboxStore,
};
use sandbox_protocol::{ArgKind, CliOperationExecutionSpace, CliOperationScope, Request, Response};
use serde_json::{json, Value};

#[derive(Default)]
struct FakeRuntime {
    created: Mutex<Vec<(String, PathBuf)>>,
    destroyed: Mutex<Vec<String>>,
}

impl SandboxRuntime for FakeRuntime {
    fn create_sandbox(
        &self,
        request: &CreateSandboxRequest,
    ) -> Result<CreateSandboxResult, ManagerError> {
        self.created
            .lock()
            .expect("created lock")
            .push((request.image.clone(), request.workspace_root.clone()));
        Ok(CreateSandboxResult {
            id: id("container-1"),
        })
    }

    fn destroy_sandbox(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        self.destroyed
            .lock()
            .expect("destroyed lock")
            .push(record.id.as_str().to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct FakeInstaller {
    started: Mutex<Vec<String>>,
    stopped: Mutex<Vec<String>>,
}

impl SandboxDaemonInstaller for FakeInstaller {
    fn install_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn start_daemon(&self, record: &SandboxRecord) -> Result<SandboxDaemonEndpoint, ManagerError> {
        self.started
            .lock()
            .expect("started lock")
            .push(record.id.as_str().to_owned());
        Ok(SandboxDaemonEndpoint::new(
            PathBuf::from(format!("/tmp/{}.sock", record.id.as_str())),
            Some("token".to_owned()),
        ))
    }

    fn stop_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        self.stopped
            .lock()
            .expect("stopped lock")
            .push(record.id.as_str().to_owned());
        Ok(())
    }

    fn check_daemon(&self, _endpoint: &SandboxDaemonEndpoint) -> Result<(), ManagerError> {
        Ok(())
    }
}

struct FakeClient;

impl SandboxDaemonClient for FakeClient {
    fn invoke(
        &self,
        _endpoint: &SandboxDaemonEndpoint,
        _request: sandbox_protocol::Request,
    ) -> Result<Response, ManagerError> {
        Ok(Response::ok(json!({"forwarded": true})))
    }
}

#[derive(Default)]
struct RecordingTreeClient {
    invocations: Mutex<Vec<TreeInvocation>>,
    failures: Mutex<Vec<String>>,
}

#[derive(Debug)]
struct TreeInvocation {
    socket_path: PathBuf,
    op: String,
    scope: CliOperationScope,
    args: Value,
    timeout: Duration,
}

impl RecordingTreeClient {
    fn fail_sandbox(&self, sandbox_id: &str) {
        self.failures
            .lock()
            .expect("failures lock")
            .push(sandbox_id.to_owned());
    }

    fn invocations(&self) -> Vec<TreeInvocation> {
        self.invocations
            .lock()
            .expect("invocations lock")
            .iter()
            .map(|invocation| TreeInvocation {
                socket_path: invocation.socket_path.clone(),
                op: invocation.op.clone(),
                scope: invocation.scope.clone(),
                args: invocation.args.clone(),
                timeout: invocation.timeout,
            })
            .collect()
    }
}

impl SandboxDaemonClient for RecordingTreeClient {
    fn invoke(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: Request,
    ) -> Result<Response, ManagerError> {
        self.invoke_with_timeout(endpoint, request, Duration::from_millis(30_000))
    }

    fn invoke_with_timeout(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: Request,
        timeout: Duration,
    ) -> Result<Response, ManagerError> {
        let sandbox_id = request
            .scope
            .sandbox_id()
            .expect("private daemon request has sandbox scope")
            .to_owned();
        self.invocations
            .lock()
            .expect("invocations lock")
            .push(TreeInvocation {
                socket_path: endpoint.socket_path.clone(),
                op: request.op.clone(),
                scope: request.scope.clone(),
                args: request.args.clone(),
                timeout,
            });
        if self
            .failures
            .lock()
            .expect("failures lock")
            .iter()
            .any(|failed| failed == &sandbox_id)
        {
            return Err(ManagerError::ForwardingFailed {
                message: format!("daemon {sandbox_id} timed out"),
            });
        }
        Ok(Response::ok(daemon_snapshot(&sandbox_id)))
    }
}

struct SlowTreeClient {
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl SlowTreeClient {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        }
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

impl SandboxDaemonClient for SlowTreeClient {
    fn invoke(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: Request,
    ) -> Result<Response, ManagerError> {
        self.invoke_with_timeout(endpoint, request, Duration::from_millis(30_000))
    }

    fn invoke_with_timeout(
        &self,
        _endpoint: &SandboxDaemonEndpoint,
        request: Request,
        _timeout: Duration,
    ) -> Result<Response, ManagerError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(10));
        self.active.fetch_sub(1, Ordering::SeqCst);
        let sandbox_id = request
            .scope
            .sandbox_id()
            .expect("private daemon request has sandbox scope");
        Ok(Response::ok(daemon_snapshot(sandbox_id)))
    }
}

fn services() -> (
    ManagerServices,
    Arc<FakeRuntime>,
    Arc<FakeInstaller>,
    Arc<FakeClient>,
) {
    let store = Arc::new(SandboxStore::new());
    let runtime = Arc::new(FakeRuntime::default());
    let installer = Arc::new(FakeInstaller::default());
    let client = Arc::new(FakeClient);
    let services = ManagerServices::new(
        Arc::clone(&store),
        runtime.clone(),
        installer.clone(),
        client.clone(),
    );
    (services, runtime, installer, client)
}

fn services_with_client(
    client: Arc<dyn SandboxDaemonClient>,
) -> (ManagerServices, Arc<SandboxStore>) {
    let store = Arc::new(SandboxStore::new());
    let runtime = Arc::new(FakeRuntime::default());
    let installer = Arc::new(FakeInstaller::default());
    let services = ManagerServices::new(Arc::clone(&store), runtime, installer, client);
    (services, store)
}

fn dispatch(services: &ManagerServices, op: &str, args: Value) -> Value {
    let request = Request::new(op, "req-1", CliOperationScope::System, args);
    sandbox_manager::dispatch_operation(services, &request).into_json_value()
}

fn id(value: &str) -> SandboxId {
    SandboxId::new(value).expect("valid sandbox id")
}

fn endpoint(value: &str) -> SandboxDaemonEndpoint {
    SandboxDaemonEndpoint::new(PathBuf::from(format!("/tmp/{value}.sock")), None)
}

fn sandbox_record(
    value: &str,
    state: SandboxState,
    daemon: Option<SandboxDaemonEndpoint>,
) -> SandboxRecord {
    SandboxRecord {
        id: id(value),
        workspace_root: PathBuf::from("/testbed"),
        state,
        daemon,
    }
}

fn daemon_snapshot(sandbox_id: &str) -> Value {
    json!({
        "sandbox_id": sandbox_id,
        "lifecycle_state": "daemon-ready",
        "availability": "available",
        "sampled_at_unix_ms": 1_000,
        "errors": [],
        "daemon": {
            "socket_path": format!("/daemon/{sandbox_id}/runtime.sock"),
            "pid_path": format!("/daemon/{sandbox_id}/runtime.pid"),
            "daemon_pid": 42,
            "runtime_dir": format!("/daemon/{sandbox_id}"),
        },
        "resources": {
            "latest": Value::Null,
            "history": [],
        },
        "workspaces": [],
        "recent_traces": [],
    })
}

#[cfg(unix)]
fn temp_root(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(std::env::temp_dir().join(format!(
        "sandbox-manager-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    )))
}

#[test]
fn cli_operation_catalog_contains_only_manager_operations() {
    let catalog = sandbox_manager::cli_operation_catalog();
    let names = catalog
        .operations
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    assert_eq!(
        catalog.operation_execution_space,
        CliOperationExecutionSpace::Manager
    );
    assert_eq!(catalog.families.len(), 1);
    assert_eq!(catalog.families[0].title, "Management");
    assert_eq!(
        names,
        [
            "create_sandbox",
            "destroy_sandbox",
            "get_observability_tree",
            "list_sandboxes",
            "inspect_sandbox",
        ]
    );
    assert!(catalog.operations.iter().all(|spec| !matches!(
        spec.name,
        "exec_command" | "write_command_stdin" | "read_command_lines"
    )));
    assert!(catalog
        .operations
        .iter()
        .all(|spec| spec.family == "management"));
    assert!(catalog.operations.iter().any(|spec| spec
        .args
        .iter()
        .any(|arg| arg.name == "sandbox_id" && arg.kind == ArgKind::String)));
    assert!(catalog.operations.iter().all(|spec| {
        spec.cli
            .map(|cli| {
                cli.examples
                    .iter()
                    .all(|example| example.starts_with("sandbox-cli manager "))
            })
            .unwrap_or(true)
    }));
}

#[test]
fn create_list_inspect_destroy_sandbox_with_fake_runtime() {
    let (services, runtime, installer, _client) = services();

    let created = dispatch(
        &services,
        "create_sandbox",
        json!({"image": "ubuntu:24.04", "workspace_root": "/testbed"}),
    );
    assert_eq!(created["id"], "container-1");
    assert_eq!(created["workspace_root"], "/testbed");
    assert_eq!(created["state"], "ready");
    assert_eq!(created["daemon"]["socket_path"], "/tmp/container-1.sock");
    assert_eq!(created["daemon"]["auth_token_configured"], true);

    let listed = dispatch(&services, "list_sandboxes", json!({}));
    assert_eq!(listed["sandboxes"][0]["id"], "container-1");
    assert_eq!(
        listed["sandboxes"][0]["daemon"]["socket_path"],
        "/tmp/container-1.sock"
    );

    let inspected = dispatch(
        &services,
        "inspect_sandbox",
        json!({"sandbox_id": "container-1"}),
    );
    assert_eq!(inspected["id"], "container-1");

    let destroyed = dispatch(
        &services,
        "destroy_sandbox",
        json!({"sandbox_id": "container-1"}),
    );
    assert_eq!(destroyed["state"], "stopped");

    let listed = dispatch(&services, "list_sandboxes", json!({}));
    assert_eq!(
        listed["sandboxes"]
            .as_array()
            .expect("sandboxes array")
            .len(),
        0
    );
    assert_eq!(
        runtime.created.lock().expect("created lock").as_slice(),
        [("ubuntu:24.04".to_owned(), PathBuf::from("/testbed"))]
    );
    assert_eq!(
        runtime.destroyed.lock().expect("destroyed lock").as_slice(),
        ["container-1"]
    );
    assert_eq!(
        installer.started.lock().expect("started lock").as_slice(),
        ["container-1"]
    );
    assert_eq!(
        installer.stopped.lock().expect("stopped lock").as_slice(),
        ["container-1"]
    );
}

#[test]
fn get_observability_tree_aggregates_ready_sandboxes_with_private_daemon_requests() {
    let client = Arc::new(RecordingTreeClient::default());
    let (services, store) = services_with_client(client.clone());
    store
        .insert(sandbox_record(
            "sbox-1",
            SandboxState::Ready,
            Some(endpoint("sbox-1")),
        ))
        .expect("insert ready sandbox");
    store
        .insert(sandbox_record(
            "sbox-2",
            SandboxState::Ready,
            Some(endpoint("sbox-2")),
        ))
        .expect("insert ready sandbox");
    store
        .insert(sandbox_record(
            "creating",
            SandboxState::Creating,
            Some(endpoint("creating")),
        ))
        .expect("insert non-ready sandbox");

    let response = dispatch(
        &services,
        "get_observability_tree",
        json!({
            "include_recent_traces": 1,
            "trace_limit": 500,
            "resource_window_ms": 999_999,
        }),
    );

    let sandboxes = response["sandboxes"].as_array().expect("sandboxes array");
    assert_eq!(sandboxes.len(), 2);
    assert_eq!(sandboxes[0]["sandbox_id"], "sbox-1");
    assert_eq!(sandboxes[0]["lifecycle_state"], "ready");
    assert_eq!(sandboxes[0]["availability"], "available");
    assert_eq!(sandboxes[1]["sandbox_id"], "sbox-2");

    let invocations = client.invocations();
    assert_eq!(invocations.len(), 2);
    assert!(invocations
        .iter()
        .all(|invocation| invocation.op == "get_observability_snapshot"));
    assert!(invocations.iter().all(|invocation| {
        matches!(
            &invocation.scope,
            CliOperationScope::Sandbox { sandbox_id }
                if sandbox_id == "sbox-1" || sandbox_id == "sbox-2"
        )
    }));
    assert!(invocations
        .iter()
        .all(|invocation| invocation.args["include_recent_traces"] == true));
    assert!(invocations
        .iter()
        .all(|invocation| invocation.args["trace_limit"] == 100));
    assert!(invocations
        .iter()
        .all(|invocation| invocation.args["resource_window_ms"] == 600_000));
    assert!(invocations
        .iter()
        .all(|invocation| invocation.timeout == Duration::from_millis(1_500)));
}

#[test]
fn get_observability_tree_converts_one_daemon_failure_to_one_unavailable_node() {
    let client = Arc::new(RecordingTreeClient::default());
    client.fail_sandbox("sbox-2");
    let (services, store) = services_with_client(client);
    store
        .insert(sandbox_record(
            "sbox-1",
            SandboxState::Ready,
            Some(endpoint("sbox-1")),
        ))
        .expect("insert ready sandbox");
    store
        .insert(sandbox_record(
            "sbox-2",
            SandboxState::Ready,
            Some(endpoint("sbox-2")),
        ))
        .expect("insert ready sandbox");

    let response = dispatch(&services, "get_observability_tree", json!({}));

    assert!(response.get("error").is_none());
    let sandboxes = response["sandboxes"].as_array().expect("sandboxes array");
    assert_eq!(sandboxes.len(), 2);
    assert_eq!(sandboxes[0]["availability"], "available");
    assert_eq!(sandboxes[1]["sandbox_id"], "sbox-2");
    assert_eq!(sandboxes[1]["lifecycle_state"], "ready");
    assert_eq!(sandboxes[1]["availability"], "unavailable");
    assert!(sandboxes[1]["errors"][0]
        .as_str()
        .expect("error text")
        .contains("daemon sbox-2 timed out"));
}

#[test]
fn get_observability_tree_errors_for_explicit_unknown_sandbox_id() {
    let client = Arc::new(RecordingTreeClient::default());
    let (services, _store) = services_with_client(client);

    let response = dispatch(
        &services,
        "get_observability_tree",
        json!({"sandbox_id": "missing"}),
    );

    assert_eq!(
        response["error"]["kind"],
        sandbox_protocol::error_kind::INVALID_REQUEST
    );
    assert!(response["error"]["message"]
        .as_str()
        .expect("message")
        .contains("sandbox not found: missing"));
}

#[test]
fn get_observability_tree_returns_unavailable_node_for_explicit_non_ready_sandbox() {
    let client = Arc::new(RecordingTreeClient::default());
    let (services, store) = services_with_client(client.clone());
    store
        .insert(sandbox_record(
            "creating",
            SandboxState::Creating,
            Some(endpoint("creating")),
        ))
        .expect("insert non-ready sandbox");

    let response = dispatch(
        &services,
        "get_observability_tree",
        json!({"sandbox_id": "creating"}),
    );

    let sandboxes = response["sandboxes"].as_array().expect("sandboxes array");
    assert_eq!(sandboxes.len(), 1);
    assert_eq!(sandboxes[0]["sandbox_id"], "creating");
    assert_eq!(sandboxes[0]["lifecycle_state"], "creating");
    assert_eq!(sandboxes[0]["availability"], "unavailable");
    assert!(client.invocations().is_empty());
}

#[test]
fn get_observability_tree_bounds_daemon_fanout_concurrency() {
    let client = Arc::new(SlowTreeClient::new());
    let (services, store) = services_with_client(client.clone());
    for index in 0..12 {
        let sandbox_id = format!("sbox-{index}");
        store
            .insert(sandbox_record(
                &sandbox_id,
                SandboxState::Ready,
                Some(endpoint(&sandbox_id)),
            ))
            .expect("insert ready sandbox");
    }

    let response = dispatch(&services, "get_observability_tree", json!({}));

    assert_eq!(
        response["sandboxes"]
            .as_array()
            .expect("sandboxes array")
            .len(),
        12
    );
    assert!(
        client.max_active() <= 8,
        "manager fan-out exceeded cap: {}",
        client.max_active()
    );
}

#[test]
fn local_daemon_installer_launch_spec_passes_dynamic_sandbox_id() {
    let installer = LocalSandboxDaemonInstaller::new(
        "/bin/sandbox-daemon",
        "/etc/eos/prd.yml",
        "/tmp/eos-daemons",
        Some("secret-token".to_owned()),
    );
    let record = SandboxRecord::new(
        id("container-1"),
        PathBuf::from("/testbed"),
        SandboxState::Ready,
    );

    let spec = installer
        .launch_spec(&record)
        .expect("launch spec builds from record");

    assert_eq!(spec.executable, PathBuf::from("/bin/sandbox-daemon"));
    assert_eq!(
        spec.socket_path,
        PathBuf::from("/tmp/eos-daemons/container-1/runtime.sock")
    );
    assert_eq!(
        spec.pid_path,
        PathBuf::from("/tmp/eos-daemons/container-1/runtime.pid")
    );
    assert!(spec
        .args
        .windows(2)
        .any(|window| window[0] == "--sandbox-id" && window[1] == "container-1"));
    assert!(spec
        .args
        .windows(2)
        .any(|window| window[0] == "--workspace-root" && window[1] == "/testbed"));
    assert!(!spec.args.iter().any(|arg| arg == "secret-token"));
    assert_eq!(spec.auth_token.as_deref(), Some("secret-token"));
}

#[cfg(unix)]
#[test]
fn local_daemon_installer_stop_daemon_terminates_pid_file_process(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("daemon-stop")?;
    let workspace_root = root.join("workspace");
    let runtime_root = root.join("runtime");
    std::fs::create_dir_all(&workspace_root)?;
    let installer = LocalSandboxDaemonInstaller::new(
        "/bin/sandbox-daemon",
        root.join("config.yml"),
        runtime_root,
        None,
    );
    let record = SandboxRecord::new(id("container-1"), workspace_root, SandboxState::Ready);
    let spec = installer.launch_spec(&record)?;
    std::fs::create_dir_all(spec.pid_path.parent().expect("pid path parent"))?;
    std::fs::write(&spec.socket_path, b"socket placeholder")?;

    let child = Command::new("/bin/sleep").arg("30").spawn()?;
    let pid = child.id();
    let _cleanup = ChildCleanup::new(child);
    std::fs::write(&spec.pid_path, pid.to_string())?;

    installer.stop_daemon(&record)?;

    assert!(
        !pid_exists(pid),
        "daemon pid {pid} should be gone after stop_daemon"
    );
    assert!(!spec.pid_path.exists());
    assert!(!spec.socket_path.exists());

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_daemon_installer_stop_daemon_rejects_socket_without_pid_file(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("daemon-stop-missing-pid")?;
    let workspace_root = root.join("workspace");
    let runtime_root = root.join("runtime");
    std::fs::create_dir_all(&workspace_root)?;
    let installer = LocalSandboxDaemonInstaller::new(
        "/bin/sandbox-daemon",
        root.join("config.yml"),
        runtime_root,
        None,
    );
    let record = SandboxRecord::new(id("container-1"), workspace_root, SandboxState::Ready);
    let spec = installer.launch_spec(&record)?;
    std::fs::create_dir_all(spec.socket_path.parent().expect("socket path parent"))?;
    std::fs::write(&spec.socket_path, b"socket placeholder")?;

    let error = installer
        .stop_daemon(&record)
        .expect_err("socket without pid is not silently cleaned up");

    assert!(
        matches!(error, ManagerError::DaemonInstallFailed { .. }),
        "unexpected error: {error}"
    );
    assert!(
        spec.socket_path.exists(),
        "socket artifact should remain for failed stop diagnosis"
    );

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn store_duplicate_and_missing_sandbox_error_cases() {
    let store = SandboxStore::new();
    store
        .insert(SandboxRecord::new(
            id("sbox-1"),
            PathBuf::from("/testbed"),
            SandboxState::Ready,
        ))
        .expect("insert sandbox");

    let duplicate = store
        .create(id("sbox-1"), PathBuf::from("/testbed"))
        .expect_err("duplicate should fail");
    assert!(matches!(duplicate, ManagerError::DuplicateSandbox { .. }));

    let missing = store
        .inspect(&id("missing"))
        .expect_err("missing should fail");
    assert!(matches!(missing, ManagerError::MissingSandbox { .. }));
}

#[cfg(unix)]
struct ChildCleanup {
    child: Child,
}

#[cfg(unix)]
impl ChildCleanup {
    fn new(child: Child) -> Self {
        Self { child }
    }
}

#[cfg(unix)]
impl Drop for ChildCleanup {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

#[cfg(unix)]
fn pid_exists(pid: u32) -> bool {
    let pid = nix::unistd::Pid::from_raw(pid.try_into().expect("test pid fits nix pid"));
    nix::sys::signal::kill(pid, None).is_ok()
}
