use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, ManagerServices, SandboxDaemonClient,
    SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxId, SandboxRecord, SandboxRuntime,
    SandboxState, SandboxStore,
};
use sandbox_protocol::{
    ArgKind, CliOperationSpec, OperationCatalog, OperationExecutionSpace, OperationFamilySpec,
    OperationScope, Request, Response,
};
use serde_json::{json, Value};

static TEST_RUNTIME_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "test",
    title: "Test",
    summary: "Test runtime operations.",
    description: "Test runtime operations.",
};

static TEST_RUNTIME_SPEC: CliOperationSpec = CliOperationSpec {
    name: "runtime_test_operation",
    family: "test",
    summary: "Test runtime operation.",
    description: "Test runtime operation.",
    args: &[],
    cli: None,
    related: &[],
};

static TEST_RUNTIME_FAMILIES: &[&OperationFamilySpec] = &[&TEST_RUNTIME_FAMILY];
static TEST_RUNTIME_SPECS: &[&CliOperationSpec] = &[&TEST_RUNTIME_SPEC];

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
}

#[derive(Default)]
struct FakeClient {
    described: Mutex<Vec<PathBuf>>,
}

impl SandboxDaemonClient for FakeClient {
    fn describe_operations(
        &self,
        endpoint: &SandboxDaemonEndpoint,
    ) -> Result<OperationCatalog, ManagerError> {
        self.described
            .lock()
            .expect("described lock")
            .push(endpoint.socket_path.clone());
        Ok(OperationCatalog::new(
            OperationExecutionSpace::Runtime,
            TEST_RUNTIME_FAMILIES,
            TEST_RUNTIME_SPECS,
        ))
    }

    fn invoke(
        &self,
        _endpoint: &SandboxDaemonEndpoint,
        _request: sandbox_protocol::Request,
    ) -> Result<Response, ManagerError> {
        Ok(Response::ok(json!({"forwarded": true})))
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
    let client = Arc::new(FakeClient::default());
    let services = ManagerServices::new(
        Arc::clone(&store),
        runtime.clone(),
        installer.clone(),
        client.clone(),
    );
    (services, runtime, installer, client)
}

fn dispatch(services: &ManagerServices, op: &str, args: Value) -> Value {
    let request = Request::new(op, "req-1", OperationScope::System, args);
    sandbox_manager::dispatch_operation(services, &request).into_json_value()
}

fn id(value: &str) -> SandboxId {
    SandboxId::new(value).expect("valid sandbox id")
}

#[test]
fn operation_catalog_contains_only_manager_operations() {
    let catalog = sandbox_manager::operation_catalog();
    let names = catalog
        .operations
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    assert_eq!(
        catalog.operation_execution_space,
        OperationExecutionSpace::Manager
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
