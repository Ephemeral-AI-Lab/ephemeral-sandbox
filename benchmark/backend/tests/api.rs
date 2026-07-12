use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, ORIGIN};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use sandbox_benchmark::api::{handle, ResponseBody};
use sandbox_benchmark::app::{AppState, ExecutionDependencies};
use sandbox_benchmark::artifacts::{ArtifactId, ArtifactStore};
use sandbox_benchmark::checks::CorrectnessFold;
use sandbox_benchmark::config::{ResolveInput, StartupConfig};
use sandbox_benchmark::definitions::{catalog, definition};
use sandbox_benchmark::events::{EventData, EventJournal, RunState};
use sandbox_benchmark::model::{ConfigurationScope, OperationId, OperationPlan};
use sandbox_benchmark::plan::{
    load_plan, slice_default, validate_and_expand, ExpandedPlan, EXPANDED_PLAN_SCHEMA_VERSION,
};
use sandbox_benchmark::report::{
    PRIMARY_LATENCY_METRIC_ID, REPORT_SCHEMA_NAME, REPORT_SCHEMA_VERSION,
};
use sandbox_benchmark::scheduler::{
    EnvironmentMetadata, HostEnvironment, LifecycleDurations, ObservationRecord, PhaseObservation,
    PhaseStatus, RequestObservation, RunArtifacts, RunManifest, SequencedObservation,
    TreatmentIdentity, TrialKind, TrialSample, DEFINITION_SNAPSHOT_SCHEMA_NAME,
    ENVIRONMENT_METADATA_SCHEMA_NAME, EXPANDED_PLAN_SCHEMA_NAME, INTENT_PLAN_SCHEMA_NAME,
    OBSERVATION_SCHEMA_NAME, OBSERVATION_SCHEMA_VERSION, RUN_MANIFEST_SCHEMA_NAME,
    RUN_MANIFEST_SCHEMA_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod support;

use support::TestRoot;

const AUTHORITY: &str = "127.0.0.1:41827";
const NONCE_HEADER: &str = "x-eos-benchmark-nonce";

struct ApiFixture {
    _root: TestRoot,
    state: Arc<AppState>,
    web_root: PathBuf,
}

impl ApiFixture {
    fn new(label: &str, execution_ready: bool) -> Self {
        let root = TestRoot::new(label);
        let web_root = root.join("web");
        fs::create_dir_all(web_root.join("assets")).expect("create production asset directory");
        fs::write(
            web_root.join("index.html"),
            concat!(
                "<!doctype html><html><head>",
                "<meta name=\"eos-benchmark-nonce\" content=\"\" />",
                "</head><body><div id=\"root\"></div></body></html>"
            ),
        )
        .expect("write production index");
        fs::write(web_root.join("assets/app.css"), "body { color: #123; }")
            .expect("write production asset");

        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("canonical repository");
        let bind: SocketAddr = AUTHORITY.parse().expect("valid loopback bind");
        let (config, source) = StartupConfig::resolve(ResolveInput {
            repo,
            bind,
            web_root: Some(web_root.clone()),
            workspace_override: Some(root.join("workspace")),
        })
        .expect("resolve isolated API fixture");
        let state = AppState::new(config, source, AUTHORITY.to_owned(), execution_ready)
            .expect("create API state");
        Self {
            _root: root,
            state,
            web_root,
        }
    }

    fn store(&self) -> ArtifactStore {
        self.state.artifacts().expect("read artifact store")
    }

    fn config(&self) -> StartupConfig {
        self.state.config().expect("read startup configuration")
    }

    #[cfg(unix)]
    fn install_fake_execution_dependencies(&self) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let binaries = self._root.join("fake-execution-bin");
        fs::create_dir_all(&binaries).expect("create fake execution binary directory");
        let git = binaries.join("git");
        fs::write(
            &git,
            concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = \"rev-parse\" ]; then\n",
                "  printf '%s\\n' '0123456789abcdef0123456789abcdef01234567'\n",
                "fi\n",
            ),
        )
        .expect("write fake git");
        let docker = binaries.join("docker");
        fs::write(
            &docker,
            concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n",
                "  printf '%s\\n' 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'\n",
                "fi\n",
            ),
        )
        .expect("write fake docker");
        let gateway = binaries.join("sandbox-gateway");
        fs::write(&gateway, "#!/bin/sh\nexit 1\n").expect("write fake gateway");
        let daemon = binaries.join("sandbox-daemon");
        fs::write(&daemon, "#!/bin/sh\nexit 1\n").expect("write fake daemon");
        let stat = binaries.join("stat");
        fs::write(&stat, "#!/bin/sh\nprintf '%s\\n' 'apfs'\n").expect("write fake stat");
        let df = binaries.join("df");
        fs::write(
            &df,
            concat!(
                "#!/bin/sh\n",
                "printf '%s\\n' 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n",
                "printf '%s\\n' 'fake 2000000 1000000 1000000 50% /'\n",
            ),
        )
        .expect("write fake df");
        for binary in [&git, &docker, &gateway, &daemon, &stat, &df] {
            fs::set_permissions(binary, fs::Permissions::from_mode(0o700))
                .expect("make fake execution binary executable");
        }
        self.state
            .install_execution_dependencies(ExecutionDependencies {
                gateway_binary: gateway,
                daemon_binary: daemon,
                docker_binary: docker,
                git_binary: git,
                stat_binary: stat,
                df_binary: df.clone(),
                docker_engine_version: "27.4.1-fake".to_owned(),
            })
            .expect("install fake execution dependencies");
        df
    }
}

fn get(state: &AppState, uri: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(HOST, state.authority())
        .body(Full::new(Bytes::new()))
        .expect("build GET request")
}

fn mutation(state: &AppState, method: Method, uri: &str, body: Vec<u8>) -> Request<Full<Bytes>> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(HOST, state.authority())
        .header(ORIGIN, state.origin())
        .header(NONCE_HEADER, state.nonce())
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("build mutation request")
}

fn json_mutation(
    state: &AppState,
    method: Method,
    uri: &str,
    value: &Value,
) -> Request<Full<Bytes>> {
    mutation(
        state,
        method,
        uri,
        serde_json::to_vec(value).expect("serialize request JSON"),
    )
}

async fn body_bytes(response: Response<ResponseBody>) -> Bytes {
    response
        .into_body()
        .collect()
        .await
        .expect("response body is infallible")
        .to_bytes()
}

async fn body_json(response: Response<ResponseBody>) -> Value {
    serde_json::from_slice(&body_bytes(response).await).expect("response body is JSON")
}

async fn assert_error(response: Response<ResponseBody>, status: StatusCode, code: &str) -> Value {
    assert_eq!(response.status(), status);
    assert!(!response
        .headers()
        .contains_key("access-control-allow-origin"));
    let value = body_json(response).await;
    assert_eq!(value["error"]["code"], code);
    assert!(value["error"]["message"]
        .as_str()
        .is_some_and(|message| !message.is_empty()));
    assert!(value["error"]["request_id"]
        .as_str()
        .is_some_and(|request_id| !request_id.is_empty()));
    value
}

async fn validated_command_plan(fixture: &ApiFixture) -> (Value, String) {
    let definitions = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/api/v1/definitions"),
    )
    .await;
    assert_eq!(definitions.status(), StatusCode::OK);
    let definitions = body_json(definitions).await;
    let defaults = definitions["defaults"]
        .as_array()
        .expect("definitions include defaults");
    let plan = defaults
        .iter()
        .find(|plan| plan["configuration_base"]["scope"] == "command")
        .expect("definitions include command default")
        .clone();
    let request = json!({"plan": plan, "starting_preset": null});
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/plans/validate",
            &request,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let expanded = body_json(response).await;
    assert_eq!(expanded["runnable"], true);
    let hash = expanded["plan_hash"]
        .as_str()
        .expect("expanded plan has hash")
        .to_owned();
    (request["plan"].clone(), hash)
}

#[tokio::test]
async fn host_origin_nonce_and_cors_are_fail_closed() {
    let fixture = ApiFixture::new("api-security", false);

    let response = handle(
        Arc::clone(&fixture.state),
        Request::builder()
            .method(Method::GET)
            .uri("/api/v1/health")
            .body(Full::new(Bytes::new()))
            .expect("build missing-host request"),
    )
    .await;
    assert_error(response, StatusCode::BAD_REQUEST, "required_header_missing").await;

    let response = handle(
        Arc::clone(&fixture.state),
        Request::builder()
            .method(Method::GET)
            .uri("/api/v1/health")
            .header(HOST, "localhost:41827")
            .body(Full::new(Bytes::new()))
            .expect("build wrong-host request"),
    )
    .await;
    assert_error(response, StatusCode::FORBIDDEN, "host_mismatch").await;

    let response = handle(
        Arc::clone(&fixture.state),
        Request::builder()
            .method(Method::GET)
            .uri("/api/v1/health")
            .header(HOST, AUTHORITY)
            .header(HOST, AUTHORITY)
            .body(Full::new(Bytes::new()))
            .expect("build duplicate-host request"),
    )
    .await;
    assert_error(response, StatusCode::BAD_REQUEST, "duplicate_header").await;

    let response = handle(
        Arc::clone(&fixture.state),
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/compare")
            .header(HOST, AUTHORITY)
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from_static(b"{}")))
            .expect("build missing-origin request"),
    )
    .await;
    assert_error(response, StatusCode::BAD_REQUEST, "required_header_missing").await;

    let response = handle(
        Arc::clone(&fixture.state),
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/compare")
            .header(HOST, AUTHORITY)
            .header(ORIGIN, "http://localhost:41827")
            .header(NONCE_HEADER, fixture.state.nonce())
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from_static(b"{}")))
            .expect("build wrong-origin request"),
    )
    .await;
    assert_error(response, StatusCode::FORBIDDEN, "origin_mismatch").await;

    let response = handle(
        Arc::clone(&fixture.state),
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/compare")
            .header(HOST, AUTHORITY)
            .header(ORIGIN, fixture.state.origin())
            .header(NONCE_HEADER, "wrong-process-nonce")
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from_static(b"{}")))
            .expect("build wrong-nonce request"),
    )
    .await;
    assert_error(response, StatusCode::FORBIDDEN, "nonce_mismatch").await;

    let response = handle(
        Arc::clone(&fixture.state),
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/compare")
            .header(HOST, AUTHORITY)
            .header(ORIGIN, fixture.state.origin())
            .header(NONCE_HEADER, fixture.state.nonce())
            .header(CONTENT_TYPE, "text/plain")
            .body(Full::new(Bytes::from_static(b"{}")))
            .expect("build wrong-content-type request"),
    )
    .await;
    assert_error(
        response,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "json_content_type_required",
    )
    .await;

    let health = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/api/v1/health"),
    )
    .await;
    assert_eq!(health.status(), StatusCode::OK);
    assert!(!health.headers().contains_key("access-control-allow-origin"));
    let health = body_json(health).await;
    assert_eq!(health["status"], "degraded");
    assert_eq!(health["execution_ready"], false);

    let settings = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/api/v1/settings"),
    )
    .await;
    assert_eq!(settings.status(), StatusCode::OK);
    let settings = body_json(settings).await;
    assert_eq!(settings["schema_version"], 1);
    assert_eq!(settings["writable"], true);
    assert_eq!(settings["path_health"]["root_marker"], true);
    assert_eq!(settings["path_health"]["outside_repository"], true);
}

#[tokio::test]
async fn strict_json_canonical_hash_and_disabled_execution_contract() {
    let fixture = ApiFixture::new("api-plan", false);
    let (plan, plan_hash) = validated_command_plan(&fixture).await;

    let validation = json!({"plan": plan, "starting_preset": null});
    let first = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/plans/validate",
            &validation,
        ),
    )
    .await;
    let second = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/plans/validate",
            &validation,
        ),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(body_json(first).await["plan_hash"], plan_hash);
    assert_eq!(body_json(second).await["plan_hash"], plan_hash);

    let unknown = json!({
        "plan": validation["plan"],
        "starting_preset": null,
        "future_field": true
    });
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/plans/validate",
            &unknown,
        ),
    )
    .await;
    let error = assert_error(response, StatusCode::BAD_REQUEST, "invalid_json").await;
    assert!(error["error"]["details"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("unknown field")));

    let response = handle(
        Arc::clone(&fixture.state),
        mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/plans/validate",
            vec![b' '; 1024 * 1024 + 1],
        ),
    )
    .await;
    assert_error(
        response,
        StatusCode::PAYLOAD_TOO_LARGE,
        "request_body_too_large",
    )
    .await;

    let mismatched = json!({
        "plan": validation["plan"],
        "plan_hash": "sha256:not-the-reviewed-plan",
        "client_request_id": "api-contract-mismatch",
        "starting_preset": null
    });
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(&fixture.state, Method::POST, "/api/v1/runs", &mismatched),
    )
    .await;
    assert_error(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "execution_unready",
    )
    .await;

    let create = json!({
        "plan": validation["plan"],
        "plan_hash": plan_hash,
        "client_request_id": "api-contract-disabled-execution",
        "starting_preset": null
    });
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(&fixture.state, Method::POST, "/api/v1/runs", &create),
    )
    .await;
    assert_error(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "execution_unready",
    )
    .await;
    assert!(fixture
        .store()
        .list_run_ids()
        .expect("list runs after refused execution")
        .is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn reviewed_resolved_environment_creates_an_authoritative_run() {
    let fixture = ApiFixture::new("api-resolved-environment", false);
    let _df = fixture.install_fake_execution_dependencies();
    let definitions = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/api/v1/definitions"),
    )
    .await;
    assert_eq!(definitions.status(), StatusCode::OK);
    let definitions = body_json(definitions).await;
    let plan = definitions["defaults"]
        .as_array()
        .expect("definitions include defaults")
        .iter()
        .find(|plan| plan["configuration_base"]["scope"] == "command")
        .expect("definitions include command default")
        .clone();

    let validation = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/plans/validate",
            &json!({"plan": plan, "starting_preset": null}),
        ),
    )
    .await;
    assert_eq!(validation.status(), StatusCode::OK);
    let validation = body_json(validation).await;
    assert_eq!(validation["runnable"], true);
    let plan_hash = validation["plan_hash"]
        .as_str()
        .expect("resolved validation hash")
        .to_owned();
    assert_eq!(
        validation["effective_environment"]["image_digest"],
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert!(validation["effective_environment"]["filesystem"]
        .as_str()
        .is_some_and(|filesystem| !filesystem.is_empty()));
    assert_eq!(
        validation["effective_environment"]["free_space_bytes"].as_u64(),
        Some(1_024_000_000)
    );

    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/runs",
            &json!({
                "plan": validation["canonical_plan"],
                "plan_hash": plan_hash,
                "client_request_id": "api-resolved-environment-start",
                "starting_preset": null
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let response = body_json(response).await;
    let run_id = response["run_id"].as_str().expect("created run id");
    let store = fixture.store();
    let expanded: ExpandedPlan = store
        .read_envelope(
            run_id,
            ArtifactId::ExpandedPlan,
            EXPANDED_PLAN_SCHEMA_NAME,
            EXPANDED_PLAN_SCHEMA_VERSION,
        )
        .expect("read persisted expanded plan");
    let manifest: RunManifest = store
        .read_envelope(
            run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .expect("read authoritative run manifest");
    assert_eq!(expanded.plan_hash, plan_hash);
    assert_eq!(
        expanded.effective_environment.free_space_bytes,
        Some(1_024_000_000)
    );
    assert_eq!(
        manifest.environment.image_digest,
        expanded.effective_environment.image_digest
    );
    assert_eq!(
        manifest.environment.host.filesystem,
        expanded.effective_environment.filesystem
    );
    assert_eq!(
        manifest.environment.host.free_space_bytes,
        expanded.effective_environment.free_space_bytes
    );
    assert_eq!(
        manifest.environment.workspace_root_identity,
        expanded.effective_environment.workspace_root_identity
    );
}

#[cfg(unix)]
#[tokio::test]
async fn run_start_rejects_a_fresh_identity_environment_mismatch() {
    let fixture = ApiFixture::new("api-environment-drift", false);
    let df = fixture.install_fake_execution_dependencies();
    let (plan, plan_hash) = validated_command_plan(&fixture).await;

    let docker = df.parent().expect("fake binary parent").join("docker");
    fs::write(
        docker,
        concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n",
            "  printf '%s\\n' 'sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'\n",
            "fi\n",
        ),
    )
    .expect("change fixed image identity after review");

    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/runs",
            &json!({
                "plan": plan,
                "plan_hash": plan_hash,
                "client_request_id": "api-environment-drift-start",
                "starting_preset": null
            }),
        ),
    )
    .await;
    assert_error(response, StatusCode::CONFLICT, "plan_hash_mismatch").await;
    assert!(fixture
        .store()
        .list_run_ids()
        .expect("list runs after rejected environment drift")
        .is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn run_start_accepts_free_space_snapshot_movement_and_records_admission_value() {
    let fixture = ApiFixture::new("api-free-space-movement", false);
    let df = fixture.install_fake_execution_dependencies();
    let (plan, plan_hash) = validated_command_plan(&fixture).await;

    fs::write(
        df,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n",
            "printf '%s\\n' 'fake 2000000 1000001 999999 50% /'\n",
        ),
    )
    .expect("move free-space snapshot after review");

    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/runs",
            &json!({
                "plan": plan,
                "plan_hash": plan_hash,
                "client_request_id": "api-free-space-movement-start",
                "starting_preset": null
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let created = body_json(response).await;
    let run_id = created["run_id"].as_str().expect("accepted run id");
    let expanded = fixture
        .store()
        .read_envelope::<ExpandedPlan>(
            run_id,
            ArtifactId::ExpandedPlan,
            EXPANDED_PLAN_SCHEMA_NAME,
            EXPANDED_PLAN_SCHEMA_VERSION,
        )
        .expect("read admission-expanded plan");
    assert_eq!(expanded.plan_hash, plan_hash);
    assert_eq!(
        expanded.effective_environment.free_space_bytes,
        Some(999_999 * 1024)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn run_start_rechecks_free_space_admission_after_review() {
    let fixture = ApiFixture::new("api-free-space-admission", false);
    let df = fixture.install_fake_execution_dependencies();
    let (plan, plan_hash) = validated_command_plan(&fixture).await;

    fs::write(
        df,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n",
            "printf '%s\\n' 'fake 2000000 1999999 1 100% /'\n",
        ),
    )
    .expect("make free-space admission fail after review");

    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/runs",
            &json!({
                "plan": plan,
                "plan_hash": plan_hash,
                "client_request_id": "api-free-space-admission-start",
                "starting_preset": null
            }),
        ),
    )
    .await;
    assert_error(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "plan_not_runnable",
    )
    .await;
    assert!(fixture
        .store()
        .list_run_ids()
        .expect("list runs after failed free-space admission")
        .is_empty());
}

#[tokio::test]
async fn artifact_routes_expose_only_generated_ids_and_reject_path_guesses() {
    let fixture = ApiFixture::new("api-artifacts", false);
    let store = fixture.store();
    let run = store
        .create_run("artifact-run")
        .expect("create artifact run");
    store
        .write_immutable(
            "artifact-run",
            ArtifactId::Summary,
            "test_summary",
            1,
            &json!({"message": "visible"}),
        )
        .expect("write allowlisted artifact");
    let evidence_reference = store
        .write_trial_evidence(
            "artifact-run",
            "sha256:cell",
            "trial-000001",
            &json!({"kind": "bounded-test-evidence"}),
        )
        .expect("write bounded trial evidence");
    fs::write(run.join("secret.txt"), "must remain private").expect("write unlisted secret");

    let response = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/api/v1/runs/artifact-run/artifacts"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let index = body_json(response).await;
    let items = index["artifacts"]
        .as_array()
        .expect("artifact response has an array");
    assert_eq!(items.len(), 2);
    let summary = items
        .iter()
        .find(|item| item["artifact_id"] == "summary")
        .expect("summary index entry");
    assert_eq!(summary["media_type"], "application/json");
    assert!(summary["sha256"]
        .as_str()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    let bounded = items
        .iter()
        .find(|item| item["artifact_id"] == evidence_reference.artifact_id)
        .expect("bounded evidence index entry");
    assert!(bounded["label"]
        .as_str()
        .is_some_and(|label| label.contains("/bounded-evidence/operation-evidence-")));

    let response = handle(
        Arc::clone(&fixture.state),
        get(
            &fixture.state,
            "/api/v1/runs/artifact-run/artifacts/summary",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let content = body_json(response).await;
    assert_eq!(content["artifact_id"], "summary");
    assert_eq!(content["encoding"], "utf-8");
    assert!(content["content"]
        .as_str()
        .is_some_and(|text| text.contains("visible")));

    let response = handle(
        Arc::clone(&fixture.state),
        get(
            &fixture.state,
            &format!(
                "/api/v1/runs/artifact-run/artifacts/{}",
                evidence_reference.artifact_id
            ),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let content = body_json(response).await;
    assert_eq!(content["artifact_id"], evidence_reference.artifact_id);
    assert!(content["content"]
        .as_str()
        .is_some_and(|text| text.contains("bounded-test-evidence")));

    for uri in [
        "/api/v1/runs/artifact-run/artifacts/secret",
        "/api/v1/runs/artifact-run/artifacts/%2e%2e",
        "/api/v1/runs/../artifacts/summary",
    ] {
        let response = handle(Arc::clone(&fixture.state), get(&fixture.state, uri)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "URI: {uri}");
    }
    assert_eq!(
        fs::read_to_string(run.join("secret.txt")).expect("read untouched secret"),
        "must remain private"
    );
}

#[tokio::test]
async fn reports_regenerate_from_artifacts_and_compare_checks_compatibility_first() {
    let fixture = ApiFixture::new("api-report-compare", false);
    let expanded = command_expanded_plan(&fixture);
    persist_run(&fixture, "reference-run", &expanded, RunState::Completed);
    persist_run(&fixture, "candidate-run", &expanded, RunState::Completed);
    persist_run(&fixture, "active-run", &expanded, RunState::Running);

    let response = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/api/v1/runs/active-run/report"),
    )
    .await;
    assert_error(response, StatusCode::CONFLICT, "report_not_final").await;

    let response = handle(
        Arc::clone(&fixture.state),
        get(
            &fixture.state,
            "/api/v1/runs/active-run/report?provisional=true",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let provisional = body_json(response).await;
    assert_eq!(provisional["run_id"], "active-run");
    assert_eq!(provisional["provisional"], true);
    assert_eq!(provisional["correctness_verdict"], "pending");

    for run_id in ["reference-run", "candidate-run"] {
        let response = handle(
            Arc::clone(&fixture.state),
            get(&fixture.state, &format!("/api/v1/runs/{run_id}/report")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let report = body_json(response).await;
        assert_eq!(report["run_id"], run_id);
        assert_eq!(report["provisional"], false);
        assert_eq!(report["correctness_verdict"], "pass");
        assert_eq!(report["plan_hash"], expanded.plan_hash);
    }

    let response = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/api/v1/runs/reference-run"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let run = body_json(response).await;
    assert_eq!(run["manifest"]["run_id"], "reference-run");
    assert_eq!(run["manifest"]["state"], "completed");
    assert_eq!(run["report_ready"], true);
    assert_eq!(run["latest_sequence"], 0);
    assert_eq!(
        run["progress"]["total_trial_batches"],
        expanded.estimates.trial_batch_count
    );

    let response = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/api/v1/runs"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let list = body_json(response).await;
    assert!(list["runs"].as_array().is_some_and(|runs| {
        runs.iter().any(|run| run["run_id"] == "reference-run")
            && runs.iter().any(|run| run["run_id"] == "candidate-run")
    }));

    let store = fixture.store();
    fs::remove_file(
        store
            .run_path("reference-run")
            .expect("resolve reference artifact directory")
            .join(ArtifactId::Report.file_name()),
    )
    .expect("delete derived reference report before comparison");
    let mut stale_candidate: Value = store
        .read_envelope(
            "candidate-run",
            ArtifactId::Report,
            REPORT_SCHEMA_NAME,
            REPORT_SCHEMA_VERSION,
        )
        .expect("read derived candidate report");
    stale_candidate["plan_hash"] = json!("stale-derived-plan-hash");
    store
        .replace_snapshot(
            "candidate-run",
            ArtifactId::Report,
            REPORT_SCHEMA_NAME,
            REPORT_SCHEMA_VERSION,
            &stale_candidate,
        )
        .expect("persist stale derived candidate report");

    let compare = json!({
        "reference_run_id": "reference-run",
        "candidate_run_id": "candidate-run",
        "descriptive_override": false
    });
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(&fixture.state, Method::POST, "/api/v1/compare", &compare),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let comparison = body_json(response).await;
    assert_eq!(comparison["reference_run_id"], "reference-run");
    assert_eq!(comparison["candidate_run_id"], "candidate-run");
    assert_eq!(comparison["compatible"], false);
    let checks = comparison["checks"]
        .as_array()
        .expect("comparison has ordered compatibility checks");
    assert_eq!(checks[0]["check_id"], "comparison_declaration");
    assert_eq!(checks[1]["check_id"], "terminal_reports");
    assert!(checks
        .iter()
        .any(|check| { check["blocks_aggregate"] == true && check["compatible"] == false }));
    assert!(comparison["matched_cells"]
        .as_array()
        .is_some_and(|cells| !cells.is_empty()));
    assert!(comparison["deltas"].as_array().is_some_and(Vec::is_empty));
    assert!(comparison["performance_verdict"].is_null());
    for run_id in ["reference-run", "candidate-run"] {
        assert!(store
            .run_path(run_id)
            .expect("resolve regenerated run artifact directory")
            .join(ArtifactId::Report.file_name())
            .is_file());
        let regenerated: Value = store
            .read_envelope(
                run_id,
                ArtifactId::Report,
                REPORT_SCHEMA_NAME,
                REPORT_SCHEMA_VERSION,
            )
            .expect("read regenerated report");
        assert_eq!(regenerated["plan_hash"], expanded.plan_hash);
    }

    let same_run = json!({
        "reference_run_id": "reference-run",
        "candidate_run_id": "reference-run",
        "descriptive_override": false
    });
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(&fixture.state, Method::POST, "/api/v1/compare", &same_run),
    )
    .await;
    assert_error(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "comparison_unavailable",
    )
    .await;
}

#[tokio::test]
async fn compare_blocks_nonidentical_cell_and_operation_sets_before_deltas() {
    let fixture = ApiFixture::new("api-compare-scope", false);

    let reference = command_expanded_plan(&fixture);
    let narrowed = command_expanded_plan_with_concurrency(&fixture, vec![1, 5]);
    persist_run(&fixture, "scope-reference", &reference, RunState::Completed);
    persist_run(&fixture, "scope-candidate", &narrowed, RunState::Completed);
    sandbox_benchmark::report::regenerate(&fixture.store(), "scope-reference", false)
        .expect("materialize reference report");
    sandbox_benchmark::report::regenerate(&fixture.store(), "scope-candidate", false)
        .expect("materialize narrowed report");

    let comparison = compare_json(&fixture, "scope-reference", "scope-candidate", false).await;
    assert_eq!(comparison["compatible"], false);
    assert!(comparison["matched_cells"]
        .as_array()
        .is_some_and(|cells| !cells.is_empty()));
    assert_blocking_incompatibility(&comparison, "matched_cell_scope");
    assert!(comparison["deltas"].as_array().is_some_and(Vec::is_empty));

    let files = expanded_plan_for_scope(&fixture, ConfigurationScope::Files);
    persist_run(&fixture, "operation-candidate", &files, RunState::Completed);
    sandbox_benchmark::report::regenerate(&fixture.store(), "operation-candidate", false)
        .expect("materialize extra-operation report");

    let comparison = compare_json(&fixture, "scope-reference", "operation-candidate", false).await;
    assert_eq!(comparison["compatible"], false);
    assert_blocking_incompatibility(&comparison, "matched_cell_scope");
    assert_blocking_incompatibility(&comparison, "operation_definition_revision.file_read");
    assert!(comparison["deltas"].as_array().is_some_and(Vec::is_empty));
}

#[tokio::test]
async fn effective_policy_identity_mismatches_block_aggregate_comparison() {
    let fixture = ApiFixture::new("api-compare-policy", false);
    let expanded = command_expanded_plan(&fixture);
    persist_run(&fixture, "policy-reference", &expanded, RunState::Completed);

    let mut failure_expanded = expanded.clone();
    failure_expanded.fixed_lifecycle_policy.failure_revision += 1;
    persist_run(
        &fixture,
        "failure-candidate",
        &failure_expanded,
        RunState::Completed,
    );

    for run_id in [
        "timeout-candidate",
        "gateway-candidate",
        "safety-candidate",
        "fixture-generator-candidate",
    ] {
        persist_run(&fixture, run_id, &expanded, RunState::Completed);
    }
    rewrite_manifest(&fixture, "timeout-candidate", |manifest| {
        manifest.effective_timeouts.sandbox_create_timeout_ms += 1;
    });
    rewrite_manifest(&fixture, "gateway-candidate", |manifest| {
        manifest.gateway_policy.maximum_connections += 1;
    });
    rewrite_manifest(&fixture, "safety-candidate", |manifest| {
        manifest.safety_policy.fixed_gateway_caps.log_bytes += 1;
    });
    rewrite_manifest(&fixture, "fixture-generator-candidate", |manifest| {
        manifest.fixture_generator_revision += 1;
    });

    for (candidate, check_id) in [
        ("failure-candidate", "effective_failure_policy"),
        ("timeout-candidate", "effective_timeout_policy"),
        ("gateway-candidate", "effective_gateway_policy"),
        ("safety-candidate", "effective_safety_policy"),
        ("fixture-generator-candidate", "fixture_generator_revision"),
    ] {
        sandbox_benchmark::report::regenerate(&fixture.store(), candidate, false)
            .unwrap_or_else(|error| panic!("materialize {candidate} report: {error}"));
        let comparison = compare_json(&fixture, "policy-reference", candidate, false).await;
        assert_eq!(comparison["compatible"], false, "candidate: {candidate}");
        assert_blocking_incompatibility(&comparison, check_id);
        assert!(comparison["deltas"].as_array().is_some_and(Vec::is_empty));
    }
}

#[tokio::test]
async fn descriptive_override_returns_raw_metric_and_phase_summaries_without_aggregate_claims() {
    let fixture = ApiFixture::new("api-compare-descriptive", false);
    let expanded = expanded_plan_for_scope(&fixture, ConfigurationScope::LayerStack);
    persist_run(
        &fixture,
        "descriptive-reference",
        &expanded,
        RunState::Completed,
    );
    persist_run(
        &fixture,
        "descriptive-candidate",
        &expanded,
        RunState::Completed,
    );
    rewrite_manifest(&fixture, "descriptive-candidate", |manifest| {
        manifest.effective_timeouts.operation_teardown_timeout_ms += 1;
    });
    append_layerstack_samples(
        &fixture,
        "descriptive-reference",
        &expanded,
        &[(10, 5), (12, 7)],
    );
    append_layerstack_samples(
        &fixture,
        "descriptive-candidate",
        &expanded,
        &[(20, 8), (22, 10)],
    );

    let comparison = compare_json(
        &fixture,
        "descriptive-reference",
        "descriptive-candidate",
        true,
    )
    .await;
    assert_eq!(comparison["compatible"], false);
    assert_eq!(comparison["descriptive_only"], true);
    assert!(comparison["performance_verdict"].is_null());
    let cell_id = &expanded.cells[0].cell_id;
    let latency = comparison["deltas"]
        .as_array()
        .and_then(|deltas| {
            deltas.iter().find(|delta| {
                delta["reference_cell_id"] == *cell_id
                    && delta["metric_id"] == PRIMARY_LATENCY_METRIC_ID
            })
        })
        .expect("descriptive operation-latency projection");
    assert_eq!(latency["descriptive_only"], true);
    assert!(latency["absolute_change"].is_null());
    assert!(latency["percent_change"].is_null());
    assert!(latency["median_difference_confidence_interval"].is_null());
    assert_eq!(
        latency["reference_statistics"]["distribution"],
        json!({"kind": "raw_points", "values": [10.0, 12.0]})
    );
    assert_eq!(
        latency["candidate_statistics"]["distribution"],
        json!({"kind": "raw_points", "values": [20.0, 22.0]})
    );

    let phase_id = serde_json::to_value(definition(OperationId::SquashLayerstack).phases[0].id)
        .expect("serialize registered phase id");
    let phase = comparison["phase_comparisons"]
        .as_array()
        .and_then(|phases| {
            phases.iter().find(|phase| {
                phase["reference_cell_id"] == *cell_id && phase["phase_id"] == phase_id
            })
        })
        .expect("descriptive phase projection");
    assert_eq!(phase["descriptive_only"], true);
    assert!(phase["absolute_change"].is_null());
    assert!(phase["percent_change"].is_null());
    assert!(phase["median_difference_confidence_interval"].is_null());
    assert_eq!(phase["reference_summary"]["attempted"], 2);
    assert_eq!(phase["candidate_summary"]["attempted"], 2);
    assert_eq!(
        phase["reference_summary"]["duration"]["distribution"],
        json!({"kind": "raw_points", "values": [5.0, 7.0]})
    );
    assert_eq!(
        phase["candidate_summary"]["duration"]["distribution"],
        json!({"kind": "raw_points", "values": [8.0, 10.0]})
    );
}

#[tokio::test]
async fn compatible_comparison_derives_phase_deltas() {
    let fixture = ApiFixture::new("api-compare-phase", false);
    let mut expanded = expanded_plan_for_scope(&fixture, ConfigurationScope::LayerStack);
    expanded.effective_environment.image_digest = Some(format!("sha256:{}", "1".repeat(64)));
    expanded.effective_environment.filesystem = Some("contract-test-fs".to_owned());
    persist_comparable_run(&fixture, "phase-reference", &expanded, RunState::Completed);
    persist_comparable_run(&fixture, "phase-candidate", &expanded, RunState::Completed);
    append_layerstack_samples(&fixture, "phase-reference", &expanded, &[(10, 5), (12, 7)]);
    append_layerstack_samples(&fixture, "phase-candidate", &expanded, &[(20, 8), (22, 10)]);

    let comparison = compare_json(&fixture, "phase-reference", "phase-candidate", false).await;
    assert_eq!(comparison["compatible"], true, "{comparison:#}");
    assert_eq!(comparison["descriptive_only"], false);
    let cell_id = &expanded.cells[0].cell_id;
    let phase_id = serde_json::to_value(definition(OperationId::SquashLayerstack).phases[0].id)
        .expect("serialize registered phase id");
    let phase = comparison["phase_comparisons"]
        .as_array()
        .and_then(|phases| {
            phases.iter().find(|phase| {
                phase["reference_cell_id"] == *cell_id && phase["phase_id"] == phase_id
            })
        })
        .expect("aggregate phase projection");
    assert_eq!(phase["reference_value"], 6.0);
    assert_eq!(phase["candidate_value"], 9.0);
    assert_eq!(phase["absolute_change"], 3.0);
    assert_eq!(phase["percent_change"], 50.0);
    assert_eq!(
        phase["confidence_interval_omission_reason"],
        "insufficient_n"
    );
    assert_eq!(phase["descriptive_only"], false);
}

#[tokio::test]
async fn cancellation_is_idempotent_and_bound_to_the_active_campaign() {
    let fixture = ApiFixture::new("api-cancel", false);
    let expanded = command_expanded_plan(&fixture);
    let (run_id, cancellation, reused) = fixture
        .state
        .campaigns
        .reserve("api-contract-cancellation")
        .expect("reserve active campaign");
    assert!(!reused);
    let artifacts = RunArtifacts::create(
        fixture.store(),
        &run_id,
        &expanded,
        None,
        environment_for(&expanded),
        catalog(),
    )
    .await
    .expect("create authoritative active run");
    for state in [RunState::Queued, RunState::Preparing, RunState::Running] {
        artifacts
            .transition(state, None)
            .expect("advance active run manifest");
    }
    fixture
        .state
        .register_run_artifacts(&run_id, Arc::clone(&artifacts))
        .expect("register active manifest authority");
    fixture
        .state
        .campaigns
        .update_state(&run_id, RunState::Running)
        .expect("mark campaign running");

    let uri = format!("/api/v1/runs/{run_id}/cancel");
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(&fixture.state, Method::POST, &uri, &json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let first = body_json(response).await;
    assert_eq!(first["run_id"], run_id);
    assert_eq!(first["state"], "cancelling");
    assert_eq!(first["cancellation_requested"], true);
    assert!(cancellation.is_cancelled());
    assert_eq!(
        artifacts
            .manifest()
            .expect("read cancellation manifest")
            .state,
        RunState::Cancelling
    );
    let persisted: RunManifest = fixture
        .store()
        .read_envelope(
            &run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .expect("read durable cancellation state");
    assert_eq!(persisted.state, RunState::Cancelling);

    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(&fixture.state, Method::POST, &uri, &json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(body_json(response).await["cancellation_requested"], false);

    persist_run(&fixture, "detached-run", &expanded, RunState::Running);
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/runs/detached-run/cancel",
            &json!({}),
        ),
    )
    .await;
    assert_error(response, StatusCode::CONFLICT, "run_not_active").await;

    persist_run(&fixture, "finished-run", &expanded, RunState::Completed);
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/runs/finished-run/cancel",
            &json!({}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let finished = body_json(response).await;
    assert_eq!(finished["state"], "completed");
    assert_eq!(finished["cancellation_requested"], false);
}

#[tokio::test]
async fn sse_replays_in_order_and_reconnects_after_last_event_id() {
    let fixture = ApiFixture::new("api-sse", false);
    let store = fixture.store();
    store.create_run("event-run").expect("create event run");
    let journal = EventJournal::open(store, "event-run")
        .await
        .expect("open event journal");
    journal
        .emit(
            10,
            EventData::RunState {
                state: RunState::Queued,
            },
        )
        .await
        .expect("emit first event");
    journal
        .emit(
            20,
            EventData::Warning {
                code: "fixture_warning".to_owned(),
                message: "persisted warning".to_owned(),
            },
        )
        .await
        .expect("emit second event");
    fixture
        .state
        .register_journal("event-run", Arc::clone(&journal))
        .await;

    let response = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/api/v1/runs/event-run/events"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[CONTENT_TYPE],
        "text/event-stream; charset=utf-8"
    );
    assert_eq!(response.headers()[CACHE_CONTROL], "no-cache, no-store");
    let mut body = response.into_body();
    let first = next_sse_frame(&mut body).await;
    let second = next_sse_frame(&mut body).await;
    assert!(first.starts_with("id: 1\nevent: run_state\n"));
    assert!(second.starts_with("id: 2\nevent: warning\n"));
    assert!(!second.contains("id: 1\n"));
    drop(body);

    let reconnect = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/runs/event-run/events")
        .header(HOST, fixture.state.authority())
        .header("last-event-id", "1")
        .body(Full::new(Bytes::new()))
        .expect("build SSE reconnect request");
    let response = handle(Arc::clone(&fixture.state), reconnect).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let replayed = next_sse_frame(&mut body).await;
    assert!(replayed.starts_with("id: 2\nevent: warning\n"));
    assert!(!replayed.contains("id: 1\n"));

    let invalid = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/runs/event-run/events")
        .header(HOST, fixture.state.authority())
        .header("last-event-id", "not-a-sequence")
        .body(Full::new(Bytes::new()))
        .expect("build invalid SSE replay request");
    let response = handle(Arc::clone(&fixture.state), invalid).await;
    assert_error(response, StatusCode::BAD_REQUEST, "invalid_last_event_id").await;
}

#[tokio::test]
async fn static_bootstrap_injects_nonce_and_rejects_symlinks_and_traversal() {
    let fixture = ApiFixture::new("api-static", false);

    let response = handle(Arc::clone(&fixture.state), get(&fixture.state, "/command")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let csp = response.headers()[CONTENT_SECURITY_POLICY]
        .to_str()
        .expect("CSP is text");
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("connect-src 'self'"));
    assert!(csp.contains("object-src 'none'"));
    assert!(!response
        .headers()
        .contains_key("access-control-allow-origin"));
    let html = String::from_utf8(body_bytes(response).await.to_vec()).expect("HTML is UTF-8");
    assert!(html.contains(&format!("content=\"{}\"", fixture.state.nonce())));
    assert!(!html.contains("eos-benchmark-nonce\" content=\"\""));

    let response = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/assets/app.css"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "text/css; charset=utf-8");
    assert_eq!(
        response.headers()[CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );

    let response = handle(
        Arc::clone(&fixture.state),
        get(&fixture.state, "/assets/../outside.js"),
    )
    .await;
    assert_error(response, StatusCode::BAD_REQUEST, "invalid_web_asset_path").await;

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = fixture._root.join("outside.js");
        fs::write(&outside, "globalThis.stolen = true").expect("write outside asset");
        symlink(&outside, fixture.web_root.join("assets/leak.js")).expect("create asset symlink");
        let response = handle(
            Arc::clone(&fixture.state),
            get(&fixture.state, "/assets/leak.js"),
        )
        .await;
        assert_error(response, StatusCode::NOT_FOUND, "web_asset_not_found").await;
    }
}

async fn next_sse_frame(body: &mut ResponseBody) -> String {
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("SSE frame should not time out")
        .expect("SSE stream should remain open")
        .expect("SSE body is infallible");
    String::from_utf8(
        frame
            .into_data()
            .expect("SSE frame should contain data")
            .to_vec(),
    )
    .expect("SSE frame should be UTF-8")
}

async fn compare_json(
    fixture: &ApiFixture,
    reference_run_id: &str,
    candidate_run_id: &str,
    descriptive_override: bool,
) -> Value {
    let response = handle(
        Arc::clone(&fixture.state),
        json_mutation(
            &fixture.state,
            Method::POST,
            "/api/v1/compare",
            &json!({
                "reference_run_id": reference_run_id,
                "candidate_run_id": candidate_run_id,
                "descriptive_override": descriptive_override,
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

fn assert_blocking_incompatibility(comparison: &Value, check_id: &str) {
    let check = comparison["checks"]
        .as_array()
        .and_then(|checks| checks.iter().find(|check| check["check_id"] == check_id))
        .unwrap_or_else(|| panic!("comparison is missing compatibility check {check_id}"));
    assert_eq!(check["compatible"], false, "check: {check_id}");
    assert_eq!(check["blocks_aggregate"], true, "check: {check_id}");
}

fn command_expanded_plan(fixture: &ApiFixture) -> ExpandedPlan {
    expanded_plan_for_scope(fixture, ConfigurationScope::Command)
}

fn expanded_plan_for_scope(fixture: &ApiFixture, scope: ConfigurationScope) -> ExpandedPlan {
    let config = fixture.config();
    let all = load_plan(&config.repo.join("benchmark/defaults/standard-local.yml"))
        .expect("load versioned default");
    let scoped = slice_default(&all, scope);
    let expanded =
        validate_and_expand(&scoped, &config.paths, Some(&scoped)).expect("expand scoped default");
    assert!(expanded.runnable);
    expanded
}

fn command_expanded_plan_with_concurrency(
    fixture: &ApiFixture,
    concurrent_requests: Vec<u32>,
) -> ExpandedPlan {
    let config = fixture.config();
    let all = load_plan(&config.repo.join("benchmark/defaults/standard-local.yml"))
        .expect("load versioned default");
    let base = slice_default(&all, ConfigurationScope::Command);
    let mut plan = base.clone();
    let command = plan
        .operations
        .iter_mut()
        .find_map(|operation| match operation {
            OperationPlan::ExecCommand(command) => Some(command),
            OperationPlan::FileRead(_)
            | OperationPlan::FileWrite(_)
            | OperationPlan::FileEdit(_)
            | OperationPlan::FileBlame(_)
            | OperationPlan::CreateWorkspace(_)
            | OperationPlan::SquashLayerstack(_) => None,
        })
        .expect("command operation");
    command.factors.concurrent_requests.values = concurrent_requests;
    command.factors.concurrent_requests.control = Some(1);
    let expanded = validate_and_expand(&plan, &config.paths, Some(&base))
        .expect("expand narrowed command plan");
    assert!(expanded.runnable);
    expanded
}

fn persist_comparable_run(
    fixture: &ApiFixture,
    run_id: &str,
    expanded: &ExpandedPlan,
    state: RunState,
) {
    persist_run(fixture, run_id, expanded, state);
    rewrite_manifest(fixture, run_id, |manifest| {
        manifest.environment.host.kernel_release = Some("6.10.14-contract".to_owned());
        manifest.environment.host.docker_engine_version = Some("27.4.1".to_owned());
        manifest.fixture_hashes.insert(
            "fixture:contract".to_owned(),
            format!("sha256:{}", "a".repeat(64)),
        );
    });
}

fn rewrite_manifest(fixture: &ApiFixture, run_id: &str, mutate: impl FnOnce(&mut RunManifest)) {
    let store = fixture.store();
    let mut manifest: RunManifest = store
        .read_envelope(
            run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .expect("read run manifest");
    mutate(&mut manifest);
    store
        .replace_snapshot(
            run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
            &manifest,
        )
        .expect("rewrite run manifest");
}

fn append_layerstack_samples(
    fixture: &ApiFixture,
    run_id: &str,
    expanded: &ExpandedPlan,
    samples: &[(u64, u64)],
) {
    let store = fixture.store();
    let cell = expanded.cells.first().expect("layerstack cell");
    assert_eq!(cell.operation_id, OperationId::SquashLayerstack);
    let phase = definition(cell.operation_id)
        .phases
        .first()
        .expect("registered layerstack phase");
    let mut sequence = 0_u64;
    for (index, &(latency_ns, phase_duration_ns)) in samples.iter().enumerate() {
        let trial_id = format!("trial-{}", index + 1);
        let request_id = format!("request-{}", index + 1);
        let sequence_in_cell = u32::try_from(index + 1).expect("bounded sample index");
        for record in [
            ObservationRecord::Request(RequestObservation {
                operation_id: cell.operation_id,
                cell_id: cell.cell_id.clone(),
                trial_id: trial_id.clone(),
                request_id: request_id.clone(),
                start_offset_ns: 5,
                latency_ns,
                succeeded: true,
                status: "ok".to_owned(),
                response_bytes: 0,
                bounded_response_sha256: None,
            }),
            ObservationRecord::Phase(PhaseObservation {
                id: phase.id,
                semantic_revision: phase.semantic_revision,
                unit: phase.unit,
                cell_id: cell.cell_id.clone(),
                trial_id: trial_id.clone(),
                request_id: Some(request_id.clone()),
                source: phase.source,
                correlation: phase.correlation,
                trace_span_name: phase.trace_span_name.to_owned(),
                start_offset_ns: 6,
                duration_ns: phase_duration_ns,
                status: PhaseStatus::Succeeded,
            }),
            ObservationRecord::Trial(TrialSample {
                operation_id: cell.operation_id,
                cell_id: cell.cell_id.clone(),
                trial_id,
                kind: TrialKind::Measured,
                sequence_in_cell,
                lifecycle: LifecycleDurations {
                    setup_ns: 1,
                    operation_ns: latency_ns,
                    verify_ns: 1,
                    teardown_ns: 1,
                },
                product_succeeded: true,
                infrastructure_failed: false,
                cleanup_baseline_restored: true,
                correctness: CorrectnessFold {
                    product_succeeded: true,
                    required_check_count: 0,
                    attempted_check_count: 0,
                    passed_check_count: 0,
                    failed_check_count: 0,
                    missing_checks: Vec::new(),
                    unexpected_checks: Vec::new(),
                    eligible_for_latency: true,
                },
                primary_operation_latency_ns: Some(latency_ns),
                artifacts: Vec::new(),
            }),
        ] {
            sequence += 1;
            store
                .append_record(
                    run_id,
                    ArtifactId::Observations,
                    OBSERVATION_SCHEMA_NAME,
                    OBSERVATION_SCHEMA_VERSION,
                    &SequencedObservation { sequence, record },
                )
                .expect("append authoritative comparison observation");
        }
    }
}

fn persist_run(fixture: &ApiFixture, run_id: &str, expanded: &ExpandedPlan, state: RunState) {
    let store = fixture.store();
    store.create_run(run_id).expect("create persisted run");
    store
        .write_immutable(
            run_id,
            ArtifactId::IntentPlan,
            INTENT_PLAN_SCHEMA_NAME,
            expanded.canonical_plan.schema_version,
            &expanded.canonical_plan,
        )
        .expect("write intent plan");
    store
        .write_immutable(
            run_id,
            ArtifactId::ExpandedPlan,
            EXPANDED_PLAN_SCHEMA_NAME,
            EXPANDED_PLAN_SCHEMA_VERSION,
            expanded,
        )
        .expect("write expanded plan");
    let definitions = catalog();
    store
        .write_immutable(
            run_id,
            ArtifactId::DefinitionSnapshot,
            DEFINITION_SNAPSHOT_SCHEMA_NAME,
            definitions.schema_version,
            &definitions,
        )
        .expect("write definition snapshot");

    let environment = environment_for(expanded);
    store
        .write_immutable(
            run_id,
            ArtifactId::EnvironmentMetadata,
            ENVIRONMENT_METADATA_SCHEMA_NAME,
            environment.schema_version,
            &environment,
        )
        .expect("write environment metadata");
    let terminal = matches!(
        state,
        RunState::Completed | RunState::Failed | RunState::Cancelled
    );
    let definition_snapshot_sha256 = format!(
        "sha256:{:x}",
        Sha256::digest(
            &store
                .content(run_id, ArtifactId::DefinitionSnapshot.as_str())
                .expect("read definition snapshot")
                .bytes
        )
    );
    let mut manifest = RunManifest::planned(
        run_id,
        expanded,
        None,
        environment,
        &definitions,
        definition_snapshot_sha256,
    )
    .expect("build authoritative run manifest");
    manifest.state = state;
    manifest.created_at = "2026-07-12T00:00:00Z".to_owned();
    manifest.started_at = Some("2026-07-12T00:00:01Z".to_owned());
    manifest.ended_at = terminal.then(|| "2026-07-12T00:00:02Z".to_owned());
    store
        .replace_snapshot(
            run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
            &manifest,
        )
        .expect("write run manifest");
}

fn environment_for(expanded: &ExpandedPlan) -> EnvironmentMetadata {
    let treatment = TreatmentIdentity {
        source_commit: "0123456789abcdef".to_owned(),
        source_dirty: false,
        source_diff_hash: None,
        daemon_binary_hash: Some("sha256:daemon".to_owned()),
        gateway_binary_hash: Some("sha256:gateway".to_owned()),
    };
    EnvironmentMetadata {
        schema_version: 1,
        treatment: treatment.clone(),
        host: HostEnvironment {
            operating_system: "contract-test".to_owned(),
            architecture: "test".to_owned(),
            kernel_release: Some("test".to_owned()),
            docker_engine_version: Some("test".to_owned()),
            filesystem: expanded.effective_environment.filesystem.clone(),
            free_space_bytes: expanded.effective_environment.free_space_bytes,
            monotonic_clock: "std::time::Instant".to_owned(),
        },
        image_reference: expanded.canonical_plan.environment.image.0.clone(),
        image_digest: expanded.effective_environment.image_digest.clone(),
        workspace_root_identity: expanded
            .effective_environment
            .workspace_root_identity
            .clone(),
        client_cohort: expanded.effective_environment.client_cohort,
        gateway_endpoint_identity: "isolated-contract-gateway".to_owned(),
    }
}
