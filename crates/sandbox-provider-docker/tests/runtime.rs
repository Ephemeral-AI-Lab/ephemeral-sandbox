use std::convert::Infallible;
use std::io::Cursor;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;

use sandbox_config::configs::manager::{DockerCgroupNamespaceMode, DockerPersistentVolumeMount};
use sandbox_manager::{
    CreateSandboxRequest, ManagerError, SandboxRecord, SandboxRuntime, SandboxState,
    SharedBaseMount,
};
use sandbox_provider_docker::{DockerRuntimeConfig, DockerSandboxRuntime};

#[test]
fn container_limits_come_from_the_selected_named_profile() {
    let config = DockerRuntimeConfig {
        resource_profile: "build-heavy".to_owned(),
        ..DockerRuntimeConfig::default()
    };
    let runtime = DockerSandboxRuntime::new(config);

    let limits = runtime
        .configured_resource_limits()
        .expect("selected profile is available");
    assert_eq!(limits.profile_name, "build-heavy");
    assert_eq!(limits.nano_cpus, 4_000_000_000);
    assert_eq!(limits.memory_high_bytes, 3 * 1024 * 1024 * 1024);
    assert_eq!(limits.memory_max_bytes, 4 * 1024 * 1024 * 1024);
    assert_eq!(limits.pids_max, 1024);
    assert_eq!(limits.workload_memory_high_bytes, 3 * 1024 * 1024 * 1024);
    assert_eq!(limits.workload_memory_max_bytes, 3 * 1024 * 1024 * 1024);
    assert_eq!(limits.workload_pids_max, 960);
    assert_eq!(limits.control_plane_pids_reserve, 64);
    assert!(limits.separate_workload_cgroup);
}

#[test]
fn container_limits_preserve_an_explicit_smaller_workload_memory_envelope() {
    let mut config = DockerRuntimeConfig {
        resource_profile: "build-heavy".to_owned(),
        ..DockerRuntimeConfig::default()
    };
    let profile = config
        .resource_profiles
        .get_mut("build-heavy")
        .expect("build-heavy profile");
    profile.workload_memory_high_bytes = Some(96 * 1024 * 1024);
    profile.workload_memory_max_bytes = Some(128 * 1024 * 1024);
    let runtime = DockerSandboxRuntime::new(config);

    let limits = runtime
        .configured_resource_limits()
        .expect("selected profile is available");
    assert_eq!(limits.memory_high_bytes, 3 * 1024 * 1024 * 1024);
    assert_eq!(limits.memory_max_bytes, 4 * 1024 * 1024 * 1024);
    assert_eq!(limits.workload_memory_high_bytes, 96 * 1024 * 1024);
    assert_eq!(limits.workload_memory_max_bytes, 128 * 1024 * 1024);
}

#[test]
fn container_creation_persists_the_exact_resolved_resource_profile() {
    let docker = FakeDockerApi::new(|request| {
        if request.method == "GET" && request.target.contains("/volumes/") {
            return FakeResponse::json(
                200,
                serde_json::json!({
                    "CreatedAt": "",
                    "Driver": "local",
                    "Labels": {},
                    "Mountpoint": "/mock",
                    "Name": "existing-shared-base",
                    "Options": {},
                    "Scope": "local"
                }),
            );
        }
        if request.method == "POST" && request.target.contains("/volumes/create") {
            return FakeResponse::json(
                201,
                serde_json::json!({
                    "CreatedAt": "",
                    "Driver": "local",
                    "Labels": {},
                    "Mountpoint": "/mock",
                    "Name": "runtime-volume",
                    "Options": {},
                    "Scope": "local"
                }),
            );
        }
        if request.method == "POST" && request.target.contains("/containers/create") {
            return FakeResponse::json(
                201,
                serde_json::json!({"Id": "mock-container", "Warnings": []}),
            );
        }
        if request.method == "PUT" && request.target.contains("/archive") {
            return FakeResponse::empty(200);
        }
        FakeResponse::json(
            404,
            serde_json::json!({"message": format!("unexpected request: {} {}", request.method, request.target)}),
        )
    });
    let root = std::env::temp_dir().join(format!("eos-profile-labels-{}", unique_test_suffix()));
    let workspace = root.join("workspace");
    let shared_base = root.join("base");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(&shared_base).expect("create shared base");

    let config = DockerRuntimeConfig {
        docker_endpoint: Some(docker.endpoint()),
        daemon_config_yaml_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/prd.yml"),
        resource_profile: "build-heavy".to_owned(),
        cgroup_namespace_mode: DockerCgroupNamespaceMode::Host,
        persistent_volume_mounts: vec![DockerPersistentVolumeMount {
            name: "eos-mpla-prepared-s4-chain-v1".to_owned(),
            target: PathBuf::from("/eos/mpla-fixtures"),
            readonly: true,
        }],
        ..DockerRuntimeConfig::default()
    };
    let runtime = DockerSandboxRuntime::new(config);
    let result = runtime
        .create_sandbox(&CreateSandboxRequest {
            image: "ubuntu:24.04".to_owned(),
            workspace_root: workspace,
            shared_base: Some(SharedBaseMount {
                source: shared_base,
                target: PathBuf::from("/eos/base"),
                root_hash: unique_test_suffix(),
                readonly: true,
            }),
        })
        .expect("fake Docker accepts container creation");

    let profile = result.resource_profile.expect("selected profile returned");
    assert_eq!(profile.name, "build-heavy");
    assert_eq!(profile.workload_memory_max_bytes, 3 * 1024 * 1024 * 1024);
    assert_eq!(profile.workload_pids_max, 960);

    let requests = docker.requests();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[0].method, "GET");
    assert!(requests[0].target.contains("/volumes/"));
    assert!(requests[1..4]
        .iter()
        .all(|request| { request.method == "POST" && request.target.contains("/volumes/create") }));
    assert_eq!(requests[4].method, "POST");
    assert!(requests[4].target.contains("/containers/create"));
    assert_eq!(requests[5].method, "PUT");
    assert!(requests[5].target.contains("/archive"));

    let request = requests
        .into_iter()
        .find(|request| request.method == "POST" && request.target.contains("/containers/create"))
        .expect("container create request captured");
    let document: serde_json::Value =
        serde_json::from_slice(&request.body).expect("container create JSON");
    assert_eq!(document["Labels"]["eos.resource_profile"], "build-heavy");
    assert_eq!(
        document["Labels"]["eos.resource.workload_memory_max_bytes"],
        "3221225472"
    );
    assert_eq!(document["Labels"]["eos.resource.workload_pids_max"], "960");
    assert_eq!(
        document["HostConfig"]["MemoryReservation"],
        3_221_225_472_i64
    );
    assert_eq!(document["HostConfig"]["Memory"], 4_294_967_296_i64);
    assert_eq!(document["HostConfig"]["NanoCpus"], 4_000_000_000_i64);
    assert_eq!(document["HostConfig"]["PidsLimit"], 1024);
    assert_eq!(document["HostConfig"]["CgroupnsMode"], "host");
    assert!(document["HostConfig"]["Binds"]
        .as_array()
        .expect("Docker binds array")
        .iter()
        .any(|value| value == "eos-mpla-prepared-s4-chain-v1:/eos/mpla-fixtures:ro"));

    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn fixture_builder_materializes_shared_base_into_writable_persistent_volume() {
    let docker = FakeDockerApi::new(|request| {
        if request.method == "POST" && request.target.contains("/volumes/create") {
            return FakeResponse::json(
                201,
                serde_json::json!({
                    "CreatedAt": "",
                    "Driver": "local",
                    "Labels": {},
                    "Mountpoint": "/mock",
                    "Name": "fixture-volume",
                    "Options": {},
                    "Scope": "local"
                }),
            );
        }
        if request.method == "POST" && request.target.contains("/containers/create") {
            return FakeResponse::json(
                201,
                serde_json::json!({"Id": "mock-container", "Warnings": []}),
            );
        }
        if request.method == "PUT" && request.target.contains("/archive") {
            return FakeResponse::empty(200);
        }
        FakeResponse::json(
            404,
            serde_json::json!({"message": format!("unexpected request: {} {}", request.method, request.target)}),
        )
    });
    let root =
        std::env::temp_dir().join(format!("eos-materialized-volume-{}", unique_test_suffix()));
    let workspace = root.join("workspace");
    let shared_base = root.join("base");
    let tool = shared_base
        .join("B000001-base")
        .join("_campaign-tools")
        .join("mpla-speed-poc-v1");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(tool.parent().expect("tool parent")).expect("create shared base");
    std::fs::write(&tool, b"static-arm64-tool").expect("write staged tool");

    let config = DockerRuntimeConfig {
        docker_endpoint: Some(docker.endpoint()),
        daemon_config_yaml_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/mpla-poc-m3-fixture-builder-phase-profile-v2.yml"),
        resource_profile: "build-heavy".to_owned(),
        cgroup_namespace_mode: DockerCgroupNamespaceMode::Host,
        sandbox_owned_workspace_volumes: false,
        materialize_shared_base_into_persistent_volume: true,
        persistent_volume_mounts: vec![DockerPersistentVolumeMount {
            name: "eos-mpla-prepared-s4-phase-profile-v2".to_owned(),
            target: PathBuf::from("/eos/mpla-fixtures"),
            readonly: false,
        }],
        ..DockerRuntimeConfig::default()
    };
    DockerSandboxRuntime::new(config)
        .create_sandbox(&CreateSandboxRequest {
            image: "ubuntu:24.04".to_owned(),
            workspace_root: workspace,
            shared_base: Some(SharedBaseMount {
                source: shared_base,
                target: PathBuf::from("/eos/mpla-fixtures/s4-chain-v2/layer-stack/base"),
                root_hash: unique_test_suffix(),
                readonly: true,
            }),
        })
        .expect("fake Docker accepts materialized fixture builder");

    let requests = docker.requests();
    assert!(requests
        .iter()
        .all(|request| !(request.method == "GET" && request.target.contains("/volumes/"))));
    let create = requests
        .iter()
        .find(|request| request.method == "POST" && request.target.contains("/containers/create"))
        .expect("container create request");
    let document: serde_json::Value =
        serde_json::from_slice(&create.body).expect("container create JSON");
    let binds = document["HostConfig"]["Binds"]
        .as_array()
        .expect("Docker binds array");
    assert!(binds
        .iter()
        .any(|value| value == "eos-mpla-prepared-s4-phase-profile-v2:/eos/mpla-fixtures"));
    assert!(binds.iter().all(|value| !value
        .as_str()
        .unwrap_or_default()
        .starts_with("eos-shared-base-")));

    let archive = requests
        .iter()
        .find(|request| request.method == "PUT" && request.target.contains("/archive"))
        .expect("seed archive request");
    let archive_paths = tar::Archive::new(Cursor::new(archive.body.clone()))
        .entries()
        .expect("read archive")
        .map(|entry| {
            entry
                .expect("read archive entry")
                .path()
                .expect("archive entry path")
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    assert!(archive_paths.iter().any(|path| {
        path == "eos/mpla-fixtures/s4-chain-v2/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1"
    }));
    assert!(archive_paths
        .iter()
        .any(|path| path == "eos/mpla-fixtures/s4-chain-v2/layer-stack/manifest.json"));

    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn recovery_reads_the_persisted_profile_instead_of_current_config() {
    let response = serde_json::json!([{
        "Labels": {
            "eos.sandbox_id": "eos-recovered",
            "eos.host_workspace_root": "/host/workspace",
            "eos.auth_token": "token",
            "eos.resource_profile": "small-test",
            "eos.resource.nano_cpus": "500000000",
            "eos.resource.memory_high_bytes": "67108864",
            "eos.resource.memory_max_bytes": "100663296",
            "eos.resource.pids_max": "64",
            "eos.resource.workload_memory_high_bytes": "67108864",
            "eos.resource.workload_memory_max_bytes": "67108864",
            "eos.resource.workload_pids_max": "48",
            "eos.resource.control_plane_pids_reserve": "16",
            "eos.resource.daemon_runtime_profile": "standard",
            "eos.resource.separate_workload_cgroup": "true"
        },
        "Ports": [
            {"PrivatePort": 7000, "PublicPort": 17000, "Type": "tcp"},
            {"PrivatePort": 7001, "PublicPort": 17001, "Type": "tcp"}
        ]
    }]);
    let docker = FakeDockerApi::new(move |request| {
        if request.method == "GET" && request.target.contains("/containers/json") {
            FakeResponse::json(200, response.clone())
        } else {
            FakeResponse::json(
                404,
                serde_json::json!({"message": format!("unexpected request: {} {}", request.method, request.target)}),
            )
        }
    });
    let config = DockerRuntimeConfig {
        docker_endpoint: Some(docker.endpoint()),
        daemon_port: 7000,
        daemon_http_port: 7001,
        ..DockerRuntimeConfig::default()
    };
    let runtime = DockerSandboxRuntime::new(config);

    let records = runtime.recover_sandboxes().expect("recover fake container");
    assert_eq!(records.len(), 1);
    let profile = records[0]
        .resource_profile
        .as_ref()
        .expect("persisted profile");
    assert_eq!(profile.name, "small-test");
    assert_eq!(profile.nano_cpus, 500_000_000);
    assert_eq!(profile.memory_high_bytes, 67_108_864);
    assert_eq!(profile.memory_max_bytes, 100_663_296);
    assert_eq!(profile.pids_max, 64);
    assert_eq!(profile.workload_memory_high_bytes, 67_108_864);
    assert_eq!(profile.workload_memory_max_bytes, 67_108_864);
    assert_eq!(profile.workload_pids_max, 48);
    assert_eq!(profile.control_plane_pids_reserve, 16);
    assert_eq!(profile.daemon_runtime_profile, "standard");
    assert!(profile.separate_workload_cgroup);
}

#[test]
fn create_sandbox_rejects_missing_shared_base_mount() {
    let runtime = DockerSandboxRuntime::new(DockerRuntimeConfig::default());
    let error = runtime
        .create_sandbox(&CreateSandboxRequest {
            image: "ubuntu:24.04".to_owned(),
            workspace_root: PathBuf::from("/workspace"),
            shared_base: None,
        })
        .expect_err("missing shared base rejected before docker");

    assert!(matches!(error, ManagerError::RuntimeFailed { .. }));
    assert!(error.to_string().contains("shared base mount is required"));
}

#[test]
fn create_sandbox_rejects_missing_shared_base_source() {
    let runtime = DockerSandboxRuntime::new(DockerRuntimeConfig::default());
    let missing_source =
        std::env::temp_dir().join(format!("eos-missing-shared-base-{}", unique_test_suffix()));
    let error = runtime
        .create_sandbox(&CreateSandboxRequest {
            image: "ubuntu:24.04".to_owned(),
            workspace_root: PathBuf::from("/workspace"),
            shared_base: Some(SharedBaseMount {
                source: missing_source,
                target: PathBuf::from("/eos/layer-stack/base"),
                root_hash: "root-hash".to_owned(),
                readonly: true,
            }),
        })
        .expect_err("missing shared base source rejected before docker");

    assert!(matches!(error, ManagerError::RuntimeFailed { .. }));
    assert!(error.to_string().contains("shared base source"));
}

#[test]
#[ignore = "requires a local Docker Engine and the ubuntu:24.04 image"]
fn create_sandbox_accepts_image_without_git() {
    let root = std::env::temp_dir().join(format!("eos-no-git-sandbox-{}", unique_test_suffix()));
    let workspace = root.join("workspace");
    let shared_base = root.join("base");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(&shared_base).expect("create shared base");

    let config = sandbox_config::configs::manager::DockerRuntimeConfig {
        daemon_config_yaml_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/prd.yml"),
        ..sandbox_config::configs::manager::DockerRuntimeConfig::default()
    };
    let runtime = DockerSandboxRuntime::new(config);
    let result = runtime
        .create_sandbox(&CreateSandboxRequest {
            image: "ubuntu:24.04".to_owned(),
            workspace_root: workspace.clone(),
            shared_base: Some(SharedBaseMount {
                source: shared_base,
                target: PathBuf::from("/eos/layer-stack/base"),
                root_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                    .to_owned(),
                readonly: true,
            }),
        })
        .expect("sandbox creation must not require git in the image");

    let record = SandboxRecord::new(result.id, workspace, SandboxState::Creating);
    runtime.destroy_sandbox(&record).expect("destroy sandbox");
    std::fs::remove_dir_all(root).expect("remove fixture");
}

fn unique_test_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    format!("{nanos}-{}", std::process::id())
}

#[derive(Clone, Debug)]
struct FakeRequest {
    method: String,
    target: String,
    body: Vec<u8>,
}

struct FakeResponse {
    status: u16,
    body: Vec<u8>,
}

impl FakeResponse {
    fn json(status: u16, value: serde_json::Value) -> Self {
        Self {
            status,
            body: serde_json::to_vec(&value).expect("serialize fake Docker response"),
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
        }
    }
}

struct FakeDockerApi {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<FakeRequest>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeDockerApi {
    fn new<F>(handler: F) -> Self
    where
        F: Fn(&FakeRequest) -> FakeResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fake Docker API");
        listener
            .set_nonblocking(true)
            .expect("configure fake Docker listener");
        let address = listener.local_addr().expect("fake Docker address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stopped = Arc::clone(&stopped);
        let handler = Arc::new(handler);
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build fake Docker runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("adopt fake Docker listener");
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    let (stream, _) = listener.accept().await.expect("accept fake Docker request");
                    if worker_stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let requests = Arc::clone(&worker_requests);
                    let handler = Arc::clone(&handler);
                    connections.spawn(async move {
                        let service = service_fn(move |request: Request<Incoming>| {
                            serve_fake_request(request, Arc::clone(&requests), Arc::clone(&handler))
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                    });
                }
                connections.abort_all();
                while connections.join_next().await.is_some() {}
            });
        });
        Self {
            address,
            requests,
            stopped,
            worker: Some(worker),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<FakeRequest> {
        self.requests
            .lock()
            .expect("read captured Docker requests")
            .clone()
    }
}

impl Drop for FakeDockerApi {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join fake Docker API");
        }
    }
}

async fn serve_fake_request<F>(
    request: Request<Incoming>,
    requests: Arc<Mutex<Vec<FakeRequest>>>,
    handler: Arc<F>,
) -> Result<Response<Full<Bytes>>, Infallible>
where
    F: Fn(&FakeRequest) -> FakeResponse + Send + Sync + 'static,
{
    let (parts, body) = request.into_parts();
    let body = body
        .collect()
        .await
        .expect("read fake Docker request body")
        .to_bytes()
        .to_vec();
    let request = FakeRequest {
        method: parts.method.to_string(),
        target: parts.uri.to_string(),
        body,
    };
    requests
        .lock()
        .expect("capture fake Docker request")
        .push(request.clone());
    let response = handler(&request);
    Ok(Response::builder()
        .status(response.status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(response.body)))
        .expect("build fake Docker response"))
}
