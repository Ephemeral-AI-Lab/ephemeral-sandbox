//! [`ApiError`] — the single error type every handler returns, mapped to a
//! sanitized JSON response.
//!
//! Sanitization is the contract here (AC4): internal failures — store/sqlx
//! errors, agent-core read errors, host teardown failures — collapse to a generic
//! `500` whose body never echoes the source. The source is logged through
//! `tracing`, not returned. Only client-controllable failures (bad input, not
//! found, delete refused) carry a specific, credential-free message.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use eos_agent_core_server::AgentCoreServerError;
use eos_backend_runtime::{DeleteRejection, SandboxManagerError};
use eos_backend_store::StoreError;
use eos_engine::records::AgentRunRecordError;
use eos_types::CoreError;

/// A handler failure with an HTTP status and a client-safe message.
#[derive(Debug)]
pub enum ApiError {
    /// The targeted resource does not exist (`404`).
    NotFound(&'static str),
    /// The request was malformed or violated a v1 constraint (`400`).
    BadRequest(String),
    /// The operation conflicts with current state, e.g. a delete refused while
    /// the sandbox is still referenced (`409`).
    Conflict(String),
    /// An internal failure (`500`). The detail is logged, never returned.
    Internal,
}

/// The JSON error envelope: `{ "error": { "code", "message" } }`.
#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
}

impl ApiError {
    fn parts(self) -> (StatusCode, &'static str, String) {
        match self {
            Self::NotFound(what) => (
                StatusCode::NOT_FOUND,
                "not_found",
                format!("{what} not found"),
            ),
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, "bad_request", msg),
            Self::Conflict(msg) => (StatusCode::CONFLICT, "conflict", msg),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal server error".to_owned(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        (
            status,
            Json(ErrorBody {
                error: ErrorDetail { code, message },
            }),
        )
            .into_response()
    }
}

/// Backend store failures are internal; the sqlx/JSON detail is logged, the
/// client sees only a generic `500` (no SQL text, no column data).
impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        tracing::error!(error = %err, "backend store error");
        Self::Internal
    }
}

/// Agent-core read failures are internal: the backend never surfaces agent-core
/// store detail to API clients.
impl From<CoreError> for ApiError {
    fn from(err: CoreError) -> Self {
        tracing::error!(error = %err, "agent-core state read error");
        Self::Internal
    }
}

/// Agent-core request service errors are mapped at the backend HTTP edge.
impl From<AgentCoreServerError> for ApiError {
    fn from(err: AgentCoreServerError) -> Self {
        match err {
            AgentCoreServerError::UserRequestNotFound(_) => Self::NotFound("user request"),
            AgentCoreServerError::UserRequestAlreadyFinished { .. } => {
                Self::Conflict("user request already finished".to_owned())
            }
            other => {
                tracing::error!(error = %other, "agent-core service error");
                Self::Internal
            }
        }
    }
}

/// Message-record read failures are sanitized like store failures, except the
/// client-controllable lookup/range cases stay specific.
impl From<AgentRunRecordError> for ApiError {
    fn from(err: AgentRunRecordError) -> Self {
        match err {
            AgentRunRecordError::NotFound(_) => Self::NotFound("agent run message record"),
            AgentRunRecordError::OffsetOutOfRange { .. }
            | AgentRunRecordError::UnsafeSegment { .. } => Self::BadRequest(err.to_string()),
            other => {
                tracing::error!(error = %other, "agent message-record error");
                Self::Internal
            }
        }
    }
}

/// Map the sandbox manager's lifecycle errors to client-safe statuses. A refused
/// delete is a `409`; an unknown sandbox is a `404`; everything else (host
/// teardown, provisioning, capacity) is an internal `500` with the detail logged.
impl From<SandboxManagerError> for ApiError {
    fn from(err: SandboxManagerError) -> Self {
        match err {
            SandboxManagerError::DeleteRejected { reason, .. } => {
                let why = match reason {
                    DeleteRejection::Active => "it has active runs",
                    DeleteRejection::Retained => "it is retained",
                };
                Self::Conflict(format!("cannot delete sandbox while {why}"))
            }
            SandboxManagerError::UnknownSandbox(_) => Self::NotFound("sandbox"),
            other => {
                tracing::error!(error = %other, "sandbox manager error");
                Self::Internal
            }
        }
    }
}
