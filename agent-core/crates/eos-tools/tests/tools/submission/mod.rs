#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use eos_state::{
    GeneratorSubmission, PlanDisposition, PlanNodeId, ReducerSubmission, TaskOutcomeStatus,
};
use eos_types::{AttemptId, JsonObject, TaskId};
use serde_json::{json, Value};

use crate::ports::{
    AttemptSubmissionPort, PlanReducer, PlanTask, PlannerPlan, Sealed, SubmissionAck,
};
use crate::support::{metadata, FakeTransport};
use crate::tools::{AttemptSubmissionService, CallerScope, SandboxToolService, SkillToolService};
use crate::{ToolError, ToolName, ToolRegistry};

#[derive(Debug)]
struct RecordingAttemptPort {
    ack: Mutex<SubmissionAck>,
    plans: Mutex<Vec<PlannerPlan>>,
    generators: Mutex<Vec<GeneratorSubmission>>,
    reducers: Mutex<Vec<ReducerSubmission>>,
}

impl RecordingAttemptPort {
    fn accepting() -> Arc<Self> {
        Self::with_ack(SubmissionAck::Accepted)
    }

    fn rejecting(message: &str) -> Arc<Self> {
        Self::with_ack(SubmissionAck::Rejected(message.to_owned()))
    }

    fn with_ack(ack: SubmissionAck) -> Arc<Self> {
        Arc::new(Self {
            ack: Mutex::new(ack),
            plans: Mutex::new(Vec::new()),
            generators: Mutex::new(Vec::new()),
            reducers: Mutex::new(Vec::new()),
        })
    }
}

impl Sealed for RecordingAttemptPort {}

#[async_trait]
impl AttemptSubmissionPort for RecordingAttemptPort {
    async fn apply_plan(&self, plan: PlannerPlan) -> Result<SubmissionAck, ToolError> {
        self.plans.lock().unwrap().push(plan);
        Ok(self.ack.lock().unwrap().clone())
    }

    async fn submit_generator(
        &self,
        submission: GeneratorSubmission,
    ) -> Result<SubmissionAck, ToolError> {
        self.generators.lock().unwrap().push(submission);
        Ok(self.ack.lock().unwrap().clone())
    }

    async fn apply_reducer(
        &self,
        submission: ReducerSubmission,
    ) -> Result<SubmissionAck, ToolError> {
        self.reducers.lock().unwrap().push(submission);
        Ok(self.ack.lock().unwrap().clone())
    }
}

fn obj(pairs: &[(&str, Value)]) -> JsonObject {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn node(id: &str) -> PlanNodeId {
    PlanNodeId::new(id).unwrap()
}

fn registry(port: Option<Arc<dyn AttemptSubmissionPort>>) -> ToolRegistry {
    crate::tools::build_default_registry_with_services(
        &crate::tools::repo_tools_config(),
        &CallerScope::default(),
        SandboxToolService::new(Arc::new(FakeTransport::inert())),
        None,
        port.map(AttemptSubmissionService::new),
        None,
        None,
        None,
        SkillToolService::new(Arc::new(eos_skills::SkillRegistry::new())),
    )
}

fn attempt_ctx() -> crate::ExecutionMetadata {
    let mut ctx = metadata();
    ctx.attempt_id = Some(AttemptId::new_v4());
    ctx.task_id = Some(TaskId::new_v4());
    ctx
}

async fn execute(
    registry: &ToolRegistry,
    name: ToolName,
    input: JsonObject,
    ctx: &crate::ExecutionMetadata,
) -> Result<crate::ToolResult, ToolError> {
    registry
        .get(name)
        .expect("registered")
        .executor()
        .execute(&input, ctx)
        .await
}

fn complete_plan_input() -> JsonObject {
    obj(&[
        (
            "tasks",
            json!([
                {"id": "g1", "agent_name": "coder", "needs": []},
                {"id": "g2", "agent_name": "coder", "needs": ["g1"]},
            ]),
        ),
        (
            "task_specs",
            json!({
                "g1": "implement first piece",
                "g2": "implement second piece",
            }),
        ),
        (
            "reducers",
            json!([
                {"id": "r1", "needs": ["g1", "g2"], "prompt": "combine results"}
            ]),
        ),
    ])
}

#[tokio::test]
async fn submit_planner_outcome_accepts_complete_plan() {
    let port = RecordingAttemptPort::accepting();
    let registry = registry(Some(port.clone()));
    let ctx = attempt_ctx();

    let res = execute(
        &registry,
        ToolName::SubmitPlannerOutcome,
        complete_plan_input(),
        &ctx,
    )
    .await
    .unwrap();

    assert!(!res.is_error, "{}", res.output);
    assert_eq!(res.output, "Accepted planner submission.");
    assert_eq!(res.metadata["submission_kind"], json!("planner_completes"));
    assert_eq!(res.metadata["task_id"], json!(ctx.task_id.as_ref().unwrap().as_str()));
    assert_eq!(
        res.metadata["attempt_id"],
        json!(ctx.attempt_id.as_ref().map(AttemptId::as_str))
    );

    let plans = port.plans.lock().unwrap();
    assert_eq!(plans.len(), 1);
    let plan = &plans[0];
    assert_eq!(plan.attempt_id, ctx.attempt_id.clone().unwrap());
    assert_eq!(plan.planner_task_id, ctx.task_id.clone().unwrap());
    assert_eq!(plan.disposition, PlanDisposition::Complete);
    assert_eq!(
        plan.tasks,
        vec![
            PlanTask {
                id: node("g1"),
                agent_name: "coder".to_owned(),
                needs: Vec::new(),
            },
            PlanTask {
                id: node("g2"),
                agent_name: "coder".to_owned(),
                needs: vec![node("g1")],
            },
        ]
    );
    assert_eq!(plan.task_specs[&node("g1")], "implement first piece");
    assert_eq!(
        plan.reducers,
        vec![PlanReducer {
            id: node("r1"),
            needs: vec![node("g1"), node("g2")],
            prompt: "combine results".to_owned(),
        }]
    );
}

#[tokio::test]
async fn submit_planner_outcome_rejects_blank_ids_specs_and_deferred_goal() {
    let port = RecordingAttemptPort::accepting();
    let registry = registry(Some(port.clone()));
    let ctx = attempt_ctx();

    for input in [
        obj(&[
            ("tasks", json!([{"id": " ", "agent_name": "coder", "needs": []}])),
            ("task_specs", json!({"g1": "do it"})),
            ("reducers", json!([{"id": "r1", "needs": ["g1"], "prompt": "reduce"}])),
        ]),
        obj(&[
            ("tasks", json!([{"id": "g1", "agent_name": " ", "needs": []}])),
            ("task_specs", json!({"g1": "do it"})),
            ("reducers", json!([{"id": "r1", "needs": ["g1"], "prompt": "reduce"}])),
        ]),
        obj(&[
            ("tasks", json!([{"id": "g1", "agent_name": "coder", "needs": [" "]}])),
            ("task_specs", json!({"g1": "do it"})),
            ("reducers", json!([{"id": "r1", "needs": ["g1"], "prompt": "reduce"}])),
        ]),
        obj(&[
            ("tasks", json!([{"id": "g1", "agent_name": "coder", "needs": []}])),
            ("task_specs", json!({"g1": " "})),
            ("reducers", json!([{"id": "r1", "needs": ["g1"], "prompt": "reduce"}])),
        ]),
        obj(&[
            ("tasks", json!([{"id": "g1", "agent_name": "coder", "needs": []}])),
            ("task_specs", json!({"g1": "do it"})),
            ("reducers", json!([{"id": " ", "needs": ["g1"], "prompt": "reduce"}])),
        ]),
        obj(&[
            ("tasks", json!([{"id": "g1", "agent_name": "coder", "needs": []}])),
            ("task_specs", json!({"g1": "do it"})),
            ("reducers", json!([{"id": "r1", "needs": [" "], "prompt": "reduce"}])),
        ]),
        obj(&[
            ("tasks", json!([{"id": "g1", "agent_name": "coder", "needs": []}])),
            ("task_specs", json!({"g1": "do it"})),
            ("reducers", json!([{"id": "r1", "needs": ["g1"], "prompt": " "} ])),
        ]),
        {
            let mut input = complete_plan_input();
            input.insert(
                "deferred_goal_for_next_iteration".to_owned(),
                json!("   "),
            );
            input
        },
    ] {
        let res = execute(&registry, ToolName::SubmitPlannerOutcome, input, &ctx)
            .await
            .unwrap();
        assert!(res.is_error, "{res:?}");
    }
    assert!(port.plans.lock().unwrap().is_empty());
}

#[tokio::test]
async fn submit_planner_outcome_rejects_missing_or_extra_task_specs() {
    let registry = registry(Some(RecordingAttemptPort::accepting()));
    let ctx = attempt_ctx();

    for input in [
        obj(&[
            ("tasks", json!([{"id": "g1", "agent_name": "coder", "needs": []}])),
            ("task_specs", json!({})),
            ("reducers", json!([{"id": "r1", "needs": ["g1"], "prompt": "reduce"}])),
        ]),
        obj(&[
            ("tasks", json!([{"id": "g1", "agent_name": "coder", "needs": []}])),
            ("task_specs", json!({"g1": "do it", "g2": "extra"})),
            ("reducers", json!([{"id": "r1", "needs": ["g1"], "prompt": "reduce"}])),
        ]),
    ] {
        let res = execute(&registry, ToolName::SubmitPlannerOutcome, input, &ctx)
            .await
            .unwrap();
        assert!(res.is_error, "{res:?}");
    }
}

#[tokio::test]
async fn submit_planner_outcome_returns_rejected_ack_without_recording_success() {
    let registry = registry(Some(RecordingAttemptPort::rejecting("plan rejected")));
    let ctx = attempt_ctx();

    let res = execute(
        &registry,
        ToolName::SubmitPlannerOutcome,
        complete_plan_input(),
        &ctx,
    )
    .await
    .unwrap();

    assert!(res.is_error);
    assert_eq!(res.output, "plan rejected");
    assert!(res.metadata.is_empty(), "rejected ack must not carry success metadata");
}

#[tokio::test]
async fn submit_generator_outcome_records_success_and_failure_metadata() {
    let port = RecordingAttemptPort::accepting();
    let registry = registry(Some(port.clone()));
    let ctx = attempt_ctx();

    for (status, expected_kind, expected_status) in [
        (
            "success",
            "generator_success",
            TaskOutcomeStatus::Success,
        ),
        ("failed", "generator_failure", TaskOutcomeStatus::Failed),
    ] {
        let res = execute(
            &registry,
            ToolName::SubmitGeneratorOutcome,
            obj(&[("status", json!(status)), ("outcome", json!("generated"))]),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!res.is_error, "{}", res.output);
        assert_eq!(res.metadata["submission_kind"], json!(expected_kind));
        assert_eq!(res.metadata["task_id"], json!(ctx.task_id.as_ref().unwrap().as_str()));
        assert_eq!(
            port.generators.lock().unwrap().last().unwrap().status,
            expected_status
        );
    }
}

#[tokio::test]
async fn submit_generator_outcome_rejects_blank_outcome() {
    let port = RecordingAttemptPort::accepting();
    let registry = registry(Some(port.clone()));
    let ctx = attempt_ctx();

    let res = execute(
        &registry,
        ToolName::SubmitGeneratorOutcome,
        obj(&[("status", json!("success")), ("outcome", json!("  "))]),
        &ctx,
    )
    .await
    .unwrap();

    assert!(res.is_error);
    assert_eq!(res.output, "outcome must be nonblank");
    assert!(port.generators.lock().unwrap().is_empty());
}

#[tokio::test]
async fn submit_reducer_outcome_records_success_and_failure_metadata() {
    let port = RecordingAttemptPort::accepting();
    let registry = registry(Some(port.clone()));
    let ctx = attempt_ctx();

    for (status, expected_kind, expected_status) in [
        ("success", "reducer_success", TaskOutcomeStatus::Success),
        ("failed", "reducer_failure", TaskOutcomeStatus::Failed),
    ] {
        let res = execute(
            &registry,
            ToolName::SubmitReducerOutcome,
            obj(&[("status", json!(status)), ("outcome", json!("reduced"))]),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!res.is_error, "{}", res.output);
        assert_eq!(res.metadata["submission_kind"], json!(expected_kind));
        assert_eq!(res.metadata["task_id"], json!(ctx.task_id.as_ref().unwrap().as_str()));
        assert_eq!(
            port.reducers.lock().unwrap().last().unwrap().status,
            expected_status
        );
    }
}

#[tokio::test]
async fn submit_reducer_outcome_rejects_blank_outcome() {
    let port = RecordingAttemptPort::accepting();
    let registry = registry(Some(port.clone()));
    let ctx = attempt_ctx();

    let res = execute(
        &registry,
        ToolName::SubmitReducerOutcome,
        obj(&[("status", json!("failed")), ("outcome", json!("  "))]),
        &ctx,
    )
    .await
    .unwrap();

    assert!(res.is_error);
    assert_eq!(res.output, "outcome must be nonblank");
    assert!(port.reducers.lock().unwrap().is_empty());
}

#[tokio::test]
async fn submission_tools_error_when_attempt_metadata_missing() {
    let registry = registry(Some(RecordingAttemptPort::accepting()));

    for tool in [
        ToolName::SubmitPlannerOutcome,
        ToolName::SubmitGeneratorOutcome,
        ToolName::SubmitReducerOutcome,
    ] {
        let mut no_attempt = metadata();
        no_attempt.task_id = Some(TaskId::new_v4());
        let err = execute(&registry, tool, valid_input_for(tool), &no_attempt)
            .await
            .expect_err("missing attempt is a framework fault");
        assert!(matches!(err, ToolError::MissingContext("attempt_id")));

        let mut no_task = metadata();
        no_task.attempt_id = Some(AttemptId::new_v4());
        let err = execute(&registry, tool, valid_input_for(tool), &no_task)
            .await
            .expect_err("missing task is a framework fault");
        assert!(matches!(err, ToolError::MissingContext("task_id")));
    }
}

#[tokio::test]
async fn submission_tools_error_when_attempt_submission_port_missing() {
    let registry = registry(None);
    let ctx = attempt_ctx();

    for tool in [
        ToolName::SubmitPlannerOutcome,
        ToolName::SubmitGeneratorOutcome,
        ToolName::SubmitReducerOutcome,
    ] {
        let err = execute(&registry, tool, valid_input_for(tool), &ctx)
            .await
            .expect_err("missing port is a framework fault");
        assert!(matches!(err, ToolError::MissingPort("attempt_submission")));
    }
}

fn valid_input_for(tool: ToolName) -> JsonObject {
    match tool {
        ToolName::SubmitPlannerOutcome => complete_plan_input(),
        ToolName::SubmitGeneratorOutcome | ToolName::SubmitReducerOutcome => {
            obj(&[("status", json!("success")), ("outcome", json!("done"))])
        }
        other => panic!("unexpected submission tool {other:?}"),
    }
}
