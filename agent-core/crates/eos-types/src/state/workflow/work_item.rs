//! Planner-authored work-item values shared by tools, workflow, and persistence.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{AgentName, CoreError};

macro_rules! nonblank_string_newtype {
    ($(#[$meta:meta])* $name:ident, $label:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Construct a nonblank ", $label, ".")]
            ///
            /// # Errors
            /// Returns [`CoreError`] when the value is blank.
            pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(CoreError::Store(concat!($label, " must be nonblank").to_owned()));
                }
                Ok(Self(value))
            }

            #[doc = concat!("Borrow the raw ", $label, ".")]
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[doc = concat!("Consume the value and return the raw ", $label, ".")]
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl TryFrom<String> for $name {
            type Error = CoreError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = CoreError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value.to_owned())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

nonblank_string_newtype!(
    /// Planner-authored workflow-local work item id.
    WorkItemId,
    "work item id"
);

nonblank_string_newtype!(
    /// Attempt-local plan id, minted when the attempt is created.
    PlanId,
    "plan id"
);

impl PlanId {
    /// Mint a fresh opaque plan id.
    #[must_use]
    pub fn new_v4() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

nonblank_string_newtype!(
    /// Goal carried to the next workflow iteration.
    DeferredGoal,
    "deferred goal"
);

/// One planner-authored work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkItemSpec {
    /// Planner-authored workflow-local id.
    pub id: WorkItemId,
    /// Selected worker-capable agent profile name.
    pub agent_name: AgentName,
    /// Executable work instruction, used as the worker run instruction.
    pub work_spec: String,
    /// Direct work-item dependencies.
    #[serde(default)]
    pub needs: Vec<WorkItemId>,
}
