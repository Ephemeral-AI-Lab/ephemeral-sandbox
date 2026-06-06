use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use eos_state::{DeferredGoal, PlanDisposition, PlanNodeId};
use eos_types::JsonObject;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::ports::{PlanReducer, PlanTask, PlannerPlan};
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;
use crate::tools::AttemptSubmissionService;

use super::super::lib::{is_blank, meta_obj, submission_ack_result};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct PlanTaskInput {
    id: String,
    agent_name: String,
    #[serde(default)]
    needs: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct ReducerInput {
    id: String,
    #[serde(default)]
    needs: Vec<String>,
    prompt: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct SubmitPlannerOutcomeInput {
    tasks: Vec<PlanTaskInput>,
    task_specs: BTreeMap<String, String>,
    reducers: Vec<ReducerInput>,
    #[serde(default)]
    deferred_goal_for_next_iteration: Option<String>,
}

struct SubmitPlannerOutcome {
    service: Option<AttemptSubmissionService>,
}

impl SubmitPlannerOutcome {
    fn new(service: Option<AttemptSubmissionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for SubmitPlannerOutcome {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: SubmitPlannerOutcomeInput =
            match parse_input(ToolName::SubmitPlannerOutcome, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        if let Err(message) = validate_planner_input(&parsed) {
            return Ok(ToolResult::error(message));
        }
        if let Err(message) = validate_planner_structure(&parsed) {
            return Ok(ToolResult::error(message));
        }

        let attempt_id = ctx.require_attempt_id()?.clone();
        let planner_task_id = ctx.require_task_id()?.clone();
        let plan = match planner_plan(parsed, attempt_id, planner_task_id.clone()) {
            Ok(plan) => plan,
            Err(err) => return Ok(ToolResult::error(err.to_string())),
        };
        let submission_kind = plan.disposition.submission_kind_label();

        let ack = self
            .service
            .as_ref()
            .ok_or(ToolError::MissingPort("attempt_submission"))?
            .port
            .apply_plan(plan)
            .await?;
        Ok(submission_ack_result(
            ack,
            "Accepted planner submission.",
            &meta_obj(&[
                ("submission_kind", json!(submission_kind)),
                ("task_id", json!(planner_task_id.as_str())),
                (
                    "attempt_id",
                    json!(ctx.attempt_id.as_ref().map(eos_types::AttemptId::as_str)),
                ),
            ]),
        ))
    }
}

fn planner_plan(
    parsed: SubmitPlannerOutcomeInput,
    attempt_id: eos_types::AttemptId,
    planner_task_id: eos_types::TaskId,
) -> Result<PlannerPlan, eos_types::CoreError> {
    let disposition = PlanDisposition::from_deferred_goal(
        parsed
            .deferred_goal_for_next_iteration
            .map(DeferredGoal::new)
            .transpose()?,
    );
    Ok(PlannerPlan {
        attempt_id,
        planner_task_id,
        disposition,
        tasks: parsed
            .tasks
            .into_iter()
            .map(|task| {
                Ok(PlanTask {
                    id: PlanNodeId::new(task.id)?,
                    agent_name: task.agent_name,
                    needs: task
                        .needs
                        .into_iter()
                        .map(PlanNodeId::new)
                        .collect::<Result<Vec<_>, _>>()?,
                })
            })
            .collect::<Result<Vec<_>, eos_types::CoreError>>()?,
        task_specs: parsed
            .task_specs
            .into_iter()
            .map(|(key, value)| PlanNodeId::new(key).map(|key| (key, value)))
            .collect::<Result<BTreeMap<_, _>, _>>()?,
        reducers: parsed
            .reducers
            .into_iter()
            .map(|reducer| {
                Ok(PlanReducer {
                    id: PlanNodeId::new(reducer.id)?,
                    needs: reducer
                        .needs
                        .into_iter()
                        .map(PlanNodeId::new)
                        .collect::<Result<Vec<_>, _>>()?,
                    prompt: reducer.prompt,
                })
            })
            .collect::<Result<Vec<_>, eos_types::CoreError>>()?,
    })
}

fn validate_planner_input(input: &SubmitPlannerOutcomeInput) -> Result<(), String> {
    if input.tasks.is_empty() {
        return Err("tasks must not be empty".to_owned());
    }
    if input.task_specs.is_empty() {
        return Err("task_specs must not be empty".to_owned());
    }
    if input.reducers.is_empty() {
        return Err("reducers must not be empty".to_owned());
    }
    for task in &input.tasks {
        if is_blank(&task.id) {
            return Err("id must be nonblank".to_owned());
        }
        if is_blank(&task.agent_name) {
            return Err("agent_name must be nonblank".to_owned());
        }
        if task.needs.iter().any(|need| is_blank(need)) {
            return Err("needs must be nonblank".to_owned());
        }
    }
    for (key, spec) in &input.task_specs {
        if is_blank(key) {
            return Err("task_specs key must be nonblank".to_owned());
        }
        if is_blank(spec) {
            return Err(format!("task spec for '{key}' must be nonblank"));
        }
    }
    for reducer in &input.reducers {
        if is_blank(&reducer.id) {
            return Err("id must be nonblank".to_owned());
        }
        if reducer.needs.iter().any(|need| is_blank(need)) {
            return Err("needs must be nonblank".to_owned());
        }
        if is_blank(&reducer.prompt) {
            return Err("prompt must be nonblank".to_owned());
        }
    }
    if let Some(deferred) = &input.deferred_goal_for_next_iteration {
        if is_blank(deferred) {
            return Err("deferred_goal_for_next_iteration must be nonblank".to_owned());
        }
    }
    Ok(())
}

fn validate_planner_structure(input: &SubmitPlannerOutcomeInput) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for task in &input.tasks {
        if !seen.insert(task.id.as_str()) {
            return Err(format!("Plan contains duplicate task id '{}'.", task.id));
        }
    }
    let task_ids: BTreeSet<&str> = input.tasks.iter().map(|task| task.id.as_str()).collect();
    let spec_ids: BTreeSet<&str> = input.task_specs.keys().map(String::as_str).collect();

    let missing: Vec<&str> = task_ids.difference(&spec_ids).copied().collect();
    if !missing.is_empty() {
        return Err(format!("Missing task_specs for {}.", missing.join(", ")));
    }
    let extra: Vec<&str> = spec_ids.difference(&task_ids).copied().collect();
    if !extra.is_empty() {
        return Err(format!(
            "task_specs contains unknown ids {}.",
            extra.join(", ")
        ));
    }
    Ok(())
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    attempt_submission: Option<AttemptSubmissionService>,
) {
    let planner = config.get(ToolName::SubmitPlannerOutcome);
    super::super::super::register_tool(
        registry,
        ToolName::SubmitPlannerOutcome,
        planner,
        text_spec(
            ToolName::SubmitPlannerOutcome,
            &planner.description,
            schema_for!(SubmitPlannerOutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitPlannerOutcome::new(attempt_submission)),
    );
}
