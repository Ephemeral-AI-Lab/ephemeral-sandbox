use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::convert::Infallible;
use std::fs;
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use http::header::{
    CACHE_CONTROL, CONNECTION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, ORIGIN,
};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt as _, Full, Limited, StreamBody};
use hyper::body::{Body, Frame};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app::AppState;
use crate::artifacts::{ArtifactError, ArtifactId, ArtifactStore};
use crate::compare::{self, ComparisonRequest};
use crate::config::{SettingsUpdate, StartupConfig};
use crate::definitions::{self, DefinitionCatalog};
use crate::events::{
    encode_sse, EventData, EventRecord, RequestState, RunState, WorkState, EVENT_SCHEMA_NAME,
    EVENT_SCHEMA_VERSION,
};
use crate::fixtures::load_workspace_profiles;
use crate::model::{ConfigurationScope, ExperimentPlan};
use crate::plan::{
    load_plan, load_presets, slice_default, validate_and_expand_with_profiles,
    validate_and_expand_with_profiles_and_environment, ExpandedPlan, FindingSeverity, PlanError,
    PlanValidationRequest, PresetFile, RunCreateRequest, RuntimeEnvironmentSnapshot,
    ValidationFinding, EXPANDED_PLAN_SCHEMA_VERSION,
};
use crate::report::{self, CorrectnessVerdict};
use crate::scheduler::{
    capture_environment, capture_runtime_environment, fail_campaign, is_terminal, run_campaign,
    CampaignGateError, RunArtifacts, RunManifest, RunProgress, SchedulerError, TrialKind,
    EXPANDED_PLAN_SCHEMA_NAME, RUN_MANIFEST_SCHEMA_NAME, RUN_MANIFEST_SCHEMA_VERSION,
};

pub const API_PREFIX: &str = "/api/v1";
pub const API_SCHEMA_VERSION: u32 = 1;
const MAX_JSON_BODY_BYTES: usize = 1024 * 1024;
const MAX_STATIC_ASSET_BYTES: u64 = 32 * 1024 * 1024;
const NONCE_HEADER: &str = "x-eos-benchmark-nonce";
const LAST_EVENT_ID: &str = "last-event-id";

pub type ResponseBody = UnsyncBoxBody<Bytes, Infallible>;

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ApiErrorBody {
    code: String,
    message: String,
    details: Value,
    request_id: String,
}

#[derive(Debug)]
struct ApiFailure {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Value,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct DefinitionsResponse {
    schema_version: u32,
    catalog: DefinitionCatalog,
    defaults: Vec<ExperimentPlan>,
    presets: Vec<PresetFile>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct HealthCheck {
    id: String,
    status: HealthCheckStatus,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum HealthCheckStatus {
    Pass,
    Warning,
    Fail,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum HealthStatus {
    Ready,
    Degraded,
    Unready,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct HealthResponse {
    schema_version: u32,
    status: HealthStatus,
    execution_ready: bool,
    version: &'static str,
    runner_instance_id: String,
    active_run: Option<ActiveRunResponse>,
    checks: Vec<HealthCheck>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ActiveRunResponse {
    run_id: String,
    state: RunState,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RunCreateResponse {
    schema_version: u32,
    run_id: String,
    state: RunState,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RunCancelResponse {
    schema_version: u32,
    run_id: String,
    state: RunState,
    cancellation_requested: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum SummaryCorrectness {
    Pass,
    Fail,
    Pending,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RunSummary {
    run_id: String,
    name: String,
    state: RunState,
    plan_hash: String,
    configuration_scope: ConfigurationScope,
    source_commit: String,
    source_dirty: bool,
    started_at: String,
    ended_at: Option<String>,
    correctness: SummaryCorrectness,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RunListResponse {
    schema_version: u32,
    runs: Vec<RunSummary>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RunManifestSummary {
    #[serde(flatten)]
    summary: RunSummary,
    definition_snapshot_version: u32,
    environment_fingerprint: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RunResponse {
    schema_version: u32,
    manifest: RunManifestSummary,
    progress: RunProgress,
    latest_sequence: u64,
    report_ready: bool,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactIndexResponse {
    schema_version: u32,
    run_id: String,
    artifacts: Vec<ArtifactIndexItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactIndexItem {
    artifact_id: String,
    label: String,
    media_type: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactContentResponse {
    schema_version: u32,
    #[serde(flatten)]
    index: ArtifactIndexItem,
    encoding: &'static str,
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyRequest {}

pub async fn handle<B>(state: Arc<AppState>, request: Request<B>) -> Response<ResponseBody>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let request_id = Uuid::new_v4().to_string();
    match route(state, request).await {
        Ok(response) => response,
        Err(failure) => error_response(failure, &request_id),
    }
}

async fn route<B>(
    state: Arc<AppState>,
    request: Request<B>,
) -> Result<Response<ResponseBody>, ApiFailure>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    validate_host(state.authority(), request.headers())?;
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_owned();
    let query = parts.uri.query().map(str::to_owned);
    let headers = parts.headers;

    if path == API_PREFIX || path.starts_with(&format!("{API_PREFIX}/")) {
        if !matches!(method, Method::GET | Method::HEAD) {
            validate_mutation_headers(&state, &headers)?;
        }
        return route_api(state, method, &path, query.as_deref(), headers, body).await;
    }
    if method != Method::GET && method != Method::HEAD {
        return Err(failure(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "This resource is read-only.",
        ));
    }
    serve_web(&state, &path, method == Method::HEAD)
}

async fn route_api<B>(
    state: Arc<AppState>,
    method: Method,
    path: &str,
    query: Option<&str>,
    headers: HeaderMap,
    body: B,
) -> Result<Response<ResponseBody>, ApiFailure>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let relative = path.strip_prefix(API_PREFIX).unwrap_or(path);
    let segments = relative
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    match (method, segments.as_slice()) {
        (Method::GET, ["health"]) => json_response(StatusCode::OK, &health(&state)?),
        (Method::GET, ["settings"]) => json_response(
            StatusCode::OK,
            &state.settings().map_err(internal_app_error)?,
        ),
        (Method::PUT, ["settings"]) => {
            let update: SettingsUpdate = read_json(&headers, body).await?;
            if state
                .campaigns
                .active()
                .map_err(internal_gate_error)?
                .is_some()
            {
                return Err(failure(
                    StatusCode::CONFLICT,
                    "runner_busy",
                    "The workspace root cannot change while a campaign is active.",
                ));
            }
            let settings = state
                .update_workspace_root(&update.test_workspace_root)
                .map_err(|_| {
                    failure(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "invalid_workspace_root",
                        "The workspace root is invalid or cannot be initialized.",
                    )
                })?;
            json_response(StatusCode::OK, &settings)
        }
        (Method::GET, ["definitions"]) => {
            let response = definitions_response(&state.config().map_err(internal_app_error)?)?;
            json_response(StatusCode::OK, &response)
        }
        (Method::POST, ["plans", "validate"]) => {
            let request: PlanValidationRequest = read_json(&headers, body).await?;
            let expanded = validate_review_request(&state, &request).await?;
            json_response(StatusCode::OK, &expanded)
        }
        (Method::POST, ["runs"]) => {
            let request: RunCreateRequest = read_json(&headers, body).await?;
            create_run(state, request).await
        }
        (Method::GET, ["runs"]) => list_runs(&state, query),
        (Method::GET, ["runs", run_id]) => run_response(&state, run_id).await,
        (Method::GET, ["runs", run_id, "events"]) => sse_response(&state, run_id, &headers).await,
        (Method::POST, ["runs", run_id, "cancel"]) => {
            let _: EmptyRequest = read_json(&headers, body).await?;
            cancel_run(&state, run_id).await
        }
        (Method::GET, ["runs", run_id, "report"]) => report_response(&state, run_id, query),
        (Method::GET, ["runs", run_id, "artifacts"]) => artifact_index(&state, run_id),
        (Method::GET, ["runs", run_id, "artifacts", artifact_id]) => {
            artifact_content(&state, run_id, artifact_id)
        }
        (Method::POST, ["compare"]) => {
            let request: ComparisonRequest = read_json(&headers, body).await?;
            let response =
                compare::compare(&state.artifacts().map_err(internal_app_error)?, &request)
                    .map_err(|_| {
                        failure(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "comparison_unavailable",
                            "The selected runs cannot be compared from their persisted evidence.",
                        )
                    })?;
            json_response(StatusCode::OK, &response)
        }
        _ => Err(failure(
            StatusCode::NOT_FOUND,
            "not_found",
            "The requested API resource does not exist.",
        )),
    }
}

fn health(state: &AppState) -> Result<HealthResponse, ApiFailure> {
    let execution_ready = state.execution_ready();
    let settings = state.settings().map_err(internal_app_error)?;
    let active_run = match state.campaigns.active().map_err(internal_gate_error)? {
        Some((run_id, gate_state)) => {
            let manifest_state = state
                .artifacts()
                .ok()
                .and_then(|store| read_manifest(&store, &run_id).ok())
                .map(|manifest| manifest.state);
            Some(ActiveRunResponse {
                run_id,
                state: if gate_state == RunState::Cancelling {
                    gate_state
                } else {
                    manifest_state.unwrap_or(gate_state)
                },
            })
        }
        None => None,
    };
    let mut checks = vec![HealthCheck {
        id: "workspace_root".to_owned(),
        status: if settings.writable && settings.path_health.root_marker {
            HealthCheckStatus::Pass
        } else {
            HealthCheckStatus::Fail
        },
        message: if settings.writable && settings.path_health.root_marker {
            "The benchmark workspace is writable and ownership-marked.".to_owned()
        } else {
            "The benchmark workspace failed its writable ownership check.".to_owned()
        },
    }];
    checks.push(HealthCheck {
        id: "execution_backend".to_owned(),
        status: if execution_ready {
            HealthCheckStatus::Pass
        } else {
            HealthCheckStatus::Warning
        },
        message: if execution_ready {
            "The isolated product execution path is ready.".to_owned()
        } else {
            "Artifact browsing remains available, but product execution is not ready.".to_owned()
        },
    });
    let workspace_failed = !settings.writable || !settings.path_health.root_marker;
    Ok(HealthResponse {
        schema_version: API_SCHEMA_VERSION,
        status: if workspace_failed {
            HealthStatus::Unready
        } else if execution_ready {
            HealthStatus::Ready
        } else {
            HealthStatus::Degraded
        },
        execution_ready,
        version: env!("CARGO_PKG_VERSION"),
        runner_instance_id: state.instance_id().to_owned(),
        active_run,
        checks,
    })
}

fn definitions_response(config: &StartupConfig) -> Result<DefinitionsResponse, ApiFailure> {
    let (default, defaults, presets) = load_configuration_data(config)?;
    let profiles =
        load_workspace_profiles(&config.repo.join("benchmark/defaults/workspace-profiles"))
            .map_err(|_| configuration_failure("The workspace-profile data is invalid."))?;
    for scoped in &defaults {
        let expanded =
            validate_and_expand_with_profiles(scoped, &config.paths, &profiles, Some(scoped))
                .map_err(configuration_error)?;
        if !expanded.runnable {
            return Err(configuration_failure(
                "A server-authored default failed canonical validation.",
            ));
        }
    }
    for preset in &presets {
        let Some(declared_default) = defaults
            .iter()
            .find(|candidate| candidate.configuration_base == preset.plan.configuration_base)
        else {
            return Err(configuration_failure(
                "A preset does not target an exact server-authored default.",
            ));
        };
        let expanded = validate_and_expand_with_profiles(
            &preset.plan,
            &config.paths,
            &profiles,
            Some(declared_default),
        )
        .map_err(configuration_error)?;
        if !expanded.runnable {
            return Err(configuration_failure(
                "A versioned preset failed canonical validation.",
            ));
        }
    }
    let _ = default;
    Ok(DefinitionsResponse {
        schema_version: API_SCHEMA_VERSION,
        catalog: definitions::catalog_with_workspace_profiles(profiles),
        defaults,
        presets,
    })
}

fn load_configuration_data(
    config: &StartupConfig,
) -> Result<(ExperimentPlan, Vec<ExperimentPlan>, Vec<PresetFile>), ApiFailure> {
    let default_path = config.repo.join("benchmark/defaults/standard-local.yml");
    let default = load_plan(&default_path).map_err(configuration_error)?;
    if default.configuration_base.scope != ConfigurationScope::All {
        return Err(configuration_failure(
            "The standard local default must have all-family scope.",
        ));
    }
    let defaults = [
        ConfigurationScope::All,
        ConfigurationScope::Command,
        ConfigurationScope::Files,
        ConfigurationScope::Workspace,
        ConfigurationScope::LayerStack,
    ]
    .into_iter()
    .map(|scope| slice_default(&default, scope))
    .collect();
    let presets =
        load_presets(&config.repo.join("benchmark/presets")).map_err(configuration_error)?;
    Ok((default, defaults, presets))
}

fn validate_request_plan_for_config(
    config: &StartupConfig,
    request: &PlanValidationRequest,
    runtime_environment: Option<&RuntimeEnvironmentSnapshot>,
) -> Result<ExpandedPlan, ApiFailure> {
    let (_, defaults, presets) = load_configuration_data(config)?;
    let profiles =
        load_workspace_profiles(&config.repo.join("benchmark/defaults/workspace-profiles"))
            .map_err(|_| configuration_failure("The workspace-profile data is invalid."))?;
    let declared_default = defaults
        .iter()
        .find(|default| default.configuration_base == request.plan.configuration_base);
    let mut expanded = validate_and_expand_with_profiles_and_environment(
        &request.plan,
        &config.paths,
        &profiles,
        declared_default,
        runtime_environment,
    )
    .map_err(configuration_error)?;
    if declared_default.is_none() {
        expanded.runnable = false;
        expanded.validation.push(ValidationFinding {
            severity: FindingSeverity::Error,
            code: "configuration_base_mismatch".to_owned(),
            message:
                "The configuration base must exactly match a server-authored default id, version, and scope."
                    .to_owned(),
            path: Some("configuration_base".to_owned()),
        });
    }
    if let Some(starting_preset) = &request.starting_preset {
        let valid = presets.iter().any(|preset| {
            preset.id == starting_preset.id
                && preset.version == starting_preset.version
                && preset.plan.configuration_base == request.plan.configuration_base
        });
        if !valid {
            expanded.runnable = false;
            expanded.validation.push(ValidationFinding {
                severity: FindingSeverity::Error,
                code: "starting_preset_mismatch".to_owned(),
                message: "The starting preset is unknown or belongs to another default scope."
                    .to_owned(),
                path: Some("starting_preset".to_owned()),
            });
        }
    }
    Ok(expanded)
}

async fn validate_review_request(
    state: &AppState,
    request: &PlanValidationRequest,
) -> Result<ExpandedPlan, ApiFailure> {
    let config = state.config().map_err(internal_app_error)?;
    if !state.execution_ready() {
        return validate_request_plan_for_config(&config, request, None);
    }
    let Some(dependencies) = state.execution_dependencies().map_err(internal_app_error)? else {
        return validate_request_plan_for_config(&config, request, None);
    };
    let runtime = match capture_runtime_environment(&config, &dependencies, &request.plan).await {
        Ok(runtime) => runtime,
        Err(_) => {
            let mut expanded = validate_request_plan_for_config(&config, request, None)?;
            expanded.runnable = false;
            expanded.validation.push(ValidationFinding {
                severity: FindingSeverity::Error,
                code: "effective_environment_unavailable".to_owned(),
                message: "The effective benchmark environment could not be resolved.".to_owned(),
                path: Some("environment".to_owned()),
            });
            return Ok(expanded);
        }
    };
    validate_request_plan_for_config(&config, request, Some(&runtime))
}

async fn create_run(
    state: Arc<AppState>,
    request: RunCreateRequest,
) -> Result<Response<ResponseBody>, ApiFailure> {
    let RunCreateRequest {
        plan,
        plan_hash,
        client_request_id,
        starting_preset,
    } = request;
    let validation_request = PlanValidationRequest {
        plan,
        starting_preset: starting_preset.clone(),
    };
    let config = state.config().map_err(internal_app_error)?;
    let dependencies = state.execution_dependencies().map_err(internal_app_error)?;
    if !state.execution_ready() || dependencies.is_none() {
        let expanded = validate_request_plan_for_config(&config, &validation_request, None)?;
        if !expanded.runnable {
            return Err(ApiFailure {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "plan_not_runnable",
                message: "The plan has blocking validation findings.".to_owned(),
                details: json!({ "validation": expanded.validation }),
            });
        }
        return Err(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "execution_unready",
            "The isolated product execution path is not ready.",
        ));
    }
    let dependencies = dependencies.expect("execution dependency presence checked");
    let store = state.artifacts().map_err(internal_app_error)?;
    // Keep reservation, immutable artifact creation, and registration atomic
    // with respect to cancellation. An idempotent replay therefore cannot
    // expose a cancellable run before its authoritative manifest writer exists.
    let _registration = state.lock_run_registration().await;
    let (run_id, cancellation, replayed) = state
        .campaigns
        .reserve(&client_request_id)
        .map_err(run_reservation_failure)?;

    if replayed {
        let response_state = match read_manifest(&store, &run_id) {
            Ok(manifest) => {
                let persisted = read_expanded(&store, &run_id)?;
                if !constant_time_eq(manifest.plan_hash.as_bytes(), plan_hash.as_bytes())
                    || persisted.canonical_plan != validation_request.plan
                    || manifest.starting_preset != starting_preset
                {
                    return Err(ApiFailure {
                        status: StatusCode::CONFLICT,
                        code: "idempotency_conflict",
                        message:
                            "The client request id already belongs to a different reviewed plan."
                                .to_owned(),
                        details: json!({ "run_id": run_id }),
                    });
                }
                manifest.state
            }
            Err(failure) if failure.status == StatusCode::NOT_FOUND => state
                .campaigns
                .active()
                .map_err(internal_gate_error)?
                .filter(|(active_id, _)| active_id == &run_id)
                .map_or(RunState::Planned, |(_, state)| state),
            Err(failure) => return Err(failure),
        };
        return json_response(
            StatusCode::ACCEPTED,
            &RunCreateResponse {
                schema_version: API_SCHEMA_VERSION,
                run_id,
                state: response_state,
            },
        );
    }

    let captured = match capture_environment(&config, &dependencies, &validation_request.plan).await
    {
        Ok(captured) => captured,
        Err(_) => {
            rollback_reservation(&state, &client_request_id, &run_id)?;
            return Err(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "execution_preparation_failed",
                "The run environment could not be captured from the isolated product path.",
            ));
        }
    };
    let expanded = match validate_request_plan_for_config(
        &config,
        &validation_request,
        Some(&captured.runtime),
    ) {
        Ok(expanded) => expanded,
        Err(error) => {
            rollback_reservation(&state, &client_request_id, &run_id)?;
            return Err(error);
        }
    };
    if !expanded.runnable {
        rollback_reservation(&state, &client_request_id, &run_id)?;
        return Err(ApiFailure {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "plan_not_runnable",
            message: "The plan has blocking validation findings.".to_owned(),
            details: json!({ "validation": expanded.validation }),
        });
    }
    if !constant_time_eq(expanded.plan_hash.as_bytes(), plan_hash.as_bytes()) {
        rollback_reservation(&state, &client_request_id, &run_id)?;
        return Err(failure(
            StatusCode::CONFLICT,
            "plan_hash_mismatch",
            "The plan changed after validation.",
        ));
    }
    let profiles =
        match load_workspace_profiles(&config.repo.join("benchmark/defaults/workspace-profiles")) {
            Ok(profiles) => profiles,
            Err(_) => {
                rollback_reservation(&state, &client_request_id, &run_id)?;
                return Err(configuration_failure(
                    "The workspace-profile data is invalid.",
                ));
            }
        };
    let definition_snapshot = definitions::catalog_with_workspace_profiles(profiles);
    let artifacts = match RunArtifacts::create(
        store,
        &run_id,
        &expanded,
        starting_preset,
        captured.metadata,
        definition_snapshot,
    )
    .await
    {
        Ok(artifacts) => artifacts,
        Err(_) => {
            rollback_reservation(&state, &client_request_id, &run_id)?;
            return Err(failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_creation_failed",
                "The immutable run artifacts could not be created.",
            ));
        }
    };
    if let Err(error) = state.register_run_artifacts(&run_id, Arc::clone(&artifacts)) {
        let message = format!("active run artifacts could not be registered: {error}");
        let _ = fail_campaign(
            Arc::clone(&artifacts),
            "campaign_registration_failed",
            &message,
        )
        .await;
        let _ = state.campaigns.release(&run_id);
        return Err(internal_app_error(error));
    }
    state
        .register_journal(&run_id, Arc::clone(&artifacts.events))
        .await;

    // `planned` covers admission and immutable artifact construction. The run
    // becomes scheduler-visible only after its manifest and journal writers
    // are registered, at which point this persisted transition admits it to
    // the queue. The scheduler alone advances `preparing` and later states.
    let admission: Result<(), SchedulerError> = async {
        artifacts
            .events
            .emit(
                0,
                EventData::RunState {
                    state: RunState::Planned,
                },
            )
            .await?;
        artifacts.transition(RunState::Queued, None)?;
        artifacts
            .events
            .emit(
                0,
                EventData::RunState {
                    state: RunState::Queued,
                },
            )
            .await?;
        Ok(())
    }
    .await;
    if let Err(error) = admission {
        let message = format!("campaign admission could not be persisted: {error}");
        let _ = fail_campaign(
            Arc::clone(&artifacts),
            "campaign_admission_failed",
            &message,
        )
        .await;
        let _ = state.campaigns.release(&run_id);
        let _ = state.forget_run_artifacts(&run_id);
        state.forget_journal(&run_id).await;
        return Err(internal_scheduler_error(error));
    }
    if let Err(error) = state.campaigns.update_state(&run_id, RunState::Queued) {
        let message = format!("campaign gate could not enter queued state: {error}");
        let _ = fail_campaign(Arc::clone(&artifacts), "campaign_gate_failed", &message).await;
        let _ = state.campaigns.release(&run_id);
        let _ = state.forget_run_artifacts(&run_id);
        state.forget_journal(&run_id).await;
        return Err(internal_gate_error(error));
    }

    let task_state = Arc::clone(&state);
    let task_run_id = run_id.clone();
    tokio::spawn(async move {
        let campaign_artifacts = Arc::clone(&artifacts);
        let execution = tokio::spawn(run_campaign(
            config,
            dependencies,
            expanded,
            campaign_artifacts,
            cancellation,
        ))
        .await;
        let terminalization_error = match execution {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(format!(
                "scheduler returned an unrecoverable error: {error}"
            )),
            Err(error) => Some(format!("campaign task did not return normally: {error}")),
        };
        if let Some(message) = terminalization_error {
            if let Err(error) =
                fail_campaign(Arc::clone(&artifacts), "campaign_owner_failed", &message).await
            {
                eprintln!(
                    "sandbox-benchmark run {task_run_id} could not be durably terminalized: {error}"
                );
            }
        }
        if let Err(error) = task_state.campaigns.release(&task_run_id) {
            eprintln!(
                "sandbox-benchmark could not release campaign gate for {task_run_id}: {error}"
            );
        }
        if let Err(error) = task_state.forget_run_artifacts(&task_run_id) {
            eprintln!(
                "sandbox-benchmark could not forget active artifacts for {task_run_id}: {error}"
            );
        }
        task_state.forget_journal(&task_run_id).await;
    });

    json_response(
        StatusCode::ACCEPTED,
        &RunCreateResponse {
            schema_version: API_SCHEMA_VERSION,
            run_id,
            state: RunState::Queued,
        },
    )
}

fn rollback_reservation(
    state: &AppState,
    client_request_id: &str,
    run_id: &str,
) -> Result<(), ApiFailure> {
    state
        .campaigns
        .rollback_reservation(client_request_id, run_id)
        .map_err(internal_gate_error)
}

fn run_reservation_failure(error: CampaignGateError) -> ApiFailure {
    match error {
        CampaignGateError::Busy { run_id } => ApiFailure {
            status: StatusCode::CONFLICT,
            code: "runner_busy",
            message: "Another benchmark campaign is active.".to_owned(),
            details: json!({ "active_run_id": run_id }),
        },
        CampaignGateError::InvalidClientRequestId => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_client_request_id",
            "The client request id must be non-empty and no longer than 128 bytes.",
        ),
        other => internal_gate_error(other),
    }
}

fn list_runs(state: &AppState, query: Option<&str>) -> Result<Response<ResponseBody>, ApiFailure> {
    let store = state.artifacts().map_err(internal_app_error)?;
    let cursor = query_parameter(query, "cursor");
    let mut ids = store.list_run_ids().map_err(artifact_failure)?;
    if let Some(cursor) = cursor {
        ids.retain(|run_id| run_id.as_str() < cursor);
    }
    let has_more = ids.len() > 50;
    ids.truncate(50);
    let mut runs = Vec::with_capacity(ids.len());
    for run_id in ids {
        let manifest = read_manifest(&store, &run_id)?;
        let plan = read_expanded(&store, &run_id)?;
        runs.push(run_summary(&store, &manifest, &plan)?);
    }
    let next_cursor = has_more
        .then(|| runs.last().map(|run| run.run_id.clone()))
        .flatten();
    json_response(
        StatusCode::OK,
        &RunListResponse {
            schema_version: API_SCHEMA_VERSION,
            runs,
            next_cursor,
        },
    )
}

async fn run_response(
    state: &AppState,
    run_id: &str,
) -> Result<Response<ResponseBody>, ApiFailure> {
    let store = state.artifacts().map_err(internal_app_error)?;
    let manifest = read_manifest(&store, run_id)?;
    let expanded = read_expanded(&store, run_id)?;
    let recovered = store
        .read_records_recovering::<EventRecord>(
            run_id,
            ArtifactId::Events,
            EVENT_SCHEMA_NAME,
            EVENT_SCHEMA_VERSION,
        )
        .map_err(artifact_failure)?;
    let latest_sequence = recovered.records.last().map_or(0, |event| event.sequence);
    let report_ready = store
        .index(run_id)
        .map_err(artifact_failure)?
        .iter()
        .any(|entry| entry.id == ArtifactId::Report);
    let progress = project_progress(&expanded, &manifest, &recovered.records);
    let summary = run_summary(&store, &manifest, &expanded)?;
    let response = RunResponse {
        schema_version: API_SCHEMA_VERSION,
        manifest: RunManifestSummary {
            summary,
            definition_snapshot_version: manifest.definition_snapshot.schema_version,
            environment_fingerprint: environment_fingerprint(&manifest)?,
        },
        progress,
        latest_sequence,
        report_ready,
    };
    json_response(StatusCode::OK, &response)
}

fn read_manifest(store: &ArtifactStore, run_id: &str) -> Result<RunManifest, ApiFailure> {
    store
        .read_envelope(
            run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .map_err(artifact_failure)
}

fn read_expanded(store: &ArtifactStore, run_id: &str) -> Result<ExpandedPlan, ApiFailure> {
    store
        .read_envelope(
            run_id,
            ArtifactId::ExpandedPlan,
            EXPANDED_PLAN_SCHEMA_NAME,
            EXPANDED_PLAN_SCHEMA_VERSION,
        )
        .map_err(artifact_failure)
}

fn run_summary(
    store: &ArtifactStore,
    manifest: &RunManifest,
    expanded: &ExpandedPlan,
) -> Result<RunSummary, ApiFailure> {
    if manifest.plan_hash != expanded.plan_hash {
        return Err(failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "artifact_inconsistent",
            "The run manifest and expanded plan hashes do not match.",
        ));
    }
    let has_report = store
        .index(&manifest.run_id)
        .map_err(artifact_failure)?
        .iter()
        .any(|entry| entry.id == ArtifactId::Report);
    let correctness = if has_report {
        match report::read_report(store, &manifest.run_id)
            .map_err(report_failure)?
            .correctness_verdict
        {
            CorrectnessVerdict::Pass => SummaryCorrectness::Pass,
            CorrectnessVerdict::Fail => SummaryCorrectness::Fail,
            CorrectnessVerdict::Pending => SummaryCorrectness::Pending,
        }
    } else {
        SummaryCorrectness::Pending
    };
    Ok(RunSummary {
        run_id: manifest.run_id.clone(),
        name: manifest.name.clone(),
        state: manifest.state,
        plan_hash: manifest.plan_hash.clone(),
        configuration_scope: expanded.canonical_plan.configuration_base.scope,
        source_commit: manifest.treatment.source_commit.clone(),
        source_dirty: manifest.treatment.source_dirty,
        started_at: manifest
            .started_at
            .clone()
            .unwrap_or_else(|| manifest.created_at.clone()),
        ended_at: manifest.ended_at.clone(),
        correctness,
    })
}

fn project_progress(
    expanded: &ExpandedPlan,
    manifest: &RunManifest,
    events: &[EventRecord],
) -> RunProgress {
    let cells = expanded
        .cells
        .iter()
        .map(|cell| (cell.cell_id.as_str(), (cell.family_id, cell.operation_id)))
        .collect::<BTreeMap<_, _>>();
    let mut progress = RunProgress {
        current_family: None,
        current_operation: None,
        current_cell_id: None,
        current_trial_id: None,
        trial_kind: None,
        phase: None,
        completed_trial_batches: 0,
        total_trial_batches: expanded.estimates.trial_batch_count,
        issued_operation_requests: 0,
        warning_count: 0,
        failure_count: 0,
    };
    let mut completed_trials = BTreeSet::new();
    let mut completed_requests = BTreeSet::new();
    for event in events {
        match &event.data {
            EventData::RunState { .. } | EventData::ReportReady { .. } | EventData::Log { .. } => {}
            EventData::FamilyState { family, state } => {
                if work_is_active(*state) {
                    progress.current_family = parse_family_id(family);
                } else if progress.current_family == parse_family_id(family) {
                    progress.current_family = None;
                }
                if *state == WorkState::Failed {
                    progress.failure_count = progress.failure_count.saturating_add(1);
                }
            }
            EventData::CellState { cell_id, state } => {
                if work_is_active(*state) {
                    set_current_cell(&mut progress, &cells, cell_id);
                } else if progress.current_cell_id.as_deref() == Some(cell_id) {
                    clear_current_cell(&mut progress);
                }
                if *state == WorkState::Failed {
                    progress.failure_count = progress.failure_count.saturating_add(1);
                }
            }
            EventData::TrialState {
                cell_id,
                trial_id,
                warmup,
                state,
            } => {
                if work_is_active(*state) {
                    set_current_cell(&mut progress, &cells, cell_id);
                    progress.current_trial_id = Some(trial_id.clone());
                    progress.trial_kind = Some(if *warmup {
                        TrialKind::Warmup
                    } else {
                        TrialKind::Measured
                    });
                } else if trial_is_terminal(*state) {
                    if completed_trials.insert((cell_id.clone(), trial_id.clone())) {
                        progress.completed_trial_batches =
                            progress.completed_trial_batches.saturating_add(1);
                    }
                    if progress.current_trial_id.as_deref() == Some(trial_id) {
                        progress.current_trial_id = None;
                        progress.trial_kind = None;
                        progress.phase = None;
                    }
                }
                if *state == WorkState::Failed {
                    progress.failure_count = progress.failure_count.saturating_add(1);
                }
            }
            EventData::TrialPhase {
                cell_id,
                trial_id,
                warmup,
                phase,
                state,
            } => {
                if work_is_active(*state) {
                    set_current_cell(&mut progress, &cells, cell_id);
                    progress.current_trial_id = Some(trial_id.clone());
                    progress.trial_kind = Some(if *warmup {
                        TrialKind::Warmup
                    } else {
                        TrialKind::Measured
                    });
                    progress.phase = Some(*phase);
                } else if progress.current_trial_id.as_deref() == Some(trial_id)
                    && progress.phase == Some(*phase)
                {
                    progress.phase = None;
                }
            }
            EventData::RequestState {
                cell_id,
                trial_id,
                request_id,
                state,
            } => {
                if request_is_terminal(*state)
                    && completed_requests.insert((
                        cell_id.clone(),
                        trial_id.clone(),
                        request_id.clone(),
                    ))
                {
                    progress.issued_operation_requests =
                        progress.issued_operation_requests.saturating_add(1);
                    if *state == RequestState::Failed {
                        progress.failure_count = progress.failure_count.saturating_add(1);
                    }
                }
            }
            EventData::Correctness { passed, .. } => {
                if !passed {
                    progress.failure_count = progress.failure_count.saturating_add(1);
                }
            }
            EventData::Warning { .. } => {
                progress.warning_count = progress.warning_count.saturating_add(1);
            }
            EventData::ResourceWindow { .. } => {}
        }
    }
    if is_terminal(manifest.state) {
        clear_current_cell(&mut progress);
    }
    progress
}

fn set_current_cell(
    progress: &mut RunProgress,
    cells: &BTreeMap<&str, (crate::model::FamilyId, crate::model::OperationId)>,
    cell_id: &str,
) {
    progress.current_cell_id = Some(cell_id.to_owned());
    if let Some((family, operation)) = cells.get(cell_id) {
        progress.current_family = Some(*family);
        progress.current_operation = Some(*operation);
    }
}

fn clear_current_cell(progress: &mut RunProgress) {
    progress.current_family = None;
    progress.current_operation = None;
    progress.current_cell_id = None;
    progress.current_trial_id = None;
    progress.trial_kind = None;
    progress.phase = None;
}

const fn work_is_active(state: WorkState) -> bool {
    matches!(
        state,
        WorkState::Preparing | WorkState::Running | WorkState::Verifying | WorkState::TearingDown
    )
}

const fn trial_is_terminal(state: WorkState) -> bool {
    matches!(
        state,
        WorkState::Completed | WorkState::Failed | WorkState::Cancelled | WorkState::Skipped
    )
}

const fn request_is_terminal(state: RequestState) -> bool {
    matches!(
        state,
        RequestState::Succeeded | RequestState::Failed | RequestState::Cancelled
    )
}

fn parse_family_id(value: &str) -> Option<crate::model::FamilyId> {
    match value {
        "command" => Some(crate::model::FamilyId::Command),
        "files" => Some(crate::model::FamilyId::Files),
        "workspace_lifecycle" => Some(crate::model::FamilyId::WorkspaceLifecycle),
        "layer_stack" => Some(crate::model::FamilyId::LayerStack),
        _ => None,
    }
}

async fn cancel_run(state: &AppState, run_id: &str) -> Result<Response<ResponseBody>, ApiFailure> {
    let _registration = state.lock_run_registration().await;
    let store = state.artifacts().map_err(internal_app_error)?;
    let active_artifacts = state.run_artifacts(run_id).map_err(internal_app_error)?;
    let manifest = active_artifacts.as_ref().map_or_else(
        || read_manifest(&store, run_id),
        |artifacts| artifacts.manifest().map_err(internal_scheduler_error),
    )?;
    if is_terminal(manifest.state) {
        return json_response(
            StatusCode::OK,
            &RunCancelResponse {
                schema_version: API_SCHEMA_VERSION,
                run_id: run_id.to_owned(),
                state: manifest.state,
                cancellation_requested: false,
            },
        );
    }
    let cancellation = match state.campaigns.cancellation_token(run_id) {
        Ok(cancellation) => cancellation,
        Err(crate::scheduler::CampaignGateError::NotActive(_)) => {
            // Completion can win after the first manifest read but before the
            // cancellation authority is acquired. Preserve idempotent reads
            // of the terminal manifest in that race.
            let latest = active_artifacts.as_ref().map_or_else(
                || read_manifest(&store, run_id),
                |artifacts| artifacts.manifest().map_err(internal_scheduler_error),
            )?;
            if is_terminal(latest.state) {
                return json_response(
                    StatusCode::OK,
                    &RunCancelResponse {
                        schema_version: API_SCHEMA_VERSION,
                        run_id: run_id.to_owned(),
                        state: latest.state,
                        cancellation_requested: false,
                    },
                );
            }
            return Err(failure(
                StatusCode::CONFLICT,
                "run_not_active",
                "The run is persisted but is not active in this runner process.",
            ));
        }
        Err(other) => return Err(internal_gate_error(other)),
    };
    let Some(artifacts) = active_artifacts else {
        return Err(failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "run_authority_unavailable",
            "The active run has no registered manifest authority.",
        ));
    };
    let (persisted, requested) = artifacts
        .request_cancellation(&cancellation)
        .map_err(internal_scheduler_error)?;
    if is_terminal(persisted.state) {
        return json_response(
            StatusCode::OK,
            &RunCancelResponse {
                schema_version: API_SCHEMA_VERSION,
                run_id: run_id.to_owned(),
                state: persisted.state,
                cancellation_requested: false,
            },
        );
    }
    match state.campaigns.update_state(run_id, RunState::Cancelling) {
        Ok(()) => {}
        Err(CampaignGateError::NotActive(_)) => {
            // The owner releases the gate only after durable terminalization.
            // If that happened after the cancellation signal, report the
            // terminal state rather than manufacturing a conflict.
            let latest = artifacts.manifest().map_err(internal_scheduler_error)?;
            if is_terminal(latest.state) {
                return json_response(
                    StatusCode::OK,
                    &RunCancelResponse {
                        schema_version: API_SCHEMA_VERSION,
                        run_id: run_id.to_owned(),
                        state: latest.state,
                        cancellation_requested: false,
                    },
                );
            }
            return Err(failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_authority_inconsistent",
                "The active run gate was released before terminalization.",
            ));
        }
        Err(other) => return Err(internal_gate_error(other)),
    }
    json_response(
        StatusCode::ACCEPTED,
        &RunCancelResponse {
            schema_version: API_SCHEMA_VERSION,
            run_id: run_id.to_owned(),
            state: RunState::Cancelling,
            cancellation_requested: requested,
        },
    )
}

fn report_response(
    state: &AppState,
    run_id: &str,
    query: Option<&str>,
) -> Result<Response<ResponseBody>, ApiFailure> {
    let allow_provisional = match query_parameter(query, "provisional") {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                "The provisional query parameter must be true or false.",
            ));
        }
    };
    let report = report::regenerate(
        &state.artifacts().map_err(internal_app_error)?,
        run_id,
        allow_provisional,
    )
    .map_err(report_failure)?;
    json_response(StatusCode::OK, &report)
}

fn artifact_index(state: &AppState, run_id: &str) -> Result<Response<ResponseBody>, ApiFailure> {
    let entries = state
        .artifacts()
        .map_err(internal_app_error)?
        .index(run_id)
        .map_err(artifact_failure)?;
    let artifacts = entries.into_iter().map(artifact_index_item).collect();
    json_response(
        StatusCode::OK,
        &ArtifactIndexResponse {
            schema_version: API_SCHEMA_VERSION,
            run_id: run_id.to_owned(),
            artifacts,
        },
    )
}

fn artifact_content(
    state: &AppState,
    run_id: &str,
    artifact_id: &str,
) -> Result<Response<ResponseBody>, ApiFailure> {
    let store = state.artifacts().map_err(internal_app_error)?;
    let content = store
        .content(run_id, artifact_id)
        .map_err(artifact_failure)?;
    let index = store
        .index(run_id)
        .map_err(artifact_failure)?
        .into_iter()
        .find(|entry| entry.artifact_id == content.artifact_id)
        .map(artifact_index_item)
        .ok_or_else(|| {
            failure(
                StatusCode::NOT_FOUND,
                "artifact_not_found",
                "The requested allowlisted artifact does not exist.",
            )
        })?;
    let text = String::from_utf8(content.bytes).map_err(|_| {
        failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "artifact_not_text",
            "This artifact cannot be represented as bounded UTF-8 content.",
        )
    })?;
    json_response(
        StatusCode::OK,
        &ArtifactContentResponse {
            schema_version: API_SCHEMA_VERSION,
            index,
            encoding: "utf-8",
            content: text,
        },
    )
}

fn artifact_index_item(entry: crate::artifacts::ArtifactIndexEntry) -> ArtifactIndexItem {
    ArtifactIndexItem {
        artifact_id: entry.artifact_id,
        label: entry.file_name,
        media_type: entry.media_type.to_owned(),
        size_bytes: entry.bytes,
        sha256: entry.sha256,
    }
}

struct SseState {
    queued: VecDeque<EventRecord>,
    receiver: tokio::sync::broadcast::Receiver<EventRecord>,
    heartbeat: tokio::time::Interval,
    last_sequence: u64,
}

async fn sse_response(
    state: &AppState,
    run_id: &str,
    headers: &HeaderMap,
) -> Result<Response<ResponseBody>, ApiFailure> {
    let after = match optional_single_header(headers, LAST_EVENT_ID)? {
        Some(value) => value.parse::<u64>().map_err(|_| {
            failure(
                StatusCode::BAD_REQUEST,
                "invalid_last_event_id",
                "Last-Event-ID must be an unsigned event sequence.",
            )
        })?,
        None => 0,
    };
    let journal = state.journal(run_id).await.map_err(|error| match error {
        crate::app::AppError::Artifact(artifact) => artifact_failure(artifact),
        other => internal_app_error(other),
    })?;

    // Subscribe before replay. Events persisted while replay is read will also
    // appear on the receiver and are de-duplicated by sequence below.
    let receiver = journal.subscribe();
    let replay = journal.replay_after(after).await.map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "event_replay_failed",
            "Persisted events could not be replayed.",
        )
    })?;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let stream_state = SseState {
        queued: replay.into(),
        receiver,
        heartbeat,
        last_sequence: after,
    };
    let stream = stream::unfold(stream_state, |mut state| async move {
        loop {
            if let Some(record) = state.queued.pop_front() {
                if record.sequence <= state.last_sequence {
                    continue;
                }
                state.last_sequence = record.sequence;
                let Ok(encoded) = encode_sse(&record) else {
                    return None;
                };
                return Some((
                    Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from(encoded))),
                    state,
                ));
            }
            tokio::select! {
                received = state.receiver.recv() => {
                    match received {
                        Ok(record) => state.queued.push_back(record),
                        // A lagged client reconnects with its last delivered id;
                        // persisted replay is the lossless source of truth.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
                        | Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    }
                }
                _ = state.heartbeat.tick() => {
                    return Some((
                        Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from_static(b": heartbeat\n\n"))),
                        state,
                    ));
                }
            }
        }
    });
    let body = StreamBody::new(stream).boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store"),
    );
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("keep-alive"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

fn serve_web(
    state: &AppState,
    request_path: &str,
    head: bool,
) -> Result<Response<ResponseBody>, ApiFailure> {
    let config = state.config().map_err(internal_app_error)?;
    let relative = if request_path.starts_with("/assets/") {
        request_path.trim_start_matches('/')
    } else {
        "index.html"
    };
    let path = static_file_path(&config.web_root, relative)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| {
        failure(
            StatusCode::NOT_FOUND,
            "web_asset_not_found",
            "The requested web asset does not exist.",
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(failure(
            StatusCode::NOT_FOUND,
            "web_asset_not_found",
            "The requested web asset does not exist.",
        ));
    }
    if metadata.len() > MAX_STATIC_ASSET_BYTES {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "web_asset_too_large",
            "The requested web asset exceeds the serving limit.",
        ));
    }
    let mut bytes = fs::read(&path).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "web_asset_read_failed",
            "The production web asset could not be read.",
        )
    })?;
    let is_index = relative == "index.html";
    if is_index {
        let html = String::from_utf8(bytes).map_err(|_| {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_bootstrap_html",
                "The production bootstrap document is not UTF-8.",
            )
        })?;
        let marker = "<meta name=\"eos-benchmark-nonce\" content=\"\" />";
        if !html.contains(marker) {
            return Err(failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "missing_nonce_slot",
                "The production bootstrap document is missing its nonce slot.",
            ));
        }
        bytes = html
            .replacen(
                marker,
                &format!(
                    "<meta name=\"eos-benchmark-nonce\" content=\"{}\" />",
                    state.nonce()
                ),
                1,
            )
            .into_bytes();
    }
    let content_type = static_media_type(&path);
    let length = bytes.len();
    let body = if head { Vec::new() } else { bytes };
    let mut response = Response::new(Full::new(Bytes::from(body)).boxed_unsync());
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static(if is_index {
            "no-store"
        } else {
            "public, max-age=31536000, immutable"
        }),
    );
    response.headers_mut().insert(
        "content-length",
        HeaderValue::from_str(&length.to_string()).map_err(|_| {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_content_length",
                "The web asset length could not be represented.",
            )
        })?,
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if is_index {
        response.headers_mut().insert(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
            ),
        );
    }
    Ok(response)
}

fn static_file_path(root: &Path, relative: &str) -> Result<std::path::PathBuf, ApiFailure> {
    let root = root.canonicalize().map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "web_assets_unavailable",
            "The production web assets are not available.",
        )
    })?;
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "invalid_web_asset_path",
            "The web asset path is invalid.",
        ));
    }
    let mut current = root;
    for component in relative_path.components() {
        let Component::Normal(name) = component else {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "invalid_web_asset_path",
                "The web asset path is invalid.",
            ));
        };
        current.push(name);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                return Err(failure(
                    StatusCode::NOT_FOUND,
                    "web_asset_not_found",
                    "Symlinked web assets are not served.",
                ));
            }
        }
    }
    Ok(current)
}

fn static_media_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

async fn read_json<T, B>(headers: &HeaderMap, body: B) -> Result<T, ApiFailure>
where
    T: DeserializeOwned,
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let content_type = required_single_header(headers, CONTENT_TYPE.as_str())?;
    if content_type
        .split(';')
        .next()
        .is_none_or(|media_type| media_type.trim() != "application/json")
    {
        return Err(failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "json_content_type_required",
            "State-changing requests require application/json.",
        ));
    }
    if body
        .size_hint()
        .upper()
        .is_some_and(|upper| upper > MAX_JSON_BODY_BYTES as u64)
    {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_body_too_large",
            "The JSON request body exceeds the one MiB limit.",
        ));
    }
    let collected = Limited::new(body, MAX_JSON_BODY_BYTES)
        .collect()
        .await
        .map_err(|_| {
            failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_body_too_large",
                "The JSON request body exceeds the one MiB limit or could not be read.",
            )
        })?;
    let bytes = collected.to_bytes();
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    let value = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| ApiFailure {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_json",
        message: "The JSON request does not match the endpoint schema.".to_owned(),
        details: json!({ "path": error.path().to_string(), "reason": error.inner().to_string() }),
    })?;
    deserializer.end().map_err(|error| ApiFailure {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_json",
        message: "The JSON request contains trailing data.".to_owned(),
        details: json!({ "reason": error.to_string() }),
    })?;
    Ok(value)
}

fn validate_host(authority: &str, headers: &HeaderMap) -> Result<(), ApiFailure> {
    let host = required_single_header(headers, HOST.as_str())?;
    if !constant_time_eq(host.as_bytes(), authority.as_bytes()) {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "host_mismatch",
            "The Host header does not match the bound loopback authority.",
        ));
    }
    Ok(())
}

fn validate_mutation_headers(state: &AppState, headers: &HeaderMap) -> Result<(), ApiFailure> {
    let origin = required_single_header(headers, ORIGIN.as_str())?;
    if !constant_time_eq(origin.as_bytes(), state.origin().as_bytes()) {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "origin_mismatch",
            "The Origin header does not match this benchmark runner.",
        ));
    }
    let nonce = required_single_header(headers, NONCE_HEADER)?;
    if !constant_time_eq(nonce.as_bytes(), state.nonce().as_bytes()) {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "nonce_mismatch",
            "The benchmark request nonce is missing or invalid.",
        ));
    }
    Ok(())
}

fn required_single_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, ApiFailure> {
    optional_single_header(headers, name)?.ok_or_else(|| {
        failure(
            StatusCode::BAD_REQUEST,
            "required_header_missing",
            &format!("The {name} header is required."),
        )
    })
}

fn optional_single_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<Option<&'a str>, ApiFailure> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "duplicate_header",
            &format!("The {name} header must appear exactly once."),
        ));
    }
    value.to_str().map(Some).map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "invalid_header",
            &format!("The {name} header is not valid text."),
        )
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let maximum = left.len().max(right.len());
    for index in 0..maximum {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn query_parameter<'a>(query: Option<&'a str>, name: &str) -> Option<&'a str> {
    query?.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key == name).then_some(value)
    })
}

fn environment_fingerprint(manifest: &RunManifest) -> Result<String, ApiFailure> {
    let bytes = serde_json::to_vec(&manifest.environment).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "environment_fingerprint_failed",
            "The persisted environment identity could not be fingerprinted.",
        )
    })?;
    Ok(sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn json_response<T: Serialize>(
    status: StatusCode,
    value: &T,
) -> Result<Response<ResponseBody>, ApiFailure> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "response_serialization_failed",
            "The API response could not be serialized.",
        )
    })?;
    Ok(json_bytes_response(status, bytes))
}

fn json_bytes_response(status: StatusCode, bytes: Vec<u8>) -> Response<ResponseBody> {
    let mut response = Response::new(Full::new(Bytes::from(bytes)).boxed_unsync());
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn error_response(failure: ApiFailure, request_id: &str) -> Response<ResponseBody> {
    let envelope = ApiErrorEnvelope {
        error: ApiErrorBody {
            code: failure.code.to_owned(),
            message: failure.message,
            details: failure.details,
            request_id: request_id.to_owned(),
        },
    };
    let bytes = serde_json::to_vec(&envelope).unwrap_or_else(|_| {
        br#"{"error":{"code":"internal_error","message":"The error response could not be serialized.","details":{},"request_id":"unavailable"}}"#.to_vec()
    });
    json_bytes_response(failure.status, bytes)
}

fn failure(status: StatusCode, code: &'static str, message: &str) -> ApiFailure {
    ApiFailure {
        status,
        code,
        message: message.to_owned(),
        details: json!({}),
    }
}

fn internal_app_error(_error: crate::app::AppError) -> ApiFailure {
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "application_state_failed",
        "The benchmark application state is unavailable.",
    )
}

fn internal_gate_error(_error: crate::scheduler::CampaignGateError) -> ApiFailure {
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "campaign_state_failed",
        "The benchmark campaign state is unavailable.",
    )
}

fn internal_scheduler_error(_error: crate::scheduler::SchedulerError) -> ApiFailure {
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "scheduler_state_failed",
        "The durable benchmark scheduler state is unavailable.",
    )
}

fn configuration_failure(message: &str) -> ApiFailure {
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "configuration_invalid",
        message,
    )
}

fn configuration_error(_error: PlanError) -> ApiFailure {
    configuration_failure("The versioned benchmark configuration data is invalid.")
}

fn artifact_failure(error: ArtifactError) -> ApiFailure {
    match error {
        ArtifactError::InvalidRunId(_) | ArtifactError::RunNotFound(_) => failure(
            StatusCode::NOT_FOUND,
            "run_not_found",
            "The requested benchmark run does not exist.",
        ),
        ArtifactError::UnknownArtifact(_) => failure(
            StatusCode::NOT_FOUND,
            "artifact_not_found",
            "The requested artifact id is not allowlisted or does not exist.",
        ),
        ArtifactError::ArtifactTooLarge { .. } => failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "artifact_too_large",
            "The requested artifact exceeds the bounded download limit.",
        ),
        ArtifactError::RunAlreadyExists(_) => failure(
            StatusCode::CONFLICT,
            "run_already_exists",
            "Run artifacts already exist for this identifier.",
        ),
        ArtifactError::InvalidArtifactComponent(_)
        | ArtifactError::ImmutableArtifactConflict(_)
        | ArtifactError::SchemaMismatch { .. }
        | ArtifactError::UnsupportedSchema { .. }
        | ArtifactError::PartialNdjsonTail { .. }
        | ArtifactError::Json { .. }
        | ArtifactError::Io { .. } => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "artifact_read_failed",
            "The persisted benchmark artifact could not be read safely.",
        ),
    }
}

fn report_failure(error: report::ReportError) -> ApiFailure {
    match error {
        report::ReportError::Artifact(artifact) => artifact_failure(artifact),
        report::ReportError::ProvisionalNotAllowed(_) => failure(
            StatusCode::CONFLICT,
            "report_not_final",
            "The run is not terminal; request a provisional report explicitly.",
        ),
        report::ReportError::PlanHashMismatch { .. }
        | report::ReportError::ObservationSequence { .. }
        | report::ReportError::InvalidDefinitionSnapshot(_)
        | report::ReportError::InvalidManifestAuthority(_)
        | report::ReportError::UnknownPersistedMetric(_)
        | report::ReportError::MetricDefinitionMismatch(_)
        | report::ReportError::InvalidArtifactReference { .. } => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "report_evidence_invalid",
            "The persisted evidence cannot produce a valid report.",
        ),
        report::ReportError::Statistics(_) | report::ReportError::Json(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "report_generation_failed",
            "The benchmark report could not be generated.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ResolveInput, WorkspaceRootSource};
    use std::net::SocketAddr;
    use std::path::PathBuf;

    struct TestState {
        state: Arc<AppState>,
        workspace: PathBuf,
    }

    impl Drop for TestState {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.workspace);
        }
    }

    fn test_state() -> TestState {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository should canonicalize");
        let workspace = std::env::temp_dir().join(format!(
            "eos-benchmark-api-test-{}",
            Uuid::new_v4().simple()
        ));
        let bind: SocketAddr = "127.0.0.1:41827".parse().expect("valid bind");
        let (config, _) = StartupConfig::resolve(ResolveInput {
            repo,
            bind,
            web_root: None,
            workspace_override: Some(workspace.clone()),
        })
        .expect("test configuration should resolve");
        let state = AppState::new(
            config,
            WorkspaceRootSource::CommandLine,
            bind.to_string(),
            false,
        )
        .expect("test state should initialize");
        TestState { state, workspace }
    }

    async fn response_json(response: Response<ResponseBody>) -> Value {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("response body is infallible")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("response should be JSON")
    }

    #[tokio::test]
    async fn health_requires_the_exact_bound_host_and_emits_no_cors() {
        let fixture = test_state();
        let good = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/health")
            .header(HOST, "127.0.0.1:41827")
            .body(Full::new(Bytes::new()))
            .expect("request should build");
        let response = handle(Arc::clone(&fixture.state), good).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response
            .headers()
            .contains_key("access-control-allow-origin"));

        let bad = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/health")
            .header(HOST, "localhost:41827")
            .body(Full::new(Bytes::new()))
            .expect("request should build");
        let response = handle(Arc::clone(&fixture.state), bad).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "host_mismatch"
        );
    }

    #[tokio::test]
    async fn mutation_requires_origin_nonce_and_json_before_dispatch() {
        let fixture = test_state();
        let missing = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/compare")
            .header(HOST, fixture.state.authority())
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from_static(b"{}")))
            .expect("request should build");
        let response = handle(Arc::clone(&fixture.state), missing).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let wrong_nonce = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/compare")
            .header(HOST, fixture.state.authority())
            .header(ORIGIN, fixture.state.origin())
            .header(NONCE_HEADER, "not-the-process-nonce")
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from_static(b"{}")))
            .expect("request should build");
        let response = handle(Arc::clone(&fixture.state), wrong_nonce).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "nonce_mismatch"
        );

        let valid_security_headers = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/compare")
            .header(HOST, fixture.state.authority())
            .header(ORIGIN, fixture.state.origin())
            .header(NONCE_HEADER, fixture.state.nonce())
            .header(CONTENT_TYPE, "text/plain")
            .body(Full::new(Bytes::from_static(b"{}")))
            .expect("request should build");
        let response = handle(Arc::clone(&fixture.state), valid_security_headers).await;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn definitions_are_server_authored_and_each_default_is_runnable() {
        let fixture = test_state();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/definitions")
            .header(HOST, fixture.state.authority())
            .body(Full::new(Bytes::new()))
            .expect("request should build");
        let response = handle(Arc::clone(&fixture.state), request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = response_json(response).await;
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["defaults"].as_array().map(Vec::len), Some(5));
        assert_eq!(
            value["catalog"]["operations"].as_array().map(Vec::len),
            Some(7)
        );
        assert_eq!(
            value["catalog"]["workspace_profiles"]["profiles"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
    }

    #[test]
    fn comparison_for_secrets_checks_content_and_length() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"diff"));
        assert!(!constant_time_eq(b"same", b"same-longer"));
    }
}
