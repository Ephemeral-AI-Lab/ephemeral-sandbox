//! Subagent tools: `run_subagent` (the restricted, caller-scoped dispatch),
//! `check_subagent_progress`, `cancel_subagent`. All call the
//! [`SubagentSupervisorPort`].
//!
//! `run_subagent` is the restricted variant: its `agent_name` input schema is
//! patched per caller with the `enum` of dispatchable subagents (§6.6). The
//! downstream validation (caller is not a subagent, the agent exists and is a
//! subagent) lives in the port implementor (`eos-engine`, which has the agent
//! registry that `eos-tools` deliberately does not depend on).

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{JsonObject, SubagentSessionId};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::CallerScope;
use crate::error::ToolError;
use crate::execution::parse_input;
use crate::executor::ToolExecutor;
use crate::metadata::ExecutionMetadata;
use crate::name::ToolName;
use crate::ports::{SpawnedSubagent, StartedSubagent};
use crate::registry::ToolRegistry;
use crate::result::{OutputShape, ToolResult};
use crate::spec::{text_spec, text_spec_with_agent_enum};

const RUN_SUBAGENT_DESCRIPTION: &str = include_str!("descriptions/run_subagent.md");
const CHECK_DESCRIPTION: &str = "Check a running or finished subagent by subagent_session_id. Returns the latest child-agent message snapshot while running and the terminal result after successful completion.";
const CANCEL_DESCRIPTION: &str = "Cancel a running subagent by subagent_session_id.";

fn default_five() -> u8 {
    5
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct RunSubagentInput {
    /// Name of a registered dispatchable subagent (caller-scoped enum, §6.6).
    agent_name: String,
    prompt: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CheckSubagentProgressInput {
    subagent_session_id: SubagentSessionId,
    // Matches Python `Field(ge=1, le=10)` in both schema and runtime validation.
    #[serde(default = "default_five")]
    #[schemars(default = "default_five", range(min = 1, max = 10))]
    last_n_messages: u8,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CancelSubagentInput {
    subagent_session_id: SubagentSessionId,
    #[serde(default)]
    reason: String,
}

struct RunSubagent;

fn launch_result(agent_name: &str, started: &StartedSubagent) -> ToolResult {
    let session_id = started.subagent_session_id.as_str();
    let mut metadata = JsonObject::new();
    metadata.insert("subagent_session_id".to_owned(), json!(session_id));
    metadata.insert("status".to_owned(), json!("running"));
    metadata.insert("agent_name".to_owned(), json!(agent_name));
    ToolResult::ok(format!(
        "[SUBAGENT LAUNCHED] subagent_session_id=\"{session_id}\" status=running \
         agent_name=\"{agent_name}\"\nUse check_subagent_progress(\
         subagent_session_id=\"{session_id}\", last_n_messages=5) to inspect progress, \
         or cancel_subagent(subagent_session_id=\"{session_id}\") to stop it. \
         Keep using the current response on other ready work first."
    ))
    .with_metadata(metadata)
}

#[async_trait]
impl ToolExecutor for RunSubagent {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: RunSubagentInput = match parse_input(ToolName::RunSubagent, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.agent_name.trim().is_empty() {
            return Ok(ToolResult::error(
                "run_subagent: `agent_name` must be a non-empty string.",
            ));
        }
        if parsed.prompt.trim().is_empty() {
            return Ok(ToolResult::error(
                "run_subagent: `prompt` must be a non-empty string.",
            ));
        }
        match ctx
            .require_subagent_supervisor()?
            .spawn(ctx, &parsed.agent_name, &parsed.prompt)
            .await?
        {
            SpawnedSubagent::Launched(started) => Ok(launch_result(&parsed.agent_name, &started)),
            SpawnedSubagent::Rejected(message) => Ok(ToolResult::error(message)),
        }
    }
}

struct CheckSubagentProgress;

fn empty_subagent_session_error(tool: ToolName) -> ToolResult {
    ToolResult::error(format!(
        "Invalid input for {}: subagent_session_id must be non-empty. \
         Please retry the tool call with valid arguments.",
        tool.as_str()
    ))
}

#[async_trait]
impl ToolExecutor for CheckSubagentProgress {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: CheckSubagentProgressInput =
            match parse_input(ToolName::CheckSubagentProgress, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        if parsed.subagent_session_id.as_str().is_empty() {
            return Ok(empty_subagent_session_error(
                ToolName::CheckSubagentProgress,
            ));
        }
        if !(1..=10).contains(&parsed.last_n_messages) {
            return Ok(ToolResult::error(
                "Invalid input for check_subagent_progress: last_n_messages must be between 1 and 10. \
                 Please retry the tool call with valid arguments.",
            ));
        }
        ctx.require_subagent_supervisor()?
            .progress(&parsed.subagent_session_id, parsed.last_n_messages)
            .await
    }
}

struct CancelSubagent;

#[async_trait]
impl ToolExecutor for CancelSubagent {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: CancelSubagentInput = match parse_input(ToolName::CancelSubagent, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.subagent_session_id.as_str().is_empty() {
            return Ok(empty_subagent_session_error(ToolName::CancelSubagent));
        }
        ctx.require_subagent_supervisor()?
            .cancel(&parsed.subagent_session_id, &parsed.reason)
            .await
    }
}

pub(crate) fn register(registry: &mut ToolRegistry, caller: &CallerScope) {
    super::register_tool(
        registry,
        ToolName::RunSubagent,
        text_spec_with_agent_enum(
            ToolName::RunSubagent,
            RUN_SUBAGENT_DESCRIPTION,
            schema_for!(RunSubagentInput),
            &caller.dispatchable_subagents,
        ),
        OutputShape::Text,
        Arc::new(RunSubagent),
    );
    super::register_tool(
        registry,
        ToolName::CheckSubagentProgress,
        text_spec(
            ToolName::CheckSubagentProgress,
            CHECK_DESCRIPTION,
            schema_for!(CheckSubagentProgressInput),
        ),
        OutputShape::Text,
        Arc::new(CheckSubagentProgress),
    );
    super::register_tool(
        registry,
        ToolName::CancelSubagent,
        text_spec(
            ToolName::CancelSubagent,
            CANCEL_DESCRIPTION,
            schema_for!(CancelSubagentInput),
        ),
        OutputShape::Text,
        Arc::new(CancelSubagent),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Mutex;

    use crate::ports::{BackgroundInflightReport, Sealed, SubagentSupervisorPort};
    use crate::testsupport::metadata;

    use super::*;

    #[derive(Default)]
    struct FakeSubagentSupervisor {
        spawned: Mutex<Vec<(String, String)>>,
    }

    impl Sealed for FakeSubagentSupervisor {}

    #[async_trait]
    impl SubagentSupervisorPort for FakeSubagentSupervisor {
        async fn spawn(
            &self,
            _ctx: &ExecutionMetadata,
            agent_name: &str,
            prompt: &str,
        ) -> Result<SpawnedSubagent, ToolError> {
            self.spawned
                .lock()
                .unwrap()
                .push((agent_name.to_owned(), prompt.to_owned()));
            Ok(SpawnedSubagent::Launched(StartedSubagent {
                subagent_session_id: "subagent_1".parse()?,
            }))
        }

        async fn progress(
            &self,
            _subagent_session_id: &SubagentSessionId,
            _last_n_messages: u8,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::ok("running"))
        }

        async fn cancel(
            &self,
            _subagent_session_id: &SubagentSessionId,
            _reason: &str,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::ok("cancelled"))
        }

        async fn inflight_report(&self, _agent_id: &str) -> BackgroundInflightReport {
            BackgroundInflightReport {
                total: 0,
                subagent: 0,
                workflow: 0,
                command_session: 0,
            }
        }

        async fn drain_for_agent(&self, _agent_id: &str) -> BackgroundInflightReport {
            BackgroundInflightReport {
                total: 0,
                subagent: 0,
                workflow: 0,
                command_session: 0,
            }
        }
    }

    fn obj(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[tokio::test]
    async fn run_subagent_returns_session_handle() {
        let supervisor = Arc::new(FakeSubagentSupervisor::default());
        let mut ctx = metadata();
        ctx.subagent_supervisor = Some(supervisor.clone());

        let res = RunSubagent
            .execute(
                &obj(&[
                    ("agent_name", json!("explorer")),
                    ("prompt", json!("inspect the plan")),
                ]),
                &ctx,
            )
            .await
            .expect("ok");

        assert!(!res.is_error, "{}", res.output);
        assert!(res.output.contains("[SUBAGENT LAUNCHED]"), "{}", res.output);
        assert_eq!(res.metadata["subagent_session_id"], json!("subagent_1"));
        assert_eq!(res.metadata["status"], json!("running"));
        assert_eq!(
            supervisor.spawned.lock().unwrap().as_slice(),
            &[("explorer".to_owned(), "inspect the plan".to_owned())]
        );
    }

    #[tokio::test]
    async fn check_subagent_progress_rejects_out_of_range_last_n() {
        let supervisor = Arc::new(FakeSubagentSupervisor::default());
        let mut ctx = metadata();
        ctx.subagent_supervisor = Some(supervisor);

        for last_n in [0, 11] {
            let res = CheckSubagentProgress
                .execute(
                    &obj(&[
                        ("subagent_session_id", json!("subagent_1")),
                        ("last_n_messages", json!(last_n)),
                    ]),
                    &ctx,
                )
                .await
                .expect("ok");
            assert!(res.is_error);
            assert!(res.output.contains("last_n_messages"), "{}", res.output);
        }
    }

    #[tokio::test]
    async fn subagent_controls_reject_empty_session_id() {
        let ctx = metadata();
        let progress = CheckSubagentProgress
            .execute(
                &obj(&[
                    ("subagent_session_id", json!("")),
                    ("last_n_messages", json!(5)),
                ]),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(progress.is_error);
        assert!(progress.output.contains("subagent_session_id"));

        let cancel = CancelSubagent
            .execute(
                &obj(&[("subagent_session_id", json!("")), ("reason", json!("x"))]),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(cancel.is_error);
        assert!(cancel.output.contains("subagent_session_id"));
    }
}
