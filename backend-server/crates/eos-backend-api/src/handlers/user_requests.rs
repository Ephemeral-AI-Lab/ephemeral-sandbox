//! `/api/agent-core/requests` routes: create, list, detail, cancel, events.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use eos_agent_core_server::{CancelUserRequestInput, CreateUserRequestInput};
use eos_backend_runtime::resolve_api_status;
use eos_backend_types::{
    BackendRunStatus, CreateUserRequest, CreateUserRequestResponse, EventRecord, PageResult,
    RunMeta, RunRecord, UserRequestDetail,
};
use eos_types::{Page as AgentCorePage, RequestStatus};
use eos_types::{RequestId, UtcDateTime};

use super::{parse_id, Pagination, ValidatedJson};
use crate::error::ApiError;
use crate::router::AppState;

/// Default cancellation reason recorded when a client cancels with no reason.
const DEFAULT_CANCEL_REASON: &str = "cancelled via api request";

/// `POST /api/agent-core/requests` — accept a prompt and launch an agent-core run.
/// Returns `202 { request_id }`. v1 accepts only `sandbox_args.sandbox_id`;
/// unsupported override fields are rejected by `deny_unknown_fields` at
/// deserialize time.
pub async fn create(
    State(state): State<AppState>,
    ValidatedJson(request): ValidatedJson<CreateUserRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let CreateUserRequest {
        prompt,
        sandbox_args,
        client_meta,
    } = request;
    let label = client_meta.as_ref().and_then(|meta| meta.label.clone());
    let client_meta_json = client_meta
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|err| {
            tracing::error!(error = %err, "failed to encode client metadata");
            ApiError::Internal
        })?
        .unwrap_or_else(|| serde_json::json!({}));
    let output = state
        .agent_core
        .create_user_request(CreateUserRequestInput {
            prompt,
            sandbox_id: sandbox_args.and_then(|args| args.sandbox_id),
            client_label: label.clone(),
            client_metadata: client_meta_json.clone(),
        })
        .await?;
    let request_id = output.request_id;
    state
        .run_meta
        .insert(&RunMeta {
            request_id: request_id.clone(),
            status: BackendRunStatus::Running,
            label,
            client_meta: client_meta_json,
            created_at: UtcDateTime::now(),
            finished_at: None,
            cancel_reason: None,
        })
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(CreateUserRequestResponse { request_id }),
    ))
}

/// `GET /api/agent-core/requests` — list agent-core request records, newest
/// first, joined with backend metadata when present.
pub async fn list(
    State(state): State<AppState>,
    Query(pagination): Query<Pagination>,
) -> Result<Json<PageResult<RunRecord>>, ApiError> {
    let backend_page = pagination.page();
    let page = state
        .agent_core
        .list_user_requests(AgentCorePage {
            limit: backend_page.limit,
            offset: backend_page.offset,
        })
        .await?;
    let mut items = Vec::with_capacity(page.items.len());
    for summary in page.items {
        let meta = state.run_meta.get(&summary.request_id).await?;
        let meta = match meta {
            Some(meta) => {
                Some(reconcile(&state, &summary.request_id, meta, Some(summary.status)).await?)
            }
            None => None,
        };
        items.push(run_record_from_summary(summary, meta));
    }
    Ok(Json(PageResult {
        items,
        total: page.total,
        limit: backend_page.limit,
        offset: backend_page.offset,
    }))
}

/// `GET /api/agent-core/requests/{request_id}` — backend lifecycle joined with
/// the agent-core request outcome through `AgentCoreService`. When the
/// backend row is still non-terminal but agent-core has finished, the resolved
/// terminal status is persisted with a CAS guard so the next read is stable
/// (and a concurrent cancellation is never clobbered).
pub async fn detail(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> Result<Json<UserRequestDetail>, ApiError> {
    let request_id: RequestId = parse_id(&request_id, "request")?;
    let detail = state
        .agent_core
        .read_user_request(&request_id)
        .await?
        .ok_or(ApiError::NotFound("user request"))?;
    let agent_status = Some(detail.status);
    let meta = match state.run_meta.get(&request_id).await? {
        Some(meta) => reconcile(&state, &request_id, meta, agent_status).await?,
        None => RunMeta {
            request_id: request_id.clone(),
            status: BackendRunStatus::Running,
            label: None,
            client_meta: serde_json::json!({}),
            created_at: detail.created_at,
            finished_at: detail.finished_at,
            cancel_reason: None,
        },
    };
    let status = resolve_api_status(meta.status, agent_status);
    Ok(Json(UserRequestDetail {
        request_id: meta.request_id,
        status,
        label: meta.label,
        client_meta: meta.client_meta,
        created_at: meta.created_at,
        finished_at: meta.finished_at,
        cancel_reason: meta.cancel_reason,
    }))
}

/// Persist agent-core's terminal outcome onto a still-non-terminal backend row,
/// CAS-guarded. If the CAS matches nothing (a concurrent `DELETE` wrote
/// `cancelled` between the read and the update), re-read the authoritative row so
/// the response never reports `done`/`failed` over a just-written `cancelled`.
async fn reconcile(
    state: &AppState,
    request_id: &RequestId,
    meta: RunMeta,
    agent_status: Option<RequestStatus>,
) -> Result<RunMeta, ApiError> {
    let terminal = match (meta.status, agent_status) {
        (BackendRunStatus::Accepted | BackendRunStatus::Running, Some(RequestStatus::Done)) => {
            BackendRunStatus::Done
        }
        (BackendRunStatus::Accepted | BackendRunStatus::Running, Some(RequestStatus::Failed)) => {
            BackendRunStatus::Failed
        }
        (
            BackendRunStatus::Accepted | BackendRunStatus::Running,
            Some(RequestStatus::Cancelled),
        ) => BackendRunStatus::Cancelled,
        _ => return Ok(meta),
    };
    match state
        .run_meta
        .reconcile_terminal(request_id, terminal, UtcDateTime::now())
        .await?
    {
        Some(updated) => Ok(updated),
        None => state
            .run_meta
            .get(request_id)
            .await?
            .ok_or(ApiError::NotFound("user request")),
    }
}

/// `DELETE /api/agent-core/requests/{request_id}` — request cancellation.
/// `202` when the request was cancelled; `409` when the run already finalized;
/// `404` when no such run exists.
pub async fn cancel(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let request_id: RequestId = parse_id(&request_id, "request")?;
    state
        .agent_core
        .cancel_user_request(CancelUserRequestInput {
            request_id: request_id.clone(),
            reason: DEFAULT_CANCEL_REASON.to_owned(),
        })
        .await?;
    if state
        .run_meta
        .set_status(
            &request_id,
            BackendRunStatus::Cancelled,
            Some(UtcDateTime::now()),
            Some(DEFAULT_CANCEL_REASON),
        )
        .await?
        .is_none()
    {
        tracing::warn!(
            request_id = request_id.as_str(),
            "agent-core request cancelled but backend run_meta row was missing"
        );
    }
    Ok(StatusCode::ACCEPTED)
}

/// `?after_seq=` query for the events replay route.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    after_seq: Option<i64>,
}

/// `GET /api/agent-core/requests/{request_id}/events` — replay persisted
/// milestone events from `event_log` (those with `seq > after_seq`), including
/// any `event_stream_gap` markers so dropped milestones stay visible.
pub async fn events(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<EventRecord>>, ApiError> {
    let request_id: RequestId = parse_id(&request_id, "request")?;
    if state.run_meta.get(&request_id).await?.is_none() {
        return Err(ApiError::NotFound("user request"));
    }
    let events = state
        .event_log
        .list_since(&request_id, query.after_seq.unwrap_or(0))
        .await?;
    Ok(Json(events))
}

fn run_record_from_summary(
    summary: eos_agent_core_server::UserRequestSummary,
    meta: Option<RunMeta>,
) -> RunRecord {
    let Some(meta) = meta else {
        return RunRecord {
            request_id: summary.request_id,
            status: resolve_api_status(BackendRunStatus::Running, Some(summary.status)),
            label: None,
            created_at: summary.created_at,
            finished_at: summary.finished_at,
        };
    };
    RunRecord {
        request_id: meta.request_id,
        status: resolve_api_status(meta.status, Some(summary.status)),
        label: meta.label,
        created_at: meta.created_at,
        finished_at: meta.finished_at,
    }
}
