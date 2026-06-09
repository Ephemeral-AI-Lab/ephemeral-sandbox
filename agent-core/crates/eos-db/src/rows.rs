//! Typed SQL row structs (`sqlx::FromRow`) and row → `eos-types` DTO mappers.

use serde_json::Value as JsonValue;
use time::OffsetDateTime;

use eos_types::{
    AgentRun, Attempt, AttemptBudget, AttemptClosure, AttemptExecutionTree, AttemptFailReason,
    AttemptStage, AttemptState, AttemptStatus, CoreError, Iteration, PlanId, Request,
    RequestStatus, Task, TaskOutcome, UtcDateTime, Workflow,
};

use crate::error::DbError;
use crate::json_col;

// ---- typed rows -----------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct RequestRow {
    pub id: String,
    pub cwd: String,
    pub sandbox_id: Option<String>,
    pub request_prompt: String,
    pub root_task_id: Option<String>,
    pub status: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct TaskRow {
    pub id: String,
    pub request_id: String,
    pub role: String,
    pub instruction: String,
    pub status: String,
    pub agent_name: Option<String>,
    pub task_outcome: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct WorkflowRow {
    pub id: String,
    pub request_id: String,
    pub parent_task_id: String,
    pub parent_agent_run_id: String,
    pub tool_use_id: Option<String>,
    pub workflow_goal: String,
    pub status: String,
    pub iteration_ids: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub closed_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct IterationRow {
    pub id: String,
    pub workflow_id: String,
    pub sequence_no: i64,
    pub creation_reason: String,
    pub workflow_goal: String,
    pub iteration_goal: String,
    pub attempt_budget: i64,
    pub status: String,
    pub attempt_ids: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub closed_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct AttemptRow {
    pub id: String,
    pub iteration_id: String,
    pub workflow_id: String,
    pub attempt_sequence_no: i64,
    pub stage: String,
    pub status: String,
    pub plan_id: String,
    pub execution_tree: String,
    pub fail_reason: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub closed_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct AgentRunRow {
    pub id: String,
    pub task_id: Option<String>,
    pub agent_name: String,
    pub terminal_payload: Option<String>,
    pub token_count: i64,
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

// ---- parse helpers --------------------------------------------------------

pub(crate) fn parse_id<T>(field: &'static str, raw: &str) -> Result<T, DbError>
where
    T: std::str::FromStr<Err = CoreError>,
{
    raw.parse().map_err(|_| DbError::InvalidEnum {
        field,
        value: raw.to_owned(),
    })
}

pub(crate) fn opt_id<T>(field: &'static str, raw: Option<&str>) -> Result<Option<T>, DbError>
where
    T: std::str::FromStr<Err = CoreError>,
{
    raw.map(|s| parse_id(field, s)).transpose()
}

pub(crate) fn parse_enum<T: serde::de::DeserializeOwned>(
    field: &'static str,
    raw: &str,
) -> Result<T, DbError> {
    serde_json::from_value(JsonValue::String(raw.to_owned())).map_err(|_| DbError::InvalidEnum {
        field,
        value: raw.to_owned(),
    })
}

/// Serialize a `snake_case` enum to its wire string for binding.
pub(crate) fn enum_to_db<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .expect("status/stage/reason enums serialize to a json string")
}

// ---- row → DTO mappers ----------------------------------------------------

pub(crate) fn row_to_request(r: RequestRow) -> Result<Request, DbError> {
    Ok(Request {
        id: parse_id("requests.id", &r.id)?,
        cwd: r.cwd,
        sandbox_id: opt_id("requests.sandbox_id", r.sandbox_id.as_deref())?,
        request_prompt: r.request_prompt,
        root_task_id: opt_id("requests.root_task_id", r.root_task_id.as_deref())?,
        status: parse_enum::<RequestStatus>("requests.status", &r.status)?,
        created_at: UtcDateTime::from_offset(r.created_at),
        updated_at: UtcDateTime::from_offset(r.updated_at),
        finished_at: r.finished_at.map(UtcDateTime::from_offset),
    })
}

pub(crate) fn row_to_task(r: TaskRow) -> Result<Task, DbError> {
    Ok(Task {
        id: parse_id("tasks.id", &r.id)?,
        request_id: parse_id("tasks.request_id", &r.request_id)?,
        role: parse_enum("tasks.role", &r.role)?,
        instruction: r.instruction,
        status: parse_enum("tasks.status", &r.status)?,
        agent_name: r.agent_name,
        task_outcome: json_col::decode_opt::<TaskOutcome>(r.task_outcome.as_deref())?,
    })
}

pub(crate) fn row_to_workflow(r: WorkflowRow) -> Result<Workflow, DbError> {
    Ok(Workflow {
        id: parse_id("workflows.id", &r.id)?,
        request_id: parse_id("workflows.request_id", &r.request_id)?,
        workflow_goal: r.workflow_goal,
        status: parse_enum("workflows.status", &r.status)?,
        iteration_ids: json_col::decode_default(Some(&r.iteration_ids))?,
        parent_task_id: parse_id("workflows.parent_task_id", &r.parent_task_id)?,
        parent_agent_run_id: parse_id("workflows.parent_agent_run_id", &r.parent_agent_run_id)?,
        tool_use_id: opt_id("workflows.tool_use_id", r.tool_use_id.as_deref())?,
        created_at: UtcDateTime::from_offset(r.created_at),
        updated_at: UtcDateTime::from_offset(r.updated_at),
        closed_at: r.closed_at.map(UtcDateTime::from_offset),
    })
}

pub(crate) fn row_to_iteration(r: IterationRow) -> Result<Iteration, DbError> {
    let attempt_budget =
        AttemptBudget::try_from_i64(r.attempt_budget).map_err(|_| DbError::InvalidEnum {
            field: "iterations.attempt_budget",
            value: r.attempt_budget.to_string(),
        })?;
    Ok(Iteration {
        id: parse_id("iterations.id", &r.id)?,
        workflow_id: parse_id("iterations.workflow_id", &r.workflow_id)?,
        sequence_no: r.sequence_no,
        creation_reason: parse_enum("iterations.creation_reason", &r.creation_reason)?,
        workflow_goal: r.workflow_goal,
        iteration_goal: r.iteration_goal,
        attempt_budget,
        status: parse_enum("iterations.status", &r.status)?,
        attempt_ids: json_col::decode_default(Some(&r.attempt_ids))?,
        created_at: UtcDateTime::from_offset(r.created_at),
        updated_at: UtcDateTime::from_offset(r.updated_at),
        closed_at: r.closed_at.map(UtcDateTime::from_offset),
    })
}

pub(crate) fn row_to_attempt(r: AttemptRow) -> Result<Attempt, DbError> {
    let plan_id = PlanId::new(r.plan_id.clone()).map_err(|_| DbError::InvalidEnum {
        field: "attempts.plan_id",
        value: r.plan_id,
    })?;
    let execution_tree = decode_execution_tree(&r.execution_tree, &plan_id)?;
    let stage = parse_enum::<AttemptStage>("attempts.stage", &r.stage)?;
    let status = parse_enum::<AttemptStatus>("attempts.status", &r.status)?;
    let fail_reason: Option<AttemptFailReason> = r
        .fail_reason
        .as_deref()
        .map(|s| parse_enum("attempts.fail_reason", s))
        .transpose()?;
    let closed_at = r.closed_at.map(UtcDateTime::from_offset);
    let state = attempt_state_from_columns(
        stage,
        status,
        fail_reason,
        closed_at,
        execution_tree.planner_task_id.is_some(),
    )?;
    Ok(Attempt {
        id: parse_id("attempts.id", &r.id)?,
        iteration_id: parse_id("attempts.iteration_id", &r.iteration_id)?,
        workflow_id: parse_id("attempts.workflow_id", &r.workflow_id)?,
        attempt_sequence_no: r.attempt_sequence_no,
        plan_id,
        execution_tree,
        state,
        created_at: UtcDateTime::from_offset(r.created_at),
        updated_at: UtcDateTime::from_offset(r.updated_at),
    })
}

fn decode_execution_tree(raw: &str, plan_id: &PlanId) -> Result<AttemptExecutionTree, DbError> {
    let tree = if raw.trim() == "{}" {
        AttemptExecutionTree::new(plan_id.clone())
    } else {
        serde_json::from_str::<AttemptExecutionTree>(raw).map_err(DbError::JsonDecode)?
    };
    if &tree.plan_id != plan_id {
        return Err(DbError::InvalidEnum {
            field: "attempts.execution_tree.plan_id",
            value: tree.plan_id.to_string(),
        });
    }
    Ok(tree)
}

fn attempt_state_from_columns(
    stage: AttemptStage,
    status: AttemptStatus,
    fail_reason: Option<AttemptFailReason>,
    closed_at: Option<UtcDateTime>,
    planner_started: bool,
) -> Result<AttemptState, DbError> {
    let lifecycle_value = format!("{stage:?}/{status:?}");
    let invalid_lifecycle = || DbError::InvalidEnum {
        field: "attempts.lifecycle",
        value: lifecycle_value.clone(),
    };
    match stage {
        AttemptStage::Plan => {
            if status != AttemptStatus::Running || fail_reason.is_some() || closed_at.is_some() {
                return Err(invalid_lifecycle());
            }
            Ok(AttemptState::Planning {
                started: planner_started,
            })
        }
        AttemptStage::Run => {
            if status != AttemptStatus::Running || fail_reason.is_some() || closed_at.is_some() {
                return Err(invalid_lifecycle());
            }
            Ok(AttemptState::Running)
        }
        AttemptStage::Closed => {
            let closed_at = closed_at.ok_or_else(invalid_lifecycle)?;
            let closure = match status {
                AttemptStatus::Running => return Err(invalid_lifecycle()),
                AttemptStatus::Passed => {
                    if fail_reason.is_some() {
                        return Err(invalid_lifecycle());
                    }
                    AttemptClosure::Passed { closed_at }
                }
                AttemptStatus::Failed => AttemptClosure::Failed {
                    reason: fail_reason.ok_or_else(invalid_lifecycle)?,
                    closed_at,
                },
                AttemptStatus::Cancelled => {
                    if fail_reason.is_some() {
                        return Err(invalid_lifecycle());
                    }
                    AttemptClosure::Cancelled {
                        reason: String::new(),
                        closed_at,
                    }
                }
            };
            Ok(AttemptState::Closed { closure })
        }
    }
}

pub(crate) fn row_to_agent_run(r: AgentRunRow) -> Result<AgentRun, DbError> {
    Ok(AgentRun {
        id: parse_id("agent_runs.id", &r.id)?,
        task_id: r
            .task_id
            .as_deref()
            .map(|task_id| parse_id("agent_runs.task_id", task_id))
            .transpose()?,
        agent_name: r.agent_name,
        terminal_payload: json_col::decode_opt(r.terminal_payload.as_deref())?,
        token_count: r.token_count,
        error: r.error,
        created_at: UtcDateTime::from_offset(r.created_at),
        finished_at: r.finished_at.map(UtcDateTime::from_offset),
    })
}
