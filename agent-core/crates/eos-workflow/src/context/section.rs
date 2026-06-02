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
