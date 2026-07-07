use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sandbox_manager::LocalSandboxDaemonInstaller;
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, ManagerServices, SandboxDaemonClient,
    SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxId, SandboxRecord, SandboxRuntime,
    SandboxState, SandboxStore, SharedBaseMount, StartedDaemon,
};
use sandbox_protocol::{ArgKind, CliOperationExecutionSpace, CliOperationScope, Request, Response};
use serde_json::{json, Value};

#[derive(Default)]
struct FakeRuntime {
    created: Mutex<Vec<(String, PathBuf, Option<SharedBaseMount>)>>,
    destroyed: Mutex<Vec<String>>,
}

impl SandboxRuntime for FakeRuntime {
    fn create_sandbox(
        &self,
        request: &CreateSandboxRequest,
    ) -> Result<CreateSandboxResult, ManagerError> {
        let mut created = self.created.lock().expect("created lock");
        created.push((
            request.image.clone(),
            request.workspace_root.clone(),
            request.shared_base.clone(),
        ));
        let sandbox_id = format!("container-{}", created.len());
        Ok(CreateSandboxResult {
            id: SandboxId::new(sandbox_id).expect("valid id"),
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
    checked: Mutex<Vec<String>>,
    fail_install: bool,
    fail_start: bool,
    fail_check: bool,
}

impl FakeInstaller {
    fn failing_install() -> Self {
        Self {
            fail_install: true,
            ..Self::default()
        }
    }

    fn failing_start() -> Self {
        Self {
            fail_start: true,
            ..Self::default()
        }
    }

    fn failing_check() -> Self {
        Self {
            fail_check: true,
            ..Self::default()
        }
    }
}

impl SandboxDaemonInstaller for FakeInstaller {
    fn install_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        if self.fail_install {
            return Err(ManagerError::DaemonInstallFailed {
                message: "install stage failed".to_owned(),
            });
        }
        Ok(())
    }

    fn start_daemon(&self, record: &SandboxRecord) -> Result<StartedDaemon, ManagerError> {
        if self.fail_start {
            return Err(ManagerError::DaemonInstallFailed {
                message: "start stage failed".to_owned(),
            });
        }
        self.started
            .lock()
            .expect("started lock")
            .push(record.id.as_str().to_owned());
        Ok(StartedDaemon {
            daemon: SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                format!("token-{}", record.id.as_str()),
            ),
            daemon_http: None,
        })
    }

    fn stop_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        self.stopped
            .lock()
            .expect("stopped lock")
            .push(record.id.as_str().to_owned());
        Ok(())
    }

    fn check_daemon(
        &self,
        record: &SandboxRecord,
        _endpoint: &SandboxDaemonEndpoint,
    ) -> Result<(), ManagerError> {
        self.checked
            .lock()
            .expect("checked lock")
            .push(record.id.as_str().to_owned());
        if self.fail_check {
            return Err(ManagerError::DaemonInstallFailed {
                message: "check stage failed".to_owned(),
            });
        }
        Ok(())
    }
}

struct FakeClient;

impl SandboxDaemonClient for FakeClient {
    fn invoke_with_timeout(
        &self,
        _endpoint: &SandboxDaemonEndpoint,
        _request: sandbox_protocol::Request,
        _timeout: Duration,
    ) -> Result<Response, ManagerError> {
        Ok(Response::ok(json!({"forwarded": true})))
    }
}

#[derive(Default)]
struct RecordingSnapshotClient {
    invocations: Mutex<Vec<SnapshotInvocation>>,
    failures: Mutex<Vec<String>>,
}

#[derive(Debug)]
struct SnapshotInvocation {
    op: String,
    scope: CliOperationScope,
    args: Value,
    timeout: Duration,
}

impl RecordingSnapshotClient {
    fn fail_sandbox(&self, sandbox_id: &str) {
        self.failures
            .lock()
            .expect("failures lock")
            .push(sandbox_id.to_owned());
    }

    fn invocations(&self) -> Vec<SnapshotInvocation> {
        self.invocations
            .lock()
            .expect("invocations lock")
            .iter()
            .map(|invocation| SnapshotInvocation {
                op: invocation.op.clone(),
                scope: invocation.scope.clone(),
                args: invocation.args.clone(),
                timeout: invocation.timeout,
            })
            .collect()
    }
}

impl SandboxDaemonClient for RecordingSnapshotClient {
    fn invoke_with_timeout(
        &self,
        _endpoint: &SandboxDaemonEndpoint,
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
            .push(SnapshotInvocation {
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

struct SlowSnapshotClient {
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl SlowSnapshotClient {
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

impl SandboxDaemonClient for SlowSnapshotClient {
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

fn services_with_installer(
    installer: Arc<FakeInstaller>,
) -> (ManagerServices, Arc<FakeRuntime>, Arc<SandboxStore>) {
    let store = Arc::new(SandboxStore::new());
    let runtime = Arc::new(FakeRuntime::default());
    let client = Arc::new(FakeClient);
    let services = ManagerServices::new(Arc::clone(&store), runtime.clone(), installer, client);
    (services, runtime, store)
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

struct TestWorkspace {
    root: PathBuf,
    workspace: PathBuf,
}

impl TestWorkspace {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sandbox-manager-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time after epoch")
                .as_nanos()
        ));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("create test workspace");
        std::fs::write(workspace.join("README.md"), b"base\n").expect("seed test workspace");
        Self { root, workspace }
    }

    fn path(&self) -> &Path {
        &self.workspace
    }

    fn path_string(&self) -> String {
        self.workspace.to_string_lossy().into_owned()
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn endpoint(value: &str) -> SandboxDaemonEndpoint {
    SandboxDaemonEndpoint::new("127.0.0.1", 7000, format!("token-{value}"))
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
        daemon_http: None,
        shared_base: None,
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

#[cfg(unix)]
fn daemon_file_paths(runtime_root: &std::path::Path, sandbox_id: &str) -> (PathBuf, PathBuf) {
    let runtime_dir = runtime_root.join(sandbox_id);
    (
        runtime_dir.join("runtime.sock"),
        runtime_dir.join("runtime.pid"),
    )
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
            "list_sandboxes",
            "inspect_sandbox",
            "checkpoint_squash",
            "export_changes",
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
                    .all(|example| example.starts_with("sandbox-manager-cli "))
            })
            .unwrap_or(true)
    }));
}

#[test]
fn create_list_inspect_destroy_sandbox_with_fake_runtime() {
    let (services, runtime, installer, _client) = services();
    let workspace = TestWorkspace::new("create-list");
    let workspace_root = workspace.path_string();

    let created = dispatch(
        &services,
        "create_sandbox",
        json!({"image": "ubuntu:24.04", "workspace_root": workspace_root.clone()}),
    );
    assert_eq!(created["id"], "container-1");
    assert_eq!(
        created["workspace_root"].as_str(),
        Some(workspace_root.as_str())
    );
    assert_eq!(created["state"], "ready");
    assert_eq!(
        created["daemon"],
        json!({"host": "127.0.0.1", "port": 7000})
    );

    let listed = dispatch(&services, "list_sandboxes", json!({}));
    assert_eq!(listed["sandboxes"][0]["id"], "container-1");
    assert_eq!(listed["sandboxes"][0]["daemon"]["host"], "127.0.0.1");
    assert_eq!(listed["sandboxes"][0]["daemon"]["port"], 7000);

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
    let created_calls = runtime.created.lock().expect("created lock");
    assert_eq!(created_calls.len(), 1);
    assert_eq!(created_calls[0].0, "ubuntu:24.04");
    assert_eq!(created_calls[0].1.as_path(), workspace.path());
    let shared_base = created_calls[0]
        .2
        .as_ref()
        .expect("manager create_sandbox always passes shared base");
    assert_eq!(shared_base.target, PathBuf::from("/eos/layer-stack/base"));
    assert!(shared_base.readonly);
    assert!(shared_base.source.starts_with(
        workspace
            .path()
            .parent()
            .expect("workspace has parent")
            .join("eos-shared-workspace-base-cache")
    ));
    assert!(shared_base.source.ends_with("base"));
    assert!(!shared_base.root_hash.is_empty());
    assert_eq!(
        runtime.destroyed.lock().expect("destroyed lock").as_slice(),
        ["container-1"]
    );
    assert_eq!(
        installer.started.lock().expect("started lock").as_slice(),
        ["container-1"]
    );
    assert_eq!(
        installer.checked.lock().expect("checked lock").as_slice(),
        ["container-1"],
        "readiness check must receive the sandbox record"
    );
    assert_eq!(
        installer.stopped.lock().expect("stopped lock").as_slice(),
        ["container-1"]
    );
}

#[test]
fn create_sandbox_rolls_back_runtime_and_store_when_install_fails() {
    let installer = Arc::new(FakeInstaller::failing_install());
    let (services, runtime, store) = services_with_installer(installer);
    let workspace = TestWorkspace::new("install-fails");

    let response = dispatch(
        &services,
        "create_sandbox",
        json!({"image": "ubuntu:24.04", "workspace_root": workspace.path_string()}),
    );

    assert_eq!(
        response["error"]["kind"],
        sandbox_protocol::error_kind::INTERNAL_ERROR
    );
    assert!(response["error"]["message"]
        .as_str()
        .expect("message")
        .contains("install stage failed"));
    assert_eq!(
        runtime.destroyed.lock().expect("destroyed lock").as_slice(),
        ["container-1"],
        "a failed install must destroy the runtime sandbox exactly once"
    );
    assert!(
        store.list().expect("list").is_empty(),
        "a failed install must leave no store record"
    );
}

#[test]
fn create_sandbox_rolls_back_runtime_and_store_when_start_fails() {
    let installer = Arc::new(FakeInstaller::failing_start());
    let (services, runtime, store) = services_with_installer(installer);
    let workspace = TestWorkspace::new("start-fails");

    let response = dispatch(
        &services,
        "create_sandbox",
        json!({"image": "ubuntu:24.04", "workspace_root": workspace.path_string()}),
    );

    assert_eq!(
        response["error"]["kind"],
        sandbox_protocol::error_kind::INTERNAL_ERROR
    );
    assert!(response["error"]["message"]
        .as_str()
        .expect("message")
        .contains("start stage failed"));
    assert_eq!(
        runtime.destroyed.lock().expect("destroyed lock").as_slice(),
        ["container-1"]
    );
    assert!(store.list().expect("list").is_empty());
}

#[test]
fn create_sandbox_rolls_back_runtime_and_store_when_check_fails() {
    let installer = Arc::new(FakeInstaller::failing_check());
    let (services, runtime, store) = services_with_installer(installer.clone());
    let workspace = TestWorkspace::new("check-fails");

    let response = dispatch(
        &services,
        "create_sandbox",
        json!({"image": "ubuntu:24.04", "workspace_root": workspace.path_string()}),
    );

    assert_eq!(
        response["error"]["kind"],
        sandbox_protocol::error_kind::INTERNAL_ERROR
    );
    assert!(response["error"]["message"]
        .as_str()
        .expect("message")
        .contains("check stage failed"));
    assert_eq!(
        runtime.destroyed.lock().expect("destroyed lock").as_slice(),
        ["container-1"]
    );
    assert!(store.list().expect("list").is_empty());
    assert_eq!(
        installer.started.lock().expect("started lock").as_slice(),
        ["container-1"],
        "start ran before the failing readiness check"
    );
    assert_eq!(
        installer.checked.lock().expect("checked lock").as_slice(),
        ["container-1"],
        "readiness check ran against the started record before failing"
    );
    assert_eq!(
        installer.stopped.lock().expect("stopped lock").as_slice(),
        ["container-1"],
        "rollback must best-effort stop the daemon"
    );
}

#[test]
fn observability_snapshot_aggregates_ready_sandboxes_with_private_daemon_requests() {
    let client = Arc::new(RecordingSnapshotClient::default());
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

    let response = dispatch(&services, "snapshot", json!({}));

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
        .all(|invocation| invocation.op == "get_observability"));
    assert!(invocations.iter().all(|invocation| {
        matches!(
            &invocation.scope,
            CliOperationScope::Sandbox { sandbox_id }
                if sandbox_id == "sbox-1" || sandbox_id == "sbox-2"
        )
    }));
    assert!(invocations
        .iter()
        .all(|invocation| invocation.args == json!({ "view": "snapshot" })));
    assert!(invocations
        .iter()
        .all(|invocation| invocation.timeout == Duration::from_millis(1_500)));
}

#[test]
fn observability_snapshot_converts_one_daemon_failure_to_one_unavailable_node() {
    let client = Arc::new(RecordingSnapshotClient::default());
    client.fail_sandbox("sbox-2");
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

    let response = dispatch(&services, "snapshot", json!({}));

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
    assert!(client
        .invocations()
        .iter()
        .all(|invocation| invocation.args == json!({ "view": "snapshot" })));
}

#[test]
fn observability_snapshot_errors_for_explicit_unknown_sandbox_id() {
    let client = Arc::new(RecordingSnapshotClient::default());
    let (services, _store) = services_with_client(client);

    let response = dispatch(&services, "snapshot", json!({"sandbox_id": "missing"}));

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
fn observability_snapshot_returns_unavailable_node_for_explicit_non_ready_sandbox() {
    let client = Arc::new(RecordingSnapshotClient::default());
    let (services, store) = services_with_client(client.clone());
    store
        .insert(sandbox_record(
            "creating",
            SandboxState::Creating,
            Some(endpoint("creating")),
        ))
        .expect("insert non-ready sandbox");

    let response = dispatch(&services, "snapshot", json!({"sandbox_id": "creating"}));

    let sandboxes = response["sandboxes"].as_array().expect("sandboxes array");
    assert_eq!(sandboxes.len(), 1);
    assert_eq!(sandboxes[0]["sandbox_id"], "creating");
    assert_eq!(sandboxes[0]["lifecycle_state"], "creating");
    assert_eq!(sandboxes[0]["availability"], "unavailable");
    assert!(client.invocations().is_empty());
}

#[test]
fn observability_snapshot_bounds_daemon_fanout_concurrency() {
    let client = Arc::new(SlowSnapshotClient::new());
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

    let response = dispatch(&services, "snapshot", json!({}));

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
        runtime_root.clone(),
    );
    let record = SandboxRecord::new(id("container-1"), workspace_root, SandboxState::Ready);
    let (socket_path, pid_path) = daemon_file_paths(&runtime_root, "container-1");
    std::fs::create_dir_all(pid_path.parent().expect("pid path parent"))?;
    std::fs::write(&socket_path, b"socket placeholder")?;

    let child = Command::new("/bin/sleep").arg("30").spawn()?;
    let pid = child.id();
    let _cleanup = ChildCleanup::new(child);
    std::fs::write(&pid_path, pid.to_string())?;

    installer.stop_daemon(&record)?;

    assert!(
        !pid_exists(pid),
        "daemon pid {pid} should be gone after stop_daemon"
    );
    assert!(!pid_path.exists());
    assert!(!socket_path.exists());

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
        runtime_root.clone(),
    );
    let record = SandboxRecord::new(id("container-1"), workspace_root, SandboxState::Ready);
    let (socket_path, _pid_path) = daemon_file_paths(&runtime_root, "container-1");
    std::fs::create_dir_all(socket_path.parent().expect("socket path parent"))?;
    std::fs::write(&socket_path, b"socket placeholder")?;

    let error = installer
        .stop_daemon(&record)
        .expect_err("socket without pid is not silently cleaned up");

    assert!(
        matches!(error, ManagerError::DaemonInstallFailed { .. }),
        "unexpected error: {error}"
    );
    assert!(
        socket_path.exists(),
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

// Test 18: checkpoint_squash lives under the existing "management" family,
// takes only --sandbox-id, and forwards ONE sandbox-scoped
// squash_layerstack request through the generic router path — no bespoke
// client sequence, no "checkpoint" family, no manager-local lifecycle work.
#[test]
fn checkpoint_squash_manager_cli_forwards_to_runtime() {
    let catalog = sandbox_manager::cli_operation_catalog();
    let encoded = sandbox_protocol::catalog_to_value(catalog).to_string();
    let catalog: Value = serde_json::from_str(&encoded).expect("catalog json");
    let spec = catalog["operations"]
        .as_array()
        .expect("operations array")
        .iter()
        .find(|op| op["name"] == "checkpoint_squash")
        .expect("checkpoint_squash present in the manager catalog");
    assert_eq!(spec["family"], "management");
    assert!(
        !encoded.contains("\"checkpoint\""),
        "no one-member checkpoint family creeps in"
    );

    let client = Arc::new(RecordingSnapshotClient::default());
    let (services, store) = services_with_client(client.clone());
    store
        .insert(sandbox_record(
            "sbox-1",
            SandboxState::Ready,
            Some(endpoint("sbox-1")),
        ))
        .expect("insert ready sandbox");

    let response = dispatch(
        &services,
        "checkpoint_squash",
        json!({ "sandbox_id": "sbox-1" }),
    );
    assert!(
        response.get("error").is_none(),
        "forwarded response passes through: {response}"
    );

    let invocations = client.invocations();
    assert_eq!(invocations.len(), 1, "exactly one wire round trip");
    assert_eq!(
        invocations[0].op, "squash_layerstack",
        "renamed to the daemon op"
    );
    assert!(
        matches!(&invocations[0].scope, CliOperationScope::Sandbox { sandbox_id } if sandbox_id == "sbox-1"),
        "rebuilt as a sandbox-scoped runtime request"
    );
    assert_eq!(invocations[0].args, json!({}), "squash takes no options");
}

#[test]
fn checkpoint_squash_reports_stale_daemon_unknown_op() {
    struct UnknownOpClient;

    impl SandboxDaemonClient for UnknownOpClient {
        fn invoke_with_timeout(
            &self,
            _endpoint: &SandboxDaemonEndpoint,
            _request: Request,
            _timeout: Duration,
        ) -> Result<Response, ManagerError> {
            Ok(Response::unknown_op())
        }
    }

    let (services, store) = services_with_client(Arc::new(UnknownOpClient));
    store
        .insert(sandbox_record(
            "sbox-1",
            SandboxState::Ready,
            Some(endpoint("sbox-1")),
        ))
        .expect("insert ready sandbox");

    let response = dispatch(
        &services,
        "checkpoint_squash",
        json!({ "sandbox_id": "sbox-1" }),
    );

    assert_eq!(response["error"]["kind"], "operation_failed");
    assert!(response["error"]["message"]
        .as_str()
        .expect("message string")
        .contains("current daemon binary"));
    assert_eq!(
        response["error"]["details"]["daemon_op"],
        "squash_layerstack"
    );
}

#[test]
fn checkpoint_squash_requires_sandbox_id_and_a_ready_sandbox() {
    let client = Arc::new(RecordingSnapshotClient::default());
    let (services, store) = services_with_client(client.clone());

    let missing = dispatch(&services, "checkpoint_squash", json!({}));
    assert_eq!(missing["error"]["kind"], "invalid_request");
    assert!(
        client.invocations().is_empty(),
        "no forward without a sandbox id"
    );

    let unknown = dispatch(
        &services,
        "checkpoint_squash",
        json!({ "sandbox_id": "nope" }),
    );
    assert!(
        unknown.get("error").is_some(),
        "unknown sandbox is an error"
    );

    store
        .insert(sandbox_record(
            "creating",
            SandboxState::Creating,
            Some(endpoint("creating")),
        ))
        .expect("insert non-ready sandbox");
    let not_ready = dispatch(
        &services,
        "checkpoint_squash",
        json!({ "sandbox_id": "creating" }),
    );
    assert!(
        not_ready.get("error").is_some(),
        "non-ready sandbox is an error"
    );
    assert!(
        client.invocations().is_empty(),
        "the generic forward's Ready check runs before any wire call"
    );
}
