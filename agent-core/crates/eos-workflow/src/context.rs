mod composer;
mod engine;
mod scope {
    use eos_types::{AttemptId, IterationId, TaskId, WorkflowId};

    use super::ContextRole;

    /// Identity a context builder reads, keyed by launch role so each role carries
    /// exactly the ids it requires. Constructed only through the `for_*`
    /// constructors, which makes an id/role mismatch unrepresentable.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ContextScope {
        /// Planner launch: workflow + iteration + attempt.
        Planner {
            /// Workflow id.
            workflow_id: WorkflowId,
            /// Iteration id.
            iteration_id: IterationId,
            /// Attempt id.
            attempt_id: AttemptId,
        },
        /// Generator launch: planner ids plus the assigned task.
        Generator {
            /// Workflow id.
            workflow_id: WorkflowId,
            /// Iteration id.
            iteration_id: IterationId,
            /// Attempt id.
            attempt_id: AttemptId,
            /// Assigned task id.
            task_id: TaskId,
        },
        /// Reducer launch: planner ids plus the assigned task.
        Reducer {
            /// Workflow id.
            workflow_id: WorkflowId,
            /// Iteration id.
            iteration_id: IterationId,
            /// Attempt id.
            attempt_id: AttemptId,
            /// Assigned task id.
            task_id: TaskId,
        },
    }

    impl ContextScope {
        /// Scope for a planner launch.
        #[must_use]
        pub fn for_planner(
            workflow_id: WorkflowId,
            iteration_id: IterationId,
            attempt_id: AttemptId,
        ) -> Self {
            Self::Planner {
                workflow_id,
                iteration_id,
                attempt_id,
            }
        }

        /// Scope for a generator launch.
        #[must_use]
        pub fn for_generator(
            workflow_id: WorkflowId,
            iteration_id: IterationId,
            attempt_id: AttemptId,
            task_id: TaskId,
        ) -> Self {
            Self::Generator {
                workflow_id,
                iteration_id,
                attempt_id,
                task_id,
            }
        }

        /// Scope for a reducer launch.
        #[must_use]
        pub fn for_reducer(
            workflow_id: WorkflowId,
            iteration_id: IterationId,
            attempt_id: AttemptId,
            task_id: TaskId,
        ) -> Self {
            Self::Reducer {
                workflow_id,
                iteration_id,
                attempt_id,
                task_id,
            }
        }

        /// The launch role this scope was built for.
        #[must_use]
        pub fn role(&self) -> ContextRole {
            match self {
                Self::Planner { .. } => ContextRole::Planner,
                Self::Generator { .. } => ContextRole::Generator,
                Self::Reducer { .. } => ContextRole::Reducer,
            }
        }
    }
}
mod section {
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    /// Role-specific context packet kind.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum ContextRole {
        /// Planner context.
        Planner,
        /// Generator context.
        Generator,
        /// Reducer context.
        Reducer,
    }

    impl ContextRole {
        /// Canonical role token.
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Planner => "planner",
                Self::Generator => "generator",
                Self::Reducer => "reducer",
            }
        }
    }

    /// One XML-like context section.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    pub struct ContextSection {
        /// Element tag.
        pub tag: String,
        /// Insertion-ordered attributes.
        #[serde(default)]
        pub attrs: Vec<(String, String)>,
        /// Optional text body.
        #[serde(default)]
        pub text: Option<String>,
        /// Child sections.
        #[serde(default)]
        pub children: Vec<ContextSection>,
    }

    impl ContextSection {
        /// Section with a tag and no content.
        #[must_use]
        pub fn new(tag: impl Into<String>) -> Self {
            Self {
                tag: tag.into(),
                attrs: Vec::new(),
                text: None,
                children: Vec::new(),
            }
        }

        /// Attach attributes.
        #[must_use]
        pub fn with_attrs(mut self, attrs: Vec<(String, String)>) -> Self {
            self.attrs = attrs;
            self
        }

        /// Attach text.
        #[must_use]
        pub fn with_text(mut self, text: impl Into<String>) -> Self {
            self.text = Some(text.into());
            self
        }

        /// Attach children.
        #[must_use]
        pub fn with_children(mut self, children: Vec<ContextSection>) -> Self {
            self.children = children;
            self
        }
    }

    /// Full role context packet.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    pub struct AgentContext {
        /// Context role.
        pub role: ContextRole,
        /// Top-level sections.
        pub sections: Vec<ContextSection>,
        /// Role directive.
        pub directive: String,
        /// Explicit context limits.
        #[serde(default)]
        pub context_limits: Vec<String>,
    }
}
mod xml {
    use eos_types::ExecutionTaskOutcome;

    use super::{AgentContext, ContextSection};

    /// Render a context packet into the XML-like prompt envelope.
    #[must_use]
    pub fn render_context_xml(context: &AgentContext) -> String {
        let root = ContextSection::new("context")
            .with_attrs(vec![("role".to_owned(), context.role.as_str().to_owned())])
            .with_children(context.sections.clone());
        format!("{}\n", render_section(&root))
    }

    pub(crate) fn render_task_outcome(outcome: &ExecutionTaskOutcome) -> ContextSection {
        ContextSection::new("task")
            .with_attrs(vec![
                ("task_id".to_owned(), outcome.task_id.as_str().to_owned()),
                ("role".to_owned(), outcome.role.as_str().to_owned()),
                ("status".to_owned(), outcome.status.as_str().to_owned()),
            ])
            .with_text(outcome.outcome.clone())
    }

    pub(crate) fn render_section(section: &ContextSection) -> String {
        let attrs = section
            .attrs
            .iter()
            .map(|(k, v)| format!(" {}=\"{}\"", escape(k), escape(v)))
            .collect::<String>();
        let mut body = Vec::new();
        if let Some(text) = &section.text {
            body.push(escape(text));
        }
        body.extend(section.children.iter().map(render_section));
        format!(
            "<{}{}>\n{}\n</{}>",
            section.tag,
            attrs,
            body.join("\n"),
            section.tag
        )
    }

    fn escape(s: &str) -> String {
        // Matches Rust `html.escape(s, quote=True)` (xml.py): `&` first, then the
        // angle brackets, then both quote forms (`"` -> `&quot;`, `'` -> `&#x27;`).
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#x27;")
    }
}

pub use composer::{render_task_guidance, AgentEntryComposer, AgentEntryMessages};
pub use engine::{ContextEngine, ContextEngineStores};
pub use scope::ContextScope;
pub use section::{AgentContext, ContextRole, ContextSection};
pub use xml::render_context_xml;
