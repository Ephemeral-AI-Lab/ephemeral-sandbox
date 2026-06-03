//! Submission terminal tools: `submit_root_outcome`, `submit_generator_outcome`,
//! `submit_reducer_outcome`, `submit_planner_outcome`, `submit_advisor_feedback`,
//! `submit_exploration_result`.
//!
//! `eos-tools` owns the DTOs, names, intent, terminal flag, descriptors, and the
//! **pure** structural validation (GC-tools-01). `submit_root_outcome` is the
//! clean case — it uses `TaskStore`/`RequestStore` directly. The orchestrator-
//! coupled planner/generator/reducer executors call [`PlanSubmissionPort`]
//! (implemented by `eos-workflow`); the downstream-state checks (planner-task
//! ownership, unknown-agent, DAG cycle, task persistence) live there.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use eos_state::{
    GeneratorSubmission, PlannerKind, ReducerSubmission, TaskOutcomeStatus, TaskRole, TaskStatus,
};
use eos_types::JsonObject;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::ToolError;
use crate::execution::parse_input;
use crate::executor::ToolExecutor;
use crate::metadata::ExecutionMetadata;
use crate::name::ToolName;
use crate::ports::{PlanReducer, PlanTask, PlannerPlan, SubmissionAck};
use crate::registry::ToolRegistry;
use crate::result::{OutputShape, ToolResult};
use crate::spec::text_spec;

/// `Literal["success", "failed"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SubmissionStatus {
    Success,
    Failed,
}

impl SubmissionStatus {
    fn as_str(self) -> &'static str {
        match self {
            SubmissionStatus::Success => "success",
            SubmissionStatus::Failed => "failed",
        }
    }
    fn outcome_status(self) -> TaskOutcomeStatus {
        match self {
            SubmissionStatus::Success => TaskOutcomeStatus::Success,
            SubmissionStatus::Failed => TaskOutcomeStatus::Failed,
        }
    }
}

/// `Literal["approve", "reject"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Approve,
    Reject,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Approve => "approve",
            Verdict::Reject => "reject",
        }
    }
}

fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

fn meta_obj(pairs: &[(&str, Value)]) -> JsonObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// submit_root_outcome — pure TaskStore/RequestStore path (§8.7).
// ---------------------------------------------------------------------------

const SUBMIT_ROOT_DESCRIPTION: &str = "Terminate the root request with SUCCESS or FAILED.\n\n- `status`: \"success\" when the user request is complete and verified; \"failed\" when it cannot be completed.\n- `outcome`: the user-facing request result (for success) or the concrete blocker (for failure). The outcome is returned to the user.";

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct SubmitRootOutcomeInput {
    status: SubmissionStatus,
    outcome: String,
}

struct SubmitRootOutcome;

#[async_trait]
impl ToolExecutor for SubmitRootOutcome {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: SubmitRootOutcomeInput = match parse_input(ToolName::SubmitRootOutcome, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if is_blank(&parsed.outcome) {
            return Ok(ToolResult::error("outcome must be nonblank"));
        }

        let request_id = ctx.require_request_id()?;
        let task_id = ctx.require_task_id()?;

        let task = match ctx.task_store.get(task_id).await? {
            Some(task) => task,
            None => {
                // Python `f"Root task {task_id!r} was not found."` → single quotes.
                return Ok(ToolResult::error(format!(
                    "Root task '{}' was not found.",
                    task_id.as_str()
                )));
            }
        };
        if task.request_id != *request_id {
            return Ok(ToolResult::error(
                "Root task does not belong to this request.",
            ));
        }
        if task.workflow_id.is_some() {
            return Ok(ToolResult::error(
                "submit_root_outcome is only valid for the root task.",
            ));
        }
        if task.role != TaskRole::Root {
            // Python `f"Task {task_id!r} is not a root task."` → single quotes.
            return Ok(ToolResult::error(format!(
                "Task '{}' is not a root task.",
                task_id.as_str()
            )));
        }

        let task_status = match parsed.status {
            SubmissionStatus::Success => TaskStatus::Done,
            SubmissionStatus::Failed => TaskStatus::Failed,
        };
        let request_status = parsed.status.as_str(); // "success"/"failed" → "done"/"failed"
        let request_status = if parsed.status == SubmissionStatus::Success {
            "done"
        } else {
            request_status
        };
        // The root outcome is recorded in terminal_tool_result (and status); the
        // typed `outcomes` column is left unchanged because root is not an
        // ExecutionRole (eos-state models only Generator|Reducer) — anchor §4.
        let terminal = meta_obj(&[
            ("status", json!(parsed.status.as_str())),
            ("outcome", json!(parsed.outcome)),
        ]);
        ctx.task_store
            .set_task_status(task_id, task_status, None, Some(&terminal))
            .await?;
        ctx.request_store
            .finish_request(request_id, request_status)
            .await?;

        let kind = if parsed.status == SubmissionStatus::Success {
            "root_success"
        } else {
            "root_failure"
        };
        Ok(
            ToolResult::ok(format!("Accepted root {}.", parsed.status.as_str())).with_metadata(
                meta_obj(&[
                    ("submission_kind", json!(kind)),
                    ("request_id", json!(request_id.as_str())),
                    ("task_id", json!(task_id.as_str())),
                ]),
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// submit_generator_outcome / submit_reducer_outcome — PlanSubmissionPort.
// ---------------------------------------------------------------------------

const SUBMIT_GENERATOR_DESCRIPTION: &str = include_str!("descriptions/submit_generator_outcome.md");
const SUBMIT_REDUCER_DESCRIPTION: &str = include_str!("descriptions/submit_reducer_outcome.md");

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct OutcomeInput {
    status: SubmissionStatus,
    outcome: String,
}

struct SubmitGeneratorOutcome;

#[async_trait]
impl ToolExecutor for SubmitGeneratorOutcome {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: OutcomeInput = match parse_input(ToolName::SubmitGeneratorOutcome, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if is_blank(&parsed.outcome) {
            return Ok(ToolResult::error("outcome must be nonblank"));
        }
        let attempt_id = ctx.require_attempt_id()?.clone();
        let task_id = ctx.require_task_id()?.clone();
        let submission = GeneratorSubmission {
            attempt_id,
            task_id: task_id.clone(),
            status: parsed.status.outcome_status(),
            outcome: parsed.outcome.clone(),
            // Normalized from the vestigial Python {"generator_role": "executor"}
            // marker (anchor §4 forbids the `executor` token in persisted state).
            terminal_tool_result: meta_obj(&[("generator_role", json!("generator"))]),
        };
        let ack = ctx
            .require_plan_submission()?
            .submit_generator(submission)
            .await?;
        Ok(submission_ack_result(
            ack,
            &format!("Accepted generator {}.", parsed.status.as_str()),
            &meta_obj(&[
                (
                    "submission_kind",
                    json!(if parsed.status == SubmissionStatus::Success {
                        "generator_success"
                    } else {
                        "generator_failure"
                    }),
                ),
                ("task_id", json!(task_id.as_str())),
                (
                    "attempt_id",
                    json!(ctx.attempt_id.as_ref().map(eos_types::AttemptId::as_str)),
                ),
            ]),
        ))
    }
}

struct SubmitReducerOutcome;

#[async_trait]
impl ToolExecutor for SubmitReducerOutcome {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: OutcomeInput = match parse_input(ToolName::SubmitReducerOutcome, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if is_blank(&parsed.outcome) {
            return Ok(ToolResult::error("outcome must be nonblank"));
        }
        let attempt_id = ctx.require_attempt_id()?.clone();
        let task_id = ctx.require_task_id()?.clone();
        let submission = ReducerSubmission {
            attempt_id,
            task_id: task_id.clone(),
            status: parsed.status.outcome_status(),
            outcome: parsed.outcome.clone(),
            terminal_tool_result: JsonObject::new(),
        };
        let ack = ctx
            .require_plan_submission()?
            .apply_reducer(submission)
            .await?;
        Ok(submission_ack_result(
            ack,
            &format!("Accepted reducer {}.", parsed.status.as_str()),
            &meta_obj(&[
                (
                    "submission_kind",
                    json!(if parsed.status == SubmissionStatus::Success {
                        "reducer_success"
                    } else {
                        "reducer_failure"
                    }),
                ),
                ("task_id", json!(task_id.as_str())),
                (
                    "attempt_id",
                    json!(ctx.attempt_id.as_ref().map(eos_types::AttemptId::as_str)),
                ),
            ]),
        ))
    }
}

fn submission_ack_result(ack: SubmissionAck, success: &str, metadata: &JsonObject) -> ToolResult {
    match ack {
        SubmissionAck::Accepted => ToolResult::ok(success).with_metadata(metadata.clone()),
        SubmissionAck::Rejected(message) => ToolResult::error(message),
    }
}

// ---------------------------------------------------------------------------
// submit_planner_outcome — structural validation + PlanSubmissionPort.
// ---------------------------------------------------------------------------

const SUBMIT_PLANNER_DESCRIPTION: &str = include_str!("descriptions/submit_planner_outcome.md");

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

struct SubmitPlannerOutcome;

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

        let kind = if parsed.deferred_goal_for_next_iteration.is_some() {
            PlannerKind::Defers
        } else {
            PlannerKind::Completes
        };
        let attempt_id = ctx.require_attempt_id()?.clone();
        let planner_task_id = ctx.require_task_id()?.clone();

        let plan = PlannerPlan {
            attempt_id,
            planner_task_id: planner_task_id.clone(),
            kind,
            deferred_goal_for_next_iteration: parsed.deferred_goal_for_next_iteration,
            tasks: parsed
                .tasks
                .into_iter()
                .map(|t| PlanTask {
                    id: t.id,
                    agent_name: t.agent_name,
                    needs: t.needs,
                })
                .collect(),
            task_specs: parsed.task_specs,
            reducers: parsed
                .reducers
                .into_iter()
                .map(|r| PlanReducer {
                    id: r.id,
                    needs: r.needs,
                    prompt: r.prompt,
                })
                .collect(),
        };

        let ack = ctx.require_plan_submission()?.apply_plan(plan).await?;
        Ok(submission_ack_result(
            ack,
            "Accepted planner submission.",
            &meta_obj(&[
                (
                    "submission_kind",
                    json!(match kind {
                        PlannerKind::Defers => "planner_defers",
                        PlannerKind::Completes => "planner_completes",
                    }),
                ),
                ("task_id", json!(planner_task_id.as_str())),
                (
                    "attempt_id",
                    json!(ctx.attempt_id.as_ref().map(eos_types::AttemptId::as_str)),
                ),
            ]),
        ))
    }
}

/// Per-field nonblank / non-empty validation (the input model's `@field_validator`s).
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
        if task.needs.iter().any(|n| is_blank(n)) {
            return Err("needs must be nonblank".to_owned());
        }
    }
    for (key, spec) in &input.task_specs {
        if is_blank(key) {
            return Err("task_specs key must be nonblank".to_owned());
        }
        if is_blank(spec) {
            // Python `validate_nonblank(spec, f"task spec for {key!r}")` → `!r`
            // renders single quotes; match it verbatim (not Rust `{:?}`).
            return Err(format!("task spec for '{key}' must be nonblank"));
        }
    }
    for reducer in &input.reducers {
        if is_blank(&reducer.id) {
            return Err("id must be nonblank".to_owned());
        }
        if reducer.needs.iter().any(|n| is_blank(n)) {
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

/// The pure structural plan checks (`build_planner_submission`, the parts that
/// need no downstream state): duplicate task ids, missing/extra `task_specs`.
fn validate_planner_structure(input: &SubmitPlannerOutcomeInput) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for task in &input.tasks {
        if !seen.insert(task.id.as_str()) {
            // Python `f"Plan contains duplicate task id {task.id!r}."` → single
            // quotes (verbatim contract); Rust `{:?}` would emit double quotes.
            return Err(format!("Plan contains duplicate task id '{}'.", task.id));
        }
    }
    let task_ids: BTreeSet<&str> = input.tasks.iter().map(|t| t.id.as_str()).collect();
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

// ---------------------------------------------------------------------------
// submit_advisor_feedback / submit_exploration_result — helper terminals.
// ---------------------------------------------------------------------------

const SUBMIT_ADVISOR_DESCRIPTION: &str = include_str!("descriptions/submit_advisor_feedback.md");
const SUBMIT_EXPLORATION_DESCRIPTION: &str =
    include_str!("descriptions/submit_exploration_result.md");

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct SubmitAdvisorFeedbackInput {
    verdict: Verdict,
    summary: String,
}

struct SubmitAdvisorFeedback;

#[async_trait]
impl ToolExecutor for SubmitAdvisorFeedback {
    async fn execute(
        &self,
        input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: SubmitAdvisorFeedbackInput =
            match parse_input(ToolName::SubmitAdvisorFeedback, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        if is_blank(&parsed.summary) {
            return Ok(ToolResult::error("summary must be nonblank"));
        }
        Ok(ToolResult::ok(parsed.summary).with_metadata(meta_obj(&[
            ("helper_role", json!("advisor")),
            ("verdict", json!(parsed.verdict.as_str())),
        ])))
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct SubmitExplorationResultInput {
    summary: String,
    #[serde(default)]
    findings: Vec<String>,
    #[serde(default)]
    references: Vec<String>,
}

struct SubmitExplorationResult;

#[async_trait]
impl ToolExecutor for SubmitExplorationResult {
    async fn execute(
        &self,
        input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: SubmitExplorationResultInput =
            match parse_input(ToolName::SubmitExplorationResult, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        if is_blank(&parsed.summary) {
            return Ok(ToolResult::error("summary must be nonblank"));
        }
        Ok(ToolResult::ok(parsed.summary).with_metadata(meta_obj(&[
            ("subagent_role", json!("explorer")),
            ("findings", json!(parsed.findings)),
            ("references", json!(parsed.references)),
        ])))
    }
}

// ---------------------------------------------------------------------------
// Registration (Python make_submission_tools order).
// ---------------------------------------------------------------------------

pub(crate) fn register(registry: &mut ToolRegistry) {
    super::register_tool(
        registry,
        ToolName::SubmitPlannerOutcome,
        text_spec(
            ToolName::SubmitPlannerOutcome,
            SUBMIT_PLANNER_DESCRIPTION,
            schema_for!(SubmitPlannerOutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitPlannerOutcome),
    );
    super::register_tool(
        registry,
        ToolName::SubmitRootOutcome,
        text_spec(
            ToolName::SubmitRootOutcome,
            SUBMIT_ROOT_DESCRIPTION,
            schema_for!(SubmitRootOutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitRootOutcome),
    );
    super::register_tool(
        registry,
        ToolName::SubmitGeneratorOutcome,
        text_spec(
            ToolName::SubmitGeneratorOutcome,
            SUBMIT_GENERATOR_DESCRIPTION,
            schema_for!(OutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitGeneratorOutcome),
    );
    super::register_tool(
        registry,
        ToolName::SubmitReducerOutcome,
        text_spec(
            ToolName::SubmitReducerOutcome,
            SUBMIT_REDUCER_DESCRIPTION,
            schema_for!(OutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitReducerOutcome),
    );
    super::register_tool(
        registry,
        ToolName::SubmitAdvisorFeedback,
        text_spec(
            ToolName::SubmitAdvisorFeedback,
            SUBMIT_ADVISOR_DESCRIPTION,
            schema_for!(SubmitAdvisorFeedbackInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitAdvisorFeedback),
    );
    super::register_tool(
        registry,
        ToolName::SubmitExplorationResult,
        text_spec(
            ToolName::SubmitExplorationResult,
            SUBMIT_EXPLORATION_DESCRIPTION,
            schema_for!(SubmitExplorationResultInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitExplorationResult),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // unwrap permitted in tests (err-no-unwrap-prod)
    use std::sync::Mutex;

    use eos_state::{RequestId, Task};

    use super::*;
    use crate::ports::{PlanSubmissionPort, Sealed};
    use crate::testsupport::{metadata, FakeRequestStore, FakeTaskStore};

    fn obj(pairs: &[(&str, Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn root_task(request_id: &RequestId) -> Task {
        Task {
            id: "root-1".parse().expect("id"),
            request_id: request_id.clone(),
            role: TaskRole::Root,
            instruction: "do the request".to_owned(),
            status: TaskStatus::Running,
            workflow_id: None,
            iteration_id: None,
            attempt_id: None,
            agent_name: Some("root".to_owned()),
            needs: Vec::new(),
            outcomes: Vec::new(),
            terminal_tool_result: None,
        }
    }

    fn root_metadata(
        task_store: Arc<FakeTaskStore>,
        request_store: Arc<FakeRequestStore>,
        request_id: RequestId,
    ) -> ExecutionMetadata {
        let mut ctx = metadata();
        ctx.task_store = task_store;
        ctx.request_store = request_store;
        ctx.request_id = Some(request_id);
        ctx.task_id = Some("root-1".parse().expect("id"));
        ctx
    }

    // AC-tools-10: root accepts a valid outcome and finishes the request;
    // rejects blank outcome and a non-root task.
    #[tokio::test]
    async fn main_role_terminals() {
        let request_id: RequestId = RequestId::new_v4();
        let task_store = Arc::new(FakeTaskStore::new());
        task_store.put(root_task(&request_id));
        let request_store = Arc::new(FakeRequestStore::new());
        let ctx = root_metadata(
            task_store.clone(),
            request_store.clone(),
            request_id.clone(),
        );

        // Valid success.
        let res = SubmitRootOutcome
            .execute(
                &obj(&[("status", json!("success")), ("outcome", json!("all done"))]),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(!res.is_error, "{}", res.output);
        assert_eq!(res.metadata["submission_kind"], json!("root_success"));
        assert_eq!(
            request_store.finished(),
            vec![(request_id.as_str().to_owned(), "done".to_owned())]
        );

        // Blank outcome → rejected (AC-10 blank-outcome).
        let res = SubmitRootOutcome
            .execute(
                &obj(&[("status", json!("success")), ("outcome", json!("   "))]),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(res.output.contains("outcome must be nonblank"));

        // Foreign request → rejected.
        let other = root_metadata(
            task_store.clone(),
            Arc::new(FakeRequestStore::new()),
            RequestId::new_v4(),
        );
        let res = SubmitRootOutcome
            .execute(
                &obj(&[("status", json!("failed")), ("outcome", json!("blocked"))]),
                &other,
            )
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(
            res.output.contains("does not belong to this request"),
            "{}",
            res.output
        );
    }

    // Non-root task → rejected.
    #[tokio::test]
    async fn root_rejects_non_root_task() {
        let request_id = RequestId::new_v4();
        let task_store = Arc::new(FakeTaskStore::new());
        let mut task = root_task(&request_id);
        task.role = TaskRole::Generator;
        task_store.put(task);
        let ctx = root_metadata(task_store, Arc::new(FakeRequestStore::new()), request_id);
        let res = SubmitRootOutcome
            .execute(
                &obj(&[("status", json!("success")), ("outcome", json!("x"))]),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(res.output.contains("is not a root task"), "{}", res.output);
    }

    #[derive(Default)]
    struct FakePlanSubmission {
        plans: Mutex<Vec<PlannerPlan>>,
    }
    impl Sealed for FakePlanSubmission {}
    #[async_trait]
    impl PlanSubmissionPort for FakePlanSubmission {
        async fn apply_plan(&self, plan: PlannerPlan) -> Result<SubmissionAck, ToolError> {
            self.plans.lock().unwrap().push(plan);
            Ok(SubmissionAck::Accepted)
        }
        async fn submit_generator(
            &self,
            _submission: GeneratorSubmission,
        ) -> Result<SubmissionAck, ToolError> {
            Ok(SubmissionAck::Accepted)
        }
        async fn apply_reducer(
            &self,
            _submission: ReducerSubmission,
        ) -> Result<SubmissionAck, ToolError> {
            Ok(SubmissionAck::Accepted)
        }
    }

    fn planner_ctx(port: Arc<FakePlanSubmission>) -> ExecutionMetadata {
        let mut ctx = metadata();
        ctx.attempt_id = Some("attempt-1".parse().expect("id"));
        ctx.task_id = Some("planner-1".parse().expect("id"));
        ctx.plan_submission = Some(port);
        ctx
    }

    fn valid_plan() -> JsonObject {
        obj(&[
            (
                "tasks",
                json!([
                    {"id": "g1", "agent_name": "coder", "needs": []},
                    {"id": "g2", "agent_name": "coder", "needs": ["g1"]},
                ]),
            ),
            ("task_specs", json!({"g1": "do a", "g2": "do b"})),
            (
                "reducers",
                json!([{"id": "r1", "needs": ["g2"], "prompt": "reduce"}]),
            ),
        ])
    }

    // AC-tools-12: planner validates duplicate ids / missing+extra task_specs /
    // deferred nonblank; a valid plan reaches the port with ordered ids.
    #[tokio::test]
    async fn planner_dag() {
        let port = Arc::new(FakePlanSubmission::default());
        let ctx = planner_ctx(port.clone());

        // Valid plan → accepted, port sees the ordered task + reducer ids.
        let res = SubmitPlannerOutcome
            .execute(&valid_plan(), &ctx)
            .await
            .expect("ok");
        assert!(!res.is_error, "{}", res.output);
        assert_eq!(res.metadata["submission_kind"], json!("planner_completes"));
        // Extract under the lock and drop the guard before any later `.await`
        // (await_holding_lock is denied workspace-wide).
        let (count, task_ids, reducer_id) = {
            let plans = port.plans.lock().unwrap();
            (
                plans.len(),
                plans[0]
                    .tasks
                    .iter()
                    .map(|t| t.id.clone())
                    .collect::<Vec<_>>(),
                plans[0].reducers[0].id.clone(),
            )
        };
        assert_eq!(count, 1);
        assert_eq!(task_ids, vec!["g1", "g2"]);
        assert_eq!(reducer_id, "r1");

        // Duplicate task id.
        let mut dup = valid_plan();
        dup.insert(
            "tasks".to_owned(),
            json!([
                {"id": "g1", "agent_name": "coder"},
                {"id": "g1", "agent_name": "coder"},
            ]),
        );
        dup.insert("task_specs".to_owned(), json!({"g1": "x"}));
        dup.insert(
            "reducers".to_owned(),
            json!([{"id": "r1", "needs": ["g1"], "prompt": "p"}]),
        );
        let res = SubmitPlannerOutcome.execute(&dup, &ctx).await.expect("ok");
        assert!(res.is_error);
        assert!(res.output.contains("duplicate task id"), "{}", res.output);

        // Missing task_specs.
        let mut missing = valid_plan();
        missing.insert("task_specs".to_owned(), json!({"g1": "do a"}));
        let res = SubmitPlannerOutcome
            .execute(&missing, &ctx)
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(
            res.output.contains("Missing task_specs for g2"),
            "{}",
            res.output
        );

        // Extra task_specs.
        let mut extra = valid_plan();
        extra.insert(
            "task_specs".to_owned(),
            json!({"g1": "a", "g2": "b", "g9": "c"}),
        );
        let res = SubmitPlannerOutcome
            .execute(&extra, &ctx)
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(res.output.contains("unknown ids g9"), "{}", res.output);

        // Deferred blank.
        let mut deferred = valid_plan();
        deferred.insert("deferred_goal_for_next_iteration".to_owned(), json!("   "));
        let res = SubmitPlannerOutcome
            .execute(&deferred, &ctx)
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(res
            .output
            .contains("deferred_goal_for_next_iteration must be nonblank"));

        // Deferred present (nonblank) → planner_defers.
        let mut defers = valid_plan();
        defers.insert(
            "deferred_goal_for_next_iteration".to_owned(),
            json!("finish item X"),
        );
        let res = SubmitPlannerOutcome
            .execute(&defers, &ctx)
            .await
            .expect("ok");
        assert_eq!(res.metadata["submission_kind"], json!("planner_defers"));
    }
}
