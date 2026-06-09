//! Tool framework model contracts.

mod intent {
    //! [`ToolIntent`] — the tool-classification intent (read-only / write / lifecycle).
    //!
    //! `eos-tool` owns this enum (anchor §5). It shares the three values of
    //! `eos_sandbox_port::Intent` (the foreground sandbox-call intent) but is a
    //! distinct, locally-owned contract; the sandbox boundary converts via
    //! [`From`]/[`Into`] rather than aliasing another crate's type (GC: avoids an
    //! unrecorded cross-crate ownership inversion). The lifecycle-batch predicate
    //! (`runtime/dispatch.rs`) and sandbox routing both read this.

    use eos_sandbox_port::Intent;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    /// How a tool is classified for batch-dispatch policy and sandbox routing.
    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
    )]
    #[serde(rename_all = "snake_case")]
    #[non_exhaustive]
    pub enum ToolIntent {
        /// Read-only operation (no mutations) — `Intent.READ_ONLY`.
        ReadOnly,
        /// Operation permitted to mutate the workspace — `Intent.WRITE_ALLOWED`.
        WriteAllowed,
        /// Workspace lifecycle operation — `Intent.LIFECYCLE`. Drives the
        /// lifecycle-batch policy (§6.6): a lifecycle call executes solo so later
        /// calls observe new routing state.
        Lifecycle,
    }

    impl ToolIntent {
        /// Every intent, in a stable order — the canonical iteration order, mirrored
        /// from [`ToolName::ALL`](crate::ToolName) for totality and [`from_wire`].
        ///
        /// [`from_wire`]: ToolIntent::from_wire
        pub const ALL: [ToolIntent; 3] = [
            ToolIntent::ReadOnly,
            ToolIntent::WriteAllowed,
            ToolIntent::Lifecycle,
        ];

        /// The wire string for this intent (the serde `snake_case` form).
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            match self {
                ToolIntent::ReadOnly => "read_only",
                ToolIntent::WriteAllowed => "write_allowed",
                ToolIntent::Lifecycle => "lifecycle",
            }
        }

        /// Parse a wire string into a [`ToolIntent`], or `None` when unknown,
        /// reusing [`as_str`](ToolIntent::as_str) as the single source of spelling.
        #[must_use]
        pub fn from_wire(value: &str) -> Option<Self> {
            Self::ALL
                .into_iter()
                .find(|intent| intent.as_str() == value)
        }
    }

    impl From<Intent> for ToolIntent {
        fn from(intent: Intent) -> Self {
            match intent {
                Intent::ReadOnly => ToolIntent::ReadOnly,
                Intent::WriteAllowed => ToolIntent::WriteAllowed,
                Intent::Lifecycle => ToolIntent::Lifecycle,
            }
        }
    }

    impl From<ToolIntent> for Intent {
        fn from(intent: ToolIntent) -> Self {
            match intent {
                ToolIntent::ReadOnly => Intent::ReadOnly,
                ToolIntent::WriteAllowed => Intent::WriteAllowed,
                ToolIntent::Lifecycle => Intent::Lifecycle,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used)] // unwrap permitted in tests (err-no-unwrap-prod)
        use super::*;

        #[test]
        fn round_trips_with_sandbox_intent() {
            for intent in [
                ToolIntent::ReadOnly,
                ToolIntent::WriteAllowed,
                ToolIntent::Lifecycle,
            ] {
                let sandbox: Intent = intent.into();
                assert_eq!(ToolIntent::from(sandbox), intent);
                assert_eq!(intent.as_str(), sandbox.as_wire());
            }
        }

        #[test]
        fn from_wire_round_trips_and_rejects_unknown() {
            for intent in ToolIntent::ALL {
                assert_eq!(ToolIntent::from_wire(intent.as_str()), Some(intent));
            }
            assert_eq!(ToolIntent::from_wire("nope"), None);
        }

        #[test]
        fn wire_values_match_rust() {
            assert_eq!(
                serde_json::to_value(ToolIntent::ReadOnly).unwrap(),
                "read_only"
            );
            assert_eq!(
                serde_json::to_value(ToolIntent::WriteAllowed).unwrap(),
                "write_allowed"
            );
            assert_eq!(
                serde_json::to_value(ToolIntent::Lifecycle).unwrap(),
                "lifecycle"
            );
        }
    }
}
mod metadata {
    //! Service-free execution facts supplied to one tool call.

    use std::sync::Arc;

    use eos_types::Message;
    use eos_types::{
        AgentRunId, AttemptId, InvocationId, RequestId, SandboxId, TaskId, ToolUseId, WorkItemId,
        WorkflowId,
    };

    use crate::ToolError;

    /// The typed facts a tool executor reads. Built per tool call and owned by the
    /// call; no shared mutable service state is stored here.
    #[derive(Clone)]
    pub struct ExecutionMetadata {
        /// Bound agent profile name.
        pub agent_name: String,
        /// Agent-run id.
        pub agent_run_id: Option<AgentRunId>,
        /// Owning request, when set.
        pub request_id: Option<RequestId>,
        /// Owning task, when set.
        pub task_id: Option<TaskId>,
        /// Owning attempt, when set.
        pub attempt_id: Option<AttemptId>,
        /// Owning workflow, when set.
        pub workflow_id: Option<WorkflowId>,
        /// Planner-authored work item id, when this is a worker run.
        pub work_item_id: Option<WorkItemId>,
        /// Per-call tool-use id.
        pub tool_use_id: Option<ToolUseId>,
        /// In-flight sandbox correlation id, when set.
        pub sandbox_invocation_id: Option<InvocationId>,
        /// Provisioned sandbox, when the agent is sandbox-bound.
        pub sandbox_id: Option<SandboxId>,
        /// Whether this agent currently has an open isolated workspace.
        pub is_isolated_workspace_mode: bool,
        /// Request-visible workspace root.
        pub workspace_root: String,
        /// Per-turn snapshot of the live conversation transcript.
        pub conversation: Arc<[Message]>,
    }

    impl std::fmt::Debug for ExecutionMetadata {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ExecutionMetadata")
                .field("agent_name", &self.agent_name)
                .field("agent_run_id", &self.agent_run_id)
                .field("request_id", &self.request_id)
                .field("task_id", &self.task_id)
                .field("attempt_id", &self.attempt_id)
                .field("workflow_id", &self.workflow_id)
                .field("work_item_id", &self.work_item_id)
                .field("tool_use_id", &self.tool_use_id)
                .field("sandbox_id", &self.sandbox_id)
                .field(
                    "is_isolated_workspace_mode",
                    &self.is_isolated_workspace_mode,
                )
                .finish_non_exhaustive()
        }
    }

    impl ExecutionMetadata {
        /// The calling agent's sandbox id as a string, or `""` when unbound.
        #[must_use]
        pub fn sandbox_id_str(&self) -> &str {
            self.sandbox_id.as_ref().map_or("", SandboxId::as_str)
        }

        /// Require the bound sandbox id, else a framework fault.
        ///
        /// # Errors
        /// Returns [`ToolError::MissingContext`] when no sandbox is bound.
        pub fn require_sandbox_id(&self) -> Result<&SandboxId, ToolError> {
            self.sandbox_id
                .as_ref()
                .ok_or(ToolError::MissingContext("sandbox_id"))
        }

        /// Require the owning task id, else a framework fault.
        ///
        /// # Errors
        /// Returns [`ToolError::MissingContext`] when no task id is set.
        pub fn require_task_id(&self) -> Result<&TaskId, ToolError> {
            self.task_id
                .as_ref()
                .ok_or(ToolError::MissingContext("task_id"))
        }

        /// Require the owning request id, else a framework fault.
        ///
        /// # Errors
        /// Returns [`ToolError::MissingContext`] when no request id is set.
        pub fn require_request_id(&self) -> Result<&RequestId, ToolError> {
            self.request_id
                .as_ref()
                .ok_or(ToolError::MissingContext("request_id"))
        }

        /// Require the current agent-run id, else a framework fault.
        ///
        /// # Errors
        /// Returns [`ToolError::MissingContext`] when no agent-run id is set.
        pub fn require_agent_run_id(&self) -> Result<&AgentRunId, ToolError> {
            self.agent_run_id
                .as_ref()
                .ok_or(ToolError::MissingContext("agent_run_id"))
        }

        /// Require the owning attempt id, else a framework fault.
        ///
        /// # Errors
        /// Returns [`ToolError::MissingContext`] when no attempt id is set.
        pub fn require_attempt_id(&self) -> Result<&AttemptId, ToolError> {
            self.attempt_id
                .as_ref()
                .ok_or(ToolError::MissingContext("attempt_id"))
        }

        /// Require the current work item id, else a framework fault.
        ///
        /// # Errors
        /// Returns [`ToolError::MissingContext`] when no work item id is set.
        pub fn require_work_item_id(&self) -> Result<&WorkItemId, ToolError> {
            self.work_item_id
                .as_ref()
                .ok_or(ToolError::MissingContext("work_item_id"))
        }
    }
}
mod name {
    //! [`ToolName`] / [`ToolKey`] — typed names for model-facing tools.
    //!
    //! Ports `_names.py` **plus** the five names that module omits (`write_stdin`,
    //! `read_command_progress`, `enter_isolated_workspace`, `exit_isolated_workspace`,
    //! `load_skill_reference`) and `cancel_subagent` — GC-tools-04. The authoritative
    //! set is the union of the six registration sites, not `_names.py`. Each variant
    //! maps to its wire string (the exact `snake_case` of the variant), so `serde`
    //! `rename_all` and the hand-written [`ToolName::as_str`] agree (asserted by a
    //! test).

    use std::fmt;
    use std::str::FromStr;

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    /// The typed name of a public tool (`type-no-stringly`). `#[non_exhaustive]`:
    /// new tools are added here, never as raw strings.
    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
    )]
    #[serde(rename_all = "snake_case")]
    #[non_exhaustive]
    pub enum ToolName {
        /// `read_file` (sandbox).
        ReadFile,
        /// `write_file` (sandbox).
        WriteFile,
        /// `edit_file` (sandbox).
        EditFile,
        /// `multi_edit` (sandbox).
        MultiEdit,
        /// `exec_command` (sandbox command session).
        ExecCommand,
        /// `write_stdin` (sandbox command session; omitted from `_names.py`).
        WriteStdin,
        /// `read_command_progress` (sandbox command session).
        ReadCommandProgress,
        /// `enter_isolated_workspace` (omitted from `_names.py`).
        EnterIsolatedWorkspace,
        /// `exit_isolated_workspace` (omitted from `_names.py`).
        ExitIsolatedWorkspace,
        /// `run_subagent` (subagent).
        RunSubagent,
        /// `cancel_subagent` (subagent control).
        CancelSubagent,
        /// `ask_advisor` (ask helper).
        AskAdvisor,
        /// `delegate_workflow` (workflow).
        DelegateWorkflow,
        /// `check_workflow_status` (workflow).
        CheckWorkflowStatus,
        /// `cancel_workflow` (workflow).
        CancelWorkflow,
        /// `load_skill_reference` (skills; omitted from `_names.py`).
        LoadSkillReference,
        /// `submit_root_task_outcome` (submission, terminal).
        SubmitRootTaskOutcome,
        /// `submit_plan_outcome` (submission, terminal).
        SubmitPlanOutcome,
        /// `submit_worker_outcome` (submission, terminal).
        SubmitWorkerOutcome,
        /// `submit_advisor_outcome` (submission, terminal).
        SubmitAdvisorOutcome,
        /// `submit_subagent_outcome` (submission, terminal).
        SubmitSubagentOutcome,
    }

    impl ToolName {
        /// Every tool name, in a stable order. Used by registry-totality tests and
        /// as the canonical iteration order for default-set construction.
        pub const ALL: [ToolName; 21] = [
            ToolName::ReadFile,
            ToolName::WriteFile,
            ToolName::EditFile,
            ToolName::MultiEdit,
            ToolName::ExecCommand,
            ToolName::WriteStdin,
            ToolName::ReadCommandProgress,
            ToolName::EnterIsolatedWorkspace,
            ToolName::ExitIsolatedWorkspace,
            ToolName::RunSubagent,
            ToolName::CancelSubagent,
            ToolName::AskAdvisor,
            ToolName::DelegateWorkflow,
            ToolName::CheckWorkflowStatus,
            ToolName::CancelWorkflow,
            ToolName::LoadSkillReference,
            ToolName::SubmitRootTaskOutcome,
            ToolName::SubmitPlanOutcome,
            ToolName::SubmitWorkerOutcome,
            ToolName::SubmitAdvisorOutcome,
            ToolName::SubmitSubagentOutcome,
        ];

        /// The wire string the model calls this tool by.
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            match self {
                ToolName::ReadFile => "read_file",
                ToolName::WriteFile => "write_file",
                ToolName::EditFile => "edit_file",
                ToolName::MultiEdit => "multi_edit",
                ToolName::ExecCommand => "exec_command",
                ToolName::WriteStdin => "write_stdin",
                ToolName::ReadCommandProgress => "read_command_progress",
                ToolName::EnterIsolatedWorkspace => "enter_isolated_workspace",
                ToolName::ExitIsolatedWorkspace => "exit_isolated_workspace",
                ToolName::RunSubagent => "run_subagent",
                ToolName::CancelSubagent => "cancel_subagent",
                ToolName::AskAdvisor => "ask_advisor",
                ToolName::DelegateWorkflow => "delegate_workflow",
                ToolName::CheckWorkflowStatus => "check_workflow_status",
                ToolName::CancelWorkflow => "cancel_workflow",
                ToolName::LoadSkillReference => "load_skill_reference",
                ToolName::SubmitRootTaskOutcome => "submit_root_task_outcome",
                ToolName::SubmitPlanOutcome => "submit_plan_outcome",
                ToolName::SubmitWorkerOutcome => "submit_worker_outcome",
                ToolName::SubmitAdvisorOutcome => "submit_advisor_outcome",
                ToolName::SubmitSubagentOutcome => "submit_subagent_outcome",
            }
        }

        /// Parse a wire string into a [`ToolName`], or `None` when unknown.
        #[must_use]
        pub fn from_wire(value: &str) -> Option<Self> {
            Self::ALL.into_iter().find(|name| name.as_str() == value)
        }
    }

    /// The registry key for a public model-facing tool.
    ///
    /// Built-in tools still use [`ToolName`]. Plugin tools are validated dynamic
    /// names such as `lsp.hover`, carried as a typed key so the registry can accept
    /// plugin-provided tools without extending the built-in enum.
    #[derive(
        Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
    )]
    #[serde(transparent)]
    #[schemars(transparent)]
    pub struct ToolKey(String);

    impl ToolKey {
        /// Parse a wire tool name into a registry key.
        ///
        /// Built-ins are always accepted. Dynamic plugin names must be a dotted
        /// `<plugin>.<op>` name with non-empty identifier segments; this is enough
        /// for agent-profile validation to reach the registry lookup while rejecting
        /// arbitrary strings that are neither built-ins nor plugin tools.
        #[must_use]
        pub fn from_wire(value: &str) -> Option<Self> {
            if let Some(name) = ToolName::from_wire(value) {
                return Some(Self::from(name));
            }
            if is_valid_dynamic_tool_name(value) {
                Some(Self(value.to_owned()))
            } else {
                None
            }
        }

        /// Build a dynamic tool key from a catalog-validated name.
        #[must_use]
        pub fn dynamic(value: impl Into<String>) -> Self {
            Self(value.into())
        }

        /// The model/provider wire string.
        #[must_use]
        pub fn as_str(&self) -> &str {
            &self.0
        }

        /// The built-in tool name, when this key names one.
        #[must_use]
        pub fn as_builtin(&self) -> Option<ToolName> {
            ToolName::from_wire(&self.0)
        }
    }

    impl From<ToolName> for ToolKey {
        fn from(name: ToolName) -> Self {
            Self(name.as_str().to_owned())
        }
    }

    impl From<&ToolKey> for ToolKey {
        fn from(name: &ToolKey) -> Self {
            name.clone()
        }
    }

    impl fmt::Display for ToolKey {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_str())
        }
    }

    fn is_valid_dynamic_tool_name(value: &str) -> bool {
        let Some((plugin, op)) = value.split_once('.') else {
            return false;
        };
        is_valid_tool_segment(plugin) && op.split('.').all(is_valid_tool_segment)
    }

    fn is_valid_tool_segment(value: &str) -> bool {
        let mut chars = value.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    impl fmt::Display for ToolName {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_str())
        }
    }

    impl FromStr for ToolName {
        type Err = ();

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            Self::from_wire(s).ok_or(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // ALL lists every variant exactly once (no duplicates, no omissions).
        #[test]
        fn all_is_complete_and_unique() {
            let mut seen = std::collections::BTreeSet::new();
            for name in ToolName::ALL {
                assert!(seen.insert(name.as_str()), "duplicate {}", name.as_str());
            }
            assert_eq!(seen.len(), ToolName::ALL.len());
        }

        // The hand-written wire table agrees with the serde `rename_all` projection.
        #[test]
        fn as_str_matches_serde_rename() {
            for name in ToolName::ALL {
                let serde_value = serde_json::to_value(name).expect("serialize");
                assert_eq!(serde_value, serde_json::json!(name.as_str()));
                // round-trip through from_wire and serde.
                assert_eq!(ToolName::from_wire(name.as_str()), Some(name));
                let back: ToolName =
                    serde_json::from_value(serde_json::json!(name.as_str())).expect("parse");
                assert_eq!(back, name);
            }
            assert_eq!(ToolName::from_wire("not_a_tool"), None);
        }

        #[test]
        fn tool_key_accepts_builtin_and_plugin_names() {
            assert_eq!(
                ToolKey::from_wire("read_file").and_then(|key| key.as_builtin()),
                Some(ToolName::ReadFile)
            );
            let plugin = ToolKey::from_wire("lsp.hover").expect("plugin key");
            assert_eq!(plugin.as_str(), "lsp.hover");
            assert_eq!(plugin.as_builtin(), None);
            assert!(ToolKey::from_wire("not_a_builtin").is_none());
            assert!(ToolKey::from_wire("lsp.").is_none());
            assert!(ToolKey::from_wire(".hover").is_none());
        }
    }
}
mod result {
    //! In-band tool result contracts.

    use eos_types::JsonObject;
    use serde::de::DeserializeOwned;

    /// A normalized in-band tool result. Both success and tool-domain failure are
    /// values of this type; only framework faults are `Err(crate::ToolError)`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ToolResult {
        /// The model-facing output text.
        pub output: String,
        /// Whether this is an in-band tool-domain error.
        pub is_error: bool,
        /// Heterogeneous result metadata.
        pub metadata: JsonObject,
        /// Set by the tool pipeline when a terminal tool succeeds.
        pub is_terminal: bool,
    }

    impl ToolResult {
        /// A successful plain result.
        #[must_use]
        pub fn ok(output: impl Into<String>) -> Self {
            Self {
                output: output.into(),
                is_error: false,
                metadata: JsonObject::new(),
                is_terminal: false,
            }
        }

        /// An in-band tool-domain error result.
        #[must_use]
        pub fn error(output: impl Into<String>) -> Self {
            Self {
                output: output.into(),
                is_error: true,
                metadata: JsonObject::new(),
                is_terminal: false,
            }
        }

        /// Attach result metadata.
        #[must_use]
        pub fn with_metadata(mut self, metadata: JsonObject) -> Self {
            self.metadata = metadata;
            self
        }

        /// Insert one metadata key.
        #[must_use]
        pub fn meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
            self.metadata.insert(key.into(), value);
            self
        }
    }

    /// The declared shape of a tool's successful output.
    #[derive(Clone)]
    pub enum OutputShape {
        /// Plain text.
        Text,
        /// Structured JSON that must deserialize into the named model.
        Json {
            /// The output model name.
            model_name: &'static str,
            /// Validator for the serialized output string.
            validate: fn(&str) -> Result<(), String>,
        },
    }

    impl std::fmt::Debug for OutputShape {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Text => f.write_str("OutputShape::Text"),
                Self::Json { model_name, .. } => {
                    write!(f, "OutputShape::Json({model_name})")
                }
            }
        }
    }

    impl OutputShape {
        /// Build an [`OutputShape::Json`] for output model `T`.
        #[must_use]
        pub fn json<T: DeserializeOwned>(model_name: &'static str) -> Self {
            Self::Json {
                model_name,
                validate: validate_json::<T>,
            }
        }
    }

    fn validate_json<T: DeserializeOwned>(output: &str) -> Result<(), String> {
        serde_json::from_str::<T>(output)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }
}

pub use intent::ToolIntent;
pub use metadata::ExecutionMetadata;
pub use name::{ToolKey, ToolName};
pub use result::{OutputShape, ToolResult};

/// Typed launch rejection facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentLaunchRejection {
    /// The caller is already a subagent.
    Recursive,
    /// The requested agent name is not registered.
    NotRegistered {
        /// Requested agent name.
        agent_name: String,
    },
    /// The requested agent exists but is not subagent-typed.
    NotSubagent {
        /// Requested agent name.
        agent_name: String,
        /// Registered agent type string.
        agent_type: String,
    },
}
