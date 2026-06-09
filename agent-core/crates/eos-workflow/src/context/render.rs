use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Role-specific context packet kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextRole {
    /// Planner context.
    Planner,
    /// Worker context.
    Worker,
}

impl ContextRole {
    /// Canonical role token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Worker => "worker",
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

/// Render a context packet into the XML-like prompt envelope.
#[must_use]
pub fn render_context_xml(context: &AgentContext) -> String {
    let root = ContextSection::new("context")
        .with_attrs(vec![("role".to_owned(), context.role.as_str().to_owned())])
        .with_children(context.sections.clone());
    format!("{}\n", render_section(&root))
}

/// Render role guidance from a context packet.
#[must_use]
pub fn render_task_guidance(context: &AgentContext) -> String {
    let contents = match context.role {
        ContextRole::Planner => [
            "- <workflow>: workflow goal and current planning frame",
            "- <current_iteration>: current iteration goal and attempt identity",
            "- <prior_attempts>: earlier attempt status for this iteration",
        ]
        .as_slice(),
        ContextRole::Worker => [
            "- <plan_spec>: the planner's plan explanation",
            "- <work_item>: your assigned work item only",
            "- <needs>: direct dependency outcomes only",
        ]
        .as_slice(),
    };
    let mut parts = vec![format!("What's in context:\n{}", contents.join("\n"))];
    if !context.context_limits.is_empty() {
        parts.push(format!(
            "Context limits:\n{}",
            context
                .context_limits
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    parts.push(format!("What to do:\n- {}", context.directive));
    parts.join("\n\n")
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
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
