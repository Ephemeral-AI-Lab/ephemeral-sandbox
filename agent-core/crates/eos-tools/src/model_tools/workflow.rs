//! Workflow delegation tools: `delegate_workflow`, `check_workflow_status`,
//! `cancel_workflow`. All call the [`WorkflowControlPort`]; the live
//! workflow/outcome state lives downstream, so `status`/`cancel` return
//! already-rendered model-facing text.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{JsonObject, WorkflowId, WorkflowSessionId};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::ToolConfigSet;
use crate::error::ToolError;
use crate::execution::parse_input;
use crate::executor::ToolExecutor;
use crate::metadata::ExecutionMetadata;
use crate::name::ToolName;
use crate::registry::ToolRegistry;
use crate::result::{OutputShape, ToolResult};
use crate::spec::text_spec;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct DelegateWorkflowInput {
    goal: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CheckWorkflowStatusInput {
    workflow_id: WorkflowId,
    #[serde(default)]
    workflow_task_id: Option<WorkflowSessionId>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CancelWorkflowInput {
    workflow_task_id: WorkflowSessionId,
    #[serde(default)]
    reason: String,
}

struct DelegateWorkflow;

#[async_trait]
impl ToolExecutor for DelegateWorkflow {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: DelegateWorkflowInput = match parse_input(ToolName::DelegateWorkflow, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.goal.trim().is_empty() {
            return Ok(ToolResult::error("goal must be nonblank"));
        }
        let task_id = ctx.require_task_id()?;
        let agent_id = ctx.agent_id();
        let control = ctx.require_workflow_control()?;
        let supervisor = ctx.require_background_supervisor()?;

        let outstanding = control.find_outstanding(task_id, &agent_id).await?;
        if let Some(existing) = outstanding.first() {
            let payload = json!({
                "workflow_task_id": existing.workflow_task_id.as_str(),
                "workflow_id": existing.workflow_id.as_str(),
                "status": "running",
                "message": "A delegated workflow is already outstanding for this task. \
                    Use check_workflow_status or cancel_workflow before starting another.",
            });
            // Python returns this short-circuit with `is_error=True`
            // (`delegate_workflow.py:67-81`); the flag is consumed downstream
            // (supervisor/dispatch/audit), so it must be an in-band error.
            return Ok(ToolResult::error(payload.to_string()));
        }

        let started = control.start(task_id, &agent_id, &parsed.goal).await?;
        supervisor
            .register_workflow(task_id, &agent_id, &parsed.goal, &started)
            .await;
        let payload = json!({
            "workflow_task_id": started.workflow_task_id.as_str(),
            "workflow_id": started.workflow_id.as_str(),
            "status": "running",
            "message": format!(
                "Started delegated workflow {}. Use check_workflow_status to inspect progress \
                 or cancel_workflow to stop it.",
                started.workflow_task_id
            ),
        });
        let metadata: JsonObject = [
            ("submission_kind".to_owned(), json!("workflow_delegated")),
            (
                "workflow_task_id".to_owned(),
                json!(started.workflow_task_id.as_str()),
            ),
            (
                "workflow_id".to_owned(),
                json!(started.workflow_id.as_str()),
            ),
            ("task_id".to_owned(), json!(task_id.as_str())),
        ]
        .into_iter()
        .collect();
        Ok(ToolResult::ok(payload.to_string()).with_metadata(metadata))
    }
}

struct CheckWorkflowStatus;

fn empty_workflow_id_error(tool: ToolName, field: &str) -> ToolResult {
    ToolResult::error(format!(
        "Invalid input for {}: {field} must be non-empty. \
         Please retry the tool call with valid arguments.",
        tool.as_str()
    ))
}

#[async_trait]
impl ToolExecutor for CheckWorkflowStatus {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: CheckWorkflowStatusInput =
            match parse_input(ToolName::CheckWorkflowStatus, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        if parsed.workflow_id.as_str().is_empty() {
            return Ok(empty_workflow_id_error(
                ToolName::CheckWorkflowStatus,
                "workflow_id",
            ));
        }
        if parsed
            .workflow_task_id
            .as_ref()
            .is_some_and(|id| id.as_str().is_empty())
        {
            return Ok(empty_workflow_id_error(
                ToolName::CheckWorkflowStatus,
                "workflow_task_id",
            ));
        }
        let output = ctx
            .require_workflow_control()?
            .status(&parsed.workflow_id, parsed.workflow_task_id.as_ref())
            .await?;
        Ok(ToolResult::ok(output))
    }
}

struct CancelWorkflow;

#[async_trait]
impl ToolExecutor for CancelWorkflow {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: CancelWorkflowInput = match parse_input(ToolName::CancelWorkflow, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.workflow_task_id.as_str().is_empty() {
            return Ok(empty_workflow_id_error(
                ToolName::CancelWorkflow,
                "workflow_task_id",
            ));
        }
        let output = ctx
            .require_workflow_control()?
            .cancel(&parsed.workflow_task_id, &parsed.reason)
            .await?;
        if let Some(supervisor) = &ctx.background_supervisor {
            supervisor
                .cancel_workflow_record(&parsed.workflow_task_id, &parsed.reason)
                .await;
        }
        Ok(ToolResult::ok(output))
    }
}

pub(crate) fn register(registry: &mut ToolRegistry, config: &ToolConfigSet) {
    let delegate = config.get(ToolName::DelegateWorkflow);
    super::register_tool(
        registry,
        ToolName::DelegateWorkflow,
        delegate,
        text_spec(
            ToolName::DelegateWorkflow,
            &delegate.description,
            schema_for!(DelegateWorkflowInput),
        ),
        OutputShape::Text,
        Arc::new(DelegateWorkflow),
    );
    let check = config.get(ToolName::CheckWorkflowStatus);
    super::register_tool(
        registry,
        ToolName::CheckWorkflowStatus,
        check,
        text_spec(
            ToolName::CheckWorkflowStatus,
            &check.description,
            schema_for!(CheckWorkflowStatusInput),
        ),
        OutputShape::Text,
        Arc::new(CheckWorkflowStatus),
    );
    let cancel = config.get(ToolName::CancelWorkflow);
    super::register_tool(
        registry,
        ToolName::CancelWorkflow,
        cancel,
        text_spec(
            ToolName::CancelWorkflow,
            &cancel.description,
            schema_for!(CancelWorkflowInput),
        ),
        OutputShape::Text,
        Arc::new(CancelWorkflow),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Mutex;

    use eos_types::{SubagentSessionId, TaskId};

    use crate::ports::{
        BackgroundInflightReport, BackgroundSupervisorPort, OutstandingWorkflow, Sealed,
        SpawnedSubagent, StartedWorkflow, WorkflowControlPort,
    };
    use crate::testsupport::metadata;

    use super::*;

    fn obj(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[derive(Default)]
    struct RecordingSupervisor {
        workflows: Mutex<Vec<String>>,
        cancelled_workflows: Mutex<Vec<String>>,
    }

    impl Sealed for RecordingSupervisor {}

    #[async_trait]
    impl BackgroundSupervisorPort for RecordingSupervisor {
        async fn spawn(
            &self,
            _ctx: &ExecutionMetadata,
            _agent_name: &str,
            _prompt: &str,
        ) -> Result<SpawnedSubagent, ToolError> {
            unreachable!()
        }

        async fn progress(
            &self,
            _subagent_session_id: &SubagentSessionId,
            _last_n_messages: u8,
        ) -> Result<ToolResult, ToolError> {
            unreachable!()
        }

        async fn cancel(
            &self,
            _subagent_session_id: &SubagentSessionId,
            _reason: &str,
        ) -> Result<ToolResult, ToolError> {
            unreachable!()
        }

        async fn inflight_report(&self, _agent_id: &str) -> BackgroundInflightReport {
            BackgroundInflightReport {
                total: 0,
                subagent: 0,
                workflow: 0,
                command_session: 0,
            }
        }

        async fn cancel_subagents_for_agent(&self, agent_id: &str) -> BackgroundInflightReport {
            self.inflight_report(agent_id).await
        }

        async fn register_workflow(
            &self,
            _parent_task_id: &TaskId,
            _agent_id: &str,
            _workflow_goal: &str,
            workflow: &StartedWorkflow,
        ) {
            self.workflows
                .lock()
                .unwrap()
                .push(workflow.workflow_task_id.as_str().to_owned());
        }

        async fn cancel_workflow_record(
            &self,
            workflow_task_id: &WorkflowSessionId,
            _reason: &str,
        ) -> bool {
            self.cancelled_workflows
                .lock()
                .unwrap()
                .push(workflow_task_id.as_str().to_owned());
            true
        }

        async fn cancel_for_parent_exit(
            &self,
            agent_id: &str,
            _workflow_control: Option<Arc<dyn WorkflowControlPort>>,
            _reason: &str,
        ) -> BackgroundInflightReport {
            self.inflight_report(agent_id).await
        }
    }

    /// A `WorkflowControlPort` that always reports one outstanding workflow, to
    /// drive the `delegate_workflow` already-outstanding short-circuit.
    struct OutstandingControl;

    impl crate::ports::Sealed for OutstandingControl {}

    #[async_trait]
    impl WorkflowControlPort for OutstandingControl {
        async fn start(
            &self,
            _parent_task_id: &TaskId,
            _agent_id: &str,
            _workflow_goal: &str,
        ) -> Result<StartedWorkflow, ToolError> {
            unreachable!("outstanding short-circuit returns before start")
        }

        async fn status(
            &self,
            _workflow_id: &WorkflowId,
            _workflow_task_id: Option<&WorkflowSessionId>,
        ) -> Result<String, ToolError> {
            unreachable!()
        }

        async fn cancel(
            &self,
            _workflow_task_id: &WorkflowSessionId,
            _reason: &str,
        ) -> Result<String, ToolError> {
            unreachable!()
        }

        async fn find_outstanding(
            &self,
            _parent_task_id: &TaskId,
            _agent_id: &str,
        ) -> Result<Vec<OutstandingWorkflow>, ToolError> {
            Ok(vec![OutstandingWorkflow {
                workflow_id: WorkflowId::new_v4(),
                workflow_task_id: WorkflowSessionId::new_v4(),
                workflow_goal: "prior goal".to_owned(),
            }])
        }

        async fn workflow_depth(&self, _workflow_id: &WorkflowId) -> Result<u32, ToolError> {
            Ok(1)
        }
    }

    // The already-outstanding short-circuit is an in-band error (`is_error=true`),
    // matching Python `delegate_workflow.py:67-81`; the flag is consumed downstream.
    #[tokio::test]
    async fn delegate_workflow_outstanding_is_error() {
        let mut ctx = metadata();
        ctx.task_id = Some("parent".parse().unwrap());
        ctx.workflow_control = Some(Arc::new(OutstandingControl));
        ctx.background_supervisor = Some(Arc::new(RecordingSupervisor::default()));

        let res = DelegateWorkflow
            .execute(&obj(&[("goal", json!("do something"))]), &ctx)
            .await
            .expect("ok");

        assert!(res.is_error, "outstanding-workflow branch must be is_error");
        assert!(res.output.contains("already outstanding"), "{}", res.output);
    }

    struct StartingControl;

    impl Sealed for StartingControl {}

    #[async_trait]
    impl WorkflowControlPort for StartingControl {
        async fn start(
            &self,
            _parent_task_id: &TaskId,
            _agent_id: &str,
            _workflow_goal: &str,
        ) -> Result<StartedWorkflow, ToolError> {
            Ok(StartedWorkflow {
                workflow_id: WorkflowId::new_v4(),
                workflow_task_id: "wf_1".parse()?,
            })
        }

        async fn status(
            &self,
            _workflow_id: &WorkflowId,
            _workflow_task_id: Option<&WorkflowSessionId>,
        ) -> Result<String, ToolError> {
            unreachable!()
        }

        async fn cancel(
            &self,
            _workflow_task_id: &WorkflowSessionId,
            _reason: &str,
        ) -> Result<String, ToolError> {
            unreachable!()
        }

        async fn find_outstanding(
            &self,
            _parent_task_id: &TaskId,
            _agent_id: &str,
        ) -> Result<Vec<OutstandingWorkflow>, ToolError> {
            Ok(Vec::new())
        }

        async fn workflow_depth(&self, _workflow_id: &WorkflowId) -> Result<u32, ToolError> {
            Ok(1)
        }
    }

    #[tokio::test]
    async fn delegate_workflow_registers_background_record() {
        let supervisor = Arc::new(RecordingSupervisor::default());
        let mut ctx = metadata();
        ctx.task_id = Some("parent".parse().unwrap());
        ctx.workflow_control = Some(Arc::new(StartingControl));
        ctx.background_supervisor = Some(supervisor.clone());

        let res = DelegateWorkflow
            .execute(&obj(&[("goal", json!("do something"))]), &ctx)
            .await
            .expect("ok");

        assert!(!res.is_error, "{res:?}");
        assert_eq!(
            supervisor.workflows.lock().unwrap().as_slice(),
            ["wf_1"],
            "delegate_workflow must register the workflow as background work"
        );
    }

    #[tokio::test]
    async fn workflow_controls_reject_empty_ids() {
        let ctx = metadata();

        for input in [
            obj(&[("workflow_id", json!(""))]),
            obj(&[
                ("workflow_id", json!("workflow-1")),
                ("workflow_task_id", json!("")),
            ]),
        ] {
            let res = CheckWorkflowStatus.execute(&input, &ctx).await.expect("ok");
            assert!(res.is_error);
            assert!(res.output.contains("workflow"), "{}", res.output);
        }

        let cancel = CancelWorkflow
            .execute(&obj(&[("workflow_task_id", json!(""))]), &ctx)
            .await
            .expect("ok");
        assert!(cancel.is_error);
        assert!(cancel.output.contains("workflow_task_id"));
    }
}
