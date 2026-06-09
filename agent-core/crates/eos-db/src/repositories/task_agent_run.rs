//! `SqlTaskAgentRunStore` — task-agent-run lineage repository.

use async_trait::async_trait;
use sqlx::{Sqlite, SqlitePool};
use time::OffsetDateTime;

use eos_types::{
    format_record_dir, parented_task_id, AgentName, AgentRunId, AgentRunRecordDir,
    AgentRunRecordIndex, AgentRunRecordTarget, CoreError, CreatedTaskAgentRun,
    ParentAgentRunAnchor, ParentedAgentRunKind, ParentedOutcome, ParentedRun, PlanId, RequestId,
    RunningRequestAgentRun, Sealed, TaskAgentRunKind, TaskAgentRunStore, TaskExecutionIndex,
    TaskId, TaskOutcome, TaskRole, TaskRun, TaskStatus, ToolUseId, WorkItemId, WorkflowCoordinates,
    WorkflowTaskRole,
};

use crate::error::DbError;
use crate::json_col;
use crate::rows::{enum_to_db, parse_enum, parse_id};

/// `SQLite` repository for task-agent-run lineage rows.
#[derive(Debug)]
pub struct SqlTaskAgentRunStore {
    pool: SqlitePool,
}

impl SqlTaskAgentRunStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Sealed for SqlTaskAgentRunStore {}

#[async_trait]
impl TaskAgentRunStore for SqlTaskAgentRunStore {
    async fn create_root_task_agent_run(
        &self,
        request_id: &RequestId,
        agent_run_id: &AgentRunId,
        agent_name: &AgentName,
    ) -> Result<CreatedTaskAgentRun, CoreError> {
        let task_id = TaskId::new_v4();
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(DbError::from)?;
        sqlx::query(
            "INSERT INTO task_runs \
             (task_id, agent_run_id, request_id, role, status, agent_name, terminal_payload, \
              task_outcome, token_count, error, created_at, \
              updated_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, NULL, 0, NULL, ?, ?, NULL)",
        )
        .bind(task_id.as_str())
        .bind(agent_run_id.as_str())
        .bind(request_id.as_str())
        .bind(enum_to_db(&TaskRole::Root))
        .bind(enum_to_db(&TaskStatus::Running))
        .bind(agent_name.as_str())
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(DbError::from)?;
        let updated =
            sqlx::query("UPDATE requests SET root_task_id = ?, updated_at = ? WHERE id = ?")
                .bind(task_id.as_str())
                .bind(now)
                .bind(request_id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(DbError::from)?;
        if updated.rows_affected() == 0 {
            return Err(DbError::NotFound {
                table: "requests",
                id: request_id.to_string(),
            }
            .into());
        }
        tx.commit().await.map_err(DbError::from)?;
        let index = AgentRunRecordIndex {
            request_id: request_id.clone(),
            agent_run_id: agent_run_id.clone(),
            task_id,
            kind: TaskAgentRunKind::Root,
            parent_record_dir: None,
        };
        Ok(created_from_index(&index))
    }

    async fn create_workflow_task_agent_run(
        &self,
        request_id: &RequestId,
        agent_run_id: &AgentRunId,
        coords: &WorkflowCoordinates,
        role: WorkflowTaskRole,
        _plan_id: &PlanId,
        _work_item_id: Option<&WorkItemId>,
        agent_name: &AgentName,
    ) -> Result<CreatedTaskAgentRun, CoreError> {
        let task_id = TaskId::new_v4();
        let now = OffsetDateTime::now_utc();
        let task_role = task_role_from_workflow_role(role);
        sqlx::query(
            "INSERT INTO task_runs \
             (task_id, agent_run_id, request_id, role, status, agent_name, terminal_payload, \
             task_outcome, token_count, error, created_at, \
             updated_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, NULL, 0, NULL, ?, ?, NULL)",
        )
        .bind(task_id.as_str())
        .bind(agent_run_id.as_str())
        .bind(request_id.as_str())
        .bind(enum_to_db(&task_role))
        .bind(enum_to_db(&TaskStatus::Running))
        .bind(agent_name.as_str())
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(DbError::from)?;
        let index = AgentRunRecordIndex {
            request_id: request_id.clone(),
            agent_run_id: agent_run_id.clone(),
            task_id,
            kind: TaskAgentRunKind::Workflow {
                workflow: coords.clone(),
                role,
            },
            parent_record_dir: None,
        };
        Ok(created_from_index(&index))
    }

    async fn create_parented_task_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        parent: &ParentAgentRunAnchor,
        kind: ParentedAgentRunKind,
        tool_use_id: Option<&ToolUseId>,
        agent_name: &AgentName,
    ) -> Result<CreatedTaskAgentRun, CoreError> {
        let task_id = parented_task_id(&parent.agent_run_id, kind, tool_use_id)?;
        let parent_index = resolved_record_index(&self.pool, &parent.agent_run_id)
            .await?
            .ok_or_else(|| DbError::NotFound {
                table: "task_agent_runs",
                id: parent.agent_run_id.to_string(),
            })?;
        validate_parent_anchor(parent, &parent_index)?;
        let parent_record_dir = format_record_dir(&parent_index);
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            "INSERT INTO parented_runs \
             (task_id, agent_run_id, request_id, status, parent_agent_run_id, parent_task_id, \
              kind, tool_use_id, agent_name, terminal_payload, parented_outcome, token_count, \
              error, created_at, \
              updated_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, 0, NULL, ?, ?, NULL)",
        )
        .bind(task_id.as_str())
        .bind(agent_run_id.as_str())
        .bind(parent.request_id.as_str())
        .bind(enum_to_db(&TaskStatus::Running))
        .bind(parent.agent_run_id.as_str())
        .bind(parent.parent_task_id.as_str())
        .bind(kind.as_str())
        .bind(tool_use_id.map(ToolUseId::as_str))
        .bind(agent_name.as_str())
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(DbError::from)?;
        let index = AgentRunRecordIndex {
            request_id: parent.request_id.clone(),
            agent_run_id: agent_run_id.clone(),
            task_id,
            kind: TaskAgentRunKind::Parented {
                parent_agent_run_id: parent.agent_run_id.clone(),
                kind,
            },
            parent_record_dir: Some(parent_record_dir),
        };
        Ok(created_from_index(&index))
    }

    async fn finish_task_run(
        &self,
        agent_run_id: &AgentRunId,
        status: TaskStatus,
        terminal_payload: Option<&eos_types::JsonObject>,
        task_outcome: Option<&TaskOutcome>,
        token_count: i64,
        error: Option<&str>,
    ) -> Result<Option<TaskRun>, CoreError> {
        let now = OffsetDateTime::now_utc();
        let terminal = terminal_payload.map(json_col::encode).transpose()?;
        let outcome = task_outcome.map(json_col::encode).transpose()?;
        let row = sqlx::query_as::<Sqlite, TaskRunRow>(
            "UPDATE task_runs SET status = ?, terminal_payload = COALESCE(?, terminal_payload), \
             task_outcome = COALESCE(?, task_outcome), \
             token_count = ?, error = ?, updated_at = ?, finished_at = ? \
             WHERE agent_run_id = ? RETURNING *",
        )
        .bind(enum_to_db(&status))
        .bind(terminal)
        .bind(outcome)
        .bind(token_count)
        .bind(error)
        .bind(now)
        .bind(now)
        .bind(agent_run_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?;
        row.map(row_to_task_run).transpose().map_err(Into::into)
    }

    async fn finish_parented_run(
        &self,
        agent_run_id: &AgentRunId,
        status: TaskStatus,
        terminal_payload: Option<&eos_types::JsonObject>,
        parented_outcome: Option<&ParentedOutcome>,
        token_count: i64,
        error: Option<&str>,
    ) -> Result<Option<ParentedRun>, CoreError> {
        let now = OffsetDateTime::now_utc();
        let terminal = terminal_payload.map(json_col::encode).transpose()?;
        let outcome = parented_outcome.map(json_col::encode).transpose()?;
        let row = sqlx::query_as::<Sqlite, ParentedRunRow>(
            "UPDATE parented_runs SET status = ?, terminal_payload = COALESCE(?, terminal_payload), \
             parented_outcome = COALESCE(?, parented_outcome), \
             token_count = ?, error = ?, updated_at = ?, finished_at = ? \
             WHERE agent_run_id = ? RETURNING *",
        )
        .bind(enum_to_db(&status))
        .bind(terminal)
        .bind(outcome)
        .bind(token_count)
        .bind(error)
        .bind(now)
        .bind(now)
        .bind(agent_run_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?;
        row.map(row_to_parented_run).transpose().map_err(Into::into)
    }

    async fn record_index_for_agent_run(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<Option<AgentRunRecordIndex>, CoreError> {
        resolved_record_index(&self.pool, agent_run_id)
            .await
            .map_err(Into::into)
    }

    async fn get_task_run(&self, task_id: &TaskId) -> Result<Option<TaskRun>, CoreError> {
        let row = sqlx::query_as::<Sqlite, TaskRunRow>("SELECT * FROM task_runs WHERE task_id = ?")
            .bind(task_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(DbError::from)?;
        row.map(row_to_task_run).transpose().map_err(Into::into)
    }

    async fn list_task_runs_for_request(
        &self,
        request_id: &RequestId,
    ) -> Result<Vec<TaskRun>, CoreError> {
        let rows = sqlx::query_as::<Sqlite, TaskRunRow>(
            "SELECT * FROM task_runs WHERE request_id = ? ORDER BY created_at ASC, task_id ASC",
        )
        .bind(request_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(DbError::from)?;
        rows.into_iter()
            .map(row_to_task_run)
            .collect::<Result<Vec<_>, DbError>>()
            .map_err(Into::into)
    }

    async fn list_running_agent_runs_for_request(
        &self,
        request_id: &RequestId,
    ) -> Result<Vec<RunningRequestAgentRun>, CoreError> {
        let rows = sqlx::query_as::<Sqlite, RunningRequestAgentRunRow>(
            "SELECT request_id, task_id, agent_run_id, status FROM task_runs \
             WHERE request_id = ? AND status = 'running' \
             UNION ALL \
             SELECT request_id, task_id, agent_run_id, status FROM parented_runs \
             WHERE request_id = ? AND status = 'running' \
             ORDER BY task_id ASC, agent_run_id ASC",
        )
        .bind(request_id.as_str())
        .bind(request_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(DbError::from)?;
        rows.iter()
            .map(row_to_running_request_agent_run)
            .collect::<Result<Vec<_>, DbError>>()
            .map_err(Into::into)
    }

    async fn list_parented_runs_for_parent_task(
        &self,
        parent_task_id: &TaskId,
        kind: ParentedAgentRunKind,
    ) -> Result<Vec<ParentedRun>, CoreError> {
        let rows = sqlx::query_as::<Sqlite, ParentedRunRow>(
            "SELECT * FROM parented_runs WHERE parent_task_id = ? AND kind = ? \
             ORDER BY created_at ASC, agent_run_id ASC",
        )
        .bind(parent_task_id.as_str())
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(DbError::from)?;
        rows.into_iter()
            .map(row_to_parented_run)
            .collect::<Result<Vec<_>, DbError>>()
            .map_err(Into::into)
    }

    async fn task_execution_index(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<TaskExecutionIndex>, CoreError> {
        let Some(agent_run_id) = sqlx::query_scalar::<Sqlite, String>(
            "SELECT agent_run_id FROM task_runs WHERE task_id = ?",
        )
        .bind(task_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?
        else {
            return Ok(None);
        };
        let workflow_ids = id_list(
            &self.pool,
            "SELECT id FROM workflows WHERE parent_task_id = ? ORDER BY created_at ASC, id ASC",
            task_id,
        )
        .await?;
        let subagent_ids =
            parented_ids(&self.pool, task_id, ParentedAgentRunKind::Subagent).await?;
        let advisor_ids = parented_ids(&self.pool, task_id, ParentedAgentRunKind::Advisor).await?;
        Ok(Some(TaskExecutionIndex {
            task_id: task_id.clone(),
            agent_run_id: parse_id("task_runs.agent_run_id", &agent_run_id)?,
            workflow_ids,
            subagent_ids,
            advisor_ids,
        }))
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct TaskRunRow {
    task_id: String,
    agent_run_id: String,
    request_id: String,
    role: String,
    status: String,
    agent_name: String,
    terminal_payload: Option<String>,
    task_outcome: Option<String>,
    token_count: i64,
    error: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    finished_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ParentedRunRecordIndexRow {
    task_id: String,
    agent_run_id: String,
    request_id: String,
    parent_agent_run_id: String,
    kind: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ParentedRunRow {
    task_id: String,
    agent_run_id: String,
    request_id: String,
    status: String,
    parent_agent_run_id: String,
    parent_task_id: String,
    kind: String,
    tool_use_id: Option<String>,
    agent_name: String,
    terminal_payload: Option<String>,
    parented_outcome: Option<String>,
    token_count: i64,
    error: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    finished_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RunningRequestAgentRunRow {
    request_id: String,
    task_id: String,
    agent_run_id: String,
    status: String,
}

fn created_from_index(index: &AgentRunRecordIndex) -> CreatedTaskAgentRun {
    CreatedTaskAgentRun {
        agent_run_id: index.agent_run_id.clone(),
        task_id: index.task_id.clone(),
        record_target: AgentRunRecordTarget {
            request_id: index.request_id.clone(),
            agent_run_id: index.agent_run_id.clone(),
            task_id: index.task_id.clone(),
            task_agent_run_kind: index.kind.clone(),
            record_dir: format_record_dir(index),
        },
    }
}

fn row_to_running_request_agent_run(
    row: &RunningRequestAgentRunRow,
) -> Result<RunningRequestAgentRun, DbError> {
    Ok(RunningRequestAgentRun {
        request_id: parse_id("running_request_agent_runs.request_id", &row.request_id)?,
        task_id: parse_id("running_request_agent_runs.task_id", &row.task_id)?,
        agent_run_id: parse_id("running_request_agent_runs.agent_run_id", &row.agent_run_id)?,
        status: parse_enum("running_request_agent_runs.status", &row.status)?,
    })
}

fn validate_parent_anchor(
    parent: &ParentAgentRunAnchor,
    parent_index: &AgentRunRecordIndex,
) -> Result<(), DbError> {
    if parent_index.request_id.as_str() != parent.request_id.as_str() {
        return Err(DbError::InvalidEnum {
            field: "parent_agent_run_anchor.request_id",
            value: parent.request_id.to_string(),
        });
    }
    if parent_index.task_id.as_str() != parent.parent_task_id.as_str() {
        return Err(DbError::InvalidEnum {
            field: "parent_agent_run_anchor.parent_task_id",
            value: parent.parent_task_id.to_string(),
        });
    }
    Ok(())
}

fn row_to_task_run(row: TaskRunRow) -> Result<TaskRun, DbError> {
    Ok(TaskRun {
        task_id: parse_id("task_runs.task_id", &row.task_id)?,
        agent_run_id: parse_id("task_runs.agent_run_id", &row.agent_run_id)?,
        request_id: parse_id("task_runs.request_id", &row.request_id)?,
        role: parse_enum("task_runs.role", &row.role)?,
        status: parse_enum("task_runs.status", &row.status)?,
        agent_name: AgentName::new(&row.agent_name).map_err(|_| DbError::InvalidEnum {
            field: "task_runs.agent_name",
            value: row.agent_name.clone(),
        })?,
        terminal_payload: json_col::decode_opt(row.terminal_payload.as_deref())?,
        task_outcome: json_col::decode_opt(row.task_outcome.as_deref())?,
        token_count: row.token_count,
        error: row.error,
        created_at: eos_types::UtcDateTime::from_offset(row.created_at),
        updated_at: eos_types::UtcDateTime::from_offset(row.updated_at),
        finished_at: row.finished_at.map(eos_types::UtcDateTime::from_offset),
    })
}

fn row_to_parented_run(row: ParentedRunRow) -> Result<ParentedRun, DbError> {
    Ok(ParentedRun {
        task_id: parse_id("parented_runs.task_id", &row.task_id)?,
        agent_run_id: parse_id("parented_runs.agent_run_id", &row.agent_run_id)?,
        request_id: parse_id("parented_runs.request_id", &row.request_id)?,
        status: parse_enum("parented_runs.status", &row.status)?,
        parent_agent_run_id: parse_id(
            "parented_runs.parent_agent_run_id",
            &row.parent_agent_run_id,
        )?,
        parent_task_id: parse_id("parented_runs.parent_task_id", &row.parent_task_id)?,
        kind: parse_parented_kind(&row.kind)?,
        tool_use_id: row
            .tool_use_id
            .as_deref()
            .map(|id| parse_id("parented_runs.tool_use_id", id))
            .transpose()?,
        agent_name: AgentName::new(&row.agent_name).map_err(|_| DbError::InvalidEnum {
            field: "parented_runs.agent_name",
            value: row.agent_name.clone(),
        })?,
        terminal_payload: json_col::decode_opt(row.terminal_payload.as_deref())?,
        parented_outcome: json_col::decode_opt(row.parented_outcome.as_deref())?,
        token_count: row.token_count,
        error: row.error,
        created_at: eos_types::UtcDateTime::from_offset(row.created_at),
        updated_at: eos_types::UtcDateTime::from_offset(row.updated_at),
        finished_at: row.finished_at.map(eos_types::UtcDateTime::from_offset),
    })
}

fn task_run_record_index(row: &TaskRunRow) -> Result<AgentRunRecordIndex, DbError> {
    let task_id = parse_id("task_runs.task_id", &row.task_id)?;
    let request_id = parse_id("task_runs.request_id", &row.request_id)?;
    let agent_run_id = parse_id("task_runs.agent_run_id", &row.agent_run_id)?;
    let role = parse_enum::<TaskRole>("task_runs.role", &row.role)?;
    let kind = match role {
        TaskRole::Root => TaskAgentRunKind::Root,
        TaskRole::Planner | TaskRole::Worker => TaskAgentRunKind::Root,
    };
    Ok(AgentRunRecordIndex {
        request_id,
        agent_run_id,
        task_id,
        kind,
        parent_record_dir: None,
    })
}

fn parented_run_record_index(
    row: &ParentedRunRecordIndexRow,
    parent_record_dir: AgentRunRecordDir,
) -> Result<AgentRunRecordIndex, DbError> {
    let kind = parse_parented_kind(&row.kind)?;
    Ok(AgentRunRecordIndex {
        request_id: parse_id("parented_runs.request_id", &row.request_id)?,
        agent_run_id: parse_id("parented_runs.agent_run_id", &row.agent_run_id)?,
        task_id: parse_id("parented_runs.task_id", &row.task_id)?,
        kind: TaskAgentRunKind::Parented {
            parent_agent_run_id: parse_id(
                "parented_runs.parent_agent_run_id",
                &row.parent_agent_run_id,
            )?,
            kind,
        },
        parent_record_dir: Some(parent_record_dir),
    })
}

async fn resolved_record_index(
    pool: &SqlitePool,
    agent_run_id: &AgentRunId,
) -> Result<Option<AgentRunRecordIndex>, DbError> {
    let mut current = agent_run_id.clone();
    let mut parented_chain = Vec::new();
    for _ in 0..64 {
        if let Some(row) =
            sqlx::query_as::<Sqlite, TaskRunRow>("SELECT * FROM task_runs WHERE agent_run_id = ?")
                .bind(current.as_str())
                .fetch_optional(pool)
                .await
                .map_err(DbError::from)?
        {
            let mut index = task_run_record_index(&row)?;
            while let Some(parented) = parented_chain.pop() {
                let parent_record_dir = format_record_dir(&index);
                index = parented_run_record_index(&parented, parent_record_dir)?;
            }
            return Ok(Some(index));
        }

        let Some(row) = sqlx::query_as::<Sqlite, ParentedRunRecordIndexRow>(
            "SELECT task_id, agent_run_id, request_id, parent_agent_run_id, kind \
             FROM parented_runs WHERE agent_run_id = ?",
        )
        .bind(current.as_str())
        .fetch_optional(pool)
        .await
        .map_err(DbError::from)?
        else {
            return Ok(None);
        };
        current = parse_id(
            "parented_runs.parent_agent_run_id",
            &row.parent_agent_run_id,
        )?;
        parented_chain.push(row);
    }
    Err(DbError::InvalidEnum {
        field: "parented_runs.parent_agent_run_id",
        value: "record lineage exceeded max depth".to_owned(),
    })
}

fn task_role_from_workflow_role(role: WorkflowTaskRole) -> TaskRole {
    match role {
        WorkflowTaskRole::Planner => TaskRole::Planner,
        WorkflowTaskRole::Worker => TaskRole::Worker,
    }
}

fn parse_parented_kind(raw: &str) -> Result<ParentedAgentRunKind, DbError> {
    match raw {
        "subagent" => Ok(ParentedAgentRunKind::Subagent),
        "advisor" => Ok(ParentedAgentRunKind::Advisor),
        other => Err(DbError::InvalidEnum {
            field: "parented_runs.kind",
            value: other.to_owned(),
        }),
    }
}

async fn id_list<T>(pool: &SqlitePool, sql: &str, task_id: &TaskId) -> Result<Vec<T>, DbError>
where
    T: std::str::FromStr<Err = CoreError>,
{
    let rows = sqlx::query_scalar::<Sqlite, String>(sql)
        .bind(task_id.as_str())
        .fetch_all(pool)
        .await
        .map_err(DbError::from)?;
    rows.into_iter()
        .map(|raw| parse_id("lineage.id", &raw))
        .collect()
}

async fn parented_ids(
    pool: &SqlitePool,
    task_id: &TaskId,
    kind: ParentedAgentRunKind,
) -> Result<Vec<AgentRunId>, DbError> {
    let rows = sqlx::query_scalar::<Sqlite, String>(
        "SELECT agent_run_id FROM parented_runs \
         WHERE parent_task_id = ? AND kind = ? ORDER BY created_at ASC, agent_run_id ASC",
    )
    .bind(task_id.as_str())
    .bind(kind.as_str())
    .fetch_all(pool)
    .await
    .map_err(DbError::from)?;
    rows.into_iter()
        .map(|raw| parse_id("parented_runs.agent_run_id", &raw))
        .collect()
}
